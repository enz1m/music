use super::*;

#[path = "library_parts/home.rs"]
mod home;
pub(super) use home::track_list_section;
pub use home::{ADD_FOLDER_ID, ADD_FOLDER_LABEL, REMOVE_FOLDER_LABEL, home_view};
pub(crate) use rows::filter_chip;
pub(super) use rows::sort_control;
pub use rows::{
    MAIN_MENU_TOOLTIP, NOW_PLAYING_MENU_LABEL, QUEUE_TOGGLE_ICON, QUEUE_TOGGLE_ID, SEARCH_ICON,
    SEARCH_ICON_ID, album_card, now_playing_menu_trigger_id, queue_toggle_label, sort_albums,
    sort_applies_to, sort_artists, sort_trigger_label, track_row,
};
pub(crate) use rows::{TRACK_ROW_PAD_Y, track_artwork};
#[path = "library_parts/rows.rs"]
mod rows;

use iced::widget::{column, row};

/// The top-`limit` tracks by play count for the current filter, descending,
/// with a title tie-break for a stable order. Unplayed tracks are excluded.
pub fn most_played_ids(library: &Library, filter: LibraryFilter, limit: usize) -> Vec<u64> {
    library
        .ordered_tracks(SortMode::MostPlayed, filter)
        .into_iter()
        .filter(|id| {
            library
                .tracks
                .get(id)
                .map(|t| t.play_count > 0)
                .unwrap_or(false)
        })
        .take(limit)
        .collect()
}

// ---------------------------------------------------------------------------
// Large-library projections
// ---------------------------------------------------------------------------

#[cfg(test)]
pub(crate) mod perf_cost {
    use std::cell::Cell;
    thread_local! {
        static REALIZATIONS: Cell<u64> = const { Cell::new(0) };
        static MIN_INDEX: Cell<Option<u64>> = const { Cell::new(None) };
        static MAX_INDEX: Cell<Option<u64>> = const { Cell::new(None) };
    }

    pub(super) fn realized(index: u64) {
        REALIZATIONS.with(|counter| counter.set(counter.get() + 1));
        MIN_INDEX.with(|cell| {
            let next = Some(cell.get().map_or(index, |min| min.min(index)));
            cell.set(next);
        });
        MAX_INDEX.with(|cell| {
            let next = Some(cell.get().map_or(index, |max| max.max(index)));
            cell.set(next);
        });
    }
}

#[derive(Debug, Default)]
pub(crate) struct IdProvider {
    ids: std::cell::RefCell<Vec<u64>>,
}

impl IdProvider {
    pub(crate) fn replace(&self, ids: Vec<u64>) {
        *self.ids.borrow_mut() = ids;
    }

    pub(crate) fn len(&self) -> usize {
        self.ids.borrow().len()
    }
}

impl ItemProvider<u64> for IdProvider {
    fn len(&self) -> u64 {
        self.ids.borrow().len() as u64
    }

    fn item(&self, index: u64) -> Option<u64> {
        #[cfg(test)]
        perf_cost::realized(index);
        self.ids.borrow().get(usize::try_from(index).ok()?).copied()
    }
}

impl KeyedProvider<u64> for IdProvider {
    fn key(&self, index: u64) -> Option<SequenceKey> {
        self.item(index)
            .map(|id| SequenceKey::new(album_grid_key(id)))
    }
}

impl TileProvider<u64> for IdProvider {}

pub(crate) fn album_projection(
    library: &Library,
    filter: LibraryFilter,
    sort: SortMode,
) -> Vec<u64> {
    let mut albums: Vec<&Album> = visible_albums(library, filter)
        .into_iter()
        .filter_map(|id| library.album(id))
        .collect();
    sort_albums(&mut albums, sort);
    albums.into_iter().map(|album| album.id).collect()
}

pub(crate) fn artist_projection(library: &Library, sort: SortMode) -> Vec<u64> {
    let mut artists: Vec<&Artist> = library.artists.iter().collect();
    sort_artists(&mut artists, sort);
    artists.into_iter().map(|artist| artist.id).collect()
}

// ---------------------------------------------------------------------------
// Albums view
// ---------------------------------------------------------------------------

/// The laid-out height of a two-line [`track_row`], for virtualized lists.
pub(crate) const TRACK_ROW_H: f32 = 52.0;

/// The Artists list's row pitch: a 40 px avatar plus the row's 6 px insets.
pub(crate) const ARTIST_ROW_H: f32 = 60.0;
/// The edge of one round album cover in an Artists row.
const ARTIST_COVER: f32 = 40.0;
/// How far each cover sits from the one before it: about 40% overlap.
const ARTIST_COVER_STEP: f32 = 24.0;
/// The ring that separates overlapping covers.
const ARTIST_COVER_RING: f32 = 2.0;
/// Covers shown before a "+N" bubble.
const ARTIST_COVERS_SHOWN: usize = 4;
/// The counts column, wide enough for "9,999 tracks".
const ARTIST_COUNTS_W: f32 = 84.0;

/// Stable grid cell key for an album id.
///
/// Keyed reflow follows this key rather than the cell's position, so it must
/// not depend on the album's index: at a column-count change, a positional key
/// reconciles focus, hover and scroll state into the wrong card, and a title
/// key aliases two same-titled albums onto one cell.
pub(super) fn album_grid_key(album_id: u64) -> String {
    album_id.to_string()
}

/// The album ids the Albums grid renders, in library order, after applying the
/// current [`LibraryFilter`]. The grid keys one cell per id via
/// [`album_grid_key`].
pub(super) fn visible_albums(library: &Library, filter: LibraryFilter) -> Vec<u64> {
    let mut ids: Vec<u64> = library.albums.iter().map(|a| a.id).collect();
    // An `Album` filter narrows the grid to that album, so the chip, the
    // `Albums (N)` header and the empty state all describe the visible album.
    if let LibraryFilter::Album(aid) = filter {
        ids.retain(|id| *id == aid);
    }
    if let LibraryFilter::Artist(aid) = filter
        && let Some(artist) = library.artist(aid)
    {
        ids.retain(|id| {
            library
                .albums
                .iter()
                .find(|a| a.id == *id)
                .is_some_and(|a| a.track_ids.iter().any(|t| artist.track_ids.contains(t)))
        });
    }
    // The genre domain already carries `album_ids`, so narrowing is a retain.
    if let LibraryFilter::Genre(gid) = filter
        && let Some(genre) = library.genres.iter().find(|g| g.id == gid)
    {
        ids.retain(|id| genre.album_ids.contains(id));
    }
    // A tag is a track list too.
    if let LibraryFilter::Tag(tid) = filter
        && let Some(tag) = library.tags.iter().find(|t| t.id == tid)
    {
        ids.retain(|id| {
            library
                .albums
                .iter()
                .find(|a| a.id == *id)
                .is_some_and(|a| a.track_ids.iter().any(|t| tag.track_ids.contains(t)))
        });
    }
    // A playlist is a track list, so albums are matched through `track_ids`.
    if let LibraryFilter::Playlist(pid) = filter
        && let Some(pl) = library.playlists.iter().find(|p| p.id == pid)
    {
        ids.retain(|id| {
            library
                .albums
                .iter()
                .find(|a| a.id == *id)
                .is_some_and(|a| a.track_ids.iter().any(|t| pl.track_ids.contains(t)))
        });
    }
    ids
}

/// Gap in pixels between album grid tiles, horizontally and vertically.
pub(crate) const ALBUM_GRID_GAP: f32 = 12.0;

/// Minimum tile width in pixels for the album grid in a `pane_w`-wide master
/// pane.
///
/// The grid derives its column count from this minimum and its own laid-out
/// width, then stretches the tiles to fill it; a pane too narrow for two
/// tiles gets one stretched across it.
pub(crate) fn album_grid_tile_width(pane_w: f32, _compact: bool) -> f32 {
    let grid_width = (pane_w - 32.0).max(1.0);
    let min_tile: f32 = if pane_w < 800.0 { 160.0 } else { 180.0 };
    min_tile.min(grid_width)
}

/// A tile's height beyond its width: the title and artist lines and the
/// gaps above them (the art's inset is part of the width), with 2px slack.
const ALBUM_TILE_LABEL_H: f32 = 46.0;

struct AlbumTracks<'a>(&'a [u64]);

impl ItemProvider<u64> for AlbumTracks<'_> {
    fn len(&self) -> u64 {
        self.0.len() as u64
    }
    fn item(&self, index: u64) -> Option<u64> {
        self.0.get(usize::try_from(index).ok()?).copied()
    }
}

impl KeyedProvider<u64> for AlbumTracks<'_> {
    fn key(&self, index: u64) -> Option<SequenceKey> {
        self.item(index)
            .map(|id| SequenceKey::new(format!("album-track-{id}")))
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "The view borrows independent library, playback, viewport and virtual-grid state."
)]
pub fn albums_view<'a>(
    library: &'a Library,
    playback: &PlaybackState,
    enz_theme: &'a EnzTheme,
    master_width: f32,
    // A wide-mode master pane stays a multi-column grid area even when the
    // now-playing panel narrows it below the compact card threshold.
    compact: bool,
    filter: LibraryFilter,
    _sort: SortMode,
    albums: &'a IdProvider,
    scroll: enzui_widgets::scroll_view::ScrollTranslation,
    // Owned by the caller because the grid detects a column-count change by
    // comparing against the previous frame's values.
    anchor: &'a std::cell::RefCell<Option<enzui_core::virtual_sequence::KeyedAnchor>>,
    anchor_columns: &'a std::cell::Cell<usize>,
    geometry: &'a std::cell::Cell<enzui_core::virtual_grid::ResolvedTileGrid>,
) -> Element<'a, Message> {
    let tc = theme::text_color(enz_theme);
    let sc = theme::secondary_text_color(enz_theme);

    let mut content = column![]
        .spacing(enz_theme.spacing.md())
        .padding(page_padding(enz_theme));

    // The grid opens with its count; the title (and, for an artist, the
    // artist's name and the way back) is in the titlebar. An album shows its
    // own header below instead.
    if !matches!(filter, LibraryFilter::Album(_)) && albums.len() > 0 {
        let mut header = row![
            enz_text(format_albums_header(albums.len()))
                .size(TextSize::Footnote)
                .color(sc)
                .full_width(),
        ]
        .spacing(enz_theme.spacing.sm())
        .align_y(Alignment::Center);
        // Filters without a titlebar context of their own keep a chip.
        if matches!(
            filter,
            LibraryFilter::Genre(_) | LibraryFilter::Tag(_) | LibraryFilter::Playlist(_)
        ) && let Some(chip) = filter_chip(filter, library)
        {
            header = header.push(chip);
        }
        // On the tiles' art edge, inside their hover inset.
        content =
            content.push(container(header).padding(Padding::new(0.0).left(enz_theme.spacing.sm())));
    }

    if let LibraryFilter::Album(aid) = filter
        && let Some(album) = library.album(aid)
    {
        let art_width = if master_width < 500.0 { 100.0 } else { 140.0 };
        let art: Element<'a, Message> = match &album.cover {
            Some(cover) => enz_image_handle(cover.handle().clone())
                .fit(EnzImageFit::Cover)
                .radius(enz_theme.radius.xl)
                .alt_text(format!("{} album art", album.title))
                .width(Length::Fixed(art_width))
                .height(Length::Fixed(art_width))
                .into_element(),
            None => {
                use enzui_widgets::surface::{RadiusSize, SurfaceTone, enz_surface};
                enz_surface(
                    container(
                        enz_icon("music")
                            .size(enz_theme.sizing.icon_xl)
                            .tint(sc)
                            .into_element(),
                    )
                    .center_x(art_width)
                    .center_y(art_width),
                )
                .tone(SurfaceTone::SurfaceAlt)
                .radius_size(RadiusSize::Xl)
                .width(art_width)
                .height(art_width)
                .into_element()
            }
        };
        let year = (album.year > 0).then(|| album.year.to_string());
        let mut facts = vec![
            format_count(album.track_ids.len(), "track", "tracks"),
            format_duration(
                album
                    .track_ids
                    .iter()
                    .filter_map(|id| library.tracks.get(id))
                    .map(|track| track.duration_secs)
                    .sum(),
            ),
        ];
        if let Some(year) = year {
            facts.insert(0, year);
        }
        let summary = column![
            enz_text(album.title.as_str())
                .size(TextSize::Title2)
                .color(tc)
                .full_width()
                .wrap(true)
                .lines(2)
                .heading(HeadingLevel::Section),
            enz_text(album.display_artist())
                .size(TextSize::Body)
                .color(tc)
                .full_width()
                .single_line_elided(),
            enz_text(facts.join(" · "))
                .size(TextSize::Footnote)
                .color(sc),
        ]
        .spacing(enz_theme.spacing.xs())
        .width(Length::Fill);
        content = content.push(
            container(
                row![art, summary]
                    .spacing(enz_theme.spacing.lg())
                    .align_y(Alignment::End)
                    .height(Length::Shrink),
            )
            .padding(
                Padding::new(0.0)
                    .left(layout::TrackRowLayout::PAD_L)
                    .right(layout::TrackRowLayout::PAD_L)
                    .bottom(enz_theme.spacing.sm()),
            ),
        );
        let tracks = std::rc::Rc::new(AlbumTracks(&album.track_ids));
        let current_track_id = playback.current_track_id;
        let rows = iced::widget::responsive(move |available| {
            let heights = album.track_ids.iter().map(|id| {
                if library.availability_of(*id).badge_label().is_some() {
                    TRACK_ROW_H + 24.0
                } else {
                    TRACK_ROW_H
                }
            });
            enzui_widgets::virtual_list::enz_virtual_list_variable(
                std::rc::Rc::clone(&tracks),
                heights,
                move |index, id| {
                    library
                        .tracks
                        .get(id)
                        .map(|track| {
                            track_row(
                                "Album",
                                index as usize,
                                track,
                                current_track_id == Some(*id),
                                library.availability_of(*id),
                                enz_theme,
                            )
                        })
                        .unwrap_or_else(|| Space::new().into())
                },
            )
            .viewport_height(available.height)
            .self_scrolling(scroll.clone())
            .height(Length::Fill)
            .into_element()
        });
        return content.push(rows).height(Length::Fill).into();
    }

    if albums.len() == 0 {
        // A filtered-to-nothing state and an empty library are distinct; showing
        // the wrong one reads as data loss.
        let el = if matches!(filter, LibraryFilter::None) {
            empty_state("disc", "No albums yet", "Your library has no albums yet.")
        } else {
            let q = filter_display_value(filter, library).unwrap_or_default();
            empty_state_owned(
                "disc",
                format_no_matches_for(&q),
                "Try clearing the filter.".to_string(),
            )
        };
        content = content.push(el);
        return crate::views::page_scroll(content);
    }

    // The provider keeps stable album ids, so virtual reflow retains logical
    // identity while the number of built cards stays viewport-bounded.
    let tile_width = album_grid_tile_width(master_width, compact);
    // The art is square at whatever width the columns stretch to; the grid
    // grows the height with it so the label lines stay inside the tile.
    let tile_height = tile_width + ALBUM_TILE_LABEL_H;
    let grid = enz_virtual_grid(
        albums,
        TileGrid::new(tile_width, tile_height).with_spacing(ALBUM_GRID_GAP, ALBUM_GRID_GAP),
        move |_index, album_id| {
            library
                .album(*album_id)
                .map(|album| album_card(album, enz_theme))
                .unwrap_or_else(|| Space::new().into())
        },
    )
    .adaptive_stretch(geometry)
    .stretch_height_with_width(true)
    .self_scrolling(scroll)
    .reflow_anchor(anchor, anchor_columns)
    .height(Length::Fill);

    content.push(grid).height(Length::Fill).into()
}

// ---------------------------------------------------------------------------
// Artists view
// ---------------------------------------------------------------------------

/// Pluralise a count against a unit with the same 0/1/n contract as
/// `podcasts::format_episode_count`: `"No {plural}"`, `"1 {singular}"`, or
/// `"{n} {plural}"`.
pub fn format_count(n: usize, singular: &str, plural: &str) -> String {
    match n {
        0 => format!("No {plural}"),
        1 => format!("1 {singular}"),
        n => format!("{n} {plural}"),
    }
}

/// The `"N albums · M tracks"` subtitle shared by the Artists and Genres rows,
/// each side pluralised through [`format_count`].
pub fn format_albums_tracks(n_albums: usize, n_tracks: usize) -> String {
    format!(
        "{} · {}",
        format_count(n_albums, "album", "albums"),
        format_count(n_tracks, "track", "tracks"),
    )
}

pub fn artists_view<'a>(
    library: &'a Library,
    _sort: SortMode,
    enz_theme: &'a EnzTheme,
    artists: &'a IdProvider,
    scroll: enzui_widgets::scroll_view::ScrollTranslation,
) -> Element<'a, Message> {
    let tc = theme::text_color(enz_theme);
    let sc = theme::secondary_text_color(enz_theme);

    let mut content = column![]
        .spacing(enz_theme.spacing.md())
        .padding(page_padding(enz_theme));

    let artists_count = library.artists.len();
    if artists_count > 0 {
        content = content.push(
            container(
                enz_text(format_artists_header(artists_count))
                    .size(TextSize::Footnote)
                    .color(sc),
            )
            .padding(Padding::new(0.0).left(layout::TrackRowLayout::PAD_L)),
        );
    }

    if library.artists.is_empty() {
        content = content.push(empty_state(
            "mic",
            "No artists yet",
            "Your library has no artists yet.",
        ));
        return crate::views::page_scroll(content);
    }

    let list = enz_virtual_list(artists, ARTIST_ROW_H, move |_index, artist_id| {
        let Some(artist) = library.artist(*artist_id) else {
            return Space::new().into();
        };
        let n_albums = artist.album_ids.len();
        let n_tracks = artist.track_ids.len();
        let artist_id = artist.id;

        // Name, then the artist's albums as overlapping round covers, then
        // the counts stacked and right-aligned, then the drill-in chevron.
        let name = enz_text(artist.name.as_str())
            .size(TextSize::Body)
            .weight(iced::font::Weight::Semibold)
            .color(tc)
            .full_width()
            .single_line_elided();
        let counts = column![
            enz_text(format_count(n_albums, "album", "albums"))
                .size(TextSize::Footnote)
                .color(tc),
            enz_text(format_count(n_tracks, "track", "tracks"))
                .size(TextSize::Footnote)
                .color(sc),
        ]
        .spacing(enz_theme.spacing.space0)
        .align_x(Alignment::End)
        .width(Length::Fixed(ARTIST_COUNTS_W));

        let artist_row = row![
            name,
            artist_cover_stack(library, &artist.album_ids, enz_theme),
            counts,
            drill_in_chevron(enz_theme),
        ]
        .spacing(enz_theme.spacing.md())
        .align_y(Alignment::Center);

        // Clicking an artist shows that artist's albums.
        let artist_el: Element<'_, Message> = enz_list_row_surface(
            container(artist_row)
                .padding(Padding::new(0.0).top(8.0).bottom(8.0))
                .width(Length::Fill),
            Message::SelectArtist(artist_id),
        )
        .content_padding(layout::TrackRowLayout::PAD_L)
        .outer_gutter(0.0)
        .content_height(true)
        .label(artist.name.clone())
        .id(enzui_core::identity::derive_id(
            "music-artist",
            &artist_id.to_string(),
        ))
        .into();
        artist_el
    })
    .self_scrolling(scroll)
    .height(Length::Fill);

    content.push(list).height(Length::Fill).into()
}

// ---------------------------------------------------------------------------
// Genres view
// ---------------------------------------------------------------------------

pub fn genres_view<'a>(library: &'a Library, enz_theme: &'a EnzTheme) -> Element<'a, Message> {
    let tc = theme::text_color(enz_theme);
    let sc = theme::secondary_text_color(enz_theme);

    let mut content = column![]
        .spacing(enz_theme.spacing.space0)
        .padding(page_padding(enz_theme));
    if !library.genres.is_empty() {
        content = content.push(
            container(
                enz_text(format_count(library.genres.len(), "genre", "genres"))
                    .size(TextSize::Footnote)
                    .color(sc),
            )
            .padding(
                Padding::new(0.0)
                    .left(layout::TrackRowLayout::PAD_L)
                    .bottom(enz_theme.spacing.md()),
            ),
        );
    }

    if library.genres.is_empty() {
        content = content.push(empty_state(
            "music",
            "No genres",
            "Your library has no genres yet.",
        ));
        return crate::views::page_scroll(content);
    }

    for genre in &library.genres {
        let n_tracks = genre.track_ids.len();
        let n_albums = genre.album_ids.len();
        let genre_id = genre.id;

        let content_row = row![
            list_glyph("music-4", enz_theme),
            column![
                enz_text(genre.name.as_str())
                    .size(TextSize::Body)
                    .color(tc)
                    .full_width()
                    .single_line_elided(),
                enz_text(format_albums_tracks(n_albums, n_tracks))
                    .size(TextSize::Footnote)
                    .color(sc),
            ]
            .spacing(enz_theme.spacing.space1)
            .width(Length::Fill),
            drill_in_chevron(enz_theme),
        ]
        .spacing(enz_theme.spacing.md())
        .align_y(Alignment::Center);

        // Row ids derive from the genre's immutable library id, never its
        // renamable name, so renaming under a live menu cannot move the id that
        // focus restoration probes. `on_context` publishes `ShowContextMenu` and
        // reports the anchor point for the shared host.
        let genre_name = genre.name.clone();
        let genre_row_id = crate::context_menu::genre_row_id(genre_id);
        let genre_el: Element<'_, Message> = enz_list_row_surface(
            container(content_row)
                .width(Length::Fill)
                .padding(Padding::new(0.0).top(6.0).bottom(6.0)),
            Message::SelectGenre(genre_id),
        )
        .content_padding(layout::TrackRowLayout::PAD_L)
        .outer_gutter(0.0)
        .content_height(true)
        .label(genre_name.clone())
        .id(genre_row_id.clone())
        .context_id(crate::context_menu::context_target_id(&genre_row_id))
        .on_context(move |at| Message::OpenContextMenu(ContextTarget::Genre(genre_id), at))
        .context_label(format!("{genre_name} actions"))
        .into();
        content = content.push(genre_el);
    }

    crate::views::page_scroll(content)
}

// ---------------------------------------------------------------------------
// Tags view
// ---------------------------------------------------------------------------

/// The tags view: the tag groupings `Library::rebuild_tags` derives from each
/// track's genre string. Unlike Genres, a track can carry many tags.
///
/// `No tags found` is distinct from the Home view's empty-library and
/// no-audio-files states.
pub fn tags_view<'a>(library: &'a Library, enz_theme: &'a EnzTheme) -> Element<'a, Message> {
    let tc = theme::text_color(enz_theme);
    let sc = theme::secondary_text_color(enz_theme);

    let mut content = column![]
        .spacing(enz_theme.spacing.space0)
        .padding(page_padding(enz_theme));
    if !library.tags.is_empty() {
        content = content.push(
            container(
                enz_text(format_count(library.tags.len(), "tag", "tags"))
                    .size(TextSize::Footnote)
                    .color(sc),
            )
            .padding(
                Padding::new(0.0)
                    .left(layout::TrackRowLayout::PAD_L)
                    .bottom(enz_theme.spacing.md()),
            ),
        );
    }

    if library.tags.is_empty() {
        content = content.push(empty_state(
            "tag",
            "No tags found",
            "Your library has no tags yet.",
        ));
        return crate::views::page_scroll(content);
    }

    for tag in &library.tags {
        let n_tracks = tag.track_ids.len();
        let content_row = row![
            list_glyph("tag", enz_theme),
            column![
                enz_text(tag.name.as_str())
                    .size(TextSize::Body)
                    .color(tc)
                    .full_width()
                    .single_line_elided(),
                enz_text(format_count(n_tracks, "track", "tracks"))
                    .size(TextSize::Footnote)
                    .color(sc),
            ]
            .spacing(enz_theme.spacing.space1)
            .width(Length::Fill),
            drill_in_chevron(enz_theme),
        ]
        .spacing(enz_theme.spacing.md())
        .align_y(Alignment::Center);

        // A tag opens its tracks, as a genre does.
        let row_el: Element<'_, Message> = enz_list_row_surface(
            container(content_row)
                .width(Length::Fill)
                .padding(Padding::new(0.0).top(6.0).bottom(6.0)),
            Message::SelectTag(tag.id),
        )
        .content_padding(layout::TrackRowLayout::PAD_L)
        .outer_gutter(0.0)
        .content_height(true)
        .label(tag.name.clone())
        .into();
        content = content.push(row_el);
    }

    crate::views::page_scroll(content)
}

/// A neutral 32 px leading glyph tile for list rows without artwork, the same
/// size and shape as [`track_artwork`]'s placeholder.
pub(crate) fn list_glyph<'a>(icon: &'a str, enz_theme: &'a EnzTheme) -> Element<'a, Message> {
    use enzui_widgets::surface::{SurfaceTone, enz_surface};
    let edge = layout::TrackRowLayout::INDEX_W;
    enz_surface(
        container(
            enz_icon(icon)
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
    .into_element()
}

/// An artist's albums as round covers, each overlapping the one before, with a "+N" bubble for the rest. Right-aligned in a fixed width so
/// the counts beside it line up from row to row.
fn artist_cover_stack<'a>(
    library: &'a Library,
    album_ids: &[u64],
    enz_theme: &'a EnzTheme,
) -> Element<'a, Message> {
    use enzui_widgets::surface::{SurfaceTone, enz_surface};
    let edge = ARTIST_COVER;
    let ring = ARTIST_COVER_RING;
    let outer = edge + ring * 2.0;
    let slots = ARTIST_COVERS_SHOWN + 1;
    let full_width = outer + ARTIST_COVER_STEP * (slots - 1) as f32;
    // A ring in the hover tone: it separates overlapping covers, gives a pale
    // cover an edge on a pale page, and reads the same on a hovered row.
    let ring_color = theme::surface_alt_color(enz_theme);
    let ringed = move |inner: Element<'a, Message>| -> Element<'a, Message> {
        container(inner)
            .padding(ring)
            .style(move |_| container::Style {
                background: Some(ring_color.into()),
                border: iced::Border {
                    radius: (outer / 2.0).into(),
                    ..iced::Border::default()
                },
                ..container::Style::default()
            })
            .into()
    };
    let placeholder = |child: Element<'a, Message>| -> Element<'a, Message> {
        enz_surface(container(child).center_x(edge).center_y(edge))
            .tone(SurfaceTone::SurfaceAlt)
            .radius(edge / 2.0)
            .width(edge)
            .height(edge)
            .into_element()
    };

    // A "+1" bubble would take the slot one more cover fills.
    let shown = if album_ids.len() == ARTIST_COVERS_SHOWN + 1 {
        album_ids.len()
    } else {
        ARTIST_COVERS_SHOWN
    };
    let extra = album_ids.len().saturating_sub(shown);
    // A stable order: albums with art first, then the newest, then by title.
    let mut albums: Vec<&Album> = album_ids
        .iter()
        .filter_map(|id| library.album(*id))
        .collect();
    albums.sort_by(|a, b| {
        b.cover
            .is_some()
            .cmp(&a.cover.is_some())
            .then(b.year.cmp(&a.year))
            .then_with(|| a.title.cmp(&b.title))
    });
    let mut discs: Vec<Element<'a, Message>> = albums
        .into_iter()
        .take(shown)
        .map(|album| {
            let inner = match &album.cover {
                Some(cover) => enz_image_handle(cover.handle().clone())
                    .fit(EnzImageFit::Cover)
                    .radius(edge / 2.0)
                    .alt_text(format!("{} album art", album.title))
                    .width(Length::Fixed(edge))
                    .height(Length::Fixed(edge))
                    .into_element(),
                None => placeholder(
                    enz_icon("music")
                        .size(enz_theme.sizing.icon_sm)
                        .tint(theme::secondary_text_color(enz_theme))
                        .into_element(),
                ),
            };
            ringed(inner)
        })
        .collect();
    if extra > 0 {
        discs.push(ringed(placeholder(
            enz_text(format!("+{extra}"))
                .size(TextSize::Caption)
                .weight(iced::font::Weight::Semibold)
                .color(theme::text_color(enz_theme))
                .into(),
        )));
    }
    let count = discs.len();
    if count == 0 {
        return Space::new().width(Length::Fixed(full_width)).into();
    }
    // Each disc overlaps the one before it, so a "+N" bubble is whole.
    let start = full_width - (outer + ARTIST_COVER_STEP * (count - 1) as f32);
    let mut layers = iced::widget::Stack::new()
        .width(Length::Fixed(full_width))
        .height(Length::Fixed(outer));
    for (i, disc) in discs.into_iter().enumerate() {
        let left = start + ARTIST_COVER_STEP * i as f32;
        layers = layers.push(container(disc).padding(Padding::new(0.0).left(left)));
    }
    layers.into()
}

/// The trailing chevron of a row that opens a page.
pub(crate) fn drill_in_chevron<'a>(enz_theme: &'a EnzTheme) -> Element<'a, Message> {
    enz_icon("chevron-right")
        .size(enz_theme.sizing.icon_sm)
        .tint(theme::secondary_text_color(enz_theme))
        .into_element()
}
