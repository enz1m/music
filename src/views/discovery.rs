use super::*;
use iced::widget::{column, row};

// ---------------------------------------------------------------------------
// Search view
// ---------------------------------------------------------------------------

#[path = "discovery_parts/podcasts.rs"]
mod podcasts;
pub use radio::{SEARCH_FIELD_ID, podcasts_view, radio_view};
#[path = "discovery_parts/radio.rs"]
mod radio;

// ---------------------------------------------------------------------------
// Manage Feeds dialog (OPML import/export)
// ---------------------------------------------------------------------------

/// The per-row Retry button's label, shown only on a feed whose fetch failed.
pub const PODCAST_RETRY_LABEL: &str = "Retry";

/// The routable id of the Podcasts header Manage Feeds button.
pub const MANAGE_FEEDS_BUTTON_ID: &str = "music-app-manage-feeds";

/// The playlist detail header's Rename action label. The trailing ellipsis
/// marks a dialog opener and keeps this distinct from the dialog's own
/// `Rename` commit label while both are in the tree.
pub const RENAME_PLAYLIST_ACTION_LABEL: &str = "Rename\u{2026}";

/// Id prefix for a playlist detail header's Rename button.
pub const RENAME_PLAYLIST_BUTTON_ID: &str = "music-app-playlist-rename";

/// Stable focusable id of the Rename button for playlist `pid`, used to
/// restore focus when the dialog closes.
#[must_use]
pub fn rename_playlist_button_id(pid: u64) -> iced::advanced::widget::Id {
    enzui_core::identity::derive_id(RENAME_PLAYLIST_BUTTON_ID, &pid.to_string())
}
/// The Feed URL form's visible Subscribe button. Enter in the field and this
/// button both dispatch [`Message::PodcastSubscribeSubmit`].
pub const PODCAST_SUBSCRIBE_BUTTON_ID: &str = "music-app-podcast-subscribe";
/// The Manage Feeds dialog title.
pub const MANAGE_FEEDS_TITLE: &str = "Manage Feeds";
/// The import section heading / textarea label.
pub const MANAGE_FEEDS_IMPORT_LABEL: &str = "Import OPML";
/// The import textarea placeholder.
pub const MANAGE_FEEDS_IMPORT_PLACEHOLDER: &str = "<opml version=\"2.0\">…";
/// What an OPML file is, for someone moving from another app.
pub const MANAGE_FEEDS_FILE_HINT: &str = "Podcast apps such as AntennaPod, Pocket Casts, Overcast and Apple Podcasts export subscriptions as an OPML file.";
/// The export section heading.
pub const MANAGE_FEEDS_EXPORT_LABEL: &str = "Export OPML";

/// Compose the Manage Feeds dialog: OPML import by paste, export as copyable
/// text, and the subscription list with per-row Unsubscribe.
///
/// Import and export use text rather than a file picker, which yields only a
/// path and would add unguarded filesystem I/O.
pub fn manage_feeds_overlay<'a>(
    subscriptions: &'a [crate::podcasts::PodcastFeed],
    import_buffer: &'a iced::widget::text_editor::Content,
    export_text: &'a str,
    background: Element<'a, Message>,
    enz_theme: &'a EnzTheme,
    lifecycle: &enzui_widgets::modal_host::ModalFocusLifecycle,
) -> Element<'a, Message> {
    use enzui_widgets::text_area::{FieldSize, enz_text_area};

    let enz = enz_theme;
    let tc = theme::text_color(enz);
    let sc = theme::secondary_text_color(enz);

    let title_el: Element<'a, Message> = enz_text(MANAGE_FEEDS_TITLE)
        .size(TextSize::Title3)
        .color(tc)
        .into_element();

    // ---- Import section ---------------------------------------------------
    let import_area: Element<'a, Message> = enz_text_area(import_buffer, Message::OpmlImportAction)
        .label("Or paste OPML")
        .placeholder(MANAGE_FEEDS_IMPORT_PLACEHOLDER)
        .size(FieldSize::Md)
        .area_height(96.0)
        .id("music-manage-feeds-import")
        .into_element();

    let import_file_btn: Element<'a, Message> =
        enz_button("Import from file…", Message::OpmlImportFile)
            .icon("file-up")
            .kind(ButtonKind::Quiet)
            .size(ButtonSize::Sm)
            .id(iced::advanced::widget::Id::new(
                "music-manage-feeds-import-file",
            ))
            .into_element();
    let import_btn: Element<'a, Message> = enz_button("Import pasted", Message::OpmlImportSubmit)
        .icon("download")
        .kind(ButtonKind::Quiet)
        .size(ButtonSize::Sm)
        .id(iced::advanced::widget::Id::new(
            "music-manage-feeds-import-button",
        ))
        .into_element();

    // ---- Export section ---------------------------------------------------
    let export_file_btn: Element<'a, Message> =
        enz_button("Export to file…", Message::OpmlExportFile)
            .icon("file-down")
            .kind(ButtonKind::Quiet)
            .size(ButtonSize::Sm)
            .enabled(!subscriptions.is_empty())
            .id(iced::advanced::widget::Id::new(
                "music-manage-feeds-export-file",
            ))
            .into_element();
    let export_btn: Element<'a, Message> = enz_button("Show as text", Message::OpmlExport)
        .icon("upload")
        .kind(ButtonKind::Quiet)
        .size(ButtonSize::Sm)
        .id(iced::advanced::widget::Id::new(
            "music-manage-feeds-export-button",
        ))
        .into_element();

    let mut body_col = column![
        title_el,
        enz_divider().into_element(),
        enz_text(MANAGE_FEEDS_IMPORT_LABEL)
            .size(TextSize::Body)
            .color(tc),
        enz_text(MANAGE_FEEDS_FILE_HINT)
            .size(TextSize::Footnote)
            .color(sc),
        row![import_file_btn].align_y(Alignment::Center),
        import_area,
        row![h_space(), import_btn].align_y(Alignment::Center),
        enz_divider().into_element(),
        enz_text(MANAGE_FEEDS_EXPORT_LABEL)
            .size(TextSize::Body)
            .color(tc),
        row![export_file_btn, h_space(), export_btn].align_y(Alignment::Center),
    ]
    .spacing(enz.spacing.sm())
    .width(Length::Fixed(460.0));

    // Selectable text, not a read-only `enz_text_area`: its `Content` would
    // have to outlive this frame.
    if !export_text.is_empty() {
        body_col = body_col.push(
            container(enz_text(export_text).size(TextSize::Footnote).color(sc))
                .padding(Padding::new(8.0))
                .width(Length::Fill),
        );
    }

    // ---- Subscription list with reused Unsubscribe ------------------------
    body_col = body_col.push(enz_divider().into_element());
    body_col = body_col.push(
        enz_text("Your subscriptions")
            .size(TextSize::Body)
            .color(tc),
    );
    if subscriptions.is_empty() {
        body_col = body_col.push(
            enz_text(crate::podcasts::podcast_library_empty_label(
                crate::podcasts::PodcastLibraryState::NoSubscriptions,
            ))
            .size(TextSize::Footnote)
            .color(sc),
        );
    } else {
        for (idx, feed) in subscriptions.iter().enumerate() {
            let title: String = if feed.title.is_empty() {
                feed.feed_url.clone()
            } else {
                feed.title.clone()
            };
            body_col = body_col.push(
                row![
                    enz_text(title).size(TextSize::Body).color(tc),
                    h_space(),
                    enz_button("Unsubscribe", Message::PodcastUnsubscribe(idx))
                        .kind(ButtonKind::QuietDestructive)
                        .size(ButtonSize::Sm)
                        .into_element(),
                ]
                .align_y(Alignment::Center),
            );
        }
    }

    // ---- Footer -----------------------------------------------------------
    let close_btn: Element<'a, Message> = enz_button("Close", Message::CloseManageFeeds)
        .kind(ButtonKind::Quiet)
        .size(ButtonSize::Sm)
        .id(iced::advanced::widget::Id::new("music-manage-feeds-close"))
        .into_element();
    body_col = body_col.push(enz_divider().into_element());
    body_col = body_col.push(row![h_space(), close_btn].align_y(Alignment::Center));

    let surface: Element<'a, Message> = enz_modal_surface(body_col.into())
        .width(Length::Fixed(460.0))
        .into_element();

    // Dismissal uses `CloseManageFeeds`, which restores focus to the Manage
    // Feeds button.
    enzui_widgets::modal_host::enz_modal_host(background, Some(surface), |event| {
        Message::ModalHost(crate::ModalPresentationId::ManageFeeds, event)
    })
    .scoped_to(lifecycle)
    .into_element()
}

/// A podcast episode's details: its cover, podcast, title, date and length,
/// and its show notes in full, with Play.
#[allow(clippy::too_many_arguments)]
pub fn episode_details_overlay<'a>(
    background: Element<'a, Message>,
    feed: &'a crate::podcasts::PodcastFeed,
    episode: &'a crate::podcasts::PodcastEpisode,
    art_cache: &'a crate::podcasts::artwork::PodcastArtworkCache,
    is_current: bool,
    window: iced::Size,
    enz_theme: &'a EnzTheme,
    lifecycle: &enzui_widgets::modal_host::ModalFocusLifecycle,
) -> Element<'a, Message> {
    let enz = enz_theme;
    let tc = theme::text_color(enz);
    let sc = theme::secondary_text_color(enz);
    let width = (window.width - 2.0 * enz.spacing.lg()).clamp(280.0, 560.0);
    let podcast = if feed.title.is_empty() {
        feed.feed_url.as_str()
    } else {
        feed.title.as_str()
    };
    let mut facts: Vec<String> = Vec::new();
    if !episode.published.trim().is_empty() {
        facts.push(crate::podcasts::local_date(&episode.published, true));
    }
    if let Some(length) = crate::podcasts::format_duration(episode.duration_secs) {
        facts.push(length);
    }
    let header = row![
        podcasts::podcast_art_thumb(
            art_cache.episode_art(&feed.feed_url, &episode.guid),
            88.0,
            enz
        ),
        column![
            enz_text(podcast)
                .size(TextSize::Footnote)
                .color(sc)
                .full_width()
                .single_line_elided(),
            enz_text(episode.title.as_str())
                .size(TextSize::Title3)
                .color(tc)
                .full_width()
                .wrap(true),
            enz_text(facts.join(" \u{b7} "))
                .size(TextSize::Footnote)
                .color(sc),
        ]
        .spacing(enz.spacing.xs())
        .width(Length::Fill),
    ]
    .spacing(enz.spacing.md())
    .align_y(Alignment::Center);

    let notes = crate::podcasts::plain_paragraphs(&episode.description);
    let mut paragraphs = column![].spacing(enz.spacing.sm()).width(Length::Fill);
    if notes.is_empty() {
        paragraphs = paragraphs.push(
            enz_text("This episode has no show notes.")
                .size(TextSize::Body)
                .color(sc),
        );
    }
    for line in notes.lines() {
        paragraphs = paragraphs.push(
            enz_text(line.to_owned())
                .size(TextSize::Body)
                .color(tc)
                .full_width()
                .wrap(true),
        );
    }
    let notes_height = (window.height * 0.5).clamp(140.0, 420.0);
    let notes_view: Element<'a, Message> = container(
        enz_scroll_view(container(paragraphs).padding(Padding::new(0.0).right(enz.spacing.md())))
            .height(Length::Fill),
    )
    .height(Length::Fixed(notes_height))
    .width(Length::Fill)
    .into();

    let play: Element<'a, Message> = enz_button(
        if is_current {
            "Playing"
        } else {
            "Play episode"
        },
        Message::PodcastEpisodeSelect(feed.feed_url.clone(), episode.guid.clone()),
    )
    .icon("play")
    .kind(ButtonKind::Accent)
    .size(ButtonSize::Sm)
    .enabled(!is_current)
    .id(iced::advanced::widget::Id::new(
        "music-episode-details-play",
    ))
    .into_element();
    let close: Element<'a, Message> = enz_button("Close", Message::CloseEpisodeDetails)
        .kind(ButtonKind::Quiet)
        .size(ButtonSize::Sm)
        .id(iced::advanced::widget::Id::new(
            "music-episode-details-close",
        ))
        .into_element();

    let body = column![
        header,
        enz_divider().into_element(),
        notes_view,
        row![play, h_space(), close].align_y(Alignment::Center),
    ]
    .spacing(enz.spacing.md())
    .width(Length::Fixed(width));
    let surface: Element<'a, Message> = enz_modal_surface(body.into())
        .width(Length::Fixed(width))
        .into_element();
    enzui_widgets::modal_host::enz_modal_host(background, Some(surface), |event| {
        Message::ModalHost(crate::ModalPresentationId::EpisodeDetails, event)
    })
    .label(format!("{} details", episode.title))
    .scoped_to(lifecycle)
    .into_element()
}

pub fn search_view<'a>(
    library: &'a Library,
    playback: &PlaybackState,
    enz_theme: &'a EnzTheme,
    query: &'a str,
    expansion: TrackListExpansion,
) -> Element<'a, Message> {
    let tc = theme::text_color(enz_theme);
    let sc = theme::secondary_text_color(enz_theme);

    let mut content = column![]
        .spacing(enz_theme.spacing.lg())
        .padding(page_padding(enz_theme));

    // The query is edited in the titlebar (see `views::titlebar`).
    if query.trim().is_empty() {
        // Exact string; see `search_empty_prompt_matches_dossier`.
        content = content.push(empty_state(
            "search",
            "Search your library",
            "Start typing to search your library.",
        ));
        return crate::views::page_scroll(content);
    }

    let results = library.search(query);

    if results.is_empty() {
        // Single-quoted `No results for '<query>'` keeps search-empty distinct
        // from the filter-empty `No matches for "X"`.
        content = content.push(empty_state_owned(
            "search-x",
            format_no_results_for(query.trim()),
            "No tracks match your search.".to_string(),
        ));
        return crate::views::page_scroll(content);
    }

    // The result count across all three sections.
    content = content.push(
        enz_text(format_search_result_count(results.total()))
            .size(TextSize::Footnote)
            .color(sc),
    );

    // Artists section
    if !results.artists.is_empty() {
        content = content.push(
            enz_text("Artists")
                .size(TextSize::Title3)
                .color(tc)
                .heading(HeadingLevel::Section),
        );
        let mut rows = column![].spacing(enz_theme.spacing.space0);
        for aid in &results.artists {
            if let Some(a) = library.artist(*aid) {
                let n_tracks = a.track_ids.len();
                let aid_ = a.id;
                let el: Element<'_, Message> = enz_list_row_surface(
                    container(
                        row![
                            enz_avatar(a.name.as_str()).avatar_size(32.0).into_element(),
                            column![
                                enz_text(a.name.as_str())
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
                        .spacing(layout::TrackRowLayout::SPACING)
                        .align_y(Alignment::Center),
                    )
                    .width(Length::Fill)
                    .padding(
                        Padding::new(0.0)
                            .top(TRACK_ROW_PAD_Y)
                            .bottom(TRACK_ROW_PAD_Y),
                    ),
                    Message::SelectArtist(aid_),
                )
                .content_padding(layout::TrackRowLayout::PAD_L)
                .outer_gutter(0.0)
                .content_height(true)
                .id_prefix(format!("music-search-artist-{aid_}"))
                .into();
                rows = rows.push(el);
            }
        }
        content = content.push(rows);
    }

    // Albums section
    if !results.albums.is_empty() {
        content = content.push(
            enz_text("Albums")
                .size(TextSize::Title3)
                .color(tc)
                .heading(HeadingLevel::Section),
        );
        let mut rows = column![].spacing(enz_theme.spacing.space0);
        for aid in &results.albums {
            if let Some(a) = library.album(*aid) {
                let n_tracks = a.track_ids.len();
                let aid_ = a.id;
                let el: Element<'_, Message> = enz_list_row_surface(
                    container(
                        row![
                            track_artwork(
                                a.cover.as_ref(),
                                format!("{} album art", a.title),
                                enz_theme
                            ),
                            column![
                                enz_text(a.title.as_str())
                                    .size(TextSize::Body)
                                    .color(tc)
                                    .full_width()
                                    .single_line_elided(),
                                enz_text(format!(
                                    "{} · {}",
                                    a.display_artist(),
                                    format_count(n_tracks, "track", "tracks")
                                ))
                                .size(TextSize::Footnote)
                                .color(sc)
                                .full_width()
                                .single_line_elided(),
                            ]
                            .spacing(enz_theme.spacing.space1)
                            .width(Length::Fill),
                            drill_in_chevron(enz_theme),
                        ]
                        .spacing(layout::TrackRowLayout::SPACING)
                        .align_y(Alignment::Center),
                    )
                    .padding(
                        Padding::new(0.0)
                            .top(TRACK_ROW_PAD_Y)
                            .bottom(TRACK_ROW_PAD_Y),
                    )
                    .width(Length::Fill),
                    Message::SelectAlbum(aid_),
                )
                .content_padding(layout::TrackRowLayout::PAD_L)
                .outer_gutter(0.0)
                .content_height(true)
                .id_prefix(format!("music-search-album-{aid_}"))
                .into();
                rows = rows.push(el);
            }
        }
        content = content.push(rows);
    }

    // Tracks section, with a counted caption and `Show all N tracks`.
    if !results.tracks.is_empty() {
        content = content.push(track_list_section(
            "Tracks",
            &results.tracks,
            library,
            playback,
            enz_theme,
            TrackListScope::Search,
            expansion,
        ));
    }

    crate::views::page_scroll(content)
}

// ---------------------------------------------------------------------------
// Playlists view
// ---------------------------------------------------------------------------

/// The playlist page header's artwork tile edge, in px.
const PLAYLIST_ART_EDGE: f32 = 72.0;

/// The playlist Import/Export pending state. The titlebar disables the exact
/// action while it is in flight; the page keeps the value for its callers.
#[derive(Debug, Clone, Copy, Default)]
#[allow(dead_code)]
pub struct PlaylistTransferView {
    /// An import is in flight (dialog open or file being read/parsed).
    pub import_pending: bool,
    /// The playlist id whose export is in flight, if any.
    pub export_pending: Option<u64>,
}

pub fn playlists_view<'a>(
    library: &'a Library,
    playback: &PlaybackState,
    enz_theme: &'a EnzTheme,
    filter: LibraryFilter,
    transfer: PlaylistTransferView,
) -> Element<'a, Message> {
    let tc = theme::text_color(enz_theme);
    let sc = theme::secondary_text_color(enz_theme);

    let _ = transfer;
    let mut content = column![]
        .spacing(enz_theme.spacing.md())
        .padding(page_padding(enz_theme));

    if let LibraryFilter::Playlist(pid) = filter
        && let Some(pl) = library.playlists.iter().find(|p| p.id == pid)
    {
        let n = pl.track_ids.len();
        let total_secs: f64 = pl
            .track_ids
            .iter()
            .filter_map(|id| library.tracks.get(id))
            .map(|t| t.duration_secs)
            .sum();
        // Rename, Export and Delete are titlebar commands; the header names
        // the playlist and states its size.
        content = content.push(
            row![
                {
                    use enzui_widgets::surface::{RadiusSize, SurfaceTone, enz_surface};
                    enz_surface(
                        container(
                            enz_icon("list-music")
                                .size(enz_theme.sizing.icon_xl)
                                .tint(sc)
                                .into_element(),
                        )
                        .center_x(Length::Fixed(PLAYLIST_ART_EDGE))
                        .center_y(Length::Fixed(PLAYLIST_ART_EDGE)),
                    )
                    .tone(SurfaceTone::SurfaceAlt)
                    .radius_size(RadiusSize::Xl)
                    .width(PLAYLIST_ART_EDGE)
                    .height(PLAYLIST_ART_EDGE)
                    .into_element()
                },
                column![
                    enz_text(pl.name.as_str())
                        .size(TextSize::Title2)
                        .color(tc)
                        .full_width()
                        .single_line_elided()
                        .heading(HeadingLevel::Section),
                    enz_text(if n == 0 {
                        format_count(n, "track", "tracks")
                    } else {
                        format!(
                            "{} · {}",
                            format_count(n, "track", "tracks"),
                            format_duration(total_secs)
                        )
                    })
                    .size(TextSize::Footnote)
                    .color(sc),
                ]
                .spacing(enz_theme.spacing.xs())
                .width(Length::Fill),
            ]
            .spacing(enz_theme.spacing.lg())
            .align_y(Alignment::Center)
            .padding(
                Padding::new(0.0)
                    .left(layout::TrackRowLayout::PAD_L)
                    .right(layout::TrackRowLayout::PAD_L)
                    .bottom(enz_theme.spacing.sm()),
            ),
        );
        if pl.track_ids.is_empty() {
            content = content.push(empty_state(
                "list-music",
                "No tracks",
                "Add tracks from your library to fill this playlist.",
            ));
        } else {
            let mut list = column![].spacing(enz_theme.spacing.space0);
            for (i, &tid) in pl.track_ids.iter().enumerate() {
                if let Some(t) = library.tracks.get(&tid) {
                    let is_playing = playback.current_track_id == Some(tid);
                    list = list.push(track_row(
                        "Playlist",
                        i,
                        t,
                        is_playing,
                        library.availability_of(tid),
                        enz_theme,
                    ));
                }
            }
            content = content.push(list);
        }
        return crate::views::page_scroll(content);
    }

    if library.playlists.is_empty() {
        content = content.push(
            enz_status_page("No playlists yet")
                .icon("list-music")
                .description("Create your first playlist.")
                .variant(StatusPageVariant::Inline)
                .primary_action(
                    Action::new("music.create-playlist", Message::CreatePlaylist)
                        .with_label("New Playlist"),
                )
                .into_element(),
        );
        return crate::views::page_scroll(content);
    }

    content = content.push(
        container(
            enz_text(format_count(
                library.playlists.len(),
                "playlist",
                "playlists",
            ))
            .size(TextSize::Footnote)
            .color(sc),
        )
        .padding(Padding::new(0.0).left(layout::TrackRowLayout::PAD_L)),
    );
    let mut list = column![].spacing(enz_theme.spacing.space0);
    for pl in &library.playlists {
        let n_tracks = pl.track_ids.len();
        let pid = pl.id;
        let row_content = row![
            list_glyph("list-music", enz_theme),
            column![
                enz_text(pl.name.as_str())
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
        .spacing(layout::TrackRowLayout::SPACING)
        .align_y(Alignment::Center);
        // Ids key on the immutable playlist id, so a rename under a live menu
        // cannot move where focus returns on close.
        let pl_name = pl.name.clone();
        let playlist_row_id = crate::context_menu::playlist_row_id(pid);
        let pl_el: Element<'_, Message> = enz_list_row_surface(
            container(row_content).width(Length::Fill).padding(
                Padding::new(0.0)
                    .top(TRACK_ROW_PAD_Y)
                    .bottom(TRACK_ROW_PAD_Y),
            ),
            Message::SelectPlaylist(pid),
        )
        .content_padding(layout::TrackRowLayout::PAD_L)
        .outer_gutter(0.0)
        .content_height(true)
        .label(pl_name.clone())
        .id(playlist_row_id.clone())
        .context_id(crate::context_menu::context_target_id(&playlist_row_id))
        .on_context(move |at| Message::OpenContextMenu(ContextTarget::PlaylistRow(pid), at))
        .context_label(format!("{pl_name} actions"))
        .into();
        let pl_el =
            enzui_widgets::file_drop::enz_file_drop_target_with_receipt(pl_el, move |drop| {
                Message::FilesDropped(drop, crate::native_drop::Target::Playlist(pid))
            })
            .copy_only(true)
            .key(("music-playlist", pid));
        list = list.push(pl_el);
    }
    content = content.push(list);

    crate::views::page_scroll(content)
}

// ---------------------------------------------------------------------------
// Queue view
// ---------------------------------------------------------------------------

pub fn queue_view<'a>(
    library: &'a Library,
    playback: &PlaybackState,
    enz_theme: &'a EnzTheme,
    more_menu_model: &'a MenuModel,
    more_menu_state: &'a MenuState,
) -> Element<'a, Message> {
    // Save and Clear are titlebar commands, Close is the titlebar's back.
    let _ = (more_menu_model, more_menu_state);
    let tc = theme::text_color(enz_theme);
    let sc = theme::secondary_text_color(enz_theme);

    let mut content = column![]
        .spacing(enz_theme.spacing.space0)
        .padding(page_padding(enz_theme));

    if playback.queue.is_empty() {
        // Play All / Shuffle Library sit in the titlebar too; the status page
        // offers the shuffle where the eye lands.
        let mut page = enz_status_page("Nothing queued")
            .icon("list-music")
            .description("Play a track to build your up-next queue.")
            .variant(StatusPageVariant::Inline);
        if library.track_count() > 0 {
            page = page.primary_action(
                Action::new("music.queue-shuffle-library", Message::ShuffleLibrary)
                    .with_label("Shuffle Library"),
            );
        }
        content = content.push(page.into_element());
        return crate::views::page_scroll(content);
    }

    let total_secs: f64 = playback
        .queue
        .iter()
        .filter_map(|id| library.tracks.get(id))
        .map(|t| t.duration_secs)
        .sum();
    content = content.push(
        container(
            enz_text(format!(
                "{} · {}",
                format_queue_count_sentence(playback.queue.len()),
                format_duration(total_secs),
            ))
            .size(TextSize::Footnote)
            .color(sc),
        )
        .padding(
            Padding::new(0.0)
                .left(layout::TrackRowLayout::PAD_L)
                .bottom(enz_theme.spacing.md()),
        ),
    );

    // Entries reorder by drag and drop; a click plays one, right click opens
    // its menu, and the whole row lights on hover.
    let mut items: Vec<(String, Element<'a, Message>)> = Vec::with_capacity(playback.queue.len());
    for (pos, &tid) in playback.queue.iter().enumerate() {
        let Some(track) = library.tracks.get(&tid) else {
            continue;
        };
        let is_current = playback.current_track_id == Some(tid) && pos == playback.queue_index;
        // Focus and context target key on the occurrence id, so a reorder
        // cannot move them onto another appearance of the track.
        let entry_id = playback
            .entry_id_at(pos)
            .expect("queue_entry_ids stays in lockstep with queue");

        let mut info_col_q = column![
            enz_text(track.display_title().to_string())
                .size(TextSize::Body)
                .color(tc)
                .full_width()
                .single_line_elided(),
            enz_text(track.display_artist().to_string())
                .size(TextSize::Footnote)
                .color(sc)
                .full_width()
                .single_line_elided(),
        ]
        .spacing(enz_theme.spacing.space1)
        .width(Length::Fill);

        // The availability badge sits inside the Fill info column so the
        // fixed duration and action columns do not move.
        let entry_availability = library.availability_of(tid);
        if let Some(badge) = entry_availability.badge_label() {
            let mut availability_line =
                row![enz_badge(badge).tone(super::availability_badge_tone(entry_availability)),]
                    .spacing(enz_theme.spacing.space1)
                    .align_y(Alignment::Center);
            if let Some(disclosure) = entry_availability.disclosure() {
                availability_line = availability_line.push(
                    enz_text(disclosure)
                        .size(TextSize::Footnote)
                        .color(sc)
                        .full_width()
                        .single_line_elided(),
                );
            }
            info_col_q = info_col_q.push(availability_line.width(Length::Fill));
        }

        let duration_col_q = container(
            enz_text(track.duration_display())
                .size(TextSize::Footnote)
                .color(sc),
        )
        .align_right(layout::TrackRowLayout::DURATION_W)
        .center_y(Length::Shrink);

        // The playing entry shows a speaker mark in place of a badge line.
        let now_mark: Element<'a, Message> = if is_current {
            enz_icon("audio-lines")
                .size(enz_theme.sizing.icon_sm)
                .tint(theme::accent_color(enz_theme))
                .into_element()
        } else {
            Space::new()
                .width(enz_theme.sizing.icon_sm)
                .height(1)
                .into()
        };

        let entry_row = row![
            track_artwork(
                track.cover.as_ref(),
                format!("{} album art", track.display_album()),
                enz_theme,
            ),
            info_col_q,
            now_mark,
            duration_col_q,
            transport_icon_sized(
                "x",
                "Remove from queue",
                Message::RemoveFromQueue(pos),
                28.0,
                enzui_core::identity::derive_id(
                    &format!("music-queue-{}", entry_id.get()),
                    "remove"
                ),
                enz_theme,
            ),
        ]
        .align_y(Alignment::Center)
        .spacing(layout::TrackRowLayout::SPACING);

        let entry_title = track.display_title().to_string();
        let entry_el: Element<'a, Message> = enz_list_row_surface(
            container(entry_row).width(Length::Fill).padding(
                Padding::new(0.0)
                    .top(TRACK_ROW_PAD_Y)
                    .bottom(TRACK_ROW_PAD_Y),
            ),
            Message::JumpToQueueEntry(pos),
        )
        .selected(is_current)
        .content_padding(layout::TrackRowLayout::PAD_L)
        .outer_gutter(0.0)
        .content_height(true)
        .label(format!("Play {entry_title}"))
        // Keyed by occurrence id, distinct even for a repeated track.
        .id(crate::context_menu::queue_entry_id(entry_id))
        .context_id(crate::context_menu::context_target_id(
            &crate::context_menu::queue_entry_id(entry_id),
        ))
        // The target carries the occurrence id, so "Move to top" moves the
        // clicked entry even after the queue shifts under the open menu.
        .on_context(move |at| Message::OpenContextMenu(ContextTarget::QueueEntry(entry_id), at))
        .context_label(format!("{entry_title} actions"))
        .into();
        items.push((entry_id.get().to_string(), entry_el));
    }

    let list = enzui_widgets::reorder_list::enz_reorder_list(items, |request| {
        let entry = request
            .items
            .first()
            .and_then(|item| item.0.parse::<u64>().ok())
            .unwrap_or(0);
        Message::ReorderQueue {
            entry,
            slot: request.slot.index(),
        }
    })
    .collection_id("music-up-next")
    .drag_through_children(true)
    .into_element();
    content = content.push(list);

    crate::views::page_scroll(content)
}
