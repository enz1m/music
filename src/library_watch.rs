//! Automatic library reconciliation: watch configured folders and route
//! filesystem changes through one bounded, generation-checked rescan path.
//!
//! Three collaborators, one owner:
//!
//! * a [`DirectoryWatchService`] over an injected backend — `inotify` on a
//!   normal Linux runtime, an in-memory backend in controls and unsupported
//!   builds;
//! * a [`RootScanProvider`] that performs one bounded recursive scan of a
//!   configured root and reports a typed [`ScanCompletion`];
//! * a [`BlockingTaskOwner`] the scan runs on, so a directory walk never
//!   occupies the UI executor.
//!
//! The watcher coalesces every delivery burst per configured root into one
//! debounce deadline and one pending rescan intent, keeps at most one scan per
//! root in flight, arms exactly one follow-up for changes seen during a scan,
//! and rejects stale or removed-root completions by monotonic generation.
//! Only a fully successful ([`ScanCompletion::Complete`]) scan is authoritative
//! for removals; a vanished, refused, or partial scan never interprets absence
//! as deletion.

use std::collections::{HashMap, HashSet};
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use enzui_core::blocking_task::{BlockingTask, BlockingTaskOwner};
use enzui_core::file_tree_watch::{
    DirectoryChange, DirectoryWatchBackend, DirectoryWatchService, WatchDelivery, WatchId,
    WatchOutcome, WatchRefusal,
};
use iced::Subscription;

use crate::art::CoverCache;
use crate::domain::Track;
use crate::scanner;

/// Debounce window applied to a delivery burst before a rescan is dispatched.
pub const DEBOUNCE: Duration = Duration::from_millis(400);

/// Cadence of the coordinator's own wake while a debounce or retry is pending.
pub const TICK: Duration = Duration::from_millis(200);

/// Retry backoff after a scan submission is refused for a full worker queue.
pub const BUSY_RETRY: Duration = Duration::from_millis(300);

/// Total directory-watch cap this service declares across every configured
/// root. Recursive coverage above this degrades to a partial state rather than
/// silently dropping watches.
pub const MAX_WATCHES: usize = 256;

/// Bounds one recursive scan of a configured root.
#[derive(Clone, Copy, Debug)]
pub struct ScanLimits {
    /// Maximum directories traversed.
    pub max_directories: usize,
    /// Maximum aggregate bytes of every visited path.
    pub max_path_bytes: usize,
    /// Maximum non-directory entries inspected.
    pub max_files_visited: usize,
    /// Maximum tracks admitted.
    pub max_tracks: usize,
    /// Maximum aggregate bytes of admitted track metadata strings.
    pub max_metadata_bytes: usize,
    /// Maximum traversal depth below the root.
    pub max_depth: usize,
    /// Wall-clock budget for the whole scan.
    pub max_duration: Duration,
}

impl Default for ScanLimits {
    fn default() -> Self {
        Self {
            max_directories: 4096,
            max_path_bytes: 1 << 20,
            max_files_visited: 100_000,
            max_tracks: 50_000,
            max_metadata_bytes: 8 << 20,
            max_depth: 32,
            max_duration: Duration::from_secs(30),
        }
    }
}

/// Why a bounded scan stopped short of an authoritative traversal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RefusalReason {
    /// The directory cap was reached.
    Directories,
    /// The aggregate path-byte cap was reached.
    PathBytes,
    /// The files-visited cap was reached.
    FilesVisited,
    /// The admitted-tracks cap was reached.
    Tracks,
    /// The metadata-byte cap was reached.
    MetadataBytes,
    /// The traversal-depth cap was reached.
    Depth,
    /// The wall-clock budget was exhausted.
    Duration,
    /// Cooperative cancellation was requested.
    Cancelled,
    /// A directory below the root could not be read.
    Unreadable,
}

/// Typed outcome of a bounded scan.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScanCompletion {
    /// The whole tree was traversed. Authoritative for removals; an empty
    /// successful folder is distinct from every other arm here.
    Complete,
    /// The root itself is gone or is no longer a directory (deleted mount).
    Vanished,
    /// A bound stopped the traversal, or a subdirectory was unreadable. Never
    /// authoritative for removals.
    Refused(RefusalReason),
}

/// One bounded scan of a configured root.
#[derive(Clone, Debug)]
pub struct RootScan {
    /// The configured root this scan owns.
    pub root: PathBuf,
    /// Monotonic generation stamped at dispatch.
    pub generation: u64,
    /// Tracks admitted within the caps.
    pub tracks: Vec<Track>,
    /// Admitted audio candidates whose metadata or decode probe failed.
    ///
    /// Each is classified [`crate::domain::TrackAvailability::Undecodable`]
    /// per file, so a Complete scan can still classify absent siblings as
    /// `Missing`.
    pub undecodable: Vec<PathBuf>,
    /// Every directory traversed, root first, needed for recursive watch
    /// coverage.
    pub directories: Vec<PathBuf>,
    /// The typed completion.
    pub completion: ScanCompletion,
}

impl RootScan {
    /// Whether this scan is authoritative for removals.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.completion == ScanCompletion::Complete
    }
}

fn metadata_bytes(track: &Track) -> usize {
    track.title.len()
        + track.artist.len()
        + track.album.len()
        + track.album_artist.len()
        + track.genre.len()
        + track.format.len()
        + track.path.as_os_str().len()
}

/// Perform one bounded recursive scan of `root`.
///
/// Traversal is iterative and does not follow directory symlinks (which could
/// escape the root or loop indefinitely); a file symlink is admitted only when
/// its canonical target stays under the canonical root. A completely
/// successful traversal returns [`ScanCompletion::Complete`] even when it found
/// no audio; a missing root returns [`ScanCompletion::Vanished`]; any bound or
/// an unreadable subdirectory returns [`ScanCompletion::Refused`].
pub fn scan_root_bounded(
    root: &Path,
    generation: u64,
    limits: &ScanLimits,
    is_cancelled: &dyn Fn() -> bool,
) -> RootScan {
    let start = Instant::now();
    let mut tracks: Vec<Track> = Vec::new();
    let mut undecodable: Vec<PathBuf> = Vec::new();
    let mut directories: Vec<PathBuf> = Vec::new();
    let mut covers = CoverCache::new();
    let mut path_bytes = 0usize;
    let mut meta_bytes = 0usize;
    let mut files_visited = 0usize;

    let done = |root: &Path, tracks, undecodable, directories, completion| RootScan {
        root: root.to_path_buf(),
        generation,
        tracks,
        undecodable,
        directories,
        completion,
    };

    let root_meta = match std::fs::symlink_metadata(root) {
        Ok(m) => m,
        Err(_) => {
            return done(
                root,
                tracks,
                undecodable,
                directories,
                ScanCompletion::Vanished,
            );
        }
    };
    if !root_meta.is_dir() {
        return done(
            root,
            tracks,
            undecodable,
            directories,
            ScanCompletion::Vanished,
        );
    }
    let canon_root = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());

    let mut stack: Vec<(PathBuf, usize)> = vec![(root.to_path_buf(), 0)];
    let mut completion = ScanCompletion::Complete;

    'outer: while let Some((dir, depth)) = stack.pop() {
        if is_cancelled() {
            completion = ScanCompletion::Refused(RefusalReason::Cancelled);
            break;
        }
        if start.elapsed() > limits.max_duration {
            completion = ScanCompletion::Refused(RefusalReason::Duration);
            break;
        }
        if directories.len() >= limits.max_directories {
            completion = ScanCompletion::Refused(RefusalReason::Directories);
            break;
        }
        let dir_bytes = dir.as_os_str().len();
        if path_bytes.saturating_add(dir_bytes) > limits.max_path_bytes {
            completion = ScanCompletion::Refused(RefusalReason::PathBytes);
            break;
        }
        path_bytes += dir_bytes;
        directories.push(dir.clone());

        let read = match std::fs::read_dir(&dir) {
            Ok(rd) => rd,
            Err(_) => {
                if dir == root {
                    completion = ScanCompletion::Vanished;
                    break;
                }
                // An unreadable subdirectory makes membership unknowable, so
                // the scan is no longer authoritative for removals.
                completion = ScanCompletion::Refused(RefusalReason::Unreadable);
                continue;
            }
        };

        for entry in read {
            let entry = match entry {
                Ok(e) => e,
                Err(_) => {
                    completion = ScanCompletion::Refused(RefusalReason::Unreadable);
                    continue;
                }
            };
            let path = entry.path();
            // Charge every visited entry path, directory or file, before any
            // metadata or tag work, so the byte cap bounds the whole traversal
            // rather than only admitted tracks.
            let entry_bytes = path.as_os_str().len();
            if path_bytes.saturating_add(entry_bytes) > limits.max_path_bytes {
                completion = ScanCompletion::Refused(RefusalReason::PathBytes);
                break 'outer;
            }
            path_bytes = path_bytes.saturating_add(entry_bytes);

            let file_type = match entry.file_type() {
                Ok(t) => t,
                Err(_) => {
                    completion = ScanCompletion::Refused(RefusalReason::Unreadable);
                    continue;
                }
            };

            let (is_dir, is_file) = if file_type.is_symlink() {
                match std::fs::canonicalize(&path) {
                    Ok(target) if target.starts_with(&canon_root) => {
                        match std::fs::metadata(&path) {
                            Ok(m) => (m.is_dir(), m.is_file()),
                            Err(_) => (false, false),
                        }
                    }
                    // A symlink resolving outside the root, or one that cannot
                    // be resolved, is not followed.
                    _ => (false, false),
                }
            } else {
                (file_type.is_dir(), file_type.is_file())
            };

            if is_dir && !file_type.is_symlink() {
                if depth + 1 > limits.max_depth {
                    completion = ScanCompletion::Refused(RefusalReason::Depth);
                    continue;
                }
                stack.push((path, depth + 1));
            } else if is_file {
                files_visited += 1;
                if files_visited > limits.max_files_visited {
                    completion = ScanCompletion::Refused(RefusalReason::FilesVisited);
                    break 'outer;
                }
                if !scanner::is_audio_file(&path) {
                    continue;
                }
                if tracks.len() >= limits.max_tracks {
                    completion = ScanCompletion::Refused(RefusalReason::Tracks);
                    break 'outer;
                }
                match scanner::scan_track_with_covers(&path, None, &mut covers) {
                    Ok(track) => {
                        let cost = metadata_bytes(&track);
                        if meta_bytes.saturating_add(cost) > limits.max_metadata_bytes {
                            completion = ScanCompletion::Refused(RefusalReason::MetadataBytes);
                            break 'outer;
                        }
                        meta_bytes = meta_bytes.saturating_add(cost);
                        tracks.push(track);
                    }
                    // A failed probe marks only this file `Undecodable`; the row
                    // survives a Complete reconcile.
                    Err(_) => {
                        undecodable.push(path.clone());
                    }
                }
            }
        }
    }

    done(root, tracks, undecodable, directories, completion)
}

/// Performs bounded root scans.
pub trait RootScanProvider: Send + Sync + 'static {
    /// Scan `root`, stamping the result with `generation`.
    fn scan(&self, root: &Path, generation: u64, is_cancelled: &dyn Fn() -> bool) -> RootScan;
}

/// Production provider: a real bounded filesystem walk.
#[derive(Clone, Copy, Debug, Default)]
pub struct FilesystemScanProvider {
    limits: ScanLimits,
}

impl RootScanProvider for FilesystemScanProvider {
    fn scan(&self, root: &Path, generation: u64, is_cancelled: &dyn Fn() -> bool) -> RootScan {
        scan_root_bounded(root, generation, &self.limits, is_cancelled)
    }
}

/// Whether automatic updates are trustworthy for a configured root.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RootAvailability {
    /// Every directory in the last complete scan is watched.
    Live,
    /// Watches are incomplete (cap exceeded or an install failed); the last
    /// safe watch set is retained and a manual rescan is the honest recovery.
    Partial(String),
    /// The root vanished or became unreadable; its last known tracks are kept.
    Unavailable(String),
    /// The backend reported a non-fatal probe error; the watch stays live.
    ProbeFailed(String),
}

/// One dispatch the coordinator asks its owner to run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScanDispatch {
    /// The root to scan.
    pub root: PathBuf,
    /// The generation to stamp.
    pub generation: u64,
}

struct RootState {
    in_flight: Option<u64>,
    follow_up: bool,
    due_at: Option<Instant>,
    watches: Vec<(WatchId, PathBuf)>,
    last_safe_dirs: Vec<PathBuf>,
    availability: RootAvailability,
    /// A manual Rescan is awaiting a terminal, so the next fully-settled scan
    /// posts one completion status. Coalesced across in-flight follow-ups.
    manual: bool,
}

impl RootState {
    fn new() -> Self {
        Self {
            in_flight: None,
            follow_up: false,
            due_at: None,
            watches: Vec::new(),
            last_safe_dirs: Vec::new(),
            availability: RootAvailability::Live,
            manual: false,
        }
    }
}

/// Owns the directory-watch service, the scan owner, and per-root scheduling.
pub struct LibraryWatcher {
    service: DirectoryWatchService,
    provider: Arc<dyn RootScanProvider>,
    owner: BlockingTaskOwner,
    roots: HashMap<PathBuf, RootState>,
    next_generation: u64,
}

impl std::fmt::Debug for LibraryWatcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LibraryWatcher")
            .field("roots", &self.roots.len())
            .field("active_watches", &self.service.active_watches())
            .field("next_generation", &self.next_generation)
            .finish_non_exhaustive()
    }
}

impl LibraryWatcher {
    /// Build a watcher over an explicit backend, provider, and worker owner.
    #[must_use]
    pub fn with_parts(
        backend: Arc<dyn DirectoryWatchBackend>,
        provider: Arc<dyn RootScanProvider>,
        owner: BlockingTaskOwner,
    ) -> Self {
        let service = DirectoryWatchService::with_config(
            backend,
            MAX_WATCHES,
            enzui_core::file_tree_watch::DEFAULT_MAX_PENDING_PER_WATCH,
            enzui_core::file_tree_watch::DEFAULT_MAX_PATH_BYTES,
        );
        Self {
            service,
            provider,
            owner,
            roots: HashMap::new(),
            next_generation: 1,
        }
    }

    /// Build the shipping watcher: an `inotify` backend on Linux (falling back
    /// to the no-op backend when the FD cannot be created), a bounded
    /// filesystem scan provider, and a one-worker scan owner.
    #[must_use]
    pub fn shipping() -> Self {
        let backend = default_backend();
        let owner = scan_owner();
        Self::with_parts(backend, Arc::new(FilesystemScanProvider::default()), owner)
    }

    /// The event-driven directory-change subscription.
    pub fn subscription(&self) -> Subscription<Vec<WatchDelivery>> {
        self.service.subscription()
    }

    /// Whether the coordinator has debounce or retry work pending, so its wake
    /// tick should be armed this frame.
    #[must_use]
    pub fn wants_tick(&self) -> bool {
        self.roots.values().any(|r| r.due_at.is_some())
    }

    /// One bounded, persistent status line while any root is not fully live,
    /// or `None` when every configured root is live.
    #[must_use]
    pub fn status_notice(&self) -> Option<String> {
        let mut partial = 0usize;
        let mut unavailable = 0usize;
        for state in self.roots.values() {
            match state.availability {
                RootAvailability::Live => {}
                RootAvailability::Partial(_) => partial += 1,
                RootAvailability::Unavailable(_) => unavailable += 1,
                RootAvailability::ProbeFailed(_) => partial += 1,
            }
        }
        if partial == 0 && unavailable == 0 {
            return None;
        }
        Some(
            "Automatic library updates are unavailable or partial. \
             Use Rescan Library to refresh."
                .to_string(),
        )
    }

    /// Register a configured root and arm an immediate authoritative scan.
    ///
    /// The root's own directory watch is installed only after that first
    /// complete scan reconciles the watch set, so a freshly added folder is
    /// scanned before it is watched. When a scan is already in flight for the
    /// root, one follow-up is armed instead of restarting it.
    pub fn register_root(&mut self, root: PathBuf, now: Instant) {
        let state = self.roots.entry(root).or_insert_with(RootState::new);
        if state.in_flight.is_some() {
            state.follow_up = true;
        } else {
            state.due_at = Some(now);
        }
    }

    /// Arm an immediate authoritative rescan of a configured root, as the
    /// manual `Rescan Library` does.
    ///
    /// Shares the watcher's generation and reconciliation path, and flags the
    /// root so its next settled scan posts one terminal status.
    pub fn request_rescan(&mut self, root: PathBuf, now: Instant) {
        self.register_root(root.clone(), now);
        if let Some(state) = self.roots.get_mut(&root) {
            state.manual = true;
        }
    }

    /// Whether a manual Rescan of `root` has fully settled (no scan in flight
    /// and no further deadline armed), consuming the flag so its terminal
    /// status is posted exactly once. Automatic scans never set it.
    pub fn take_manual_if_settled(&mut self, root: &Path) -> bool {
        let Some(state) = self.roots.get_mut(root) else {
            return false;
        };
        if state.manual && state.in_flight.is_none() && state.due_at.is_none() {
            state.manual = false;
            true
        } else {
            false
        }
    }

    /// Forget a root and unwatch every directory it owned.
    pub fn remove_root(&mut self, root: &Path) {
        if let Some(state) = self.roots.remove(root) {
            for (id, _) in state.watches {
                self.service.unwatch(id);
            }
        }
    }

    /// Coalesce a delivery burst into per-root debounce deadlines and typed
    /// policy for overflow, vanished, and probe-failed outcomes.
    pub fn on_deliveries(&mut self, deliveries: &[WatchDelivery], now: Instant) {
        for delivery in deliveries {
            let Some(root) = self.owning_root(&delivery.path) else {
                continue;
            };
            match &delivery.outcome {
                WatchOutcome::Changes(changes) => {
                    let significant = changes.iter().any(is_significant_change);
                    if significant {
                        self.arm_rescan(&root, now);
                    }
                }
                WatchOutcome::Overflow => {
                    self.arm_rescan(&root, now);
                }
                WatchOutcome::Vanished => {
                    if let Some(state) = self.roots.get_mut(&root) {
                        state.availability = RootAvailability::Unavailable(
                            "A watched folder is no longer available.".to_string(),
                        );
                    }
                }
                WatchOutcome::ProbeFailed(msg) => {
                    if let Some(state) = self.roots.get_mut(&root) {
                        state.availability = RootAvailability::ProbeFailed(msg.clone());
                    }
                }
            }
        }
    }

    fn arm_rescan(&mut self, root: &Path, now: Instant) {
        let Some(state) = self.roots.get_mut(root) else {
            return;
        };
        if state.in_flight.is_some() {
            // One follow-up is armed for the whole in-flight window.
            state.follow_up = true;
        } else {
            // Trailing debounce: the deadline moves out with each burst.
            state.due_at = Some(now + DEBOUNCE);
        }
    }

    /// Return the roots whose debounce or retry deadline has elapsed, marking
    /// each one in flight under a fresh generation. A root already in flight is
    /// never dispatched again here.
    pub fn dispatch_due(&mut self, now: Instant) -> Vec<ScanDispatch> {
        let mut out = Vec::new();
        for (root, state) in &mut self.roots {
            if state.in_flight.is_some() {
                continue;
            }
            let Some(due) = state.due_at else {
                continue;
            };
            if due > now {
                continue;
            }
            state.due_at = None;
            let generation = self.next_generation;
            self.next_generation = self.next_generation.wrapping_add(1);
            state.in_flight = Some(generation);
            out.push(ScanDispatch {
                root: root.clone(),
                generation,
            });
        }
        out
    }

    /// Submit one scan to the bounded owner. The returned future settles once;
    /// dropping it cancels the generation but the running walk keeps its slot.
    pub fn submit_scan(&self, root: PathBuf, generation: u64) -> BlockingTask<RootScan> {
        let provider = Arc::clone(&self.provider);
        self.owner.submit(generation, move |ctx| {
            provider.scan(&root, generation, &|| ctx.is_cancelled())
        })
    }

    /// Whether `scan` is the live in-flight generation for its root.
    ///
    /// A completion for a removed root, or for a generation that has been
    /// superseded, is inert. On acceptance the in-flight marker is cleared and,
    /// when a change was seen mid-scan, one follow-up deadline is armed.
    pub fn accept_completion(&mut self, scan: &RootScan, now: Instant) -> bool {
        let Some(state) = self.roots.get_mut(&scan.root) else {
            return false;
        };
        if state.in_flight != Some(scan.generation) {
            return false;
        }
        state.in_flight = None;
        if state.follow_up {
            state.follow_up = false;
            state.due_at = Some(now);
        }
        true
    }

    /// Retain intent after a refused (busy/cancelled/shutdown) submission and
    /// schedule a bounded retry rather than spinning.
    pub fn on_scan_refused(&mut self, root: &Path, generation: u64, now: Instant) {
        if let Some(state) = self.roots.get_mut(root)
            && state.in_flight == Some(generation)
        {
            state.in_flight = None;
            state.due_at = Some(now + BUSY_RETRY);
        }
    }

    /// Reconcile the exact directory watch set for `root` to the traversed
    /// directories of a complete scan.
    ///
    /// New watches install before any stale watch is removed, and the exact
    /// prior safe set survives every failure: if temporary service capacity
    /// cannot hold the old and new sets at once, or an install fails, every
    /// newly installed id is rolled back, the prior set is left intact, and the
    /// root becomes [`RootAvailability::Partial`]. A build whose backend refuses
    /// with [`WatchRefusal::Unsupported`] falls back to manual refresh silently,
    /// leaving availability unchanged. Only when every desired addition
    /// succeeds are stale watches removed, the exact desired state installed,
    /// and `last_safe_dirs` updated.
    pub fn reconcile_watches(&mut self, root: &Path, directories: &[PathBuf]) {
        if !self.roots.contains_key(root) {
            return;
        }
        let mut desired: Vec<PathBuf> = Vec::new();
        let mut seen: HashSet<&Path> = HashSet::new();
        for dir in directories {
            if seen.insert(dir.as_path()) {
                desired.push(dir.clone());
            }
        }
        let desired_set: HashSet<&Path> = desired.iter().map(PathBuf::as_path).collect();

        let current: Vec<(WatchId, PathBuf)> = self
            .roots
            .get(root)
            .map(|s| s.watches.clone())
            .unwrap_or_default();
        let current_set: HashSet<&Path> = current.iter().map(|(_, p)| p.as_path()).collect();

        let additions: Vec<PathBuf> = desired
            .iter()
            .filter(|d| !current_set.contains(d.as_path()))
            .cloned()
            .collect();
        let stale: Vec<WatchId> = current
            .iter()
            .filter(|(_, p)| !desired_set.contains(p.as_path()))
            .map(|(id, _)| *id)
            .collect();

        let others: usize = self
            .roots
            .iter()
            .filter(|(other, _)| other.as_path() != root)
            .map(|(_, s)| s.watches.len())
            .sum();
        // Install-before-remove needs room for the old and new sets together.
        let peak = others + current.len() + additions.len();
        if peak > self.service.max_watches() {
            if let Some(state) = self.roots.get_mut(root) {
                state.availability = RootAvailability::Partial(
                    "Automatic updates cover only part of this folder.".to_string(),
                );
            }
            return;
        }

        let mut installed: Vec<(WatchId, PathBuf)> = Vec::with_capacity(additions.len());
        for dir in &additions {
            match self.service.watch(dir.clone()) {
                Ok(id) => installed.push((id, dir.clone())),
                Err(WatchRefusal::Unsupported(_)) => {
                    for (id, _) in installed {
                        self.service.unwatch(id);
                    }
                    return;
                }
                Err(_) => {
                    for (id, _) in installed {
                        self.service.unwatch(id);
                    }
                    if let Some(state) = self.roots.get_mut(root) {
                        state.availability = RootAvailability::Partial(
                            "Some folder watches could not be installed.".to_string(),
                        );
                    }
                    return;
                }
            }
        }

        for id in stale {
            self.service.unwatch(id);
        }
        let mut next: Vec<(WatchId, PathBuf)> = current
            .into_iter()
            .filter(|(_, p)| desired_set.contains(p.as_path()))
            .collect();
        next.extend(installed);
        if let Some(state) = self.roots.get_mut(root) {
            state.watches = next;
            state.last_safe_dirs = desired;
            state.availability = RootAvailability::Live;
        }
    }

    /// Mark a root unavailable without purging its known tracks.
    pub fn mark_unavailable(&mut self, root: &Path, reason: impl Into<String>) {
        if let Some(state) = self.roots.get_mut(root) {
            state.availability = RootAvailability::Unavailable(reason.into());
        }
    }

    /// Mark a root partial (refused/limit-hit scan) without purging tracks.
    pub fn mark_partial(&mut self, root: &Path, reason: impl Into<String>) {
        if let Some(state) = self.roots.get_mut(root) {
            state.availability = RootAvailability::Partial(reason.into());
        }
    }

    fn owning_root(&self, path: &Path) -> Option<PathBuf> {
        // A watched directory belongs to the deepest configured root that is
        // its ancestor.
        self.roots
            .keys()
            .filter(|root| path.starts_with(root))
            .max_by_key(|root| root.as_os_str().len())
            .cloned()
    }

    #[cfg(test)]
    pub(crate) fn service(&self) -> &DirectoryWatchService {
        &self.service
    }

    #[cfg(test)]
    pub(crate) fn in_flight_generation(&self, root: &Path) -> Option<u64> {
        self.roots.get(root).and_then(|s| s.in_flight)
    }

    #[cfg(test)]
    pub(crate) fn watch_count(&self, root: &Path) -> usize {
        self.roots.get(root).map_or(0, |s| s.watches.len())
    }
}

fn is_significant_change(change: &DirectoryChange) -> bool {
    // `DirectoryChange` has no file-type bit and a dotted name may be a music
    // directory, so creation and deletion are always significant.
    let touches_audio = |p: &Path| -> bool {
        scanner::is_audio_file(p)
            || matches!(
                p.file_name().and_then(|n| n.to_str()),
                Some("cover.jpg" | "cover.png" | "folder.jpg" | "folder.png")
            )
    };
    match change {
        DirectoryChange::Created(_) | DirectoryChange::Deleted(_) => true,
        DirectoryChange::Modified(p) => touches_audio(p),
        DirectoryChange::Renamed { from, to } => touches_audio(from) || touches_audio(to),
    }
}

/// Build the default directory-watch backend for this build.
///
/// A normal Linux runtime gets an `inotify` backend; a failure to create the
/// FD, and every non-Linux or test build, gets a backend that refuses to watch
/// so the app keeps its explicit manual-refresh path rather than claiming a
/// live library it cannot deliver.
#[must_use]
pub fn default_backend() -> Arc<dyn DirectoryWatchBackend> {
    #[cfg(all(target_os = "linux", not(test)))]
    {
        match enzui_core::file_tree_watch::linux::LinuxInotifyBackend::new() {
            Ok(backend) => Arc::new(backend),
            Err(_) => Arc::new(enzui_core::file_tree_watch::NoopBackend),
        }
    }
    #[cfg(not(all(target_os = "linux", not(test))))]
    {
        Arc::new(enzui_core::file_tree_watch::NoopBackend)
    }
}

/// Build the bounded owner the scan walk runs on, degrading to an unavailable
/// owner (every submission settles as shut down) if the thread cannot spawn.
#[must_use]
pub fn scan_owner() -> BlockingTaskOwner {
    let workers = NonZeroUsize::new(1).expect("nonzero worker count");
    let queue = NonZeroUsize::new(4).expect("nonzero queue capacity");
    BlockingTaskOwner::new("music-library-scan", workers, queue)
        .unwrap_or_else(|_| BlockingTaskOwner::unavailable(queue))
}

#[cfg(test)]
impl LibraryWatcher {
    #[must_use]
    pub fn availability(&self, root: &Path) -> Option<&RootAvailability> {
        self.roots.get(root).map(|r| &r.availability)
    }
}
