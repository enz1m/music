//! Track rows, album cards, and sort controls.

use super::*;
use iced::widget::{column, row};

// ---------------------------------------------------------------------------
// Track row: one component, one internal grid
// ---------------------------------------------------------------------------

/// Vertical inset inside a track row, so the hover/selection fill does not
/// touch the two text lines.
pub(crate) const TRACK_ROW_PAD_Y: f32 = 6.0;

/// The leading artwork of a track-like row: the cover thumbnail, or a
/// neutral placeholder of the same size, so every row in a list starts on the
/// same edge with the same shape.
pub(crate) fn track_artwork<'a>(
    cover: Option<&crate::art::CoverArt>,
    alt: String,
    enz_theme: &'a EnzTheme,
) -> Element<'a, Message> {
    use enzui_widgets::surface::{SurfaceTone, enz_surface};
    let edge = layout::TrackRowLayout::INDEX_W;
    match cover {
        Some(art) => enz_image_handle(art.handle().clone())
            .fit(EnzImageFit::Cover)
            .radius(enz_theme.radius.sm)
            .alt_text(alt)
            .width(Length::Fixed(edge))
            .height(Length::Fixed(edge))
            .into_element(),
        None => enz_surface(
            container(
                enz_icon("music")
                    .size(enz_theme.sizing.icon_sm)
                    .tint(theme::secondary_text_color(enz_theme))
                    .into_element(),
            )
            .center_x(Length::Fixed(edge))
            .center_y(Length::Fixed(edge)),
        )
        .tone(SurfaceTone::SurfaceAlt)
        .radius(enz_theme.radius.sm)
        .width(Length::Fixed(edge))
        .height(Length::Fixed(edge))
        .into_element(),
    }
}

/// A single track row with fixed column widths.
/// Columns: [lead 32px] [title+artist Fill] [duration 52px] [actions 40px reserved]
///
/// The lead is the track's artwork (or a same-size placeholder) everywhere
/// except inside an album, where the track number is the meaningful key.
/// The action column is reserved even when empty, so the duration column's x
/// never shifts.
pub fn track_row<'a>(
    scope: &str,
    index: usize,
    track: &'a Track,
    is_playing: bool,
    availability: crate::domain::TrackAvailability,
    enz_theme: &'a EnzTheme,
) -> Element<'a, Message> {
    let tc = theme::text_color(enz_theme);
    let sc = theme::secondary_text_color(enz_theme);
    let main_list = scope == "Tracks";
    let numbered = scope == "Album";
    let invoker = if main_list {
        crate::context_menu::track_row_id(track.id)
    } else {
        enzui_core::identity::derive_id(
            crate::context_menu::TRACK_ROW_ID_PREFIX,
            &format!("{scope}-{}", track.id),
        )
    };

    let index_col: Element<'a, Message> = if numbered {
        container(
            enz_text(format!("{}", index + 1))
                .size(TextSize::Footnote)
                .color(sc),
        )
        .align_right(layout::TrackRowLayout::INDEX_W)
        .center_y(Length::Shrink)
        .into()
    } else {
        track_artwork(
            track.cover.as_ref(),
            format!("{} album art", track.display_album()),
            enz_theme,
        )
    };
    let title_color = tc;

    let mut info_col = column![
        enz_text(track.display_title())
            .size(TextSize::Body)
            .color(title_color)
            .full_width()
            .single_line_elided(),
        enz_text(track.display_artist())
            .size(TextSize::Footnote)
            .color(sc)
            .full_width()
            .single_line_elided(),
    ]
    .spacing(enz_theme.spacing.space1)
    .width(Length::Fill);

    // Availability rides inside the Fill info column as a third line, so the
    // fixed column edges never move. Only Missing/Undecodable rows show a
    // badge and its path-free disclosure.
    if let Some(label) = availability.badge_label() {
        let mut availability_line =
            row![enz_badge(label).tone(super::availability_badge_tone(availability)),]
                .spacing(enz_theme.spacing.space1)
                .align_y(Alignment::Center);
        if let Some(text) = availability.disclosure() {
            availability_line = availability_line.push(
                enz_text(text)
                    .size(TextSize::Footnote)
                    .color(sc)
                    .full_width()
                    .single_line_elided(),
            );
        }
        info_col = info_col.push(availability_line.width(Length::Fill));
    }

    let duration_col = container(
        enz_text(track.duration_display())
            .size(TextSize::Footnote)
            .color(sc),
    )
    .align_right(layout::TrackRowLayout::DURATION_W)
    .center_y(Length::Shrink);

    // Add-to-playlist affordance: fixed width keeps column alignment, and
    // `PressableStyle::Row` reads as an inline icon rather than a boxed button.
    let add_icon = container(enz_icon("list-plus").into_element())
        .center_x(Length::Shrink)
        .center_y(Length::Shrink);
    let action_col = container(
        enz_pressable(
            add_icon.into(),
            Message::OpenPlaylistPicker(PickerTarget::Track(track.id)),
        )
        .style(PressableStyle::Row)
        // Context-menu dismissal restores the playlist action's focus.
        .id(invoker.clone())
        .aria_label(format!("Add {} to playlist", track.display_title())),
    )
    .width(layout::TrackRowLayout::ACTION_W)
    .center_x(layout::TrackRowLayout::ACTION_W);

    let content = container(
        row![index_col, info_col, duration_col, action_col]
            .spacing(layout::TrackRowLayout::SPACING)
            .align_y(Alignment::Center),
    )
    .padding(
        Padding::new(0.0)
            .top(TRACK_ROW_PAD_Y)
            .bottom(TRACK_ROW_PAD_Y),
    );

    let body: Element<'a, Message> = enz_list_row_surface(content, Message::PlayTrack(track.id))
        .id(enzui_core::identity::derive_id(
            "music-play-track",
            &format!("{scope}-{}", track.id),
        ))
        .label(format!("Play {}", track.display_title()))
        .selected(is_playing)
        .content_height(true)
        .content_padding(layout::TrackRowLayout::PAD_L)
        .outer_gutter(0.0)
        .into_element();

    // Right-click opens the track context menu at the cursor. The capture
    // delegates to its child first and only claims a right-press, so the row's
    // own left-press affordances still work. `.id(..)` and `.label(..)` make
    // the menu reachable without a pointer: the target advertises
    // `ShowContextMenu` only when it has an id to route to.
    let track_id = track.id;
    let track_title = track.display_title().to_string();
    let context_id = crate::context_menu::context_target_id(&invoker);
    let body = enzui_widgets::file_drag::enz_file_drag_source(body, [track.path.clone()])
        .allows_captured_press(true);
    enz_context_target(body, move |at| {
        if main_list {
            Message::OpenContextMenu(ContextTarget::Track(track_id), at)
        } else {
            Message::OpenContextMenuFrom(ContextTarget::Track(track_id), at, invoker.clone())
        }
    })
    .id(context_id)
    .label(format!("{track_title} actions"))
    .height(Length::Shrink)
    .into()
}

// ---------------------------------------------------------------------------
// Album card
// ---------------------------------------------------------------------------

pub fn album_card<'a>(album: &Album, enz_theme: &'a EnzTheme) -> Element<'a, Message> {
    use enzui_widgets::aspect_ratio::enz_aspect_ratio;
    let art_bg = theme::overlay_hover_color(enz_theme);
    // `enz_image_handle` takes the RGBA thumbnail the scanner decoded
    // off-thread (`art::CoverCache`); a path source would decode a multi-MB
    // JPEG in the renderer once per visible tile. `alt_text` names the image.
    //
    // The slot is square via `enz_aspect_ratio`, following the grid's column
    // width at every rung; a fixed-height slot with `ContentFit::Cover` crops
    // square art.
    let art_slot: Element<'a, Message> = match &album.cover {
        Some(art) => enz_aspect_ratio(
            enz_image_handle(art.handle().clone())
                .fit(EnzImageFit::Cover)
                .radius(ALBUM_TILE_ART_RADIUS)
                .alt_text(format!("{} album art", album.title))
                .width(Length::Fill)
                .height(Length::Fill)
                .placeholder_background(art_bg)
                .into_element(),
            1.0,
        )
        .into(),
        None => {
            use enzui_widgets::surface::{RadiusSize, SurfaceTone, enz_surface};
            enz_aspect_ratio(
                enz_surface(
                    container(
                        enz_icon("music")
                            .size(enz_theme.sizing.icon_xl)
                            .tint(theme::secondary_text_color(enz_theme))
                            .into_element(),
                    )
                    .center_x(Length::Fill)
                    .center_y(Length::Fill),
                )
                .tone(SurfaceTone::SurfaceAlt)
                .radius_size(RadiusSize::Xl)
                .width(Length::Fill)
                .height(Length::Fill)
                .into_element(),
                1.0,
            )
            .into()
        }
    };

    let album_id = album.id;
    let title_text = album.title.clone();
    let artist_text = album.display_artist().to_owned();

    let tc = theme::text_color(enz_theme);
    let sc2 = theme::secondary_text_color(enz_theme);

    // `enz_text` already elides single-line by default. `.full_width()` gives
    // the run a bound to elide against (a `Shrink` run never engages the
    // ellipsis), and `single_line_elided()` opts it into the framework elision
    // audit so a truncated title is reported as truncated to AT.
    let card = column![
        art_slot,
        enz_text(title_text)
            .size(TextSize::Body)
            .color(tc)
            .single_line_elided()
            .full_width(),
        enz_text(artist_text)
            .size(TextSize::Caption)
            .color(sc2)
            .single_line_elided()
            .full_width(),
    ]
    .spacing(enz_theme.spacing.xs())
    .width(Length::Fill);

    // `enz_pressable` gives the card keyboard activation, hover/pressed
    // treatment and a named clickable surface. The `Sm` inset applies on all
    // four sides so the art does not bleed into the hover fill's corners.
    let body = enz_pressable(card.into(), Message::SelectAlbum(album_id))
        .style(PressableStyle::Plain)
        .padding(enz_theme.spacing.sm())
        .aria_label(album.title.clone())
        // The card's focusable surface, and so the restore target when
        // its context menu closes.
        .id(crate::context_menu::album_card_id(album_id))
        .into_element();

    // Right-click opens the album context menu, anchored at the cursor.
    let album_title = album.title.clone();
    enz_context_target(body, move |at| {
        Message::OpenContextMenu(ContextTarget::Album(album_id), at)
    })
    // See `track_row`: the id makes the node routable, the label names the
    // album.
    .id(crate::context_menu::context_target_id(
        &crate::context_menu::album_card_id(album_id),
    ))
    .label(format!("{album_title} actions"))
    .height(Length::Shrink)
    .into()
}

// ---------------------------------------------------------------------------
// Home view
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Filter chip
// ---------------------------------------------------------------------------

pub(crate) fn filter_chip<'a>(
    filter: LibraryFilter,
    library: &Library,
) -> Option<Element<'a, Message>> {
    if matches!(filter, LibraryFilter::None) {
        return None;
    }
    let label = match filter {
        LibraryFilter::Artist(id) => library
            .artists
            .iter()
            .find(|a| a.id == id)
            .map(|a| format!("Artist: {}", a.name)),
        LibraryFilter::Album(id) => library
            .albums
            .iter()
            .find(|a| a.id == id)
            .map(|a| format!("Album: {}", a.title)),
        LibraryFilter::Genre(id) => library
            .genres
            .iter()
            .find(|g| g.id == id)
            .map(|g| format!("Genre: {}", g.name)),
        LibraryFilter::Tag(id) => library
            .tags
            .iter()
            .find(|t| t.id == id)
            .map(|t| format!("Tag: {}", t.name)),
        LibraryFilter::Playlist(id) => library
            .playlists
            .iter()
            .find(|p| p.id == id)
            .map(|p| format!("Playlist: {}", p.name)),
        LibraryFilter::Rated => Some("Rated tracks".to_string()),
        LibraryFilter::None => None,
    }?;

    let description = label.clone();
    Some(
        enzui_widgets::button::enz_filter_chip(label, true, Message::ClearFilter)
            .aria_description(description)
            .icon("x")
            .icon_position(enzui_widgets::button::ButtonIconPosition::Trailing)
            .aria_label("Clear filter")
            .into_element(),
    )
}

/// The stable widget id the titlebar Sort menu trigger publishes.
pub const SORT_MENU_TRIGGER_ID: &str = "music-sort-menu";

/// The accessible name of the Sort menu trigger, e.g. `"Sort: Album"`.
///
/// Stating the current value makes it available to screen readers.
pub fn sort_trigger_label(current: SortMode) -> String {
    format!("Sort: {}", current.label())
}

/// Whether the Sort dropdown affects the content pane of `view`.
///
/// Derived from whether the view sorts a track list by the user's order;
/// `sort_control_is_absent_where_sorting_changes_nothing` and
/// `sort_applies_to_agrees_with_what_the_frames_actually_do` check this
/// against rendered frames.
pub fn sort_applies_to(view: View) -> bool {
    matches!(view, View::Home | View::Albums | View::Artists)
}

/// Reorder an album list by the user's chosen [`SortMode`], in place.
///
/// Modes without an album meaning fall back to a stable title order:
///
/// * `Album` / `Title`: album title, A–Z;
/// * `Artist`: album artist, then title;
/// * `Recent`: newest release year first, then title;
/// * `Shuffle`: the deterministic per-id order [`Library::ordered_tracks`]
///   uses, reproducible within a session.
pub fn sort_albums(albums: &mut [&Album], sort: SortMode) {
    let title_key = |a: &Album| a.title.to_lowercase();
    match sort {
        SortMode::Artist => albums.sort_by(|a, b| {
            a.artist
                .to_lowercase()
                .cmp(&b.artist.to_lowercase())
                .then_with(|| title_key(a).cmp(&title_key(b)))
                .then_with(|| a.id.cmp(&b.id))
        }),
        SortMode::Recent => albums.sort_by(|a, b| {
            b.year
                .cmp(&a.year)
                .then_with(|| title_key(a).cmp(&title_key(b)))
                .then_with(|| a.id.cmp(&b.id))
        }),
        SortMode::Shuffle => albums.sort_by(|a, b| {
            shuffle_key(a.id)
                .cmp(&shuffle_key(b.id))
                .then_with(|| a.id.cmp(&b.id))
        }),
        // Album / Title / MostPlayed: album title, A–Z.
        _ => albums.sort_by(|a, b| {
            title_key(a)
                .cmp(&title_key(b))
                .then_with(|| a.id.cmp(&b.id))
        }),
    }
}

/// Reorder an artist list by the user's chosen [`SortMode`], in place.
///
/// * `Recent`: most tracks first, then name;
/// * `Shuffle`: the deterministic per-id order;
/// * everything else: artist name, A–Z.
pub fn sort_artists(artists: &mut [&Artist], sort: SortMode) {
    let name_key = |a: &Artist| a.name.to_lowercase();
    match sort {
        SortMode::Recent => artists.sort_by(|a, b| {
            b.track_ids
                .len()
                .cmp(&a.track_ids.len())
                .then_with(|| name_key(a).cmp(&name_key(b)))
                .then_with(|| a.id.cmp(&b.id))
        }),
        SortMode::Shuffle => artists.sort_by(|a, b| {
            shuffle_key(a.id)
                .cmp(&shuffle_key(b.id))
                .then_with(|| a.id.cmp(&b.id))
        }),
        _ => artists.sort_by(|a, b| name_key(a).cmp(&name_key(b)).then_with(|| a.id.cmp(&b.id))),
    }
}

/// The deterministic shuffle key `Library::ordered_tracks(SortMode::Shuffle)`
/// uses (a splitmix64-style hash of the id), so an album/artist shuffle is
/// stable within a session and reproducible in tests.
fn shuffle_key(id: u64) -> u64 {
    let mut x = id.wrapping_add(0x9E37_79B9_7F4A_7C15);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^ (x >> 31)
}

/// The titlebar Up Next toggle's Lucide glyph.
pub const QUEUE_TOGGLE_ICON: &str = "list-ordered";

/// The compact-rung search affordance's Lucide glyph and routable id.
pub const SEARCH_ICON: &str = "search";
/// The stable widget id the compact-rung search icon publishes.
pub const SEARCH_ICON_ID: &str = "music-titlebar-search";

/// The main menu trigger's accessible name.
pub const NOW_PLAYING_MENU_LABEL: &str = "Menu";
/// The main menu trigger's tooltip.
pub const MAIN_MENU_TOOLTIP: &str = "Main Menu";

/// The stable widget id the now-playing overflow trigger publishes.
pub const NOW_PLAYING_MENU_TRIGGER_ID: &str = "music-now-playing-menu";

/// Identity shared by the overflow trigger and its modal focus-restoration routes.
pub fn now_playing_menu_trigger_id() -> iced::advanced::widget::Id {
    iced::advanced::widget::Id::new(NOW_PLAYING_MENU_TRIGGER_ID)
}

/// The stable widget id the Up Next toggle publishes so a render test can
/// find and press it by handle.
pub const QUEUE_TOGGLE_ID: &str = "music-queue-toggle";

/// The Up Next toggle's accessible name and tooltip: `Up Next` when the queue
/// is hidden, `Close Up Next` when it is shown.
pub fn queue_toggle_label(on_queue: bool) -> &'static str {
    if on_queue { "Close Up Next" } else { "Up Next" }
}

pub(crate) fn sort_control<'a>(
    current: SortMode,
    sort_menu_model: &'a enzui_core::menu_model::MenuModel,
    sort_menu: &'a enzui_core::menu_model::MenuState,
    view: View,
) -> Element<'a, Message> {
    // Row ids derive from `SortMode` in [`sort_menu_entry_id`], so menu and
    // reducer cannot drift. The trigger is a toolbar icon like its
    // neighbours; its accessible name states the current mode.
    use enzui_widgets::menu_button::{MenuButtonKind, enz_menu_button};
    enz_menu_button("", sort_menu_model, sort_menu, Message::SortMenu)
        .kind(MenuButtonKind::Toolbar)
        .trigger_icon(SORT_ICON)
        .tooltip(if view == View::Home {
            SORT_AND_FILTER_TOOLTIP
        } else {
            SORT_TOOLTIP
        })
        .aria_label(sort_trigger_label(current))
        .menu_id(iced::advanced::widget::Id::new(SORT_MENU_TRIGGER_ID))
        .into()
}

/// The Sort menu trigger's glyph and tooltips.
pub const SORT_ICON: &str = "arrow-up-down";
pub const SORT_TOOLTIP: &str = "Sort";
pub const SORT_AND_FILTER_TOOLTIP: &str = "Sort and Filter";
