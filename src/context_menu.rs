//! Right-click context menus.
//!
//! [`crate::domain::context_menu_items`] decides which actions a target kind
//! offers; this module owns the right-click capture, open state, menu model and
//! reducer wiring. The framework's `enz_context_menu_host` resolves its anchor
//! from its own trigger's layout, which a track row inside a scrolled column is
//! not, so the app supplies the cursor point reported by `enz_context_target`.
//!
//! * [`items_for`] narrows the domain's item set with per-target guards.
//! * [`label_for`] composes each row's visible label.
//! * Separators are derived from [`ContextAction::group`]: one is emitted
//!   wherever the group changes, so a dropped item cannot leave a dangling rule.

use enzui_core::context_menu::{InvocationKind, Point as CorePoint};
use enzui_core::menu_model::{EntryId, MenuEntryModel, MenuModel};
use enzui_widgets::context_menu_host::{ContextMenuState, FocusRestoreTarget};
use iced::Point;

use crate::domain::{ContextAction, ContextTarget, Library, PlaybackState, Track, truncate_label};

/// The id namespace a genre row's focusable surface derives its widget id
/// under, keyed by the immutable library id so a rename cannot move it.
pub const GENRE_ROW_ID_PREFIX: &str = "music-genre-row";

/// The id namespace a playlist list row's focusable surface derives its widget
/// id under, keyed by the immutable playlist id.
pub const PLAYLIST_ROW_ID_PREFIX: &str = "music-playlist-row";

/// The id namespace a radio station row's focusable surface derives its widget
/// id under, keyed by the durable `remote_uuid`.
pub const RADIO_ROW_ID_PREFIX: &str = "music-radio-row";

/// The id namespace a podcast episode row's focusable surface derives its
/// widget id under, keyed by the durable `(feed_url, guid)`.
pub const PODCAST_EPISODE_ROW_ID_PREFIX: &str = "music-podcast-episode-row";

/// The widget id a genre row's focusable surface derives from the genre's
/// immutable library id; shared by the render site and [`menu_invoker_id`].
pub fn genre_row_id(genre_id: u64) -> iced::advanced::widget::Id {
    enzui_core::identity::derive_id(GENRE_ROW_ID_PREFIX, &genre_id.to_string())
}

/// The widget id a playlist list row's focusable surface derives from the
/// immutable playlist id.
pub fn playlist_row_id(playlist_id: u64) -> iced::advanced::widget::Id {
    enzui_core::identity::derive_id(PLAYLIST_ROW_ID_PREFIX, &playlist_id.to_string())
}

/// The id namespace a track row's own focusable surface derives its widget id
/// under.
///
/// Shared by `views::track_row` and [`track_row_id`]; a mismatch is silent
/// because `RestoreFocus` leaves focus alone when the id is absent.
pub const TRACK_ROW_ID_PREFIX: &str = "music-track-row";

/// The id namespace an album card's pressable derives its widget id under.
pub const ALBUM_CARD_ID_PREFIX: &str = "music-album-card";

/// The id namespace a queue entry's jump-to pressable derives its widget id
/// under, keyed by occurrence (see [`menu_invoker_id`]).
pub const QUEUE_ENTRY_ID_PREFIX: &str = "music-queue-entry";

/// Width of the menu surface, in px.
///
/// Documented exception: no `SizingTokens` rung fits a menu sized by its own
/// text. 220 is a 200 px floor for `Add album to playlist…` plus the leading
/// icon gutter.
pub const MENU_WIDTH: f32 = 220.0;

/// A context menu that is currently up.
///
/// `at` is the raw click point; the framework host clamps against the live
/// viewport, so pre-clamping here would clamp twice.
///
/// [`MenuModel`] owns the rows and the highlight, and [`ContextMenuState`] owns
/// whether the host presents plus the invocation it anchors on. The host is
/// controlled: it reports [`ContextMenuEvent`]s and the app applies them. The
/// model is stored rather than rebuilt per frame because the host borrows it
/// for the element's lifetime and a temporary would reset the highlight.
///
/// [`ContextMenuEvent`]: enzui_widgets::context_menu_host::ContextMenuEvent
#[derive(Clone, Debug, PartialEq)]
pub struct OpenContextMenu {
    /// What was right-clicked.
    pub target: ContextTarget,
    /// Where the cursor was, in viewport coordinates.
    pub at: Point,
    /// The rows, plus the roving highlight arrow navigation moves.
    pub model: MenuModel,
    /// Whether the host is presenting, and the invocation it anchors on.
    pub state: ContextMenuState,
    /// The durable `(feed_url, guid)` a podcast-episode menu was raised on;
    /// `None` for other targets. Re-resolved against the live feed set, so a
    /// target made stale by a refresh is inert.
    pub podcast_key: Option<(String, String)>,
    /// The `(id, remote_uuid)` a radio-station menu was raised on; `None` for
    /// other targets. The u64 `id` can collide or be reassigned by a catalog
    /// reload, so every use requires the station at `id` to carry this uuid.
    pub radio_key: Option<(u64, String)>,
}

impl OpenContextMenu {
    /// Open a menu while retaining its independently focusable invoker.
    pub fn new_with_restore(
        target: ContextTarget,
        at: Point,
        rows: MenuModel,
        invoker: Option<iced::advanced::widget::Id>,
    ) -> Option<Self> {
        if !rows.has_focusable() {
            return None;
        }
        let mut state = ContextMenuState::closed();
        let invocation = InvocationKind::Pointer(CorePoint::new(at.x, at.y));
        match invoker {
            Some(id) => {
                state.open_with_restore(invocation, FocusRestoreTarget::widget(id));
            }
            None => {
                state.open(invocation);
            }
        }
        Some(Self {
            target,
            at,
            model: rows,
            state,
            podcast_key: None,
            radio_key: None,
        })
    }

    /// Record the durable podcast-episode identity this menu was raised on.
    /// A no-op for non-podcast menus.
    #[must_use]
    pub fn with_podcast_key(mut self, key: Option<(String, String)>) -> Self {
        self.podcast_key = key;
        self
    }

    /// Record the `(id, remote_uuid)` a radio-station menu was raised on.
    /// A no-op for non-radio menus.
    #[must_use]
    pub fn with_radio_key(mut self, key: Option<(u64, String)>) -> Self {
        self.radio_key = key;
        self
    }
}

/// The track a target resolves to, if any.
fn target_track<'a>(
    target: ContextTarget,
    library: &'a Library,
    playback: &PlaybackState,
) -> Option<&'a Track> {
    match target {
        ContextTarget::Track(id) => library.tracks.get(&id),
        // Resolve through the occurrence id so a reorder cannot retarget it
        // and a departed occurrence resolves to nothing.
        ContextTarget::QueueEntry(entry) => playback
            .track_of_entry(entry)
            .and_then(|id| library.tracks.get(&id)),
        _ => None,
    }
}

/// The item set for `target`, with its per-target guards applied.
///
/// Starts from [`crate::domain::context_menu_items`] and only removes rows that
/// would not apply:
///
/// * `Go to album` / `Go to artist` when the track has no album / artist;
/// * `Move to top` for the entry that is playing or already at the head;
/// * `Track info` for a queue slot that resolves to no real track.
pub fn items_for(
    target: ContextTarget,
    library: &Library,
    playback: &PlaybackState,
) -> Vec<ContextAction> {
    let track = target_track(target, library, playback);
    crate::domain::context_menu_items(target)
        .into_iter()
        .filter(|action| match (action, target) {
            (ContextAction::GoToAlbum, ContextTarget::Track(_)) => {
                track.map(|t| !t.album.is_empty()).unwrap_or(false)
            }
            (ContextAction::GoToArtist, ContextTarget::Track(_)) => {
                track.map(|t| !t.artist.is_empty()).unwrap_or(false)
            }
            (ContextAction::MoveToTopOfQueue, ContextTarget::QueueEntry(entry)) => {
                // Guard against the occurrence's live position; a departed
                // entry gets no row.
                playback
                    .index_of_entry(entry)
                    .is_some_and(|pos| pos > 0 && pos != playback.queue_index)
            }
            (ContextAction::TrackInfo, ContextTarget::QueueEntry(_)) => track.is_some(),
            // Rescan/Remove recovery rows appear only when the source needs
            // recovery (Missing/Undecodable/Unverified).
            (ContextAction::Rescan | ContextAction::Remove, _) => track
                .map(|t| library.availability_of_path(&t.path).offers_recovery())
                .unwrap_or(false),
            _ => true,
        })
        .collect()
}

/// The visible label for one row.
///
/// Most rows read [`ContextAction::label`] verbatim. The exceptions name what
/// they act on:
///
/// * a track's or album's Play row is `Play “{title}”`, truncated at 32 chars;
/// * a queue entry's Play row is `Play “{title}” now`, truncated at 28;
/// * the album menu says `Add album to queue` / `Add album to playlist…`.
pub fn label_for(
    action: ContextAction,
    target: ContextTarget,
    library: &Library,
    playback: &PlaybackState,
) -> String {
    let title = || -> String {
        match target {
            ContextTarget::Album(id) => library
                .albums
                .iter()
                .find(|a| a.id == id)
                .map(|a| a.title.clone())
                .unwrap_or_else(|| format!("Album {id}")),
            _ => target_track(target, library, playback)
                .map(|t| t.display_title().to_string())
                .unwrap_or_else(|| "Unknown".to_string()),
        }
    };

    match (action, target) {
        (ContextAction::Play, ContextTarget::QueueEntry(_)) => {
            format!("Play \u{201C}{}\u{201D} now", truncate_label(&title(), 28))
        }
        (ContextAction::Play, ContextTarget::Track(_) | ContextTarget::Album(_)) => {
            format!("Play \u{201C}{}\u{201D}", truncate_label(&title(), 32))
        }
        (ContextAction::AddToQueue, ContextTarget::Album(_)) => "Add album to queue".to_string(),
        (ContextAction::AddToPlaylist, ContextTarget::Album(_)) => {
            "Add album to playlist\u{2026}".to_string()
        }
        _ => action.label().to_string(),
    }
}

/// The [`MenuModel`] for `target`: the rows the framework host renders.
///
/// Rows carry their label, icon, group-derived separators and the destructive
/// tone. They do not carry a `Message`: the host reports `Activated { id, .. }`
/// and the app resolves it through [`row_for_id`] against live state, so a row
/// whose condition lapsed while the menu was open cannot fire.
pub fn menu_model(target: ContextTarget, library: &Library, playback: &PlaybackState) -> MenuModel {
    let mut entries: Vec<MenuEntryModel> = Vec::new();
    let mut previous_group: Option<u8> = None;

    for action in items_for(target, library, playback) {
        // A rule wherever the group changes (see module docs).
        if let Some(prev) = previous_group
            && prev != action.group()
        {
            entries.push(MenuEntryModel::separator(format!("rule-{}", action.key())));
        }
        previous_group = Some(action.group());

        entries.push(
            MenuEntryModel::command(action.key(), label_for(action, target, library, playback))
                .icon(action.icon())
                // Remove from queue, Remove from library (deletes the durable
                // track, never its file) and playlist Delete are destructive.
                .destructive(matches!(
                    action,
                    ContextAction::RemoveFromQueue
                        | ContextAction::Remove
                        | ContextAction::DeletePlaylist
                )),
        );
    }

    MenuModel::new(entries)
}

/// The [`ContextAction`] an activated entry id names, resolved against the
/// target's current state.
///
/// `None` for an id that no longer names an offered row, and for separators.
/// Resolution goes through [`items_for`], so a row this target omits (such as
/// `Move to top of queue` for the current entry) cannot be fired by a stale id.
pub fn row_for_id(
    target: ContextTarget,
    library: &Library,
    playback: &PlaybackState,
    id: &EntryId,
) -> Option<ContextAction> {
    items_for(target, library, playback)
        .into_iter()
        .find(|a| a.key() == id.as_str())
}

/// The accessible name the menu surface publishes.
///
/// Names the subject using the same display strings as the rows, so assistive
/// technology can tell which track, album or queue entry the menu acts on.
pub fn menu_label(
    target: ContextTarget,
    library: &Library,
    playback: &PlaybackState,
) -> Option<String> {
    match target {
        ContextTarget::Track(_) | ContextTarget::QueueEntry(_) => {
            target_track(target, library, playback).map(|t| t.display_title().to_string())
        }
        ContextTarget::Album(id) => library
            .albums
            .iter()
            .find(|a| a.id == id)
            .map(|a| a.title.clone()),
        // Name the genre/playlist with the same display string the row draws.
        ContextTarget::Genre(id) => library
            .genres
            .iter()
            .find(|g| g.id == id)
            .map(|g| g.name.clone()),
        ContextTarget::PlaylistRow(id) => library
            .playlists
            .iter()
            .find(|p| p.id == id)
            .map(|p| p.name.clone()),
        // Radio/podcast names need app state and come from the app's
        // `context_menu_label`; listed explicitly so a new capture site must
        // decide its accessible name here.
        ContextTarget::Artist(_)
        | ContextTarget::RadioStation(_)
        | ContextTarget::PodcastEpisode(_) => None,
    }
}

/// The widget focus returns to when the menu closes.
///
/// Minted from the target's stable model key, never a screen position:
/// library ids for tracks and albums, and the immutable `QueueEntryId` for a
/// queue occurrence, so focus returns to the clicked row after a re-sort or
/// reorder and duplicates of one track stay distinct. `None` once the subject
/// is gone, such as a track removed by the menu's own verb.
pub fn menu_invoker_id(
    target: ContextTarget,
    library: &Library,
    playback: &PlaybackState,
) -> Option<iced::advanced::widget::Id> {
    match target {
        ContextTarget::Track(id) => library.tracks.contains_key(&id).then(|| track_row_id(id)),
        ContextTarget::Album(id) => library
            .albums
            .iter()
            .any(|a| a.id == id)
            .then(|| album_card_id(id)),
        // Present only while that exact occurrence is still queued.
        ContextTarget::QueueEntry(entry) => playback
            .contains_entry(entry)
            .then(|| queue_entry_id(entry)),
        // The list row is itself the focusable surface; its id comes from the
        // same helper as the render site, keyed by the immutable library id.
        // `None` once the subject is deleted.
        ContextTarget::Genre(id) => library
            .genres
            .iter()
            .any(|g| g.id == id)
            .then(|| genre_row_id(id)),
        ContextTarget::PlaylistRow(id) => library
            .playlists
            .iter()
            .any(|p| p.id == id)
            .then(|| playlist_row_id(id)),
        // Radio/podcast invokers need app state and come from the app's
        // `context_menu_invoker`.
        ContextTarget::Artist(_)
        | ContextTarget::RadioStation(_)
        | ContextTarget::PodcastEpisode(_) => None,
    }
}

/// The widget id of a track row's own focusable surface.
///
/// Must match `views::track_row`'s `.id(..)`;
/// `the_invoker_id_reaches_the_track_row` checks this on the rendered tree.
pub fn track_row_id(track_id: u64) -> iced::advanced::widget::Id {
    enzui_core::identity::derive_id(TRACK_ROW_ID_PREFIX, &track_id.to_string())
}

/// The widget id of an album card's pressable.
pub fn album_card_id(album_id: u64) -> iced::advanced::widget::Id {
    enzui_core::identity::derive_id(ALBUM_CARD_ID_PREFIX, &album_id.to_string())
}

/// The widget id of a queue entry's jump-to pressable, keyed by its immutable
/// [`crate::domain::QueueEntryId`] so it survives reorder and duplicates.
pub fn queue_entry_id(entry: crate::domain::QueueEntryId) -> iced::advanced::widget::Id {
    enzui_core::identity::derive_id(QUEUE_ENTRY_ID_PREFIX, &entry.get().to_string())
}

/// The widget id a radio station row's focusable surface derives from the
/// station's durable `remote_uuid`. The u64 `id` can collide and the row label
/// changes when tuned, so either would strand focus restoration.
pub fn radio_row_id(remote_uuid: &str) -> iced::advanced::widget::Id {
    enzui_core::identity::derive_id(RADIO_ROW_ID_PREFIX, remote_uuid)
}

/// The widget id a podcast episode row's focusable surface derives from the
/// durable `(feed_url, guid)`, joined by a NUL that neither can contain.
pub fn podcast_episode_row_id(feed_url: &str, guid: &str) -> iced::advanced::widget::Id {
    enzui_core::identity::derive_id(
        PODCAST_EPISODE_ROW_ID_PREFIX,
        &format!("{feed_url}\u{0}{guid}"),
    )
}

/// The id a capture site advertises its routable `ShowContextMenu` node under.
///
/// Derived from the invoker id but never equal to it: two semantics nodes with
/// one id abort accessibility tree construction. Uses the framework's
/// `derive_context_menu_id`, the same rule as
/// `EnzContextMenuHost::advertised_id`.
pub fn context_target_id(invoker: &iced::advanced::widget::Id) -> iced::advanced::widget::Id {
    enzui_widgets::context_menu_host::derive_context_menu_id(invoker)
}

#[cfg(test)]
mod tests {}
