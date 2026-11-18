//! The MusicApp state struct.

use super::*;

pub(crate) struct MusicApp {
    pub(crate) native_import_pending: bool,
    pub(crate) desktop_instance: Option<enzui_widgets::single_instance::SingleInstance>,
    pub(crate) desktop_paths: Vec<std::path::PathBuf>,
    pub(crate) desktop_open_pending: bool,
    pub(crate) native_import_receipts: Vec<(u64, enzui_widgets::file_drop::FileDrop)>,
    pub(crate) themes: ThemeIntegration,
    pub(crate) library: Library,
    pub(crate) album_projection: views::IdProvider,
    pub(crate) artist_projection: views::IdProvider,
    pub(crate) projection_key: std::cell::Cell<Option<(u64, LibraryFilter, SortMode)>>,
    pub(crate) album_scroll: enzui_widgets::scroll_view::ScrollTranslation,
    /// Must outlive a `view` call: the album grid detects a column-count change
    /// by comparing the stored count against the one it resolves this frame.
    pub(crate) album_anchor: std::cell::RefCell<Option<enzui_core::virtual_sequence::KeyedAnchor>>,
    pub(crate) album_geometry: std::cell::Cell<enzui_core::virtual_grid::ResolvedTileGrid>,
    pub(crate) album_anchor_columns: std::cell::Cell<usize>,
    pub(crate) artist_scroll: enzui_widgets::scroll_view::ScrollTranslation,
    pub(crate) playback: PlaybackState,
    /// The app-owned framework media reducer and single generation authority.
    ///
    /// Source swaps go through `AudioService::load_into(&mut self.media)`, which
    /// advances service and state together, so `MediaState::apply` drops a
    /// `PlaybackCompleted` stamped with an older generation. Present even without
    /// an audio backend so tests can drive completion events.
    pub(crate) media: enzui_services::media::MediaState,
    /// The framework audio service owner; `None` in tests and where GStreamer
    /// could not initialise, in which case transport commands reach no backend.
    pub(crate) audio: Option<audio::Playback>,
    /// The event receiver paired with [`Self::audio`]; `None` installs no wake
    /// source.
    pub(crate) audio_receiver: Option<enzui_services::media::AudioEventReceiver>,
    pub(crate) mpris: mpris_runtime::Runtime,
    pub(crate) mpris_source: Option<MprisSourceIdentity>,
    /// Autosave handle. Every test constructor leaves this inert; only the
    /// shipping constructor and persistence tests explicitly arm a real path.
    pub(crate) persist: persistence::Persist,
    /// Set only by reducer arms that mutate fields represented by
    /// `persistence::snapshot`. The outer update wrapper consumes it once,
    /// after nested reducer dispatch has finished.
    pub(crate) persistence_dirty: bool,
    /// Orders the off-reducer state writes. Cloned into every
    /// persistence task; the reducer only registers tickets.
    pub(crate) persistence_queue: persistence::PersistenceQueue,
    /// The newest document not yet confirmed durable. Errors and superseded
    /// completions retain it; the close path drains it synchronously.
    pub(crate) pending_save: Option<persistence::PendingSave>,
    /// The global persistence feedback rendered above every routed view.
    pub(crate) persistence_notice: Option<PersistenceNotice>,
    /// A close request vetoed by failed I/O resumes only after Retry succeeds
    /// or the listener explicitly discards the retained debt.
    pub(crate) close_after_save: bool,
    /// The process-wide enzui-center session, when opted in and enrolled.
    #[cfg(feature = "center")]
    pub(crate) center: Option<enzui_agent::center::CenterHost<crate::center::CenterCall>>,
    /// Latches once a permanently vetoed close (a fetch-pool worker/reaper panic
    /// or lost-custody poison) has been reported, so the per-poll close wake
    /// does not re-log it.
    pub(crate) fetch_close_veto_reported: bool,
    /// The persisted transport session awaiting boot-scan resolution.
    ///
    /// [`Self::finalize_pending_playback_session`] drains it once `library.tracks`
    /// is populated, so paths resolve to per-scan track ids.
    pub(crate) pending_playback_session: Option<persistence::PlaybackRec>,
    /// The persisted play stats awaiting boot-scan resolution, drained on the
    /// same edge as [`Self::finalize_pending_playback_session`] so each count lands
    /// on the file actually played.
    pub(crate) pending_track_stats: Option<Vec<persistence::TrackStatsRec>>,
    /// The configured roots whose initial bounded scan has not yet reached a
    /// generation-owned terminal. Seeded once when boot dispatches the initial
    /// scans through [`library_watch::LibraryWatcher`]; an accepted current
    /// [`Message::RootScanComplete`] for a root, or its removal, retires it. A
    /// busy owner refusal retains the entry so a later completion, not the
    /// refusal, finalizes. Draining the last entry finalizes the pending
    /// playback session and play stats exactly once.
    pub(crate) initial_scan_roots: std::collections::HashSet<std::path::PathBuf>,
    /// Guards the deferred boot-state settlement. Boot mints a
    /// generation for the off-thread state load; the [`Message::BootStateSettled`]
    /// arm applies a report only when its generation still matches, then
    /// advances this, so a stale or re-delivered completion is dropped rather
    /// than clobbering a session that already moved on.
    pub(crate) boot_load_generation: u64,
    /// True while a boot-state load worker is in flight. Bounds
    /// the off-thread loader to one live worker so repeated boot or a retry
    /// cannot multiply blocking threads; cleared when the matching settlement
    /// applies.
    pub(crate) boot_load_worker_spawned: bool,
    /// The bounded owner of the boot-state load and reacquire worker.
    /// Owning the worker set means a dropped app shuts it down and waits within
    /// a bounded budget rather than detaching a live thread; cancellation of a
    /// settlement future retains the running slot until the read actually exits.
    pub(crate) boot_load_owner: enzui_core::blocking_task::BlockingTaskOwner,
    /// Folders to scan when the deferred load carries no document (fresh
    /// install, recovered-from-corrupt, or a refused read-only file). Resolved
    /// on the responsive boot and consumed once by the settlement.
    pub(crate) fallback_folders: Vec<std::path::PathBuf>,
    pub(crate) current_view: View,
    /// The view before a modal side-trip (Search/Queue).
    pub(crate) view_before_modal: Option<View>,
    pub(crate) now_playing_open: bool,
    /// Framework-owned navigation-split state: the durable sidebar preference
    /// plus the overlay-drawer flag. The collapse decision itself is the
    /// framework's (`resolve_for_width`), not a hand-rolled width threshold.
    pub(crate) nav: NavSplitState,
    pub(crate) window_size: Size,
    pub(crate) selected_album_id: Option<u64>,
    pub(crate) filter: LibraryFilter,
    pub(crate) sort_mode: SortMode,
    pub(crate) search_query: String,
    /// Per-surface "Show all N tracks" state for lists capped at
    /// `domain::TRACK_LIST_PAGE` rows (Home library and Search tracks).
    pub(crate) track_list_expansion: domain::TrackListExpansion,
    /// Monotonic id used to auto-name new playlists.
    pub(crate) next_playlist_seq: u64,
    /// The add-to-playlist picker, when open. `None` means closed.
    pub(crate) playlist_picker: Option<PlaylistPicker>,
    /// The playlist Rename dialog, when open. `None` means closed.
    pub(crate) playlist_rename: Option<PlaylistRename>,
    /// Whether the keyboard-shortcuts overlay (Ctrl+/) is open.
    pub(crate) shortcuts_open: bool,
    /// Persisted equalizer policy for native audio processing.
    pub(crate) eq: EqState,
    /// Whether the equalizer dialog is open.
    pub(crate) eq_open: bool,
    /// Whether the equalizer's preset menu is open.
    pub(crate) eq_preset_menu_open: bool,
    /// ReplayGain policy edited in Settings › Playback.
    pub(crate) rg: ReplayGainState,
    /// Retains Settings while its adaptive host animates closed.
    pub(crate) rg_presentation: ModalPresentation<()>,
    /// The Settings page shown (`views::SETTINGS_*_PAGE`).
    pub(crate) settings_page: String,
    /// Whether a narrow Settings shows the page rather than the page list.
    pub(crate) settings_page_open: bool,
    /// Whether the Settings volume-normalization menu is open.
    pub(crate) rg_mode_menu_open: bool,
    /// Sleep-timer state.
    pub(crate) sleep_timer: SleepTimerState,
    pub(crate) sleep_pause_pending: bool,
    /// Whether the sleep-timer dialog is open.
    pub(crate) sleep_open: bool,
    /// When the armed sleep timer was last advanced, so a frame tick can turn
    /// wall-clock elapsed time into a `SleepTimerState::tick` delta. `None`
    /// whenever the timer is not counting.
    pub(crate) sleep_last_tick: Option<std::time::Instant>,
    /// The fade-out-at-end choice the sleep dialog will arm the next timer
    /// with. Kept separate from the armed state so the checkbox is meaningful
    /// before arming.
    pub(crate) sleep_fade_choice: bool,
    /// The preset the live timer was armed from, so the dialog's radio group
    /// can reflect the active choice. `Some(_)` exactly while a timer is armed.
    ///
    /// Retained because `SleepTimerState::Armed` carries only the decreasing
    /// `remaining_secs`.
    pub(crate) sleep_armed_preset: Option<SleepPreset>,

    // Controlled overflow menus: the app owns `MenuState` and `MenuModel` and
    // applies each `MenuButtonEvent` through `menu_button::apply_event`.
    /// Now-playing overflow menu state (open / highlighted row).
    pub(crate) now_playing_menu: MenuState,
    /// Now-playing overflow menu entries.
    pub(crate) now_playing_menu_model: MenuModel,
    /// The title select's destinations menu state.
    pub(crate) nav_menu: MenuState,
    /// The title select's entries, the current destination checked.
    pub(crate) nav_menu_model: MenuModel,
    /// Queue `More` overflow menu state.
    pub(crate) queue_more_menu: MenuState,
    /// Queue `More` overflow menu entries.
    pub(crate) queue_more_menu_model: MenuModel,
    /// Titlebar Sort dropdown open/highlight state.
    pub(crate) sort_menu: MenuState,
    /// Titlebar Sort menu entries: one radio row per `SortMode::ALL` entry,
    /// plus the Songs-only `Rated tracks only` check row.
    pub(crate) sort_menu_model: MenuModel,
    /// Whether the titlebar's `More actions` overflow menu is open.
    pub(crate) titlebar_overflow_open: bool,

    // Podcasts surface state. Feed fetch runs on `PodcastService`'s bounded
    // blocking workers through an injected transport.
    pub(crate) subscribe_input: String,
    pub(crate) subscriptions: Vec<podcasts::PodcastFeed>,
    /// The bounded audited transport for the Radio Browser catalog fetch, with
    /// the same worker, transport and URL policy shape as `podcast_service`.
    pub(crate) radio_catalog_service: radio_catalog::RadioCatalogService,
    /// Monotonic top-list generation. Bumped by
    /// `dispatch_catalog_top_list` before submit; the completion arm
    /// discards a reply for an older generation. Search has its own
    /// generation on `RadioState::search_generation`.
    pub(crate) radio_top_list_generation: u64,
    /// Favourites recovered from disk, applied on the first successful
    /// `RadioCatalogLoaded` once their catalog rows exist.
    /// A fetch of starred stations' records is in flight.
    pub(crate) radio_favorites_pending: bool,
    /// Exactly two blocking workers own all feed transport I/O.
    pub(crate) podcast_service: podcasts::PodcastService,
    /// Owns generations and cancellation signals; its Drop cancels shutdown.
    pub(crate) podcast_requests: podcasts::PodcastRequests,
    /// The keyless discovery (iTunes Search) transport owner, sharing the audited
    /// transport and URL policy of `podcast_service`.
    pub(crate) podcast_search: podcasts::search::PodcastSearchService,
    /// The discovery query text and its generation; replies for an older
    /// generation are discarded.
    pub(crate) podcast_search_query: String,
    pub(crate) podcast_search_generation: u64,
    /// The discovery surface state: idle, in flight, results, or a typed error.
    pub(crate) podcast_search_state: PodcastSearchState,
    /// Per-episode resume points keyed on the durable `(feed_url, guid)` pair,
    /// saved from the confirmed playhead and applied on the next select. Ordered
    /// so `persistence` writes a stable on-disk order.
    pub(crate) episode_resume: std::collections::BTreeMap<(String, String), f64>,
    /// The bounded remote-artwork transport owner. A failed fetch is silent and
    /// the view draws the themed placeholder.
    pub(crate) podcast_artwork: podcasts::artwork::PodcastArtworkService,
    pub(crate) podcast_art_cache: podcasts::artwork::PodcastArtworkCache,

    /// The Radio surface's state: mode, catalog, query and favourites.
    pub(crate) radio: radio::RadioState,
    pub(crate) spotify: spotify::State,

    /// The Manage Feeds dialog, opened from the Podcasts header, holding OPML
    /// import and export.
    ///
    /// OPML moves through text areas rather than a file picker because the portal
    /// chooser only yields a path and the app has no file-byte I/O seam.
    pub(crate) manage_feeds_open: bool,
    /// The paste-to-import OPML buffer (the editor owns caret/undo; a
    /// `text_editor::Content`, mirrored to text on submit).
    pub(crate) opml_import: text_editor::Content,
    /// The exported OPML, shown read-only for copy; empty until Export is pressed.
    pub(crate) opml_export: String,
    /// The OPML file import and export dialogs.
    pub(crate) opml_transfer: crate::opml_transfer::OpmlTransfer,
    /// Apple's top podcasts on the Podcasts page.
    pub(crate) podcast_charts: crate::PodcastCharts,

    /// The XDG portal directory chooser behind Add Folder; its backend is built
    /// on the first pick.
    pub(crate) folder_chooser: picker::Picker,
    /// The one transient live-feedback authority for this app.
    ///
    /// Library acknowledgements and refusals are posted only through
    /// [`MusicApp::post_folder_status`]; persistent portal failures go to
    /// [`MusicApp::folder_failure`] instead.
    pub(crate) toasts: enzui_widgets::toast_host::ToastHost,
    /// The earliest-deadline one-shot wake for `toasts`; an idle app installs no
    /// toast timer.
    pub(crate) toast_expiry: enzui_widgets::toast_host::ToastExpirySchedule,
    // Logical pixels, 0 until a frame has measured the chrome.
    pub(crate) bottom_chrome_height: f32,
    /// Decoded waveforms by track id; `None` for a track with none.
    pub(crate) waveforms: std::collections::HashMap<u64, Option<std::sync::Arc<[f32]>>>,
    /// The track whose waveform is decoding.
    pub(crate) waveform_pending: Option<u64>,
    /// Whether a pointer drag on the seek bar is live.
    pub(crate) seek_dragging: bool,
    /// The position a live seek bar drag shows, sought once on release.
    pub(crate) seek_preview: Option<f64>,
    /// After a drag's seek, until when the preview holds while playback
    /// catches up with it.
    pub(crate) seek_settle_until: Option<std::time::Instant>,
    /// The persistent folder-chooser failure, if any.
    ///
    /// Portal failures are standing host facts, so they show as a dismissible
    /// standing notice rather than an expiring toast. Cleared on dismissal or the
    /// next Add Folder attempt.
    pub(crate) folder_failure: Option<String>,
    /// Standing notices the user closed, by id and what they said; each stays
    /// hidden until what it reports changes (see `MusicApp::service_notices`).
    pub(crate) hidden_notices: Vec<(&'static str, String)>,

    /// The right-click context menu currently up; `None` means closed and
    /// `enz_positioned_overlay` reads this same value.
    pub(crate) context_menu: Option<context_menu::OpenContextMenu>,
    /// A strictly increasing generation minted per podcast-episode menu open and
    /// carried by `ContextTarget::PodcastEpisode`. On exhaustion the menu refuses
    /// to open rather than reuse a token.
    pub(crate) context_menu_generation: u64,
    /// The playlist a Delete confirmation is open on. Only the persisted name and
    /// ordering are deleted, never the tracks.
    pub(crate) playlist_delete: Option<u64>,
    /// Controlled state for the playlist-delete `enz_confirm_dialog`.
    pub(crate) playlist_delete_confirm: enzui_core::confirm::ConfirmState,
    pub(crate) track_info_id: Option<u64>,
    /// The podcast episode whose details are open, by `(feed_url, guid)`.
    pub(crate) episode_details: Option<(String, String)>,
    pub(crate) track_details: Option<track_details::Presentation>,
    pub(crate) track_details_service: track_details::Service,
    pub(crate) track_details_generation: u64,
    // Last confirmed user policy survives the framework rate reset on source replacement.
    pub(crate) rate_policy: f64,
    pub(crate) rate_requested_generation: Option<u64>,
    /// The source whose audio pipeline exists (it reported `SourceReady`).
    /// Volume, mute and effects reach a pipeline only then; before, the
    /// backend refuses them ("no live audio pipeline") while a stream
    /// connects, and the listener's choice is applied once it's ready.
    pub(crate) audio_live_generation: Option<u64>,
    /// The last volume asked for and not yet confirmed, so confirmations of
    /// earlier values (a slider drag sends many) don't pull the slider back.
    pub(crate) volume_pending: Option<f32>,

    /// One lifecycle slot per retained/nested presentation.
    pub(crate) modal_focus: modal_focus::ModalFocusSet,

    /// Watches every configured library folder and routes filesystem changes
    /// through one bounded, generation-checked scan/reconcile path so new,
    /// removed, and renamed music appears without a manual rescan.
    pub(crate) library_watch: library_watch::LibraryWatcher,

    /// The single monotonic ordering source for track-availability decisions.
    ///
    /// Scan reconciles and backend outcomes each draw a fresh value, so a backend
    /// success cannot overwrite a newer scan `Missing`. Advances by checked add and
    /// never reuses generation 0.
    pub(crate) availability_generation: u64,
    /// True once [`Self::availability_generation`] is exhausted: no further
    /// availability decision is admitted and local playback is refused until
    /// restart.
    pub(crate) availability_exhausted: bool,
    /// A deferred load of the restored current track, held until its owning-root
    /// scan proves it `Present`. `None` when there is nothing to
    /// restore or the intent already fired/retired.
    pub(crate) restore_intent: Option<RestoreIntent>,
    /// The latest restore-session identity token. Advanced by checked
    /// add for each armed intent and never wrapped or reused, so a reconcile
    /// resolving an intent can reject one belonging to a superseded session.
    pub(crate) restore_session: u64,
    /// The source-open admitted to the audio backend: media generation, durable
    /// track path/id and availability generation. A later `SourceReady`/`Error`
    /// applies only while the current track still has this identity.
    pub(crate) pending_source_open: Option<PendingSourceOpen>,

    /// The portal-mediated playlist Import/Export owner: two host
    /// dialogs and one bounded worker (one thread, queue two).
    pub(crate) playlist_transfer: playlist_transfer::PlaylistTransfer,
    /// The accepted import currently being read/parsed, stamped with its request
    /// generation. A parse settlement whose generation does not match this is
    /// stale and inert; a Busy refusal flips its state to await one coalesced
    /// retry. `None` when no import is in flight.
    pub(crate) import_job: Option<ImportJob>,
    /// Owned export snapshots keyed by the exact request generation: the
    /// rows and display name captured before `SaveFile`, and the portal grant
    /// once it completes. A job holding a grant carries publication debt that
    /// vetoes an exact close until it is published, retried, or discarded.
    pub(crate) export_jobs: std::collections::HashMap<u64, ExportJob>,
}
