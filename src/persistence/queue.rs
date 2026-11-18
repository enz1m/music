//! Persistence queue and lifecycle: scheduling, probe, acquire,
//! snapshot, and restore.

use super::{
    ExpectedSourceIdentity, LoadError, Origin, PersistedState, RadioCustomRec, RefusalKind,
    SharedAuthority, TrackAddedRec, TrackStatsRec, load_identified, serialize_pretty,
};
#[cfg(test)]
use super::{
    FORMAT_VERSION, PlaylistRec, SubscriptionRec, episode_resume_recs_from_app,
    playback_rec_from_app,
};
use super::{
    STATE_DESTINATION, SharedDocument, bounded_half, rebuild_track_added_at, track_stats_records,
};
use crate::MusicApp;
use crate::domain::{Playlist, Track};
use crate::podcasts::{FeedStatus, PodcastFeed};
use enzui_core::state_authority::StateAuthority;
use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

type TrackStateIndex = (Arc<Vec<TrackAddedRec>>, HashMap<PathBuf, u64>);

/// Orders background state writes without putting serialisation or filesystem
/// work back on the reducer thread.
///
/// Registration is one map insertion. A writer takes the separate writer lock
/// off-thread and re-checks its ticket: a superseded task skips entirely, and a
/// task already inside its atomic rename holds the lock so the newer write
/// follows it. Per destination the result is atomic and last-scheduled-write
/// wins, whatever order the runtime polls tasks in.
#[derive(Clone, Default, Debug)]
pub struct PersistenceQueue {
    inner: Arc<PersistenceQueueInner>,
}

#[derive(Default, Debug)]
struct PersistenceQueueInner {
    next_generation: AtomicU64,
    latest_by_destination: Mutex<HashMap<String, u64>>,
    writer: Mutex<()>,
    #[cfg(test)]
    injected_error: Mutex<Option<(std::io::ErrorKind, String)>>,
}

/// A registered intent to write one destination, with its ordering generation.
#[derive(Clone, Debug)]
pub struct PersistenceTicket {
    destination: String,
    generation: u64,
}

impl PersistenceTicket {
    #[must_use]
    pub const fn generation(&self) -> u64 {
        self.generation
    }
}

impl PersistenceQueue {
    /// Register the newest intended contents for one logical destination.
    pub fn register(&self, destination: impl Into<String>) -> PersistenceTicket {
        let destination = destination.into();
        let generation = self
            .inner
            .next_generation
            .fetch_add(1, Ordering::Relaxed)
            .wrapping_add(1);
        self.inner
            .latest_by_destination
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(destination.clone(), generation);
        PersistenceTicket {
            destination,
            generation,
        }
    }

    /// Run `write` only if `ticket` is still the newest registration for its
    /// destination. `Ok(false)` means a newer snapshot superseded it.
    pub fn execute(
        &self,
        ticket: &PersistenceTicket,
        write: impl FnOnce() -> std::io::Result<()>,
    ) -> std::io::Result<bool> {
        let _writer = self
            .inner
            .writer
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let is_latest = self
            .inner
            .latest_by_destination
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&ticket.destination)
            .copied()
            == Some(ticket.generation);
        if !is_latest {
            return Ok(false);
        }
        #[cfg(test)]
        if let Some((kind, diagnostic)) = self
            .inner
            .injected_error
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
        {
            return Err(std::io::Error::new(kind, diagnostic));
        }
        write()?;
        Ok(true)
    }

    #[cfg(test)]
    pub fn inject_write_error(&self, kind: std::io::ErrorKind, diagnostic: impl Into<String>) {
        *self
            .inner
            .injected_error
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some((kind, diagnostic.into()));
    }

    #[cfg(test)]
    pub fn clear_injected_write_error(&self) {
        *self
            .inner
            .injected_error
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
    }
}

/// The typed result of executing one persistence ticket.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PersistenceCompletion {
    /// This ticket wrote its complete document atomically.
    Saved,
    /// A newer ticket existed before this one reached the writer.
    Superseded,
    /// The newest ticket could not be written.
    Error(PersistenceFailure),
}

/// A persistence error with user-safe copy separated from raw diagnostics.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PersistenceFailure(enzui_core::persist::PersistError);

impl PersistenceFailure {
    fn from_io(error: std::io::Error) -> Self {
        Self(error.into())
    }

    #[cfg(test)]
    pub fn injected(kind: std::io::ErrorKind, diagnostic: impl Into<String>) -> Self {
        Self::from_io(std::io::Error::new(kind, diagnostic.into()))
    }

    /// Bounded, single-line text safe to render to the listener.
    #[must_use]
    pub fn safe_summary(&self) -> String {
        self.0.user_summary()
    }

    /// Unmodified backend detail for structured diagnostics only.
    #[must_use]
    pub fn diagnostic(&self) -> &str {
        self.0.diagnostic()
    }
}

/// An immutable document plus its place in the write order.
#[derive(Clone, Debug)]
pub struct PendingSave {
    ticket: PersistenceTicket,
    doc: SharedDocument,
    path: PathBuf,
    /// The retained authority every generation of this destination publishes
    /// through. Cloning the debt shares custody, never duplicates it.
    authority: SharedAuthority,
}

impl PendingSave {
    #[must_use]
    pub fn ticket(&self) -> &PersistenceTicket {
        &self.ticket
    }

    /// Serialise and write, honouring the queue's ordering. Called from a
    /// `Task` in production and from the blocking close drain — never from a
    /// reducer arm.
    pub fn execute(&self, queue: &PersistenceQueue) -> std::io::Result<bool> {
        queue.execute(&self.ticket, || {
            // Materialise here, after the queue confirmed this write is still
            // the newest, so the O(tracks) half stays off the reducer thread.
            let bytes = serialize_pretty(&self.doc.materialize())?;
            // Publish through the retained identity-bound authority, not
            // an unbound `write_serialized` path write.
            publish_through_authority(&self.authority, &bytes)
        })
    }

    /// [`Self::complete`] on the save worker rather than the awaiting
    /// executor: the write serialises the library and renames a file.
    ///
    /// Saves run one at a time and wait for a slot instead of being refused,
    /// because a refused save would lose the listener's newest state. A save
    /// the worker could not settle reports an error rather than nothing.
    pub fn complete_off_executor(
        self,
        queue: PersistenceQueue,
    ) -> impl std::future::Future<Output = PersistenceCompletion> + Send + 'static {
        use enzui_core::blocking_task::BlockingTaskOwner;
        use std::num::NonZeroUsize;
        static OWNER: std::sync::OnceLock<Option<BlockingTaskOwner>> = std::sync::OnceLock::new();
        let owner = OWNER
            .get_or_init(|| {
                BlockingTaskOwner::new(
                    "music-persist",
                    NonZeroUsize::MIN,
                    NonZeroUsize::new(4).expect("nonzero"),
                )
                .ok()
            })
            .as_ref();
        let job = match owner {
            Some(owner) => {
                Ok(owner.submit_waiting(self.ticket.generation(), move |_| self.complete(&queue)))
            }
            // Without a worker the save runs where it is awaited.
            None => Err((self, queue)),
        };
        async move {
            match job {
                Ok(job) => job.await.into_result().unwrap_or_else(|error| {
                    PersistenceCompletion::Error(PersistenceFailure::from_io(
                        std::io::Error::other(error.to_string()),
                    ))
                }),
                Err((pending, queue)) => pending.complete(&queue),
            }
        }
    }

    /// Execute without collapsing a skipped or failed write into success.
    #[must_use]
    pub fn complete(&self, queue: &PersistenceQueue) -> PersistenceCompletion {
        match self.execute(queue) {
            Ok(true) => PersistenceCompletion::Saved,
            Ok(false) => PersistenceCompletion::Superseded,
            Err(error) => PersistenceCompletion::Error(PersistenceFailure::from_io(error)),
        }
    }

    #[cfg(test)]
    #[must_use]
    pub fn document(&self) -> &SharedDocument {
        &self.doc
    }

    /// Re-register this document as the newest write for its destination, so a
    /// close drain supersedes every task that has not started yet.
    #[must_use]
    pub fn renewed(&self, queue: &PersistenceQueue) -> Self {
        Self {
            ticket: queue.register(self.ticket.destination.clone()),
            doc: self.doc.clone(),
            path: self.path.clone(),
            authority: Arc::clone(&self.authority),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Persist {
    path: PathBuf,
    /// The newest document confirmed on disk (or loaded at boot).
    last_saved: Option<SharedDocument>,
    /// The newest document registered for writing. Keeping this distinct from
    /// `last_saved` suppresses duplicate scheduling without pretending that
    /// registration made bytes durable.
    last_prepared: Option<SharedDocument>,
    /// The library `track_identity_revision` the shared first-seen table was
    /// built at, and the table itself. Reusing this `Arc` keeps the reducer's
    /// change check O(1).
    track_added_at: Option<(u64, Arc<Vec<TrackAddedRec>>)>,
    /// Play-stats counterpart of `track_added_at`, keyed on
    /// `Library::track_stats_revision`. A message that credits no play reuses
    /// the `Arc`; a miss walks the tracks once and clones only played paths.
    track_stats: Option<(u64, Arc<Vec<TrackStatsRec>>)>,
    /// Cached `path → added_at` index for [`Self::apply_track_state`], derived
    /// from `self.last_saved.track_added_at` and invalidated when that `Arc`
    /// changes allocation. A read-only cache, never a source of truth.
    track_state_index: Option<TrackStateIndex>,
    enabled: bool,
    /// The retained identity-bound publication authority. Present only
    /// for an enabled session whose acquisition succeeded; every save publishes
    /// through it. A disabled or authority-less session prepares no write.
    authority: Option<SharedAuthority>,
    /// The device/inode the read observed for the file at [`Self::path`], so
    /// an explicit Retire quarantines only that inode. `None` for an absent
    /// file or an untrusted identity, which cannot retire by pathname.
    retire_identity: Option<ExpectedSourceIdentity>,
}

impl Persist {
    #[must_use]
    pub fn inert() -> Self {
        Self {
            path: PathBuf::from("/nonexistent/music-app/state.json"),
            last_saved: None,
            last_prepared: None,
            track_added_at: None,
            track_stats: None,
            track_state_index: None,
            enabled: false,
            authority: None,
            retire_identity: None,
        }
    }

    fn enabled(path: PathBuf, last: Option<PersistedState>) -> Self {
        let last_saved = last.map(SharedDocument::from);
        Self {
            path,
            last_prepared: last_saved.clone(),
            last_saved,
            track_added_at: None,
            track_stats: None,
            track_state_index: None,
            enabled: true,
            authority: None,
            retire_identity: None,
        }
    }

    fn disabled(path: PathBuf) -> Self {
        Self {
            path,
            last_saved: None,
            last_prepared: None,
            track_added_at: None,
            track_stats: None,
            track_state_index: None,
            enabled: false,
            authority: None,
            retire_identity: None,
        }
    }

    /// Install the retained publication authority acquired at boot,
    /// binding it to this session. Only an enabled session with an authority
    /// prepares a write.
    pub fn install_authority(&mut self, authority: SharedAuthority) {
        self.authority = Some(authority);
    }

    /// Install the identity the read observed for the file at [`Self::path`],
    /// so an explicit Retire can refuse a later substitution.
    pub fn install_retire_identity(&mut self, identity: Option<ExpectedSourceIdentity>) {
        self.retire_identity = identity;
    }

    /// The path of this session's state file, for an explicit Retire.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The identity an explicit Retire must still observe before quarantining.
    /// `None` cannot retire by pathname.
    #[must_use]
    pub fn retire_identity(&self) -> Option<ExpectedSourceIdentity> {
        self.retire_identity
    }

    /// Whether custody was lost, so ordinary retry must reacquire off-thread
    /// rather than reopen the same authority.
    #[must_use]
    pub fn authority_lost_custody(&self) -> bool {
        self.authority.as_ref().is_some_and(|authority| {
            authority
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .has_lost_custody()
        })
    }

    /// The shared first-seen table for `library`, rebuilt only when a track has
    /// entered it since the last build.
    ///
    /// [`SharedDocument`] compares this collection by pointer, so returning
    /// the same `Arc` proves the table unchanged without reading it.
    pub fn track_added_at_for(
        &mut self,
        library: &crate::domain::Library,
    ) -> Arc<Vec<TrackAddedRec>> {
        let revision = library.track_identity_revision();
        if let Some((cached_revision, records)) = &self.track_added_at
            && *cached_revision == revision
        {
            return Arc::clone(records);
        }
        let previous = self
            .track_added_at
            .as_ref()
            .map(|(_, records)| Arc::clone(records))
            .or_else(|| {
                self.last_saved
                    .as_ref()
                    .map(|last| Arc::clone(&last.track_added_at))
            });
        let records = Arc::new(rebuild_track_added_at(
            library,
            previous.as_deref().map(Vec::as_slice),
        ));
        self.track_added_at = Some((revision, Arc::clone(&records)));
        records
    }

    /// Cached path-keyed history and ratings, including ratings for temporarily missing tracks.
    /// A revision change rebuilds the sparse union; an unchanged revision clones only its Arc.
    pub fn track_stats_for(&mut self, library: &crate::domain::Library) -> Arc<Vec<TrackStatsRec>> {
        let revision = library.track_stats_revision();
        if let Some((cached_revision, records)) = &self.track_stats
            && *cached_revision == revision
        {
            return Arc::clone(records);
        }
        let records = track_stats_records(library);
        let records = Arc::new(records);
        self.track_stats = Some((revision, Arc::clone(&records)));
        records
    }

    /// Adopt `app`'s persisted projection as "already on disk", without
    /// writing. Called once at boot, off any frame.
    pub fn rebaseline(&mut self, app: &MusicApp) {
        let track_added_at = self.track_added_at_for(&app.library);
        let track_stats = self.track_stats_for(&app.library);
        let baseline = SharedDocument::new(bounded_half(app), track_added_at, track_stats);
        self.last_saved = Some(baseline.clone());
        self.last_prepared = Some(baseline);
    }

    /// Restore a scanned track's original first-seen timestamp. The scanner's
    /// `now()` remains the value for genuinely new paths.
    pub fn apply_track_state(&mut self, track: &mut Track) {
        let Some(doc) = self.last_saved.as_ref() else {
            return;
        };
        let source = &doc.track_added_at;
        let need_rebuild = match &self.track_state_index {
            Some((cached_arc, _)) => !Arc::ptr_eq(cached_arc, source),
            None => true,
        };
        if need_rebuild {
            let index: HashMap<PathBuf, u64> = source
                .iter()
                .map(|rec| (rec.path.clone(), rec.added_at))
                .collect();
            self.track_state_index = Some((Arc::clone(source), index));
        }
        if let Some((_, index)) = &self.track_state_index
            && let Some(&added_at) = index.get(&track.path)
        {
            track.added_at = added_at;
        }
    }

    /// The reducer half of a save: if anything changed, return an immutable
    /// document registered in `queue`.
    ///
    /// Registration advances only `last_prepared`; the durable baseline
    /// advances in [`Self::acknowledge_saved`]. Nothing here serialises or
    /// touches the filesystem.
    pub fn prepare(
        &mut self,
        doc: impl Into<SharedDocument>,
        queue: &PersistenceQueue,
    ) -> Option<PendingSave> {
        if !self.enabled {
            return None;
        }
        // No authority means acquisition failed or was refused: never fall back
        // to an unbound path write. The session stays read-only.
        let authority = self.authority.clone()?;
        let doc = doc.into();
        if self
            .last_prepared
            .as_ref()
            .is_some_and(|last| doc.unchanged_from(last))
        {
            return None;
        }
        self.last_prepared = Some(doc.clone());
        Some(PendingSave {
            ticket: queue.register(STATE_DESTINATION),
            doc,
            path: self.path.clone(),
            authority,
        })
    }

    /// Advance the durable baseline only after the matching write succeeded.
    pub fn acknowledge_saved(&mut self, pending: &PendingSave) {
        self.last_saved = Some(pending.doc.clone());
        self.last_prepared = Some(pending.doc.clone());
        self.track_state_index = None;
    }

    #[cfg(test)]
    #[must_use]
    pub fn last_saved_for_test(&self) -> Option<&SharedDocument> {
        self.last_saved.as_ref()
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled
    }
}

/// Build the session's persistence from a probed document and origin.
///
/// Enabled for a restored, missing, or recovered origin; disabled
/// (byte-identical, read-only) for a future-version or refused origin.
#[must_use]
pub fn persist_for(path: &Path, doc: Option<&PersistedState>, origin: &Origin) -> Persist {
    match origin {
        Origin::Restored => Persist::enabled(path.to_path_buf(), doc.cloned()),
        Origin::Missing => Persist::enabled(path.to_path_buf(), None),
        Origin::ReadOnlyFutureVersion(_)
        | Origin::RefusedReadOnly(_)
        | Origin::PersistenceRootUnavailable(_) => Persist::disabled(path.to_path_buf()),
    }
}

#[cfg(test)]
type AcquireRace = Box<dyn FnMut(&Path)>;

#[cfg(test)]
thread_local! {
    static ACQUIRE_RACE: std::cell::RefCell<Option<AcquireRace>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
fn run_acquire_race(path: &Path) {
    ACQUIRE_RACE.with(|slot| {
        if let Some(mut action) = slot.borrow_mut().take() {
            action(path);
        }
    });
}

/// Acquire the identity-bound publication authority, then read the bounded
/// document, and bind the two to the same inode.
///
/// The authority opens first and anchors the read: a substitution between the
/// two opens returns neither document nor authority. Mutates nothing; corrupt
/// bytes stay read-only ([`RefusalKind::Corrupt`]) until an explicit Retire.
/// The fourth return is the identity the read observed, so a later
/// [`super::retire_and_reacquire`] quarantines exactly that inode; `None` for
/// an absent file or an untrusted identity.
pub fn acquire(
    path: &Path,
) -> (
    Option<PersistedState>,
    Origin,
    Option<StateAuthority>,
    Option<ExpectedSourceIdentity>,
) {
    // Open the authority first and retain its identity before any read, closing
    // the window where a read of one inode is authorised for another.
    let authority = match StateAuthority::open_or_create_tightening(path) {
        Ok(authority) => authority,
        // Group/other-accessible, symlinked, or an unsafe parent: read-only.
        Err(_) => {
            return (
                None,
                Origin::RefusedReadOnly(RefusalKind::Unreadable),
                None,
                None,
            );
        }
    };
    let held = authority
        .identity()
        .map(|identity| (identity.device(), identity.inode()));

    // Test seam: a substitution in the window between the authority open and
    // the read, the race the identity check must refuse.
    #[cfg(test)]
    run_acquire_race(path);

    let (result, read_identity) = load_identified(path);
    // The read must have observed the same inode the authority retained, or both
    // must have observed absence for a create-on-first-publication authority.
    let coherent = held == read_identity;
    // Only a coherent read yields a Retire identity: a substitution refusal, or
    // an absent file, retains no inode a later Retire could trust.
    let retire_identity = if coherent {
        read_identity.map(|(device, inode)| ExpectedSourceIdentity::from_dev_ino(device, inode))
    } else {
        None
    };

    match result {
        Ok(doc) if coherent => (
            Some(doc),
            Origin::Restored,
            Some(authority),
            retire_identity,
        ),
        Err(LoadError::Missing) if coherent => (None, Origin::Missing, Some(authority), None),
        // Corrupt bytes stay read-only until an explicit Retire, which moves
        // exactly this inode.
        Err(LoadError::Corrupt(_)) => (
            None,
            Origin::RefusedReadOnly(RefusalKind::Corrupt),
            None,
            retire_identity,
        ),
        Err(LoadError::Unsupported(version)) => (
            None,
            Origin::ReadOnlyFutureVersion(version),
            None,
            retire_identity,
        ),
        Err(LoadError::TooLarge(_)) => (
            None,
            Origin::RefusedReadOnly(RefusalKind::TooLarge),
            None,
            retire_identity,
        ),
        Err(LoadError::OverLimit(_)) => (
            None,
            Origin::RefusedReadOnly(RefusalKind::OverLimit),
            None,
            retire_identity,
        ),
        Err(LoadError::Unreadable(_)) => (
            None,
            Origin::RefusedReadOnly(RefusalKind::Unreadable),
            None,
            retire_identity,
        ),
        // A read that would restore or create, but whose descriptor did not
        // match the retained authority: a substitution slipped between the two
        // opens. Refuse document, authority, and any Retire identity.
        Ok(_) | Err(LoadError::Missing) => (
            None,
            Origin::RefusedReadOnly(RefusalKind::Unreadable),
            None,
            None,
        ),
    }
}

/// Open an explicit path, acquiring the publication authority. No bytes are
/// written during startup.
pub fn open_at(path: &Path) -> (Option<PersistedState>, Persist, Origin) {
    let (doc, origin, authority, retire_identity) = acquire(path);
    let mut persist = persist_for(path, doc.as_ref(), &origin);
    if let Some(authority) = authority {
        persist.install_authority(Arc::new(Mutex::new(authority)));
    }
    persist.install_retire_identity(retire_identity);
    (doc, persist, origin)
}

/// Publish `bytes` through the retained authority, never reopening after
/// custody is lost and never reporting a rotated-but-undurable write as saved.
///
/// Both a lost-custody refusal and a post-rotation durability failure return an
/// error, so the completion path never advances the durable baseline for bytes
/// that are not safely named at the destination.
pub(super) fn publish_through_authority(
    authority: &SharedAuthority,
    bytes: &[u8],
) -> std::io::Result<()> {
    let mut authority = authority
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if authority.has_lost_custody() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "state authority lost custody; refusing to reopen",
        ));
    }
    authority
        .publish(bytes)
        .map(|_identity| ())
        .map_err(|error| std::io::Error::new(error.io_error().kind(), error.to_string()))
}

#[cfg(test)]
#[must_use]
pub fn snapshot(app: &MusicApp) -> PersistedState {
    let mut track_added_at: Vec<TrackAddedRec> = app
        .library
        .tracks
        .values()
        .map(|track| TrackAddedRec {
            path: track.path.clone(),
            added_at: track.added_at,
        })
        .collect();
    track_added_at.sort_by(|a, b| a.path.cmp(&b.path));

    let track_stats = track_stats_records(&app.library);

    PersistedState {
        version: FORMAT_VERSION,
        folders: app.library.folders.clone(),
        playlists: app
            .library
            .playlists
            .iter()
            .map(|playlist| PlaylistRec {
                id: playlist.id,
                name: playlist.name.clone(),
                track_paths: playlist.track_paths.clone(),
            })
            .collect(),
        subscriptions: app
            .subscriptions
            .iter()
            .map(|feed| SubscriptionRec {
                feed_url: feed.feed_url.clone(),
                title: feed.title.clone(),
                description: feed.description.clone(),
            })
            .collect(),
        // Legacy u64 `radio_favorites` is always empty in a new snapshot; the
        // durable set is `radio_favorites_uuids`.
        radio_favorites: Vec::new(),
        radio_favorites_uuids: {
            let mut uuids: Vec<String> = app.radio.favorites.iter().cloned().collect();
            uuids.sort();
            uuids
        },
        track_added_at,
        next_playlist_seq: app.next_playlist_seq,
        playback: playback_rec_from_app(app),
        track_stats,
        episode_resume: episode_resume_recs_from_app(app),
        podcast_country: app.podcast_charts.chosen_country.clone(),
        radio_custom_stations: radio_custom_recs_from_app(app),
    }
}

/// The most stations a user can add by stream URL.
pub const MAX_CUSTOM_STATIONS: usize = 200;

/// The stations the user added by stream URL, for the document.
#[must_use]
pub fn radio_custom_recs_from_app(app: &MusicApp) -> Vec<RadioCustomRec> {
    app.radio
        .custom
        .iter()
        .take(MAX_CUSTOM_STATIONS)
        .map(|s| RadioCustomRec {
            name: s.name.clone(),
            stream_url: s.stream_url.clone(),
        })
        .collect()
}

/// Apply only persisted state; catalogs, fetched episodes and scanned metadata
/// remain runtime concerns.
pub fn restore(app: &mut MusicApp, doc: &PersistedState) {
    app.library.folders = dedup_paths(&doc.folders);
    let playlists = doc
        .playlists
        .iter()
        .map(|rec| Playlist {
            id: rec.id,
            name: rec.name.clone(),
            track_paths: dedup_paths(&rec.track_paths),
            track_ids: Vec::new(),
        })
        .collect();
    app.library.restore_playlists(playlists);
    if let Some(code) = doc
        .podcast_country
        .as_deref()
        .filter(|code| crate::podcasts::charts::valid_country(code))
    {
        app.podcast_charts.country = code.to_owned();
        app.podcast_charts.chosen_country = Some(code.to_owned());
    }
    app.next_playlist_seq = doc.next_playlist_seq.max(1);

    let mut subscriptions = Vec::new();
    let mut urls = BTreeSet::new();
    for rec in &doc.subscriptions {
        let url = rec.feed_url.trim();
        if url.is_empty() || !urls.insert(url.to_owned()) {
            continue;
        }
        subscriptions.push(PodcastFeed {
            feed_url: url.to_owned(),
            title: if rec.title.trim().is_empty() {
                url.to_owned()
            } else {
                rec.title.clone()
            },
            description: rec.description.clone(),
            // Channel and episode metadata are re-derived by the boot refetch,
            // not persisted, so they start empty.
            episodes: Vec::new(),
            status: FeedStatus::Loaded,
            ..PodcastFeed::default()
        });
    }
    app.subscriptions = subscriptions;

    // Resume points need a non-blank `(feed_url, guid)` and a positive finite
    // position; the last duplicate wins.
    app.episode_resume.clear();
    for rec in &doc.episode_resume {
        let feed_url = rec.feed_url.trim();
        let guid = rec.guid.trim();
        if feed_url.is_empty() || guid.is_empty() {
            continue;
        }
        if !rec.position_secs.is_finite() || rec.position_secs <= 0.0 {
            continue;
        }
        app.episode_resume
            .insert((feed_url.to_owned(), guid.to_owned()), rec.position_secs);
    }

    // Favourites persist by durable UUID; their records are fetched by UUID
    // once the catalog service runs, and no reload drops them. Legacy u64
    // ids identified a hard-coded station position and are dropped.
    for uuid in &doc.radio_favorites_uuids {
        if !uuid.trim().is_empty() && !uuid.starts_with(crate::radio::CUSTOM_PREFIX) {
            app.radio.favorites.insert(uuid.clone());
        }
    }
    // Stations added by stream URL, only ones the radio policy admits.
    app.radio.custom = doc
        .radio_custom_stations
        .iter()
        .take(MAX_CUSTOM_STATIONS)
        .filter(|rec| {
            crate::radio_catalog::REMOTE_POLICY
                .check_url(&rec.stream_url)
                .is_ok()
        })
        .map(|rec| crate::radio::custom_station(&rec.name, &rec.stream_url))
        .collect();

    // The reducer keeps default playback state until the boot scan resolves
    // paths to ids; `MusicApp::finalize_pending_playback_session` drains this.
    app.pending_playback_session = if doc.playback.is_empty() {
        None
    } else {
        Some(doc.playback.clone())
    };

    app.library.restore_ratings(
        doc.track_stats
            .iter()
            .map(|record| (record.path.clone(), record.rating)),
    );
    app.rate_policy = doc.playback.rate;
    (app.eq, app.rg) = doc.playback.audio.states();
    app.pending_track_stats = if doc.track_stats.is_empty() {
        None
    } else {
        Some(doc.track_stats.clone())
    };
}

fn dedup_paths(paths: &[PathBuf]) -> Vec<PathBuf> {
    let mut seen = BTreeSet::new();
    paths
        .iter()
        .filter(|path| seen.insert((*path).clone()))
        .cloned()
        .collect()
}
