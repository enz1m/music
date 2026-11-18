//! The Songs view and the capped track-list section it shares with Search.

use super::rows::{filter_chip, track_row};
use super::*;
use iced::widget::column;

// Home view

/// The Add Folder action's label (Songs titlebar, Settings › Library and
/// the empty library); opens the native XDG directory chooser.
pub const ADD_FOLDER_LABEL: &str = "Add Folder";

/// The Add Folder button's stable widget id; its icon label is not a
/// plain-text selector target.
pub const ADD_FOLDER_ID: &str = "music-app-add-folder";

/// The per-folder trash control's accessible label and tooltip, on the
/// Settings Library page.
pub const REMOVE_FOLDER_LABEL: &str = "Remove folder";

/// The label on the control that lifts a track list's page cap.
///
/// It carries the total, so the user learns how much was withheld.
pub fn format_show_all_tracks(total: usize) -> String {
    format!("Show all {total} tracks")
}

/// The caption beside a track-list heading: `"50 of 120 shown"` while
/// truncated, `"120 shown"` when complete, never a clamped
/// `format!("{} shown", ids.len().min(50))`.
pub fn format_track_list_caption(visible: usize, total: usize) -> String {
    if visible < total {
        format!("{visible} of {total} shown")
    } else {
        format!("{total} shown")
    }
}

/// A heading, capped track list and disclosure of the cap, shared by Home's
/// library list and Search's `Tracks` section.
///
/// Whenever `visible < total`, the frame carries the
/// `"{visible} of {total} shown"` caption and a `Show all {total} tracks`
/// control that renders every remaining row.
#[allow(clippy::too_many_arguments)]
pub(crate) fn track_list_section<'a>(
    heading: &'static str,
    ids: &[u64],
    library: &'a Library,
    playback: &PlaybackState,
    enz_theme: &'a EnzTheme,
    scope: TrackListScope,
    expansion: TrackListExpansion,
) -> Element<'a, Message> {
    let tc = theme::text_color(enz_theme);
    let sc = theme::secondary_text_color(enz_theme);

    let total = ids.len();
    let visible = track_list_visible(total, expansion.is_expanded(scope));

    let mut section = column![].spacing(enz_theme.spacing.sm());
    let library_list = heading == "Tracks" && scope == TrackListScope::Library;
    if !library_list {
        section = section.push(
            row![
                enz_text(heading.to_string())
                    .size(TextSize::Title3)
                    .color(tc)
                    // Section rung under the page title in the titlebar.
                    .heading(HeadingLevel::Section),
                h_space(),
                enz_text(format_track_list_caption(visible, total))
                    .size(TextSize::Caption)
                    .color(sc),
            ]
            .align_y(Alignment::Center),
        );
    }
    // The library list's count is the header's stats line, and the `Show all`
    // control at the list's end discloses the cap, so it takes no caption row.
    let rows = ids.iter().take(visible).enumerate().fold(
        column![].spacing(enz_theme.spacing.space0),
        |col, (i, &id)| {
            if let Some(track) = library.tracks.get(&id) {
                let is_playing = playback.current_track_id == Some(id);
                col.push(track_row(
                    heading,
                    i,
                    track,
                    is_playing,
                    library.availability_of(id),
                    enz_theme,
                ))
            } else {
                col
            }
        },
    );
    section = section.push(rows);

    // Shown only while rows are withheld.
    if visible < total {
        section = section.push(
            row![
                enz_button(
                    format_show_all_tracks(total),
                    Message::ExpandTrackList(scope),
                )
                .id(enzui_core::identity::derive_id(
                    "music-expand-tracks",
                    &format!("{scope:?}")
                ))
                .kind(ButtonKind::Quiet)
                .size(ButtonSize::Sm)
                .icon("chevron-down")
                .into_element(),
                h_space(),
            ]
            .align_y(Alignment::Center),
        );
    }

    section.into()
}

/// The Songs header's facts with their glyphs: the library's tracks, albums
/// and length, or a filter's tracks and length.
pub fn songs_stats(
    library: &Library,
    ids: &[u64],
    filter: LibraryFilter,
) -> Vec<(&'static str, String)> {
    if filter == LibraryFilter::None {
        vec![
            (
                "music",
                format_count(library.track_count(), "track", "tracks"),
            ),
            (
                "disc",
                format_count(library.albums.len(), "album", "albums"),
            ),
            ("clock", format_duration(library.total_duration())),
        ]
    } else {
        let secs: f64 = ids
            .iter()
            .filter_map(|id| library.tracks.get(id))
            .map(|t| t.duration_secs)
            .sum();
        vec![
            ("music", format_count(ids.len(), "track", "tracks")),
            ("clock", format_duration(secs)),
        ]
    }
}

/// The Songs page. Its title, Play/Shuffle, Sort (with the rated filter)
/// and Add Folder live in the titlebar; library folders are managed in
/// Settings › Library.
#[allow(clippy::too_many_arguments)]
pub fn home_view<'a>(
    library: &'a Library,
    playback: &PlaybackState,
    enz_theme: &'a EnzTheme,
    sort_mode: SortMode,
    filter: LibraryFilter,
    expansion: TrackListExpansion,
) -> Element<'a, Message> {
    let sc = theme::secondary_text_color(enz_theme);
    let ids = library.ordered_tracks(sort_mode, filter);
    let mut content = column![]
        .spacing(enz_theme.spacing.md())
        .padding(super::page_padding(enz_theme));

    if library.track_count() > 0 {
        // Each fact led by its glyph, spaced apart rather than run together
        // with separators.
        let mut header = row![]
            .spacing(enz_theme.spacing.lg())
            .align_y(Alignment::Center);
        for (icon, fact) in songs_stats(library, &ids, filter) {
            header = header.push(
                row![
                    enz_icon(icon)
                        .size(enz_theme.sizing.icon_xs)
                        .tint(sc)
                        .into_element(),
                    enz_text(fact).size(TextSize::Footnote).color(sc),
                ]
                .spacing(enz_theme.spacing.xs())
                .align_y(Alignment::Center),
            );
        }
        header = header.push(h_space());
        // A genre is the titlebar's title (with its way back); only the
        // rated filter, toggled from the Sort menu, keeps a clearing chip.
        if filter == LibraryFilter::Rated
            && let Some(chip) = filter_chip(filter, library)
        {
            header = header.push(chip);
        }
        // On the rows' content edge.
        content = content.push(
            container(header).padding(
                Padding::new(0.0)
                    .left(layout::TrackRowLayout::PAD_L)
                    .right(layout::TrackRowLayout::PAD_L),
            ),
        );
    }

    if !ids.is_empty() {
        content = content.push(track_list_section(
            "Tracks",
            &ids,
            library,
            playback,
            enz_theme,
            TrackListScope::Library,
            expansion,
        ));
        let ranked = most_played_ids(library, filter, 10);
        if !ranked.is_empty() {
            content = content.push(track_list_section(
                "Most Played",
                &ranked,
                library,
                playback,
                enz_theme,
                TrackListScope::Library,
                expansion,
            ));
        }
        let recent = library.recently_played_ids(filter, 10);
        if !recent.is_empty() {
            content = content.push(track_list_section(
                "Recently Played",
                &recent,
                library,
                playback,
                enz_theme,
                TrackListScope::Library,
                expansion,
            ));
        }
    } else if library.track_count() > 0 && filter != LibraryFilter::None {
        content = content.push(empty_state_owned(
            "search",
            format_no_matches_for(&filter_display_value(filter, library).unwrap_or_default()),
            "Try clearing the filter.".into(),
        ));
    } else if !library.folders.is_empty() {
        content = content.push(
            enz_status_page("No audio files found")
                .icon("file-music")
                .description("The folders you added contain no audio files yet.")
                .variant(StatusPageVariant::Inline)
                .primary_action(
                    Action::new(
                        "music.open-library-preferences",
                        Message::OpenLibraryPreferences,
                    )
                    .with_label("Manage Folders"),
                )
                .into_element(),
        );
    } else {
        content = content.push(
            enz_status_page("Your library is empty")
                .icon("music")
                .description("Add a music folder to auto-detect metadata and cover art.")
                .variant(StatusPageVariant::Inline)
                .primary_action(
                    Action::new("music.add-folder", Message::AddFolder)
                        .with_label(ADD_FOLDER_LABEL),
                )
                .into_element(),
        );
    }

    crate::views::page_scroll(content)
}
