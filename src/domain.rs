//! Domain model — data structures for the music library.
//!
//! Key invariants:
//! - `Track.id` is per-scan and is never persisted. Durable identity is the
//!   file path.
//! - Playlists persist track paths, resolved to ids on load.
//! - Albums/Artists/Genres are derived from track metadata via `Library::reindex`.

use std::collections::HashMap;
use std::path::PathBuf;

// ---------------------------------------------------------------------------
// Core data types
// ---------------------------------------------------------------------------

#[path = "domain_parts/audio.rs"]
mod audio;
#[path = "domain_parts/library_a.rs"]
mod library_a;
#[path = "domain_parts/library_b.rs"]
mod library_b;
#[path = "domain_parts/playlists.rs"]
mod playlists;
#[path = "domain_parts/tracklist.rs"]
mod tracklist;
pub use audio::{
    EQ_BAND_COUNT, EQ_BAND_LABELS, EQ_GAIN_MAX_DB, EQ_GAIN_MIN_DB, EqPreset, EqState,
    RG_PREAMP_MAX_DB, RG_PREAMP_MIN_DB, ReplayGain, ReplayGainMode, ReplayGainState,
    SLEEP_FADE_SECS, SleepPreset, SleepTimerState, sleep_fade_status,
};
#[cfg(test)]
pub use audio::{RG_MAX_SCALAR, effective_sample_scale, effective_sample_scale_with_rg};
pub use library_a::Library;
pub(crate) use library_a::track_tie_break;
pub use playlists::{
    ImportCounts, MAX_AGGREGATE_STRING_BYTES, MAX_EPISODE_GUID_BYTES, MAX_EPISODE_RESUME,
    MAX_FEED_DESCRIPTION_BYTES, MAX_FEED_TITLE_BYTES, MAX_FEED_URL_BYTES, MAX_LIBRARY_FOLDERS,
    MAX_PERSISTED_PATH_BYTES, MAX_PERSISTED_PLAYLISTS, MAX_PERSISTED_TRACKS,
    MAX_PLAYLIST_NAME_BYTES, MAX_PLAYLIST_NAME_LEN, MAX_QUEUE_PATHS, MAX_RADIO_FAVORITES,
    MAX_RADIO_UUID_BYTES, MAX_SUBSCRIPTIONS, MAX_TOTAL_PLAYLIST_TRACKS, MAX_TRACKS_PER_PLAYLIST,
    Playlist, PlaylistNameError, View,
};
pub use tracklist::{
    AVAILABILITY_EXHAUSTED_DISCLOSURE, BULK_NONE_PLAYABLE_DISCLOSURE, QUEUE_FULL_DISCLOSURE,
    SearchResults, TrackAvailability, TrackListExpansion, TrackListScope, owning_root,
    track_list_visible,
};

/// A single audio track with metadata and cover art references.
#[derive(Clone, Debug)]
pub struct Track {
    /// Per-scan identifier, not stable across runs.
    pub id: u64,
    /// The real identity for persistence.
    pub path: PathBuf,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub album_artist: String,
    pub track_no: u32,
    pub disc_no: u32,
    pub year: u32,
    pub genre: String,
    pub format: String,
    pub duration_secs: f64,
    pub bitrate_kbps: u32,
    pub sample_rate: u32,
    pub channels: u8,
    pub bit_depth: u8,
    pub cover_path: Option<PathBuf>,
    /// The decoded, downscaled cover, or `None` when the file has no picture
    /// and no sidecar.
    ///
    /// Decoded off the UI thread by the scanner; identical artwork shares pixels
    /// through [`crate::art::CoverCache`]. Encoded tag bytes are not retained.
    pub cover: Option<crate::art::CoverArt>,
    pub added_at: u64,
    /// How many times this track has been started via [`PlaybackState::play_track`].
    ///
    /// Drives `SortMode::MostPlayed` and the Home `Most Played` section. Kept on
    /// `Track` rather than a side table so a rescan cannot silently drop it.
    pub play_count: u32,
    /// A monotonic per-library "last played" ordering; `0` means never played.
    ///
    /// Memory-only tiebreak for `Recently Played`: [`Track::last_played_unix`] is
    /// the persisted primary key, and this orders plays within the same second.
    /// Every credit path stamps both.
    pub last_played_seq: u64,
    /// The Unix second at which the last playback completion was credited;
    /// `0` means never played.
    ///
    /// Persisted via `persistence::TrackStatsRec`, reconciled at boot by
    /// [`crate::MusicApp::finalize_pending_track_stats`], and preserved across a
    /// rescan. Only a backend `Message::PlaybackCompleted` whose generation
    /// matches the current source credits a play; pressing Play does not.
    pub last_played_unix: u64,
    /// The ReplayGain values read from this file's tags.
    ///
    /// All `None` for an untagged file, which [`ReplayGainState::scalar_for`]
    /// treats as a no-op.
    pub replay_gain: ReplayGain,
}

impl Track {
    pub fn display_title(&self) -> &str {
        if self.title.is_empty() {
            self.path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("Unknown")
        } else {
            &self.title
        }
    }

    pub fn display_album(&self) -> &str {
        if self.album.is_empty() {
            "Unknown Album"
        } else {
            &self.album
        }
    }

    pub fn display_artist(&self) -> &str {
        if self.artist.is_empty() {
            "Unknown Artist"
        } else {
            &self.artist
        }
    }

    /// Format duration as "M:SS".
    pub fn duration_display(&self) -> String {
        format_duration(self.duration_secs)
    }
}

/// Seedless splitmix64 shuffle key for [`SortMode::Shuffle`].
///
/// Stable across calls so a re-render keeps the order, and well mixed so the
/// result does not track id order.
pub(crate) fn shuffle_key(id: u64) -> u64 {
    let mut z = id.wrapping_add(0x9E3779B97F4A7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
    z ^ (z >> 31)
}

/// Format seconds as "M:SS" or "H:MM:SS" for long tracks.
pub fn format_duration(secs: f64) -> String {
    // Floor, not round: an elapsed clock must not show a second that has not
    // happened, and `enz_media_controls` floors its own labels too.
    let total = secs.max(0.0).floor() as u64;
    let h = total / 3600;
    let m = (total % 3600) / 60;
    let s = total % 60;
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

/// A grouped album, computed from a list of tracks.
#[derive(Clone, Debug)]
pub struct Album {
    pub id: u64,
    pub title: String,
    pub artist: String,
    /// Release year from tags; not yet rendered.
    #[allow(dead_code)]
    pub year: u32,
    pub track_ids: Vec<u64>,
    #[allow(dead_code)]
    pub cover_path: Option<PathBuf>,
    /// The cover of the first track on the album that has one, or `None`
    /// when no track carries art.
    pub cover: Option<crate::art::CoverArt>,
}

impl Album {
    /// Artist credit, or “Unknown Artist” when metadata is empty.
    pub fn display_artist(&self) -> &str {
        if self.artist.is_empty() {
            "Unknown Artist"
        } else {
            &self.artist
        }
    }
}

/// An artist grouping.
#[derive(Clone, Debug)]
pub struct Artist {
    pub id: u64,
    pub name: String,
    pub album_ids: Vec<u64>,
    pub track_ids: Vec<u64>,
}

/// A genre grouping.
#[derive(Clone, Debug)]
pub struct Genre {
    pub id: u64,
    pub name: String,
    pub track_ids: Vec<u64>,
    pub album_ids: Vec<u64>,
}

/// A tag grouping split out of [`Track::genre`] on comma, slash or semicolon.
///
/// `"Rock, Alternative"` yields `Rock` and `Alternative`. Tags are derived,
/// not persisted.
#[derive(Clone, Debug)]
pub struct Tag {
    /// Derived from the lowercased name, so a tag keeps its id, and an open
    /// tag filter its target, across library rebuilds.
    pub id: u64,
    pub name: String,
    pub track_ids: Vec<u64>,
}

/// Split a genre string into trimmed, non-blank tags in source order.
pub fn split_tags(genre: &str) -> Vec<String> {
    genre
        .split([',', '/', ';'])
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect()
}

/// Library ordering; `MostPlayed` is reserved for the Home ranking section.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SortMode {
    Album,
    Artist,
    Title,
    Recent,
    /// Deterministic shuffle derived from the track id, so a re-render keeps
    /// the order. See [`Library::ordered_tracks`].
    Shuffle,
    /// Rank by [`Track::play_count`] descending, then by title.
    ///
    /// Not in [`SortMode::ALL`]: it ranks the Home `Most Played` section.
    MostPlayed,
    Rating,
}

impl SortMode {
    pub const ALL: &'static [SortMode] = &[
        SortMode::Album,
        SortMode::Artist,
        SortMode::Title,
        SortMode::Recent,
        SortMode::Shuffle,
        SortMode::Rating,
    ];

    pub fn label(self) -> &'static str {
        match self {
            SortMode::Rating => "Rating",
            SortMode::Album => "Album",
            SortMode::Artist => "Artist",
            SortMode::Title => "Title",
            SortMode::Recent => "Recent",
            SortMode::Shuffle => "Shuffle",
            SortMode::MostPlayed => "Most Played",
        }
    }

    pub fn menu_label(self) -> &'static str {
        match self {
            SortMode::Rating => "Sort by Rating",
            SortMode::Album => "Sort by Album",
            SortMode::Artist => "Sort by Artist",
            SortMode::Title => "Sort by Title",
            SortMode::Recent => "Sort by Recent",
            SortMode::Shuffle => "Sort by Shuffle",
            SortMode::MostPlayed => "Sort by Most Played",
        }
    }
}

/// Right-click context target, keyed by the clicked entity rather than the view.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ContextTarget {
    Track(u64),
    Album(u64),
    Artist(u64),
    /// One queue occurrence, keyed by its immutable [`QueueEntryId`].
    ///
    /// A stale menu whose occurrence left the queue resolves to no position
    /// and is inert.
    QueueEntry(QueueEntryId),
    PlaylistRow(u64),
    Genre(u64),
    /// An internet-radio station, keyed by an id derived from its durable
    /// `remote_uuid`.
    ///
    /// The id survives a catalog reload; a vanished station resolves to no
    /// `radio.station(id)`, so a stale menu is inert.
    RadioStation(u64),
    /// A podcast episode, keyed by the per-open menu generation.
    ///
    /// The durable `(feed_url, guid)` identity rides on
    /// [`crate::context_menu::OpenContextMenu::podcast_key`].
    PodcastEpisode(u64),
}

/// The set of actions a right-click menu can perform on a `ContextTarget`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ContextAction {
    Play,
    PlayNext,
    AddToQueue,
    /// Open the add-to-playlist picker for a track or album target.
    AddToPlaylist,
    GoToAlbum,
    GoToArtist,
    RemoveFromQueue,
    MoveToTopOfQueue,
    TrackInfo,
    /// Re-probe this track's owning root through the finite watcher so a
    /// Missing/Undecodable row can recover to Present without a full rescan.
    Rescan,
    /// Delete this durable track from the library (never its file),
    /// routing the queue/current update through shared domain logic.
    Remove,
    /// Star or unstar a radio station via `Message::RadioToggleFavorite`.
    ///
    /// The menu composes its label from live `RadioState`, not [`Self::label`].
    ToggleFavorite,
    /// Open the playlist Rename modal.
    RenamePlaylist,
    /// Delete a playlist after the `Message::ConfirmDeletePlaylist` modal.
    DeletePlaylist,
}

impl ContextAction {
    pub fn label(self) -> &'static str {
        match self {
            ContextAction::Play => "Play",
            ContextAction::PlayNext => "Play next",
            ContextAction::AddToQueue => "Add to queue",
            ContextAction::AddToPlaylist => "Add to playlist\u{2026}",
            ContextAction::GoToAlbum => "Go to album",
            ContextAction::GoToArtist => "Go to artist",
            ContextAction::RemoveFromQueue => "Remove from queue",
            ContextAction::MoveToTopOfQueue => "Move to top",
            ContextAction::TrackInfo => "Track info",
            ContextAction::Rescan => "Rescan",
            ContextAction::Remove => "Remove from library",
            // Fallback only; the radio menu composes a stateful label.
            ContextAction::ToggleFavorite => "Favourite",
            ContextAction::RenamePlaylist => "Rename\u{2026}",
            ContextAction::DeletePlaylist => "Delete",
        }
    }

    /// The Lucide / semantic icon on this row.
    pub fn icon(self) -> &'static str {
        match self {
            // `track.play` / `album.play` / `queue.play-now`
            ContextAction::Play => "media.play",
            ContextAction::PlayNext => "list-start",
            ContextAction::AddToQueue => "list-end",
            ContextAction::AddToPlaylist => "list-plus",
            ContextAction::GoToAlbum => "disc-3",
            ContextAction::GoToArtist => "mic-vocal",
            ContextAction::RemoveFromQueue => "trash-2",
            ContextAction::MoveToTopOfQueue => "arrow-up-to-line",
            ContextAction::TrackInfo => "info",
            ContextAction::Rescan => "refresh-cw",
            ContextAction::Remove => "trash-2",
            ContextAction::ToggleFavorite => "star",
            ContextAction::RenamePlaylist => "pencil",
            ContextAction::DeletePlaylist => "trash-2",
        }
    }

    /// Which separator-delimited group the row belongs to.
    ///
    /// Rendering inserts a rule wherever this value changes, so separators
    /// cannot outlive the rows they fence.
    pub fn group(self) -> u8 {
        match self {
            ContextAction::Play
            | ContextAction::PlayNext
            | ContextAction::AddToQueue
            | ContextAction::MoveToTopOfQueue => 0,
            ContextAction::AddToPlaylist => 1,
            ContextAction::GoToAlbum | ContextAction::GoToArtist => 2,
            ContextAction::RemoveFromQueue => 3,
            ContextAction::TrackInfo => 4,
            ContextAction::Rescan | ContextAction::Remove => 5,
            // The radio favourite shares Play's group; playlist Rename/Delete
            // trail in their own.
            ContextAction::ToggleFavorite => 0,
            ContextAction::RenamePlaylist | ContextAction::DeletePlaylist => 6,
        }
    }

    /// The row's `EntryId` in `enz_menu`, derived from the action so the
    /// keyboard cursor follows the row across a reorder.
    pub fn key(self) -> &'static str {
        match self {
            ContextAction::Play => "play",
            ContextAction::PlayNext => "play-next",
            ContextAction::AddToQueue => "add-to-queue",
            ContextAction::AddToPlaylist => "add-to-playlist",
            ContextAction::GoToAlbum => "go-to-album",
            ContextAction::GoToArtist => "go-to-artist",
            ContextAction::RemoveFromQueue => "remove-from-queue",
            ContextAction::MoveToTopOfQueue => "move-to-top",
            ContextAction::TrackInfo => "track-info",
            ContextAction::Rescan => "rescan-track",
            ContextAction::Remove => "remove-track",
            ContextAction::ToggleFavorite => "toggle-favorite",
            ContextAction::RenamePlaylist => "rename-playlist",
            ContextAction::DeletePlaylist => "delete-playlist",
        }
    }
}

/// The menu items available for a context target.
pub fn context_menu_items(target: ContextTarget) -> Vec<ContextAction> {
    use ContextAction::*;
    match target {
        ContextTarget::Track(_) => {
            vec![
                Play,
                PlayNext,
                AddToQueue,
                AddToPlaylist,
                GoToAlbum,
                GoToArtist,
                TrackInfo,
                // `items_for` shows these only for a Missing/Undecodable source.
                Rescan,
                Remove,
            ]
        }
        ContextTarget::Album(_) => vec![Play, AddToQueue, AddToPlaylist],
        ContextTarget::Artist(_) => vec![Play, AddToQueue],
        ContextTarget::QueueEntry(_) => {
            vec![
                Play,
                MoveToTopOfQueue,
                RemoveFromQueue,
                TrackInfo,
                Rescan,
                Remove,
            ]
        }
        ContextTarget::PlaylistRow(_) => vec![Play, AddToQueue, RenamePlaylist, DeletePlaylist],
        ContextTarget::Genre(_) => vec![Play, AddToQueue],
        // The queue holds track ids, so a stream cannot be queued.
        ContextTarget::RadioStation(_) => vec![Play, ToggleFavorite],
        ContextTarget::PodcastEpisode(_) => vec![Play],
    }
}

/// Shorten `s` to at most `max` characters, ending in an ellipsis when cut.
///
/// Every context menu's Play row is decorated with the target's title; a long
/// album title would otherwise blow the menu past its width cap.
pub fn truncate_label(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let head: String = s.chars().take(max.saturating_sub(1)).collect();
        format!("{head}\u{2026}")
    }
}

/// What the "Add to playlist" picker is currently adding.
///
/// [`Library::picker_track_ids`] resolves the ids at commit time, so an album
/// target reflects the album's current contents.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PickerTarget {
    /// A single track, by id. Entry point: a track row's overflow menu.
    Track(u64),
    /// A whole album, by album id — every track on it, in album order.
    /// Entry point: an album's overflow menu.
    Album(u64),
    /// The track playing right now. Entry point: the now-playing bar.
    /// Resolves to nothing when the queue is empty / nothing is playing.
    CurrentTrack,
}

/// The picker's draft state while it is open; `None` in the app means closed.
///
/// Destination playlists are read live from the [`Library`] on every render.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlaylistPicker {
    /// What this picker is adding.
    pub target: PickerTarget,
    /// The draft name for the "New playlist…" affordance. Empty until typed.
    pub new_name: String,
}

impl PlaylistPicker {
    /// Open a picker for `target` with an empty new-playlist name.
    pub fn new(target: PickerTarget) -> Self {
        Self {
            target,
            new_name: String::new(),
        }
    }
}

/// Transient state of the playlist Rename dialog.
///
/// The commit resolves `playlist_id` against the live [`Library`], so a dialog
/// left open past a delete renames nothing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlaylistRename {
    pub playlist_id: u64,
    pub draft: String,
}

impl PlaylistRename {
    /// Open a rename dialog for `playlist_id`, seeded with its current name.
    pub fn new(playlist_id: u64, current_name: String) -> Self {
        Self {
            playlist_id,
            draft: current_name,
        }
    }
}

/// Repeat playback mode.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RepeatMode {
    Off,
    All,
    One,
}

/// Optional filter applied to the master content (e.g. "albums by artist X").
/// A chip on the top of the filtered view lets the user clear it.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum LibraryFilter {
    Rated,
    #[default]
    None,
    Artist(u64),
    Album(u64),
    Genre(u64),
    /// A tag, by its name-derived [`Tag::id`].
    Tag(u64),
    Playlist(u64),
}

// ---------------------------------------------------------------------------
// Shell layout state — the spine of the adaptive UI
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Library — the in-memory music database
// ---------------------------------------------------------------------------

include!("domain/artist_names.rs");
include!("domain/playback.rs");

#[cfg(test)]
#[path = "domain/load_current_track_tests.rs"]
mod load_current_track_tests;
#[cfg(test)]
#[path = "domain/replay_gain_tests.rs"]
mod replay_gain_tests;

#[cfg(test)]
#[path = "domain/stream_selection_tests.rs"]
mod stream_selection_tests;
#[cfg(test)]
#[path = "domain/tests.rs"]
mod tests;
