//! Podcast discovery section and result rows.

use super::*;
use iced::widget::column;

/// The discovery Search button's routable id.
pub const PODCAST_SEARCH_BUTTON_ID: &str = "music-app-podcast-search";
/// The discovery Clear button's routable id and label, shown only while a
/// search is live.
pub const PODCAST_SEARCH_CLEAR_ID: &str = "music-app-podcast-search-clear";
pub const PODCAST_SEARCH_CLEAR_LABEL: &str = "Clear";
/// The per-result Subscribe button's id prefix, suffixed with the feed URL.
pub const PODCAST_RESULT_SUBSCRIBE_ID: &str = "music-app-podcast-result-subscribe";
/// The empty-results label.
pub const PODCAST_SEARCH_NO_MATCHES_LABEL: &str = "No podcasts matched your search.";

/// A small square artwork tile: the cached decoded thumbnail, else the themed
/// podcast-glyph placeholder. Never decodes during `view`.
pub(crate) fn podcast_art_thumb<'a>(
    art: Option<&crate::art::CoverArt>,
    size: f32,
    enz_theme: &EnzTheme,
) -> Element<'a, Message> {
    let bg = theme::surface_alt_color(enz_theme);
    match art {
        Some(cover) => container(
            enz_image_handle(cover.handle().clone())
                .fit(EnzImageFit::Cover)
                .radius(enz_theme.radius.sm)
                .alt_text("Podcast artwork")
                .width(Length::Fixed(size))
                .height(Length::Fixed(size))
                .placeholder_background(bg)
                .into_element(),
        )
        .width(Length::Fixed(size))
        .height(Length::Fixed(size))
        .into(),
        None => {
            use enzui_widgets::surface::{RadiusSize, SurfaceTone, enz_surface};
            enz_surface(
                container(
                    enz_icon("podcast")
                        .size(enz_theme.sizing.icon_md)
                        .tint(theme::secondary_text_color(enz_theme))
                        .into_element(),
                )
                .center_x(size)
                .center_y(size),
            )
            .tone(SurfaceTone::SurfaceAlt)
            .radius_size(RadiusSize::Sm)
            .width(size)
            .height(size)
            .into_element()
        }
    }
}

/// The keyless discovery section: a search field and button above the state
/// of the last query.
///
/// Idle, searching, failed and no-match states each render distinctly.
pub(crate) fn podcast_discovery_section<'a>(
    query: &'a str,
    state: &'a crate::PodcastSearchState,
    subscriptions: &'a [crate::podcasts::PodcastFeed],
    art_cache: &'a crate::podcasts::artwork::PodcastArtworkCache,
    enz_theme: &'a EnzTheme,
) -> Element<'a, Message> {
    use enzui_widgets::text_field::enz_text_field;
    let sc = theme::secondary_text_color(enz_theme);

    let mut section = column![]
        .spacing(enz_theme.spacing.sm())
        .width(Length::Fill);

    let field: Element<'_, Message> = enz_text_field(
        "Search podcasts",
        crate::podcasts::PODCAST_SEARCH_PLACEHOLDER,
        query,
        Message::PodcastSearchInputChanged,
    )
    .on_submit(Message::PodcastSearchSubmit)
    .size(TextFieldSize::Md)
    .into();
    let can_search = !query.trim().is_empty();
    let search_button: Element<'_, Message> = enz_button("Search", Message::PodcastSearchSubmit)
        .id(enzui_core::identity::derive_id(
            PODCAST_SEARCH_BUTTON_ID,
            "",
        ))
        .icon("search")
        .kind(ButtonKind::Quiet)
        .size(ButtonSize::Md)
        .enabled(can_search)
        .into_element();
    let mut controls = row![field, search_button]
        .spacing(enz_theme.spacing.sm())
        .align_y(Alignment::End);
    // Clear is hidden while Idle.
    if !matches!(state, crate::PodcastSearchState::Idle) {
        controls = controls.push(
            enz_button(PODCAST_SEARCH_CLEAR_LABEL, Message::PodcastSearchClear)
                .id(enzui_core::identity::derive_id(PODCAST_SEARCH_CLEAR_ID, ""))
                .kind(ButtonKind::Quiet)
                .size(ButtonSize::Sm)
                .into_element(),
        );
    }
    section = section.push(controls);

    match state {
        crate::PodcastSearchState::Idle => {}
        crate::PodcastSearchState::Searching => {
            section = section.push(
                enz_text(crate::podcasts::PODCAST_SEARCH_IN_FLIGHT_LABEL)
                    .size(TextSize::Footnote)
                    .color(sc),
            );
        }
        crate::PodcastSearchState::Unavailable(message) => {
            section = section.push(
                enz_text(message.as_str())
                    .size(TextSize::Footnote)
                    .color(theme::danger_color(enz_theme)),
            );
        }
        crate::PodcastSearchState::Error { message, retryable } => {
            section = section.push(
                enz_text(message.as_str())
                    .size(TextSize::Footnote)
                    .color(theme::danger_color(enz_theme)),
            );
            if *retryable {
                section = section.push(
                    enz_button(PODCAST_RETRY_LABEL, Message::PodcastSearchSubmit)
                        .kind(ButtonKind::Quiet)
                        .size(ButtonSize::Sm)
                        .into_element(),
                );
            }
        }
        crate::PodcastSearchState::Results(results) => {
            if results.is_empty() {
                section = section.push(
                    enz_text(PODCAST_SEARCH_NO_MATCHES_LABEL)
                        .size(TextSize::Footnote)
                        .color(sc),
                );
            } else {
                for result in results {
                    section = section.push(podcast_result_row(
                        result,
                        subscriptions,
                        art_cache,
                        enz_theme,
                    ));
                }
            }
        }
    }
    section.into()
}

/// One discovery result row: artwork, title, author, a bounded description
/// and a Subscribe button, disabled as “Subscribed” for a show already
/// subscribed.
fn podcast_result_row<'a>(
    result: &'a crate::podcasts::search::PodcastSearchResult,
    subscriptions: &'a [crate::podcasts::PodcastFeed],
    art_cache: &'a crate::podcasts::artwork::PodcastArtworkCache,
    enz_theme: &'a EnzTheme,
) -> Element<'a, Message> {
    let tc = theme::text_color(enz_theme);
    let sc = theme::secondary_text_color(enz_theme);
    let already = subscriptions
        .iter()
        .any(|feed| feed.feed_url == result.feed_url);

    let mut text_col = column![
        enz_text(result.title.as_str())
            .size(TextSize::Body)
            .color(tc)
            .width(Length::Fill),
    ]
    .spacing(enz_theme.spacing.space1)
    .width(Length::Fill);
    if !result.author.trim().is_empty() {
        text_col = text_col.push(
            enz_text(result.author.as_str())
                .size(TextSize::Caption)
                .color(sc),
        );
    }
    if !result.description.trim().is_empty() {
        text_col = text_col.push(
            enz_text(result.description.as_str())
                .size(TextSize::Caption)
                .color(sc)
                .width(Length::Fill),
        );
    }

    let subscribe: Element<'_, Message> = enz_button(
        if already { "Subscribed" } else { "Subscribe" },
        Message::PodcastSubscribeFromResult(result.feed_url.clone()),
    )
    .id(enzui_core::identity::derive_id(
        PODCAST_RESULT_SUBSCRIBE_ID,
        &result.feed_url,
    ))
    .size(ButtonSize::Sm)
    .enabled(!already)
    .kind(ButtonKind::Quiet)
    .into_element();

    let opens = enz_pressable(
        row![
            podcast_art_thumb(art_cache.channel_art(&result.feed_url), 48.0, enz_theme),
            text_col,
        ]
        .spacing(enz_theme.spacing.sm())
        .align_y(Alignment::Center)
        .width(Length::Fill)
        .into(),
        Message::PodcastOpen(result.feed_url.clone()),
    )
    .style(PressableStyle::Row)
    .padding(enz_theme.spacing.xs())
    .width(Length::Fill)
    .aria_label(format!("Open {}", result.title))
    .id(enzui_core::identity::derive_id(
        "music-podcast-result-open",
        &result.feed_url,
    ))
    .into_element();
    row![opens, subscribe]
        .spacing(enz_theme.spacing.sm())
        .align_y(Alignment::Center)
        .width(Length::Fill)
        .into()
}
