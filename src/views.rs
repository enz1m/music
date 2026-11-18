//! View construction for the music app.
//!
//! Every view is a pure function from model state → Element. The shell
//! assembles them into the wide or compact layout.

use iced::widget::{Space, container};
use iced::{Alignment, Element, Length, Padding};

use crate::Message;
use crate::domain::*;
use crate::layout;
use crate::layout::NowPlayingLayout;
use crate::theme;
use enzui_core::action::Action;
use enzui_core::menu_model::{MenuModel, MenuState};
use enzui_core::semantics::HeadingLevel;
use enzui_core::virtual_grid::{TileGrid, TileProvider};
use enzui_core::virtual_sequence::{ItemProvider, KeyedProvider, SequenceKey};
use enzui_theme::{EnzTheme, StrokeToken};
use enzui_widgets::avatar::enz_avatar;
use enzui_widgets::badge::{BadgeTone, enz_badge};
use enzui_widgets::button::{ButtonKind, ButtonSize, enz_button};
use enzui_widgets::checkbox::enz_checkbox;
use enzui_widgets::context_target::enz_context_target;
use enzui_widgets::divider::enz_divider;
use enzui_widgets::enz_text::{TextAlign, TextSize, enz_text};
use enzui_widgets::icon::enz_icon;
use enzui_widgets::image_widget::{EnzImageFit, enz_image_handle};
use enzui_widgets::list_row_surface::enz_list_row_surface;
use enzui_widgets::menu_button::enz_menu_button;
use enzui_widgets::modal_surface::enz_modal_surface;
use enzui_widgets::preferences_row::enz_preferences_row;
use enzui_widgets::pressable::{PressableStyle, enz_pressable};
use enzui_widgets::radio_group::enz_radio_group;
use enzui_widgets::scroll_view::enz_scroll_view;
use enzui_widgets::search_field::enz_search_field;
use enzui_widgets::settings_dialog::{
    DialogSize, EnzSettingsPresentation, SettingsPage, enz_settings_dialog,
    enz_settings_presentation_for,
};
use enzui_widgets::shortcuts_dialog::{ShortcutsDialogEvent, enz_shortcuts_dialog};
use enzui_widgets::slider::SliderOrientation;
use enzui_widgets::slider::enz_slider;
use enzui_widgets::status_page::{StatusPageVariant, enz_status_page};
use enzui_widgets::text_field::{TextFieldSize, enz_text_field};
use enzui_widgets::title_bar::enz_title_bar;
use enzui_widgets::virtual_grid::enz_virtual_grid;
use enzui_widgets::virtual_list::enz_virtual_list;

use library::{sort_control, track_list_section};
#[cfg(test)]
use player::transport_icon_sized;

/// Fixed titlebar height, the shell's shared titlebar-band axis.
///
/// Reads the shell's
/// [`DEFAULT_SHELL_TITLEBAR_HEIGHT`](enzui_widgets::app_shell::DEFAULT_SHELL_TITLEBAR_HEIGHT)
/// (40 px), not `toolbar_height` (48), so the app's CSD band and the shell band
/// that `ShellTokens::titlebar_height` receives are one value and every app's
/// window controls share a midline.
pub const TITLEBAR_HEIGHT: f32 = enzui_widgets::app_shell::DEFAULT_SHELL_TITLEBAR_HEIGHT;

// ---------------------------------------------------------------------------
// Content width — the centring contract
// ---------------------------------------------------------------------------

/// The maximum content width inside the now-playing panel; every panel row is
/// centred over this axis.
///
/// Documented exception: 380 is narrower than every readable-width container
/// tier because this is a portrait album/transport column, not a reading width.
pub const NOW_PLAYING_CONTENT_W: f32 = 480.0;

/// Horizontal padding on each side of the now-playing panel content.
///
/// Documented exception: 24 is `SpacingScale::xl()`, kept as a raw f32 because
/// it feeds `NowPlayingLayout::compute` alongside its sibling literals.
pub const NOW_PLAYING_H_PAD: f32 = 24.0;

/// The album grid tile's art slot height, in px.
///
/// TOKEN-AUDIT-EXCEPTION(dim): album-tile art slot; no `SizingTokens` rung is
/// an artwork height. A fixed slot shared by image and placeholder keeps tiles
/// from changing height when a file carries no picture.
/// Unused while the tile sizes itself; only [`ALBUM_TILE_ART_RADIUS`] is read.
#[allow(dead_code)]
pub const ALBUM_TILE_ART_H: f32 = 140.0;

/// The album tile art slot's corner radius, in px.
///
/// TOKEN-AUDIT-EXCEPTION(dim): artwork corner rounding; no `SizingTokens`
/// rung is an image radius. The image and placeholder paths must round
/// identically so a tile keeps its shape with or without a picture.
pub const ALBUM_TILE_ART_RADIUS: f32 = 8.0;

/// The content inset every page uses under the titlebar: the page title
/// lives in the titlebar, so the content opens close under it.
pub(crate) fn page_padding(enz_theme: &EnzTheme) -> Padding {
    // The page starts right under the titlebar band, which already clears it.
    Padding::new(enz_theme.spacing.lg()).top(0.0)
}

/// Convenience: horizontal fill space.
fn h_space() -> Space {
    Space::new().width(Length::Fill)
}

/// The mini-player's album-art thumb edge, in px.
///
/// Documented exception: no `SizingTokens` rung is 44 px. `icon_xl` (32) is
/// smaller than the mini-player's 36px touch target and `icon_xxl` (48) does
/// not fit the 56px band's 8px+8px padding.
///
/// Takes the live theme so its guard can assert `icon_xl < f() < icon_xxl`
/// against a retuned theme.
pub fn mini_player_thumb_px(enz_theme: &EnzTheme) -> f32 {
    let _ = enz_theme;
    44.0
}

/// The shared badge tone for a track's availability.
///
/// Missing is an error, Undecodable a warning, and every other state carries no
/// badge, so every track surface paints the same colour for the same state.
pub(crate) fn availability_badge_tone(state: crate::domain::TrackAvailability) -> BadgeTone {
    use crate::domain::TrackAvailability as A;
    match state {
        A::Missing => BadgeTone::Error,
        A::Undecodable => BadgeTone::Warning,
        A::Unverified => BadgeTone::Warning,
        // `Checking` is neutral, not an alarm; `Present` renders no badge.
        A::Present | A::Unknown => BadgeTone::Default,
    }
}

/// A centred empty-state hero built on the inline `enz_status_page`.
///
/// Publishes an icon, title and description as a titled AccessKit status region.
fn empty_state<'a>(icon: &'a str, title: &'a str, description: &'a str) -> Element<'a, Message> {
    let mut sp = enz_status_page(title)
        .icon(icon)
        .variant(StatusPageVariant::Inline);
    if !description.is_empty() {
        sp = sp.description(description);
    }
    sp.into_element()
}

/// Owned-string form of [`empty_state`] for runtime-formatted titles such as
/// `No matches for "query"`.
fn empty_state_owned<'a>(
    icon: &'a str,
    title: String,
    description: String,
) -> Element<'a, Message> {
    let mut sp = enz_status_page(title)
        .icon(icon)
        .variant(StatusPageVariant::Inline);
    if !description.is_empty() {
        sp = sp.description(description);
    }
    sp.into_element()
}

/// The Albums grid's count line, `"N albums"`, where `N` counts the albums
/// visible after filtering, not the library total.
pub fn format_albums_header(visible_count: usize) -> String {
    format_count(visible_count, "album", "albums")
}

/// Same, for the Artists list.
pub fn format_artists_header(visible_count: usize) -> String {
    format_count(visible_count, "artist", "artists")
}

/// The composed "no matches" string, `No matches for "X"`.
pub fn format_no_matches_for(value: &str) -> String {
    format!("No matches for {value:?}")
}

/// The Search view's no-results specimen, `"No results for 'X'"`.
///
/// Uses single quotes, unlike the double-quoted `format_no_matches_for`.
pub fn format_no_results_for(query: &str) -> String {
    format!("No results for '{query}'")
}

/// The queue's inline count sentence: `"1 track in queue"`, `"N tracks in
/// queue"`, or the empty-queue prompt.
///
/// Distinct from the header count (`"N tracks · MM:SS"`).
pub fn format_queue_count_sentence(n: usize) -> String {
    match n {
        0 => "The play queue is empty. Pick a track to start.".to_string(),
        1 => "1 track in queue".to_string(),
        n => format!("{n} tracks in queue"),
    }
}

/// Required Search-field placeholder; the order (tracks first) and the absent
/// ellipsis are part of the specification, and `search_input` reads only this.
pub const SEARCH_PLACEHOLDER: &str = "Search tracks, albums, artists";

/// The shuffle transport button's label: `"Shuffle"` when off, `"Shuffle on"`
/// when on.
///
/// `enz_media_controls` owns the rendered button; this delegates to the
/// framework helper so the app's label assertions bind to the published name.
#[allow(
    dead_code,
    reason = "render site moved to enz_media_controls; retained as the \
              anchor for the shuffle label assertions in cluster_c_tests"
)]
pub fn shuffle_button_label(shuffle_on: bool) -> String {
    enzui_widgets::media_controls::shuffle_label(shuffle_on)
}

/// The transport caption shared by the player panel and current stream row.
///
/// A selected track, station or episode is paused until `playing` is confirmed;
/// no selection returns `None` so the empty panel has no transport caption.
pub fn transport_status_label(playback: &PlaybackState) -> Option<&'static str> {
    if playback.current_track_id.is_none() && playback.stream_selection().is_none() {
        None
    } else if playback.playing {
        Some("Now playing")
    } else {
        Some("Paused")
    }
}

/// The value carried by a filter, for display in `No matches for ".."`. `None`
/// when the filter carries no interesting value (an unresolved id, or
/// `LibraryFilter::None`).
pub fn filter_display_value(filter: LibraryFilter, library: &Library) -> Option<String> {
    match filter {
        LibraryFilter::None => None,
        LibraryFilter::Rated => Some("Rated tracks".into()),
        LibraryFilter::Artist(id) => library
            .artists
            .iter()
            .find(|a| a.id == id)
            .map(|a| a.name.clone()),
        LibraryFilter::Album(id) => library
            .albums
            .iter()
            .find(|a| a.id == id)
            .map(|a| a.title.clone()),
        LibraryFilter::Genre(id) => library
            .genres
            .iter()
            .find(|g| g.id == id)
            .map(|g| g.name.clone()),
        LibraryFilter::Tag(id) => library
            .tags
            .iter()
            .find(|t| t.id == id)
            .map(|t| t.name.clone()),
        LibraryFilter::Playlist(id) => library
            .playlists
            .iter()
            .find(|p| p.id == id)
            .map(|p| p.name.clone()),
    }
}

mod library;
pub(crate) use library::*;

mod discovery;
pub(crate) use discovery::*;

mod player;
pub(crate) use player::*;

mod chrome;
pub(crate) use chrome::*;

mod dialogs;
pub(crate) use dialogs::*;

// ---------------------------------------------------------------------------
// Settings on `enz_settings_dialog`: a labelled `Role::Dialog`, the
// shared bottom sheet when narrow, the Compact 460 px rung when wide, and the
// circular close as the single dismiss.
// ---------------------------------------------------------------------------

mod track_dialogs;
pub(crate) use track_dialogs::*;

// ---------------------------------------------------------------------------
// Album grid contract: one `enz_responsive_grid_keyed` cell per visible album,
// keyed by the album's stable id, so keyed reflow keeps widget state on the
// right card.
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Now-playing title / artist / album use `single_line_elided()`, so a long value
// in the fixed-width panel publishes a TextElision finding and one that fits
// publishes none.
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Cluster A tests: library browsing, empty states, ordering.
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Queue-removal focus: deleting the focused queue row moves focus to its
// successor, read back from a laid-out `queue_view`.
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Pure formatter tests: composed output for a fixed input.
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Tests for sort labels, library chrome, podcasts view and the lyrics dialog.
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// The picker (420 px) and sleep-timer (400 px) dialog widths are documented
// literals between `dialog_width_sm` and `dialog_width_md`; this guard checks
// the literals are present and still lie strictly between the live rungs.
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// The two strings the truncation disclosure is made of. Rendering is covered by
// `render_tests::track_list_truncation_tests`.
// ---------------------------------------------------------------------------

// Floating chrome: the titlebar and the player bar float over the page, so a
// page scrolls under both and its first and last rows rest clear of them.
thread_local! {
    // (top, bottom) clearance for the page being built, in px.
    static PAGE_INSETS: std::cell::Cell<(f32, f32)> = const { std::cell::Cell::new((0.0, 0.0)) };
    // Whether the page built a [`page_scroll`], which clears the chrome itself.
    static PAGE_SCROLLED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Set the floating chrome's clearance for the page about to be built.
pub(crate) fn begin_page(top: f32, bottom: f32) {
    PAGE_INSETS.with(|insets| insets.set((top, bottom)));
    PAGE_SCROLLED.with(|scrolled| scrolled.set(false));
}

/// Whether the page built since [`begin_page`] scrolls under the chrome on
/// its own; a page that does not is padded clear of it instead.
pub(crate) fn page_scrolls_under_chrome() -> bool {
    PAGE_SCROLLED.with(std::cell::Cell::get)
}

/// [`page_scroll`] with an id (for scroll operations) that reports its
/// offset.
pub(crate) fn page_scroll_tracked<'a>(
    content: impl Into<Element<'a, Message>>,
    id: iced::widget::Id,
    on_scroll: impl Fn(f32) -> Message + 'a,
) -> Element<'a, Message> {
    PAGE_SCROLLED.with(|scrolled| scrolled.set(true));
    let (top, bottom) = PAGE_INSETS.with(std::cell::Cell::get);
    enz_scroll_view(content)
        .insets(0.0, 0.0, top, bottom)
        .scrollbar_insets(Padding::new(0.0).top(top).bottom(bottom))
        .height(Length::Fill)
        .id(id)
        .on_scroll(move |viewport| on_scroll(viewport.absolute_offset().y))
        .into()
}

/// A page's scroll view: its rows pass under the floating titlebar and
/// player bar, and rest clear of both at the ends.
pub(crate) fn page_scroll<'a>(content: impl Into<Element<'a, Message>>) -> Element<'a, Message> {
    PAGE_SCROLLED.with(|scrolled| scrolled.set(true));
    let (top, bottom) = PAGE_INSETS.with(std::cell::Cell::get);
    enz_scroll_view(content)
        .insets(0.0, 0.0, top, bottom)
        .scrollbar_insets(Padding::new(0.0).top(top).bottom(bottom))
        .height(Length::Fill)
        .into()
}

// Split layout: on a window wide enough, the browse pane and a Now Playing
// column side by side, in place of the bottom player bar and its sheet.

/// The narrowest window that splits: a comfortable browse pane beside the
/// narrowest Now Playing column.
pub const SPLIT_MIN_WIDTH: f32 = 640.0;
/// The Now Playing column's width range, and its share of the window.
const SPLIT_NOW_PLAYING_MIN: f32 = 300.0;
const SPLIT_NOW_PLAYING_MAX: f32 = 720.0;
/// Half the window: track titles are short, so the browse pane rarely needs
/// more, and Now Playing grows with the window instead of staying a strip.
const SPLIT_NOW_PLAYING_SHARE: f32 = 0.5;
/// The browse pane never narrows below this.
const SPLIT_BROWSE_MIN: f32 = 340.0;

/// The browse pane's and the Now Playing column's widths for a window
/// `width` wide, or `None` below [`SPLIT_MIN_WIDTH`].
#[must_use]
pub fn split_widths(width: f32) -> Option<(f32, f32)> {
    if width < SPLIT_MIN_WIDTH {
        return None;
    }
    let now_playing = (width * SPLIT_NOW_PLAYING_SHARE)
        .clamp(SPLIT_NOW_PLAYING_MIN, SPLIT_NOW_PLAYING_MAX)
        .min(width - SPLIT_BROWSE_MIN);
    Some(((width - now_playing).floor(), now_playing.ceil()))
}
