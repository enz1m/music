//! Versioned, atomic persistence for the state a listener creates.
//!
//! The file lives at `$XDG_DATA_HOME/music-app/state.json`, falling back to
//! `$HOME/.local/share/music-app/state.json`. Domain types deliberately do not
//! define the file format: the records below are versioned DTOs whose fields
//! default independently, so an older version-1 file loads cleanly.
//!
//! Saves are sibling-temp-file + rename. Invalid JSON is renamed to
//! `state.json.corrupt` (then `.corrupt.1`, …), preserving the only recoverable
//! copy before the app starts from defaults. A document from a future format is
//! left byte-for-byte alone and persistence is disabled for that session.
//!
//! The load opens the path with `O_NOFOLLOW | O_NONBLOCK` and bounds the read
//! against the opened descriptor before allocating, refusing a symlink, FIFO,
//! device, directory, or a file over `MAX_STATE_FILE_BYTES` (a ceiling
//! derived from Music's own collection and aggregate authorities, not the
//! framework's larger framed one). Every persisted collection, path and string
//! is deserialized through [`bounded`], which refuses over its domain
//! cardinality, its per-value byte cap, or the aggregate retained-byte budget
//! before the next value is retained — the load authority. `semantic_over_limit`
//! adds only the checks the visitors cannot (name scalar count, cross-playlist
//! track total); `over_domain_limit` is the save-side gate, not re-run on load.
//! After the read the observed length must match the admitted descriptor and
//! the pathname must still name the same inode, or the snapshot is refused.
//! Every such refusal leaves the bytes untouched and disables saving, so the
//! session cannot overwrite a file it could not admit.

use std::path::{Path, PathBuf};
use std::sync::Arc;

pub use enzui_core::quarantine::ExpectedSourceIdentity;
use enzui_core::state_authority::StateAuthority;

use crate::MusicApp;

mod bounded;

/// The typed result of quarantining the current state file and reacquiring a
/// fresh publication authority.
pub enum RetireOutcome {
    /// The prior bytes were provably moved into recovery custody and a fresh
    /// authority was acquired over the now-absent destination.
    Reacquired {
        authority: StateAuthority,
        recovered: Option<PathBuf>,
    },
    /// The quarantine did not prove a clean move, so saving stays disabled and a
    /// bounded typed reason is surfaced.
    Refused(RetireRefusal),
}

/// A bounded, path-free reason an explicit Retire did not yield a fresh
/// authority.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RetireRefusal {
    /// No trusted read identity was retained, so the file cannot be retired by
    /// pathname alone.
    NoExpectedIdentity,
    /// The opened source no longer had the identity the reader retained: a
    /// substitution arrived after the read but before the quarantine.
    SourceMismatch,
    /// The recovery-name space was exhausted without a clean publication.
    CollisionExhausted,
    /// The source pathname was replaced by a foreign inode before the move.
    SourceReplaced,
    /// The move published but its durability barrier could not be confirmed.
    DurabilityUncertain,
    /// Only part of the custody could be verified against the retained source.
    PartialCustody,
    /// Identity-bound quarantine is unsupported on this platform.
    Unsupported,
    /// The source was refused before any name was mutated.
    Refused,
    /// The move succeeded but a fresh authority could not be acquired.
    AuthorityUnavailable,
}

/// Retire the current state file through the identity-bound sibling move,
/// then create a fresh authority only on a proven move of the expected inode.
///
/// `expected` is the identity the reader retained. A `None` expected identity
/// cannot retire by pathname and refuses without touching any name. The move is
/// [`quarantine_sibling_expected`], which refuses with
/// [`QuarantineOutcome::SourceMismatch`] when the opened source no longer has
/// that identity, closing the window where a substitution arrives after the read
/// but before the move. Only [`QuarantineOutcome::Moved`] whose custody names
/// the expected source creates the fresh authority, and only through
/// [`StateAuthority::create_new`] over the now-absent destination. Every
/// mismatch, replacement, durability-uncertain, partial, collision, or refusal
/// outcome preserves all names, stays disabled, and surfaces a bounded typed
/// [`RetireRefusal`].
///
/// [`quarantine_sibling_expected`]: enzui_core::quarantine::quarantine_sibling_expected
/// [`QuarantineOutcome::SourceMismatch`]: enzui_core::quarantine::QuarantineOutcome::SourceMismatch
/// [`QuarantineOutcome::Moved`]: enzui_core::quarantine::QuarantineOutcome::Moved
#[must_use]
pub fn retire_and_reacquire(
    path: &Path,
    expected: Option<ExpectedSourceIdentity>,
) -> RetireOutcome {
    use enzui_core::quarantine::{
        QuarantineOutcome, QuarantineRefusal, quarantine_sibling_expected,
    };
    // A missing expected identity cannot retire by pathname: refuse before any
    // name is opened or mutated.
    let Some(expected) = expected else {
        return RetireOutcome::Refused(RetireRefusal::NoExpectedIdentity);
    };
    match quarantine_sibling_expected(path, expected) {
        QuarantineOutcome::Moved { custody } => {
            // Only a custody that actually names the expected source may create
            // a fresh authority; otherwise the move did not retain the inode the
            // reader observed.
            let Some(recovered) = custody.source_paths().next().map(Path::to_path_buf) else {
                return RetireOutcome::Refused(RetireRefusal::PartialCustody);
            };
            match StateAuthority::create_new(path) {
                Ok(authority) => RetireOutcome::Reacquired {
                    authority,
                    recovered: Some(recovered),
                },
                Err(_) => RetireOutcome::Refused(RetireRefusal::AuthorityUnavailable),
            }
        }
        QuarantineOutcome::SourceMismatch { .. } => {
            RetireOutcome::Refused(RetireRefusal::SourceMismatch)
        }
        QuarantineOutcome::CollisionExhausted { .. } => {
            RetireOutcome::Refused(RetireRefusal::CollisionExhausted)
        }
        QuarantineOutcome::SourceReplaced { .. } => {
            RetireOutcome::Refused(RetireRefusal::SourceReplaced)
        }
        QuarantineOutcome::DurabilityUncertain { .. } => {
            RetireOutcome::Refused(RetireRefusal::DurabilityUncertain)
        }
        QuarantineOutcome::PartialCustody { .. } => {
            RetireOutcome::Refused(RetireRefusal::PartialCustody)
        }
        QuarantineOutcome::Refused {
            reason: QuarantineRefusal::Unsupported,
            ..
        } => RetireOutcome::Refused(RetireRefusal::Unsupported),
        QuarantineOutcome::Refused { .. } => RetireOutcome::Refused(RetireRefusal::Refused),
        // `QuarantineOutcome` is non-exhaustive: a future outcome this build
        // does not recognise is refused fail-closed and never grants a fresh
        // authority, so an unproven move can never enable saving.
        _ => RetireOutcome::Refused(RetireRefusal::Refused),
    }
}

#[cfg(test)]
thread_local! {
    /// Serialisations performed on *this exact thread*.
    static SERIALIZATION_COUNT: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    /// Full persisted documents materialised on this exact thread.
    static MATERIALIZATION_COUNT: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// Serialisations and materialisations on any thread: saves run on their own
/// worker, so a test's positive control counts process-wide.
#[cfg(test)]
static SERIALIZATION_TOTAL: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
#[cfg(test)]
static MATERIALIZATION_TOTAL: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn record_serialization() {
    #[cfg(test)]
    {
        SERIALIZATION_COUNT.with(|count| count.set(count.get().saturating_add(1)));
        SERIALIZATION_TOTAL.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }
}

fn record_materialization() {
    #[cfg(test)]
    {
        MATERIALIZATION_COUNT.with(|count| count.set(count.get().saturating_add(1)));
        MATERIALIZATION_TOTAL.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }
}

/// The bounded half of the persisted document.
///
/// Everything the listener created by hand: folders they picked, playlists they
/// named, feeds they subscribed to, stations they starred. Building and
/// comparing this is O(what the user typed) — tens of items — which is why the
/// reducer is allowed to do it by value on every persisting message.
#[derive(Clone, Debug, PartialEq)]
pub struct BoundedHalf {
    folders: Vec<PathBuf>,
    playlists: Vec<PlaylistRec>,
    subscriptions: Vec<SubscriptionRec>,
    /// Legacy numeric favourites, kept for shape parity with `PersistedState`.
    ///
    /// Writes always project it empty, and `restore` drops any value with a
    /// warning.
    radio_favorites: Vec<u64>,
    /// The new UUID-keyed favourites. Sorted at project time
    /// so the on-disk bytes are stable across `BTreeSet` iteration.
    radio_favorites_uuids: Vec<String>,
    next_playlist_seq: u64,
    /// The transport session projected off `MusicApp`. Bounded by
    /// what the listener queued, so it belongs to this half; the `PathBuf`
    /// and small-`Vec<PathBuf>` compares are the same shape as `playlists`.
    /// A `Seek` message therefore lands here and the standard
    /// `SharedDocument::unchanged_from` short-circuit still holds (the
    /// expensive `track_added_at` table is compared by `Arc` pointer).
    playback: PlaybackRec,
    /// per-episode resume points. Bounded by the episodes the listener
    /// actually started, so it belongs to the bounded half alongside
    /// `subscriptions`; a resume-position save lands here and
    /// `unchanged_from` still short-circuits on the `Arc`-compared halves.
    episode_resume: Vec<EpisodeResumeRec>,
    /// The podcast chart country the user picked.
    podcast_country: Option<String>,
    /// Radio stations added by stream URL.
    radio_custom_stations: Vec<RadioCustomRec>,
}

/// A persisted document the reducer can build, compare and hand off in
/// O(bounded).
///
/// `track_added_at` is one record per scanned track — 20,000 `PathBuf` clones
/// plus a sort at this app's measured ceiling — so it is shared by pointer
/// rather than owned. Two documents whose `Arc`s are the same pointer hold the
/// same records by construction, which turns "did the expensive half change?"
/// from an O(n) comparison into an O(1) one. `Persist` keeps that pointer
/// stable across every message that does not put a track into the library; see
/// [`Persist::track_added_at_for`].
///
/// The full [`PersistedState`] is built by [`Self::materialize`], which runs
/// inside the write task and nowhere else.
#[derive(Clone, Debug)]
pub struct SharedDocument {
    bounded: BoundedHalf,
    track_added_at: Arc<Vec<TrackAddedRec>>,
    /// The listener's play stats, one record per played track.
    ///
    /// Shared by `Arc` like `track_added_at`: `Persist::track_stats_for` mints
    /// a new one only when `Library::track_stats_revision` moves.
    track_stats: Arc<Vec<TrackStatsRec>>,
}

impl SharedDocument {
    #[must_use]
    pub fn new(
        bounded: BoundedHalf,
        track_added_at: Arc<Vec<TrackAddedRec>>,
        track_stats: Arc<Vec<TrackStatsRec>>,
    ) -> Self {
        Self {
            bounded,
            track_added_at,
            track_stats,
        }
    }

    /// Whether this document would write the same bytes as `previous`.
    ///
    /// The expensive collections are compared by pointer. That is sound
    /// *because* [`Persist::track_added_at_for`] only mints a new `Arc`
    /// when the library's `track_identity_revision` moves, and
    /// [`Persist::track_stats_for`] only mints a new `Arc` when
    /// `Library::track_stats_revision` moves. A message that touches
    /// neither reuses both pointers and this short-circuits in `O(1)`
    /// after a bounded compare of the small `BoundedHalf`.
    #[must_use]
    pub fn unchanged_from(&self, previous: &Self) -> bool {
        Arc::ptr_eq(&self.track_added_at, &previous.track_added_at)
            && Arc::ptr_eq(&self.track_stats, &previous.track_stats)
            && self.bounded == previous.bounded
    }

    /// Build the on-disk document. This is the O(tracks) step: it is called from
    /// the write task and from the close drain's serialiser, never from a reducer
    /// arm.
    #[must_use]
    pub fn materialize(&self) -> PersistedState {
        record_materialization();
        PersistedState {
            version: FORMAT_VERSION,
            folders: self.bounded.folders.clone(),
            playlists: self.bounded.playlists.clone(),
            subscriptions: self.bounded.subscriptions.clone(),
            radio_favorites: self.bounded.radio_favorites.clone(),
            radio_favorites_uuids: self.bounded.radio_favorites_uuids.clone(),
            track_added_at: (*self.track_added_at).clone(),
            next_playlist_seq: self.bounded.next_playlist_seq,
            playback: self.bounded.playback.clone(),
            track_stats: (*self.track_stats).clone(),
            episode_resume: self.bounded.episode_resume.clone(),
            podcast_country: self.bounded.podcast_country.clone(),
            radio_custom_stations: self.bounded.radio_custom_stations.clone(),
        }
    }
}

impl From<PersistedState> for SharedDocument {
    /// Split a whole document. Used at boot (once, off any frame) and by tests
    /// that state their input as a file document.
    fn from(doc: PersistedState) -> Self {
        Self {
            bounded: BoundedHalf {
                folders: doc.folders,
                playlists: doc.playlists,
                subscriptions: doc.subscriptions,
                radio_favorites: doc.radio_favorites,
                radio_favorites_uuids: doc.radio_favorites_uuids,
                next_playlist_seq: doc.next_playlist_seq,
                playback: doc.playback,
                episode_resume: doc.episode_resume,
                podcast_country: doc.podcast_country,
                radio_custom_stations: doc.radio_custom_stations,
            },
            track_added_at: Arc::new(doc.track_added_at),
            track_stats: Arc::new(doc.track_stats),
        }
    }
}

/// The bounded half of what `app` would persist. O(folders + playlists +
/// subscriptions + favourites); it never touches `library.tracks`.
#[must_use]
pub fn bounded_half(app: &MusicApp) -> BoundedHalf {
    BoundedHalf {
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
        // `radio_favorites` (u64) is legacy — always projected
        // empty; the durable identity is `radio_favorites_uuids`.
        radio_favorites: Vec::new(),
        radio_favorites_uuids: {
            let mut uuids: Vec<String> = app.radio.favorites.iter().cloned().collect();
            // The on-disk order must be stable.
            uuids.sort();
            uuids
        },
        next_playlist_seq: app.next_playlist_seq,
        playback: playback_rec_from_app(app),
        episode_resume: episode_resume_recs_from_app(app),
        podcast_country: app.podcast_charts.chosen_country.clone(),
        radio_custom_stations: queue::radio_custom_recs_from_app(app),
    }
}

/// Project the live episode-resume table onto [`EpisodeResumeRec`]s sorted by
/// `(feed_url, guid)`, so the on-disk bytes are stable.
#[must_use]
fn episode_resume_recs_from_app(app: &MusicApp) -> Vec<EpisodeResumeRec> {
    let mut recs: Vec<EpisodeResumeRec> = app
        .episode_resume
        .iter()
        .filter(|(_, position)| **position > 0.0)
        .map(|((feed_url, guid), position)| EpisodeResumeRec {
            feed_url: feed_url.clone(),
            guid: guid.clone(),
            position_secs: *position,
        })
        .collect();
    recs.sort_by(|a, b| a.feed_url.cmp(&b.feed_url).then(a.guid.cmp(&b.guid)));
    // Defence in depth: the reducer already bounds the live table, but never
    // project more than the ceiling so a save can never be refused for this
    // record class.
    recs.truncate(crate::domain::MAX_EPISODE_RESUME);
    recs
}

/// Project the app's live [`PlaybackState`](crate::domain::PlaybackState) onto
/// a [`PlaybackRec`], resolving `queue`/`current_track_id` back to paths
/// through the library. Runs on the reducer thread as part of
/// [`bounded_half`], so it is `O(queue.len())` — bounded by what the listener
/// queued.
///
/// A queue id that no longer names a live library entry (a paranoid case: the
/// reducer holds ids only after `ScanComplete` has landed them) is silently
/// skipped rather than persisted, so the on-disk queue is always a subset of
/// what the app itself could resolve.
#[must_use]
fn playback_rec_from_app(app: &MusicApp) -> PlaybackRec {
    let playback = &app.playback;
    let path_of = |id: u64| app.library.tracks.get(&id).map(|track| track.path.clone());
    let current_track_path = playback.current_track_id.and_then(path_of);
    let mut queue_paths = Vec::with_capacity(playback.queue.len());
    let mut positions = std::collections::HashMap::new();
    for (index, &id) in playback.queue.iter().enumerate() {
        if let Some(path) = path_of(id) {
            if let Some(entry) = playback.entry_id_at(index) {
                positions.insert(entry, queue_paths.len());
            }
            queue_paths.push(path);
        }
    }
    let unshuffled_order = playback
        .unshuffled_order
        .iter()
        .filter_map(|entry| positions.get(entry).copied())
        .collect();
    PlaybackRec {
        current_track_path,
        queue_index: playback
            .current_entry_id()
            .and_then(|entry| positions.get(&entry).copied())
            .unwrap_or_else(|| {
                playback
                    .queue_index
                    .min(queue_paths.len().saturating_sub(1))
            }),
        queue_paths,
        unshuffled_order,
        position_secs: if app.media.snapshot().playback.is_terminal() {
            0.0
        } else {
            playback.position_secs
        },
        shuffle: playback.shuffle,
        repeat: RepeatModeRec::from(playback.repeat),
        rate: app.rate_policy,
        audio: AudioPreferencesRec::capture(app.eq, app.rg),
        volume: playback.volume,
        muted: playback.muted,
    }
}

fn track_stats_records(library: &crate::domain::Library) -> Vec<TrackStatsRec> {
    let mut records = Vec::new();
    for track in library.tracks.values() {
        let rating = library.rating_for_path(&track.path);
        if track.play_count > 0 || track.last_played_unix > 0 || rating > 0 {
            records.push(TrackStatsRec {
                path: track.path.clone(),
                play_count: track.play_count,
                last_played_unix: track.last_played_unix,
                rating,
            });
        }
    }
    for (path, rating) in library.ratings() {
        if library.track_id_for_path(path).is_none() {
            records.push(TrackStatsRec {
                path: path.clone(),
                rating: *rating,
                ..Default::default()
            });
        }
    }
    records.sort_by(|a, b| a.path.cmp(&b.path));
    records
}

/// Rebuild the first-seen table: one record per track in memory, unioned with
/// records for paths that are not in memory.
///
/// The union is why a playlist edit before the boot scan lands cannot erase the
/// listener's timestamps, and why a file deleted from disk keeps its first-seen
/// date. It runs when a track entered the library — once per scan — not once
/// per message.
fn rebuild_track_added_at(
    library: &crate::domain::Library,
    previous: Option<&[TrackAddedRec]>,
) -> Vec<TrackAddedRec> {
    let mut by_path: std::collections::BTreeMap<PathBuf, u64> = library
        .tracks
        .values()
        .map(|track| (track.path.clone(), track.added_at))
        .collect();
    for record in previous.unwrap_or_default() {
        by_path
            .entry(record.path.clone())
            .or_insert(record.added_at);
    }
    by_path
        .into_iter()
        .map(|(path, added_at)| TrackAddedRec { path, added_at })
        .collect()
}

/// The one logical destination this app persists to.
pub const STATE_DESTINATION: &str = "music-state";

#[path = "persistence/queue.rs"]
mod queue;
#[path = "persistence/records.rs"]
mod records;

#[cfg(test)]
pub use queue::PersistenceFailure;
#[cfg(test)]
pub use queue::snapshot;
pub use queue::{
    PendingSave, Persist, PersistenceCompletion, PersistenceQueue, acquire, open_at, persist_for,
    restore,
};
#[cfg(test)]
pub use records::load_from;
use records::load_identified;
#[cfg(test)]
pub use records::save_to;
pub use records::{
    AudioPreferencesRec, EpisodeResumeRec, FORMAT_VERSION, LoadError, Origin, PersistedState,
    PlaybackRec, PlaylistRec, RadioCustomRec, RefusalKind, RepeatModeRec, RootRefusal,
    SharedAuthority, SubscriptionRec, TrackAddedRec, TrackStatsRec, resolve_state_location,
    serialize_pretty,
};
#[cfg(test)]
use records::{MAX_STATE_FILE_BYTES, reverify_pathname};

#[cfg(test)]
mod tests;
