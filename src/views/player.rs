use super::*;
use iced::widget::{column, row};

pub(crate) const NO_AUDIO_OUTPUT_TITLE: &str = "No audio output";
pub(crate) const NO_AUDIO_OUTPUT_BODY: &str = "The audio service could not start. Library browsing is still available, but playback is unavailable.";

/// The current track's availability disclosure and recovery actions.
///
/// Shown when a probe proved the current library track
/// [`Missing`](crate::domain::TrackAvailability::Missing) or
/// [`Undecodable`](crate::domain::TrackAvailability::Undecodable): one
/// path-free disclosure with Rescan and Remove (which never deletes the file).
/// Otherwise a `1px` `Space`.
fn current_availability_disclosure<'a>(
    playback: &PlaybackState,
    library: &Library,
    content_w: f32,
    enz_theme: &'a EnzTheme,
) -> Element<'a, Message> {
    use enzui_widgets::banner::{BannerTone, enz_banner};

    let Some(id) = playback.current_track_id else {
        return Space::new().height(1).into();
    };
    let state = library.availability_of(id);
    if !state.offers_recovery() {
        return Space::new().height(1).into();
    }
    let (Some(text), Some(title)) = (state.disclosure(), state.badge_label()) else {
        return Space::new().height(1).into();
    };
    let banner = enz_banner(title)
        .body(text)
        .tone(BannerTone::Warning)
        .dismissible(false)
        .bordered(false)
        .id_prefix("music-track-availability")
        .width(Length::Fill)
        .into_element();
    let rescan = enz_button("Rescan", Message::RescanTrack(id))
        .kind(ButtonKind::Quiet)
        .size(ButtonSize::Sm)
        .into_element();
    let remove = enz_button("Remove from library", Message::RemoveTrack(id))
        .kind(ButtonKind::QuietDestructive)
        .size(ButtonSize::Sm)
        .into_element();
    let actions = row![rescan, remove]
        .spacing(enz_theme.spacing.sm())
        .align_y(Alignment::Center);
    container(
        column![banner, actions]
            .spacing(enz_theme.spacing.xs())
            .width(Length::Fill),
    )
    .width(content_w)
    .into()
}

/// Lyrics where the cover sits: timed lyrics follow the song with the line
/// being sung lit and a few around it; plain lyrics scroll.
fn lyrics_block<'a>(
    view: Option<crate::spotify::LyricsView<'_>>,
    width: f32,
    height: f32,
    enz_theme: &'a EnzTheme,
) -> Element<'a, Message> {
    use crate::spotify::Lyrics;
    let tc = theme::text_color(enz_theme);
    let sc = theme::secondary_text_color(enz_theme);
    let height = height.max(120.0);
    let note = |text: &'static str| -> Element<'a, Message> {
        container(
            enz_text(text)
                .size(TextSize::Body)
                .color(sc)
                .align(TextAlign::Center)
                .full_width(),
        )
        .center_x(Length::Fixed(width))
        .center_y(Length::Fixed(height))
        .into()
    };
    let Some(view) = view else {
        return container(enzui_widgets::spinner::enz_spinner())
            .center_x(Length::Fixed(width))
            .center_y(Length::Fixed(height))
            .into();
    };
    match view.lyrics {
        Lyrics::Loading => container(enzui_widgets::spinner::enz_spinner())
            .center_x(Length::Fixed(width))
            .center_y(Length::Fixed(height))
            .into(),
        Lyrics::Missing => note("No lyrics found for this song."),
        Lyrics::Found {
            lines,
            synced: true,
        } => {
            // Fixed line heights, so the lit line moves predictably.
            const LINE: f32 = 34.0;
            let rows = ((height / LINE).floor() as usize).max(3);
            let current = view.current;
            // The lit line sits a third of the way down; before the first
            // line, the start shows.
            let start = current.map_or(0, |c| c.saturating_sub(rows / 3));
            let mut column = iced::widget::Column::new().width(Length::Fixed(width));
            for (index, line) in lines.iter().enumerate().skip(start).take(rows) {
                let text = line.text.trim();
                let lit = Some(index) == current;
                let text = if text.is_empty() { "\u{266a}" } else { text };
                column = column.push(
                    container(
                        enz_text(text.to_owned())
                            .size(if lit {
                                TextSize::Title3
                            } else {
                                TextSize::Body
                            })
                            .color(if lit { tc } else { sc })
                            .align(TextAlign::Center)
                            .full_width()
                            .single_line_elided(),
                    )
                    .height(Length::Fixed(LINE))
                    .center_y(Length::Fixed(LINE)),
                );
            }
            container(column)
                .width(Length::Fixed(width))
                .height(Length::Fixed(height))
                .clip(true)
                .into()
        }
        Lyrics::Found {
            lines,
            synced: false,
        } => {
            let mut column = iced::widget::Column::new()
                .spacing(enz_theme.spacing.xs())
                .width(Length::Fill);
            for line in lines {
                column = column.push(
                    enz_text(line.text.trim().to_owned())
                        .size(TextSize::Body)
                        .color(tc)
                        .align(TextAlign::Center)
                        .full_width()
                        .wrap(true),
                );
            }
            container(enz_scroll_view(column).height(Length::Fill))
                .width(Length::Fixed(width))
                .height(Length::Fixed(height))
                .into()
        }
    }
}

/// The availability disclosure, for library playback only.
fn availability<'a>(
    remote: bool,
    playback: &PlaybackState,
    library: &Library,
    content_w: f32,
    enz_theme: &'a EnzTheme,
) -> Element<'a, Message> {
    if remote {
        return Space::new().height(1).into();
    }
    current_availability_disclosure(playback, library, content_w, enz_theme)
}

// ---------------------------------------------------------------------------
// Another source: Spotify
// ---------------------------------------------------------------------------

/// What the player shows while Spotify is the source: Spotify's state and
/// its cover. The controls send the app's usual transport messages, which
/// the app routes to Spotify.
pub struct RemoteView<'a> {
    pub spotify: crate::spotify::Remote<'a>,
    pub art: Option<&'a crate::art::CoverArt>,
}

impl RemoteView<'_> {
    fn transport(&self) -> Transport {
        use crate::spotify::Repeat;
        let s = &self.spotify;
        Transport {
            playing: s.playing,
            shuffle: s.shuffle,
            repeat: match s.repeat {
                Repeat::Off => ("repeat", "Repeat off", false),
                Repeat::Context => ("repeat", "Repeat all", true),
                Repeat::Track => ("repeat-1", "Repeat one", true),
            },
            connecting: false,
            full: true,
        }
    }
}

/// The transport's state, from whichever source plays.
#[derive(Clone, Copy)]
struct Transport {
    playing: bool,
    shuffle: bool,
    /// Icon, label and whether repeat is on.
    repeat: (&'static str, &'static str, bool),
    connecting: bool,
    /// Shuffle, previous, next and repeat around play (not for a stream).
    full: bool,
}

impl Transport {
    fn local(playback: &PlaybackState, has_track: bool) -> Self {
        Self {
            playing: playback.playing,
            shuffle: playback.shuffle,
            repeat: match playback.repeat {
                crate::RepeatMode::Off => ("repeat", "Repeat off", false),
                crate::RepeatMode::All => ("repeat", "Repeat all", true),
                crate::RepeatMode::One => ("repeat-1", "Repeat one", true),
            },
            connecting: !has_track && playback.stream_phase == crate::StreamPhase::Loading,
            full: has_track,
        }
    }
}

// ---------------------------------------------------------------------------
// Transport controls — framework circular icon buttons
// ---------------------------------------------------------------------------

/// A secondary transport action with explicit geometry and identity.
pub(super) fn transport_icon_sized<'a>(
    icon: &'a str,
    aria: &'a str,
    on_press: Message,
    size: f32,
    id: iced::advanced::widget::Id,
    enz_theme: &'a EnzTheme,
) -> Element<'a, Message> {
    enzui_widgets::toolbar_icon::enz_toolbar_icon(icon, on_press)
        .aria_label(aria)
        .tooltip(aria)
        .size(size, enz_theme.sizing.icon_sm)
        .id(id)
        .into_element()
}

// ---------------------------------------------------------------------------
// Now Playing panel
// ---------------------------------------------------------------------------

/// Compose the now-playing panel.
///
/// A missing audio service is disclosed by the app's standing bottom-right notice
/// (`MusicApp::service_notices`), not inside the panel.
///
/// `stream` is the resolved [`StreamNowPlaying`] for a tuned station or a
/// selected episode, or `None` when the current thing is a library
/// track or nothing at all. The caller resolves it (`domain::resolve_stream`)
/// because this view has no access to the radio catalog or the subscriptions
/// list; `PlaybackState::stream_selection` decides whether one is current.
#[allow(clippy::too_many_arguments)]
pub fn now_playing_panel<'a>(
    playback: &PlaybackState,
    library: &'a Library,
    stream: Option<&StreamNowPlaying>,
    stream_art: Option<&'a crate::art::CoverArt>,
    waveform: Option<&'a [f32]>,
    seek_preview: Option<f64>,
    enz_theme: &'a EnzTheme,
    frame: NowPlayingFrame,
    remote: Option<&RemoteView<'a>>,
) -> Element<'a, Message> {
    let tc = theme::text_color(enz_theme);
    let sc = theme::secondary_text_color(enz_theme);

    let layout = NowPlayingLayout::compute(frame.width, NOW_PLAYING_CONTENT_W, NOW_PLAYING_H_PAD);
    // Side by side, the controls keep the portrait column's width.
    let content_w = if frame.landscape {
        NP_LANDSCAPE_CONTROLS_W
    } else {
        layout.content_width
    };

    let track = if remote.is_some() {
        None
    } else {
        playback.current_track(library)
    };
    let stream = if remote.is_some() { None } else { stream };

    // The art and the waveform give up height before any control does, so
    // the panel fits without scrolling and volume never drops out.
    let sizing = if frame.landscape {
        landscape_sizing(frame.height, enz_theme)
    } else {
        portrait_sizing(frame.height, layout.album_art_size, enz_theme)
    };
    let art_size = sizing.art.unwrap_or(0.0);
    let art_bg = theme::overlay_hover_color(enz_theme);
    // The shared pre-decoded thumbnail, so selecting a track decodes nothing.
    // A station's logo or an episode's cover stands in for album art.
    let cover = match (remote, track) {
        (Some(r), _) => r.art,
        (None, Some(t)) => t.cover.as_ref(),
        (None, None) if stream.is_some() => stream_art,
        (None, None) => None,
    };
    // A station logo shows whole; covers fill the square.
    let cover_fit = if remote.is_none()
        && track.is_none()
        && matches!(
            playback.stream_selection(),
            Some(StreamSelection::Station(_))
        ) {
        EnzImageFit::Contain
    } else {
        EnzImageFit::Cover
    };
    let album_art: Element<'a, Message> = match cover {
        Some(art) => enz_image_handle(art.handle().clone())
            .fit(cover_fit)
            .radius(enz_theme.radius.xl)
            .alt_text(format!(
                "{} album art",
                match remote {
                    Some(r) => r
                        .spotify
                        .track
                        .album
                        .as_ref()
                        .map_or("", |a| a.name.as_str()),
                    None => track.map(|t| t.display_album()).unwrap_or(""),
                }
            ))
            .width(Length::Fixed(art_size))
            .height(Length::Fixed(art_size))
            .placeholder_background(art_bg)
            .into_element(),
        None => {
            use enzui_widgets::surface::{RadiusSize, SurfaceTone, enz_surface};
            enz_surface(
                container(
                    enz_icon("music-2")
                        .size(enz_theme.sizing.icon_xxl)
                        .tint(theme::secondary_text_color(enz_theme))
                        .into_element(),
                )
                .center_x(art_size)
                .center_y(art_size),
            )
            .tone(SurfaceTone::Selected)
            .radius_size(RadiusSize::Xl)
            .width(art_size)
            .height(art_size)
            .into_element()
        }
    };

    // Spotify's lyrics take the cover's place while shown.
    let album_art: Element<'a, Message> = match remote.filter(|r| r.spotify.lyrics_shown) {
        Some(r) => lyrics_block(
            r.spotify.lyrics,
            if frame.landscape { art_size } else { content_w },
            art_size,
            enz_theme,
        ),
        None => album_art,
    };

    // A tuned station or selected episode fills the same three lines.
    let title_str = match (remote, track, stream) {
        (Some(r), ..) => r.spotify.track.name.clone(),
        (None, Some(t), _) => t.display_title().to_string(),
        (None, None, Some(s)) => s.title.clone(),
        (None, None, None) => "Nothing playing".into(),
    };
    let artist_str = match (remote, track, stream) {
        (Some(r), ..) => r.spotify.track.artist_names(),
        (None, Some(t), _) => t.display_artist().to_string(),
        (None, None, Some(s)) => s.subtitle.clone(),
        (None, None, None) => "—".into(),
    };
    let album_str = match (remote, track, stream) {
        (Some(r), ..) => r
            .spotify
            .track
            .album
            .as_ref()
            .map_or_else(String::new, |a| a.name.clone()),
        (None, Some(t), _) => t.display_album().to_string(),
        (None, None, Some(s)) => s.detail.clone(),
        (None, None, None) => "—".into(),
    };

    // Elided user strings publish their full text through the elision audit.
    // A Spotify artist or album opens its page.
    let line = |text: String,
                size: TextSize,
                color,
                open: Option<Message>,
                key: &str|
     -> Element<'a, Message> {
        let text = enz_text(text)
            .size(size)
            .color(color)
            .full_width()
            .align(TextAlign::Center)
            .single_line_elided();
        let _ = key;
        match open {
            // A pointer target the text's own size, so the lines keep their rhythm.
            Some(message) => iced::widget::mouse_area(text)
                .on_press(message)
                .interaction(iced::mouse::Interaction::Pointer)
                .into(),
            None => text.into(),
        }
    };
    let open_artist = remote
        .and_then(|r| r.spotify.track.artists.first())
        .map(|a| {
            Message::Spotify(crate::spotify::Event::Open(crate::spotify::Route::Artist(
                a.uri.clone(),
            )))
        });
    let open_album = remote
        .and_then(|r| r.spotify.track.album.as_ref())
        .map(|a| {
            Message::Spotify(crate::spotify::Event::Open(crate::spotify::Route::Album(
                a.uri.clone(),
            )))
        });
    let title_stack = column![
        line(title_str, TextSize::Title3, tc, None, "title"),
        line(artist_str, TextSize::Body, sc, open_artist, "artist"),
        line(album_str, TextSize::Footnote, sc, open_album, "album"),
    ]
    .spacing(enz_theme.spacing.xs())
    .align_x(Alignment::Center)
    .width(content_w);

    let has_track = remote.is_some() || playback.current_track_id.is_some();
    let live = remote.is_none()
        && matches!(
            playback.stream_selection(),
            Some(StreamSelection::Station(_))
        );

    // The seek row: the track's waveform, played part lit, with the elapsed
    // and remaining time under it. A live station has no position to seek.
    let seek: Element<'a, Message> = if live {
        container(
            enz_text("Live")
                .size(TextSize::Caption)
                .color(sc)
                .align(TextAlign::Center)
                .full_width(),
        )
        .width(content_w)
        .into()
    } else {
        // Spotify offers no waveform: even bars stand in.
        let (peaks, position, duration) = match remote {
            Some(r) => (None, r.spotify.position, r.spotify.duration),
            None => (waveform, playback.position_secs, playback.duration_secs),
        };
        // A slim bar (no waveform to come) needs only a pointer's height.
        let slim = remote.is_some() || stream.is_some();
        waveform_seek(
            peaks,
            seek_preview.unwrap_or(position),
            duration,
            content_w,
            if slim {
                sizing.wave.min(24.0)
            } else {
                sizing.wave
            },
            enz_theme,
        )
    };

    // Secondary actions sit apart at the panel's foot.
    let mut actions = row![].align_y(Alignment::Center).width(content_w);
    if let Some(r) = remote {
        let liked = r.spotify.liked.unwrap_or(false);
        actions = actions.push(transport_icon_sized(
            if liked { "heart.fill" } else { "heart" },
            if liked {
                "Remove from Liked Songs"
            } else {
                "Add to Liked Songs"
            },
            Message::Spotify(crate::spotify::Event::ToggleSaved(
                r.spotify.track.uri.clone(),
            )),
            36.0,
            enzui_core::identity::derive_id("music-player", "spotify-like"),
            enz_theme,
        ));
        let label = if r.spotify.lyrics_shown {
            "Hide lyrics"
        } else {
            "Show lyrics"
        };
        actions = actions.push(
            enzui_widgets::toolbar_icon::enz_toolbar_icon(
                "mic-vocal",
                Message::Spotify(crate::spotify::Event::ToggleLyrics),
            )
            .size(36.0, enz_theme.sizing.icon_sm)
            .active(r.spotify.lyrics_shown)
            .aria_label(label)
            .tooltip(label)
            .id(enzui_core::identity::derive_id(
                "music-player",
                "spotify-lyrics",
            ))
            .into_element(),
        );
    } else if has_track {
        actions = actions.push(transport_icon_sized(
            "list-plus",
            "Add current track to playlist",
            Message::OpenPlaylistPicker(PickerTarget::CurrentTrack),
            36.0,
            enzui_core::identity::derive_id("music-player", "add-to-playlist"),
            enz_theme,
        ));
    }
    actions = actions.push(Space::new().width(Length::Fill));
    if let Some(r) = remote {
        let device = if r.spotify.local {
            "This computer".to_owned()
        } else {
            r.spotify.device.unwrap_or("Spotify").to_owned()
        };
        actions = actions.push(
            enz_button(device, Message::OpenSpotifyDevices)
                .icon(if r.spotify.local { "laptop" } else { "speaker" })
                .kind(ButtonKind::Quiet)
                .size(ButtonSize::Sm)
                .width(Length::Shrink)
                .id(enzui_core::identity::derive_id(
                    "music-player",
                    "spotify-device",
                ))
                .into_element(),
        );
    }
    if remote.is_none() && has_track && !playback.queue.is_empty() {
        actions = actions.push(transport_icon_sized(
            "list-music",
            "Up Next",
            Message::SelectView(View::Queue),
            36.0,
            enzui_core::identity::derive_id("music-player", "up-next"),
            enz_theme,
        ));
    }

    let body: Element<'a, Message> = if has_track || stream.is_some() {
        column![
            seek,
            title_stack,
            now_playing_transport(
                remote.map_or_else(
                    || Transport::local(playback, has_track),
                    RemoteView::transport
                ),
                enz_theme
            ),
            match remote {
                Some(r) => volume_row(
                    r.spotify.volume.unwrap_or(0.0) as f32,
                    r.spotify.volume.is_none_or(|v| v <= 0.0),
                    r.spotify.volume.is_some(),
                    content_w,
                    enz_theme,
                ),
                None => volume_row(playback.volume, playback.muted, true, content_w, enz_theme,),
            },
            actions,
        ]
        .spacing(enz_theme.spacing.lg())
        .align_x(Alignment::Center)
        .width(Length::Fill)
        .into()
    } else {
        container(empty_state(
            "music-2",
            "Pick a track to start listening",
            "Choose an album, artist or track from the library to start playback.",
        ))
        .width(content_w)
        .into()
    };

    let album_art: Option<Element<'a, Message>> = sizing.art.map(|_| album_art);
    let panel_content: Element<'a, Message> = if frame.landscape {
        column![
            availability(remote.is_some(), playback, library, content_w, enz_theme),
            row![]
                .extend(album_art)
                .push(container(body).width(content_w))
                .spacing(enz_theme.spacing.xl())
                .align_y(Alignment::Center),
        ]
        .spacing(enz_theme.spacing.lg())
        .align_x(Alignment::Center)
        .width(Length::Fill)
        .into()
    } else {
        column![availability(
            remote.is_some(),
            playback,
            library,
            content_w,
            enz_theme
        ),]
        .extend(album_art)
        .push(body)
        .spacing(enz_theme.spacing.lg())
        .align_x(Alignment::Center)
        .width(Length::Fill)
        .into()
    };

    // Transparent, so the sheet's frosted glass shows through; centred in
    // whatever height the sheet has.
    container(panel_content)
        .padding(
            Padding::new(0.0)
                .left(NOW_PLAYING_H_PAD)
                .right(NOW_PLAYING_H_PAD),
        )
        .width(frame.width)
        .height(Length::Fill)
        .center_y(Length::Fill)
        .into()
}

/// Where the now-playing panel lays out: its size inside the sheet, and
/// whether the art sits beside the controls (a wide, short window) or above
/// them.
#[derive(Clone, Copy, Debug)]
pub struct NowPlayingFrame {
    pub width: f32,
    pub height: f32,
    pub landscape: bool,
}

/// The controls column's width beside the art.
const NP_LANDSCAPE_CONTROLS_W: f32 = 360.0;

/// Below this art edge a portrait sheet reads as a thumbnail; a window wide
/// enough for [`now_playing_landscape_width`] puts the art beside the
/// controls instead.
const NP_PORTRAIT_ART_FLOOR: f32 = 200.0;

/// The landscape sheet's width: the largest art, the gap and the controls,
/// with the panel's side padding.
pub fn now_playing_landscape_width(enz_theme: &EnzTheme) -> f32 {
    NP_ART_MAX + enz_theme.spacing.xl() + NP_LANDSCAPE_CONTROLS_W + 2.0 * NOW_PLAYING_H_PAD
}

/// The now-playing sheet's frame for a window: portrait while the art keeps
/// a real size above the controls, landscape when the window is too short
/// for that but wide enough to set the art beside them.
pub fn now_playing_frame(
    window: iced::Size,
    max_height: f32,
    chrome: f32,
    enz_theme: &EnzTheme,
) -> (NowPlayingFrame, f32) {
    let portrait_height = (now_playing_natural_height(enz_theme) + chrome).min(max_height);
    let portrait_art = portrait_sizing(portrait_height - chrome, f32::INFINITY, enz_theme)
        .art
        .unwrap_or(0.0);
    let landscape_width = now_playing_landscape_width(enz_theme);
    if portrait_art < NP_PORTRAIT_ART_FLOOR && window.width >= landscape_width + 32.0 {
        let height = (now_playing_controls_height(WAVE_H, enz_theme).max(NP_ART_MAX)
            + chrome
            + enz_theme.spacing.lg())
        .min(max_height);
        return (
            NowPlayingFrame {
                width: landscape_width,
                height: height - chrome,
                landscape: true,
            },
            height,
        );
    }
    (
        NowPlayingFrame {
            width: window.width.min(NP_PORTRAIT_SHEET_W),
            height: portrait_height - chrome,
            landscape: false,
        },
        portrait_height,
    )
}

/// The portrait sheet's widest.
pub const NP_PORTRAIT_SHEET_W: f32 = 520.0;

/// The album art's largest and smallest edge in the now-playing panel.
const NP_ART_MAX: f32 = 380.0;
/// The art keeps this edge while the waveform shrinks to make room.
const NP_ART_MIN: f32 = 120.0;
/// Below this edge the art is left out rather than shown as a speck.
const NP_ART_FLOOR: f32 = 64.0;

/// How the panel spends its height: the art's edge (`None`: left out) and
/// the waveform's height.
#[derive(Clone, Copy, Debug)]
struct NowPlayingSizing {
    art: Option<f32>,
    wave: f32,
}

/// The height of the five rows under the art (seek, titles, transport,
/// volume, actions) and the gaps between them, with a `wave` tall waveform.
fn now_playing_controls_height(wave: f32, enz_theme: &EnzTheme) -> f32 {
    let seek = wave + enz_theme.spacing.xs() + 18.0;
    let titles = 28.0 + 22.0 + 20.0 + 2.0 * enz_theme.spacing.xs();
    seek + titles + NP_PLAY_SIZE + 32.0 + 36.0 + 4.0 * enz_theme.spacing.lg()
}

/// Portrait: the art takes what the controls leave, down to
/// [`NP_ART_MIN`]; then the waveform gives up height, down to
/// [`WAVE_MIN`]; then the art shrinks again, and below [`NP_ART_FLOOR`] it
/// is left out. The controls never shrink.
fn portrait_sizing(height: f32, widest_art: f32, enz_theme: &EnzTheme) -> NowPlayingSizing {
    let gap = enz_theme.spacing.lg();
    let room = |wave: f32| height - now_playing_controls_height(wave, enz_theme) - gap;
    let art = room(WAVE_H);
    if art >= NP_ART_MIN {
        return NowPlayingSizing {
            art: Some(art.min(NP_ART_MAX).min(widest_art)),
            wave: WAVE_H,
        };
    }
    let wave = (WAVE_H - (NP_ART_MIN - art)).max(WAVE_MIN);
    let art = room(wave);
    if art >= NP_ART_FLOOR {
        return NowPlayingSizing {
            art: Some(art.min(widest_art)),
            wave,
        };
    }
    // No art: the waveform takes back what it can.
    let wave = (height - now_playing_controls_height(0.0, enz_theme)).clamp(WAVE_MIN, WAVE_H);
    NowPlayingSizing { art: None, wave }
}

/// Landscape: the art fills the height beside the controls, and the
/// waveform shrinks if the controls would not fit.
fn landscape_sizing(height: f32, enz_theme: &EnzTheme) -> NowPlayingSizing {
    NowPlayingSizing {
        art: Some(height.clamp(NP_ART_FLOOR, NP_ART_MAX)),
        wave: (height - now_playing_controls_height(0.0, enz_theme)).clamp(WAVE_MIN, WAVE_H),
    }
}

/// The now-playing panel's height at the largest art, which the sheet hugs
/// when the window has room.
pub fn now_playing_natural_height(enz_theme: &EnzTheme) -> f32 {
    now_playing_controls_height(WAVE_H, enz_theme) + enz_theme.spacing.lg() + NP_ART_MAX
}

/// The waveform seek bar's height, and the least it shrinks to.
const WAVE_H: f32 = 64.0;
const WAVE_MIN: f32 = 32.0;

/// A seek bar drawn as the track's waveform: bars before the position are
/// lit. A transparent slider over it takes the pointer and the keyboard and
/// names the control for assistive technology; without a waveform (a podcast
/// episode, an undecodable file, or while decoding) the bars are even.
fn waveform_seek<'a>(
    peaks: Option<&'a [f32]>,
    position: f64,
    duration: f64,
    width: f32,
    wave_h: f32,
    enz_theme: &'a EnzTheme,
) -> Element<'a, Message> {
    let seekable = duration.is_finite() && duration > 0.0;
    let progress = if seekable {
        (position / duration).clamp(0.0, 1.0) as f32
    } else {
        0.0
    };
    let rest = theme::secondary_text_color(enz_theme);
    let wave = iced::widget::canvas(Waveform {
        peaks,
        progress,
        played: theme::text_color(enz_theme),
        rest: iced::Color {
            a: rest.a * 0.45,
            ..rest
        },
    })
    .width(Length::Fill)
    .height(Length::Fixed(wave_h));

    let clear = iced::Color::TRANSPARENT;
    let tokens = enzui_widgets::slider::SliderTokens {
        track: clear,
        track_border: clear,
        fill: clear,
        fill_hover: clear,
        thumb: clear,
        thumb_border: clear,
        thumb_shadow: clear,
        thumb_dragging: clear,
        ..enzui_widgets::slider::SliderTokens::from_palette(&enz_theme.palette)
    };
    let slider = enz_slider(
        0.0..=(if seekable { duration as f32 } else { 1.0 }),
        if seekable { position as f32 } else { 0.0 },
        |secs| Message::SeekPreview(f64::from(secs)),
    )
    // A drag previews and seeks once on release; a click or key seeks at once.
    .on_drag_start(Message::SeekDragStart)
    .on_release(Message::SeekCommit)
    .aria_label("Seek")
    .id(enzui_core::identity::derive_id("music-player", "seek"))
    .enabled(seekable)
    .tokens(tokens)
    .track_height(wave_h)
    .thumb_width(4.0)
    .thumb_height(wave_h)
    .into_element();

    let times = row![
        enz_text(format_duration(position.max(0.0)))
            .size(TextSize::Caption)
            .color(rest),
        Space::new().width(Length::Fill),
        enz_text(if seekable {
            format!(
                "\u{2212}{}",
                format_duration((duration - position).max(0.0))
            )
        } else {
            String::new()
        })
        .size(TextSize::Caption)
        .color(rest),
    ]
    .width(Length::Fill);

    column![
        iced::widget::stack![
            wave,
            container(slider)
                .width(Length::Fill)
                .height(Length::Fixed(wave_h))
                .center_y(Length::Fixed(wave_h)),
        ]
        .width(Length::Fill)
        .height(Length::Fixed(wave_h)),
        times,
    ]
    .spacing(enz_theme.spacing.xs())
    .width(width)
    .into()
}

/// The waveform's bars, drawn into the seek row.
struct Waveform<'a> {
    peaks: Option<&'a [f32]>,
    progress: f32,
    played: iced::Color,
    rest: iced::Color,
}

impl iced::widget::canvas::Program<Message> for Waveform<'_> {
    type State = ();

    fn frame_interest(&self, _state: &Self::State) -> bool {
        false
    }

    fn draw(
        &self,
        _state: &Self::State,
        renderer: &iced::Renderer,
        _theme: &iced::Theme,
        bounds: iced::Rectangle,
        _cursor: iced::mouse::Cursor,
    ) -> Vec<iced::widget::canvas::Geometry> {
        use iced::widget::canvas::{Frame, Path};
        let mut frame = Frame::new(renderer, bounds.size());
        // No waveform (a stream, Spotify, or still decoding): a slim bar.
        if self.peaks.is_none() {
            let h = 6.0_f32.min(bounds.height);
            let y = (bounds.height - h) / 2.0;
            let radius = (h / 2.0).into();
            frame.fill(
                &Path::rounded_rectangle(
                    iced::Point::new(0.0, y),
                    iced::Size::new(bounds.width, h),
                    radius,
                ),
                self.rest,
            );
            let played = bounds.width * self.progress.clamp(0.0, 1.0);
            if played > 0.0 {
                frame.fill(
                    &Path::rounded_rectangle(
                        iced::Point::new(0.0, y),
                        iced::Size::new(played.max(h), h),
                        radius,
                    ),
                    self.played,
                );
            }
            return vec![frame.into_geometry()];
        }
        let bars = crate::waveform::BARS;
        let step = bounds.width / bars as f32;
        let bar_w = (step * 0.55).max(1.5);
        for bar in 0..bars {
            // Even bars stand in until (or unless) a waveform decodes.
            let level = self
                .peaks
                .and_then(|peaks| peaks.get(bar).copied())
                .unwrap_or(0.35);
            let height = (level.clamp(0.0, 1.0) * bounds.height).max(bar_w);
            let x = bar as f32 * step + (step - bar_w) / 2.0;
            let y = (bounds.height - height) / 2.0;
            let lit = (bar as f32 + 0.5) / bars as f32 <= self.progress;
            frame.fill(
                &Path::rounded_rectangle(
                    iced::Point::new(x, y),
                    iced::Size::new(bar_w, height),
                    (bar_w / 2.0).into(),
                ),
                if lit { self.played } else { self.rest },
            );
        }
        vec![frame.into_geometry()]
    }
}

/// The now-playing transport: shuffle, previous, a large play, next and
/// repeat for library playback; play alone for a stream.
fn now_playing_transport<'a>(state: Transport, enz_theme: &'a EnzTheme) -> Element<'a, Message> {
    use enzui_widgets::toolbar_icon::enz_toolbar_icon;
    let (icon, label) = if state.playing {
        ("pause", "Pause")
    } else {
        ("play", "Play")
    };
    let connecting = state.connecting;
    let play: Element<'a, Message> = if connecting {
        container(enzui_widgets::spinner::enz_spinner().size(enz_theme.sizing.icon_lg))
            .center_x(Length::Fixed(NP_PLAY_SIZE))
            .center_y(Length::Fixed(NP_PLAY_SIZE))
            .into()
    } else {
        use enzui_widgets::surface::{SurfaceTone, enz_surface};
        enz_surface(
            enz_toolbar_icon(icon, Message::TogglePlayPause)
                .size(NP_PLAY_SIZE, enz_theme.sizing.icon_lg)
                .aria_label(label)
                .tooltip(label)
                .id(enzui_core::identity::derive_id("music-np", "play"))
                .into_element(),
        )
        // The text colour, faint: a soft disc on the sheet in either theme,
        // where surface tones vanished against the light sheet.
        .tone(SurfaceTone::Selected)
        .fill_resolved(|theme| ink(theme, 0.1))
        .radius(NP_PLAY_SIZE / 2.0)
        .border_width(0.0)
        .into_element()
    };
    if !state.full {
        return play;
    }
    let small = |icon: &'static str, label: &'static str, message: Message, key: &str| {
        enz_toolbar_icon(icon, message)
            .size(40.0, enz_theme.sizing.icon_md)
            .aria_label(label)
            .tooltip(label)
            .id(enzui_core::identity::derive_id("music-np", key))
    };
    let shuffle_label = if state.shuffle {
        "Shuffle on"
    } else {
        "Shuffle off"
    };
    let (repeat_icon, repeat_label, repeat_on) = state.repeat;
    row![
        small("shuffle", shuffle_label, Message::ToggleShuffle, "shuffle")
            .active(state.shuffle)
            .into_element(),
        Space::new().width(Length::Fill),
        small(
            "skip-back",
            "Previous track",
            Message::PreviousTrack,
            "previous"
        )
        .into_element(),
        play,
        small("skip-forward", "Next track", Message::NextTrack, "next").into_element(),
        Space::new().width(Length::Fill),
        small(repeat_icon, repeat_label, Message::CycleRepeat, "repeat")
            .active(repeat_on)
            .into_element(),
    ]
    .spacing(enz_theme.spacing.sm())
    .align_y(Alignment::Center)
    .width(Length::Fixed(NOW_PLAYING_CONTENT_W.min(420.0)))
    .into()
}

/// The now-playing play control's edge.
const NP_PLAY_SIZE: f32 = 56.0;

/// Volume on its own row: a mute toggle, a thick slider, and the loud end.
fn volume_row<'a>(
    volume: f32,
    muted: bool,
    enabled: bool,
    width: f32,
    enz_theme: &'a EnzTheme,
) -> Element<'a, Message> {
    let silent = muted || volume <= 0.0;
    let (icon, label) = if silent {
        ("volume-x", "Unmute")
    } else {
        ("volume-1", "Mute")
    };
    let slider = enz_slider(0.0..=1.0, if muted { 0.0 } else { volume }, |volume| {
        Message::SetVolume(f64::from(volume))
    })
    .enabled(enabled)
    .step(0.01)
    .tokens(enzui_widgets::slider::SliderTokens {
        // A faint text-coloured rail, visible on the sheet in either theme.
        track: iced::Color {
            a: 0.16,
            ..enz_theme.palette.text
        },
        ..enzui_widgets::slider::SliderTokens::from_palette(&enz_theme.palette)
    })
    .aria_label("Volume")
    .id(enzui_core::identity::derive_id("music-player", "volume"))
    .track_height(10.0)
    .thumb_width(20.0)
    .thumb_height(20.0)
    .into_element();
    row![
        enzui_widgets::toolbar_icon::enz_toolbar_icon(icon, Message::ToggleMute)
            .size(32.0, enz_theme.sizing.icon_sm)
            .aria_label(label)
            .tooltip(label)
            .id(enzui_core::identity::derive_id("music-player", "mute"))
            .into_element(),
        slider,
        container(
            enz_icon("volume-2")
                .size(enz_theme.sizing.icon_sm)
                .tint(theme::secondary_text_color(enz_theme))
                .into_element(),
        )
        .center_x(Length::Fixed(32.0)),
    ]
    .spacing(enz_theme.spacing.sm())
    .align_y(Alignment::Center)
    .width(width)
    .into()
}

// ---------------------------------------------------------------------------
// Compact now-playing sheet and mini-player
// ---------------------------------------------------------------------------

/// Wrap the now-playing panel in the framework bottom sheet with a close button.
///
/// The sheet is one expanded document without detents, since a peek would
/// clip the disclosure and the close control. Escape and scrim come from the
/// modal host in `MusicApp::content_pane`.
pub fn compact_now_playing_sheet<'a>(
    panel: Element<'a, Message>,
    expanded_height: f32,
    width: f32,
    palette: Option<[[u8; 3]; 3]>,
    enz_theme: &'a EnzTheme,
) -> Element<'a, Message> {
    use enzui_core::modal::DismissCause;
    use enzui_widgets::sheet::{SheetEvent, enz_sheet};

    // A centred chevron lowers the sheet: it reads as "put this away", and
    // never lines up under the window's own close button.
    let close = enzui_widgets::toolbar_icon::enz_toolbar_icon(
        "chevron-down",
        Message::NowPlayingSheet(SheetEvent::Dismissed(DismissCause::CloseButton)),
    )
    .size(40.0, enz_theme.sizing.icon_md)
    .aria_label("Close now playing")
    .tooltip("Close")
    .into_element();
    let close_row = container(close)
        .center_x(Length::Fill)
        .height(Length::Fixed(NP_SHEET_CLOSE_ROW))
        .align_y(Alignment::Center);
    // The cover's colours wash the sheet, part transparent over its frost.
    let content = iced::widget::stack![
        art_backdrop(palette, SHEET_WASH_ALPHA, enz_theme),
        container(
            column![close_row, panel]
                .width(Length::Fill)
                .height(Length::Fill)
        )
        .padding(enz_theme.spacing.sm()),
    ]
    .width(Length::Fill)
    .height(Length::Fill);

    // The sheet itself is clear; its content sits on raised frosted glass,
    // the dimmed library blurring through it.
    let mut tokens = enzui_widgets::sheet::SheetTokens::from_palette(&enz_theme.palette);
    tokens.surface = iced::Color::TRANSPARENT;
    tokens.border = iced::Color::TRANSPARENT;
    // The window's own corner radius, so the sheet's top reads as part of
    // the same family of shapes.
    let radius = enz_theme.window_radius;
    let glass = {
        use enzui_widgets::surface::{SurfaceTone, enz_surface};
        // Square glass: the renderer mis-rounds a material with mixed corner
        // radii (it rounded the bottom corners too).
        let frost = enz_surface(content)
            .tone(SurfaceTone::Elevated)
            .material(enzui_theme::MaterialStyle::FlatFrosted)
            .material_ink(enzui_theme::MaterialInk::Protected)
            .material_tint_opacity(SHEET_TINT)
            .radius(0.0)
            .border_width(0.0)
            .padding(0.0)
            .width(Length::Fill)
            .height(Length::Fill)
            .into_element();
        // A clear surface rounds its top corners by clipping the glass to
        // its own path, and casts the shadow.
        enz_surface(frost)
            .tone(SurfaceTone::Transparent)
            .radius_corners([radius, radius, 0.0, 0.0])
            .clip(true)
            .border_width(0.0)
            .elevation(4)
            .width(Length::Fill)
            .height(Length::Fill)
            .into_element()
    };
    container(
        enz_sheet(glass)
            .tokens(tokens)
            .padding(0.0)
            .height(expanded_height)
            .into_element(),
    )
    .width(width)
    .into()
}

/// The mini-player's play/pause control edge, in px.
const MINI_PLAY_SIZE: f32 = 36.0;

pub fn mini_player<'a>(
    playback: &PlaybackState,
    library: &'a Library,
    stream: Option<&StreamNowPlaying>,
    stream_art: Option<&'a crate::art::CoverArt>,
    enz_theme: &'a EnzTheme,
    remote: Option<&RemoteView<'a>>,
) -> Element<'a, Message> {
    let tc = theme::text_color(enz_theme);
    let sc = theme::secondary_text_color(enz_theme);

    let track = if remote.is_some() {
        None
    } else {
        playback.current_track(library)
    };
    let stream = if remote.is_some() { None } else { stream };
    let transport = remote.map_or_else(
        || Transport::local(playback, playback.current_track_id.is_some()),
        RemoteView::transport,
    );
    // Same lines as the panel, so a tuned station reads the same at every rung.
    let title_str = match (remote, track, stream) {
        (Some(r), ..) => r.spotify.track.name.clone(),
        (None, Some(t), _) => t.display_title().to_string(),
        (None, None, Some(s)) => s.title.clone(),
        (None, None, None) => "No Track".into(),
    };
    let artist_str = match (remote, track, stream) {
        (Some(r), ..) => r.spotify.track.artist_names(),
        (None, Some(t), _) => t.display_artist().to_string(),
        (None, None, Some(s)) => s.subtitle.clone(),
        (None, None, None) => "—".into(),
    };

    let (icon, label) = if transport.playing {
        ("pause", "Pause")
    } else {
        ("play", "Play")
    };
    // A round toolbar control one step larger than its neighbours, with a
    // glyph sized to it (a pill button drew an 8 px glyph in an oval).
    // A stream still connecting shows a spinner where play sits.
    let connecting = stream.is_some() && playback.stream_phase == crate::StreamPhase::Loading;
    let play_ctl: Element<'a, Message> = if connecting {
        container(enzui_widgets::spinner::enz_spinner().size(enz_theme.sizing.icon_md))
            .center_x(Length::Fixed(MINI_PLAY_SIZE))
            .center_y(Length::Fixed(MINI_PLAY_SIZE))
            .into()
    } else {
        enzui_widgets::toolbar_icon::enz_toolbar_icon(icon, Message::TogglePlayPause)
            .size(MINI_PLAY_SIZE, enz_theme.sizing.icon_md)
            .enabled(remote.is_some() || track.is_some() || stream.is_some())
            .aria_label(label)
            .tooltip(label)
            .id(enzui_core::identity::derive_id("music-mini", "play"))
            .into_element()
    };
    // Shuffle and repeat flank the transport for library playback; each
    // shows its state as the control's active look.
    let shuffle_label = if transport.shuffle {
        "Shuffle on"
    } else {
        "Shuffle off"
    };
    let shuffle_ctl: Element<'a, Message> =
        enzui_widgets::toolbar_icon::enz_toolbar_icon("shuffle", Message::ToggleShuffle)
            .size(32.0, enz_theme.sizing.icon_sm)
            .active(transport.shuffle)
            .aria_label(shuffle_label)
            .tooltip(shuffle_label)
            .id(enzui_core::identity::derive_id("music-player", "shuffle"))
            .into_element();
    let (repeat_icon, repeat_label, repeat_on) = transport.repeat;
    let repeat_ctl: Element<'a, Message> =
        enzui_widgets::toolbar_icon::enz_toolbar_icon(repeat_icon, Message::CycleRepeat)
            .size(32.0, enz_theme.sizing.icon_sm)
            .active(repeat_on)
            .aria_label(repeat_label)
            .tooltip(repeat_label)
            .id(enzui_core::identity::derive_id("music-player", "repeat"))
            .into_element();
    let controls = if transport.full {
        row![
            shuffle_ctl,
            transport_icon_sized(
                "skip-back",
                "Previous track",
                Message::PreviousTrack,
                32.0,
                enzui_core::identity::derive_id("music-player", "previous"),
                enz_theme
            ),
            play_ctl,
            transport_icon_sized(
                "skip-forward",
                "Next track",
                Message::NextTrack,
                32.0,
                enzui_core::identity::derive_id("music-player", "next"),
                enz_theme
            ),
            repeat_ctl,
        ]
    } else {
        row![play_ctl]
    }
    .spacing(enz_theme.spacing.xs())
    .align_y(Alignment::Center);

    let info = column![
        enz_text(title_str)
            .size(TextSize::Body)
            .color(tc)
            .full_width()
            .single_line_elided(),
        enz_text(artist_str)
            .size(TextSize::Caption)
            .color(sc)
            .full_width()
            .single_line_elided(),
    ]
    .spacing(StrokeToken::Hairline.resolve(&enz_theme.stroke))
    .width(Length::Fill);

    // Art, or the same neutral placeholder the track rows use, so the title
    // keeps its column whatever plays; nothing at all when nothing is loaded.
    // A tuned station or episode shows its logo or cover art, whole.
    let art = match (remote, track) {
        (Some(r), _) => r.art.map(|art| (art, EnzImageFit::Cover)),
        (None, Some(t)) => t.cover.as_ref().map(|art| (art, EnzImageFit::Cover)),
        (None, None) => stream_art.map(|art| (art, EnzImageFit::Contain)),
    };
    let thumb: Element<'a, Message> = match art {
        Some((art, fit)) => container(
            enz_image_handle(art.handle().clone())
                .fit(fit)
                .radius(enz_theme.radius.md)
                .alt_text(match track {
                    Some(t) => format!("{} album art", t.display_album()),
                    None => format!("{} artwork", stream.map_or("", |s| s.title.as_str())),
                })
                .width(Length::Fixed(mini_player_thumb_px(enz_theme)))
                .height(Length::Fixed(mini_player_thumb_px(enz_theme)))
                .placeholder_background(theme::overlay_hover_color(enz_theme))
                .into_element(),
        )
        // The gap is on the thumb, since row spacing would indent an art-less row.
        .padding(Padding::new(0.0).right(enz_theme.spacing.md()))
        .into(),
        None if remote.is_some() || track.is_some() || stream.is_some() => {
            use enzui_widgets::surface::{RadiusSize, SurfaceTone, enz_surface};
            let edge = mini_player_thumb_px(enz_theme);
            container(
                enz_surface(
                    container(
                        enz_icon("music")
                            .size(enz_theme.sizing.icon_md)
                            .tint(theme::secondary_text_color(enz_theme))
                            .into_element(),
                    )
                    .center_x(Length::Fixed(edge))
                    .center_y(Length::Fixed(edge)),
                )
                .tone(SurfaceTone::SurfaceAlt)
                .radius_size(RadiusSize::Md)
                .width(Length::Fixed(edge))
                .height(Length::Fixed(edge))
                .into_element(),
            )
            .padding(Padding::new(0.0).right(enz_theme.spacing.md()))
            .into()
        }
        None => Space::new().width(0).height(0).into(),
    };

    let content = row![thumb, info, controls]
        .spacing(enz_theme.spacing.xs())
        .align_y(Alignment::Center)
        .padding(Padding::new(8.0).left(16.0).right(16.0));

    // The press target is the glass alone, inside the fade, and draws no
    // hover tint: a tint over the whole bar washed a haze across the fade and
    // the page above it.
    let open = enz_pressable(content.into(), Message::ToggleNowPlaying)
        .padding(enz_theme.spacing.space0)
        .width(Length::Fill)
        .style(PressableStyle::Plain)
        .radius(0.0)
        .tokens(enzui_widgets::pressable::PressableTokens {
            hover_overlay: iced::Color::TRANSPARENT,
            ..enzui_widgets::pressable::PressableTokens::from_palette(&enz_theme.palette)
        })
        .id(enzui_core::identity::derive_id("music-mini", "open"))
        .aria_label("Open now playing")
        .into_element();
    frosted_bar(open, BarEdge::Top)
}

/// Which edge of a floating bar meets the page.
#[derive(Clone, Copy)]
pub(crate) enum BarEdge {
    Top,
    Bottom,
}

/// How much of the raised colour the now-playing sheet keeps over the
/// blurred library behind it.
const SHEET_TINT: f32 = 0.7;

/// How opaque the cover wash is over the sheet's frost.
const SHEET_WASH_ALPHA: f32 = 0.85;

/// The now-playing sheet's close row height.
pub const NP_SHEET_CLOSE_ROW: f32 = 44.0;

/// How much of the window colour a floating bar keeps over the page behind it.
const BAR_TINT: f32 = 0.55;

/// How far the page fades into a floating bar beyond its glass, in px.
const BAR_FADE: f32 = 28.0;

/// A floating bar's frosted glass: the window colour, part transparent over
/// the blurred page. Where it meets the page it fades in rather than
/// drawing a line: the titlebar through the shell's shade, the player bar
/// through a fade of its own.
pub(crate) fn frosted_bar<'a>(
    content: Element<'a, Message>,
    edge: BarEdge,
) -> Element<'a, Message> {
    use enzui_widgets::surface::{RadiusSize, SurfaceTone, enz_surface};
    let glass = enz_surface(content)
        .tone(SurfaceTone::Background)
        // Tinted with the page's own colour, so the glass shows no edge of
        // its own against the page.
        .fill_resolved(page_color)
        .material(enzui_theme::MaterialStyle::FlatFrosted)
        .material_ink(enzui_theme::MaterialInk::Protected)
        .material_tint_opacity(BAR_TINT)
        .radius_size(RadiusSize::None)
        .border_width(0.0)
        .elevation(0)
        .width(Length::Fill)
        .into_element();
    match edge {
        BarEdge::Top => column![bar_fade(), glass].into(),
        BarEdge::Bottom => glass,
    }
}

/// The theme's text colour at `alpha`: a neutral wash that shows on any
/// surface, darkening a light one and lightening a dark one.
fn ink(theme: &iced::Theme, alpha: f32) -> iced::Color {
    let text = enzui_widgets::theme_lookup::palette(theme).text;
    iced::Color {
        a: text.a * alpha,
        ..text
    }
}

/// The page's colour, which the floating bars are tinted with.
fn page_color(theme: &iced::Theme) -> iced::Color {
    enzui_widgets::surface::SurfaceTokens::from_palette(&enzui_widgets::theme_lookup::palette(
        theme,
    ))
    .fill_for(enzui_widgets::surface::SurfaceTone::Background)
}

/// The page fading into the player bar: clear at the top, the bar's tint
/// at the glass.
fn bar_fade<'a>() -> Element<'a, Message> {
    container(Space::new())
        .width(Length::Fill)
        .height(Length::Fixed(BAR_FADE))
        .style(|theme: &iced::Theme| {
            let color = page_color(theme);
            let at = |a: f32| iced::Color {
                a: color.a * a,
                ..color
            };
            iced::widget::container::Style {
                background: Some(iced::Background::Gradient(iced::Gradient::Linear(
                    iced::gradient::Linear::new(iced::Radians(0.0))
                        .add_stop(0.0, at(BAR_TINT))
                        .add_stop(1.0, at(0.0)),
                ))),
                ..Default::default()
            }
        })
        .into()
}

/// The frosted band behind the floating titlebar.
pub fn frosted_titlebar_band<'a>(height: f32) -> Element<'a, Message> {
    frosted_bar(
        Space::new()
            .width(Length::Fill)
            .height(Length::Fixed(height))
            .into(),
        BarEdge::Bottom,
    )
}

/// A cover-coloured backdrop for Now Playing: the cover's leading colours
/// spread in two crossing gradients, so they read as a soft wash rather
/// than bands. The colours are pulled dark in a dark theme and pale in a
/// light one, so the panel's text keeps its contrast. `alpha` lets frosted
/// glass show through it. Without a cover the backdrop is the raised tone.
pub fn art_backdrop<'a>(
    palette: Option<[[u8; 3]; 3]>,
    alpha: f32,
    enz_theme: &EnzTheme,
) -> Element<'a, Message> {
    let fill = |style: iced::widget::container::Style| {
        container(Space::new())
            .width(Length::Fill)
            .height(Length::Fill)
            .style(move |_| style)
    };
    let Some(palette) = palette else {
        let raised = enz_theme.palette.surface_elevated;
        return fill(iced::widget::container::Style {
            background: Some(iced::Background::Color(iced::Color {
                a: raised.a * alpha,
                ..raised
            })),
            ..Default::default()
        })
        .into();
    };
    let dark = luminance(enz_theme.palette.background) < 0.5;
    let [a, b, c] = palette.map(|rgb| backdrop_tone(rgb, dark, alpha));
    let clear = iced::Color { a: 0.0, ..b };
    // Top to bottom: the leading colour into the third.
    let base = iced::widget::container::Style {
        background: Some(iced::Background::Gradient(iced::Gradient::Linear(
            iced::gradient::Linear::new(iced::Radians(std::f32::consts::PI))
                .add_stop(0.0, a)
                .add_stop(1.0, c),
        ))),
        ..Default::default()
    };
    // The second colour washes in from the top trailing corner.
    let wash = iced::widget::container::Style {
        background: Some(iced::Background::Gradient(iced::Gradient::Linear(
            iced::gradient::Linear::new(iced::Radians(std::f32::consts::PI * 1.25))
                .add_stop(0.0, b)
                .add_stop(0.6, clear),
        ))),
        ..Default::default()
    };
    iced::widget::stack![fill(base), fill(wash)]
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}

/// Relative luminance of `color`, enough to tell a dark theme from a light.
fn luminance(color: iced::Color) -> f32 {
    0.2126 * color.r + 0.7152 * color.g + 0.0722 * color.b
}

/// A cover colour as a backdrop tone: its hue, a little calmer, at a
/// lightness the theme's text reads on.
fn backdrop_tone(rgb: [u8; 3], dark: bool, alpha: f32) -> iced::Color {
    let [r, g, b] = rgb.map(|v| f32::from(v) / 255.0);
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let lightness = (max + min) / 2.0;
    let delta = max - min;
    let saturation = if delta <= f32::EPSILON {
        0.0
    } else {
        delta / (1.0 - (2.0 * lightness - 1.0).abs())
    };
    let hue = if delta <= f32::EPSILON {
        0.0
    } else if (max - r).abs() <= f32::EPSILON {
        60.0 * (((g - b) / delta).rem_euclid(6.0))
    } else if (max - g).abs() <= f32::EPSILON {
        60.0 * ((b - r) / delta + 2.0)
    } else {
        60.0 * ((r - g) / delta + 4.0)
    };
    let (saturation, lightness) = if dark {
        (saturation * 0.75, lightness.clamp(0.16, 0.26))
    } else {
        ((saturation * 1.1).min(0.85), lightness.clamp(0.8, 0.88))
    };
    let chroma = (1.0 - (2.0 * lightness - 1.0).abs()) * saturation;
    let x = chroma * (1.0 - ((hue / 60.0).rem_euclid(2.0) - 1.0).abs());
    let m = lightness - chroma / 2.0;
    let (r, g, b) = match hue as u32 {
        0..=59 => (chroma, x, 0.0),
        60..=119 => (x, chroma, 0.0),
        120..=179 => (0.0, chroma, x),
        180..=239 => (0.0, x, chroma),
        240..=299 => (x, 0.0, chroma),
        _ => (chroma, 0.0, x),
    };
    iced::Color::from_rgba(r + m, g + m, b + m, alpha)
}
