//! Inner dispatch, folder pick, and shell presentation.

use super::*;

impl MusicApp {
    #[allow(clippy::too_many_lines)]
    pub(crate) fn update_inner(&mut self, message: Message) -> Task<Message> {
        // Dialog buttons leave bare keys uncaptured; EQ and Sleep toggles close
        // their own dialogs because Done sends the same message.
        if self.any_modal_open() {
            match &message {
                Message::TogglePlayPause
                | Message::NextTrack
                | Message::PreviousTrack
                | Message::ToggleShuffle
                | Message::CycleRepeat
                | Message::AdjustVolume(..)
                | Message::ToggleMute
                | Message::ToggleShortcuts => return Task::none(),
                Message::SelectView(View::Search | View::Queue) => return Task::none(),
                Message::ToggleEqualizer if !self.eq_open => return Task::none(),
                Message::ToggleSleepTimer if !self.sleep_open => return Task::none(),
                _ => {}
            }
        }
        // While Spotify is the player's source, the transport drives it.
        if self.spotify.is_active()
            && let Some(event) = self.spotify_transport(&message)
        {
            return self.update_inner(Message::Spotify(event));
        }
        match message {
            Message::FilesDropped(..) | Message::FilesImported(..) => Task::none(),
            Message::Spotify(event) => {
                if self.spotify.starts_playback(&event) && self.playback.playing {
                    self.request_pause();
                }
                // A link to an artist or album (from the player), or a setup
                // button in Settings, opens the page.
                let navigates = matches!(
                    event,
                    crate::spotify::Event::Open(_) | crate::spotify::Event::ChangeKeyAndOpen
                );
                let mut task = self.spotify.update(event).map(Message::Spotify);
                if navigates {
                    self.now_playing_open = false;
                    self.current_view = View::Spotify;
                    self.view_before_modal = None;
                    if self.rg_presentation.is_open() {
                        task = Task::batch([
                            task,
                            self.close_modal_focus_for_host(
                                ModalPresentationId::PlaybackPreferences,
                            ),
                        ]);
                    }
                }
                // Spotify's outcomes and errors are app notifications.
                self.post_spotify_notices();
                // New library rows bring their thumbnails.
                self.reconcile_and_dispatch_artwork();
                if self.spotify.is_active() && self.playback.playing {
                    // Spotify took over: the library's own playback pauses.
                    self.request_pause();
                }
                task
            }
            Message::PodcastChartsGenre(..)
            | Message::PodcastCountryPicker
            | Message::PodcastOpen(..)
            | Message::PodcastEpisodeDetails(..)
            | Message::CloseEpisodeDetails
            | Message::PodcastsScrolled(..)
            | Message::PodcastClose
            | Message::PodcastShowMoreEpisodes
            | Message::PodcastChartsCountry(..)
            | Message::PodcastChartsLoaded(..)
            | Message::OpmlImportFile
            | Message::OpmlExportFile
            | Message::OpmlTransferWake
            | Message::OpmlFileRead(..)
            | Message::OpmlFileSaved(..) => self.reduce_podcast_discover(message),
            Message::OpenSpotifySettings => {
                self.settings_page = views::SETTINGS_SPOTIFY_PAGE.to_owned();
                self.settings_page_open = true;
                if self.rg_presentation.is_open() {
                    return Task::none();
                }
                self.rg_mode_menu_open = false;
                self.rg_presentation.present(());
                self.open_modal_focus(
                    ModalPresentationId::PlaybackPreferences,
                    Some(enzui_core::identity::derive_id("music-spotify", "settings")),
                )
            }
            Message::OpenSpotifyDevices => {
                self.now_playing_open = false;
                self.current_view = View::Spotify;
                self.view_before_modal = None;
                if self.spotify.devices_open {
                    Task::none()
                } else {
                    self.spotify
                        .update(crate::spotify::Event::ToggleDevices)
                        .map(Message::Spotify)
                }
            }
            // Routed to the center host by `update_core` before dispatch.
            #[cfg(feature = "center")]
            Message::Center(..) => Task::none(),
            Message::TogglePlaybackPreferences
            | Message::PlaybackPreferencesExitSettled(..)
            | Message::RgSelectMode(..)
            | Message::RgSetPreamp(..)
            | Message::RgSetPreventClip(..)
            | Message::OpenLibraryPreferences
            | Message::SelectSettingsPage(..)
            | Message::SettingsStackedBack
            | Message::RgModeMenu(..)
            | Message::TitlebarOverflow(..)
            | Message::ToggleSleepTimer
            | Message::SleepArm(..)
            | Message::SleepTick
            | Message::SleepSetFade(..)
            | Message::SleepCancel
            | Message::NowPlayingMenu(..)
            | Message::NavMenu(..)
            | Message::QueueMoreMenu(..)
            | Message::SortMenu(..)
            | Message::PlayTrackSet(..)
            | Message::AddTracksToQueue(..)
            | Message::TrackDetailsPage(..)
            | Message::TrackDetailsReload
            | Message::TrackDetailsCancel
            | Message::TrackDetailsPoll
            | Message::SetTrackRating(..)
            | Message::SetRatedFilter(..)
            | Message::SetPlaybackRate(..)
            | Message::ShowTrackInfo(..)
            | Message::OpenContextMenu(..)
            | Message::OpenContextMenuFrom(..)
            | Message::OpenPodcastEpisodeMenu(..)
            | Message::ContextMenuEvent(..)
            | Message::ContextAction(..) => self.reduce_batch4(message),
            Message::ImportPlaylist
            | Message::ExportPlaylist(..)
            | Message::PlaylistTransferTick
            | Message::PlaylistTransferRetry
            | Message::PlaylistImportSettled { .. }
            | Message::PlaylistExportSettled { .. }
            | Message::ExpandTrackList(..)
            | Message::SearchQueryChanged(..)
            | Message::RadioGenre(..)
            | Message::RadioSearchClear
            | Message::RadioAddName(..)
            | Message::RadioAddUrl(..)
            | Message::RadioAddSubmit
            | Message::RadioSearchChanged(..)
            | Message::RadioSearchSubmit
            | Message::RadioToggleFavorite(..)
            | Message::RadioTune(..)
            | Message::RadioCatalogLoad
            | Message::RadioCatalogRetry
            | Message::RadioCatalogPoll
            | Message::RadioCatalogLoaded(..)
            | Message::PodcastSearchInputChanged(..)
            | Message::PodcastSearchSubmit
            | Message::PodcastSearchPoll
            | Message::PodcastArtworkPoll
            | Message::PodcastSearchClear
            | Message::PodcastSubscribeFromResult(..)
            | Message::PodcastSubscribeInputChanged(..)
            | Message::PodcastSubscribeSubmit
            | Message::PodcastFeedFetched(..)
            | Message::PodcastPoll
            | Message::PodcastSubscribeRetry(..)
            | Message::PodcastUnsubscribe(..)
            | Message::PodcastEpisodeSelect(..)
            | Message::OpenManageFeeds
            | Message::CloseManageFeeds
            | Message::OpmlImportAction(..)
            | Message::OpmlImportSubmit
            | Message::OpmlExport
            | Message::ToggleEqualizer
            | Message::EqSetEnabled(..)
            | Message::EqSetClipProtection(..)
            | Message::EqPresetMenu(..)
            | Message::EqSelectPreset(..)
            | Message::EqSetBand { .. } => self.reduce_batch3(message),
            Message::RescanComplete(..)
            | Message::WatchDeliveries(..)
            | Message::LibraryWatchTick
            | Message::RootScanComplete(..)
            | Message::RootScanFailed { .. }
            | Message::SelectAlbum(..)
            | Message::SelectArtist(..)
            | Message::SelectGenre(..)
            | Message::SelectTag(..)
            | Message::ReorderQueue { .. }
            | Message::SelectPlaylist(..)
            | Message::ClearFilter
            | Message::SetSort(..)
            | Message::PlayTrack(..)
            | Message::PlayTrackNext(..)
            | Message::AddTrackToQueue(..)
            | Message::RemoveFromQueue(..)
            | Message::MoveToTopOfQueue(..)
            | Message::MoveUpInQueue(..)
            | Message::MoveDownInQueue(..)
            | Message::JumpToQueueEntry(..)
            | Message::ClearQueue
            | Message::SaveQueueAsPlaylist
            | Message::PlayLibrary
            | Message::ShuffleLibrary
            | Message::RescanLibrary
            | Message::RescanTrack(..)
            | Message::RemoveTrack(..)
            | Message::CreatePlaylist
            | Message::AddTrackToPlaylist { .. }
            | Message::DeletePlaylist(..)
            | Message::ConfirmDeletePlaylist(..)
            | Message::PlaylistDeleteConfirm(..)
            | Message::RenamePlaylistOpen(..)
            | Message::RenamePlaylistDraftChanged(..)
            | Message::RenamePlaylistCommit
            | Message::RenamePlaylistCancel
            | Message::OpenPlaylistPicker(..)
            | Message::ClosePlaylistPicker
            | Message::PickerNameChanged(..)
            | Message::PickerCreateAndAdd
            | Message::PickerAddToExisting(..) => self.reduce_batch2(message),
            Message::DesktopWake
            | Message::DesktopFilesOpened(..)
            | Message::A11yWake(..)
            | Message::AudioServiceWake(..)
            | Message::MprisCallWake(..)
            | Message::MprisConnectSettled(_)
            | Message::MprisLifecycleWake
            | Message::Theme(..)
            | Message::SelectView(..)
            | Message::ExitModal
            | Message::FocusNext
            | Message::FocusPrevious
            | Message::ModalHost(..)
            | Message::ToggleShortcuts
            | Message::Dismiss
            | Message::ToggleNowPlaying
            | Message::WaveformReady(..)
            | Message::NowPlayingSheet(..)
            | Message::ToggleSidebar
            | Message::SidebarState(..)
            | Message::DismissSidebar
            | Message::Window(..)
            | Message::PersistenceFinished { .. }
            | Message::RetryPersistence
            | Message::DiscardUnsavedAndClose
            | Message::RetirePersistence
            | Message::AuthorityReacquired(..)
            | Message::TogglePlayPause
            | Message::TrackEnded { .. }
            | Message::NextTrack
            | Message::PreviousTrack
            | Message::ToggleShuffle
            | Message::CycleRepeat
            | Message::SetVolume(..)
            | Message::AdjustVolume(..)
            | Message::ToggleMute
            | Message::Seek(..)
            | Message::SeekDragStart
            | Message::SeekPreview(..)
            | Message::SeekCommit
            | Message::PlaybackCompleted { .. }
            | Message::WindowResized(..)
            | Message::BottomChromeMeasured(..)
            | Message::AddFolder
            | Message::RemoveFolder(..)
            | Message::PortalTick
            | Message::PortalFolderOutcome(..)
            | Message::Toast(..)
            | Message::DismissFolderFailure
            | Message::HideNotice(..)
            | Message::ScanComplete(..)
            | Message::BootStateSettled(..) => self.reduce_batch1(message),
        }
    }

    /// Routes a terminal folder-chooser outcome into the app.
    ///
    /// Receives both late replies from [`Message::PortalTick`] and requests
    /// that never started from [`Message::AddFolder`]:
    ///
    /// * Success adds the folder and scans it off-thread
    ///   ([`Message::ScanComplete`]); a result with no local directory or an
    ///   already added folder is reported as a notice.
    /// * Cancellation shows no notice.
    /// * Every failure class shows a Library notice via
    ///   [`picker::failure_text`].
    pub(crate) fn finish_folder_pick(
        &mut self,
        target: picker::PickerTarget,
        outcome: enzui_widgets::file_chooser::PortalOutcome<
            enzui_widgets::file_chooser::FileResult,
        >,
    ) -> Task<Message> {
        use enzui_widgets::file_chooser::PortalOutcome;
        match &outcome {
            PortalOutcome::Completed(result) => match target {
                picker::PickerTarget::AddFolder => match picker::first_local_dir(result) {
                    Some(dir) if self.library.add_folder(dir.clone()) => {
                        self.mark_persistence_dirty();
                        // Scan and watch through the bounded typed path automatic
                        // updates use; the folder's tracks appearing is the
                        // acknowledgement.
                        let now = std::time::Instant::now();
                        self.library_watch.register_root(dir.clone(), now);
                        self.post_folder_status(
                            format!("Scanning {}…", dir.to_string_lossy()),
                            ToastTone::Info,
                        );
                        let dispatches = self.library_watch.dispatch_due(now);
                        return self.dispatch_root_scans(dispatches);
                    }
                    Some(dir) => {
                        // Already in the library; say so rather than report an add.
                        self.post_folder_status(
                            format!("{} is already a library folder", dir.to_string_lossy()),
                            ToastTone::Warning,
                        );
                    }
                    None => {
                        self.post_folder_status(
                            "That location is not a local folder — nothing to add",
                            ToastTone::Warning,
                        );
                    }
                },
            },
            // Cancellation is a decision, not an error: no notice.
            PortalOutcome::Cancelled(_) => {}
            // `failure_text` matches the outcome enum exhaustively.
            other => {
                // A portal failure is a standing host fact, so it becomes the
                // retained banner rather than an expiring acknowledgement.
                if let Some(text) = picker::failure_text(other) {
                    self.folder_failure = Some(text);
                }
            }
        }
        Task::none()
    }

    /// The one width authority for the navigation-split sidebar.
    ///
    /// Resolves Inline / Overlay / Hidden with the same `resolve_for_width`
    /// `enz_navigation_split_view` uses; `update` and `view` both read it, so
    /// the rendered presentation and toggle semantics agree.
    pub(crate) fn sidebar_presentation(&self) -> SidebarPresentation {
        Self::resolve_sidebar_presentation(
            self.nav,
            self.themes.theme().breakpoints,
            self.window_size.width,
        )
    }

    /// The pure form of [`Self::sidebar_presentation`], with every input passed
    /// in.
    ///
    /// `EnzAppShell::forced_by(..)` calls it more than once per frame, including
    /// for the state a toggle press would publish, to prove the toggle is live;
    /// it must therefore have no ambient state.
    pub(crate) fn resolve_sidebar_presentation(
        nav: NavSplitState,
        breakpoints: enzui_theme::BreakpointScale,
        width: f32,
    ) -> SidebarPresentation {
        if width <= 0.0 {
            return SidebarPresentation::Hidden;
        }
        resolve_for_width(
            nav.preference,
            breakpoints,
            NavSplitGeometry::new(width)
                .with_sidebar(SidebarWidth::new(SIDEBAR_WIDTH))
                .with_content_minimum(breakpoints.lg() - SIDEBAR_WIDTH),
        )
    }

    /// The Spotify event a transport message stands for, while Spotify is
    /// the player's source.
    fn spotify_transport(&mut self, message: &Message) -> Option<crate::spotify::Event> {
        use crate::spotify::Event;
        Some(match message {
            Message::TogglePlayPause => Event::TogglePlay,
            Message::NextTrack => Event::Next,
            Message::PreviousTrack => Event::Previous,
            Message::ToggleShuffle => Event::Shuffle,
            Message::CycleRepeat => Event::CycleRepeat,
            Message::SetVolume(volume) => Event::Volume(*volume),
            Message::AdjustVolume(delta) => {
                let now = self.spotify.remote().and_then(|r| r.volume).unwrap_or(0.5);
                Event::Volume((now + f64::from(*delta)).clamp(0.0, 1.0))
            }
            Message::ToggleMute => Event::ToggleMute,
            Message::Seek(position) => {
                // Spotify's position moves at once; no preview to hold.
                self.seek_preview = None;
                self.seek_settle_until = None;
                Event::Seek(*position)
            }
            _ => return None,
        })
    }
}
