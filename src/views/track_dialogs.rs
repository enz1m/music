use super::*;
use iced::widget::{column, row};

pub const TRACK_INFO_DIALOG_TITLE: &str = "Track Info";

fn track_info_max_height(enz: &EnzTheme, window_height: f32) -> f32 {
    let h = if window_height > 0.0 {
        window_height
    } else {
        900.0
    };
    // Subtract the surface's own padding (`lg` top and bottom), which sits
    // outside the capped stack, so the painted surface stays within the cap.
    (h * 0.86 - 2.0 * enz.spacing.lg()).max(200.0)
}

/// Track inspection with path-keyed user ratings; file tags remain read-only.
///
/// The title and Close row are pinned chrome; the metadata scrolls between
/// them. The stack is capped from `window_size` so a short viewport keeps the
/// metadata reachable while title and Close stay visible. Escape, the scrim
/// press and the focus trap come from `enz_modal_host`.
pub fn track_info_overlay<'a>(
    background: Element<'a, Message>,
    track: &Track,
    rating: u8,
    details: Option<&crate::track_details::Presentation>,
    window_size: iced::Size,
    enz_theme: &'a EnzTheme,
    lifecycle: &enzui_widgets::modal_host::ModalFocusLifecycle,
) -> Element<'a, Message> {
    let enz = enz_theme;
    let tc = theme::text_color(enz);
    let sc = theme::secondary_text_color(enz);

    let title: Element<'a, Message> = enz_text(TRACK_INFO_DIALOG_TITLE)
        .size(TextSize::Title3)
        .color(tc)
        .into_element();

    use crate::track_details::{Page, State};
    let page = details.map(|details| details.page).unwrap_or_default();
    let path = track.path.clone();
    let rating = enz_radio_group(
        "Your rating out of 5",
        &["Unrated", "1", "2", "3", "4", "5"],
        Some(usize::from(rating)),
        move |rating| Message::SetTrackRating(path.clone(), rating),
    )
    .orientation(enzui_widgets::radio_group::Orientation::Horizontal)
    .option_id_prefix("music-track-rating-option")
    .id(iced::advanced::widget::Id::new("music-track-rating"))
    .into_element();
    let pages = enz_radio_group(
        "Track details",
        &["Info", "Lyrics", "Tags"],
        Some(match page {
            Page::Info => 0,
            Page::Lyrics => 1,
            Page::Tags => 2,
        }),
        |index| {
            Message::TrackDetailsPage(match index {
                1 => Page::Lyrics,
                2 => Page::Tags,
                _ => Page::Info,
            })
        },
    )
    .orientation(enzui_widgets::radio_group::Orientation::Horizontal)
    .option_id_prefix("music-track-page-option")
    .id(iced::advanced::widget::Id::new("music-track-details-page"))
    .into_element();
    let mut metadata = column![rating, pages].spacing(enz.spacing.sm());
    match details.map(|details| &details.state) {
        Some(State::Ready(inspection)) => match page {
            Page::Info | Page::Tags => {
                let fields = if page == Page::Info {
                    Some(&inspection.fields)
                } else {
                    match &inspection.tags {
                        Ok(tags) => Some(tags),
                        Err(error) => {
                            metadata = metadata.push(enz_text(error.label()));
                            None
                        }
                    }
                };
                if fields.is_some_and(|fields| fields.is_empty()) {
                    metadata = metadata.push(enz_text("No tags found."));
                }
                for (key, value) in fields.into_iter().flatten() {
                    metadata = metadata.push(
                        column![
                            enz_text(key.clone()).size(TextSize::Caption).color(sc),
                            enz_text(value.clone()).size(TextSize::Body).color(tc),
                        ]
                        .spacing(enz.spacing.xs()),
                    );
                }
            }
            Page::Lyrics => match &inspection.lyrics {
                Ok(lyrics) => {
                    metadata =
                        metadata.push(enz_text(lyrics.origin.clone()).size(TextSize::Caption));
                    for line in &lyrics.lines {
                        let text = match line.millis {
                            Some(time) => format!(
                                "[{}:{:02}.{:03}] {}",
                                time / 60_000,
                                (time / 1000) % 60,
                                time % 1000,
                                line.text
                            ),
                            None => line.text.clone(),
                        };
                        metadata = metadata.push(enz_text(text).size(TextSize::Body));
                    }
                }
                Err(error) => metadata = metadata.push(enz_text(error.label())),
            },
        },
        Some(State::Loading) => {
            metadata = metadata.push(enz_text("Loading track details…")).push(
                enz_button("Cancel inspection", Message::TrackDetailsCancel)
                    .id(iced::advanced::widget::Id::new(
                        "music-track-details-cancel",
                    ))
                    .kind(ButtonKind::Quiet)
                    .into_element(),
            );
        }
        Some(State::Failed(error)) => metadata = metadata.push(enz_text(error.label())),
        None => metadata = metadata.push(enz_text("Load this track to inspect its metadata.")),
    }
    metadata = metadata.push(
        enz_button("Reload details", Message::TrackDetailsReload)
            .id(iced::advanced::widget::Id::new(
                "music-track-details-reload",
            ))
            .kind(ButtonKind::Quiet)
            .into_element(),
    );

    let body: Element<'a, Message> = enz_scroll_view(metadata)
        .width(Length::Fill)
        .height(Length::Shrink)
        .id(iced::advanced::widget::Id::new("music-track-info-scroll"))
        .into();

    let close_btn: Element<'a, Message> = enz_button("Close", Message::ShowTrackInfo(None))
        .kind(ButtonKind::Accent)
        .size(ButtonSize::Sm)
        .id(iced::advanced::widget::Id::new("music-track-info-close"))
        .into_element();
    let close_row: Element<'a, Message> =
        row![h_space(), close_btn].align_y(Alignment::Center).into();

    let stack = enzui_widgets::capped_dialog_body::enz_capped_dialog_body(
        vec![title, enz_divider().into_element()],
        body,
        vec![enz_divider().into_element(), close_row],
        enz.spacing.sm(),
        track_info_max_height(enz, window_size.height),
    );

    let surface: Element<'a, Message> = enz_modal_surface(stack.into())
        .width(Length::Fixed(480.0))
        .into_element();

    enzui_widgets::modal_host::enz_modal_host(background, Some(surface), |event| {
        Message::ModalHost(crate::ModalPresentationId::TrackInfo, event)
    })
    .scoped_to(lifecycle)
    .into_element()
}

// ---------------------------------------------------------------------------
// Sleep-timer dialog
// ---------------------------------------------------------------------------

/// The sleep-timer dialog, presented over `background`.
///
/// When a timer is armed it shows the `sleep_fade_status` line and a
/// `Cancel timer` action. The preset group is controlled: `armed_preset` is
/// the active timer's preset, and `None` selects nothing.
pub fn sleep_timer_overlay<'a>(
    background: Element<'a, Message>,
    timer: SleepTimerState,
    fade_choice: bool,
    armed_preset: Option<SleepPreset>,
    enz_theme: &'a EnzTheme,
    lifecycle: &enzui_widgets::modal_host::ModalFocusLifecycle,
) -> Element<'a, Message> {
    let enz = enz_theme;

    let title_el: Element<'a, Message> = enz_text("Sleep timer")
        .size(TextSize::Title3)
        .color(theme::text_color(enz))
        .into_element();

    let presets: Element<'a, Message> = enz_radio_group(
        "Stop playback after",
        SLEEP_PRESET_LABELS,
        armed_preset.and_then(SleepPreset::index),
        Message::SleepArm,
    )
    .id(iced::advanced::widget::Id::new("music-sleep-presets"))
    .into_element();

    let fade: Element<'a, Message> =
        enz_checkbox(fade_choice, "Fade out at the end", Message::SleepSetFade)
            .description("Ramp the volume down over the final 20 seconds.")
            .id(iced::advanced::widget::Id::new("music-sleep-fade"))
            .into_element();

    let mut status_col = column![].spacing(enz.spacing.xs());
    match timer {
        SleepTimerState::Off => {}
        SleepTimerState::Armed {
            remaining_secs,
            fade: fading,
        } => {
            let line = if fading {
                sleep_fade_status(timer).unwrap_or_default()
            } else {
                format!("Stopping in {}", format_duration(remaining_secs))
            };
            status_col = status_col.push(
                enz_text(line)
                    .size(TextSize::Body)
                    .color(theme::text_color(enz))
                    .into_element(),
            );
        }
        SleepTimerState::UntilTrackEnd { fade: fading } => {
            let line = if fading {
                sleep_fade_status(timer).unwrap_or_default()
            } else {
                "End of track".to_string()
            };
            status_col = status_col.push(
                enz_text(line)
                    .size(TextSize::Body)
                    .color(theme::text_color(enz))
                    .into_element(),
            );
        }
    }
    if timer.is_armed() {
        status_col = status_col.push(
            enz_button("Cancel timer", Message::SleepCancel)
                .kind(ButtonKind::Quiet)
                .id(iced::advanced::widget::Id::new("music-sleep-cancel"))
                .into_element(),
        );
    }

    let footer = row![
        h_space(),
        enz_button("Done", Message::ToggleSleepTimer)
            .kind(ButtonKind::Accent)
            .id(iced::advanced::widget::Id::new("music-sleep-done"))
            .into_element(),
    ]
    .spacing(enz.spacing.sm())
    .align_y(Alignment::Center);

    let body = column![
        title_el,
        presets,
        fade,
        enz_divider().into_element(),
        status_col,
        footer,
    ]
    .spacing(enz.spacing.sm())
    // Documented exception: 400 sits between dialog_width_sm(360) and
    // dialog_width_md(460), sized to the sparse content; no rung matches.
    .width(Length::Fixed(400.0));

    let surface: Element<'a, Message> = enz_modal_surface(body.into())
        // Documented exception: same 400 width as the body column.
        .width(Length::Fixed(400.0))
        .into_element();

    enzui_widgets::modal_host::enz_modal_host(background, Some(surface), |event| {
        Message::ModalHost(crate::ModalPresentationId::SleepTimer, event)
    })
    .scoped_to(lifecycle)
    .into_element()
}

/// The sleep-timer preset labels, in `SleepPreset::SELECTABLE` order.
pub const SLEEP_PRESET_LABELS: &[&str] = &[
    "5 min",
    "10 min",
    "15 min",
    "30 min",
    "45 min",
    "60 min",
    "End of current track",
];
