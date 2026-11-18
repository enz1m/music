use super::*;
use iced::widget::{column, row};

// ---------------------------------------------------------------------------
// Keyboard-shortcuts overlay
// ---------------------------------------------------------------------------

// The reference uses the key decoder's shortcut table.
pub(crate) fn shortcuts_dialog_builder(
    window_size: iced::Size,
) -> enzui_widgets::shortcuts_dialog::EnzShortcutsDialog<'static, Message> {
    enz_shortcuts_dialog(true, crate::shortcuts::overlay_entries())
        .title("Keyboard Shortcuts")
        .close_label("Done")
        .viewport_height(window_size.height)
}

pub fn shortcuts_overlay<'a>(
    background: Element<'a, Message>,
    window_size: iced::Size,
    lifecycle: &enzui_widgets::modal_host::ModalFocusLifecycle,
) -> Element<'a, Message> {
    // Tab routes through the owned lifecycle (`Message::ModalFocusTraverse`),
    // so traversal wraps within the presentation, not the shell behind it.
    shortcuts_dialog_builder(window_size)
        .into_modal_host(background, |ev: ShortcutsDialogEvent| {
            let host = match ev {
                ShortcutsDialogEvent::Dismissed(cause) => {
                    enzui_widgets::modal_host::ModalHostEvent::Dismissed(cause)
                }
                ShortcutsDialogEvent::FocusNext => {
                    enzui_widgets::modal_host::ModalHostEvent::FocusNext
                }
                ShortcutsDialogEvent::FocusPrevious => {
                    enzui_widgets::modal_host::ModalHostEvent::FocusPrevious
                }
            };
            Message::ModalHost(crate::ModalPresentationId::Shortcuts, host)
        })
        .scoped_to(lifecycle)
        .into_element()
}

// ---------------------------------------------------------------------------
// Add-to-playlist picker
// ---------------------------------------------------------------------------

/// The dialog title for a picker target.
fn picker_title(target: PickerTarget) -> &'static str {
    match target {
        PickerTarget::Track(_) => "Add to playlist",
        PickerTarget::Album(_) => "Add album to playlist",
        PickerTarget::CurrentTrack => "Add current track to playlist",
    }
}

/// The dialog subtitle prompt: `Choose where to add the ` followed by the
/// target class (`track` / `album` / `current track`).
pub fn picker_prompt(target: PickerTarget) -> String {
    let noun = match target {
        PickerTarget::Track(_) => "track",
        PickerTarget::Album(_) => "album",
        PickerTarget::CurrentTrack => "current track",
    };
    format!("Choose where to add the {noun}")
}

/// The add-to-playlist picker, presented over `background`.
///
/// Destination playlists are read live from the library, so a just-created
/// playlist appears. With no playlists the list shows `Create your first
/// playlist`.
pub fn playlist_picker_overlay<'a>(
    background: Element<'a, Message>,
    picker: &'a PlaylistPicker,
    library: &'a Library,
    enz_theme: &'a EnzTheme,
    lifecycle: &enzui_widgets::modal_host::ModalFocusLifecycle,
) -> Element<'a, Message> {
    let enz = enz_theme;
    let title = picker_title(picker.target);

    let title_el: Element<'a, Message> = enz_text(title)
        .size(TextSize::Title3)
        .color(theme::text_color(enz))
        .into_element();

    let desc_el: Element<'a, Message> = enz_text(picker_prompt(picker.target))
        .size(TextSize::Caption)
        .color(theme::secondary_text_color(enz))
        .into_element();

    let list_body: Element<'a, Message> = if library.playlists.is_empty() {
        enz_text("No playlists yet. Name one below and the track goes into it.")
            .size(TextSize::Body)
            .color(theme::secondary_text_color(enz))
            .wrap(true)
            .into_element()
    } else {
        // Full list rows, as everywhere else: padded content and a hover
        // wash that stops short of the dialog's edges.
        let mut rows = column![].spacing(enz.spacing.space0);
        for pl in &library.playlists {
            let count = pl.track_ids.len();
            let row_content = row![
                enz_icon("list-music")
                    .tint(theme::secondary_text_color(enz))
                    .into_element(),
                enz_text(pl.name.clone())
                    .size(TextSize::Body)
                    .color(theme::text_color(enz))
                    .full_width()
                    .single_line_elided(),
                enz_text(format_count(count, "track", "tracks"))
                    .size(TextSize::Caption)
                    .color(theme::secondary_text_color(enz))
                    .into_element(),
            ]
            .spacing(enz.spacing.sm())
            .align_y(Alignment::Center);
            rows = rows.push(
                enz_list_row_surface(
                    container(row_content)
                        .width(Length::Fill)
                        .padding(Padding::new(0.0).top(8.0).bottom(8.0)),
                    Message::PickerAddToExisting(pl.id),
                )
                .content_padding(enz.spacing.md())
                .outer_gutter(0.0)
                .content_height(true)
                .label(pl.name.clone()),
            );
        }
        // As tall as its rows, up to the cap; then it scrolls.
        let height =
            (library.playlists.len() as f32 * PICKER_ROW_HEIGHT).min(PICKER_LIST_MAX_HEIGHT);
        enz_scroll_view(rows)
            .width(Length::Fill)
            .height(Length::Fixed(height))
            .into()
    };

    // A new playlist: its name, and Create adds the track to it.
    let name_field: Element<'a, Message> = enz_text_field(
        "",
        "New playlist name",
        &picker.new_name,
        Message::PickerNameChanged,
    )
    .on_submit(Message::PickerCreateAndAdd)
    .id(iced::advanced::widget::Id::new("music-picker-new-name"))
    .into();
    let create_btn: Element<'a, Message> = enz_button("Create", Message::PickerCreateAndAdd)
        .icon("plus")
        .kind(ButtonKind::Quiet)
        .enabled(!picker.new_name.trim().is_empty())
        .into_element();
    let new_row = row![name_field, create_btn]
        .spacing(enz.spacing.sm())
        .align_y(Alignment::Center);

    // Done closes after an add already applied; Cancel closes without adding.
    let footer = row![
        h_space(),
        enz_button("Cancel", Message::ClosePlaylistPicker)
            .kind(ButtonKind::Quiet)
            .into_element(),
        enz_button("Done", Message::ClosePlaylistPicker)
            .kind(ButtonKind::Accent)
            .into_element(),
    ]
    .spacing(enz.spacing.sm())
    .align_y(Alignment::Center);

    let body = column![
        column![title_el, desc_el].spacing(enz.spacing.xs()),
        list_body,
        new_row,
        footer,
    ]
    .spacing(enz.spacing.md())
    // Documented exception: 420 sits between dialog rungs sm(360) and md(460);
    // sm clips playlist names and md leaves the rows too roomy.
    .width(Length::Fixed(420.0));

    let surface: Element<'a, Message> = enz_modal_surface(body.into())
        // Documented exception: 420 between sm(360) and md(460), see above.
        .width(Length::Fixed(420.0))
        .into_element();

    // Tab routes through the owned lifecycle, scoped to this presentation.
    enzui_widgets::modal_host::enz_modal_host(background, Some(surface), |event| {
        Message::ModalHost(crate::ModalPresentationId::PlaylistPicker, event)
    })
    .scoped_to(lifecycle)
    .into_element()
}

/// The playlist picker's list grows to this height, then scrolls.
const PICKER_LIST_MAX_HEIGHT: f32 = 280.0;
/// One playlist row's height in the picker.
const PICKER_ROW_HEIGHT: f32 = 40.0;

/// The visible title of the Rename dialog.
pub const PLAYLIST_RENAME_TITLE: &str = "Rename playlist";
/// The Rename dialog's commit button label.
pub const PLAYLIST_RENAME_COMMIT_LABEL: &str = "Rename";

/// The playlist Rename dialog, presented over `background`.
///
/// Focus lands in the text field on open, and the dialog stays open until an
/// accepted commit closes it.
pub fn playlist_rename_overlay<'a>(
    background: Element<'a, Message>,
    rename: &'a crate::domain::PlaylistRename,
    library: &'a Library,
    enz_theme: &'a EnzTheme,
    lifecycle: &enzui_widgets::modal_host::ModalFocusLifecycle,
) -> Element<'a, Message> {
    let enz = enz_theme;

    // Title3 label, not a heading, so the modal injects no Section rung into the
    // page's heading outline while presented.
    let title_el: Element<'a, Message> = enz_text(PLAYLIST_RENAME_TITLE)
        .size(TextSize::Title3)
        .color(theme::text_color(enz))
        .into_element();

    let validation = library.validate_playlist_name(rename.playlist_id, &rename.draft);

    let mut field = enz_text_field(
        "Playlist name",
        "Playlist name",
        &rename.draft,
        Message::RenamePlaylistDraftChanged,
    )
    .on_submit(Message::RenamePlaylistCommit)
    .id(iced::advanced::widget::Id::new(
        "music-playlist-rename-field",
    ));
    if let Err(err) = &validation {
        field = field.error(err.message());
    }
    let field_el: Element<'a, Message> = field.into();

    let footer = row![
        h_space(),
        enz_button("Cancel", Message::RenamePlaylistCancel)
            .kind(ButtonKind::Quiet)
            .into_element(),
        enz_button(PLAYLIST_RENAME_COMMIT_LABEL, Message::RenamePlaylistCommit)
            .kind(ButtonKind::Accent)
            .into_element(),
    ]
    .spacing(enz.spacing.sm())
    .align_y(Alignment::Center);

    let body = column![title_el, field_el, footer]
        .spacing(enz.spacing.sm())
        // 420 px sits between the sm(360) and md(460) dialog rungs, matching the
        // sibling picker.
        .width(Length::Fixed(420.0));

    let surface: Element<'a, Message> = enz_modal_surface(body.into())
        .width(Length::Fixed(420.0))
        .into_element();

    enzui_widgets::modal_host::enz_modal_host(background, Some(surface), |event| {
        Message::ModalHost(crate::ModalPresentationId::PlaylistRename, event)
    })
    .scoped_to(lifecycle)
    .into_element()
}

/// The playlist Delete confirmation title.
pub const PLAYLIST_DELETE_TITLE: &str = "Delete playlist?";
/// The confirmation's affirmative (destructive) button label.
pub const PLAYLIST_DELETE_CONFIRM_LABEL: &str = "Delete";

/// The playlist Delete confirmation, presented over `background`.
///
/// The safe action sits at the trailing edge, so a bare Enter never fires
/// Delete and Tab reaches Cancel first. Every ending funnels through
/// [`Message::PlaylistDeleteConfirm`]. The body says the tracks stay; only the
/// playlist's name and ordering are deleted.
pub fn playlist_delete_overlay<'a>(
    background: Element<'a, Message>,
    name: &str,
    state: &'a enzui_core::confirm::ConfirmState,
    lifecycle: &enzui_widgets::modal_host::ModalFocusLifecycle,
) -> Element<'a, Message> {
    let message =
        format!("\u{201C}{name}\u{201D} will be removed. The tracks in it stay in your library.");
    enzui_widgets::confirm_dialog::enz_confirm_dialog(
        PLAYLIST_DELETE_TITLE,
        message,
        state,
        Message::PlaylistDeleteConfirm,
    )
    .destructive(true)
    .confirm_label(PLAYLIST_DELETE_CONFIRM_LABEL)
    .cancel_label("Cancel")
    .id_prefix(crate::ModalPresentationId::PlaylistDelete.scope_key())
    .into_modal_host(background)
    .scoped_to(lifecycle)
    .into_element()
}

// ---------------------------------------------------------------------------
// Equalizer dialog
// ---------------------------------------------------------------------------

/// The width of the equalizer modal, in px: `SizingTokens::dialog_width_md`,
/// so a sizing-scale retune reaches this medium dialog.
pub(crate) fn equalizer_dialog_width(sizing: &enzui_theme::SizingTokens) -> f32 {
    sizing.dialog_width_md
}

/// The equalizer dialog, presented over `background`: the switch, then the
/// preset and clip protection, then the bands in a centred bank with each
/// gain above its fader. Everything but the switch greys out while the
/// equalizer is off.
pub fn equalizer_overlay<'a>(
    background: Element<'a, Message>,
    eq: &'a EqState,
    preset_menu_open: bool,
    enz_theme: &'a EnzTheme,
    lifecycle: &enzui_widgets::modal_host::ModalFocusLifecycle,
) -> Element<'a, Message> {
    use enzui_widgets::preferences_group::enz_preferences_group;
    use enzui_widgets::surface::{SurfaceTone, enz_surface};
    let enz = enz_theme;
    let on = eq.enabled;
    let tc = theme::text_color(enz);
    let sc = theme::secondary_text_color(enz);
    // Text dims with the controls while the equalizer is off.
    let dim = |color: iced::Color| {
        if on {
            color
        } else {
            iced::Color {
                a: color.a * 0.45,
                ..color
            }
        }
    };
    let width = equalizer_dialog_width(&enz.sizing).max(560.0);

    let title_el: Element<'a, Message> = enz_text("Equalizer")
        .size(TextSize::Title3)
        .color(tc)
        .into_element();

    let enable_group = enz_preferences_group(column![
        enz_preferences_row("Equalizer")
            .subtitle("Shape the sound of everything that plays.")
            .switch(on, Message::EqSetEnabled)
            .into_element()
    ])
    .into_element();

    let preset_row = enz_preferences_row("Preset")
        .id(iced::advanced::widget::Id::new("music-eq-preset"))
        .subtitle("Moving a band makes it Custom.")
        .enabled(on)
        .value_menu(
            eq.preset.label(),
            preset_menu_open && on,
            Message::EqPresetMenu(true),
            Message::EqPresetMenu(false),
            EQ_PRESET_LABELS
                .iter()
                .enumerate()
                .map(|(index, label)| ((*label).to_owned(), Message::EqSelectPreset(index)))
                .collect(),
        )
        .into_element();
    let clip_row = enz_preferences_row("Clip protection")
        .subtitle("Lower the output to leave room for boosted bands.")
        .enabled(on)
        .switch(eq.clip_protection, Message::EqSetClipProtection)
        .into_element();
    let settings_group = enz_preferences_group(column![preset_row, clip_row]).into_element();

    // The bands: gain above, fader, frequency below; a dB scale beside them.
    const FADER: f32 = 160.0;
    const BAND_W: f32 = 36.0;
    // Off, the faders keep their positions in muted ink.
    let fader_tokens = {
        let mut tokens = enzui_widgets::slider::SliderTokens::from_palette(&enz.palette);
        if !on {
            let muted = iced::Color { a: 0.35, ..sc };
            tokens.fill = muted;
            tokens.fill_hover = muted;
            tokens.thumb = iced::Color {
                a: 0.6,
                ..tokens.thumb
            };
            tokens.thumb_shadow = iced::Color::TRANSPARENT;
        }
        tokens
    };
    let readout = |gain: f32| -> String {
        if gain.abs() < 0.05 {
            "0".to_owned()
        } else {
            format!("{gain:+.0}")
        }
    };
    let mut bank = row![].spacing(enz.spacing.xs()).align_y(Alignment::End);
    // The scale lines up with the faders' top, middle and bottom.
    let scale_label = |text: &'static str| {
        enz_text(text)
            .size(TextSize::Caption2)
            .color(dim(sc))
            .into_element()
    };
    bank = bank.push(
        column![
            Space::new().height(Length::Fixed(18.0)),
            container(
                column![
                    scale_label("+12"),
                    Space::new().height(Length::Fill),
                    scale_label("0"),
                    Space::new().height(Length::Fill),
                    scale_label("\u{2212}12"),
                ]
                .align_x(Alignment::End)
            )
            .height(Length::Fixed(FADER))
            .align_right(Length::Fixed(28.0)),
            Space::new().height(Length::Fixed(18.0)),
        ]
        .spacing(enz.spacing.xs()),
    );
    for (i, &gain) in eq.bands.iter().enumerate() {
        let fader: Element<'a, Message> =
            enz_slider(EQ_GAIN_MIN_DB..=EQ_GAIN_MAX_DB, gain, move |g| {
                Message::EqSetBand {
                    index: i,
                    gain_db: g,
                }
            })
            .orientation(SliderOrientation::Vertical)
            .length(FADER)
            .enabled(on)
            .tokens(fader_tokens)
            .aria_label(EQ_BAND_NAMES[i])
            .into();
        bank = bank.push(
            column![
                container(
                    enz_text(readout(gain))
                        .size(TextSize::Caption)
                        .color(dim(if gain.abs() < 0.05 { sc } else { tc }))
                )
                .center_x(Length::Fixed(BAND_W))
                .height(Length::Fixed(18.0)),
                container(fader).center_x(Length::Fixed(BAND_W)),
                container(
                    enz_text(EQ_BAND_LABELS[i])
                        .size(TextSize::Caption)
                        .color(dim(sc))
                )
                .center_x(Length::Fixed(BAND_W))
                .height(Length::Fixed(18.0)),
            ]
            .spacing(enz.spacing.xs())
            .align_x(Alignment::Center),
        );
    }
    let flat = eq.preset == EqPreset::Flat;
    let bands_header = row![
        column![
            enz_text("Bands").size(TextSize::Headline).color(dim(tc)),
            enz_text("Gain in dB, frequency in Hz")
                .size(TextSize::Caption)
                .color(dim(sc)),
        ]
        .spacing(2.0)
        .width(Length::Fill),
        enz_button("Reset", Message::EqSelectPreset(0))
            .icon("rotate-ccw")
            .kind(ButtonKind::Quiet)
            .size(ButtonSize::Sm)
            .enabled(on && !flat)
            .id(iced::advanced::widget::Id::new("music-eq-reset"))
            .into_element(),
    ]
    .align_y(Alignment::Center);
    let bands_card = enz_surface(
        column![bands_header, container(bank).center_x(Length::Fill)]
            .spacing(enz.spacing.md())
            .width(Length::Fill),
    )
    .tone(SurfaceTone::SurfaceAlt)
    .padding(enz.spacing.md())
    .border_width(0.0)
    .width(Length::Fill)
    .into_element();

    let footer = row![
        h_space(),
        enz_button("Done", Message::ToggleEqualizer)
            .kind(ButtonKind::Accent)
            .id(iced::advanced::widget::Id::new("music-eq-done"))
            .into_element(),
    ]
    .align_y(Alignment::Center);

    let body = column![title_el, enable_group, settings_group, bands_card, footer]
        .spacing(enz.spacing.md())
        .width(Length::Fixed(width));
    let surface: Element<'a, Message> = enz_modal_surface(body.into())
        .width(Length::Fixed(width))
        .into_element();

    // Scoped Tab traversal + host scope binding (see picker above).
    enzui_widgets::modal_host::enz_modal_host(background, Some(surface), |event| {
        Message::ModalHost(crate::ModalPresentationId::Equalizer, event)
    })
    .scoped_to(lifecycle)
    .into_element()
}

/// Each band's accessible name.
const EQ_BAND_NAMES: [&str; EQ_BAND_COUNT] = [
    "31 Hz", "62 Hz", "125 Hz", "250 Hz", "500 Hz", "1 kHz", "2 kHz", "4 kHz", "8 kHz", "16 kHz",
];

/// The preset labels for the EQ preset menu, in `EqPreset::SELECTABLE` order.
pub const EQ_PRESET_LABELS: &[&str] = &["Flat", "Bass Boost", "Treble Boost", "Classical"];

// ---------------------------------------------------------------------------
// Settings: Library (folders, rescan) and Playback (ReplayGain)
// ---------------------------------------------------------------------------

/// The dialog title, also the main menu's row label.
pub const RG_TITLE: &str = "Settings";

/// The Playback page's title (volume normalization).
pub const RG_PAGE_TITLE: &str = "Playback";
/// The Library section's title (music folders and rescan).
pub const PREFS_LIBRARY_TITLE: &str = "Library";

/// The Library section's description.
pub const PREFS_FOLDERS_DESCRIPTION: &str =
    "Music in these folders appears in your library and stays up to date as files change.";
/// The Library page's rescan row title and subtitle.
pub const PREFS_RESCAN_TITLE: &str = "Rescan Library";
pub const PREFS_RESCAN_SUBTITLE: &str =
    "Read every folder again for new, changed and removed files.";
/// The rescan row's button label.
pub const PREFS_RESCAN_ACTION: &str = "Rescan";
/// The empty folder list's copy.
pub const PREFS_NO_FOLDERS: &str = "No music folders yet.";

/// The Settings pages' ids, in sidebar order.
pub const SETTINGS_LIBRARY_PAGE: &str = "library";
pub const SETTINGS_PLAYBACK_PAGE: &str = "playback";
pub const SETTINGS_SPOTIFY_PAGE: &str = "spotify";

/// What the Settings dialog shows besides the ReplayGain state.
#[derive(Debug, Clone, Copy, Default)]
pub struct PreferencesPages<'a> {
    /// Configured library roots, in order.
    pub folders: &'a [std::path::PathBuf],
    /// The `"N tracks · M albums"` summary under the rescan row.
    pub summary: Option<(usize, usize)>,
    /// The selected page's id ([`SETTINGS_LIBRARY_PAGE`] or [`SETTINGS_PLAYBACK_PAGE`]).
    pub selected: &'a str,
    /// Whether a narrow (stacked) Settings shows the page rather than the page list.
    pub drilled_in: bool,
    /// Whether the volume-normalization value menu is open.
    pub mode_menu_open: bool,
    /// Spotify, for its page.
    pub spotify: Option<&'a crate::spotify::State>,
}

/// The volume group's title and description, and the mode row's copy.
pub const RG_GROUP_TITLE: &str = "Volume";
pub const RG_GROUP_DESCRIPTION: &str =
    "ReplayGain plays tracks at a similar loudness. Radio streams are unaffected.";
pub const RG_MODE_TITLE: &str = "Normalize volume";
pub const RG_MODE_DESCRIPTION: &str =
    "Track evens out every song, best for shuffle. Album keeps an album's quiet and loud parts.";

/// The preamp group's title and description.
///
/// The quoted range is pinned to the slider bounds by
/// `the_preamp_description_quotes_the_real_range`. The minus sign is U+2212,
/// not a hyphen-minus.
pub const RG_PREAMP_TITLE: &str = "Preamp";
pub const RG_PREAMP_DESCRIPTION: &str =
    "Boost or cut the ReplayGain adjustment (\u{2212}15 to +15 dB).";

/// The clip-protection row's description and title.
///
/// The toggle drives [`crate::domain::ReplayGainState::prevent_clip`], which
/// caps the scalar at `1.0 / peak` in `scalar_for`.
pub const RG_CLIP_DESCRIPTION: &str = "Limit the boost so loud peaks don't distort. Recommended.";
pub const RG_PREVENT_CLIP_LABEL: &str = "Prevent clipping";

/// The search view's result-count line: `"1 result"` / `"N results"`.
pub fn format_search_result_count(total: usize) -> String {
    if total == 1 {
        "1 result".to_string()
    } else {
        format!("{total} results")
    }
}

/// The mode labels for the ReplayGain radio group, in
/// [`ReplayGainMode::SELECTABLE`] order, derived from `ReplayGainMode::label()`.
///
/// `enz_radio_group` selects by index, so the order binds each label to its
/// mode.
pub static RG_MODE_LABELS: std::sync::LazyLock<Vec<&'static str>> =
    std::sync::LazyLock::new(|| {
        ReplayGainMode::SELECTABLE
            .iter()
            .map(|m| m.label())
            .collect()
    });

/// One keyboard step of the preamp slider, in dB: about the smallest change a
/// listener can hear.
pub const RG_PREAMP_STEP_DB: f32 = 0.5;

/// The preamp slider's track length in its Settings row.
const RG_PREAMP_SLIDER_WIDTH: f32 = 160.0;

/// The live dB read-out beside the preamp slider (`+3.5 dB`, `\u{2212}6.0 dB`,
/// `0.0 dB`).
///
/// Positive offsets show `+` because a preamp is a signed offset; the negative
/// sign is U+2212, matching the description.
#[must_use]
pub fn rg_preamp_readout(db: f32) -> String {
    // Round first, so a value that rounds to zero (e.g. -0.04) does not print
    // as `\u{2212}0.0`.
    let rounded = (db * 10.0).round() / 10.0;
    if rounded > 0.0 {
        format!("+{rounded:.1} dB")
    } else if rounded < 0.0 {
        format!("\u{2212}{:.1} dB", rounded.abs())
    } else {
        "0.0 dB".to_string()
    }
}

/// Namespace for [`enz_settings_dialog`]'s per-page control ids in the
/// Settings dialog, so a caller and a test resolve the derived
/// close-button id from the same key.
pub const PLAYBACK_PREFERENCES_ID_PREFIX: &str = "music-app-playback-preferences";

/// Settings over `background`, with its host mounted at rest.
///
/// `present` applies overlay precedence without unmounting the host, keeping
/// the background widget state stable through retained teardown.
#[expect(
    clippy::too_many_arguments,
    reason = "The retained modal composes background, preferences, errors and presentation state."
)]
pub fn with_playback_preferences<'a>(
    background: Element<'a, Message>,
    rg: &'a ReplayGainState,
    enz_theme: &'a EnzTheme,
    window: iced::Size,
    presentation: &enzui_widgets::modal_host::ModalPresentation<()>,
    lifecycle: Option<&enzui_widgets::modal_host::ModalFocusLifecycle>,
    present: bool,
    pages: PreferencesPages<'a>,
) -> Element<'a, Message> {
    if present && presentation.value().is_some() {
        return preferences_overlay_paged(
            background,
            rg,
            enz_theme,
            window,
            presentation,
            lifecycle,
            pages,
        );
    }

    let placement = match enz_settings_presentation_for(window) {
        EnzSettingsPresentation::Sheet => enzui_widgets::modal_host::SurfacePlacement::BottomEdge,
        _ => enzui_widgets::modal_host::SurfacePlacement::Centred,
    };
    enzui_widgets::modal_host::enz_modal_host(background, None, |event| {
        Message::ModalHost(crate::ModalPresentationId::PlaybackPreferences, event)
    })
    .label(RG_TITLE)
    .scope(enzui_widgets::modal_host::presentation_scope_id(
        crate::ModalPresentationId::PlaybackPreferences.scope_key(),
    ))
    .placement(placement)
    .presentation_open(false)
    .into_element()
}

/// One configured library folder: the home-shortened path, and a trash
/// control that removes it (`Message::RemoveFolder`).
fn folder_preferences_row<'a>(folder: &std::path::Path) -> Element<'a, Message> {
    use enzui_widgets::toolbar_icon::enz_toolbar_icon;
    let path = folder.to_path_buf();
    // The label is home-shortened; the id derives from the raw path.
    let shown = crate::library_root::display_folder_path_now(
        folder,
        std::env::var_os("HOME")
            .map(std::path::PathBuf::from)
            .as_deref(),
    );
    enz_preferences_row(shown)
        .icon("folder")
        .control(
            enz_toolbar_icon("trash-2", Message::RemoveFolder(path))
                .aria_label(REMOVE_FOLDER_LABEL)
                .tooltip(REMOVE_FOLDER_LABEL)
                .id(enzui_core::identity::derive_id(
                    "music-app-remove-folder",
                    &folder.display().to_string(),
                ))
                .into_element(),
        )
        .into_element()
}

/// The Settings Library page: the folder list with Add Folder, and the
/// rescan row.
fn library_preferences_page<'a>(
    pages: PreferencesPages<'a>,
    enz: &'a EnzTheme,
) -> Element<'a, Message> {
    let mut folders = column![].width(Length::Fill);
    if pages.folders.is_empty() {
        folders = folders.push(
            enz_text(PREFS_NO_FOLDERS)
                .size(TextSize::Body)
                .color(theme::secondary_text_color(enz)),
        );
    } else {
        for folder in pages.folders {
            folders = folders.push(folder_preferences_row(folder));
        }
    }
    let mut rescan = enz_preferences_row(PREFS_RESCAN_TITLE).control(
        enz_button(PREFS_RESCAN_ACTION, Message::RescanLibrary)
            .icon("refresh-cw")
            .kind(ButtonKind::Quiet)
            .size(ButtonSize::Sm)
            .enabled(!pages.folders.is_empty())
            .id(enzui_core::identity::derive_id(
                "music-preferences",
                "rescan",
            ))
            .into_element(),
    );
    rescan = match pages.summary {
        Some((tracks, albums)) => rescan.subtitle(format!(
            "{} · {}",
            format_count(tracks, "track", "tracks"),
            format_count(albums, "album", "albums"),
        )),
        None => rescan.subtitle(PREFS_RESCAN_SUBTITLE),
    };
    folders = folders.push(
        container(
            enz_button(ADD_FOLDER_LABEL, Message::AddFolder)
                .icon("folder-plus")
                .kind(ButtonKind::Quiet)
                .size(ButtonSize::Sm)
                .id(enzui_core::identity::derive_id(
                    ADD_FOLDER_ID,
                    "preferences",
                ))
                .into_element(),
        )
        .padding(
            Padding::new(0.0)
                .left(enzui_widgets::preferences_row::PreferencesRowTokens::default().padding_x)
                .top(enz.spacing.sm())
                .bottom(enz.spacing.sm()),
        ),
    );
    folders = folders.push(rescan.subtitle_wrap(true).into_element());
    // The page tab already reads "Library"; the group carries only its description.
    enzui_widgets::preferences_group::enz_preferences_group(folders)
        .description(PREFS_FOLDERS_DESCRIPTION)
        .into_element()
}

fn preferences_overlay_paged<'a>(
    background: Element<'a, Message>,
    rg: &'a ReplayGainState,
    enz_theme: &'a EnzTheme,
    window: iced::Size,
    presentation: &enzui_widgets::modal_host::ModalPresentation<()>,
    lifecycle: Option<&enzui_widgets::modal_host::ModalFocusLifecycle>,
    pages: PreferencesPages<'a>,
) -> Element<'a, Message> {
    let enz = enz_theme;
    let exit_key = presentation.key().expect("a retained settings generation");

    // The preset's header row already draws `RG_TITLE`; a leading
    // `enz_text(RG_TITLE)` would publish the title twice.
    let mode_index = rg.mode.selectable_index().unwrap_or(0);
    let mode_row = enz_preferences_row(RG_MODE_TITLE)
        .id(iced::advanced::widget::Id::new("music-settings-rg-mode"))
        .subtitle(RG_MODE_DESCRIPTION)
        .subtitle_wrap(true)
        .value_menu(
            RG_MODE_LABELS[mode_index],
            pages.mode_menu_open,
            Message::RgModeMenu(true),
            Message::RgModeMenu(false),
            RG_MODE_LABELS
                .iter()
                .enumerate()
                .map(|(index, label)| ((*label).to_owned(), Message::RgSelectMode(index)))
                .collect(),
        )
        .into_element();
    let normalizing = rg.mode != ReplayGainMode::Off;
    let preamp_control: Element<'a, Message> = row![
        enz_slider(
            RG_PREAMP_MIN_DB..=RG_PREAMP_MAX_DB,
            rg.preamp_db,
            Message::RgSetPreamp,
        )
        .step(RG_PREAMP_STEP_DB)
        .length(RG_PREAMP_SLIDER_WIDTH)
        .enabled(normalizing)
        .aria_label(RG_PREAMP_TITLE),
        enz_text(rg_preamp_readout(rg.preamp_db))
            .size(TextSize::Caption)
            .color(theme::secondary_text_color(enz))
            .into_element(),
    ]
    .spacing(enz.spacing.sm())
    .align_y(Alignment::Center)
    .into();
    let preamp_row = enz_preferences_row(RG_PREAMP_TITLE)
        .subtitle(RG_PREAMP_DESCRIPTION)
        .subtitle_wrap(true)
        .enabled(normalizing)
        .control(preamp_control)
        .into_element();
    let clip_row = enz_preferences_row(RG_PREVENT_CLIP_LABEL)
        .subtitle(RG_CLIP_DESCRIPTION)
        .subtitle_wrap(true)
        .enabled(normalizing)
        .switch(rg.prevent_clip, Message::RgSetPreventClip)
        .into_element();
    let playback: Element<'a, Message> = enzui_widgets::preferences_group::enz_preferences_group(
        column![mode_row, preamp_row, clip_row].width(Length::Fill),
    )
    .title(RG_GROUP_TITLE)
    .description(RG_GROUP_DESCRIPTION)
    .into_element();

    // The preset owns the header row, and the modal host publishes the same
    // string as its `Role::Dialog` name. Sheets use the 85% detent because a
    // content-hugging sheet left the scroll viewport too shallow.
    let dialog = enz_settings_dialog::<Message>(true, window)
        .title(RG_TITLE)
        .size(DialogSize::Compact)
        .presentation(EnzSettingsPresentation::Adaptive)
        .presentation_open(presentation.is_open())
        .on_exit_settled(move || Message::PlaybackPreferencesExitSettled(exit_key))
        .page(
            SettingsPage::new(SETTINGS_LIBRARY_PAGE, PREFS_LIBRARY_TITLE).icon("library"),
            library_preferences_page(pages, enz),
        )
        .page(
            SettingsPage::new(SETTINGS_PLAYBACK_PAGE, RG_PAGE_TITLE).icon("volume-2"),
            playback,
        )
        .page(
            SettingsPage::new(SETTINGS_SPOTIFY_PAGE, "Spotify").icon("audio-lines"),
            match pages.spotify {
                Some(spotify) => spotify.settings_page(enz),
                None => Space::new().into(),
            },
        )
        .selected(pages.selected)
        .on_select(Message::SelectSettingsPage)
        .stacked_navigation(pages.drilled_in, Message::SettingsStackedBack)
        .page_id_prefix(PLAYBACK_PREFERENCES_ID_PREFIX);
    // Tab routes through the owned lifecycle, scoped to this presentation.
    let mut host = dialog.into_modal_host(background, |event| {
        let host = match event {
            enzui_widgets::settings_dialog::SettingsDialogEvent::Dismissed(cause) => {
                enzui_widgets::modal_host::ModalHostEvent::Dismissed(cause)
            }
            enzui_widgets::settings_dialog::SettingsDialogEvent::FocusNext => {
                enzui_widgets::modal_host::ModalHostEvent::FocusNext
            }
            enzui_widgets::settings_dialog::SettingsDialogEvent::FocusPrevious => {
                enzui_widgets::modal_host::ModalHostEvent::FocusPrevious
            }
        };
        Message::ModalHost(crate::ModalPresentationId::PlaybackPreferences, host)
    });
    if presentation.is_open() {
        host = host.scoped_to(lifecycle.expect("open preferences have no focus lifecycle"));
    }
    host.into_element()
}
