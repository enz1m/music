//! Content pane composition.

use super::*;

impl MusicApp {
    pub(crate) fn content_pane(&self, chrome_top: f32) -> Element<'_, Message> {
        let enz = self.themes.theme();
        // A wide window sets Now Playing beside the page, with no player bar
        // floating over the page's foot.
        let split = views::split_widths(self.window_size.width);
        let chrome_bottom = if split.is_some() {
            0.0
        } else {
            self.bottom_chrome_height
        };
        views::begin_page(chrome_top, chrome_bottom);
        let master_w = split
            .map_or(self.window_size.width, |(browse, _)| browse)
            .max(320.0);
        let master_content: Element<Message> = match self.current_view {
            View::Spotify => self.spotify.view(&self.podcast_art_cache, master_w, enz),
            View::Home => views::home_view(
                &self.library,
                &self.playback,
                enz,
                self.sort_mode,
                self.filter,
                self.track_list_expansion,
            ),
            View::Albums => views::albums_view(
                &self.library,
                &self.playback,
                enz,
                master_w,
                true,
                self.filter,
                self.sort_mode,
                &self.album_projection,
                self.album_scroll.clone(),
                &self.album_anchor,
                &self.album_anchor_columns,
                &self.album_geometry,
            ),
            View::Artists => views::artists_view(
                &self.library,
                self.sort_mode,
                enz,
                &self.artist_projection,
                self.artist_scroll.clone(),
            ),
            View::Genres => views::genres_view(&self.library, enz),
            View::Tags => views::tags_view(&self.library, enz),
            View::Playlist => views::playlists_view(
                &self.library,
                &self.playback,
                enz,
                self.filter,
                views::PlaylistTransferView {
                    import_pending: self.playlist_transfer.import_portal_pending()
                        || self.import_job.is_some(),
                    export_pending: self.export_jobs.values().next().map(|j| j.playlist_id),
                },
            ),
            View::Radio => views::radio_view(
                &self.radio,
                &self.podcast_charts,
                &self.playback,
                &self.podcast_art_cache,
                master_w,
                enz,
            ),
            View::Podcasts => views::podcasts_view(
                &self.subscriptions,
                &self.subscribe_input,
                &self.podcast_search_query,
                &self.podcast_search_state,
                &self.podcast_charts,
                &self.podcast_art_cache,
                &self.playback,
                master_w,
                enz,
            ),
            View::Search => views::search_view(
                &self.library,
                &self.playback,
                enz,
                &self.search_query,
                self.track_list_expansion,
            ),
            View::Queue => views::queue_view(
                &self.library,
                &self.playback,
                enz,
                &self.queue_more_menu_model,
                &self.queue_more_menu,
            ),
        };
        let drop_enabled = !self.native_import_pending
            && !self.close_after_save
            && !self.now_playing_open
            && self.current_view != View::Spotify;
        let master_content: Element<Message> = {
            enzui_widgets::file_drop::enz_file_drop_target_with_receipt(
                master_content,
                move |drop| {
                    Message::FilesDropped(
                        drop,
                        match (self.current_view, self.filter) {
                            (View::Playlist, LibraryFilter::Playlist(id)) => {
                                crate::native_drop::Target::Playlist(id)
                            }
                            (View::Queue, _) => crate::native_drop::Target::Queue,
                            _ => crate::native_drop::Target::Library,
                        },
                    )
                },
            )
            .enabled(drop_enabled)
            .copy_only(true)
            .key((
                "music-library-import",
                format!("{:?}-{:?}", self.current_view, self.filter),
            ))
            .into()
        };
        // Storage and service notices float bottom-right with the toasts (see
        // `view_root`), so every destination receives the same single instance.
        let master_content: Element<Message> =
            enzui_core::collect::app_subtree_boundary(master_content).into();
        // A page without its own page scroll (virtual lists and grids) sits
        // clear of the floating titlebar and player bar.
        let master_content: Element<Message> = if views::page_scrolls_under_chrome() {
            master_content
        } else {
            container(master_content)
                .padding(
                    iced::Padding::new(0.0)
                        .top(chrome_top)
                        .bottom(chrome_bottom),
                )
                .into()
        };

        let Some((browse_w, now_playing_w)) = split else {
            return container(master_content)
                .width(Length::Fill)
                .height(Length::Fill)
                .into();
        };
        iced::widget::row![
            container(master_content)
                .width(Length::Fixed(browse_w))
                .height(Length::Fill),
            self.now_playing_column(now_playing_w, chrome_top),
        ]
        .into()
    }

    /// The split layout's Now Playing column: the same panel the sheet holds,
    /// always present, on a raised tone that runs the window's full height.
    /// Its top clears the titlebar band, where the window's close button sits.
    fn now_playing_column(&self, width: f32, chrome_top: f32) -> Element<'_, Message> {
        let enz = self.themes.theme();
        let stream = self
            .playback
            .stream_selection()
            .and_then(|sel| self.resolve_stream(sel));
        let bottom = enz.spacing.lg();
        let panel = views::now_playing_panel(
            &self.playback,
            &self.library,
            stream.as_ref(),
            self.stream_cover(),
            self.playback
                .current_track_id
                .and_then(|id| self.waveforms.get(&id))
                .and_then(|peaks| peaks.as_deref()),
            self.seek_preview,
            enz,
            views::NowPlayingFrame {
                width,
                height: (self.window_size.height - chrome_top - bottom).max(0.0),
                landscape: false,
            },
            self.spotify_remote().as_ref(),
        );
        // The cover's colours wash the column, as in players like Amberol.
        iced::widget::stack![
            views::art_backdrop(self.now_playing_palette(), 1.0, enz),
            container(panel)
                .padding(iced::Padding::new(0.0).top(chrome_top).bottom(bottom))
                .height(Length::Fill),
        ]
        .width(Length::Fixed(width))
        .height(Length::Fill)
        .into()
    }

    /// The station or episode a selection names, from the subscriptions or
    /// the previewed podcast.
    pub(crate) fn resolve_stream(
        &self,
        selection: &StreamSelection,
    ) -> Option<domain::StreamNowPlaying> {
        domain::resolve_stream(selection, &self.radio, &self.subscriptions)
            .or_else(|| {
                self.podcast_charts.unsubscribed_feeds().find_map(|feed| {
                    domain::resolve_stream(selection, &self.radio, std::slice::from_ref(feed))
                })
            })
            .or_else(|| match selection {
                // Whatever plays keeps a player card with its controls, even
                // when its podcast is no longer known.
                StreamSelection::Episode { feed_url, .. } => Some(domain::StreamNowPlaying {
                    title: domain::PODCAST_EPISODE_DETAIL.to_owned(),
                    subtitle: feed_url
                        .split("://")
                        .nth(1)
                        .and_then(|rest| rest.split('/').next())
                        .unwrap_or(feed_url)
                        .to_owned(),
                    detail: String::new(),
                }),
                StreamSelection::Station(_) => None,
            })
    }

    /// What the player shows while Spotify is its source.
    pub(crate) fn spotify_remote(&self) -> Option<views::RemoteView<'_>> {
        let spotify = self.spotify.remote()?;
        let art = spotify
            .cover_key
            .as_deref()
            .and_then(|key| self.podcast_art_cache.channel_art(key));
        Some(views::RemoteView { spotify, art })
    }

    /// The tuned station's logo or the selected episode's cover (its
    /// podcast's, when it has none of its own).
    pub(crate) fn stream_cover(&self) -> Option<&crate::art::CoverArt> {
        match self.playback.stream_selection()? {
            crate::StreamSelection::Station(id) => {
                let station = self.radio.station(*id)?;
                self.podcast_art_cache
                    .channel_art(&crate::update::radio_art_key(&station.remote_uuid))
            }
            crate::StreamSelection::Episode { feed_url, guid } => {
                self.podcast_art_cache.episode_art(feed_url, guid)
            }
        }
    }

    /// The current track's (or station's, or episode's) cover colours.
    pub(crate) fn now_playing_palette(&self) -> Option<[[u8; 3]; 3]> {
        if let Some(remote) = self.spotify_remote() {
            return remote.art.map(crate::art::CoverArt::palette);
        }
        if let Some(track) = self.playback.current_track(&self.library) {
            return track.cover.as_ref().map(crate::art::CoverArt::palette);
        }
        match self.playback.stream_selection()? {
            crate::StreamSelection::Station(id) => {
                let station = self.radio.station(*id)?;
                self.podcast_art_cache
                    .channel_art(&crate::update::radio_art_key(&station.remote_uuid))
            }
            crate::StreamSelection::Episode { feed_url, guid } => {
                self.podcast_art_cache.episode_art(feed_url, guid)
            }
        }
        .map(crate::art::CoverArt::palette)
    }

    /// Now Playing over the whole shell: the sheet's dimming covers the
    /// titlebar and sidebar as well as the page. The player bar floats above
    /// that layer, over the content area, so it can slide away as the sheet
    /// rises and back as it closes.
    pub(crate) fn now_playing_layer<'a>(
        &'a self,
        shell: Element<'a, Message>,
    ) -> Element<'a, Message> {
        let enz = self.themes.theme();
        // The resolved station / episode card the now-playing surfaces
        // print. `stream_selection` applies the track-wins precedence rule, and
        // `resolve_stream` returns `None` for a selection whose feed or station
        // is gone, so a stale selection degrades to the ordinary empty state.
        let stream = self
            .playback
            .stream_selection()
            .and_then(|sel| self.resolve_stream(sel));
        // The geometry the shell resolves, for the titlebar's height and
        // where the content area starts.
        // The split layout holds Now Playing in its own column.
        if views::split_widths(self.window_size.width).is_some() {
            return shell;
        }
        let geometry = enzui_widgets::app_shell::enz_app_shell::<Message>(|| {
            iced::widget::Space::new().into()
        })
        .layout(ShellLayout::FloatingTitlebar)
        .tokens(shell_tokens())
        .breakpoints(enz.breakpoints)
        .geometry_for(self.window_size.width);
        let chrome_top = geometry.content_inset[0];
        let content_left = (self.window_size.width - geometry.content_width).max(0.0);
        let stream_art = self.stream_cover();
        let mini = views::mini_player(
            &self.playback,
            &self.library,
            stream.as_ref(),
            stream_art,
            enz,
            self.spotify_remote().as_ref(),
        );
        // The player bar floats over the page's foot on frosted glass.
        // The bar reports a cursor over its whole area, so the stack hides
        // the pointer from the rows it floats over: they neither light up
        // under it nor keep a stale highlight when the pointer moves onto it.
        // (Capturing the events instead, as `opaque` does, left a row lit.)
        let player_bar = container(enzui_widgets::motion::motion(
            iced::widget::mouse_area(
                iced::widget::Sensor::new(mini).on_resize(Message::BottomChromeMeasured),
            )
            .interaction(iced::mouse::Interaction::Idle),
            !self.now_playing_open,
            enzui_widgets::motion::MotionEffect::Slide(enzui_widgets::motion::SlideDirection::Down),
        ))
        .width(Length::Fill)
        .height(Length::Fill)
        .padding(iced::Padding::new(0.0).left(content_left))
        .align_bottom(Length::Fill);
        let sheet = self.now_playing_open.then(|| {
            // The sheet hugs the panel when the window has room, and the
            // panel's art shrinks to fit (or moves beside the controls) when
            // it has not. It always stops short of the titlebar, so the
            // window's controls stay clear above it.
            let chrome = views::NP_SHEET_CLOSE_ROW + 2.0 * enz.spacing.sm();
            let (frame, sheet_height) = views::now_playing_frame(
                self.window_size,
                self.window_size.height - chrome_top - enz.spacing.md(),
                chrome,
                enz,
            );
            let panel = views::now_playing_panel(
                &self.playback,
                &self.library,
                stream.as_ref(),
                self.stream_cover(),
                self.playback
                    .current_track_id
                    .and_then(|id| self.waveforms.get(&id))
                    .and_then(|peaks| peaks.as_deref()),
                self.seek_preview,
                enz,
                frame,
                self.spotify_remote().as_ref(),
            );
            views::compact_now_playing_sheet(
                panel,
                sheet_height,
                frame.width,
                self.now_playing_palette(),
                enz,
            )
        });
        let hosted = enzui_widgets::modal_host::enz_modal_host(shell, sheet, |event| {
            Message::NowPlayingSheet(SheetEvent::from_host(event))
        })
        .placement(enzui_widgets::modal_host::SurfacePlacement::BottomEdge)
        .scope(enzui_widgets::modal_host::presentation_scope_id(
            "music-now-playing-sheet",
        ))
        .label("Now playing")
        .into_element();
        iced::widget::stack![hosted, player_bar].into()
    }
}
