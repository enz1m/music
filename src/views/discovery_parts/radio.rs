//! Radio view and podcast view with episode lists.

use super::podcasts::{podcast_art_thumb, podcast_discovery_section};
use super::*;
use iced::widget::column;

/// Stable widget id of the Search view's search field. See [`search_input`].
pub const SEARCH_FIELD_ID: &str = "music-search-field";

// Radio view

/// The Search tab's field placeholder.
pub const RADIO_SEARCH_PLACEHOLDER: &str = "Search all stations by name";

/// The favourite toggle's accessible name when the station is not starred.
pub const RADIO_ADD_FAVORITE_LABEL: &str = "Add to favorites";
/// The favourite toggle's accessible name when the station is starred.
pub const RADIO_REMOVE_FAVORITE_LABEL: &str = "Remove from favorites";

/// The favourite toggle's accessible name for a station in a given state.
pub fn radio_favorite_label(is_favorite: bool) -> &'static str {
    if is_favorite {
        RADIO_REMOVE_FAVORITE_LABEL
    } else {
        RADIO_ADD_FAVORITE_LABEL
    }
}

/// Compose Radio browsing with the current source's confirmed transport caption.
pub fn radio_view<'a>(
    radio: &'a crate::radio::RadioState,
    charts: &'a crate::PodcastCharts,
    playback: &PlaybackState,
    art_cache: &'a crate::podcasts::artwork::PodcastArtworkCache,
    width: f32,
    enz_theme: &'a EnzTheme,
) -> Element<'a, Message> {
    use crate::radio::{GENRES, RadioCatalogState};
    use enzui_widgets::text_field::enz_text_field;
    let tc = theme::text_color(enz_theme);
    let sc = theme::secondary_text_color(enz_theme);
    let grid_w = (width - 2.0 * enz_theme.spacing.lg()).max(240.0);
    let mut content = column![]
        .spacing(enz_theme.spacing.lg())
        .padding(page_padding(enz_theme));

    // Search every station, always at hand.
    let field: Element<'_, Message> =
        enz_search_field(radio.query.as_str(), Message::RadioSearchChanged)
            .placeholder(RADIO_SEARCH_PLACEHOLDER)
            .on_submit(Message::RadioSearchSubmit)
            .on_clear(Message::RadioSearchClear)
            .size(TextFieldSize::Md)
            .palette(enz_theme.palette.clone())
            .width(Length::Fill)
            .id(enzui_core::identity::derive_id(RADIO_SEARCH_FIELD_ID, ""))
            .into();
    content = content.push(field);

    if let Some(query) = &radio.submitted {
        let mut section = column![
            enz_text(format!("Stations matching \u{201c}{query}\u{201d}"))
                .size(TextSize::Title3)
                .color(tc)
                .heading(HeadingLevel::Section),
        ]
        .spacing(enz_theme.spacing.sm());
        match &radio.results {
            None => {
                section = section.push(
                    container(enzui_widgets::spinner::enz_spinner())
                        .center_x(Length::Fill)
                        .padding(enz_theme.spacing.lg()),
                );
            }
            Some(results) if results.is_empty() => {
                section = section.push(
                    enz_text("No stations found. Try another name.")
                        .size(TextSize::Footnote)
                        .color(sc),
                );
            }
            Some(results) => {
                for station in results {
                    section =
                        section.push(station_row(radio, station, playback, art_cache, enz_theme));
                }
            }
        }
        content = content.push(section);
        return crate::views::page_scroll(content);
    }

    // The user's stations as cards.
    let yours = radio.yours();
    if !yours.is_empty() {
        content = content.push(
            column![
                enz_text("Your stations")
                    .size(TextSize::Title3)
                    .color(tc)
                    .heading(HeadingLevel::Section),
                podcast_grid(
                    yours
                        .into_iter()
                        .map(|station| station_card(station, playback, art_cache, enz_theme))
                        .collect(),
                    grid_w,
                ),
            ]
            .spacing(enz_theme.spacing.sm()),
        );
    }

    // Popular stations in the country, by genre.
    let title = match crate::podcasts::charts::country_name(&charts.country) {
        Some(name) => format!("Popular stations in {name}"),
        None => "Popular stations".to_owned(),
    };
    let mut section = column![
        row![
            enz_text(title)
                .size(TextSize::Title3)
                .color(tc)
                .heading(HeadingLevel::Section)
                .width(Length::Fill),
            enz_button("Change country", Message::PodcastCountryPicker)
                .icon("globe")
                .kind(ButtonKind::Quiet)
                .size(ButtonSize::Sm)
                .id(enzui_core::identity::derive_id("music-radio", "country"))
                .into_element(),
        ]
        .align_y(Alignment::Center),
    ]
    .spacing(enz_theme.spacing.sm())
    .width(Length::Fill);
    if charts.picking_country {
        section = section.push(country_picker(&charts.country, enz_theme));
    }
    let chip = |label: &'static str, genre: Option<&'static str>| -> Element<'a, Message> {
        enz_button(label, Message::RadioGenre(genre))
            .kind(if radio.genre == genre {
                ButtonKind::Accent
            } else {
                ButtonKind::Quiet
            })
            .size(ButtonSize::Sm)
            .id(enzui_core::identity::derive_id(
                "music-radio-genre",
                genre.unwrap_or("all"),
            ))
            .into_element()
    };
    let mut chips = row![chip("All", None)].spacing(enz_theme.spacing.xs());
    for (tag, label) in GENRES {
        chips = chips.push(chip(label, Some(tag)));
    }
    section = section.push(chips.wrap().vertical_spacing(enz_theme.spacing.xs()));
    match radio.browse(&charts.country) {
        Some(stations) if stations.is_empty() => {
            section = section.push(
                enz_text("Radio Browser lists no stations here that Music can play.")
                    .size(TextSize::Footnote)
                    .color(sc),
            );
        }
        Some(stations) => {
            for station in stations {
                section = section.push(station_row(radio, station, playback, art_cache, enz_theme));
            }
        }
        None => match &radio.catalog_status {
            RadioCatalogState::Failed { message, .. } if radio.loading.is_none() => {
                section = section.push(
                    row![
                        enz_text(message.as_str())
                            .size(TextSize::Footnote)
                            .color(sc)
                            .width(Length::Fill),
                        enz_button(PODCAST_RETRY_LABEL, Message::RadioCatalogRetry)
                            .kind(ButtonKind::Quiet)
                            .size(ButtonSize::Sm)
                            .into_element(),
                    ]
                    .align_y(Alignment::Center),
                );
            }
            _ => {
                section = section.push(
                    container(enzui_widgets::spinner::enz_spinner())
                        .center_x(Length::Fill)
                        .padding(enz_theme.spacing.lg()),
                );
            }
        },
    }
    content = content.push(section);

    // A station Radio Browser doesn't list.
    let can_add = !radio.add_url.trim().is_empty();
    content = content.push(
        column![
            enz_text("Add a station by its stream address")
                .size(TextSize::Headline)
                .color(tc),
            row![
                enz_text_field(
                    "Name",
                    "Station name (optional)",
                    radio.add_name.as_str(),
                    Message::RadioAddName
                )
                .size(TextFieldSize::Md),
                enz_text_field(
                    "Stream",
                    "https://… stream address",
                    radio.add_url.as_str(),
                    Message::RadioAddUrl
                )
                .on_submit(Message::RadioAddSubmit)
                .size(TextFieldSize::Md),
                enz_button("Add", Message::RadioAddSubmit)
                    .kind(ButtonKind::Accent)
                    .size(ButtonSize::Md)
                    .enabled(can_add)
                    .id(enzui_core::identity::derive_id("music-radio", "add"))
                    .into_element(),
            ]
            .spacing(enz_theme.spacing.sm())
            .align_y(Alignment::End),
        ]
        .spacing(enz_theme.spacing.sm()),
    );
    crate::views::page_scroll(content)
}

/// The country list both Podcasts and Radio use.
fn country_picker<'a>(selected: &str, enz_theme: &'a EnzTheme) -> Element<'a, Message> {
    use crate::podcasts::charts::COUNTRIES;
    let mut countries = row![].spacing(enz_theme.spacing.xs());
    for (code, name) in COUNTRIES {
        countries = countries.push(
            enz_button(
                name.trim_start_matches("the "),
                Message::PodcastChartsCountry(code.to_owned()),
            )
            .kind(if selected == code {
                ButtonKind::Accent
            } else {
                ButtonKind::Quiet
            })
            .size(ButtonSize::Sm)
            .id(enzui_core::identity::derive_id("music-country", code))
            .into_element(),
        );
    }
    enzui_widgets::surface::enz_surface(countries.wrap().vertical_spacing(enz_theme.spacing.xs()))
        .tone(enzui_widgets::surface::SurfaceTone::SurfaceAlt)
        .padding(enz_theme.spacing.sm())
        .border_width(0.0)
        .width(Length::Fill)
        .into_element()
}

/// A station's logo, whole, on a quiet square.
fn station_cover<'a>(
    art: Option<&crate::art::CoverArt>,
    enz_theme: &EnzTheme,
) -> Element<'a, Message> {
    use enzui_widgets::aspect_ratio::enz_aspect_ratio;
    use enzui_widgets::surface::{SurfaceTone, enz_surface};
    let inner: Element<'a, Message> = match art {
        Some(cover) => container(
            enz_image_handle(cover.handle().clone())
                .fit(EnzImageFit::Contain)
                .alt_text("Station logo")
                .width(Length::Fill)
                .height(Length::Fill)
                .into_element(),
        )
        .padding(enz_theme.spacing.md())
        .into(),
        None => container(
            enz_icon("radio")
                .size(enz_theme.sizing.icon_xl)
                .tint(theme::secondary_text_color(enz_theme))
                .into_element(),
        )
        .center_x(Length::Fill)
        .center_y(Length::Fill)
        .into(),
    };
    enz_aspect_ratio(
        enz_surface(inner)
            .tone(SurfaceTone::SurfaceAlt)
            .radius(enz_theme.radius.lg)
            .border_width(0.0)
            .width(Length::Fill)
            .height(Length::Fill)
            .into_element(),
        1.0,
    )
    .into()
}

/// One of the user's stations as a card: its logo, name and where it's
/// from; it tunes the station.
fn station_card<'a>(
    station: &'a crate::radio::RadioStation,
    playback: &PlaybackState,
    art_cache: &'a crate::podcasts::artwork::PodcastArtworkCache,
    enz_theme: &'a EnzTheme,
) -> Element<'a, Message> {
    let tc = theme::text_color(enz_theme);
    let sc = theme::secondary_text_color(enz_theme);
    let id = station.id;
    let is_current = matches!(playback.stream_selection(), Some(StreamSelection::Station(current)) if *current == id);
    let detail = if is_current {
        transport_status_label(playback)
            .unwrap_or("Tuned")
            .to_owned()
    } else if !station.country.is_empty() {
        station.country.clone()
    } else {
        station.tags.clone()
    };
    let radio_row_id = crate::context_menu::radio_row_id(&station.remote_uuid);
    let station_name = station.name.clone();
    let card = enz_pressable(
        column![
            station_cover(
                art_cache.channel_art(&crate::update::radio_art_key(&station.remote_uuid)),
                enz_theme
            ),
            enz_text(station.name.as_str())
                .size(TextSize::Body)
                .color(if is_current {
                    theme::accent_color(enz_theme)
                } else {
                    tc
                })
                .full_width()
                .single_line_elided(),
            enz_text(detail)
                .size(TextSize::Caption)
                .color(sc)
                .full_width()
                .single_line_elided(),
        ]
        .spacing(enz_theme.spacing.xs())
        .into(),
        Message::RadioTune(id),
    )
    .style(PressableStyle::Plain)
    .padding(enz_theme.spacing.xs())
    .aria_label(format!("Play {}", station.name))
    .id(radio_row_id.clone())
    .into_element();
    enz_context_target(card, move |at| {
        Message::OpenContextMenu(ContextTarget::RadioStation(id), at)
    })
    .id(crate::context_menu::context_target_id(&radio_row_id))
    .label(format!("{station_name} actions"))
    .height(Length::Shrink)
    .into()
}

/// One station as a list row: logo, name, where and how it streams, its
/// genres, and the star.
fn station_row<'a>(
    radio: &'a crate::radio::RadioState,
    station: &'a crate::radio::RadioStation,
    playback: &PlaybackState,
    art_cache: &'a crate::podcasts::artwork::PodcastArtworkCache,
    enz_theme: &'a EnzTheme,
) -> Element<'a, Message> {
    use enzui_widgets::toolbar_icon::enz_toolbar_icon;
    let tc = theme::text_color(enz_theme);
    let sc = theme::secondary_text_color(enz_theme);
    let id = station.id;
    let is_fav = radio.is_favorite(id);
    let is_current = matches!(playback.stream_selection(), Some(StreamSelection::Station(current)) if *current == id);
    let status = is_current
        .then(|| transport_status_label(playback))
        .flatten();
    let glyph: Element<'a, Message> =
        match art_cache.channel_art(&crate::update::radio_art_key(&station.remote_uuid)) {
            Some(cover) => station_logo(cover, enz_theme),
            None => super::super::library::list_glyph("radio", enz_theme),
        };
    // Where and how it streams: country, codec, bitrate.
    let mut facts: Vec<String> = Vec::new();
    if !station.country.is_empty() {
        facts.push(station.country.clone());
    }
    if !station.codec.is_empty() {
        facts.push(station.codec.clone());
    }
    if station.bitrate > 0 {
        facts.push(format!("{} kbps", station.bitrate));
    }
    let mut meta = column![
        enz_text(station.name.as_str())
            .size(TextSize::Body)
            .color(tc)
            .full_width()
            .single_line_elided(),
    ]
    .spacing(enz_theme.spacing.space1)
    .width(Length::Fill);
    if !facts.is_empty() {
        meta = meta.push(
            enz_text(facts.join(" \u{b7} "))
                .size(TextSize::Caption)
                .color(sc)
                .full_width()
                .single_line_elided(),
        );
    }
    if !station.tags.is_empty() {
        meta = meta.push(
            enz_text(station.tags.replace(',', " \u{b7} "))
                .size(TextSize::Caption)
                .color(sc)
                .full_width()
                .single_line_elided(),
        );
    }
    let fav_label = radio_favorite_label(is_fav);
    let fav_btn = enz_toolbar_icon(
        if is_fav { "star.fill" } else { "star" },
        Message::RadioToggleFavorite(id),
    )
    .aria_label(fav_label)
    .tooltip(fav_label)
    .id(enzui_core::identity::derive_id(
        RADIO_FAVORITE_ID,
        &id.to_string(),
    ))
    .into_element();
    let phase = is_current.then_some(&playback.stream_phase);
    let trailing = stream_status_mark(phase, playback.playing, enz_theme);
    let mut row_content = row![glyph, meta]
        .spacing(layout::TrackRowLayout::SPACING)
        .align_y(Alignment::Center);
    if let Some(mark) = trailing {
        row_content = row_content.push(mark);
    }
    let row_content = row_content.push(fav_btn);
    let status = match phase {
        Some(crate::StreamPhase::Loading) => Some("Connecting"),
        Some(crate::StreamPhase::Failed(_)) => Some("Can't play"),
        _ => status,
    };
    let row_label = stream_row_label(&station.name, status);
    let station_name = station.name.clone();
    let radio_row_id = crate::context_menu::radio_row_id(&station.remote_uuid);
    enz_list_row_surface(
        container(row_content)
            .padding(
                Padding::new(0.0)
                    .top(TRACK_ROW_PAD_Y)
                    .bottom(TRACK_ROW_PAD_Y),
            )
            .width(Length::Fill),
        Message::RadioTune(id),
    )
    .selected(is_current)
    .content_padding(layout::TrackRowLayout::PAD_L)
    .outer_gutter(0.0)
    .content_height(true)
    .label(row_label)
    .id(radio_row_id.clone())
    .context_id(crate::context_menu::context_target_id(&radio_row_id))
    .on_context(move |at| Message::OpenContextMenu(ContextTarget::RadioStation(id), at))
    .context_label(format!("{station_name} actions"))
    .into()
}

// Status changes the accessible name, never the durable widget/focus identity.
fn stream_row_label(name: &str, status: Option<&str>) -> String {
    if let Some(status) = status {
        format!("{name} — {status}")
    } else {
        name.to_string()
    }
}

/// The Radio search field's routable id.
pub const RADIO_SEARCH_FIELD_ID: &str = "music-radio-search";
/// The per-row favourite toggle's routable id prefix, suffixed with the
/// station id.
pub const RADIO_FAVORITE_ID: &str = "music-radio-favorite";

// Podcasts view

/// Compose the podcasts surface: header, search, subscribe field and
/// subscriptions list.
///
/// `Manage Feeds` dispatches [`Message::OpenManageFeeds`] to raise
/// [`manage_feeds_overlay`] for OPML import and export. Feed fetching happens
/// in the reducer; `playback` marks the selected episode's row.
#[allow(clippy::too_many_arguments)]
pub fn podcasts_view<'a>(
    subscriptions: &'a [crate::podcasts::PodcastFeed],
    subscribe_input: &'a str,
    search_query: &'a str,
    search_state: &'a crate::PodcastSearchState,
    charts: &'a crate::PodcastCharts,
    art_cache: &'a crate::podcasts::artwork::PodcastArtworkCache,
    playback: &PlaybackState,
    width: f32,
    enz_theme: &'a EnzTheme,
) -> Element<'a, Message> {
    use enzui_widgets::text_field::enz_text_field;

    // An opened podcast has its own page.
    if let Some(feed) = charts.opened_feed(subscriptions) {
        let subscribed_at = subscriptions
            .iter()
            .position(|f| f.feed_url == feed.feed_url);
        return podcast_page(
            feed,
            subscribed_at,
            charts.episodes_shown,
            art_cache,
            playback,
            enz_theme,
        );
    }

    let tc = theme::text_color(enz_theme);
    let sc = theme::secondary_text_color(enz_theme);
    let grid_w = (width - 2.0 * enz_theme.spacing.lg()).max(240.0);

    let mut content = column![]
        .spacing(enz_theme.spacing.lg())
        .padding(page_padding(enz_theme));

    // Keyless discovery search; the URL field below is the fallback.
    content = content.push(podcast_discovery_section(
        search_query,
        search_state,
        subscriptions,
        art_cache,
        enz_theme,
    ));

    let subscribe_field: Element<'_, Message> = enz_text_field(
        "Feed URL",
        "Paste an RSS feed URL",
        subscribe_input,
        Message::PodcastSubscribeInputChanged,
    )
    .on_submit(Message::PodcastSubscribeSubmit)
    .size(TextFieldSize::Md)
    .into();
    let normalized_input = subscribe_input.trim();
    let can_subscribe = !normalized_input.is_empty()
        && !subscriptions
            .iter()
            .any(|feed| feed.feed_url == normalized_input);
    let subscribe_button: Element<'_, Message> =
        enz_button("Subscribe", Message::PodcastSubscribeSubmit)
            .id(enzui_core::identity::derive_id(
                PODCAST_SUBSCRIBE_BUTTON_ID,
                "",
            ))
            .kind(ButtonKind::Accent)
            .size(ButtonSize::Md)
            .enabled(can_subscribe)
            .into();
    // Pointer and Enter share one reducer message; the disabled state exposes
    // blank and duplicate submissions without a second validation path.
    // Adding by URL and importing sit at the foot: browsing comes first.
    let add_by_url: Element<'a, Message> = column![
        enz_text("Add a podcast by its RSS feed")
            .size(TextSize::Headline)
            .color(tc),
        row![subscribe_field, subscribe_button]
            .spacing(enz_theme.spacing.sm())
            .align_y(Alignment::End),
        row![
            enz_text(
                "Moving from another podcast app? Export your subscriptions there as OPML, then import the file."
            )
            .size(TextSize::Footnote)
            .color(sc)
            .width(Length::Fill)
            .wrap(true),
            enz_button("Import subscriptions", Message::OpmlImportFile)
                .icon("file-up")
                .kind(ButtonKind::Quiet)
                .size(ButtonSize::Sm)
                .id(enzui_core::identity::derive_id("music-podcasts", "import-opml"))
                .into_element(),
        ]
        .spacing(enz_theme.spacing.sm())
        .align_y(Alignment::Center),
    ]
    .spacing(enz_theme.spacing.sm())
    .into();

    if !subscriptions.is_empty() {
        content = content.push(
            column![
                enz_text("Your podcasts")
                    .size(TextSize::Title3)
                    .color(tc)
                    .heading(HeadingLevel::Section),
                podcast_grid(
                    subscriptions
                        .iter()
                        .map(|feed| subscribed_card(feed, art_cache, enz_theme))
                        .collect(),
                    grid_w,
                ),
            ]
            .spacing(enz_theme.spacing.sm()),
        );
    }

    content = content.push(podcast_charts_section(
        charts,
        subscriptions,
        art_cache,
        grid_w,
        enz_theme,
    ));
    content = content.push(add_by_url);
    crate::views::page_scroll_tracked(
        content,
        crate::podcast_discover::podcasts_scroll_id(),
        Message::PodcastsScrolled,
    )
}

/// Apple's top podcasts for the desktop's country, overall or by genre, as
/// cards with Subscribe.
fn podcast_charts_section<'a>(
    charts: &'a crate::PodcastCharts,
    subscriptions: &'a [crate::podcasts::PodcastFeed],
    art_cache: &'a crate::podcasts::artwork::PodcastArtworkCache,
    width: f32,
    enz_theme: &'a EnzTheme,
) -> Element<'a, Message> {
    use crate::podcasts::charts::{GENRES, country_name};
    let tc = theme::text_color(enz_theme);
    let sc = theme::secondary_text_color(enz_theme);
    let title = match country_name(&charts.country) {
        Some(name) => format!("Top podcasts in {name}"),
        None => "Top podcasts".to_owned(),
    };
    let chip = |label: &'static str, genre: Option<u32>| -> Element<'a, Message> {
        let selected = charts.genre == genre;
        enz_button(label, Message::PodcastChartsGenre(genre))
            .kind(if selected {
                ButtonKind::Accent
            } else {
                ButtonKind::Quiet
            })
            .size(ButtonSize::Sm)
            .id(enzui_core::identity::derive_id(
                "music-podcast-genre",
                &genre.map_or_else(|| "all".to_owned(), |g| g.to_string()),
            ))
            .into_element()
    };
    let mut chips = row![chip("All", None)].spacing(enz_theme.spacing.xs());
    for (id, label) in GENRES {
        chips = chips.push(chip(label, Some(id)));
    }
    let heading = row![
        enz_text(title)
            .size(TextSize::Title3)
            .color(tc)
            .heading(HeadingLevel::Section)
            .width(Length::Fill),
        enz_button("Change country", Message::PodcastCountryPicker)
            .icon("globe")
            .kind(ButtonKind::Quiet)
            .size(ButtonSize::Sm)
            .id(enzui_core::identity::derive_id("music-podcasts", "country"))
            .into_element(),
    ]
    .align_y(Alignment::Center);
    let mut section = column![heading]
        .spacing(enz_theme.spacing.sm())
        .width(Length::Fill);
    if charts.picking_country {
        section = section.push(country_picker(&charts.country, enz_theme));
    }
    section = section.push(chips.wrap().vertical_spacing(enz_theme.spacing.xs()));

    match charts.current() {
        Some(shows) if !shows.is_empty() => {
            section = section.push(podcast_grid(
                shows
                    .iter()
                    .map(|show| podcast_chart_card(show, subscriptions, art_cache, enz_theme))
                    .collect(),
                width,
            ));
        }
        Some(_) => {
            section = section.push(
                enz_text("Apple lists no podcasts here for your country.")
                    .size(TextSize::Footnote)
                    .color(sc),
            );
        }
        None => match &charts.failed {
            Some(message) => {
                section = section.push(
                    row![
                        enz_text(message.as_str())
                            .size(TextSize::Footnote)
                            .color(sc)
                            .width(Length::Fill),
                        enz_button(
                            PODCAST_RETRY_LABEL,
                            Message::PodcastChartsGenre(charts.genre)
                        )
                        .kind(ButtonKind::Quiet)
                        .size(ButtonSize::Sm)
                        .into_element(),
                    ]
                    .align_y(Alignment::Center),
                );
            }
            None => {
                section = section.push(
                    container(enzui_widgets::spinner::enz_spinner())
                        .center_x(Length::Fill)
                        .padding(enz_theme.spacing.lg()),
                );
            }
        },
    }
    section.into()
}

/// One chart card: the cover, title, author and Subscribe.
fn podcast_chart_card<'a>(
    show: &'a crate::podcasts::search::PodcastSearchResult,
    subscriptions: &'a [crate::podcasts::PodcastFeed],
    art_cache: &'a crate::podcasts::artwork::PodcastArtworkCache,
    enz_theme: &'a EnzTheme,
) -> Element<'a, Message> {
    use enzui_widgets::aspect_ratio::enz_aspect_ratio;
    let tc = theme::text_color(enz_theme);
    let sc = theme::secondary_text_color(enz_theme);
    let subscribed = subscriptions.iter().any(|f| f.feed_url == show.feed_url);
    let art: Element<'a, Message> = match art_cache.channel_art(&show.feed_url) {
        Some(cover) => enz_aspect_ratio(
            enz_image_handle(cover.handle().clone())
                .fit(EnzImageFit::Cover)
                .radius(enz_theme.radius.lg)
                .alt_text(format!("{} artwork", show.title))
                .width(Length::Fill)
                .height(Length::Fill)
                .into_element(),
            1.0,
        )
        .into(),
        None => {
            use enzui_widgets::surface::{SurfaceTone, enz_surface};
            enz_aspect_ratio(
                enz_surface(
                    container(
                        enz_icon("podcast")
                            .size(enz_theme.sizing.icon_xl)
                            .tint(sc)
                            .into_element(),
                    )
                    .center_x(Length::Fill)
                    .center_y(Length::Fill),
                )
                .tone(SurfaceTone::SurfaceAlt)
                .radius(enz_theme.radius.lg)
                .border_width(0.0)
                .width(Length::Fill)
                .height(Length::Fill)
                .into_element(),
                1.0,
            )
            .into()
        }
    };
    let opens = enz_pressable(
        column![
            art,
            enz_text(show.title.as_str())
                .size(TextSize::Body)
                .color(tc)
                .full_width()
                .single_line_elided(),
            enz_text(show.author.as_str())
                .size(TextSize::Caption)
                .color(sc)
                .full_width()
                .single_line_elided(),
        ]
        .spacing(enz_theme.spacing.xs())
        .into(),
        Message::PodcastOpen(show.feed_url.clone()),
    )
    .style(PressableStyle::Plain)
    .padding(0.0)
    .aria_label(format!("Open {}", show.title))
    .id(enzui_core::identity::derive_id(
        "music-podcast-chart-open",
        &show.feed_url,
    ))
    .into_element();
    column![
        opens,
        enz_button(
            if subscribed {
                "Subscribed"
            } else {
                "Subscribe"
            },
            Message::PodcastSubscribeFromResult(show.feed_url.clone()),
        )
        .icon(if subscribed { "check" } else { "plus" })
        .kind(ButtonKind::Quiet)
        .size(ButtonSize::Sm)
        .enabled(!subscribed)
        .id(enzui_core::identity::derive_id(
            "music-podcast-chart-subscribe",
            &show.feed_url,
        ))
        .into_element(),
    ]
    .spacing(enz_theme.spacing.xs())
    .width(Length::Fill)
    .into()
}

/// The episode rows under one loaded feed, or
/// `podcasts::feed_content_empty_label` when nothing is playable, so a
/// text-only feed reads as empty rather than broken.
fn feed_episode_list<'a>(
    feed: &'a crate::podcasts::PodcastFeed,
    limit: usize,
    art_cache: &'a crate::podcasts::artwork::PodcastArtworkCache,
    playback: &PlaybackState,
    enz_theme: &'a EnzTheme,
) -> Element<'a, Message> {
    let sc = theme::secondary_text_color(enz_theme);
    let mut list = column![].spacing(enz_theme.spacing.space1);

    if feed.playable_count() == 0 {
        list = list.push(
            enz_text(crate::podcasts::feed_content_empty_label(true))
                .size(TextSize::Footnote)
                .color(sc),
        );
        return container(list)
            .padding(Padding::new(0.0).left(24.0))
            .width(Length::Fill)
            .into();
    }

    // A page of episodes at a time: some shows have thousands.
    let total = feed.playable_count();
    for episode in feed.playable_episodes().take(limit) {
        let is_current = playback.is_current_episode(&feed.feed_url, &episode.guid);
        let status = is_current
            .then(|| transport_status_label(playback))
            .flatten();
        let mut meta = column![
            enz_text(episode.title.as_str())
                .size(TextSize::Body)
                .color(theme::text_color(enz_theme)),
        ]
        .spacing(enz_theme.spacing.space1)
        .width(Length::Fill);
        // Date and duration joined by `·`, each only when the feed declares it.
        let mut date_bits: Vec<String> = Vec::new();
        if !episode.published.trim().is_empty() {
            date_bits.push(crate::podcasts::local_date(&episode.published, false));
        }
        if let Some(duration) = crate::podcasts::format_duration(episode.duration_secs) {
            date_bits.push(duration);
        }
        if !date_bits.is_empty() {
            meta = meta.push(
                enz_text(date_bits.join(" · "))
                    .size(TextSize::Caption)
                    .color(sc),
            );
        }
        // Feeds carry HTML show notes; a short plain summary reads better.
        let summary = crate::podcasts::plain_summary(&episode.description, 240);
        if !summary.is_empty() {
            meta = meta.push(
                enz_text(summary)
                    .size(TextSize::Caption)
                    .color(sc)
                    .width(Length::Fill),
            );
        }
        // The current episode's state sits at the row's end, as on Radio.
        let phase = is_current.then_some(&playback.stream_phase);
        let trailing = stream_status_mark(phase, playback.playing, enz_theme);
        let status = match phase {
            Some(crate::StreamPhase::Loading) => Some("Connecting"),
            Some(crate::StreamPhase::Failed(_)) => Some("Can't play"),
            _ => status,
        };
        let row_label = stream_row_label(&episode.title, status);
        let episode_title = episode.title.clone();
        // Durable key for the context menu, re-resolved against the live feeds.
        let feed_url = feed.feed_url.clone();
        let guid = episode.guid.clone();
        // Focus and context ids use (feed_url, guid), so a "Now playing" caption
        // cannot move the restore target.
        let podcast_row_id =
            crate::context_menu::podcast_episode_row_id(&feed.feed_url, &episode.guid);
        // Episode artwork, falling back to the channel cover, leads the row.
        let row_el: Element<'_, Message> = enz_list_row_surface(
            container(
                row![
                    podcast_art_thumb(
                        art_cache.episode_art(&feed.feed_url, &episode.guid),
                        48.0,
                        enz_theme,
                    ),
                    meta,
                ]
                .extend(trailing)
                .spacing(enz_theme.spacing.md())
                .align_y(Alignment::Start),
            )
            .padding(Padding::new(6.0).left(12.0).right(12.0))
            .width(Length::Fill),
            Message::PodcastEpisodeSelect(feed.feed_url.clone(), episode.guid.clone()),
        )
        .content_padding(0.0)
        .label(row_label)
        .content_height(true)
        .id(podcast_row_id.clone())
        .context_id(crate::context_menu::context_target_id(&podcast_row_id))
        // One-row menu keyed by (feed_url, guid), inert if a refresh drops it.
        .on_context(move |at| Message::OpenPodcastEpisodeMenu(feed_url.clone(), guid.clone(), at))
        .context_label(format!("{episode_title} actions"))
        .into();
        list = list.push(row_el);
    }
    if total > limit {
        list = list.push(
            container(
                enz_button(
                    format!("Show more episodes ({} more)", total - limit),
                    Message::PodcastShowMoreEpisodes,
                )
                .kind(ButtonKind::Quiet)
                .size(ButtonSize::Sm)
                .id(enzui_core::identity::derive_id(
                    "music-podcast-more",
                    &feed.feed_url,
                ))
                .into_element(),
            )
            .center_x(Length::Fill),
        );
    }

    container(list).width(Length::Fill).into()
}

/// A station's logo in the row's leading tile.
/// Cards in as many columns as fit `width`.
fn podcast_grid(cards: Vec<Element<'_, Message>>, width: f32) -> Element<'_, Message> {
    const CARD_MIN: f32 = 150.0;
    const GAP: f32 = 12.0;
    let columns = (((width + GAP) / (CARD_MIN + GAP)).floor() as usize).clamp(2, 8);
    let mut grid = column![].spacing(GAP);
    let mut cards = cards.into_iter().peekable();
    while cards.peek().is_some() {
        let mut line = row![].spacing(GAP);
        for _ in 0..columns {
            line = line.push(match cards.next() {
                Some(card) => container(card).width(Length::FillPortion(1)),
                None => container(Space::new()).width(Length::FillPortion(1)),
            });
        }
        grid = grid.push(line);
    }
    grid.into()
}

/// A podcast cover filling its card's width, or the podcast glyph.
fn podcast_cover<'a>(
    art: Option<&crate::art::CoverArt>,
    enz_theme: &EnzTheme,
) -> Element<'a, Message> {
    use enzui_widgets::aspect_ratio::enz_aspect_ratio;
    let inner: Element<'a, Message> = match art {
        Some(cover) => enz_image_handle(cover.handle().clone())
            .fit(EnzImageFit::Cover)
            .radius(enz_theme.radius.lg)
            .alt_text("Podcast artwork")
            .width(Length::Fill)
            .height(Length::Fill)
            .into_element(),
        None => {
            use enzui_widgets::surface::{SurfaceTone, enz_surface};
            enz_surface(
                container(
                    enz_icon("podcast")
                        .size(enz_theme.sizing.icon_xl)
                        .tint(theme::secondary_text_color(enz_theme))
                        .into_element(),
                )
                .center_x(Length::Fill)
                .center_y(Length::Fill),
            )
            .tone(SurfaceTone::SurfaceAlt)
            .radius(enz_theme.radius.lg)
            .border_width(0.0)
            .width(Length::Fill)
            .height(Length::Fill)
            .into_element()
        }
    };
    enz_aspect_ratio(inner, 1.0).into()
}

/// A subscribed podcast's card: cover, title and how it's doing; it opens
/// the podcast's page.
fn subscribed_card<'a>(
    feed: &'a crate::podcasts::PodcastFeed,
    art_cache: &'a crate::podcasts::artwork::PodcastArtworkCache,
    enz_theme: &'a EnzTheme,
) -> Element<'a, Message> {
    let tc = theme::text_color(enz_theme);
    let sc = theme::secondary_text_color(enz_theme);
    let title = if feed.title.is_empty() {
        feed.feed_url.as_str()
    } else {
        feed.title.as_str()
    };
    let (caption, color) = match &feed.status {
        crate::podcasts::FeedStatus::Loading => (
            crate::podcasts::feed_loading_phase_label(crate::podcasts::FeedLoadingPhase::Loading)
                .to_owned(),
            sc,
        ),
        crate::podcasts::FeedStatus::Failed(_) => {
            ("Couldn't load".to_owned(), theme::danger_color(enz_theme))
        }
        crate::podcasts::FeedStatus::Loaded => (
            crate::podcasts::format_episode_count(feed.playable_count()),
            sc,
        ),
    };
    enz_pressable(
        column![
            podcast_cover(art_cache.channel_art(&feed.feed_url), enz_theme),
            enz_text(title)
                .size(TextSize::Body)
                .color(tc)
                .full_width()
                .single_line_elided(),
            enz_text(caption)
                .size(TextSize::Caption)
                .color(color)
                .full_width()
                .single_line_elided(),
        ]
        .spacing(enz_theme.spacing.xs())
        .into(),
        Message::PodcastOpen(feed.feed_url.clone()),
    )
    .style(PressableStyle::Plain)
    .padding(enz_theme.spacing.xs())
    .aria_label(format!("Open {title}"))
    .id(enzui_core::identity::derive_id(
        "music-podcast-open",
        &feed.feed_url,
    ))
    .into_element()
}

/// One podcast's page: its cover and details, Subscribe or Unsubscribe, and
/// its episodes, a page at a time.
fn podcast_page<'a>(
    feed: &'a crate::podcasts::PodcastFeed,
    subscribed_at: Option<usize>,
    episodes_shown: usize,
    art_cache: &'a crate::podcasts::artwork::PodcastArtworkCache,
    playback: &PlaybackState,
    enz_theme: &'a EnzTheme,
) -> Element<'a, Message> {
    let tc = theme::text_color(enz_theme);
    let sc = theme::secondary_text_color(enz_theme);
    let title = if feed.title.is_empty() {
        feed.feed_url.as_str()
    } else {
        feed.title.as_str()
    };
    const ART: f32 = 168.0;
    let cover: Element<'a, Message> = match art_cache.channel_art(&feed.feed_url) {
        Some(cover) => enz_image_handle(cover.handle().clone())
            .fit(EnzImageFit::Cover)
            .radius(enz_theme.radius.lg)
            .alt_text(format!("{title} artwork"))
            .width(Length::Fixed(ART))
            .height(Length::Fixed(ART))
            .into_element(),
        None => podcast_art_thumb(None, ART, enz_theme),
    };
    // One toggle: Subscribe, or Subscribed (which unsubscribes).
    let actions: Element<'a, Message> = match subscribed_at {
        Some(index) => enz_button("Subscribed", Message::PodcastUnsubscribe(index))
            .icon("check")
            .kind(ButtonKind::Quiet)
            .aria_label(format!("Subscribed to {title}. Unsubscribe"))
            .tooltip("Unsubscribe")
            .id(enzui_core::identity::derive_id(
                "music-podcast-page",
                "subscription",
            ))
            .into_element(),
        None => enz_button(
            "Subscribe",
            Message::PodcastSubscribeFromResult(feed.feed_url.clone()),
        )
        .icon("plus")
        .kind(ButtonKind::Accent)
        .id(enzui_core::identity::derive_id(
            "music-podcast-page",
            "subscription",
        ))
        .into_element(),
    };
    let mut info = column![
        enz_text("Podcast").size(TextSize::Footnote).color(sc),
        enz_text(title)
            .size(TextSize::Title1)
            .color(tc)
            .full_width()
            .wrap(true),
    ]
    .spacing(enz_theme.spacing.xs())
    .width(Length::Fill);
    if !feed.author.trim().is_empty() {
        info = info.push(enz_text(feed.author.trim()).size(TextSize::Body).color(sc));
    }
    info = info.push(container(actions).padding(Padding::new(0.0).top(enz_theme.spacing.sm())));

    let mut page = column![
        row![cover, info]
            .spacing(enz_theme.spacing.lg())
            .align_y(Alignment::End),
    ]
    .spacing(enz_theme.spacing.lg())
    .padding(page_padding(enz_theme))
    .width(Length::Fill);
    let about = crate::podcasts::plain_summary(&feed.description, 600);
    if !about.is_empty() {
        page = page.push(
            enz_text(about)
                .size(TextSize::Footnote)
                .color(sc)
                .full_width()
                .wrap(true),
        );
    }
    match &feed.status {
        crate::podcasts::FeedStatus::Loading => {
            page = page.push(
                container(enzui_widgets::spinner::enz_spinner())
                    .center_x(Length::Fill)
                    .padding(enz_theme.spacing.lg()),
            );
        }
        crate::podcasts::FeedStatus::Failed(message) => {
            let mut line = row![
                enz_text(message.as_str())
                    .size(TextSize::Footnote)
                    .color(theme::danger_color(enz_theme))
                    .width(Length::Fill),
            ]
            .align_y(Alignment::Center);
            if let Some(index) = subscribed_at {
                line = line.push(
                    enz_button(PODCAST_RETRY_LABEL, Message::PodcastSubscribeRetry(index))
                        .kind(ButtonKind::Quiet)
                        .size(ButtonSize::Sm)
                        .into_element(),
                );
            } else {
                line = line.push(
                    enz_button(
                        PODCAST_RETRY_LABEL,
                        Message::PodcastOpen(feed.feed_url.clone()),
                    )
                    .kind(ButtonKind::Quiet)
                    .size(ButtonSize::Sm)
                    .into_element(),
                );
            }
            page = page.push(line);
        }
        crate::podcasts::FeedStatus::Loaded => {
            page = page.push(
                enz_text(crate::podcasts::format_episode_count(feed.playable_count()))
                    .size(TextSize::Title3)
                    .color(tc)
                    .heading(HeadingLevel::Section),
            );
            page = page.push(feed_episode_list(
                feed,
                episodes_shown,
                art_cache,
                playback,
                enz_theme,
            ));
        }
    }
    crate::views::page_scroll_tracked(
        page,
        crate::podcast_discover::podcasts_scroll_id(),
        Message::PodcastsScrolled,
    )
}

fn station_logo<'a>(cover: &crate::art::CoverArt, enz_theme: &EnzTheme) -> Element<'a, Message> {
    let edge = layout::TrackRowLayout::INDEX_W;
    enzui_widgets::image::enz_image_handle(cover.handle().clone())
        .fit(enzui_widgets::image::EnzImageFit::Contain)
        .radius(enz_theme.radius.sm)
        .alt_text("Station logo")
        .width(Length::Fixed(edge))
        .height(Length::Fixed(edge))
        .placeholder_background(theme::surface_alt_color(enz_theme))
        .into_element()
}

/// The tuned stream's state at the row's trailing edge: a spinner while it
/// connects, a playing mark, or a quiet reason it could not play.
pub(crate) fn stream_status_mark<'a>(
    phase: Option<&crate::StreamPhase>,
    playing: bool,
    enz_theme: &EnzTheme,
) -> Option<Element<'a, Message>> {
    let mark: Element<'a, Message> = match phase? {
        crate::StreamPhase::Loading => enzui_widgets::spinner::enz_spinner()
            .size(enz_theme.sizing.icon_sm)
            .into(),
        crate::StreamPhase::Failed(reason) => row![
            enz_icon("circle-alert")
                .size(enz_theme.sizing.icon_sm)
                .tint(theme::secondary_text_color(enz_theme))
                .into_element(),
            enz_text(reason.clone())
                .size(TextSize::Caption)
                .color(theme::secondary_text_color(enz_theme)),
        ]
        .spacing(enz_theme.spacing.xs())
        .align_y(Alignment::Center)
        .into(),
        crate::StreamPhase::Idle if playing => enz_icon("audio-lines")
            .size(enz_theme.sizing.icon_sm)
            .tint(theme::accent_color(enz_theme))
            .into_element(),
        crate::StreamPhase::Idle => enz_icon("pause")
            .size(enz_theme.sizing.icon_sm)
            .tint(theme::secondary_text_color(enz_theme))
            .into_element(),
    };
    Some(mark)
}
