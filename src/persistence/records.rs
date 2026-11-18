//! Persistence records: versioned DTOs, errors, location, and
//! atomic read/write.

use super::bounded;
use super::record_serialization;
use crate::domain::RepeatMode;
use enzui_core::state_authority::StateAuthority;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// The identity-bound publication authority, shared so every cloned
/// [`super::PendingSave`] publishes through the same retained custody.
/// [`StateAuthority`] is not `Clone`; the manual `Debug` keeps
/// [`super::Persist`] and [`super::PendingSave`] derivable.
pub type SharedAuthority = Arc<Mutex<StateAuthority>>;

pub const FORMAT_VERSION: u32 = 1;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PersistedState {
    pub version: u32,
    #[serde(default, deserialize_with = "bounded::folders")]
    pub folders: Vec<PathBuf>,
    #[serde(default, deserialize_with = "bounded::playlists")]
    pub playlists: Vec<PlaylistRec>,
    #[serde(default, deserialize_with = "bounded::subscriptions")]
    pub subscriptions: Vec<SubscriptionRec>,
    /// Legacy favourites keyed on in-process `u64` ids, which are unstable
    /// across catalog rebuilds. Kept so legacy documents parse; new writes
    /// leave it empty and `restore` drops any surviving entry.
    #[serde(default, deserialize_with = "bounded::radio_favorites")]
    pub radio_favorites: Vec<u64>,
    /// Favourites keyed on the durable Radio Browser `stationuuid`, reconciled
    /// against each fresh catalog by
    /// [`crate::radio::RadioState::reconcile_favorites_against_catalog`].
    #[serde(default, deserialize_with = "bounded::radio_favorite_uuids")]
    pub radio_favorites_uuids: Vec<String>,
    #[serde(default, deserialize_with = "bounded::track_added")]
    pub track_added_at: Vec<TrackAddedRec>,
    #[serde(default = "default_playlist_seq")]
    pub next_playlist_seq: u64,
    /// The listener's play stats, one record per played track, keyed on the
    /// absolute file path like every other durable slice.
    ///
    /// A `Vec` so the JSON has no key-order ambiguity. A rescan at the same
    /// path keeps the count; a path missing from the scanned library is dropped
    /// by `MusicApp::finalize_pending_track_stats` with a Library notice.
    /// Absent in older documents, which load an empty list at the same
    /// [`FORMAT_VERSION`].
    #[serde(default, deserialize_with = "bounded::track_stats")]
    pub track_stats: Vec<TrackStatsRec>,
    /// Per-episode resume points, keyed on `(feed_url, guid)`. Absent in older
    /// documents, which load an empty list at the same [`FORMAT_VERSION`].
    #[serde(default, deserialize_with = "bounded::episode_resume")]
    pub episode_resume: Vec<EpisodeResumeRec>,
    /// The listener's transport session: current track, queue, playhead,
    /// shuffle, repeat, volume and mute, restored after the boot scan resolves
    /// paths to ids (see `MusicApp::finalize_pending_playback_session`).
    ///
    /// A document without it loads `PlaybackRec::default()` at the same
    /// [`FORMAT_VERSION`] with no resume notice. Playing state is not
    /// persisted: restart restores paused.
    #[serde(default)]
    pub playback: PlaybackRec,
    /// The country whose podcast charts the user picked. Absent in older
    /// documents and when the guessed country was never changed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub podcast_country: Option<String>,
    /// Radio stations the user added by stream URL (Radio Browser doesn't
    /// list them). Absent in older documents.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub radio_custom_stations: Vec<RadioCustomRec>,
}

/// A radio station added by its stream URL.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct RadioCustomRec {
    #[serde(default, deserialize_with = "bounded::feed_title")]
    pub name: String,
    #[serde(default, deserialize_with = "bounded::feed_url")]
    pub stream_url: String,
}

fn default_playlist_seq() -> u64 {
    1
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PlaylistRec {
    #[serde(default)]
    pub id: u64,
    #[serde(default, deserialize_with = "bounded::playlist_name")]
    pub name: String,
    #[serde(default, deserialize_with = "bounded::playlist_track_paths")]
    pub track_paths: Vec<PathBuf>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SubscriptionRec {
    #[serde(default, deserialize_with = "bounded::feed_url")]
    pub feed_url: String,
    #[serde(default, deserialize_with = "bounded::feed_title")]
    pub title: String,
    #[serde(default, deserialize_with = "bounded::feed_description")]
    pub description: String,
}

/// One per-episode resume point, keyed on the durable `(feed_url, guid)` pair
/// so a refetch that reorders or renames a feed cannot move a saved position.
/// A position at or beyond the episode end is dropped at save time.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct EpisodeResumeRec {
    #[serde(default, deserialize_with = "bounded::feed_url")]
    pub feed_url: String,
    #[serde(default, deserialize_with = "bounded::episode_guid")]
    pub guid: String,
    #[serde(default)]
    pub position_secs: f64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct TrackAddedRec {
    #[serde(default, deserialize_with = "bounded::path_field")]
    pub path: PathBuf,
    #[serde(default)]
    pub added_at: u64,
}

/// Path-keyed listener history and rating (zero means unrated, otherwise one to five).
/// Missing fields restore to zero; ratings are not written into audio files.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct TrackStatsRec {
    #[serde(default, deserialize_with = "bounded::path_field")]
    pub path: PathBuf,
    #[serde(default)]
    pub play_count: u32,
    #[serde(default)]
    pub last_played_unix: u64,
    #[serde(default, deserialize_with = "bounded::rating")]
    pub rating: u8,
}

/// Bounded listener processing preferences, independent of the saved queue.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AudioPreferencesRec {
    equalizer_enabled: bool,
    equalizer_bands_db: [f32; crate::domain::EQ_BAND_COUNT],
    equalizer_preamp_db: f32,
    equalizer_clip_protection: bool,
    replay_gain_mode: ReplayGainModeRec,
    replay_gain_preamp_db: f32,
    replay_gain_prevent_clip: bool,
}

impl Default for AudioPreferencesRec {
    fn default() -> Self {
        Self::capture(
            crate::domain::EqState::default(),
            crate::domain::ReplayGainState::default(),
        )
    }
}

impl AudioPreferencesRec {
    pub(crate) fn capture(eq: crate::domain::EqState, rg: crate::domain::ReplayGainState) -> Self {
        Self {
            equalizer_enabled: eq.enabled,
            equalizer_bands_db: eq.bands.map(|gain| bounded_gain(gain, -12.0, 12.0)),
            equalizer_preamp_db: bounded_gain(eq.preamp_db, -12.0, 12.0),
            equalizer_clip_protection: eq.clip_protection,
            replay_gain_mode: rg.mode.into(),
            replay_gain_preamp_db: bounded_gain(
                rg.preamp_db,
                crate::domain::RG_PREAMP_MIN_DB,
                crate::domain::RG_PREAMP_MAX_DB,
            ),
            replay_gain_prevent_clip: rg.prevent_clip,
        }
    }

    pub(crate) fn states(&self) -> (crate::domain::EqState, crate::domain::ReplayGainState) {
        let bands = self
            .equalizer_bands_db
            .map(|gain| bounded_gain(gain, -12.0, 12.0));
        (
            crate::domain::EqState {
                enabled: self.equalizer_enabled,
                bands,
                preamp_db: bounded_gain(self.equalizer_preamp_db, -12.0, 12.0),
                clip_protection: self.equalizer_clip_protection,
                preset: crate::domain::EqPreset::classify(&bands),
            },
            crate::domain::ReplayGainState {
                mode: self.replay_gain_mode.into(),
                preamp_db: bounded_gain(
                    self.replay_gain_preamp_db,
                    crate::domain::RG_PREAMP_MIN_DB,
                    crate::domain::RG_PREAMP_MAX_DB,
                ),
                prevent_clip: self.replay_gain_prevent_clip,
            },
        )
    }
}

fn bounded_gain(value: f32, min: f32, max: f32) -> f32 {
    if value.is_finite() {
        value.clamp(min, max)
    } else {
        0.0
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ReplayGainModeRec {
    Off,
    #[default]
    Track,
    Album,
}

impl From<crate::domain::ReplayGainMode> for ReplayGainModeRec {
    fn from(mode: crate::domain::ReplayGainMode) -> Self {
        match mode {
            crate::domain::ReplayGainMode::Off => Self::Off,
            crate::domain::ReplayGainMode::Track => Self::Track,
            crate::domain::ReplayGainMode::Album => Self::Album,
        }
    }
}

impl From<ReplayGainModeRec> for crate::domain::ReplayGainMode {
    fn from(mode: ReplayGainModeRec) -> Self {
        match mode {
            ReplayGainModeRec::Off => Self::Off,
            ReplayGainModeRec::Track => Self::Track,
            ReplayGainModeRec::Album => Self::Album,
        }
    }
}

/// Resumable transport state with path-keyed queue membership.
/// `rate` is the last confirmed policy, reissued after the restored source becomes ready;
/// it does not confirm the new pipeline's rate by itself.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PlaybackRec {
    /// The track in the now-playing slot at save time, or `None`. It need not
    /// appear in `queue_paths`.
    #[serde(default, deserialize_with = "bounded::optional_path_field")]
    pub current_track_path: Option<PathBuf>,
    /// Up Next entries in order; entries whose file has vanished are skipped at
    /// resume.
    #[serde(default, deserialize_with = "bounded::queue_paths")]
    pub queue_paths: Vec<PathBuf>,
    /// Original occurrence order as indices into the saved queue.
    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "bounded::queue_order"
    )]
    pub unshuffled_order: Vec<usize>,
    /// The saved cursor into `queue_paths`. Restored as
    /// `min(saved_index, resolved_queue.len() - 1)` when a track survives,
    /// pinned to the resolved current track's position when both survive, and
    /// `0` when the resolved queue is empty.
    #[serde(default)]
    pub queue_index: usize,
    /// The playhead in seconds at the moment of save. Applied verbatim when
    /// `current_track_path` resolves; discarded when it does not.
    #[serde(default)]
    pub position_secs: f64,
    #[serde(default)]
    pub shuffle: bool,
    #[serde(default)]
    pub repeat: RepeatModeRec,
    #[serde(default = "default_volume")]
    pub volume: f32,
    #[serde(default)]
    pub muted: bool,
    #[serde(default = "default_rate", deserialize_with = "bounded::rate")]
    pub rate: f64,
    #[serde(default)]
    pub audio: AudioPreferencesRec,
}

fn default_rate() -> f64 {
    1.0
}

impl Default for PlaybackRec {
    fn default() -> Self {
        Self {
            current_track_path: None,
            queue_paths: Vec::new(),
            unshuffled_order: Vec::new(),
            queue_index: 0,
            position_secs: 0.0,
            shuffle: false,
            repeat: RepeatModeRec::default(),
            rate: default_rate(),
            audio: AudioPreferencesRec::default(),
            volume: default_volume(),
            muted: false,
        }
    }
}

impl PlaybackRec {
    /// Whether a resume path would restore anything beyond scalar defaults; an
    /// empty session skips the finalize step and any Library notice.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        *self == PlaybackRec::default()
    }
}

/// The disk mirror of [`RepeatMode`], so the domain type carries no
/// serialisation contract.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub enum RepeatModeRec {
    #[default]
    Off,
    All,
    One,
}

impl From<RepeatMode> for RepeatModeRec {
    fn from(mode: RepeatMode) -> Self {
        match mode {
            RepeatMode::Off => Self::Off,
            RepeatMode::All => Self::All,
            RepeatMode::One => Self::One,
        }
    }
}

impl From<RepeatModeRec> for RepeatMode {
    fn from(rec: RepeatModeRec) -> Self {
        match rec {
            RepeatModeRec::Off => Self::Off,
            RepeatModeRec::All => Self::All,
            RepeatModeRec::One => Self::One,
        }
    }
}

/// The fresh-boot volume, matching `PlaybackState::default().volume`, so a
/// document without a playback block loads at the same level.
#[must_use]
pub(super) fn default_volume() -> f32 {
    0.8
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LoadError {
    Missing,
    Corrupt(String),
    Unsupported(u32),
    /// The path was not a readable regular file: a symlink, FIFO, device,
    /// directory, or an open/read that failed. The bytes are left untouched.
    Unreadable(String),
    /// The file exceeded the raw byte ceiling before or after allocation.
    TooLarge(u64),
    /// The document parsed but exceeded a domain retention limit.
    OverLimit(String),
}

/// Why the persistence data root itself was refused, disabling saving for the
/// session. Path-free by construction so the listener-facing notice carries no
/// filesystem path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RootRefusal {
    /// No usable absolute data-home root: `$XDG_DATA_HOME` and `$HOME` are both
    /// unset, empty, or relative. There is nowhere private to save.
    NoDataHome,
    /// The resolved application data directory is not a usable private
    /// directory: a symlink, a non-directory, owned by another uid, exposing
    /// group/other permissions, or a creation/replacement failure.
    Unusable,
}

/// Why a parsed-or-unread state file was refused into a read-only session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RefusalKind {
    /// The path was not a readable regular file.
    Unreadable,
    /// The file exceeded the raw byte ceiling.
    TooLarge,
    /// A domain retention limit was exceeded.
    OverLimit,
    /// The file parsed as corrupt and is left byte-identical, read-only, until
    /// an explicit Retire quarantines it through the identity-bound sibling move
    /// and reacquires.
    Corrupt,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Origin {
    Missing,
    Restored,
    ReadOnlyFutureVersion(u32),
    /// The file was refused (unsafe shape, oversize, or over a domain limit),
    /// left byte-identical, and saving disabled for the session.
    RefusedReadOnly(RefusalKind),
    /// The persistence data root could not be resolved or ensured private, so
    /// no file was read and saving is disabled for the session.
    PersistenceRootUnavailable(RootRefusal),
}

/// Music's application-id segment under the XDG data home.
pub const APP_ID: &str = "music-app";
/// The state file name inside the resolved application data directory.
pub const STATE_FILE_NAME: &str = "state.json";

/// Resolve and ensure the private state-file location through an injected
/// [`enzui_core::app_data::AppDataResolver`].
///
/// `Ok` is `<data-home>/music-app/state.json` in a non-symlink, owner-matched,
/// group/other-inaccessible directory. Every failure is a path-free
/// [`RootRefusal`] that disables saving; there is no shared `/tmp` fallback.
pub fn resolve_state_location(
    resolver: &enzui_core::app_data::AppDataResolver,
) -> Result<PathBuf, RootRefusal> {
    use enzui_core::app_data::{AppDataError, AppId};
    let app = AppId::new(APP_ID).expect("APP_ID is a static single-segment id");
    match resolver.ensure_tightening(&app) {
        Ok(dir) => dir.join(STATE_FILE_NAME).map_err(|_| RootRefusal::Unusable),
        Err(AppDataError::NoDataHome { .. }) => Err(RootRefusal::NoDataHome),
        Err(AppDataError::Create { .. } | AppDataError::InvalidAppId { .. }) => {
            Err(RootRefusal::Unusable)
        }
    }
}

// A shared playlist id is ambiguous: `rename_playlist` mutates only the first
// owner while `delete_playlist` removes every owner, so ids must be unique.
fn duplicate_playlist_id(playlists: &[PlaylistRec]) -> Option<u64> {
    let mut seen = std::collections::HashSet::new();
    playlists
        .iter()
        .find_map(|p| (!seen.insert(p.id)).then_some(p.id))
}

/// The complete-file byte ceiling admitted before allocation.
///
/// The structural JSON cost of every collection at its cardinality cap
/// ([`PER_RECORD_JSON_OVERHEAD`] × [`RECORD_COUNT_CEILING`]) plus the aggregate
/// variable-length budget [`crate::domain::MAX_AGGREGATE_STRING_BYTES`]. The
/// save side enforces the same limits, so the app never writes a document it
/// could not reload.
pub(super) const PER_RECORD_JSON_OVERHEAD: u64 = 160;
pub(super) const RECORD_COUNT_CEILING: u64 = (crate::domain::MAX_PERSISTED_PLAYLISTS
    + 2 * crate::domain::MAX_PERSISTED_TRACKS
    + crate::domain::MAX_TOTAL_PLAYLIST_TRACKS
    + crate::domain::MAX_QUEUE_PATHS
    + crate::domain::MAX_SUBSCRIPTIONS
    + crate::domain::MAX_EPISODE_RESUME
    + crate::domain::MAX_LIBRARY_FOLDERS
    + 2 * crate::domain::MAX_RADIO_FAVORITES) as u64;
pub(super) const MAX_STATE_FILE_BYTES: u64 = RECORD_COUNT_CEILING * PER_RECORD_JSON_OVERHEAD
    + crate::domain::MAX_AGGREGATE_STRING_BYTES as u64;

#[cfg(unix)]
fn open_state_nofollow_nonblocking(path: &Path) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    // `O_NOFOLLOW` closes the path-check/open race for a final symlink;
    // `O_NONBLOCK` keeps a FIFO or device from blocking. `read_state_text`
    // still rejects a non-regular descriptor.
    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
}

#[cfg(not(unix))]
fn open_state_nofollow_nonblocking(path: &Path) -> std::io::Result<std::fs::File> {
    std::fs::File::open(path)
}

// Size and type checks query the descriptor, so they cannot race a replacement.
// The descriptor identity is exposed only if the pathname still names it, so an
// identity-bound Retire never targets foreign bytes; a refused but recoverable
// read still reports it so oversized or malformed state stays retirable.
fn read_state_text(path: &Path) -> (Result<String, LoadError>, Option<(u64, u64)>) {
    use std::io::Read;
    let file = match open_state_nofollow_nonblocking(path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return (Err(LoadError::Missing), None);
        }
        Err(e) => {
            return (
                Err(LoadError::Unreadable(format!("open refused: {e}"))),
                None,
            );
        }
    };
    let meta = match file.metadata() {
        Ok(meta) => meta,
        Err(e) => {
            return (
                Err(LoadError::Unreadable(format!("fstat failed: {e}"))),
                None,
            );
        }
    };
    if !meta.is_file() {
        return (
            Err(LoadError::Unreadable(
                "state path is not a regular file".into(),
            )),
            None,
        );
    }
    let admitted_len = meta.len();
    // The read/decode outcome, computed with the descriptor identity kept aside
    // so a refused input still carries it.
    let read_result = (|| {
        if admitted_len > MAX_STATE_FILE_BYTES {
            return Err(LoadError::TooLarge(admitted_len));
        }
        let mut text = String::new();
        match (&file)
            .take(MAX_STATE_FILE_BYTES.saturating_add(1))
            .read_to_string(&mut text)
        {
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::InvalidData => {
                return Err(LoadError::Corrupt(format!("non-utf8: {e}")));
            }
            Err(e) => return Err(LoadError::Unreadable(format!("read failed: {e}"))),
        }
        let observed = text.len() as u64;
        if observed > MAX_STATE_FILE_BYTES {
            return Err(LoadError::TooLarge(observed));
        }
        if observed != admitted_len {
            return Err(LoadError::Unreadable(format!(
                "state file changed under the read: admitted {admitted_len} bytes, read {observed}"
            )));
        }
        Ok(text)
    })();

    // Only a pathname that still names the opened descriptor exposes its
    // identity; otherwise a read that would have succeeded is refused.
    match reverify_pathname(path, &meta) {
        Ok(()) => (read_result, Some(descriptor_identity(&meta))),
        Err(reverify_error) => (Err(read_result.err().unwrap_or(reverify_error)), None),
    }
}

/// The device/inode of the descriptor whose bytes were just read.
#[cfg(unix)]
fn descriptor_identity(meta: &std::fs::Metadata) -> (u64, u64) {
    use std::os::unix::fs::MetadataExt;
    (meta.dev(), meta.ino())
}

#[cfg(not(unix))]
fn descriptor_identity(_meta: &std::fs::Metadata) -> (u64, u64) {
    (0, 0)
}

// A replaced or vanished pathname means the bytes read no longer represent the
// file, so the snapshot is refused read-only rather than adopted.
#[cfg(unix)]
pub(super) fn reverify_pathname(
    path: &Path,
    admitted: &std::fs::Metadata,
) -> Result<(), LoadError> {
    use std::os::unix::fs::MetadataExt;
    match std::fs::symlink_metadata(path) {
        Ok(now) if now.dev() == admitted.dev() && now.ino() == admitted.ino() => Ok(()),
        Ok(_) => Err(LoadError::Unreadable(
            "state path was replaced during the read".into(),
        )),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(LoadError::Unreadable(
            "state path disappeared during the read".into(),
        )),
        Err(e) => Err(LoadError::Unreadable(format!("re-stat failed: {e}"))),
    }
}

#[cfg(not(unix))]
fn reverify_pathname(_path: &Path, _admitted: &std::fs::Metadata) -> Result<(), LoadError> {
    Ok(())
}

fn invalid_queue_order(playback: &PlaybackRec) -> bool {
    let mut seen = std::collections::HashSet::new();
    playback.unshuffled_order.len() > crate::domain::MAX_QUEUE_PATHS
        || playback
            .unshuffled_order
            .iter()
            .any(|index| *index >= playback.queue_paths.len() || !seen.insert(*index))
}

pub(super) fn over_domain_limit(doc: &PersistedState) -> Option<String> {
    if invalid_queue_order(&doc.playback) {
        return Some("invalid unshuffled queue order".into());
    }
    use crate::domain::{
        MAX_AGGREGATE_STRING_BYTES, MAX_EPISODE_GUID_BYTES, MAX_EPISODE_RESUME,
        MAX_FEED_DESCRIPTION_BYTES, MAX_FEED_TITLE_BYTES, MAX_FEED_URL_BYTES, MAX_LIBRARY_FOLDERS,
        MAX_PERSISTED_PATH_BYTES, MAX_PERSISTED_PLAYLISTS, MAX_PERSISTED_TRACKS,
        MAX_PLAYLIST_NAME_LEN, MAX_QUEUE_PATHS, MAX_RADIO_FAVORITES, MAX_RADIO_UUID_BYTES,
        MAX_SUBSCRIPTIONS, MAX_TOTAL_PLAYLIST_TRACKS, MAX_TRACKS_PER_PLAYLIST,
    };
    // Mirrors the pre-retention accounting in `bounded`, so save and load
    // agree.
    let mut aggregate: usize = 0;
    let path_ok = |p: &Path, what: &str| -> Option<String> {
        let n = p.as_os_str().len();
        (n > MAX_PERSISTED_PATH_BYTES)
            .then(|| format!("{what} path too long: {n} exceeds {MAX_PERSISTED_PATH_BYTES}"))
    };

    if doc.folders.len() > MAX_LIBRARY_FOLDERS {
        return Some(format!(
            "too many folders: {} exceeds {MAX_LIBRARY_FOLDERS}",
            doc.folders.len()
        ));
    }
    for folder in &doc.folders {
        if let Some(reason) = path_ok(folder, "folder") {
            return Some(reason);
        }
        aggregate = aggregate.saturating_add(folder.as_os_str().len());
    }

    if doc.playlists.len() > MAX_PERSISTED_PLAYLISTS {
        return Some(format!(
            "too many playlists: {} exceeds {MAX_PERSISTED_PLAYLISTS}",
            doc.playlists.len()
        ));
    }
    let mut total: usize = 0;
    for playlist in &doc.playlists {
        let name_len = playlist.name.chars().count();
        if name_len > MAX_PLAYLIST_NAME_LEN {
            return Some(format!(
                "playlist name too long: {name_len} exceeds {MAX_PLAYLIST_NAME_LEN}"
            ));
        }
        if playlist.track_paths.len() > MAX_TRACKS_PER_PLAYLIST {
            return Some(format!(
                "playlist too large: {} exceeds {MAX_TRACKS_PER_PLAYLIST}",
                playlist.track_paths.len()
            ));
        }
        aggregate = aggregate.saturating_add(playlist.name.len());
        for track in &playlist.track_paths {
            if let Some(reason) = path_ok(track, "playlist track") {
                return Some(reason);
            }
            aggregate = aggregate.saturating_add(track.as_os_str().len());
        }
        total = total.saturating_add(playlist.track_paths.len());
    }
    if total > MAX_TOTAL_PLAYLIST_TRACKS {
        return Some(format!(
            "aggregate playlist tracks {total} exceeds {MAX_TOTAL_PLAYLIST_TRACKS}"
        ));
    }

    if doc.subscriptions.len() > MAX_SUBSCRIPTIONS {
        return Some(format!(
            "too many subscriptions: {} exceeds {MAX_SUBSCRIPTIONS}",
            doc.subscriptions.len()
        ));
    }
    for sub in &doc.subscriptions {
        if sub.feed_url.len() > MAX_FEED_URL_BYTES {
            return Some(format!(
                "feed url too long: {} exceeds {MAX_FEED_URL_BYTES}",
                sub.feed_url.len()
            ));
        }
        if sub.title.len() > MAX_FEED_TITLE_BYTES {
            return Some(format!(
                "feed title too long: {} exceeds {MAX_FEED_TITLE_BYTES}",
                sub.title.len()
            ));
        }
        if sub.description.len() > MAX_FEED_DESCRIPTION_BYTES {
            return Some(format!(
                "feed description too long: {} exceeds {MAX_FEED_DESCRIPTION_BYTES}",
                sub.description.len()
            ));
        }
        aggregate = aggregate
            .saturating_add(sub.feed_url.len())
            .saturating_add(sub.title.len())
            .saturating_add(sub.description.len());
    }

    // Episode resume: cardinality, per-field byte caps, and an aggregate charge
    // for the identity strings.
    if doc.episode_resume.len() > MAX_EPISODE_RESUME {
        return Some(format!(
            "too many episode resume points: {} exceeds {MAX_EPISODE_RESUME}",
            doc.episode_resume.len()
        ));
    }
    for resume in &doc.episode_resume {
        if resume.feed_url.len() > MAX_FEED_URL_BYTES {
            return Some(format!(
                "resume feed url too long: {} exceeds {MAX_FEED_URL_BYTES}",
                resume.feed_url.len()
            ));
        }
        if resume.guid.len() > MAX_EPISODE_GUID_BYTES {
            return Some(format!(
                "resume guid too long: {} exceeds {MAX_EPISODE_GUID_BYTES}",
                resume.guid.len()
            ));
        }
        aggregate = aggregate
            .saturating_add(resume.feed_url.len())
            .saturating_add(resume.guid.len());
    }

    if doc.radio_favorites.len() > MAX_RADIO_FAVORITES {
        return Some(format!(
            "too many legacy radio favourites: {} exceeds {MAX_RADIO_FAVORITES}",
            doc.radio_favorites.len()
        ));
    }
    if doc.radio_favorites_uuids.len() > MAX_RADIO_FAVORITES {
        return Some(format!(
            "too many radio favourites: {} exceeds {MAX_RADIO_FAVORITES}",
            doc.radio_favorites_uuids.len()
        ));
    }
    for uuid in &doc.radio_favorites_uuids {
        if uuid.len() > MAX_RADIO_UUID_BYTES {
            return Some(format!(
                "radio uuid too long: {} exceeds {MAX_RADIO_UUID_BYTES}",
                uuid.len()
            ));
        }
        aggregate = aggregate.saturating_add(uuid.len());
    }

    if doc.track_added_at.len() > MAX_PERSISTED_TRACKS {
        return Some(format!(
            "too many first-seen records: {} exceeds {MAX_PERSISTED_TRACKS}",
            doc.track_added_at.len()
        ));
    }
    for rec in &doc.track_added_at {
        if let Some(reason) = path_ok(&rec.path, "first-seen") {
            return Some(reason);
        }
        aggregate = aggregate.saturating_add(rec.path.as_os_str().len());
    }

    if doc.track_stats.len() > MAX_PERSISTED_TRACKS {
        return Some(format!(
            "too many play-stats records: {} exceeds {MAX_PERSISTED_TRACKS}",
            doc.track_stats.len()
        ));
    }
    for rec in &doc.track_stats {
        if rec.rating > 5 {
            return Some("rating exceeds five".into());
        }
        if let Some(reason) = path_ok(&rec.path, "play-stats") {
            return Some(reason);
        }
        aggregate = aggregate.saturating_add(rec.path.as_os_str().len());
    }

    if enzui_services::media::sanitize_rate(doc.playback.rate).is_none() {
        return Some("invalid playback rate".into());
    }
    if doc.playback.queue_paths.len() > MAX_QUEUE_PATHS {
        return Some(format!(
            "queue too large: {} exceeds {MAX_QUEUE_PATHS}",
            doc.playback.queue_paths.len()
        ));
    }
    for track in &doc.playback.queue_paths {
        if let Some(reason) = path_ok(track, "queue") {
            return Some(reason);
        }
        aggregate = aggregate.saturating_add(track.as_os_str().len());
    }
    if let Some(current) = &doc.playback.current_track_path {
        if let Some(reason) = path_ok(current, "current track") {
            return Some(reason);
        }
        aggregate = aggregate.saturating_add(current.as_os_str().len());
    }

    if aggregate > MAX_AGGREGATE_STRING_BYTES {
        return Some(format!(
            "aggregate persisted bytes {aggregate} exceeds {MAX_AGGREGATE_STRING_BYTES}"
        ));
    }

    None
}

/// Post-parse semantic checks the pre-retention visitors cannot perform: the
/// playlist-name scalar-count limit and the cross-playlist track aggregate.
/// Byte, cardinality and aggregate limits are enforced earlier by [`bounded`].
fn semantic_over_limit(doc: &PersistedState) -> Option<String> {
    if invalid_queue_order(&doc.playback) {
        return Some("invalid unshuffled queue order".into());
    }
    use crate::domain::{MAX_PLAYLIST_NAME_LEN, MAX_TOTAL_PLAYLIST_TRACKS};
    let mut total: usize = 0;
    for playlist in &doc.playlists {
        let name_len = playlist.name.chars().count();
        if name_len > MAX_PLAYLIST_NAME_LEN {
            return Some(format!(
                "playlist name too long: {name_len} exceeds {MAX_PLAYLIST_NAME_LEN}"
            ));
        }
        total = total.saturating_add(playlist.track_paths.len());
    }
    (total > MAX_TOTAL_PLAYLIST_TRACKS)
        .then(|| format!("aggregate playlist tracks {total} exceeds {MAX_TOTAL_PLAYLIST_TRACKS}"))
}

// Load the document with the device/inode of the descriptor it was read from.
// `None` means no identity could be trusted; refused-but-recoverable reads and
// parse or domain failures still report it, so [`acquire`] can bind a
// malformed inode to an identity-bound Retire.
pub(super) fn load_identified(
    path: &Path,
) -> (Result<PersistedState, LoadError>, Option<(u64, u64)>) {
    let (read_result, identity) = read_state_text(path);
    let text = match read_result {
        Ok(text) => text,
        // A refused read still carries the opened-descriptor identity so an
        // oversized or malformed inode remains retirable.
        Err(error) => return (Err(error), identity),
    };
    // The bounded field readers are the load authority for bytes, cardinality
    // and the aggregate; the save-side `over_domain_limit` is not re-run.
    bounded::reset_retained();
    let parsed = (|| {
        let doc: PersistedState = serde_json::from_str(&text).map_err(|e| {
            let message = e.to_string();
            if message.contains(bounded::BOUND_MARKER) {
                LoadError::OverLimit(message)
            } else {
                LoadError::Corrupt(message)
            }
        })?;
        if doc.version > FORMAT_VERSION {
            return Err(LoadError::Unsupported(doc.version));
        }
        if let Some(id) = duplicate_playlist_id(&doc.playlists) {
            return Err(LoadError::Corrupt(format!("duplicate playlist id {id}")));
        }
        if let Some(reason) = semantic_over_limit(&doc) {
            return Err(LoadError::OverLimit(reason));
        }
        Ok(doc)
    })();
    (parsed, identity)
}

#[cfg(test)]
pub fn save_to(path: &Path, doc: &PersistedState) -> std::io::Result<()> {
    let bytes = serialize_pretty(doc)?;
    write_serialized(path, &bytes)
}

/// Serialise the document in its on-disk representation.
///
/// Kept off the reducer: at the 50,000-track ceiling it costs about a quarter of
/// a frame. Pretty JSON keeps the file a readable record of the listener's
/// playlists and subscriptions.
pub fn serialize_pretty(doc: &PersistedState) -> std::io::Result<Vec<u8>> {
    if let Some(id) = duplicate_playlist_id(&doc.playlists) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("refusing to publish duplicate playlist id {id}"),
        ));
    }
    // Refuse to publish a document the loader would reject, so save and load
    // agree: the app never writes durable state it could not reload.
    if let Some(reason) = over_domain_limit(doc) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("refusing to publish an over-limit document: {reason}"),
        ));
    }
    record_serialization();
    let bytes = serde_json::to_vec_pretty(doc)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_STATE_FILE_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "state exceeds the encoded byte limit",
        ));
    }
    Ok(bytes)
}

#[cfg(test)]
type TestRawWriter =
    Box<dyn FnOnce(&mut dyn std::io::Write, &[u8]) -> std::io::Result<()> + 'static>;

#[cfg(test)]
thread_local! {
    static TEST_RAW_WRITER: std::cell::RefCell<Option<TestRawWriter>> = const {
        std::cell::RefCell::new(None)
    };
}

#[cfg(test)]
pub fn load_from(path: &Path) -> Result<PersistedState, LoadError> {
    load_identified(path).0
}

#[cfg(test)]
pub fn write_serialized(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    #[cfg(test)]
    if let Some(write) = TEST_RAW_WRITER.with(|slot| slot.borrow_mut().take()) {
        return enzui_core::atomic_state::save_atomic_raw_with(path, |staged| write(staged, bytes));
    }

    enzui_core::atomic_state::save_atomic_raw(path, bytes)
}
