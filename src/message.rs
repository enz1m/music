//! The Message event enum.

use super::*;

#[derive(Debug, Clone)]
pub enum Message {
    DesktopWake,
    DesktopFilesOpened(Result<Vec<domain::Track>, String>),
    Theme(Box<ThemeEvent>),
    Spotify(spotify::Event),
    /// One enzui-center connector event.
    #[cfg(feature = "center")]
    Center(enzui_agent::center::ConnectorEvent),
    /// Names the coalesced batch a shared accessibility wake represents; the
    /// applier ignores a batch an earlier drain already consumed.
    A11yWake(enzui_core::app_runtime::AccessibilityWake),
    /// The audio event receiver has events to drain.
    ///
    /// The wake carries only its generation; the reducer drains the receiver
    /// and turns each outcome into transport, queue or error follow-ups.
    AudioServiceWake(enzui_services::media::AudioServiceWake),
    MprisConnectSettled(mpris_runtime::ConnectDelivery),
    MprisCallWake(enzui_services::media::mpris::zbus_transport::MprisCallWake),
    MprisLifecycleWake,
    // Navigation
    SelectView(View),
    ToggleNowPlaying,
    /// A track's waveform finished decoding (`None`: it has none).
    WaveformReady(u64, Option<std::sync::Arc<[f32]>>),
    /// A unified event from the framework bottom sheet. Escape, the scrim and
    /// the visible close control all arrive as `Dismissed`; focus events stay
    /// distinct so Tab cannot accidentally close the surface.
    NowPlayingSheet(SheetEvent),
    ExitModal, // Search/Queue back to previous view

    /// Move keyboard focus to the next focusable control (Tab).
    ///
    /// Uses the framework's wrapping traversal
    /// ([`enzui_core::focus::traversal_operation`]); iced does not move
    /// focus on Tab by itself, and its stock pair drops focus at the last control.
    FocusNext,
    /// Move keyboard focus to the previous focusable control (Shift+Tab).
    FocusPrevious,
    /// Exact host event and owning presentation.
    ModalHost(
        modal_focus::ModalPresentationId,
        enzui_widgets::modal_host::ModalHostEvent,
    ),

    // Keyboard-shortcuts overlay
    /// Toggle the keyboard-shortcuts overlay (Ctrl+/).
    ToggleShortcuts,
    /// One unified "close": the topmost dismissable surface (shortcuts overlay,
    /// then picker, then a modal side-trip). Bound to Escape.
    Dismiss,

    // Shell / window chrome
    /// Toggle the sidebar programmatically, as a keyboard shortcut does.
    ///
    /// Resolves the presentation with the shell's own `resolve_for_width`, so
    /// the app never becomes a second collapse authority.
    ToggleSidebar,
    /// The already-flipped [`NavSplitState`] the shell's own toggle publishes.
    ///
    /// The reducer stores it verbatim; re-resolving would make the app a second
    /// collapse authority.
    SidebarState(NavSplitState),
    DismissSidebar,
    /// A window gesture (close, drag, maximize) from the framework `enz_title_bar`.
    ///
    /// Routed through the framework's `WindowCommand::into_task`, so every
    /// gesture has an arm.
    Window(enzui_widgets::title_bar_gesture::WindowCommand),

    // Library
    AddFolder,
    FilesDropped(
        enzui_widgets::file_drop::FileDrop,
        crate::native_drop::Target,
    ),
    FilesImported(
        crate::native_drop::Target,
        Vec<u64>,
        enzui_widgets::file_drop::FileDrop,
        Result<Vec<domain::Track>, String>,
    ),
    /// Remove the library folder at this path (the per-row "Remove folder" control).
    RemoveFolder(std::path::PathBuf),
    ScanComplete(Vec<domain::Track>),
    /// The deferred boot-time state load settled. Carries the bounded,
    /// off-thread read/parse result; the arm applies it only when the report's
    /// generation still matches `boot_load_generation`, else drops it.
    BootStateSettled(Box<BootStateReport>),
    /// The backend settled an authoritative playback completion for its source.
    ///
    /// Sent on `EndOfStream` or scrobble-threshold crossing and stamped with
    /// [`PlaybackState::completion_generation`]; a mismatched generation is
    /// discarded, so pause, seek or restart cannot misapply it.
    PlaybackCompleted {
        generation: u64,
        reason: PlaybackCompletionReason,
    },
    /// A full rescan of every library folder, from the `Rescan Library` control.
    RescanComplete(Vec<domain::Track>),
    /// A coalesced batch of directory-change deliveries from the library
    /// watcher's event-driven subscription. The reducer routes each through the
    /// watcher's typed policy (debounce, overflow, vanished, probe-failed).
    WatchDeliveries(Vec<enzui_core::file_tree_watch::WatchDelivery>),
    /// The library watcher's own low-rate wake, armed only while a debounce or
    /// busy-retry deadline is pending. Dispatches every due root scan.
    LibraryWatchTick,
    /// A bounded root scan settled. Applied only when its
    /// root-ownership-plus-generation stamp still matches the live in-flight
    /// scan; a removed-root or superseded completion is inert.
    RootScanComplete(Box<library_watch::RootScan>),
    /// A root scan submission was refused by the bounded owner (busy queue,
    /// cancellation, or shutdown). The watcher retains the intent and retries
    /// from a bounded wake rather than spinning.
    RootScanFailed {
        root: std::path::PathBuf,
        generation: u64,
    },
    /// A low-rate tick that drives `file_chooser::pump` while the folder
    /// chooser request is live. Emitted by the subscription only while
    /// `folder_chooser.is_pending()`.
    PortalTick,
    /// A terminal folder-chooser outcome reached the app. Routed through
    /// `MusicApp::finish_folder_pick` — success adds and scans the folder,
    /// cancellation is a silent decision, and every failure class becomes the
    /// retained folder-failure banner.
    PortalFolderOutcome(
        crate::picker::PickerTarget,
        enzui_widgets::file_chooser::PortalOutcome<enzui_widgets::file_chooser::FileResult>,
    ),
    SelectAlbum(u64),
    SelectArtist(u64),
    SelectGenre(u64),
    /// Open a tag's tracks (`LibraryFilter::Tag`).
    SelectTag(u64),
    /// Up Next drag and drop: move the occurrence to the insertion slot.
    ReorderQueue {
        entry: u64,
        slot: usize,
    },
    SelectPlaylist(u64),
    ClearFilter,

    // Sort
    SetSort(SortMode),

    // Playback
    TogglePlayPause,
    NextTrack,
    TrackEnded {
        generation: u64,
    },
    PreviousTrack,
    ToggleShuffle,
    CycleRepeat,
    /// Nudge volume by a delta, clamped to \[0,1\] (keyboard +/-).
    AdjustVolume(f32),
    SetVolume(f64),
    /// Toggle mute (keyboard M).
    ToggleMute,
    /// Show where Spotify plays (from the player's device button).
    OpenSpotifyDevices,
    /// Open Settings at its Spotify page.
    OpenSpotifySettings,
    Seek(f64),
    /// A pointer drag on the seek bar began: changes preview until release.
    SeekDragStart,
    /// The seek bar moved to this position (a drag previews it; a click or
    /// key seeks to it).
    SeekPreview(f64),
    /// The seek bar drag ended: seek once to the previewed position.
    SeekCommit,
    PlayTrack(u64),
    PlayTrackNext(u64),
    AddTrackToQueue(u64),
    RemoveFromQueue(usize),
    MoveToTopOfQueue(usize),
    /// Swap a queue entry with its predecessor (`Move up`).
    MoveUpInQueue(usize),
    /// Swap a queue entry with its successor (`Move down`).
    MoveDownInQueue(usize),
    /// Play the queue entry at this position.
    JumpToQueueEntry(usize),
    /// Empty the queue and stop.
    ClearQueue,
    /// Snapshot the queue into a new playlist.
    SaveQueueAsPlaylist,
    /// Seed the queue with the entire library in the current sort order
    /// (the `Play All` card).
    PlayLibrary,
    /// Seed the queue with the entire library in shuffled order and turn
    /// shuffle on.
    ShuffleLibrary,
    /// Rescan every configured library folder off the UI thread.
    RescanLibrary,
    /// Re-probe the root that owns one track, so a Missing or Undecodable row
    /// can recover to Present.
    RescanTrack(u64),
    /// Delete a single durable track from the library (never its file),
    /// updating queue/current state through shared domain logic and persisting.
    RemoveTrack(u64),

    // Playlists
    CreatePlaylist,
    AddTrackToPlaylist {
        playlist_id: u64,
        track_id: u64,
    },
    DeletePlaylist(u64),
    /// Open the Rename dialog for a playlist by stable id, seeding the field
    /// with its current name and capturing the invoking control for focus
    /// restoration.
    RenamePlaylistOpen(u64),
    RenamePlaylistDraftChanged(String),
    /// Commit the Rename dialog. An accepted draft renames by the captured
    /// stable id, marks persistence dirty, and closes; a rejected draft keeps
    /// the dialog open with an error line.
    RenamePlaylistCommit,
    RenamePlaylistCancel,

    // Add-to-playlist picker
    /// Open the picker for a target (track / album / now-playing).
    OpenPlaylistPicker(domain::PickerTarget),
    /// Close the picker without adding.
    ClosePlaylistPicker,
    /// The "New playlist…" name field changed.
    PickerNameChanged(String),
    /// Create a playlist from the typed name, add the picker's target, and close.
    PickerCreateAndAdd,
    /// Add the picker's target to an existing destination playlist, then close.
    PickerAddToExisting(u64),

    // Playlist import / export
    /// Open the host `OpenFile` chooser to import a playlist document.
    ImportPlaylist,
    /// Open the host `SaveFile` chooser to export the playlist with this stable id.
    ExportPlaylist(u64),
    /// Pump the playlist Import/Export portals on their completion wake.
    PlaylistTransferTick,
    /// The bounded, coalesced retry wake for a Busy-refused import/export intent.
    PlaylistTransferRetry,
    /// An import read/parse settled on the playlist owner, stamped with the
    /// request generation so a stale settlement is inert.
    PlaylistImportSettled {
        generation: u64,
        settlement: playlist_transfer::ImportSettlement,
    },
    /// An export serialize/publish settled on the playlist owner, stamped with
    /// the request generation.
    PlaylistExportSettled {
        generation: u64,
        settlement: playlist_transfer::ExportSettlement,
    },

    // Search
    SearchQueryChanged(String),

    /// Lift the `domain::TRACK_LIST_PAGE` cap on a track list (`Show all N tracks`).
    ///
    /// Scoped so Home's list and Search's `Tracks` section expand independently.
    ExpandTrackList(domain::TrackListScope),

    // Radio
    /// Show the popular stations of a genre (`None`: every genre).
    RadioGenre(Option<&'static str>),
    /// Leave the search and show the browse lists again.
    RadioSearchClear,
    /// The add-a-station form's name and stream URL.
    RadioAddName(String),
    RadioAddUrl(String),
    /// Add the station in the form.
    RadioAddSubmit,
    /// The Radio search field's content changed; typing alone does not filter
    /// the list (see `RadioSearchSubmit`).
    RadioSearchChanged(String),
    /// Apply the typed query (Enter).
    ///
    /// Separate from typing so the before-submit empty state differs from the
    /// no-matches state.
    RadioSearchSubmit,
    /// Star or unstar a station by id; local state only.
    RadioToggleFavorite(u64),
    /// Tune a station by id, the row's primary action.
    ///
    /// The framework URL policy and `MediaGate::DEFAULT` re-validate the stream;
    /// a refusal shows one warning, and only the pipeline's `SourceReady →
    /// Playing` sets `playback.playing`.
    RadioTune(u64),
    /// Start a top-list catalog fetch off-thread.
    ///
    /// A no-op while a fetch for a newer generation is in flight.
    RadioCatalogLoad,
    /// [`Self::RadioCatalogLoad`] from the Retry button, named separately for
    /// diagnostics.
    RadioCatalogRetry,
    /// Drain the bounded `RadioCatalogService` (same shape as
    /// [`Self::PodcastPoll`]).
    RadioCatalogPoll,
    /// A catalog completion delivered directly by a fixture or in-process worker.
    ///
    /// A reply whose generation is stale for its search or top-list request
    /// is discarded. Boxed because the station list dwarfs other payloads.
    RadioCatalogLoaded(Box<radio_catalog::CatalogResult>),

    // Podcasts
    /// The discovery search field's content changed.
    PodcastSearchInputChanged(String),
    /// Submit the discovery query to the bounded
    /// [`podcasts::search::PodcastSearchService`] under a new generation.
    PodcastSearchSubmit,
    /// Poll the bounded search worker pool without blocking the UI.
    ///
    /// The pool discards a stale query's reply by generation.
    PodcastSearchPoll,
    /// Clear the discovery query and its results back to `Idle`.
    PodcastSearchClear,
    /// Subscribe to a discovery result by its durable `feed_url`, through the
    /// same path as `PodcastSubscribeSubmit`.
    PodcastSubscribeFromResult(String),
    /// Poll the bounded artwork worker pool and fold decoded thumbnails into
    /// the identity-keyed cache.
    PodcastArtworkPoll,
    /// The subscribe-URL text field's content changed.
    PodcastSubscribeInputChanged(String),
    /// Submit the URL currently in the subscribe field. Adds the feed to
    /// `subscriptions` in `Loading` and queues the bounded
    /// [`podcasts::PodcastService`] off-thread.
    PodcastSubscribeSubmit,
    /// A generation-stamped worker completion. Only the newest attempt for the
    /// URL may update the subscription.
    PodcastFeedFetched(podcasts::FetchResult),
    /// Poll the bounded blocking worker pool without blocking the UI thread.
    PodcastPoll,
    /// Re-run the fetch for the subscription at that index after a failure.
    /// Flips it back to `Loading` and reuses the bounded service.
    PodcastSubscribeRetry(usize),
    /// Remove the subscription at that index (`Unsubscribe`).
    PodcastUnsubscribe(usize),
    /// Play a podcast episode identified by its durable `(feed_url, guid)` pair.
    ///
    /// A refetch that reorders the feed cannot move the selection, and a saved
    /// resume point is restored once the source is ready.
    PodcastEpisodeSelect(String, String),

    // Manage Feeds dialog
    /// Open the `Manage Feeds` dialog.
    OpenManageFeeds,
    /// Close the `Manage Feeds` dialog.
    CloseManageFeeds,
    /// An edit action on the OPML import text area.
    OpmlImportAction(text_editor::Action),
    /// Import the OPML currently in the import buffer: parse it with
    /// `podcasts::import_opml` and subscribe to each `(title, url)`, skipping
    /// URLs already subscribed. Clears the import buffer on success.
    OpmlImportSubmit,
    /// Snapshot the current subscriptions as OPML via `podcasts::export_opml`
    /// into the export buffer, shown read-only for copy.
    OpmlExport,
    /// Open an episode's details and full show notes.
    PodcastEpisodeDetails(String, String),
    /// Close the episode details.
    CloseEpisodeDetails,
    /// The podcasts list scrolled to this offset.
    PodcastsScrolled(f32),
    /// Open a podcast's page (previewing it when not subscribed).
    PodcastOpen(String),
    /// Back from a podcast's page.
    PodcastClose,
    /// List more of the open podcast's episodes.
    PodcastShowMoreEpisodes,
    /// Open or close the chart country list.
    PodcastCountryPicker,
    /// Show the charts of another country.
    PodcastChartsCountry(String),
    /// Show Apple's chart for a genre (`None`: overall).
    PodcastChartsGenre(Option<u32>),
    /// A chart arrived (or failed) for a genre.
    PodcastChartsLoaded(
        Option<u32>,
        Result<(String, Vec<podcasts::search::PodcastSearchResult>), String>,
    ),
    /// Choose an OPML file to import subscriptions from.
    OpmlImportFile,
    /// Choose where to save the subscriptions as OPML.
    OpmlExportFile,
    /// An OPML file dialog finished.
    OpmlTransferWake,
    /// A chosen OPML file was read.
    OpmlFileRead(Result<String, String>),
    /// The OPML file was saved.
    OpmlFileSaved(Result<(), String>),

    // Equalizer
    /// Open/close the equalizer dialog.
    ToggleEqualizer,
    /// Open or close the equalizer's preset menu.
    EqPresetMenu(bool),
    /// Enable / disable the equalizer.
    EqSetEnabled(bool),
    /// Toggle clip protection.
    EqSetClipProtection(bool),
    /// Apply a named preset by its index in `EqPreset::SELECTABLE`.
    EqSelectPreset(usize),
    /// Move one band's gain (dB). Re-resolves preset ↔ Custom.
    EqSetBand {
        index: usize,
        gain_db: f32,
    },

    // ReplayGain / Settings › Playback
    /// Open, close, or rapidly reopen the retained settings presentation.
    TogglePlaybackPreferences,
    /// Retire only the closing generation whose exit animation settled.
    PlaybackPreferencesExitSettled(ModalPresentationKey),
    /// Select a ReplayGain mode by its index in `ReplayGainMode::SELECTABLE`.
    RgSelectMode(usize),
    /// Move the ReplayGain preamp (dB). Clamped to the stated −15..+15 range.
    RgSetPreamp(f32),
    /// Toggle `Prevent clipping`.
    RgSetPreventClip(bool),
    /// Open Settings on its Library page (folders and rescan).
    OpenLibraryPreferences,
    /// A Settings page was chosen in the page list.
    SelectSettingsPage(String),
    /// A narrow (stacked) Settings went back from a page to the page list.
    SettingsStackedBack,
    /// The volume-normalization value menu opened (`true`) or closed.
    RgModeMenu(bool),
    /// Open or close the titlebar's `More actions` overflow menu.
    TitlebarOverflow(bool),

    // Sleep timer
    /// Open/close the sleep-timer dialog.
    ToggleSleepTimer,
    /// A tick that advances the sleep-timer countdown, subscribed only while
    /// one runs.
    SleepTick,
    /// Arm the timer from a preset index in `SleepPreset::SELECTABLE`.
    SleepArm(usize),
    /// Toggle the fade-out-at-end option for the next armed timer.
    SleepSetFade(bool),
    /// Cancel the running timer.
    SleepCancel,

    // Window
    WindowResized(Size),
    /// The size the compact bottom chrome was laid out at; only the height is
    /// read, as the extent a toast must clear.
    ///
    /// Published once per changed size, not once per frame.
    BottomChromeMeasured(Size),

    // Overflow menus; `menu_button::apply_event` is the one lifecycle reducer,
    // so an activation applies exactly once.
    /// Now-playing `Menu` overflow event.
    NowPlayingMenu(MenuButtonEvent),
    /// The title select: the destinations menu that replaced the sidebar.
    NavMenu(MenuButtonEvent),
    /// Queue `More` overflow event (the split button's chevron half).
    QueueMoreMenu(MenuButtonEvent),
    /// Titlebar `Sort: {current}` menu event; an activated row dispatches
    /// `SetSort(mode)`.
    SortMenu(MenuButtonEvent),

    // Right-click context menus on tracks, albums and queue entries, each
    // opened at a target and a viewport point.
    /// Open the native context-menu host at a viewport point.
    OpenContextMenu(domain::ContextTarget, iced::Point),
    OpenContextMenuFrom(
        domain::ContextTarget,
        iced::Point,
        iced::advanced::widget::Id,
    ),
    /// A right-click or Menu key on a podcast episode row at this viewport point.
    ///
    /// Episodes are keyed by `(feed_url, guid)`, which [`domain::ContextTarget`]
    /// cannot carry; a refresh that drops the episode leaves the menu inert.
    OpenPodcastEpisodeMenu(String, String, iced::Point),
    /// A context-menu Delete on a playlist row; opens a confirmation, since
    /// deletion cannot be undone.
    ConfirmDeletePlaylist(u64),
    /// Every event the playlist-delete confirmation reports, so one arm decides
    /// and dismisses.
    PlaylistDeleteConfirm(enzui_widgets::confirm_dialog::ConfirmDialogEvent),
    /// Replace the queue with a track set and start playing it (a collection's
    /// `Play` row).
    PlayTrackSet(Vec<u64>),
    /// Append a whole track set to the queue, as the album menu's
    /// `Add album to queue` does.
    AddTracksToQueue(Vec<u64>),
    /// Open track inspection for a live id, or dismiss it with `None`.
    ShowTrackInfo(Option<u64>),
    TrackDetailsPage(track_details::Page),
    TrackDetailsReload,
    TrackDetailsCancel,
    TrackDetailsPoll,
    SetTrackRating(std::path::PathBuf, usize),
    SetRatedFilter(bool),
    SetPlaybackRate(usize),
    /// The framework `enz_menu` moved its roving-focus cursor to this row.
    /// Every event the framework context-menu host reports.
    ///
    /// Pointer and Escape dismissals both arrive as `Dismissed(cause)`, so
    /// `CloseCause::restores_focus` decides focus restoration for both.
    ContextMenuEvent(enzui_widgets::context_menu_host::ContextMenuEvent),
    /// A context-menu row was activated, with the target it was built for and
    /// its action, both from the same `context_menu::items_for` arm.
    ContextAction(domain::ContextTarget, domain::ContextAction),
    /// A background state write finished. Carries both the ordering generation
    /// and the typed outcome, so a late or superseded reply cannot clear newer
    /// debt and an actual failure can become visible retry feedback.
    PersistenceFinished {
        generation: u64,
        outcome: persistence::PersistenceCompletion,
    },
    /// Retry the newest failed persistence snapshot without rebuilding it.
    RetryPersistence,
    /// Retire the current state file through quarantine, then acquire a
    /// fresh authority off-thread once custody settles.
    RetirePersistence,
    /// A reacquired publication authority delivered off-thread.
    AuthorityReacquired(Box<AuthorityRefresh>),
    /// Explicitly abandon retained save debt after a close was vetoed.
    DiscardUnsavedAndClose,

    /// One lifecycle event of the shared transient-feedback host, routed into
    /// `ToastHost::update`.
    Toast(enzui_widgets::toast_host::ToastEvent),
    /// Dismiss the standing Library folder-chooser failure notice.
    ///
    /// The notice reports a missing host capability, so it never times out; the
    /// next Add Folder attempt also clears it.
    DismissFolderFailure,
    /// Hide the standing notice with this id until what it reports changes.
    HideNotice(&'static str),
}
