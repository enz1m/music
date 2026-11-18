//! The Spotify page: sign-in, then the library and catalog to browse, with
//! where to play. What is playing shows in the app's own player.

use super::*;
use crate::Message;
use crate::podcasts::artwork::PodcastArtworkCache;
use enzui_theme::EnzTheme;
use enzui_widgets::button::{ButtonKind, ButtonSize, enz_button};
use enzui_widgets::enz_text::{TextSize, enz_text};
use enzui_widgets::icon::enz_icon;
use enzui_widgets::image_widget::{EnzImageFit, enz_image_handle};
use enzui_widgets::list_row_surface::enz_list_row_surface;
use enzui_widgets::pressable::{PressableStyle, enz_pressable};
use enzui_widgets::surface::{SurfaceTone, enz_surface};
use enzui_widgets::text_field::enz_text_field;
use enzui_widgets::toolbar_icon::enz_toolbar_icon;
use enzui_widgets::view_switcher::{SwitcherPage, SwitcherPresentation, enz_view_switcher};
use iced::widget::{Space, column, container, row};
use iced::{Alignment, Element, Length, Padding};

/// The sign-in card's width ceiling.
const SIGN_IN_WIDTH: f32 = 560.0;
/// Card grids: the narrowest card, and the gap between cards.
const CARD_MIN: f32 = 150.0;
const CARD_GAP: f32 = 12.0;
/// Header pictures on artist, album and playlist pages.
const HEADER_ART: f32 = 168.0;
/// Row thumbnails.
const THUMB: f32 = 40.0;

fn id(scope: &str, key: &str) -> iced::advanced::widget::Id {
    enzui_core::identity::derive_id(scope, key)
}

fn msg(event: Event) -> Message {
    Message::Spotify(event)
}

impl State {
    pub fn view<'a>(
        &'a self,
        art: &'a PodcastArtworkCache,
        width: f32,
        theme: &'a EnzTheme,
    ) -> Element<'a, Message> {
        if !self.connected() {
            return self.signed_out(theme);
        }
        let content_w = (width - 2.0 * theme.spacing.lg()).max(240.0);
        self.wide.set(content_w >= 520.0);
        let mut page = column![self.header(theme)]
            .spacing(theme.spacing.md())
            .padding(crate::views::page_padding(theme))
            .width(Length::Fill);
        if self.devices_open {
            page = page.push(self.devices_panel(theme));
        }
        let Some(screen) = self.screen() else {
            return crate::views::page_scroll(page);
        };
        let at_root = matches!(screen.route, Route::Tab(_));
        if at_root
            && !matches!(
                self.local,
                Local::Ready | Local::Unknown | Local::Checking | Local::Starting
            )
        {
            page = page.push(self.local_banner(theme));
        }
        if at_root {
            page = page.push(self.tabs());
        }
        page = page.push(self.screen_view(screen, art, content_w, theme));
        crate::views::page_scroll_tracked(page, scroll_id(), |y| msg(Event::Scrolled(y)))
    }

    /// Back, search, and where it plays.
    fn header<'a>(&'a self, theme: &'a EnzTheme) -> Element<'a, Message> {
        let mut bar = row![]
            .spacing(theme.spacing.sm())
            .align_y(Alignment::Center);
        bar = bar.push(
            enzui_widgets::search_field::enz_search_field(&self.query, |value| {
                msg(Event::Query(value))
            })
            .placeholder("Search Spotify")
            .on_submit(msg(Event::Search))
            .on_clear(msg(Event::ClearSearch))
            .width(Length::Fill)
            .into_element(),
        );
        let device = self
            .playing
            .as_ref()
            .and_then(|p| p.device.as_ref())
            .or_else(|| self.target_device());
        let (icon, label) = match device {
            Some(d) if self.is_local_device(d) => ("laptop", "This computer".to_owned()),
            Some(d) => (device_icon(&d.kind), d.name.clone()),
            None => ("cast", "Choose a device".to_owned()),
        };
        bar = bar.push(
            enz_button(label.clone(), msg(Event::ToggleDevices))
                .icon(icon)
                .kind(ButtonKind::Quiet)
                .size(ButtonSize::Sm)
                .id(id("music-spotify", "devices"))
                .into_element(),
        );
        bar = bar.push(
            enz_toolbar_icon("settings", Message::OpenSpotifySettings)
                .aria_label("Spotify settings")
                .tooltip("Spotify settings")
                .id(id("music-spotify", "settings"))
                .into_element(),
        );
        bar.into()
    }

    /// The Spotify page of Settings: the account, playback on this
    /// computer, and where lyrics come from.
    pub fn settings_page<'a>(&'a self, theme: &'a EnzTheme) -> Element<'a, Message> {
        use enzui_widgets::preferences_group::enz_preferences_group;
        use enzui_widgets::preferences_row::enz_preferences_row;
        let button = |label: &'a str, event: Event, key: &str| -> Element<'a, Message> {
            enz_button(label, msg(event))
                .kind(ButtonKind::Quiet)
                .size(ButtonSize::Sm)
                .id(id("music-spotify-settings", key))
                .into_element()
        };
        let account_line = match (&self.me, self.connected()) {
            (Some(me), true) => format!("Signed in as {}", me.name),
            (None, true) => "Signed in".to_owned(),
            (_, false) => "Not signed in".to_owned(),
        };
        let mut account = enz_preferences_row("Spotify account")
            .icon("audio-lines")
            .subtitle(account_line);
        if self.connected() {
            account = account.control(button("Disconnect", Event::Disconnect, "disconnect"));
        }
        let client = enz_preferences_row("Developer app Client ID")
            .icon("key-round")
            .subtitle(if self.client_id.is_empty() {
                "Not set".to_owned()
            } else {
                self.client_id.clone()
            })
            .subtitle_wrap(true);
        let account_group =
            enz_preferences_group(column![account.into_element(), client.into_element()])
                .title("Account")
                .description(
                    "Disconnecting signs out and removes the login from your desktop keyring.",
                )
                .into_element();

        let (player_line, player_action): (String, Option<(&str, Event)>) = match &self.local {
            Local::Ready => (
                "Ready. Spotify plays here as \"Music\".".into(),
                Some(("Restart", Event::RestartPlayer)),
            ),
            Local::Pairing => (
                "Waiting to be chosen once in a Spotify app on your network.".into(),
                Some(("Show steps", Event::Open(Route::SetUp))),
            ),
            Local::Starting | Local::Checking => ("Starting…".into(), None),
            Local::Installing => ("Downloading Spotify's player…".into(), None),
            Local::NotInstalled | Local::NeedsKey | Local::Unknown => (
                "Not set up.".into(),
                Some(("Set up", Event::Open(Route::SetUp))),
            ),
            Local::Expired => (
                "This build expired. Download the new one.".into(),
                Some(("Update", Event::Install)),
            ),
            Local::Failed(reason) => (reason.clone(), Some(("Try again", Event::RestartPlayer))),
        };
        let mut player = enz_preferences_row("Play on this computer")
            .icon("laptop")
            .subtitle(player_line)
            .subtitle_wrap(true);
        if let Some((label, event)) = player_action {
            player = player.control(button(label, event, "player"));
        }
        let mut key = enz_preferences_row("Soloist API key")
            .icon("key-round")
            .subtitle(if self.has_key {
                "Saved in your desktop keyring"
            } else {
                "Not set"
            });
        key = key.control(button(
            if self.has_key { "Change" } else { "Add" },
            Event::ChangeKeyAndOpen,
            "key",
        ));
        let installed = self.paths.as_ref().is_some_and(|p| p.program().is_some());
        let mut build = enz_preferences_row("Spotify Soloist")
            .icon("download")
            .subtitle(if installed {
                "Downloaded from Spotify. Builds stop working after 90 days."
            } else {
                "Not downloaded"
            })
            .subtitle_wrap(true);
        if self.local != Local::Installing && soloist::build_url().is_some() {
            build = build.control(button(
                if installed {
                    "Download again"
                } else {
                    "Download"
                },
                Event::Install,
                "download",
            ));
        }
        let player_group = enz_preferences_group(column![
            player.into_element(),
            key.into_element(),
            build.into_element()
        ])
        .title("This computer")
        .description("Spotify plays on computers only through its own player, Soloist. Spotify Premium is required.")
        .into_element();

        let lyrics_group = enz_preferences_group(column![
            enz_preferences_row("Lyrics")
                .icon("mic-vocal")
                .subtitle(
                    "Spotify doesn't share lyrics with other apps, so Music finds them on LRCLIB, \
                     a free lyrics database. Showing lyrics sends the song's title, artist and \
                     album to lrclib.net."
                )
                .subtitle_wrap(true)
                .into_element()
        ])
        .title("Lyrics")
        .into_element();

        column![account_group, player_group, lyrics_group]
            .spacing(theme.spacing.lg())
            .width(Length::Fill)
            .into()
    }

    fn tabs(&self) -> Element<'_, Message> {
        let active = Tab::ALL.iter().position(|t| *t == self.tab).unwrap_or(0);
        enz_view_switcher(
            Tab::ALL
                .iter()
                .map(|t| SwitcherPage::new(t.label(), t.label()))
                .collect(),
            active,
            |index| msg(Event::Tab(Tab::ALL[index])),
        )
        .presentation(SwitcherPresentation::Stretch)
        .tab_id_prefix("music-spotify-library")
        .aria_label("Your Spotify library")
        .into_element()
    }

    fn notice_bar<'a>(&'a self, notice: &'a str, theme: &'a EnzTheme) -> Element<'a, Message> {
        enz_surface(
            row![
                enz_text(notice)
                    .size(TextSize::Footnote)
                    .full_width()
                    .wrap(true),
                enz_toolbar_icon("x", msg(Event::Dismiss))
                    .size(28.0, theme.sizing.icon_sm)
                    .aria_label("Dismiss")
                    .id(id("music-spotify", "dismiss"))
                    .into_element(),
            ]
            .spacing(theme.spacing.sm())
            .align_y(Alignment::Center),
        )
        .tone(SurfaceTone::SurfaceAlt)
        .padding(Padding::new(theme.spacing.xs()).left(theme.spacing.md()))
        .border_width(0.0)
        .width(Length::Fill)
        .into_element()
    }

    /// Every device Spotify lists, this computer first.
    fn devices_panel<'a>(&'a self, theme: &'a EnzTheme) -> Element<'a, Message> {
        let current = self
            .playing
            .as_ref()
            .and_then(|p| p.device.as_ref())
            .map(|d| d.id.as_str())
            .or(self.picked.as_deref());
        let mut list = column![section_title("Play on", theme)].spacing(2.0);
        let mut devices: Vec<&Device> = self.devices.iter().collect();
        devices.sort_by_key(|d| !self.is_local_device(d));
        for device in devices {
            let local = self.is_local_device(device);
            let name = if local {
                "This computer".to_owned()
            } else {
                device.name.clone()
            };
            let detail = if device.active {
                "Playing"
            } else if local {
                "Music"
            } else {
                device.kind.as_str()
            };
            list = list.push(
                enz_list_row_surface(
                    container(
                        row![
                            enz_icon(if local {
                                "laptop"
                            } else {
                                device_icon(&device.kind)
                            })
                            .into_element(),
                            column![
                                enz_text(name).full_width().single_line_elided(),
                                enz_text(detail.to_owned())
                                    .size(TextSize::Caption)
                                    .color(theme.palette.text_muted),
                            ]
                            .spacing(2.0)
                            .width(Length::Fill),
                        ]
                        .spacing(theme.spacing.md())
                        .align_y(Alignment::Center),
                    )
                    .padding(Padding::new(0.0).top(6.0).bottom(6.0)),
                    msg(Event::PickDevice(device.id.clone())),
                )
                .id(id("music-spotify-device", &device.id))
                .label(format!("Play on {}", device.name))
                .selected(current == Some(device.id.as_str()))
                .content_padding(theme.spacing.md())
                .outer_gutter(0.0)
                .content_height(true)
                .into_element(),
            );
        }
        if self.devices.is_empty() {
            list = list.push(muted(
                "No devices are available. Open Spotify on a phone, speaker or computer, or set up this computer.",
                theme,
            ));
        }
        if self.local != Local::Ready {
            list = list.push(
                enz_button("Set up this computer", msg(Event::Open(Route::SetUp)))
                    .icon("laptop")
                    .kind(ButtonKind::Quiet)
                    .size(ButtonSize::Sm)
                    .id(id("music-spotify", "set-up-from-devices"))
                    .into_element(),
            );
        }

        enz_surface(list)
            .tone(SurfaceTone::SurfaceAlt)
            .padding(theme.spacing.sm())
            .border_width(0.0)
            .width(Length::Fill)
            .into_element()
    }

    /// Playback on this computer isn't ready: why, and the way on.
    fn local_banner<'a>(&'a self, theme: &'a EnzTheme) -> Element<'a, Message> {
        let (text, action): (&str, Option<(&str, Event)>) = match &self.local {
            Local::NotInstalled | Local::NeedsKey => (
                "Play Spotify on this computer. Spotify needs a one-time setup for that.",
                Some(("Set up", Event::Open(Route::SetUp))),
            ),
            Local::Installing => ("Downloading Spotify's player…", None),
            Local::Pairing => (
                "One more step: choose \"Music\" in the Spotify app on your phone once.",
                Some(("Show me", Event::Open(Route::SetUp))),
            ),
            Local::Expired => (
                "Spotify's player on this computer needs an update.",
                Some(("Update", Event::Install)),
            ),
            Local::Failed(_) => (
                "The player on this computer stopped.",
                Some(("Details", Event::Open(Route::SetUp))),
            ),
            _ => return Space::new().into(),
        };
        let mut line = row![
            enz_icon("laptop").into_element(),
            enz_text(text)
                .size(TextSize::Footnote)
                .full_width()
                .wrap(true),
        ]
        .spacing(theme.spacing.md())
        .align_y(Alignment::Center);
        if self.local == Local::Installing {
            line = line.push(enzui_widgets::spinner::enz_spinner().size(theme.sizing.icon_sm));
        }
        if let Some((label, event)) = action {
            line = line.push(
                enz_button(label, msg(event))
                    .kind(ButtonKind::Accent)
                    .size(ButtonSize::Sm)
                    .id(id("music-spotify", "local-banner"))
                    .into_element(),
            );
        }
        enz_surface(line)
            .tone(SurfaceTone::SurfaceAlt)
            .padding(Padding::new(theme.spacing.sm()).left(theme.spacing.md()))
            .border_width(0.0)
            .width(Length::Fill)
            .into_element()
    }

    fn screen_view<'a>(
        &'a self,
        screen: &'a Screen,
        art: &'a PodcastArtworkCache,
        width: f32,
        theme: &'a EnzTheme,
    ) -> Element<'a, Message> {
        match &screen.content {
            Content::Loading => container(enzui_widgets::spinner::enz_spinner())
                .center_x(Length::Fill)
                .padding(theme.spacing.xl())
                .into(),
            Content::Failed(reason) => column![
                muted(reason, theme),
                enz_button("Try again", msg(Event::Reload))
                    .kind(ButtonKind::Quiet)
                    .size(ButtonSize::Sm)
                    .id(id("music-spotify", "retry"))
                    .into_element(),
            ]
            .spacing(theme.spacing.sm())
            .into(),
            Content::Ready => self.set_up_view(theme),
            Content::Tracks(page) => {
                if page.items.is_empty() {
                    return empty_note(
                        if self.tab == Tab::Recent {
                            "Nothing played recently"
                        } else {
                            "No liked songs yet"
                        },
                        if self.tab == Tab::Recent {
                            "Songs you play on Spotify show up here."
                        } else {
                            "Press the heart on any song to keep it here. Search Spotify above to find music."
                        },
                        if self.tab == Tab::Recent {
                            "history"
                        } else {
                            "heart"
                        },
                        theme,
                    );
                }
                column![
                    self.track_list(&page.items, List::Main, false, art, theme),
                    self.more_button(screen, page.next.is_some())
                ]
                .spacing(theme.spacing.sm())
                .into()
            }
            Content::Albums(page) => {
                if page.items.is_empty() {
                    return empty_note(
                        "No saved albums yet",
                        "Press the heart on an album's page to keep it here.",
                        "disc",
                        theme,
                    );
                }
                column![
                    grid(
                        page.items
                            .iter()
                            .map(|a| album_card(a, art, theme))
                            .collect(),
                        width
                    ),
                    self.more_button(screen, page.next.is_some())
                ]
                .spacing(theme.spacing.sm())
                .into()
            }
            Content::Artists(page) => {
                if page.items.is_empty() {
                    return empty_note(
                        "You don't follow any artists yet",
                        "Press Follow on an artist's page to keep them here.",
                        "mic",
                        theme,
                    );
                }
                column![
                    grid(
                        page.items
                            .iter()
                            .map(|a| artist_card(a, art, theme))
                            .collect(),
                        width
                    ),
                    self.more_button(screen, page.next.is_some())
                ]
                .spacing(theme.spacing.sm())
                .into()
            }
            Content::Playlists(page) => {
                if page.items.is_empty() {
                    return empty_note(
                        "No playlists yet",
                        "Playlists you make or follow in Spotify show up here.",
                        "list-music",
                        theme,
                    );
                }
                column![
                    grid(
                        page.items
                            .iter()
                            .map(|p| playlist_card(p, art, theme))
                            .collect(),
                        width
                    ),
                    self.more_button(screen, page.next.is_some())
                ]
                .spacing(theme.spacing.sm())
                .into()
            }
            Content::Search(results) => self.search_view(results, art, width, theme),
            Content::Artist(page) => self.artist_view(page, art, width, theme),
            Content::Album(page) => self.album_view(screen, page, art, theme),
            Content::Playlist(page) => self.playlist_view(screen, page, art, theme),
        }
    }

    fn more_button<'a>(&'a self, screen: &'a Screen, has_more: bool) -> Element<'a, Message> {
        if !has_more {
            return Space::new().into();
        }
        container(
            enz_button(
                if screen.loading_more {
                    "Loading…"
                } else {
                    "Show more"
                },
                msg(Event::More),
            )
            .kind(ButtonKind::Quiet)
            .size(ButtonSize::Sm)
            .enabled(!screen.loading_more)
            .id(id("music-spotify", "more"))
            .into_element(),
        )
        .center_x(Length::Fill)
        .into()
    }

    fn search_view<'a>(
        &'a self,
        results: &'a SearchResults,
        art: &'a PodcastArtworkCache,
        width: f32,
        theme: &'a EnzTheme,
    ) -> Element<'a, Message> {
        let mut page = column![].spacing(theme.spacing.lg());
        if !results.tracks.is_empty() {
            page = page.push(section(
                "Songs",
                self.track_list(&results.tracks, List::SearchSongs, false, art, theme),
                theme,
            ));
        }
        if !results.artists.is_empty() {
            page = page.push(section(
                "Artists",
                grid(
                    results
                        .artists
                        .iter()
                        .map(|a| artist_card(a, art, theme))
                        .collect(),
                    width,
                ),
                theme,
            ));
        }
        if !results.albums.is_empty() {
            page = page.push(section(
                "Albums",
                grid(
                    results
                        .albums
                        .iter()
                        .map(|a| album_card(a, art, theme))
                        .collect(),
                    width,
                ),
                theme,
            ));
        }
        if !results.playlists.is_empty() {
            page = page.push(section(
                "Playlists",
                grid(
                    results
                        .playlists
                        .iter()
                        .map(|p| playlist_card(p, art, theme))
                        .collect(),
                    width,
                ),
                theme,
            ));
        }
        if results.tracks.is_empty()
            && results.artists.is_empty()
            && results.albums.is_empty()
            && results.playlists.is_empty()
        {
            page = page.push(muted("Nothing on Spotify matches that.", theme));
        }
        page.into()
    }

    fn artist_view<'a>(
        &'a self,
        page: &'a ArtistPage,
        art: &'a PodcastArtworkCache,
        width: f32,
        theme: &'a EnzTheme,
    ) -> Element<'a, Message> {
        let artist = &page.artist;
        let following = self.saved.get(&artist.uri).copied().unwrap_or(false);
        let caption = if artist.genres.is_empty() {
            "Artist".to_owned()
        } else {
            artist.genres.join(" · ")
        };
        let actions = row![
            enz_button("Play", msg(Event::PlayUri(artist.uri.clone())))
                .icon("play")
                .kind(ButtonKind::Accent)
                .id(id("music-spotify-artist", "play"))
                .into_element(),
            enz_button(
                if following { "Following" } else { "Follow" },
                msg(Event::ToggleSaved(artist.uri.clone()))
            )
            .icon(if following { "check" } else { "plus" })
            .kind(ButtonKind::Quiet)
            .id(id("music-spotify-artist", "follow"))
            .into_element(),
            open_on_spotify(&artist.uri, &artist.name),
        ]
        .spacing(theme.spacing.sm())
        .align_y(Alignment::Center);
        let mut body = column![header(
            picture(art, &artist.uri, HEADER_ART, HEADER_ART / 2.0, "mic", theme),
            &caption,
            &artist.name,
            None,
            actions.into(),
            theme,
        )]
        .spacing(theme.spacing.xl());
        if !page.popular.is_empty() {
            body = body.push(section(
                "Popular",
                self.track_list(&page.popular, List::Popular, false, art, theme),
                theme,
            ));
        }
        for (title, list) in [
            ("Albums", &page.albums),
            ("Singles and EPs", &page.singles),
            ("Compilations", &page.compilations),
            ("Appears on", &page.appears_on),
        ] {
            if !list.is_empty() {
                body = body.push(section(
                    title,
                    grid(
                        list.iter().map(|a| album_card(a, art, theme)).collect(),
                        width,
                    ),
                    theme,
                ));
            }
        }
        body.into()
    }

    fn album_view<'a>(
        &'a self,
        screen: &'a Screen,
        page: &'a AlbumPage,
        art: &'a PodcastArtworkCache,
        theme: &'a EnzTheme,
    ) -> Element<'a, Message> {
        let album = &page.album;
        let saved = self.saved.get(&album.uri).copied().unwrap_or(false);
        let total_ms: u64 = page.tracks.items.iter().map(|t| t.duration_ms).sum();
        let kind = match album.album_type.as_str() {
            "single" => "Single",
            "compilation" => "Compilation",
            _ => "Album",
        };
        let mut facts = vec![kind.to_owned()];
        if !album.year.is_empty() {
            facts.push(album.year.clone());
        }
        let count = album.total_tracks.max(page.tracks.items.len() as u32);
        facts.push(format!(
            "{count} {}",
            if count == 1 { "song" } else { "songs" }
        ));
        if total_ms > 0 && page.tracks.next.is_none() {
            facts.push(model::length_words(total_ms));
        }
        let actions = row![
            enz_button("Play", msg(Event::PlayUri(album.uri.clone())))
                .icon("play")
                .kind(ButtonKind::Accent)
                .id(id("music-spotify-album", "play"))
                .into_element(),
            enz_toolbar_icon(
                if saved { "heart.fill" } else { "heart" },
                msg(Event::ToggleSaved(album.uri.clone()))
            )
            .aria_label(if saved {
                "Remove from your library"
            } else {
                "Save to your library"
            })
            .tooltip(if saved { "Saved" } else { "Save" })
            .id(id("music-spotify-album", "save"))
            .into_element(),
            open_on_spotify(&album.uri, &album.name),
        ]
        .spacing(theme.spacing.sm())
        .align_y(Alignment::Center);
        column![
            header(
                picture(art, &album.uri, HEADER_ART, theme.radius.lg, "disc", theme),
                &facts.join(" · "),
                &album.name,
                Some(artist_links(&album.artists, theme)),
                actions.into(),
                theme,
            ),
            self.track_list(&page.tracks.items, List::Main, true, art, theme),
            self.more_button(screen, page.tracks.next.is_some()),
        ]
        .spacing(theme.spacing.lg())
        .into()
    }

    fn playlist_view<'a>(
        &'a self,
        screen: &'a Screen,
        page: &'a PlaylistPage,
        art: &'a PodcastArtworkCache,
        theme: &'a EnzTheme,
    ) -> Element<'a, Message> {
        let playlist = &page.playlist;
        let caption = format!(
            "Playlist · {} · {} {}",
            playlist.owner,
            playlist.total,
            if playlist.total == 1 { "song" } else { "songs" }
        );
        let actions = row![
            enz_button("Play", msg(Event::PlayUri(playlist.uri.clone())))
                .icon("play")
                .kind(ButtonKind::Accent)
                .id(id("music-spotify-playlist", "play"))
                .into_element(),
            open_on_spotify(&playlist.uri, &playlist.name),
        ]
        .spacing(theme.spacing.sm())
        .align_y(Alignment::Center);
        let description =
            (!playlist.description.is_empty()).then(|| muted(&playlist.description, theme));
        let mut body = column![header(
            picture(
                art,
                &playlist.uri,
                HEADER_ART,
                theme.radius.lg,
                "list-music",
                theme
            ),
            &caption,
            &playlist.name,
            description,
            actions.into(),
            theme,
        )]
        .spacing(theme.spacing.lg());
        if page.restricted {
            body = body.push(muted(
                "Spotify lets apps list the songs only of playlists you made or collaborate on. \
                 This one still plays: press Play.",
                theme,
            ));
        } else if page.tracks.items.is_empty() {
            body = body.push(muted("This playlist is empty.", theme));
        } else {
            body = body
                .push(self.track_list(&page.tracks.items, List::Main, false, art, theme))
                .push(self.more_button(screen, page.tracks.next.is_some()));
        }
        body.into()
    }

    /// Songs as rows. Clicking plays from that song in the list's order.
    fn track_list<'a>(
        &'a self,
        tracks: &'a [Track],
        list: List,
        numbered: bool,
        art: &'a PodcastArtworkCache,
        theme: &'a EnzTheme,
    ) -> Element<'a, Message> {
        let wide = self.wide.get();
        let now = self.current_track_uri();
        let mut rows = column![].spacing(0.0);
        for (index, track) in tracks.iter().enumerate() {
            rows = rows.push(self.track_row(
                track,
                index,
                list,
                numbered || !wide,
                numbered,
                now == Some(track.uri.as_str()),
                art,
                theme,
            ));
        }
        rows.into()
    }

    #[allow(clippy::too_many_arguments)]
    fn track_row<'a>(
        &'a self,
        track: &'a Track,
        index: usize,
        list: List,
        hide_album: bool,
        numbered: bool,
        current: bool,
        art: &'a PodcastArtworkCache,
        theme: &'a EnzTheme,
    ) -> Element<'a, Message> {
        let lead: Element<'a, Message> = if numbered {
            container(
                enz_text(if current {
                    "♪".to_owned()
                } else {
                    track.number.max(index as u32 + 1).to_string()
                })
                .size(TextSize::Footnote)
                .color(theme.palette.text_muted),
            )
            .center_x(Length::Fixed(THUMB))
            .into()
        } else {
            picture(art, &track.uri, THUMB, theme.radius.sm, "music", theme)
        };
        let mut subtitle = track.artist_names();
        if !numbered && let Some(album) = &track.album {
            subtitle = format!("{subtitle} · {}", album.name);
        }
        let liked = self.saved.get(&track.uri).copied().unwrap_or(false);
        let explicit: Option<Element<'a, Message>> = track.explicit.then(|| {
            enz_surface(
                container(
                    enz_text("E")
                        .size(TextSize::Caption2)
                        .color(theme.palette.background),
                )
                .center_x(Length::Fixed(14.0))
                .center_y(Length::Fixed(14.0)),
            )
            .tone(SurfaceTone::Selected)
            .fill_resolved(|t| {
                let c = enzui_widgets::theme_lookup::palette(t).text;
                iced::Color { a: 0.55, ..c }
            })
            .radius(3.0)
            .border_width(0.0)
            .into_element()
        });
        let content = row![
            lead,
            column![
                enz_text(track.name.clone())
                    .color(if current {
                        theme.palette.accent
                    } else {
                        theme.palette.text
                    })
                    .full_width()
                    .single_line_elided(),
                row![]
                    .extend(explicit)
                    .push(
                        enz_text(subtitle)
                            .size(TextSize::Footnote)
                            .color(theme.palette.text_muted)
                            .full_width()
                            .single_line_elided()
                    )
                    .spacing(theme.spacing.xs())
                    .align_y(Alignment::Center),
            ]
            .spacing(2.0)
            .width(Length::Fill),
            enz_toolbar_icon(
                if liked { "heart.fill" } else { "heart" },
                msg(Event::ToggleSaved(track.uri.clone()))
            )
            .size(32.0, theme.sizing.icon_sm)
            .aria_label(if liked {
                "Remove from Liked Songs"
            } else {
                "Add to Liked Songs"
            })
            .tooltip(if liked { "Liked" } else { "Like" })
            .id(id("music-spotify-like", &format!("{list:?}-{index}")))
            .into_element(),
            container(
                enz_text(model::clock(track.duration_ms))
                    .size(TextSize::Footnote)
                    .color(theme.palette.text_muted)
            )
            .align_right(Length::Fixed(40.0)),
            enz_toolbar_icon("list-end", msg(Event::Queue(track.uri.clone())))
                .size(32.0, theme.sizing.icon_sm)
                .aria_label(format!("Add {} to the queue", track.name))
                .tooltip("Add to queue")
                .id(id("music-spotify-queue", &format!("{list:?}-{index}")))
                .into_element(),
        ]
        .extend(
            (!hide_album)
                .then_some(track.album.as_ref())
                .flatten()
                .map(|album| {
                    enz_toolbar_icon("disc", msg(Event::Open(Route::Album(album.uri.clone()))))
                        .size(32.0, theme.sizing.icon_sm)
                        .aria_label(format!("Go to {}", album.name))
                        .tooltip("Go to album")
                        .id(id("music-spotify-goto-album", &format!("{list:?}-{index}")))
                        .into_element()
                }),
        )
        .spacing(theme.spacing.sm())
        .align_y(Alignment::Center);
        enz_list_row_surface(
            container(content).padding(Padding::new(0.0).top(4.0).bottom(4.0)),
            msg(Event::PlayTrack(list, index)),
        )
        .id(id(
            "music-spotify-track",
            &format!("{list:?}-{index}-{}", track.uri),
        ))
        .label(format!("Play {}", track.name))
        .selected(current)
        .content_padding(theme.spacing.sm())
        .outer_gutter(0.0)
        .content_height(true)
        .into_element()
    }

    // ------------------------------------------------------------- setup

    /// Setting up playback on this computer, step by step.
    fn set_up_view<'a>(&'a self, theme: &'a EnzTheme) -> Element<'a, Message> {
        let key_done = self.has_key;
        let installed = self.paths.as_ref().is_some_and(|p| p.program().is_some());
        let paired = self.local == Local::Ready;
        let status: Element<'a, Message> = match &self.local {
            Local::Installing => spinner_line("Downloading Spotify's player…", theme),
            Local::Starting | Local::Checking => spinner_line("Starting the player…", theme),
            Local::Pairing => spinner_line(
                "Waiting for you to choose \"Music\" in a Spotify app…",
                theme,
            ),
            Local::Ready => muted(
                "This computer is ready. Music plays here by default.",
                theme,
            ),
            Local::Expired => muted(
                "Spotify's builds stop working after 90 days. Download the new one.",
                theme,
            ),
            Local::Failed(reason) => column![
                muted(reason, theme),
                enz_button("Try again", msg(Event::RestartPlayer))
                    .kind(ButtonKind::Quiet)
                    .size(ButtonSize::Sm)
                    .id(id("music-spotify-setup", "retry"))
                    .into_element(),
            ]
            .spacing(theme.spacing.xs())
            .into(),
            _ => Space::new().into(),
        };
        let key_field = enz_text_field("", "Soloist API key", &self.key_draft, |v| {
            msg(Event::KeyDraft(v))
        })
        .on_submit(msg(Event::SaveKey));
        let steps = column![
            enzui_widgets::status_page::enz_status_page("Play on this computer")
                .icon("laptop")
                .description(
                    "Spotify plays on computers only through its own player, Soloist. Music sets it up \
                     for you and starts it whenever you're signed in. You need Spotify Premium.",
                )
                .variant(enzui_widgets::status_page::StatusPageVariant::Inline)
                .into_element(),
            step(
                "1",
                "Get your Soloist API key",
                key_done,
                if key_done {
                    column![
                        muted("Your key is saved in your desktop keyring.", theme),
                        enz_button("Use a different key", msg(Event::ChangeKey))
                            .kind(ButtonKind::Quiet)
                            .size(ButtonSize::Sm)
                            .id(id("music-spotify-setup", "change-key"))
                            .into_element(),
                    ]
                    .spacing(theme.spacing.sm())
                    .into()
                } else {
                column![
                    muted(
                        "Open the Soloist page of the developer dashboard, accept the terms, choose \
                         Generate, and copy the key.",
                        theme
                    ),
                    enz_button("Open the Soloist key page", msg(Event::OpenLink(soloist::API_KEY_URL)))
                        .icon("external-link")
                        .kind(ButtonKind::Quiet)
                        .id(id("music-spotify-setup", "key-page"))
                        .into_element(),
                    row![
                        key_field,
                        enz_button("Paste", msg(Event::PasteKey))
                            .icon("clipboard-paste")
                            .kind(ButtonKind::Quiet)
                            .id(id("music-spotify-setup", "paste-key"))
                            .into_element(),
                        enz_button("Save", msg(Event::SaveKey))
                            .kind(ButtonKind::Accent)
                            .enabled(!self.key_draft.trim().is_empty())
                            .id(id("music-spotify-setup", "save-key"))
                            .into_element(),
                    ]
                    .spacing(theme.spacing.sm())
                    .align_y(Alignment::Center),
                    muted("Music keeps it in your desktop keyring.", theme),
                ]
                .spacing(theme.spacing.sm())
                .into()
                },
                theme,
            ),
            step(
                "2",
                "Download Spotify's player",
                installed && self.local != Local::Expired,
                column![
                    muted(
                        "Music downloads the official build from Spotify and keeps it in its own folder. \
                         Spotify replaces builds every 90 days; Music tells you when.",
                        theme
                    ),
                    row![
                        enz_button(
                            if installed { "Download again" } else { "Download from Spotify" },
                            msg(Event::Install)
                        )
                        .icon("download")
                        .kind(if installed { ButtonKind::Quiet } else { ButtonKind::Accent })
                        .enabled(self.local != Local::Installing && soloist::build_url().is_some())
                        .id(id("music-spotify-setup", "download"))
                        .into_element(),
                        enz_button("About Soloist builds", msg(Event::OpenLink(soloist::DOWNLOADS_URL)))
                            .kind(ButtonKind::Quiet)
                            .id(id("music-spotify-setup", "about"))
                            .into_element(),
                    ]
                    .spacing(theme.spacing.sm()),
                ]
                .spacing(theme.spacing.sm())
                .into(),
                theme,
            ),
            step(
                "3",
                "Choose \"Music\" once in a Spotify app",
                paired,
                column![
                    muted(
                        "Spotify links a new player only from one of its own apps. On your phone or another \
                         computer on the same network, open Spotify, tap the devices button, and choose Music. \
                         You only do this once.",
                        theme
                    ),
                    status,
                ]
                .spacing(theme.spacing.sm())
                .into(),
                theme,
            ),
        ]
        .spacing(theme.spacing.lg());
        container(enzui_widgets::max_width::max_width(steps, SIGN_IN_WIDTH))
            .center_x(Length::Fill)
            .into()
    }

    // ----------------------------------------------------------- sign-in

    fn signed_out<'a>(&'a self, theme: &'a EnzTheme) -> Element<'a, Message> {
        let mut actions = row![
            enz_button("Sign in", msg(Event::Connect))
                .kind(ButtonKind::Accent)
                .id(id("music-spotify", "Sign in"))
                .enabled(!self.busy)
                .into_element(),
        ]
        .spacing(theme.spacing.sm());
        if self.busy {
            actions = actions.push(
                enz_button("Cancel", msg(Event::Disconnect))
                    .id(id("music-spotify", "cancel"))
                    .kind(ButtonKind::Quiet)
                    .into_element(),
            );
        }
        let mut form = column![].spacing(theme.spacing.lg());
        if let Some(status) = &self.status {
            form = form.push(self.notice_bar(status, theme));
        }
        if self.renew {
            // The app is set up already: one approval in the browser.
            form = form.push(
                column![
                    enzui_widgets::status_page::enz_status_page("Sign in to Spotify again")
                        .icon("audio-lines")
                        .description(
                            "Music can now like songs, follow artists and show what you played recently. \
                             Spotify asks you to approve that once.",
                        )
                        .variant(enzui_widgets::status_page::StatusPageVariant::Inline)
                        .into_element(),
                    container(actions).center_x(Length::Fill),
                ]
                .spacing(theme.spacing.lg()),
            );
        } else {
            form = form.push(self.sign_in_steps(actions, theme));
        }
        crate::views::page_scroll(
            column![
                container(enzui_widgets::max_width::max_width(form, SIGN_IN_WIDTH))
                    .center_x(Length::Fill)
                    .padding(Padding::new(0.0).top(theme.spacing.xl()))
            ]
            .padding(crate::views::page_padding(theme))
            .width(Length::Fill),
        )
    }

    /// Setting up Spotify, step by step: create a developer app with the
    /// values to copy, paste its Client ID, sign in.
    fn sign_in_steps<'a>(
        &'a self,
        actions: iced::widget::Row<'a, Message>,
        theme: &'a EnzTheme,
    ) -> Element<'a, Message> {
        let copyable = |label: &'a str, value: &'static str| -> Element<'a, Message> {
            let copied = self.copied == Some(value);
            enz_surface(
                row![
                    column![
                        enz_text(label)
                            .size(TextSize::Caption)
                            .color(theme.palette.text_muted),
                        enz_text(value)
                            .size(TextSize::Body)
                            .full_width()
                            .single_line_elided(),
                    ]
                    .spacing(2.0)
                    .width(Length::Fill),
                    enz_toolbar_icon(
                        if copied { "check" } else { "copy" },
                        msg(Event::Copy(value))
                    )
                    .aria_label(format!("Copy {label}"))
                    .tooltip(if copied { "Copied" } else { "Copy" })
                    .id(id("music-spotify-copy", label))
                    .into_element(),
                ]
                .spacing(theme.spacing.sm())
                .align_y(Alignment::Center),
            )
            .tone(SurfaceTone::SurfaceAlt)
            .padding(Padding::new(theme.spacing.sm()).left(theme.spacing.md()))
            .border_width(0.0)
            .width(Length::Fill)
            .into_element()
        };
        let client_id = enz_text_field("", "32-character Client ID", &self.client_id, |v| {
            msg(Event::ClientId(v))
        })
        .on_submit(msg(Event::Connect));
        column![
            enzui_widgets::status_page::enz_status_page("Connect Spotify")
                .icon("audio-lines")
                .description(
                    "Spotify lets apps like Music connect only through a developer app you create \
                     yourself. It takes about two minutes, once.",
                )
                .variant(enzui_widgets::status_page::StatusPageVariant::Inline)
                .into_element(),
            step(
                "1",
                "Open Spotify's developer dashboard",
                false,
                column![
                    muted("Sign in with the Spotify account you listen with. Spotify asks for Premium here.", theme),
                    enz_button("Open the developer dashboard", msg(Event::OpenLink(DASHBOARD_URL)))
                        .icon("external-link")
                        .kind(ButtonKind::Quiet)
                        .id(id("music-spotify", "dashboard"))
                        .into_element(),
                    copyable("Dashboard", DASHBOARD_URL),
                ]
                .spacing(theme.spacing.sm())
                .into(),
                theme,
            ),
            step(
                "2",
                "Create an app with these values",
                false,
                column![
                    muted(
                        "Choose Create app and fill in the form. Add the redirect URI exactly as shown, \
                         tick Web API, accept the terms and save.",
                        theme
                    ),
                    copyable("App name", APP_NAME),
                    copyable("App description", APP_DESCRIPTION),
                    copyable("Redirect URI", REDIRECT_URI),
                ]
                .spacing(theme.spacing.sm())
                .into(),
                theme,
            ),
            step(
                "3",
                "Paste the app's Client ID",
                self.client_id.len() == 32,
                column![
                    muted("Open the new app's settings and copy its Client ID, then paste it here.", theme),
                    row![
                        client_id,
                        enz_button("Paste", msg(Event::PasteClientId))
                            .icon("clipboard-paste")
                            .kind(ButtonKind::Quiet)
                            .enabled(!self.busy)
                            .id(id("music-spotify", "paste"))
                            .into_element(),
                    ]
                    .spacing(theme.spacing.sm())
                    .align_y(Alignment::Center),
                ]
                .spacing(theme.spacing.sm())
                .into(),
                theme,
            ),
            step(
                "4",
                "Sign in",
                false,
                column![
                    muted(
                        "Sign-in opens your browser. Music remembers it in your desktop keyring, so you stay \
                         signed in until you disconnect.",
                        theme
                    ),
                    actions,
                    muted(
                        "To let someone else use it, add their Spotify email under User Management in the app's settings.",
                        theme
                    ),
                ]
                .spacing(theme.spacing.sm())
                .into(),
                theme,
            ),
        ]
        .spacing(theme.spacing.lg())
        .into()
    }
}

// ------------------------------------------------------------------ pieces

fn muted<'a>(label: &str, theme: &'a EnzTheme) -> Element<'a, Message> {
    enz_text(label.to_owned())
        .size(TextSize::Footnote)
        .color(theme.palette.text_muted)
        .full_width()
        .wrap(true)
        .into_element()
}

/// An empty list: what would be here, and how to fill it.
fn empty_note<'a>(
    title: &'a str,
    body: &'a str,
    icon: &'a str,
    _theme: &'a EnzTheme,
) -> Element<'a, Message> {
    enzui_widgets::status_page::enz_status_page(title)
        .icon(icon)
        .description(body)
        .variant(enzui_widgets::status_page::StatusPageVariant::Inline)
        .into_element()
}

fn spinner_line<'a>(label: &'a str, theme: &'a EnzTheme) -> Element<'a, Message> {
    row![
        enzui_widgets::spinner::enz_spinner().size(theme.sizing.icon_sm),
        enz_text(label)
            .size(TextSize::Footnote)
            .full_width()
            .wrap(true),
    ]
    .spacing(theme.spacing.sm())
    .align_y(Alignment::Center)
    .into()
}

fn section_title<'a>(label: &'a str, theme: &'a EnzTheme) -> Element<'a, Message> {
    enz_text(label)
        .size(TextSize::Footnote)
        .color(theme.palette.text_muted)
        .into_element()
}

/// A titled part of a page.
fn section<'a>(
    title: &'a str,
    body: Element<'a, Message>,
    theme: &'a EnzTheme,
) -> Element<'a, Message> {
    column![enz_text(title).size(TextSize::Title3).full_width(), body]
        .spacing(theme.spacing.sm())
        .into()
}

/// One numbered step: its number (or a check) in a disc, a title, its body.
fn step<'a>(
    number: &'static str,
    title: &'a str,
    done: bool,
    body: Element<'a, Message>,
    theme: &'a EnzTheme,
) -> Element<'a, Message> {
    let mark: Element<'a, Message> = if done {
        enz_icon("check").size(theme.sizing.icon_sm).into_element()
    } else {
        enz_text(number).size(TextSize::Footnote).into_element()
    };
    let disc = enz_surface(
        container(mark)
            .center_x(Length::Fixed(24.0))
            .center_y(Length::Fixed(24.0)),
    )
    .tone(SurfaceTone::Selected)
    .radius(12.0)
    .border_width(0.0)
    .into_element();
    row![
        disc,
        column![enz_text(title).size(TextSize::Headline).full_width(), body]
            .spacing(theme.spacing.sm())
            .width(Length::Fill),
    ]
    .spacing(theme.spacing.md())
    .into()
}

fn device_icon(kind: &str) -> &'static str {
    match kind.to_ascii_lowercase().as_str() {
        "smartphone" => "smartphone",
        "computer" => "laptop",
        "tablet" => "tablet",
        "tv" => "tv",
        "speaker" | "avr" | "stb" | "audiodongle" | "castaudio" => "speaker",
        "castvideo" => "cast",
        _ => "speaker",
    }
}

/// A picture from the artwork cache, or a glyph on a quiet tile while it
/// loads (or when there is none).
fn picture<'a>(
    art: &'a PodcastArtworkCache,
    uri: &str,
    edge: f32,
    radius: f32,
    glyph: &'static str,
    theme: &'a EnzTheme,
) -> Element<'a, Message> {
    match art.channel_art(&art_key(uri)) {
        Some(cover) => enz_image_handle(cover.handle().clone())
            .fit(EnzImageFit::Cover)
            .radius(radius)
            .width(Length::Fixed(edge))
            .height(Length::Fixed(edge))
            .into_element(),
        None => enz_surface(
            container(
                enz_icon(glyph)
                    .size((edge * 0.35).clamp(16.0, 48.0))
                    .tint(theme.palette.text_muted)
                    .into_element(),
            )
            .center_x(Length::Fixed(edge))
            .center_y(Length::Fixed(edge)),
        )
        .tone(SurfaceTone::SurfaceAlt)
        .radius(radius)
        .border_width(0.0)
        .into_element(),
    }
}

/// An artist, album or playlist page's header: picture, then what it is,
/// its name, a line under it, and its actions.
fn header<'a>(
    picture: Element<'a, Message>,
    caption: &str,
    title: &str,
    under: Option<Element<'a, Message>>,
    actions: Element<'a, Message>,
    theme: &'a EnzTheme,
) -> Element<'a, Message> {
    let mut info = column![
        enz_text(caption.to_owned())
            .size(TextSize::Footnote)
            .color(theme.palette.text_muted)
            .full_width()
            .single_line_elided(),
        enz_text(title.to_owned())
            .size(TextSize::Title1)
            .full_width()
            .wrap(true),
    ]
    .spacing(theme.spacing.xs())
    .width(Length::Fill);
    if let Some(under) = under {
        info = info.push(under);
    }
    info = info.push(container(actions).padding(Padding::new(0.0).top(theme.spacing.sm())));
    row![picture, info]
        .spacing(theme.spacing.lg())
        .align_y(Alignment::End)
        .into()
}

/// Artists as links to their pages.
fn artist_links<'a>(artists: &'a [Link], theme: &'a EnzTheme) -> Element<'a, Message> {
    let mut line = row![]
        .spacing(theme.spacing.xs())
        .align_y(Alignment::Center);
    for artist in artists.iter().take(4) {
        line = line.push(
            enz_button(
                artist.name.clone(),
                msg(Event::Open(Route::Artist(artist.uri.clone()))),
            )
            .kind(ButtonKind::Quiet)
            .size(ButtonSize::Sm)
            .id(id("music-spotify-artist-link", &artist.uri))
            .into_element(),
        );
    }
    line.into()
}

fn open_on_spotify<'a>(uri: &str, name: &str) -> Element<'a, Message> {
    enz_toolbar_icon("external-link", msg(Event::OpenOnSpotify(uri.to_owned())))
        .aria_label(format!("Open {name} in Spotify"))
        .tooltip("Open in Spotify")
        .id(id("music-spotify-open", uri))
        .into_element()
}

/// Cards in as many columns as fit `width`.
fn grid(cards: Vec<Element<'_, Message>>, width: f32) -> Element<'_, Message> {
    let columns = (((width + CARD_GAP) / (CARD_MIN + CARD_GAP)).floor() as usize).clamp(2, 8);
    let mut rows = column![].spacing(CARD_GAP);
    let mut cards = cards.into_iter().peekable();
    while cards.peek().is_some() {
        let mut line = row![].spacing(CARD_GAP);
        for slot in 0..columns {
            let _ = slot;
            line = line.push(match cards.next() {
                Some(card) => container(card).width(Length::FillPortion(1)),
                None => container(Space::new()).width(Length::FillPortion(1)),
            });
        }
        rows = rows.push(line);
    }
    rows.into()
}

/// A square picture that follows the card's width.
fn card_art<'a>(
    art: &'a PodcastArtworkCache,
    uri: &str,
    round: bool,
    glyph: &'static str,
    theme: &'a EnzTheme,
) -> Element<'a, Message> {
    use enzui_widgets::aspect_ratio::enz_aspect_ratio;
    let inner: Element<'a, Message> = match art.channel_art(&art_key(uri)) {
        Some(cover) => {
            let image = enz_image_handle(cover.handle().clone())
                .fit(EnzImageFit::Cover)
                .width(Length::Fill)
                .height(Length::Fill);
            if round {
                // Round, at any width: the radius outgrows half the edge.
                image.radius(1000.0).into_element()
            } else {
                image.radius(theme.radius.lg).into_element()
            }
        }
        None => enz_surface(
            container(
                enz_icon(glyph)
                    .size(theme.sizing.icon_xl)
                    .tint(theme.palette.text_muted)
                    .into_element(),
            )
            .center_x(Length::Fill)
            .center_y(Length::Fill),
        )
        .tone(SurfaceTone::SurfaceAlt)
        .radius(if round { 1000.0 } else { theme.radius.lg })
        .border_width(0.0)
        .width(Length::Fill)
        .height(Length::Fill)
        .into_element(),
    };
    enz_aspect_ratio(inner, 1.0).into()
}

fn card<'a>(
    picture: Element<'a, Message>,
    title: String,
    subtitle: String,
    center: bool,
    open: Event,
    key: &str,
    theme: &'a EnzTheme,
) -> Element<'a, Message> {
    use enzui_widgets::enz_text::TextAlign;
    let align = if center {
        TextAlign::Center
    } else {
        TextAlign::Left
    };
    let body = column![
        picture,
        enz_text(title.clone())
            .full_width()
            .align(align)
            .single_line_elided(),
        enz_text(subtitle)
            .size(TextSize::Caption)
            .color(theme.palette.text_muted)
            .full_width()
            .align(align)
            .single_line_elided(),
    ]
    .spacing(theme.spacing.xs())
    .width(Length::Fill);
    enz_pressable(body.into(), msg(open))
        .style(PressableStyle::Plain)
        .padding(theme.spacing.sm())
        .aria_label(title)
        .id(id("music-spotify-card", key))
        .into_element()
}

fn album_card<'a>(
    album: &'a Album,
    art: &'a PodcastArtworkCache,
    theme: &'a EnzTheme,
) -> Element<'a, Message> {
    let mut subtitle = album
        .artists
        .first()
        .map(|a| a.name.clone())
        .unwrap_or_default();
    if !album.year.is_empty() {
        subtitle = if subtitle.is_empty() {
            album.year.clone()
        } else {
            format!("{} · {subtitle}", album.year)
        };
    }
    card(
        card_art(art, &album.uri, false, "disc", theme),
        album.name.clone(),
        subtitle,
        false,
        Event::Open(Route::Album(album.uri.clone())),
        &album.uri,
        theme,
    )
}

fn artist_card<'a>(
    artist: &'a Artist,
    art: &'a PodcastArtworkCache,
    theme: &'a EnzTheme,
) -> Element<'a, Message> {
    card(
        card_art(art, &artist.uri, true, "mic", theme),
        artist.name.clone(),
        "Artist".to_owned(),
        true,
        Event::Open(Route::Artist(artist.uri.clone())),
        &artist.uri,
        theme,
    )
}

fn playlist_card<'a>(
    playlist: &'a Playlist,
    art: &'a PodcastArtworkCache,
    theme: &'a EnzTheme,
) -> Element<'a, Message> {
    card(
        card_art(art, &playlist.uri, false, "list-music", theme),
        playlist.name.clone(),
        format!("{} · {} songs", playlist.owner, playlist.total),
        false,
        Event::Open(Route::Playlist(playlist.uri.clone())),
        &playlist.uri,
        theme,
    )
}
