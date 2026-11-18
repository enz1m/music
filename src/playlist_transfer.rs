//! The portal-mediated Import/Export route for user playlists.
//!
//! This owns the app-side state around two host portal dialogs and one bounded
//! worker, and nothing else: it never mutates the library and never runs
//! filesystem or parse work on the reducer thread.
//!
//! * Import opens the host `OpenFile` chooser filtered to `m3u`/`m3u8`/`pls`
//!   through the framework [`FileRequest`] seam (the same one `picker.rs` uses
//!   for Add Folder). The reducer submits the chosen file's bounded no-follow
//!   read and parse to `Self::submit_import`, which runs on the dedicated
//!   owner; only a current, complete settlement creates a playlist.
//! * Export opens the host `SaveFile` chooser through
//!   [`enzui_core::download_destination::DestinationBroker`], whose grant
//!   authorizes exactly one location. Publication runs through the grant's
//!   reservation ([`DownloadDestination::reserve_file`] +
//!   `ReservedDownloadDestination::publish_with`), the identity-safe
//!   no-clobber shape rather than `ReplaceOrCreate` on a re-opened path.
//!
//! Request generations are monotonic and checked for exhaustion before any
//! portal or owner work. A stale settlement (a superseded generation, a
//! cancelled dialog, a dropped worker future) is inert at the reducer: it
//! carries its generation so the reducer can reject it.

use std::path::{Path, PathBuf};

use enzui_core::blocking_task::{
    BlockingTask, BlockingTaskError, BlockingTaskOwner, BlockingTaskSettlement,
};
use enzui_core::download::DownloadId;
use enzui_core::download_destination::{
    DestinationBroker, DestinationOutcome, DownloadDestination,
};
use enzui_widgets::file_chooser::{
    ChooserOptions, FileFilter, FileRequest, FileResult, PendingRequest, PortalBackend,
    PortalOperation, PortalOutcome, PortalParent, PortalWake,
};

use crate::playlist_io::{self, ParseError, ParseReport, PlaylistFormat, SerializeError};

/// Worker count for the playlist owner: one, so read/parse/serialize/publish
/// are serialized off the reducer thread.
const OWNER_WORKERS: usize = 1;

/// Pending FIFO depth for the playlist owner: two, so one in-flight call plus
/// one queued intent are admitted and a third is refused as
/// [`enzui_core::blocking_task::BlockingTaskError::Busy`].
const OWNER_QUEUE: usize = 2;

/// A live `OpenFile` import request and the generation it was minted under.
struct ImportPending {
    generation: u64,
    request: PendingRequest,
}

/// Why a chosen file could not be read as a playlist document (a file fault,
/// distinct from a document parse fault).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadRefusal {
    /// The chosen path is not a regular file (symlink refused by `O_NOFOLLOW`,
    /// or a directory/FIFO/device).
    NotRegularFile,
    /// The file is larger than [`playlist_io::MAX_DOCUMENT_BYTES`].
    TooLarge,
    /// The extension is not one of `m3u`/`m3u8`/`pls`.
    UnknownFormat,
    /// The descriptor's size changed under the read, so its identity could not
    /// be revalidated.
    IdentityChanged,
    /// A filesystem error opening or reading the file.
    Io(String),
}

/// A failed import: either the file could not be read or the document was
/// malformed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImportError {
    /// The chosen file could not be read.
    Read(ReadRefusal),
    /// The bytes were read but did not parse.
    Parse(ParseError),
}

impl ImportError {
    /// A bounded, user-safe one-line summary. Never includes bytes or a path.
    #[must_use]
    pub fn summary(&self) -> String {
        match self {
            Self::Read(ReadRefusal::NotRegularFile) => {
                "That is not a regular playlist file.".into()
            }
            Self::Read(ReadRefusal::TooLarge) => "That playlist file is too large.".into(),
            Self::Read(ReadRefusal::UnknownFormat) => {
                "That file is not an M3U, M3U8, or PLS playlist.".into()
            }
            Self::Read(ReadRefusal::IdentityChanged) => {
                "That file changed while it was being read.".into()
            }
            Self::Read(ReadRefusal::Io(_)) => "That playlist file could not be read.".into(),
            Self::Parse(e) => e.summary(),
        }
    }
}

/// The terminal outcome of an import read/parse submission, stamped with its
/// generation so a stale one is inert at the reducer.
#[derive(Debug, Clone)]
pub enum ImportSettlement {
    /// The document parsed whole.
    Parsed(ParseReport),
    /// The document could not be read or parsed.
    Failed(ImportError),
    /// The owner was saturated; the reducer retains the exact intent.
    Busy,
    /// The worker future was cancelled or the owner shut down; inert.
    Aborted,
}

/// Why an export publication failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExportPublishError {
    /// The rows could not be serialized (over-long path or non-UTF-8).
    Serialize(SerializeError),
    /// The grant URI could not be decoded to a local path.
    UriDecode(String),
    /// A failure before any publication syscall — nothing was written.
    PrePublication(String),
    /// A publication syscall ran but durability is uncertain.
    DurabilityUncertain(String),
}

impl ExportPublishError {
    /// A bounded, user-safe one-line summary.
    #[must_use]
    pub fn summary(&self) -> String {
        match self {
            Self::Serialize(_) => "That playlist could not be written.".into(),
            Self::UriDecode(_) => "That save location is not a local file.".into(),
            Self::PrePublication(_) => "The playlist could not be saved.".into(),
            Self::DurabilityUncertain(_) => {
                "The playlist was written, but saving it durably is uncertain.".into()
            }
        }
    }

    /// Whether a publication syscall ran (custody may have rotated).
    #[must_use]
    pub fn was_published(&self) -> bool {
        matches!(self, Self::DurabilityUncertain(_))
    }
}

/// The terminal outcome of an export serialize/publish submission.
#[derive(Debug, Clone)]
pub enum ExportSettlement {
    /// The bytes were durably published.
    Published,
    /// Publication failed; the reducer retains the snapshot for Retry.
    Failed(ExportPublishError),
    /// The owner was saturated; the reducer retains the exact intent.
    Busy,
    /// The worker future was cancelled or the owner shut down; inert.
    Aborted,
}

/// Why a portal request could not even start.
#[derive(Debug, Clone)]
pub enum StartRefusal {
    /// The request-generation space is exhausted; refused before portal work.
    GenerationsExhausted,
    /// The `OpenFile` portal returned a terminal outcome without a dialog.
    Import(PortalOutcome<FileResult>),
    /// The `SaveFile` portal returned a terminal outcome without a dialog.
    Export(DestinationOutcome),
}

/// The app-side owner of the playlist Import/Export portals and worker.
pub struct PlaylistTransfer {
    backend: Option<Box<dyn PortalBackend>>,
    import: Option<ImportPending>,
    export: DestinationBroker,
    generation: u64,
    owner: BlockingTaskOwner,
}

impl std::fmt::Debug for PlaylistTransfer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PlaylistTransfer")
            .field("backend", &self.backend.as_ref().map(|b| b.describe()))
            .field("import_pending", &self.import.is_some())
            .field("export_unsettled", &self.export.has_unsettled_request())
            .field("generation", &self.generation)
            .finish()
    }
}

impl Default for PlaylistTransfer {
    fn default() -> Self {
        Self::new()
    }
}

impl PlaylistTransfer {
    /// A transfer with the shipping owner (one worker, queue two), degrading to
    /// an unavailable owner if a thread cannot be spawned.
    #[must_use]
    pub fn new() -> Self {
        Self::with_owner(shipping_owner())
    }

    fn with_owner(owner: BlockingTaskOwner) -> Self {
        Self {
            backend: None,
            import: None,
            export: DestinationBroker::new(),
            generation: 0,
            owner,
        }
    }

    /// Whether the import `OpenFile` dialog is still live.
    #[must_use]
    pub fn import_portal_pending(&self) -> bool {
        self.import.is_some()
    }

    /// Mint the next request generation, or `None` when the space is exhausted.
    fn next_generation(&mut self) -> Option<u64> {
        let next = self.generation.checked_add(1)?;
        self.generation = next;
        Some(next)
    }

    /// Open the host `OpenFile` chooser filtered to playlist documents.
    ///
    /// Returns the request generation on success, or a [`StartRefusal`] for an
    /// exhausted generation space or a portal that never opened a dialog.
    pub fn start_import(&mut self) -> Result<u64, StartRefusal> {
        if self.import.is_some() {
            // One import dialog at a time; report the live generation.
            return Ok(self.import.as_ref().map(|i| i.generation).unwrap_or(0));
        }
        let generation = self
            .next_generation()
            .ok_or(StartRefusal::GenerationsExhausted)?;
        let options = ChooserOptions::new()
            .title("Import a playlist")
            .current_filter(FileFilter::extensions("Playlists", ["m3u", "m3u8", "pls"]));
        if !self.ensure_backend() {
            return Err(StartRefusal::Import(PortalOutcome::Unavailable));
        }
        let backend = self.backend.as_deref_mut().expect("backend was built");
        match FileRequest::new(PortalOperation::OpenFile, PortalParent::None)
            .with_options(options)
            .start(backend, |_| true)
        {
            Ok(request) => {
                self.import = Some(ImportPending {
                    generation,
                    request,
                });
                Ok(generation)
            }
            Err(outcome) => Err(StartRefusal::Import(outcome)),
        }
    }

    /// Open the host `SaveFile` chooser with a suggested playlist filename.
    ///
    /// Returns the request generation on success; the reducer keys the owned
    /// export snapshot by this exact generation.
    pub fn start_export(&mut self, suggested_name: String) -> Result<u64, StartRefusal> {
        let generation = self
            .next_generation()
            .ok_or(StartRefusal::GenerationsExhausted)?;
        let options = ChooserOptions::new()
            .title("Export playlist")
            .current_name(suggested_name)
            .filter(FileFilter::extensions("Playlists", ["m3u8", "m3u", "pls"]));
        if !self.ensure_backend() {
            return Err(StartRefusal::Export(DestinationOutcome::Unavailable));
        }
        let backend = self.backend.as_deref_mut().expect("backend was built");
        match self.export.request_with_options(
            DownloadId(generation),
            backend,
            PortalParent::None,
            options,
            |_| true,
        ) {
            None => Ok(generation),
            Some(outcome) => Err(StartRefusal::Export(outcome)),
        }
    }

    /// One completion-driven pass over the live import request.
    ///
    /// Returns `Some((generation, outcome))` on the wake a terminal outcome
    /// appears, clearing the slot; `None` while the user is still deciding.
    pub fn pump_import(&mut self) -> Option<(u64, PortalOutcome<FileResult>)> {
        let generation = self.import.as_ref()?.generation;
        let mut captured: Option<PortalOutcome<FileResult>> = None;
        let backend = self
            .backend
            .as_deref_mut()
            .expect("a pending import implies a backend");
        let request = &mut self.import.as_mut().expect("checked above").request;
        let _task: iced::Task<()> = enzui_widgets::file_chooser::pump(request, backend, |o| {
            captured = Some(o);
        });
        if captured.is_some() {
            self.import = None;
        }
        captured.map(|o| (generation, o))
    }

    /// One completion-driven pass over the export broker, returning every newly
    /// terminal `(generation, outcome)` in request order.
    pub fn pump_export(&mut self) -> Vec<(u64, DestinationOutcome)> {
        let Some(backend) = self.backend.as_deref_mut() else {
            return Vec::new();
        };
        self.export
            .pump(backend)
            .into_iter()
            .map(|(id, outcome)| (id.0, outcome))
            .collect()
    }

    /// The merged event-driven wake for both portals.
    pub fn completion_subscription(&self) -> iced::Subscription<PortalWake> {
        let import = enzui_widgets::file_chooser::completion_subscription(
            self.import.as_ref().map(|i| i.generation).unwrap_or(0),
            self.import.as_ref().map(|i| &i.request),
            self.backend.as_deref(),
        );
        let export = self.export.completion_subscription(self.backend.as_deref());
        iced::Subscription::batch([import, export])
    }

    /// Submit a chosen file's bounded no-follow read and parse to the owner.
    pub fn submit_import(
        &self,
        generation: u64,
        path: PathBuf,
    ) -> BlockingTask<Result<ParseReport, ImportError>> {
        self.owner
            .submit(generation, move |_ctx| read_and_parse(&path))
    }

    /// Submit an export serialization and identity-safe publication to the
    /// owner, using the grant's reservation shape.
    pub fn submit_export(
        &self,
        generation: u64,
        grant: DownloadDestination,
        paths: Vec<PathBuf>,
    ) -> BlockingTask<Result<(), ExportPublishError>> {
        self.owner
            .submit(generation, move |_ctx| publish_playlist(&grant, &paths))
    }

    /// Cancel the live import dialog and drop its intent, so a late outcome is
    /// inert. Used on close: an unstarted import is expendable.
    pub fn cancel_import(&mut self) {
        if let Some(pending) = self.import.as_mut()
            && let Some(backend) = self.backend.as_deref_mut()
        {
            let _ = pending
                .request
                .cancel(enzui_core::portal::CancelReason::Application);
            let _ = pending.request.settle(backend);
        }
        self.import = None;
    }

    /// Cancel a not-yet-granted export request by generation. Used on close for
    /// an export whose dialog is still open (expendable, no publication owed).
    pub fn cancel_export(&mut self, generation: u64) {
        let _ = self.export.cancel(
            DownloadId(generation),
            enzui_core::portal::CancelReason::Application,
        );
        if let Some(backend) = self.backend.as_deref_mut() {
            let _ = self.export.pump(backend);
        }
    }

    /// Stop new worker admission after all retained publication debt settles.
    pub fn shutdown(&mut self) {
        self.owner.shutdown();
    }

    fn ensure_backend(&mut self) -> bool {
        if self.backend.is_none() {
            match enzui_widgets::file_chooser::DbusBackend::new() {
                Ok(b) => self.backend = Some(Box::new(b)),
                Err(_) => return false,
            }
        }
        self.backend.is_some()
    }
}

/// Build the shipping owner, degrading to a permanently shut-down owner if the
/// worker thread cannot be spawned.
fn shipping_owner() -> BlockingTaskOwner {
    let workers = std::num::NonZeroUsize::new(OWNER_WORKERS).expect("nonzero workers");
    let queue = std::num::NonZeroUsize::new(OWNER_QUEUE).expect("nonzero queue");
    BlockingTaskOwner::new("music-playlist-io", workers, queue)
        .unwrap_or_else(|_| BlockingTaskOwner::unavailable(queue))
}

/// The first local file path from a completed `OpenFile` result, if any.
#[must_use]
pub fn first_local_file(result: &FileResult) -> Option<PathBuf> {
    result.local_paths().into_iter().next()
}

/// Map an owner settlement for an import read/parse into a stamped
/// [`ImportSettlement`], distinguishing a completed value, a document/read
/// fault, an owner-Busy refusal, and a cancelled/shutdown abort.
#[must_use]
pub fn import_settlement(
    outcome: BlockingTaskSettlement<Result<ParseReport, ImportError>>,
) -> ImportSettlement {
    match outcome.into_result() {
        Ok(Ok(report)) => ImportSettlement::Parsed(report),
        Ok(Err(error)) => ImportSettlement::Failed(error),
        Err(BlockingTaskError::Busy) => ImportSettlement::Busy,
        Err(_) => ImportSettlement::Aborted,
    }
}

/// Map an owner settlement for an export serialize/publish into a stamped
/// [`ExportSettlement`].
#[must_use]
pub fn export_settlement(
    outcome: BlockingTaskSettlement<Result<(), ExportPublishError>>,
) -> ExportSettlement {
    match outcome.into_result() {
        Ok(Ok(())) => ExportSettlement::Published,
        Ok(Err(error)) => ExportSettlement::Failed(error),
        Err(BlockingTaskError::Busy) => ExportSettlement::Busy,
        Err(_) => ExportSettlement::Aborted,
    }
}

/// A user-facing sentence for a non-success `OpenFile` import outcome, or
/// `None` for success and user cancellation (a decision, not an error).
#[must_use]
pub fn import_failure_text(outcome: &PortalOutcome<FileResult>) -> Option<String> {
    let text = match outcome {
        PortalOutcome::Completed(_) | PortalOutcome::Cancelled(_) => return None,
        PortalOutcome::Unavailable => {
            "No file chooser on this system — a playlist cannot be imported here."
        }
        PortalOutcome::Unsupported => "This desktop's file portal cannot open files.",
        PortalOutcome::NotDeclared => "This build is not permitted to open a file chooser.",
        PortalOutcome::Denied => "The file chooser was refused permission to open.",
        PortalOutcome::ParentUnavailable => {
            "The file chooser could not be attached to this window."
        }
        PortalOutcome::Failed => "The file chooser failed — no playlist was imported.",
    };
    Some(text.to_string())
}

/// A user-facing sentence for a non-success `SaveFile` export outcome, or
/// `None` for success and user cancellation.
#[must_use]
pub fn export_failure_text(outcome: &DestinationOutcome) -> Option<String> {
    let text = match outcome {
        DestinationOutcome::Completed(_) | DestinationOutcome::Cancelled(_) => return None,
        DestinationOutcome::Denied => "Saving was refused permission.",
        DestinationOutcome::NotDeclared => "This build is not permitted to save files.",
        DestinationOutcome::Unsupported => "This desktop's file portal cannot save files.",
        DestinationOutcome::Unavailable => {
            "No file chooser on this system — a playlist cannot be exported here."
        }
        DestinationOutcome::ParentUnavailable => {
            "The save dialog could not be attached to this window."
        }
        DestinationOutcome::Failed => "The save dialog failed — no playlist was exported.",
    };
    Some(text.to_string())
}

/// Read a chosen playlist file with a bounded, identity-checked no-follow read,
/// then parse it by its extension. Runs on the owner worker.
fn read_and_parse(path: &Path) -> Result<ParseReport, ImportError> {
    let format = PlaylistFormat::from_extension(path)
        .ok_or(ImportError::Read(ReadRefusal::UnknownFormat))?;
    let bytes = read_document_bounded(path).map_err(ImportError::Read)?;
    let base = path.parent().unwrap_or_else(|| Path::new("/"));
    playlist_io::parse(&bytes, format, base).map_err(ImportError::Parse)
}

/// Open `path` `O_NOFOLLOW | O_NONBLOCK`, refuse a non-regular descriptor, read
/// at most the document cap plus one byte, and revalidate the descriptor size,
/// mirroring `persistence::read_state_text`.
fn read_document_bounded(path: &Path) -> Result<Vec<u8>, ReadRefusal> {
    read_file_bounded(path, playlist_io::MAX_DOCUMENT_BYTES as u64)
}

/// [`read_document_bounded`] with any size cap, for other documents the user
/// picks (an OPML subscription list).
pub(crate) fn read_file_bounded(path: &Path, cap: u64) -> Result<Vec<u8>, ReadRefusal> {
    use std::io::Read;
    #[cfg(unix)]
    use std::os::unix::fs::MetadataExt;
    let file = open_nofollow(path).map_err(|e| ReadRefusal::Io(e.to_string()))?;
    let meta = file
        .metadata()
        .map_err(|e| ReadRefusal::Io(e.to_string()))?;
    if !meta.is_file() {
        return Err(ReadRefusal::NotRegularFile);
    }
    let admitted = meta.len();
    if admitted > cap {
        return Err(ReadRefusal::TooLarge);
    }
    let mut bytes = Vec::new();
    (&file)
        .take(cap.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|e| ReadRefusal::Io(e.to_string()))?;
    if bytes.len() as u64 > cap {
        return Err(ReadRefusal::TooLarge);
    }
    let after = file
        .metadata()
        .map_err(|e| ReadRefusal::Io(e.to_string()))?;
    #[cfg(unix)]
    let identity_changed = meta.dev() != after.dev()
        || meta.ino() != after.ino()
        || meta.len() != after.len()
        || meta.mtime() != after.mtime()
        || meta.mtime_nsec() != after.mtime_nsec()
        || meta.ctime() != after.ctime()
        || meta.ctime_nsec() != after.ctime_nsec();
    #[cfg(not(unix))]
    let identity_changed =
        meta.len() != after.len() || meta.modified().ok() != after.modified().ok();
    if identity_changed || bytes.len() as u64 != admitted {
        return Err(ReadRefusal::IdentityChanged);
    }
    Ok(bytes)
}

#[cfg(unix)]
fn open_nofollow(path: &Path) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
}

#[cfg(not(unix))]
fn open_nofollow(path: &Path) -> std::io::Result<std::fs::File> {
    std::fs::File::open(path)
}

/// Serialize the snapshot rows and publish them through the grant's reservation.
/// Runs on the owner worker. Format and destination parent derive from the
/// grant's chosen path.
fn publish_playlist(
    grant: &DownloadDestination,
    paths: &[PathBuf],
) -> Result<(), ExportPublishError> {
    let dest = grant
        .to_path()
        .map_err(|e| ExportPublishError::UriDecode(e.to_string()))?;
    let format = PlaylistFormat::from_extension(&dest).unwrap_or(PlaylistFormat::M3u8);
    let dest_dir = dest.parent().unwrap_or_else(|| Path::new("/"));
    let bytes =
        playlist_io::serialize(paths, format, dest_dir).map_err(ExportPublishError::Serialize)?;
    let reserved = grant
        .reserve_file()
        .map_err(|e| ExportPublishError::PrePublication(e.to_string()))?;
    reserved.publish_with(|w| w.write_all(&bytes)).map_err(|e| {
        if e.was_published() {
            ExportPublishError::DurabilityUncertain(e.to_string())
        } else {
            ExportPublishError::PrePublication(e.to_string())
        }
    })
}
