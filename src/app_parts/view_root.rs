//! Root view composition.

use super::*;

impl MusicApp {
    pub(crate) fn view(&self) -> Element<'_, Message> {
        // Refresh only after the model/filter/sort generation changes, which also
        // covers fixtures that call `Library::reindex` directly.
        self.refresh_view_projections();
        let enz = self.themes.theme();

        // The app shell owns titlebar/content composition (see [`shell_tokens`]).
        // Slots take closures because the shell rebuilds its subtree during
        // layout. The titlebar floats over the page on frosted glass that fades
        // into it, so the page scrolls under the bar.
        let split = views::split_widths(self.window_size.width);
        let base: Element<'_, Message> =
            enzui_widgets::app_shell::enz_app_shell_with_geometry(move |geometry| {
                self.content_pane(geometry.content_inset[0])
            })
            // No sidebar: destinations live in the title select.
            .layout(ShellLayout::FloatingTitlebar)
            .titlebar_scrim(true)
            .titlebar_background_with_geometry(move |geometry| {
                let band = views::frosted_titlebar_band(geometry.content_inset[0]);
                // Split: the frosted band backs the browse pane's bar only;
                // the Now Playing column runs up under the close button.
                match split {
                    Some((browse_w, _)) => iced::widget::row![
                        iced::widget::container(band).width(Length::Fixed(browse_w)),
                        iced::widget::Space::new().width(Length::Fill),
                    ]
                    .into(),
                    None => band,
                }
            })
            .tokens(shell_tokens())
            .breakpoints(enz.breakpoints)
            .titlebar(move || views::titlebar(self.titlebar_model(), enz))
            .into_element();
        // Now Playing rises over the whole window, so its dimming covers the
        // titlebar and sidebar too; the player bar floats above that layer.
        let base = self.now_playing_layer(base);

        // Keep the nonmodal, point-anchored host mounted so opening a menu
        // preserves the background tree's scroll state.
        let menu = self.context_menu.as_ref();
        let mut host = enz_optional_point_context_menu_host(
            base,
            menu.map(|open| (&open.model, &open.state)),
            Message::ContextMenuEvent,
        )
        .menu_width(context_menu::MENU_WIDTH)
        .id_prefix("music-context-menu");
        if let Some(open) = menu {
            // Resolve the accessible name through the app-level resolver
            // so a radio/podcast menu announces its subject, not a bare name.
            if let Some(label) = self.context_menu_label(
                open.target,
                open.radio_key.as_ref(),
                open.podcast_key.as_ref(),
            ) {
                host = host.label(label);
            }
            // Advertise the same retained target restoration will use.
            if let Some(invoker) = open
                .state
                .restore_target()
                .and_then(|target| target.invoker_id())
            {
                host = host.invoker(invoker.clone());
            }
        }
        let base: Element<'_, Message> = host.into_element();

        // Suppress presentation, not the host: retained teardown must preserve
        // the titlebar state that receives restored focus.
        let base: Element<'_, Message> = views::with_playback_preferences(
            base,
            &self.rg,
            enz,
            self.window_size,
            &self.rg_presentation,
            self.modal_focus
                .get(ModalPresentationId::PlaybackPreferences),
            !(self.manage_feeds_open || self.shortcuts_open || self.eq_open),
            views::PreferencesPages {
                folders: &self.library.folders,
                summary: (self.library.track_count() > 0)
                    .then(|| (self.library.track_count(), self.library.albums.len())),
                selected: &self.settings_page,
                drilled_in: self.settings_page_open,
                mode_menu_open: self.rg_mode_menu_open,
                spotify: Some(&self.spotify),
            },
        );

        // Overlay precedence mirrors the `Dismiss` reducer: shortcuts, then the
        // picker. At most one modal is presented.
        let presentation: Element<'_, Message> = if self.manage_feeds_open {
            // Highest precedence, matching the `Dismiss` reducer order.
            views::manage_feeds_overlay(
                &self.subscriptions,
                &self.opml_import,
                &self.opml_export,
                base,
                enz,
                self.modal_focus.require(ModalPresentationId::ManageFeeds),
            )
        } else if self.shortcuts_open {
            // Pass the window size so the dialog's `viewport_height` lifts the
            // 480 px default cap on tall viewports.
            views::shortcuts_overlay(
                base,
                self.window_size,
                self.modal_focus.require(ModalPresentationId::Shortcuts),
            )
        } else if self.eq_open {
            views::equalizer_overlay(
                base,
                &self.eq,
                self.eq_preset_menu_open,
                enz,
                self.modal_focus.require(ModalPresentationId::Equalizer),
            )
        } else if self.rg_presentation.value().is_some() {
            // Preserve precedence while settings is open or retained for exit.
            base
        } else if self.sleep_open {
            views::sleep_timer_overlay(
                base,
                self.sleep_timer,
                self.sleep_fade_choice,
                self.sleep_armed_preset,
                enz,
                self.modal_focus.require(ModalPresentationId::SleepTimer),
            )
        } else if let Some(picker) = &self.playlist_picker {
            views::playlist_picker_overlay(
                base,
                picker,
                &self.library,
                enz,
                self.modal_focus
                    .require(ModalPresentationId::PlaylistPicker),
            )
        } else if let Some(rename) = self
            .playlist_rename
            .as_ref()
            // Guard against a stale render; the delete reducer settles the modal.
            .filter(|r| self.library.playlists.iter().any(|p| p.id == r.playlist_id))
        {
            views::playlist_rename_overlay(
                base,
                rename,
                &self.library,
                enz,
                self.modal_focus
                    .require(ModalPresentationId::PlaylistRename),
            )
        } else if let Some(pid) = self
            .playlist_delete
            // Guard against a stale render; a delete/settle clears the target.
            .filter(|pid| self.library.playlists.iter().any(|p| p.id == *pid))
        {
            let name = self
                .library
                .playlists
                .iter()
                .find(|p| p.id == pid)
                .map(|p| p.name.clone())
                .unwrap_or_default();
            views::playlist_delete_overlay(
                base,
                &name,
                &self.playlist_delete_confirm,
                self.modal_focus
                    .require(ModalPresentationId::PlaylistDelete),
            )
        } else if let Some((feed, episode)) = self
            .episode_details
            .as_ref()
            .and_then(|(feed_url, guid)| self.feed_episode(feed_url, guid))
        {
            views::episode_details_overlay(
                base,
                feed,
                episode,
                &self.podcast_art_cache,
                self.playback
                    .is_current_episode(&feed.feed_url, &episode.guid),
                self.window_size,
                enz,
                self.modal_focus
                    .require(ModalPresentationId::EpisodeDetails),
            )
        } else if let Some(track) = self
            .track_info_id
            .and_then(|id| self.library.tracks.get(&id))
        {
            views::track_info_overlay(
                base,
                track,
                self.library.rating(track.id),
                self.track_details.as_ref(),
                self.window_size,
                enz,
                self.modal_focus.require(ModalPresentationId::TrackInfo),
            )
        } else {
            base
        };

        // The feedback corner floats over every `enz_modal_host`: their inert
        // background layers withdraw the subtree from semantics, so a toast inside
        // the shell would be missing from the AT tree while an overlay is open.
        // Standing notices stack above the transient toasts, bottom-right above
        // the player bar. Always mounted so the first toast does not re-parent
        // the app.
        let notices = self.notices_feedback();
        let corner = (notices.is_some() || !self.toasts.is_empty()).then(|| {
            let mut corner = iced::widget::column![]
                .spacing(enz.spacing.xs())
                .align_x(iced::Alignment::End);
            if let Some(notices) = notices {
                corner = corner.push(notices);
            }
            if !self.toasts.is_empty() {
                corner = corner.push(self.toasts.view().map(Message::Toast));
            }
            corner.into()
        });
        enzui_widgets::toast_host::enz_toast_layer(
            presentation,
            corner,
            enzui_widgets::toast_host::ToastOverlayPlacement::default()
                .bottom_inset(self.toast_bottom_inset()),
        )
    }
}

impl MusicApp {
    /// The content titlebar for the current destination: its title (the
    /// page heading), a back control on drilled-in pages and side trips, and
    /// the destination's commands.
    pub(crate) fn titlebar_model(&self) -> views::TitlebarModel<'_> {
        use views::TitleAction;
        let view = self.current_view;
        let mut title = view.label().to_string();
        let mut back: Option<(Message, String)> = None;
        let mut actions: Vec<TitleAction> = Vec::new();
        let play = |label: &str| {
            TitleAction::new("music.play", "play", label, Message::PlayLibrary, 30)
                .widget_id(enzui_core::identity::derive_id("music-titlebar", "play"))
        };
        let shuffle = |label: &str| {
            TitleAction::new(
                "music.shuffle",
                "shuffle",
                label,
                Message::ShuffleLibrary,
                20,
            )
            .widget_id(enzui_core::identity::derive_id("music-titlebar", "shuffle"))
        };
        let has_tracks = self.library.track_count() > 0;
        match (view, self.filter) {
            (View::Home, filter) => {
                if let LibraryFilter::Genre(gid) = filter
                    && let Some(genre) = self.library.genres.iter().find(|g| g.id == gid)
                {
                    title = genre.name.clone();
                    back = Some((Message::SelectView(View::Genres), "Back to Genres".into()));
                }
                if let LibraryFilter::Tag(tid) = filter
                    && let Some(tag) = self.library.tags.iter().find(|t| t.id == tid)
                {
                    title = tag.name.clone();
                    back = Some((Message::SelectView(View::Tags), "Back to Tags".into()));
                }
                // Play and shuffle live in the player bar for library lists.
                if matches!(filter, LibraryFilter::None | LibraryFilter::Rated) {
                    actions.push(
                        TitleAction::new(
                            "music.add-folder",
                            "folder-plus",
                            views::ADD_FOLDER_LABEL,
                            Message::AddFolder,
                            10,
                        )
                        .widget_id(enzui_core::identity::derive_id(views::ADD_FOLDER_ID, "")),
                    );
                }
            }
            (View::Albums, LibraryFilter::Album(aid)) if self.library.album(aid).is_some() => {
                back = Some((Message::ClearFilter, "Back to Albums".into()));
                actions.push(play("Play Album"));
                actions.push(shuffle("Shuffle Album"));
                actions.push(TitleAction::new(
                    "music.album-to-playlist",
                    "list-plus",
                    "Add Album to Playlist",
                    Message::OpenPlaylistPicker(PickerTarget::Album(aid)),
                    10,
                ));
            }
            (View::Albums, LibraryFilter::Artist(aid)) => {
                if let Some(artist) = self.library.artist(aid) {
                    title = artist.name.clone();
                }
                back = Some((Message::SelectView(View::Artists), "Back to Artists".into()));
                actions.push(play("Play Artist"));
                actions.push(shuffle("Shuffle Artist"));
            }
            (View::Playlist, LibraryFilter::Playlist(pid)) => {
                if let Some(pl) = self.library.playlists.iter().find(|p| p.id == pid) {
                    back = Some((Message::ClearFilter, "Back to Playlists".into()));
                    let has = !pl.track_ids.is_empty();
                    actions.push(
                        TitleAction::new(
                            "music.play",
                            "play",
                            "Play Playlist",
                            Message::PlayTrackSet(pl.track_ids.clone()),
                            30,
                        )
                        .enabled(has),
                    );
                    actions.push(shuffle("Shuffle Playlist").enabled(has));
                    actions.push(
                        TitleAction::new(
                            "music.rename-playlist",
                            "pencil",
                            views::RENAME_PLAYLIST_ACTION_LABEL,
                            Message::RenamePlaylistOpen(pid),
                            12,
                        )
                        .widget_id(views::rename_playlist_button_id(pid)),
                    );
                    let exporting = self.export_jobs.values().any(|j| j.playlist_id == pid);
                    actions.push(
                        TitleAction::new(
                            "music.export-playlist",
                            "file-output",
                            "Export Playlist…",
                            Message::ExportPlaylist(pid),
                            11,
                        )
                        .enabled(!exporting),
                    );
                    actions.push(
                        TitleAction::new(
                            "music.delete-playlist",
                            "trash-2",
                            "Delete Playlist…",
                            Message::ConfirmDeletePlaylist(pid),
                            5,
                        )
                        .destructive(),
                    );
                }
            }
            (View::Playlist, _) => {
                actions.push(
                    TitleAction::new(
                        "music.import-playlist",
                        "import",
                        "Import Playlist…",
                        Message::ImportPlaylist,
                        10,
                    )
                    .enabled(
                        !(self.playlist_transfer.import_portal_pending()
                            || self.import_job.is_some()),
                    )
                    .widget_id(enzui_core::identity::derive_id("music-playlists", "import")),
                );
                actions.push(
                    TitleAction::new(
                        "music.new-playlist",
                        "plus",
                        "New Playlist",
                        Message::CreatePlaylist,
                        20,
                    )
                    .widget_id(enzui_core::identity::derive_id("music-playlists", "new")),
                );
            }
            (View::Podcasts, _) => {
                if self.podcast_charts.open.is_some() {
                    back = Some((Message::PodcastClose, "Back to Podcasts".into()));
                }
                actions.push(
                    TitleAction::new(
                        "music.manage-feeds",
                        "rss",
                        "Manage Feeds",
                        Message::OpenManageFeeds,
                        10,
                    )
                    .widget_id(enzui_core::identity::derive_id(
                        views::MANAGE_FEEDS_BUTTON_ID,
                        "",
                    )),
                );
            }
            // Spotify's artist, album, playlist and search pages stack.
            (View::Spotify, _) if self.spotify.stack.len() > 1 => {
                back = Some((Message::Spotify(crate::spotify::Event::Back), "Back".into()));
            }
            // Search and Up Next close through their own (active) titlebar
            // toggles, so they carry no second back control.
            (View::Queue, _) => {
                if !self.playback.queue.is_empty() {
                    actions.push(
                        TitleAction::new(
                            "music.save-queue",
                            "list-plus",
                            "Save Queue as Playlist",
                            Message::SaveQueueAsPlaylist,
                            20,
                        )
                        .widget_id(enzui_core::identity::derive_id("music-queue", "save")),
                    );
                    actions.push(
                        TitleAction::new(
                            "music.clear-queue",
                            "list-x",
                            "Clear Queue",
                            Message::ClearQueue,
                            10,
                        )
                        .destructive(),
                    );
                } else if has_tracks {
                    actions.push(play("Play All"));
                    actions.push(shuffle("Shuffle Library"));
                }
            }
            _ => {}
        }
        views::TitlebarModel {
            view,
            title,
            back,
            actions,
            sort: (views::sort_applies_to(view) && !matches!(self.filter, LibraryFilter::Album(_)))
                .then_some(self.sort_mode),
            sort_menu_model: &self.sort_menu_model,
            sort_menu: &self.sort_menu,
            now_playing_menu_model: &self.now_playing_menu_model,
            now_playing_menu: &self.now_playing_menu,
            search_query: &self.search_query,
            nav_menu_model: &self.nav_menu_model,
            nav_menu: &self.nav_menu,
            split: views::split_widths(self.window_size.width),
            overflow_open: self.titlebar_overflow_open,
        }
    }
}
