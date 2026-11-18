//! Track-list truncation, expansion, availability, and roots.

use super::*;

// ---------------------------------------------------------------------------
// Track-list truncation shared by Home and Search
// ---------------------------------------------------------------------------

/// How many track rows a collapsed track list renders.
///
/// [`track_list_visible`] is the only reader, and `views::track_list_section`
/// always shows the total and a `Show all N tracks` control beside the cap.
pub const TRACK_LIST_PAGE: usize = 50;

/// How many rows of a `total`-item track list to build this frame.
///
/// `expanded` comes from `Show all N tracks` and is held per surface in
/// [`TrackListExpansion`].
pub fn track_list_visible(total: usize, expanded: bool) -> usize {
    if expanded {
        total
    } else {
        total.min(TRACK_LIST_PAGE)
    }
}

/// Which track list a reveal applies to.
///
/// Surfaces do not share one flag: a search expansion must not outlive its
/// query.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum TrackListScope {
    /// The Home view's library track list.
    Library,
    /// The Search view's `Tracks` result section.
    Search,
}

/// Per-surface "show all" state, collapsed by default.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct TrackListExpansion {
    library: bool,
    search: bool,
}

impl TrackListExpansion {
    pub fn is_expanded(self, scope: TrackListScope) -> bool {
        match scope {
            TrackListScope::Library => self.library,
            TrackListScope::Search => self.search,
        }
    }

    pub fn set(&mut self, scope: TrackListScope, expanded: bool) {
        match scope {
            TrackListScope::Library => self.library = expanded,
            TrackListScope::Search => self.search = expanded,
        }
    }

    /// Collapse the Search section back to its first page.
    ///
    /// Called when the query changes, so a reveal does not carry over to a
    /// different result set.
    pub fn reset_search(&mut self) {
        self.search = false;
    }
}

/// Result of a library search: tracks, albums and artists.
#[derive(Clone, Debug, Default)]
pub struct SearchResults {
    pub tracks: Vec<u64>,
    pub albums: Vec<u64>,
    pub artists: Vec<u64>,
}

impl SearchResults {
    pub fn is_empty(&self) -> bool {
        self.tracks.is_empty() && self.albums.is_empty() && self.artists.is_empty()
    }

    pub fn total(&self) -> usize {
        self.tracks.len() + self.albums.len() + self.artists.len()
    }
}

// ---------------------------------------------------------------------------
// Track availability authority
// ---------------------------------------------------------------------------

/// Whether a library row's underlying audio source can actually be played,
/// held per durable path identity by [`AvailabilityAuthority`].
///
/// Runtime-only and never serialized: a row is `Unknown` until a completed
/// authoritative probe decides it. Availability never deletes a track row.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TrackAvailability {
    /// No authoritative scan has decided this path yet (default; rows restored
    /// from persistence start here).
    #[default]
    Unknown,
    /// A completed probe proved the source present and decodable.
    Present,
    /// A complete/authoritative scan or a vanished root found the path absent,
    /// unmounted, or vanished. The row is kept; the source is unreachable.
    Missing,
    /// The path is an admitted regular audio candidate whose metadata/decode
    /// probe failed. The row is kept and is non-actionable for playback.
    Undecodable,
    /// A permission/transient/partial scan could not decide this path. It must
    /// never authorize removal and must never downgrade an unrelated row.
    Unverified,
}

/// The bounded, stable disclosure text for a missing source.
///
/// Never interpolates a path, so no music path leaks into screenshots.
pub const TRACK_MISSING_DISCLOSURE: &str =
    "This track's file is missing. Rescan to restore it, or remove it from the library.";
/// The bounded, stable disclosure text for an undecodable source.
pub const TRACK_UNDECODABLE_DISCLOSURE: &str =
    "This track can't be played — its file couldn't be read. Rescan after fixing it, or remove it.";
/// The bounded, stable disclosure text for an unverified source.
pub const TRACK_UNVERIFIED_DISCLOSURE: &str =
    "This track hasn't been verified yet. Rescan the library to check it.";
/// The bounded, stable disclosure text for a not-yet-verified (freshly
/// restored) source. It is not healthy until a completed scan proves it
/// present, so it reads `Checking` rather than playing.
pub const TRACK_CHECKING_DISCLOSURE: &str =
    "Checking this track — it will be playable once the library scan confirms it.";
/// The bounded, terminal disclosure shown once when the session's availability
/// generation counter is exhausted. No further scan/backend decision is
/// admitted and local playback is refused until the app is restarted; a rescan
/// after restart resumes normal verification.
pub const AVAILABILITY_EXHAUSTED_DISCLOSURE: &str = "Track verification is exhausted for this session. Restart the app to keep \
     playing local files.";
/// The bounded, actionable disclosure when a bulk play/queue request resolves
/// to no playable track (every candidate is missing or unplayable).
pub const BULK_NONE_PLAYABLE_DISCLOSURE: &str = "None of these tracks can be played — they're missing or unplayable. \
     Rescan the library to restore them.";
/// The bounded, actionable disclosure shown when an enqueue is refused because
/// the queue is full or the occurrence-id counter is exhausted.
///
/// The refusal happens before any mutation, so the queue is unchanged.
pub const QUEUE_FULL_DISCLOSURE: &str =
    "The play queue is full. Remove some tracks before adding more.";

impl TrackAvailability {
    /// Whether a completed authoritative probe proved this source present and
    /// decodable; an unverified row is neither present nor proven bad.
    #[must_use]
    pub fn is_present(self) -> bool {
        matches!(self, TrackAvailability::Present)
    }

    /// Whether an authoritative probe proved this source bad:
    /// [`Missing`](Self::Missing) or [`Undecodable`](Self::Undecodable).
    ///
    /// A bad source stops playback. An unverified row is not playable but is
    /// not proven bad.
    #[must_use]
    pub fn blocks_playback(self) -> bool {
        matches!(
            self,
            TrackAvailability::Missing | TrackAvailability::Undecodable
        )
    }

    /// Whether this state offers the Rescan/Remove recovery actions.
    ///
    /// True for the proven-bad states and [`Unverified`](Self::Unverified);
    /// [`Unknown`](Self::Unknown) already has a scan in flight.
    #[must_use]
    pub fn offers_recovery(self) -> bool {
        matches!(
            self,
            TrackAvailability::Missing
                | TrackAvailability::Undecodable
                | TrackAvailability::Unverified
        )
    }

    /// A bounded, stable disclosure category for a non-present source, or
    /// `None` for [`Present`](Self::Present) and [`Unknown`](Self::Unknown)
    /// (nothing actionable to disclose yet).
    #[must_use]
    pub fn disclosure(self) -> Option<&'static str> {
        match self {
            TrackAvailability::Present => None,
            TrackAvailability::Unknown => Some(TRACK_CHECKING_DISCLOSURE),
            TrackAvailability::Missing => Some(TRACK_MISSING_DISCLOSURE),
            TrackAvailability::Undecodable => Some(TRACK_UNDECODABLE_DISCLOSURE),
            TrackAvailability::Unverified => Some(TRACK_UNVERIFIED_DISCLOSURE),
        }
    }

    /// A short status-badge label for a row, or `None` when the row needs no
    /// badge ([`Present`](Self::Present)/[`Unknown`](Self::Unknown)).
    #[must_use]
    pub fn badge_label(self) -> Option<&'static str> {
        match self {
            TrackAvailability::Present => None,
            TrackAvailability::Unknown => Some("Checking"),
            TrackAvailability::Missing => Some("Missing"),
            TrackAvailability::Undecodable => Some("Unplayable"),
            TrackAvailability::Unverified => Some("Unverified"),
        }
    }
}

/// One authority entry: the decided state and the scan generation that decided
/// it, so an older completion cannot overwrite a fresher decision.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AvailabilityRecord {
    pub state: TrackAvailability,
    pub generation: u64,
}

/// The availability authority keyed by durable path identity.
///
/// Keying by path means a scan result can never transfer status to another
/// row.
#[derive(Clone, Debug, Default)]
pub struct AvailabilityAuthority {
    entries: HashMap<PathBuf, AvailabilityRecord>,
}

impl AvailabilityAuthority {
    /// The decided state for `path`, or [`TrackAvailability::Unknown`] when no
    /// authoritative scan has ruled on it.
    #[must_use]
    pub fn state(&self, path: &std::path::Path) -> TrackAvailability {
        self.entries.get(path).map(|r| r.state).unwrap_or_default()
    }

    /// Record `state` for `path` under `generation` unless a newer decision
    /// exists; returns whether the record changed.
    pub fn decide(&mut self, path: PathBuf, state: TrackAvailability, generation: u64) -> bool {
        if let Some(existing) = self.entries.get(&path) {
            if existing.generation > generation {
                return false;
            }
            if existing.generation == generation && existing.state == state {
                return false;
            }
        }
        self.entries
            .insert(path, AvailabilityRecord { state, generation });
        true
    }

    /// Forget the decision for `path` when its row is removed, so a re-add
    /// starts from [`TrackAvailability::Unknown`].
    pub fn forget(&mut self, path: &std::path::Path) {
        self.entries.remove(path);
    }
}

/// The deepest configured `root` in `roots` that is an ancestor of `path`, or
/// `None` when no configured root owns it, so nested roots each own their
/// own subtree.
#[must_use]
pub fn owning_root<'a>(
    path: &std::path::Path,
    roots: &'a [PathBuf],
) -> Option<&'a std::path::Path> {
    roots
        .iter()
        .map(PathBuf::as_path)
        .filter(|root| path.starts_with(root))
        .max_by_key(|root| root.as_os_str().len())
}
