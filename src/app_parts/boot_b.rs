//! Environment boot and themed boot.

use super::*;

impl MusicApp {
    /// `boot_settled` with the configured library folders overridden.
    ///
    /// An empty vec configures no folder, the fresh-install state.
    pub(crate) fn boot_with(
        scan_settled: bool,
        start_playing: bool,
        folders: Vec<std::path::PathBuf>,
    ) -> (Self, Task<Message>) {
        Self::boot_with_theme(
            scan_settled,
            start_playing,
            folders,
            ThemeIntegration::boot(),
        )
    }

    pub(crate) fn boot_with_theme(
        scan_settled: bool,
        start_playing: bool,
        folders: Vec<std::path::PathBuf>,
        (themes, theme_boot): (ThemeIntegration, Task<ThemeEvent>),
    ) -> (Self, Task<Message>) {
        let window_size = DEFAULT_WINDOW_SIZE;

        // Boot starts empty; the bounded scan below fills the library from the
        // folders `library_root` resolved, and none means the fresh-install state.
        let mut library = Library::new();
        library.folders = folders;

        // Explicit settled boot is used by deterministic startup checks.
        if scan_settled {
            settle_scan_now(&mut library);
        }

        let mut playback = PlaybackState::default();

        if start_playing && let Some(&first) = library.track_order.first() {
            playback.load_current_track(first, &library);
        }

        // One bounded owner drives the off-thread state load and reacquire; a
        // single worker suffices because `boot_load_worker_spawned` admits only
        // one in flight. A spawn failure degrades to a permanently-unavailable
        // owner that settles every submission as shut down.
        let boot_load_owner = enzui_core::blocking_task::BlockingTaskOwner::new(
            "music-state-load",
            std::num::NonZeroUsize::new(1).expect("nonzero worker count"),
            std::num::NonZeroUsize::new(1).expect("nonzero queue capacity"),
        )
        .unwrap_or_else(|_| {
            enzui_core::blocking_task::BlockingTaskOwner::unavailable(
                std::num::NonZeroUsize::new(1).expect("nonzero queue capacity"),
            )
        });

        let mut app = Self {
            themes,
            library,
            album_projection: views::IdProvider::default(),
            artist_projection: views::IdProvider::default(),
            projection_key: std::cell::Cell::new(None),
            album_scroll: Default::default(),
            album_anchor: std::cell::RefCell::new(None),
            album_geometry: Default::default(),
            album_anchor_columns: std::cell::Cell::new(0),
            artist_scroll: Default::default(),
            playback,
            // Audio stays `None` until the shipping boot's `try_spawn_audio`
            // succeeds.
            media: enzui_services::media::MediaState::new(),
            audio: None,
            audio_receiver: None,
            mpris: mpris_runtime::Runtime::inert(),
            mpris_source: None,
            persist: persistence::Persist::inert(),
            persistence_dirty: false,
            persistence_queue: persistence::PersistenceQueue::default(),
            pending_save: None,
            persistence_notice: None,
            close_after_save: false,
            #[cfg(feature = "center")]
            center: None,
            fetch_close_veto_reported: false,
            pending_playback_session: None,
            pending_track_stats: None,
            initial_scan_roots: std::collections::HashSet::new(),
            boot_load_generation: 0,
            boot_load_worker_spawned: false,
            boot_load_owner,
            native_import_pending: false,
            desktop_instance: None,
            desktop_paths: Vec::new(),
            desktop_open_pending: false,
            native_import_receipts: Vec::new(),
            fallback_folders: Vec::new(),
            current_view: View::Home,
            view_before_modal: None,
            now_playing_open: false,
            nav: NavSplitState::new(),
            window_size,
            selected_album_id: None,
            filter: LibraryFilter::None,
            sort_mode: SortMode::Album,
            search_query: String::new(),
            track_list_expansion: domain::TrackListExpansion::default(),
            next_playlist_seq: 1,
            playlist_picker: None,
            playlist_rename: None,
            shortcuts_open: false,
            eq: EqState::default(),
            eq_open: false,
            eq_preset_menu_open: false,
            sleep_timer: SleepTimerState::Off,
            sleep_pause_pending: false,
            sleep_last_tick: None,
            sleep_open: false,
            sleep_fade_choice: true,
            sleep_armed_preset: None,
            now_playing_menu: MenuState::closed(),
            now_playing_menu_model: now_playing_menu_model(1.0),
            nav_menu: MenuState::closed(),
            nav_menu_model: nav_menu_model(View::Home),
            queue_more_menu: MenuState::closed(),
            queue_more_menu_model: queue_more_menu_model(),
            sort_menu: MenuState::closed(),
            sort_menu_model: sort_menu_model(),
            titlebar_overflow_open: false,
            rg: ReplayGainState::default(),
            rg_presentation: enzui_widgets::modal_host::ModalPresentation::default(),
            settings_page: views::SETTINGS_LIBRARY_PAGE.to_owned(),
            settings_page_open: false,
            rg_mode_menu_open: false,
            subscribe_input: String::new(),
            subscriptions: Vec::new(),
            // `podcasts::default_transport()` is fail-closed under `cfg(test)`,
            // so a suite cannot reach DNS through this constructor.
            radio_catalog_service: radio_catalog::RadioCatalogService::shipping(),
            radio_top_list_generation: 0,
            radio_favorites_pending: false,
            podcast_service: podcasts::PodcastService::shipping(),
            podcast_requests: podcasts::PodcastRequests::default(),
            podcast_search: podcasts::search::PodcastSearchService::shipping(),
            podcast_search_query: String::new(),
            podcast_search_generation: 0,
            podcast_search_state: PodcastSearchState::Idle,
            episode_resume: std::collections::BTreeMap::new(),
            podcast_artwork: podcasts::artwork::PodcastArtworkService::shipping(),
            podcast_art_cache: podcasts::artwork::PodcastArtworkCache::new(),
            // Seeded on both boot paths; see `radio.rs`.
            spotify: spotify::State::default(),
            radio: radio_state_from_env(),
            manage_feeds_open: false,
            opml_import: text_editor::Content::new(),
            opml_export: String::new(),
            opml_transfer: crate::opml_transfer::OpmlTransfer::default(),
            podcast_charts: crate::PodcastCharts {
                country: crate::podcasts::charts::country(),
                ..crate::PodcastCharts::default()
            },
            folder_chooser: picker::Picker::new(),
            toasts: enzui_widgets::toast_host::ToastHost::new()
                .id_prefix(TOAST_ID_PREFIX)
                // One row, no waiting region: superseded gesture outcomes are
                // noise.
                .max_visible(1)
                .waiting_budget(
                    enzui_widgets::toast_host::WaitingBudget::unbounded().max_waiting(Some(0)),
                ),
            toast_expiry: enzui_widgets::toast_host::ToastExpirySchedule::default(),
            bottom_chrome_height: 0.0,
            waveforms: std::collections::HashMap::new(),
            waveform_pending: None,
            seek_dragging: false,
            seek_preview: None,
            seek_settle_until: None,
            folder_failure: None,
            hidden_notices: Vec::new(),
            context_menu: None,
            context_menu_generation: 0,
            playlist_delete: None,
            playlist_delete_confirm: enzui_core::confirm::ConfirmState::new(None),
            track_info_id: None,
            episode_details: None,
            track_details: None,
            track_details_service: track_details::Service::default(),
            track_details_generation: 0,
            rate_policy: 1.0,
            rate_requested_generation: None,
            audio_live_generation: None,
            volume_pending: None,
            modal_focus: modal_focus::ModalFocusSet::default(),
            library_watch: library_watch::LibraryWatcher::shipping(),
            availability_generation: 0,
            availability_exhausted: false,
            restore_intent: None,
            restore_session: 0,
            pending_source_open: None,
            playlist_transfer: playlist_transfer::PlaylistTransfer::new(),
            import_job: None,
            export_jobs: std::collections::HashMap::new(),
        };

        // Every scan shares one finite owner; after the inline settle walk
        // nothing is dispatched.
        let now = std::time::Instant::now();
        for root in app.library.folders.clone() {
            app.library_watch.register_root(root, now);
        }
        let boot_scan = if scan_settled {
            Task::none()
        } else {
            let dispatches = app.library_watch.dispatch_due(now);
            app.initial_scan_roots = dispatches.iter().map(|d| d.root.clone()).collect();
            app.dispatch_root_scans(dispatches)
        };

        app.refresh_view_projections();
        let modal_focus_boot = app.arm_boot_modal_focus();
        (
            app,
            Task::batch([
                boot_scan,
                theme_boot.map(|event| Message::Theme(Box::new(event))),
                modal_focus_boot,
            ]),
        )
    }
}
