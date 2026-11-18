//! Theme, subscription, and window wiring.

use super::*;

impl MusicApp {
    pub(crate) fn theme(&self) -> Theme {
        self.themes.theme().iced_theme()
    }

    pub(crate) fn subscription(&self) -> Subscription<Message> {
        // `iced::window::events()` never sees keyboard events, so window size and
        // Tab traversal share `listen_with`; `classify_event` keeps the decode
        // testable.
        let events = iced::event::listen_with(|event, status, _id| classify_event(&event, status));

        // Every subscription below exists only while active. The folder chooser
        // wakes on portal completion, and the sleep timer arms its own clock.
        let mut subscriptions = vec![events];
        // The OPML file dialogs wake on completion.
        subscriptions.push(
            self.opml_transfer
                .completion_subscription()
                .map(|_| Message::OpmlTransferWake),
        );
        // Spotify's player is asked what it's doing, and its position moves.
        if let Some(every) = self.spotify.tick_interval() {
            subscriptions.push(
                iced::time::every(every).map(|_| Message::Spotify(crate::spotify::Event::Tick)),
            );
        }
        if let Some(instance) = &self.desktop_instance {
            subscriptions.push(instance.subscription(|_| Message::DesktopWake));
        }
        #[cfg(feature = "center")]
        subscriptions.push(self.center_subscription());
        if self.track_details_service.needs_poll() {
            subscriptions
                .push(iced::time::every(PODCAST_POLL_INTERVAL).map(|_| Message::TrackDetailsPoll));
        }
        let portal_wake = self.folder_chooser.completion_subscription();
        subscriptions.push(portal_wake.map(|_| Message::PortalTick));
        // Playlist Import/Export portals wake on completion, with no clock.
        subscriptions.push(
            self.playlist_transfer
                .completion_subscription()
                .map(|_| Message::PlaylistTransferTick),
        );
        // One bounded, coalesced retry wake while a Busy-refused import or
        // export intent is retained; it disappears the moment none awaits.
        if self.has_awaiting_retry() {
            subscriptions.push(
                iced::time::every(PLAYLIST_RETRY_INTERVAL).map(|_| Message::PlaylistTransferRetry),
            );
        }
        // The service owns a cancelled job until its result drains; keep waking
        // so stale rows release their bounded admission slots.
        if self.podcast_service.pending_count() > 0
            || !self.podcast_service.close_debt().is_settled()
        {
            subscriptions
                .push(iced::time::every(PODCAST_POLL_INTERVAL).map(|_| Message::PodcastPoll));
        }
        // The wake stays until nothing is pending and the pool has retired, so a
        // close in flight keeps it alive until the reaper joins.
        if self.radio_catalog_service.pending_count() > 0
            || !self.radio_catalog_service.close_debt().is_settled()
        {
            subscriptions
                .push(iced::time::every(PODCAST_POLL_INTERVAL).map(|_| Message::RadioCatalogPoll));
        }
        if self.podcast_search.pending_count() > 0 || !self.podcast_search.close_debt().is_settled()
        {
            subscriptions
                .push(iced::time::every(PODCAST_POLL_INTERVAL).map(|_| Message::PodcastSearchPoll));
        }
        if self.podcast_artwork.pending_count() > 0
            || !self.podcast_artwork.close_debt().is_settled()
        {
            subscriptions.push(
                iced::time::every(PODCAST_POLL_INTERVAL).map(|_| Message::PodcastArtworkPoll),
            );
        }
        if matches!(self.sleep_timer, SleepTimerState::Armed { .. }) {
            subscriptions.push(iced::time::every(SLEEP_POLL_INTERVAL).map(|_| Message::SleepTick));
        }
        subscriptions.push(
            self.themes
                .subscription()
                .map(|event| Message::Theme(Box::new(event))),
        );
        // One deadline-keyed wake while a toast can expire, none otherwise.
        if let Some((deadline, wait)) = self.toast_expiry.armed() {
            subscriptions.push(
                iced::time::every(wait)
                    .with(deadline)
                    .map(|(_deadline, now)| {
                        Message::Toast(enzui_widgets::toast_host::ToastEvent::Tick(now))
                    }),
            );
        }
        // Always subscribed so an AccessKit request against an idle loop still
        // wakes an `update`; inert (no streams) on a build without accesskit.
        subscriptions.push(enzui_core::app_runtime::accessibility_wake_subscription(
            Message::A11yWake,
        ));
        // Wakes on accepted audio events only; absent when audio failed to spawn.
        if let Some(receiver) = self.audio_receiver.as_ref() {
            subscriptions
                .push(audio::Playback::subscription(receiver).map(Message::AudioServiceWake));
        }
        if let Some(mpris) = self.mpris.call_subscription() {
            subscriptions.push(mpris.map(Message::MprisCallWake));
        }
        if self.mpris.needs_lifecycle_wake() {
            subscriptions.push(
                iced::time::every(PODCAST_POLL_INTERVAL).map(|_| Message::MprisLifecycleWake),
            );
        }
        // Event-driven directory-change deliveries; idle watches never wake the
        // runtime. Coalesced bursts arrive as one batch per drain.
        subscriptions.push(
            self.library_watch
                .subscription()
                .map(Message::WatchDeliveries),
        );
        // The watcher's own low-rate wake, armed only while a debounce or
        // busy-retry deadline is pending. A settled library installs no ticker.
        if self.library_watch.wants_tick() {
            subscriptions
                .push(iced::time::every(library_watch::TICK).map(|_| Message::LibraryWatchTick));
        }
        Subscription::batch(subscriptions)
    }
}

// Overflow-menu models and dispatch.
//
// Each menu entry dispatches the same message as its in-page affordance, so
// the menus never become a second implementation.

/// Entry ids for the now-playing `Menu` overflow, shared by model and
/// dispatcher.
pub const NP_MENU_ADD_TO_PLAYLIST: &str = "np.add-to-playlist";
pub const NP_MENU_OPEN_QUEUE: &str = "np.open-queue";
pub const NP_MENU_OPEN_EQ: &str = "np.open-eq";
pub const NP_MENU_OPEN_SLEEP: &str = "np.open-sleep";
/// Settings (Library and Playback pages), in the main menu.
pub const NP_MENU_OPEN_RG: &str = "np.open-rg";

/// Entry ids for the queue `More` overflow.
pub const QM_MENU_SAVE_AS_PLAYLIST: &str = "qm.save-as-playlist";
pub const QM_MENU_CLEAR_QUEUE: &str = "qm.clear-queue";

/// Sort menu row ids, one per `SortMode::ALL` entry, derived by
/// [`sort_menu_entry_id`].
pub const SORT_MENU_ID_PREFIX: &str = "sort.";

/// The stable menu-row id for a sort mode, derived from `SortMode::label()`.
pub fn sort_menu_entry_id(mode: crate::domain::SortMode) -> String {
    format!(
        "{}{}",
        SORT_MENU_ID_PREFIX,
        mode.label().to_ascii_lowercase()
    )
}

/// Inverse of [`sort_menu_entry_id`]; `None` for an unknown id, so a stale row
/// is a no-op, like a `_ => {}` arm.
pub fn sort_mode_from_entry_id(id: &str) -> Option<crate::domain::SortMode> {
    use crate::domain::SortMode;
    SortMode::ALL
        .iter()
        .copied()
        .find(|m| sort_menu_entry_id(*m) == id)
}

/// The now-playing `Menu` overflow model.
/// Prefix of the title select's rows; the suffix indexes `View::nav_views`.
const NAV_MENU_PREFIX: &str = "nav.";

/// The title select's menu: every destination the sidebar listed, Library
/// then Discover, the current one checked.
pub fn nav_menu_model(current: View) -> MenuModel {
    use enzui_core::menu_model::CheckState;
    let mut entries = Vec::new();
    for (index, view) in View::nav_views().iter().enumerate() {
        if *view == View::Radio {
            entries.push(MenuEntryModel::separator("nav.discover-rule"));
        }
        entries.push(
            MenuEntryModel::command(format!("{NAV_MENU_PREFIX}{index}"), view.label())
                .icon(view.icon_name())
                .radio(true)
                .check(if *view == current {
                    CheckState::Checked
                } else {
                    CheckState::Unchecked
                }),
        );
    }
    MenuModel::new(entries)
}

/// The destination a title select row opens.
pub(crate) fn nav_menu_view(id: &str) -> Option<View> {
    let index: usize = id.strip_prefix(NAV_MENU_PREFIX)?.parse().ok()?;
    View::nav_views().get(index).copied()
}

/// The kebab's Playback speed submenu row.
pub const NP_MENU_SPEED: &str = "np.speed";
/// Prefix of the speed submenu's rows; the suffix indexes `RATES`.
const NP_MENU_SPEED_PREFIX: &str = "np.speed.";

/// The now-playing kebab menu, with `rate` checked in the speed submenu.
pub fn now_playing_menu_model(rate: f64) -> MenuModel {
    use enzui_core::menu_model::CheckState;
    let speeds = MenuModel::new(
        crate::track_experience::RATE_LABELS
            .iter()
            .zip(crate::track_experience::RATES)
            .enumerate()
            .map(|(index, (label, candidate))| {
                MenuEntryModel::command(format!("{NP_MENU_SPEED_PREFIX}{index}"), *label)
                    .radio(true)
                    .check(if (*candidate - rate).abs() < f64::EPSILON {
                        CheckState::Checked
                    } else {
                        CheckState::Unchecked
                    })
            })
            .collect(),
    );
    MenuModel::new(vec![
        MenuEntryModel::command(NP_MENU_ADD_TO_PLAYLIST, "Add to playlist").icon("list-plus"),
        MenuEntryModel::command(NP_MENU_OPEN_QUEUE, "Open Up Next").icon("list-ordered"),
        MenuEntryModel::separator("np.rule"),
        MenuEntryModel::submenu(NP_MENU_SPEED, "Playback speed").icon("gauge"),
        MenuEntryModel::command(NP_MENU_OPEN_EQ, "Equalizer").icon("sliders-horizontal"),
        MenuEntryModel::command(NP_MENU_OPEN_SLEEP, "Sleep timer").icon("timer"),
        MenuEntryModel::separator("np.help-rule"),
        // The only route to Settings (Library and Playback pages).
        MenuEntryModel::command(NP_MENU_OPEN_RG, crate::views::RG_TITLE).icon("settings"),
        MenuEntryModel::command("np.shortcuts", "Keyboard shortcuts").icon("keyboard"),
    ])
    .with_submenu(NP_MENU_SPEED, speeds)
}

/// The titlebar Sort menu's base model, one `Sort by {X}` row per
/// `SortMode::ALL` entry. [`sort_menu_entries`] adds the live check state and
/// the Songs-only filter row each time the menu opens.
pub fn sort_menu_model() -> MenuModel {
    use crate::domain::SortMode;
    MenuModel::new(
        SortMode::ALL
            .iter()
            .map(|m| MenuEntryModel::command(sort_menu_entry_id(*m), m.menu_label()))
            .collect(),
    )
}

/// The Songs-only `Rated tracks only` filter row id in the Sort menu.
pub const SORT_MENU_RATED_ID: &str = "filter.rated";
/// The Songs-only filter row's label.
pub const SORT_MENU_RATED_LABEL: &str = "Rated tracks only";

/// The Sort menu rows for `view`: the applicable sort modes as radio rows
/// with the current one checked and, on Songs, a separator and the
/// `Rated tracks only` check row (the former `Rated` checkbox).
///
/// `Rating` ranks tracks, so it is offered only where tracks are listed.
pub fn sort_menu_entries(
    view: crate::domain::View,
    current: crate::domain::SortMode,
    filter: crate::domain::LibraryFilter,
) -> Vec<MenuEntryModel> {
    use crate::domain::{LibraryFilter, SortMode, View};
    use enzui_core::menu_model::CheckState;
    let songs = view == View::Home;
    let mut entries: Vec<MenuEntryModel> = SortMode::ALL
        .iter()
        .filter(|m| songs || **m != SortMode::Rating)
        .map(|m| {
            MenuEntryModel::command(sort_menu_entry_id(*m), m.menu_label())
                .radio(true)
                .check(if *m == current {
                    CheckState::Checked
                } else {
                    CheckState::Unchecked
                })
        })
        .collect();
    if songs {
        entries.push(MenuEntryModel::separator("sort.filter-rule"));
        entries.push(
            MenuEntryModel::command(SORT_MENU_RATED_ID, SORT_MENU_RATED_LABEL).check(
                if filter == LibraryFilter::Rated {
                    CheckState::Checked
                } else {
                    CheckState::Unchecked
                },
            ),
        );
    }
    entries
}

/// The queue `More` overflow model. It repeats the split button's primary
/// action (Save queue as playlist) and adds Clear queue.
pub fn queue_more_menu_model() -> MenuModel {
    MenuModel::new(vec![
        MenuEntryModel::command(QM_MENU_SAVE_AS_PLAYLIST, "Save queue as playlist"),
        MenuEntryModel::command(QM_MENU_CLEAR_QUEUE, "Clear queue").destructive(true),
    ])
}

/// Map a now-playing menu row id to the message its affordance fires.
pub(crate) fn dispatch_now_playing_menu(app: &mut MusicApp, id: EntryId) -> Task<Message> {
    let message = match id.as_str() {
        NP_MENU_ADD_TO_PLAYLIST if app.playback.current_track_id.is_some() => {
            Some(Message::OpenPlaylistPicker(PickerTarget::CurrentTrack))
        }
        NP_MENU_OPEN_QUEUE => {
            if app.view_before_modal.is_none() {
                app.view_before_modal = Some(app.current_view);
            }
            app.current_view = View::Queue;
            None
        }
        NP_MENU_OPEN_EQ if !app.eq_open => Some(Message::ToggleEqualizer),
        NP_MENU_OPEN_RG if !app.rg_presentation.is_open() => {
            Some(Message::TogglePlaybackPreferences)
        }
        NP_MENU_OPEN_SLEEP if !app.sleep_open => Some(Message::ToggleSleepTimer),
        "np.shortcuts" if !app.shortcuts_open => Some(Message::ToggleShortcuts),
        other => other
            .strip_prefix(NP_MENU_SPEED_PREFIX)
            .and_then(|index| index.parse().ok())
            .map(Message::SetPlaybackRate),
    };
    message.map_or_else(Task::none, |message| app.update_inner(message))
}

/// Map an activated context-menu row to the message that performs it.
///
/// Every arm returns the same message as the equivalent in-page control.
/// `None` means the target vanished between the right-click and activation,
/// for example a track removed by a rescan.
pub(crate) fn context_menu_action_message(
    app: &MusicApp,
    target: domain::ContextTarget,
    action: domain::ContextAction,
) -> Option<Message> {
    use domain::ContextAction as A;
    use domain::ContextTarget as T;

    // The track a target resolves to, for the track-scoped rows.
    let track_id = |t: T| -> Option<u64> {
        match t {
            T::Track(id) => Some(id),
            T::QueueEntry(entry) => app.playback.track_of_entry(entry),
            _ => None,
        }
    };
    // Every track a container target holds, in its own order.
    let set = |t: T| -> Vec<u64> {
        match t {
            T::Album(id) => app
                .library
                .albums
                .iter()
                .find(|a| a.id == id)
                .map(|a| a.track_ids.clone())
                .unwrap_or_default(),
            T::Artist(id) => app
                .library
                .artists
                .iter()
                .find(|a| a.id == id)
                .map(|a| a.track_ids.clone())
                .unwrap_or_default(),
            T::Genre(id) => app
                .library
                .genres
                .iter()
                .find(|g| g.id == id)
                .map(|g| g.track_ids.clone())
                .unwrap_or_default(),
            T::PlaylistRow(id) => app
                .library
                .playlists
                .iter()
                .find(|p| p.id == id)
                .map(|p| p.track_ids.clone())
                .unwrap_or_default(),
            T::Track(id) => vec![id],
            T::QueueEntry(entry) => app.playback.track_of_entry(entry).into_iter().collect(),
            // Stations and episodes play through their own domain, not the queue.
            T::RadioStation(_) | T::PodcastEpisode(_) => Vec::new(),
        }
    };

    match (target, action) {
        // --- Play ---
        (T::Track(id), A::Play) => Some(Message::PlayTrack(id)),
        // Playing a queue entry is a jump; a departed occurrence yields `None`.
        (T::QueueEntry(entry), A::Play) => app
            .playback
            .index_of_entry(entry)
            .map(Message::JumpToQueueEntry),
        // Radio Play is `RadioTune` and must precede the container catch-all.
        // Episode Play is handled in the `ContextMenuEvent` arm.
        (T::RadioStation(id), A::Play) => Some(Message::RadioTune(id)),
        (t, A::Play) => Some(Message::PlayTrackSet(set(t))),

        // --- Radio favourite ---
        (T::RadioStation(id), A::ToggleFavorite) => Some(Message::RadioToggleFavorite(id)),

        // --- Playlist rename / delete ---
        (T::PlaylistRow(id), A::RenamePlaylist) => Some(Message::RenamePlaylistOpen(id)),
        (T::PlaylistRow(id), A::DeletePlaylist) => Some(Message::ConfirmDeletePlaylist(id)),

        // --- Queue ---
        (T::Track(id), A::PlayNext) => Some(Message::PlayTrackNext(id)),
        (T::Track(id), A::AddToQueue) => Some(Message::AddTrackToQueue(id)),
        (t, A::AddToQueue) => Some(Message::AddTracksToQueue(set(t))),
        // Inert once the occurrence has departed, so a stale menu never acts on
        // whatever slid into its old index.
        (T::QueueEntry(entry), A::MoveToTopOfQueue) => app
            .playback
            .index_of_entry(entry)
            .map(Message::MoveToTopOfQueue),
        (T::QueueEntry(entry), A::RemoveFromQueue) => app
            .playback
            .index_of_entry(entry)
            .map(Message::RemoveFromQueue),

        // --- Playlist picker ---
        (T::Track(id), A::AddToPlaylist) => {
            Some(Message::OpenPlaylistPicker(domain::PickerTarget::Track(id)))
        }
        (T::Album(id), A::AddToPlaylist) => {
            Some(Message::OpenPlaylistPicker(domain::PickerTarget::Album(id)))
        }

        // --- Navigation ---
        (T::Track(id), A::GoToAlbum) => app
            .library
            .albums
            .iter()
            .find(|a| a.track_ids.contains(&id))
            .map(|a| Message::SelectAlbum(a.id)),
        (T::Track(id), A::GoToArtist) => app
            .library
            .artists
            .iter()
            .find(|a| a.track_ids.contains(&id))
            .map(|a| Message::SelectArtist(a.id)),

        // --- Info ---
        (t, A::TrackInfo) => track_id(t).map(|id| Message::ShowTrackInfo(Some(id))),

        // --- Availability recovery ---
        (t, A::Rescan) => track_id(t).map(Message::RescanTrack),
        (t, A::Remove) => track_id(t).map(Message::RemoveTrack),

        // `context_menu::items_for` never builds these; debug builds assert.
        (t, a) => {
            debug_assert!(false, "context row {a:?} is not wired for target {t:?}");
            None
        }
    }
}

// Bound a rename draft to `MAX_PLAYLIST_NAME_LEN + 1` scalar values so an
// over-long paste stays `TooLong` instead of truncating to a valid name.
pub(crate) fn clamp_playlist_rename_draft(raw: &str) -> String {
    raw.chars().take(MAX_PLAYLIST_NAME_LEN + 1).collect()
}

/// Map a queue `More` menu row id to the message its affordance fires.
pub(crate) fn dispatch_queue_more_menu(app: &mut MusicApp, id: EntryId) -> Task<Message> {
    match id.as_str() {
        QM_MENU_SAVE_AS_PLAYLIST => app.update_inner(Message::SaveQueueAsPlaylist),
        QM_MENU_CLEAR_QUEUE => app.update_inner(Message::ClearQueue),
        _ => Task::none(),
    }
}

/// Map a [`shortcuts::ShortcutAction`] to the [`Message`] that performs it.
///
/// The only bridge from the shortcuts table to the reducer; the match is
/// exhaustive.
pub(crate) fn shortcut_message(action: shortcuts::ShortcutAction) -> Message {
    use shortcuts::ShortcutAction::*;
    match action {
        PlayPause => Message::TogglePlayPause,
        NextTrack => Message::NextTrack,
        PreviousTrack => Message::PreviousTrack,
        ToggleShuffle => Message::ToggleShuffle,
        CycleRepeat => Message::CycleRepeat,
        VolumeUp => Message::AdjustVolume(0.05),
        VolumeDown => Message::AdjustVolume(-0.05),
        ToggleMute => Message::ToggleMute,
        OpenSearch => Message::SelectView(View::Search),
        ToggleQueue => Message::SelectView(View::Queue),
        ToggleEqualizer => Message::ToggleEqualizer,
        ToggleSleepTimer => Message::ToggleSleepTimer,
        ToggleShortcuts => Message::ToggleShortcuts,
        Dismiss => Message::Dismiss,
    }
}

/// Classify a raw window or keyboard event into an app message, or `None`.
///
/// Tab direction is decided here because `Task`'s `Debug` cannot distinguish
/// `focus_next()` from `focus_previous()` in reducer tests. `Opened` and other
/// window events map to `Size::ZERO` (remeasure); `Resized` carries its size.
pub(crate) fn classify_event(event: &iced::Event, status: iced::event::Status) -> Option<Message> {
    if let Some(traversal) = enzui_core::focus::traversal_from_event(event, status) {
        return Some(match traversal {
            Traversal::Next => Message::FocusNext,
            Traversal::Previous => Message::FocusPrevious,
        });
    }

    match event {
        iced::Event::Window(iced::window::Event::Opened { .. }) => {
            Some(Message::WindowResized(Size::ZERO))
        }
        iced::Event::Window(iced::window::Event::Resized(size)) => {
            Some(Message::WindowResized(*size))
        }
        iced::Event::Keyboard(kb_event @ iced::keyboard::Event::KeyPressed { .. }) => {
            // Shortcuts consumed by a focused widget are not global, except
            // Escape. Core's matcher ignores key-repeat events.
            let action = shortcuts::match_event(kb_event)?;
            if status == iced::event::Status::Captured
                && action != shortcuts::ShortcutAction::Dismiss
            {
                return None;
            }
            Some(shortcut_message(action))
        }
        _ => None,
    }
}
