//! Playlists and the View enum.

use super::*;

/// User-created playlist, persisted by track path and resolved to ids on load.
#[derive(Clone, Debug)]
pub struct Playlist {
    pub id: u64,
    pub name: String,
    /// Stored as paths for persistence stability.
    pub track_paths: Vec<PathBuf>,
    /// Resolved track ids (populated after scan).
    pub track_ids: Vec<u64>,
}

/// Maximum accepted playlist name length in Unicode scalar values. A longer
/// name is rejected, never truncated.
pub const MAX_PLAYLIST_NAME_LEN: usize = 100;

/// Pre-retention byte ceiling for a persisted playlist name, deserialized
/// before the decoded string is retained. UTF-8 encodes a scalar value in at
/// most four bytes, so this admits every [`MAX_PLAYLIST_NAME_LEN`]-scalar name
/// while bounding the bytes a hostile document can force into one name. The
/// exact scalar-count rule stays the post-parse authority.
pub const MAX_PLAYLIST_NAME_BYTES: usize = MAX_PLAYLIST_NAME_LEN * 4;

/// Maximum number of playlists a persisted document may carry. A document over
/// this cardinality is refused whole rather than partially adopted.
pub const MAX_PERSISTED_PLAYLISTS: usize = 4_096;

/// Maximum number of track paths one persisted playlist may carry.
pub const MAX_TRACKS_PER_PLAYLIST: usize = 50_000;

/// Maximum number of track paths a persisted document may retain across every
/// playlist combined. Bounds total retained memory independently of the raw
/// byte ceiling, which short high-cardinality entries could otherwise defeat.
pub const MAX_TOTAL_PLAYLIST_TRACKS: usize = 50_000;

/// Maximum encoded byte length of any single persisted path, set to the Linux
/// path authority (`PATH_MAX`, 4096 including the terminator). Any path the
/// filesystem can name is admitted; the aggregate retained-byte ceiling
/// ([`MAX_AGGREGATE_STRING_BYTES`]), not this per-path cap, trades long paths
/// against count so a hostile document cannot retain unbounded bytes. A path
/// over this cap is refused, never truncated — the save side refuses it too
/// (`serialize_pretty`), so the app never writes a path it could not reload.
pub const MAX_PERSISTED_PATH_BYTES: usize = 4096;

/// Maximum aggregate encoded bytes of every persisted variable-length value
/// (all paths, playlist names, feed url/title/description strings and radio
/// uuids) that one document may retain. Enforced during deserialization before
/// each next string is retained and again on save, so retention is bounded
/// regardless of how a document distributes bytes across long values or high
/// cardinality. Sized for a full realistic library (~50,000 tracks with
/// per-track first-seen and play-stats paths, playlists, queue and
/// subscriptions, ~20 MiB) with headroom.
pub const MAX_AGGREGATE_STRING_BYTES: usize = 48 * 1024 * 1024;

/// Maximum number of library root folders a persisted document may carry.
pub const MAX_LIBRARY_FOLDERS: usize = 1_024;

/// Maximum number of per-track records (play stats and first-seen) a persisted
/// document may retain. One record exists per library track, so this matches
/// the app's realistic library ceiling (see `persistence_perf`: 50,000 tracks).
pub const MAX_PERSISTED_TRACKS: usize = 50_000;

/// Maximum number of queued track paths a persisted transport session may hold.
pub const MAX_QUEUE_PATHS: usize = 10_000;

/// Maximum number of podcast subscriptions a persisted document may carry.
pub const MAX_SUBSCRIPTIONS: usize = 1_024;

/// Maximum encoded byte length of a persisted subscription feed URL.
pub const MAX_FEED_URL_BYTES: usize = 2_048;

/// Maximum encoded byte length of a persisted subscription title.
pub const MAX_FEED_TITLE_BYTES: usize = 512;

/// Maximum encoded byte length of a persisted subscription description.
pub const MAX_FEED_DESCRIPTION_BYTES: usize = 4_096;

/// Maximum number of per-episode resume records a persisted document
/// may carry. A record exists only for an episode the listener actually
/// started, so this is far below `subscriptions × episodes`; the cap exists
/// to keep a hostile document bounded, not to constrain real use.
pub const MAX_EPISODE_RESUME: usize = 8_192;

/// Maximum encoded byte length of a persisted episode GUID. A GUID is
/// frequently a full item URL, so it shares the feed-URL ceiling.
pub const MAX_EPISODE_GUID_BYTES: usize = MAX_FEED_URL_BYTES;

/// Maximum number of radio favourites (legacy `u64` ids or durable UUID
/// strings) a persisted document may carry in either favourites collection.
pub const MAX_RADIO_FAVORITES: usize = 4_096;

/// Maximum encoded byte length of a persisted radio-favourite station UUID.
/// A Radio Browser `stationuuid` is 36 bytes; 64 leaves slack without admitting
/// an attacker-sized string.
pub const MAX_RADIO_UUID_BYTES: usize = 64;

/// Reason a proposed playlist name was rejected.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PlaylistNameError {
    /// Empty or all whitespace after trimming.
    Empty,
    /// Longer than [`MAX_PLAYLIST_NAME_LEN`] scalar values.
    TooLong,
    /// Another playlist already has this exact name.
    Duplicate,
}

impl PlaylistNameError {
    /// The single-line message shown beneath the rename field.
    #[must_use]
    pub fn message(self) -> &'static str {
        match self {
            Self::Empty => "Enter a playlist name.",
            Self::TooLong => "That name is too long.",
            Self::Duplicate => "A playlist with that name already exists.",
        }
    }
}

/// Bounded, per-import counts reported after a settled playlist import.
///
/// `imported` + `missing` is the number of valid rows admitted by the parser
/// (every one is retained as a durable path); `imported` are those that resolve
/// to a library track right now, `missing` are retained for a later scan to
/// reconcile. `duplicate` and `refused` are carried straight from the parser's
/// [`crate::playlist_io::ParseReport`] so the user sees why a document's line
/// count and the imported count differ.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ImportCounts {
    /// Rows added with a resolved, in-library track id.
    pub imported: usize,
    /// Valid rows whose path is not in the library yet, retained durably.
    pub missing: usize,
    /// Rows the parser dropped as duplicates of an earlier admitted path.
    pub duplicate: usize,
    /// Rows the parser refused (control byte, non-file scheme, credentials,
    /// overlong, non-UTF-8).
    pub refused: usize,
}

/// Monotonic playlist-id allocator.
///
/// Hands out strictly increasing ids and refuses once the `u64` space is
/// exhausted, so a deleted id is never reused and a rename dialog holding a
/// deleted id can never alias a later playlist.
#[derive(Clone, Copy, Debug)]
pub struct PlaylistIdAllocator {
    next: Option<u64>,
}

impl Default for PlaylistIdAllocator {
    fn default() -> Self {
        Self { next: Some(0) }
    }
}

impl PlaylistIdAllocator {
    /// Allocate the next id, or `None` once the id space is exhausted.
    pub fn allocate(&mut self) -> Option<u64> {
        let id = self.next?;
        self.next = id.checked_add(1);
        Some(id)
    }

    /// Advance allocation strictly past `max_existing`, keeping ids increasing
    /// across relaunch. Monotonic: a lower `max_existing` never rewinds a live
    /// allocator, an exhausted one stays exhausted, and a `u64::MAX` maximum
    /// exhausts it; `None` leaves it unchanged.
    pub fn reserve_past(&mut self, max_existing: Option<u64>) {
        let Some(current) = self.next else {
            return;
        };
        let Some(m) = max_existing else {
            return;
        };
        match m.checked_add(1) {
            None => self.next = None,
            Some(candidate) if candidate > current => self.next = Some(candidate),
            Some(_) => {}
        }
    }
}

// ---------------------------------------------------------------------------
// Enums that shape the UI
// ---------------------------------------------------------------------------

macro_rules! view_routes {
    ($( $(#[$docs:meta])* $route:ident ),+ $(,)?) => {
        /// Which top-level pane is showing on the master (left / stacked) side.
        #[derive(Clone, Copy, Debug, Eq, PartialEq)]
        pub enum View {
            $( $(#[$docs])* $route, )+
        }

        impl View {
            /// Every top-level route, generated by the declaration that defines
            /// the enum so route-wide gates cannot silently omit a new variant.
            pub const ALL: &'static [Self] = &[$(Self::$route),+];
        }
    };
}

view_routes! {
    Home,
    Artists,
    Albums,
    Genres,
    /// A distinct tags surface, separate from Genres. Tags are the
    /// per-track free-form labels (`Track::tags`), where Genres is the single
    /// primary genre string; a track can carry many tags but exactly one
    /// genre.
    Tags,
    Playlist,
    /// The internet-radio surface: Discover, Search and Favorites over the
    /// station catalog in `radio.rs`.
    Radio,
    /// The podcasts surface: subscribe field, subscribed feeds, and per-feed
    /// management, backed by `podcasts.rs`.
    Podcasts,
    Spotify,
    Search,
    Queue,
}

impl View {
    pub fn label(self) -> &'static str {
        match self {
            View::Home => "Songs",
            View::Artists => "Artists",
            View::Albums => "Albums",
            View::Genres => "Genres",
            View::Tags => "Tags",
            View::Playlist => "Playlists",
            View::Radio => "Radio",
            View::Podcasts => "Podcasts",
            View::Spotify => "Spotify",
            View::Search => "Search",
            View::Queue => "Up Next",
        }
    }

    pub fn icon_name(self) -> &'static str {
        match self {
            View::Home => "music-2",
            View::Artists => "mic-vocal",
            View::Albums => "disc-3",
            View::Genres => "music-4",
            View::Tags => "tag",
            View::Playlist => "list-music",
            View::Radio => "radio",
            View::Podcasts => "podcast",
            View::Spotify => "audio-lines",
            View::Search => "search",
            View::Queue => "list-ordered",
        }
    }

    /// Views that appear in the nav bar (not modal side-trips).
    pub fn nav_views() -> &'static [View] {
        &[
            View::Home,
            View::Artists,
            View::Albums,
            View::Genres,
            View::Tags,
            View::Playlist,
            View::Radio,
            View::Podcasts,
            View::Spotify,
        ]
    }
}
