//! Reducer batch 1: A11yWake through BootStateSettled.

use super::*;

impl MusicApp {
    pub(crate) fn reduce_batch1(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::DesktopWake => {
                let (intents, raise) = self
                    .desktop_instance
                    .as_mut()
                    .map(|instance| instance.drain::<Message>(None))
                    .unwrap_or_else(|| (Vec::new(), Task::none()));
                let open = self.desktop_intents(intents);
                return Task::batch([raise, open]);
            }
            Message::DesktopFilesOpened(result) => {
                self.desktop_open_pending = false;
                match result {
                    Ok(tracks) => {
                        let paths: Vec<_> = tracks.iter().map(|track| track.path.clone()).collect();
                        self.absorb_scanned_tracks(tracks);
                        let ids = paths
                            .iter()
                            .filter_map(|path| self.library.track_id_for_path(path))
                            .collect();
                        let play = self.update(Message::PlayTrackSet(ids));
                        let next = self.pump_desktop_paths();
                        return Task::batch([play, next]);
                    }
                    Err(error) => self.post_folder_status(&error, crate::ToastTone::Warning),
                }
                return self.pump_desktop_paths();
            }
            Message::A11yWake(receipt) => {
                return enzui_widgets::a11y_actions::apply_accessibility_wake::<Message>(receipt);
            }
            Message::AudioServiceWake(_wake) => {
                return self.drain_audio_events();
            }
            Message::MprisCallWake(wake) => {
                if !wake.state.is_live() {
                    self.mpris.transport_ended(wake.state);
                    return Task::none();
                }
                return self.drain_mpris_calls();
            }
            Message::MprisConnectSettled(_) | Message::MprisLifecycleWake => {}
            Message::Theme(event) => {
                let before = self.themes.theme_generation();
                let task = self.themes.update(*event);
                debug_assert!(self.themes.theme_generation() >= before);
                return task.map(|event| Message::Theme(Box::new(event)));
            }
            Message::SelectView(view) => {
                self.nav.dismiss_overlay();
                self.now_playing_open = false;
                // Search and Queue are modal side-trips.
                if matches!(view, View::Search | View::Queue) {
                    if self.view_before_modal.is_none() {
                        self.view_before_modal = Some(self.current_view);
                    }
                } else {
                    self.view_before_modal = None;
                    // Changing the nav destination resets the filter.
                    self.filter = LibraryFilter::None;
                }
                self.current_view = view;
                if view == View::Podcasts {
                    self.podcast_charts.open = None;
                    return self.load_podcast_chart();
                }
                if view == View::Radio {
                    self.dispatch_catalog_top_list();
                }
                if view == View::Spotify {
                    return self
                        .spotify
                        .update(crate::spotify::Event::Shown)
                        .map(Message::Spotify);
                }
                // Focus comes from `.focus_on_mount(true)` on the search field in
                // `views::search_input`.
            }
            Message::ExitModal => {
                if let Some(prev) = self.view_before_modal.take() {
                    self.current_view = prev;
                    self.search_query.clear();
                }
            }
            Message::FocusNext => return operate(traverse::<Message>(Traversal::Next)),
            Message::FocusPrevious => return operate(traverse::<Message>(Traversal::Previous)),
            Message::ModalHost(id, event) => return self.reduce_modal_host(id, event),
            Message::ToggleShortcuts => {
                self.shortcuts_open = !self.shortcuts_open;
                return self.toggle_modal_focus(
                    self.shortcuts_open,
                    ModalPresentationId::Shortcuts,
                    None,
                );
            }
            Message::Dismiss => {
                // Escape closes only the topmost dismissable surface. Modals
                // close through the owned lifecycle (precedence in
                // `close_topmost_modal`, mirroring the view's overlay chain) so
                // focus returns to the invoker. The context menu is mounted under
                // that chain and claims Escape only while presenting.
                if let Some(task) =
                    self.close_topmost_modal(enzui_core::modal::DismissCause::Escape)
                {
                    return task;
                }
                if self.now_playing_open {
                    self.now_playing_open = false;
                } else if let Some(prev) = self.view_before_modal.take() {
                    self.current_view = prev;
                    self.search_query.clear();
                }
            }
            Message::WaveformReady(track, peaks) => {
                if self.waveform_pending == Some(track) {
                    self.waveform_pending = None;
                }
                // A small cache: the panel only ever shows the current track.
                if self.waveforms.len() >= crate::update::WAVEFORM_CACHE {
                    self.waveforms.clear();
                }
                self.waveforms.insert(track, peaks);
            }
            Message::ToggleNowPlaying => {
                self.now_playing_open = !self.now_playing_open;
            }
            Message::NowPlayingSheet(event) => match event {
                SheetEvent::Dismissed(_) => return self.update_inner(Message::Dismiss),
                SheetEvent::FocusNext | SheetEvent::FocusPrevious => {
                    if let Some(operation) = event.scoped_focus_operation::<Message>(
                        enzui_widgets::modal_host::presentation_scope_id("music-now-playing-sheet"),
                    ) {
                        return iced::advanced::widget::operate(operation);
                    }
                }
            },
            Message::ToggleSidebar => {
                // Toggling means flipping the inline preference or opening the
                // drawer, so resolve the current presentation with the widget's
                // own `resolve_for_width` before routing.
                let presentation = self.sidebar_presentation();
                self.nav.toggle_for_presentation(presentation);
            }
            Message::SidebarState(next) => {
                // The shell already applied `toggle_for_presentation`; recomputing
                // here would add a second collapse authority.
                self.nav = next;
            }
            Message::DismissSidebar => {
                self.nav.dismiss_overlay();
            }
            Message::Window(command) => {
                // `enz_title_bar` only emits the gesture; `WindowCommand::into_task`
                // maps it to the per-window task.
                return iced::window::latest().then(move |maybe_id| match maybe_id {
                    Some(id) => command.into_task(id),
                    None => Task::none(),
                });
            }
            // Routed in `update` before `update_inner`, so a completion reply
            // does not re-enter the autosave tail and register another write.
            Message::PersistenceFinished { .. }
            | Message::RetryPersistence
            | Message::DiscardUnsavedAndClose => {}
            // `update` routes these first; the reducer-direct path uses the same
            // handlers, so a Retire still reacquires off-thread under the
            // generation guard.
            Message::RetirePersistence => return self.reacquire_authority(true),
            Message::AuthorityReacquired(refresh) => {
                return self.apply_authority_refresh(*refresh);
            }
            Message::TogglePlayPause => {
                // Starting a local source requires a completed scan to prove it
                // Present; otherwise refuse with the state disclosure and send no
                // audio command. Pausing and radio/podcast streams are not gated.
                if !self.playback.playing
                    && self.playback.stream_selection().is_none()
                    && let Some(id) = self.playback.current_track_id
                    && self.refuse_unplayable_activation(id)
                {
                    return Task::none();
                }
                // `playing` is not persisted: a backend-less build that woke up
                // "playing" would contradict its disclosure banner. With a real
                // backend the UI mirrors `PlayingConfirmed` / `NotPlaying` from the
                // bridge; without one the reducer flips the flag directly.
                if self.audio.is_some() {
                    if self.playback.playing {
                        self.request_pause();
                    } else if self.playback.current_track_id.is_some()
                        || self.playback.stream_selection().is_some()
                    {
                        self.request_play();
                    }
                } else {
                    self.playback.playing = !self.playback.playing;
                }
            }
            command @ (Message::NextTrack | Message::TrackEnded { .. }) => {
                if self.playback.stream_selection().is_some() {
                    return Task::none();
                }
                if let Message::TrackEnded { generation } = command
                    && (self.close_after_save
                        || generation != self.playback.completion_generation
                        || self.playback.current_track_id.is_none())
                {
                    return Task::none();
                }
                if matches!(command, Message::TrackEnded { .. })
                    && matches!(self.sleep_timer, SleepTimerState::UntilTrackEnd { .. })
                {
                    self.sleep_timer.cancel();
                    self.sleep_last_tick = None;
                    self.sleep_armed_preset = None;
                    self.playback.playing = false;
                    return Task::none();
                }
                if !self.playback.queue.is_empty() {
                    let repeat_one = matches!(command, Message::TrackEnded { .. })
                        && matches!(self.playback.repeat, RepeatMode::One);
                    let wrap = matches!(self.playback.repeat, RepeatMode::All);
                    let current_id = self.playback.queue[self.playback.queue_index];
                    let target = if repeat_one && self.library.is_playable_now(current_id) {
                        Some(self.playback.queue_index)
                    } else {
                        self.playback.present_index_forward(&self.library, wrap)
                    };
                    match target {
                        Some(idx) => {
                            self.playback.queue_index = idx;
                            // `load_current_track` moves duration, position and the
                            // completion generation/credit together.
                            let next_id = self.playback.queue[idx];
                            self.playback.load_current_track(next_id, &self.library);
                            self.route_local_track_to_audio(next_id, true);
                            // The resumable session changed.
                            self.mark_persistence_dirty();
                        }
                        None => {
                            self.playback.playing = false;
                            if matches!(command, Message::NextTrack)
                                && let Some(audio) = self.audio.as_ref()
                            {
                                let _ = audio.stop(&mut self.media);
                            }
                        }
                    }
                }
            }
            Message::PreviousTrack => {
                if self.playback.stream_selection().is_some() {
                    return Task::none();
                }
                if !self.playback.queue.is_empty() {
                    // Step back to the nearest playable predecessor, skipping
                    // entries not proved Present. Previous always wraps.
                    if let Some(idx) = self.playback.present_index_backward(&self.library, true) {
                        self.playback.queue_index = idx;
                        // Same chokepoint as `NextTrack` above.
                        let prev_id = self.playback.queue[idx];
                        self.playback.load_current_track(prev_id, &self.library);
                        self.route_local_track_to_audio(prev_id, true);
                        // Same shape as `NextTrack`.
                        self.mark_persistence_dirty();
                    }
                }
            }
            Message::ToggleShuffle => {
                self.playback.set_shuffle(!self.playback.shuffle);
                self.mark_persistence_dirty();
            }
            Message::CycleRepeat => {
                self.playback.repeat = match self.playback.repeat {
                    RepeatMode::Off => RepeatMode::All,
                    RepeatMode::All => RepeatMode::One,
                    RepeatMode::One => RepeatMode::Off,
                };
                self.mark_persistence_dirty();
            }
            Message::AdjustVolume(delta) => {
                let volume = (self.playback.volume + delta).clamp(0.0, 1.0);
                self.request_volume(volume as f64);
            }
            Message::SetVolume(volume) => {
                self.request_volume(volume);
            }
            Message::ToggleMute => {
                self.request_mute(!self.playback.muted);
            }
            // A drag only previews: seeking on every pointer move restarted
            // the decoder dozens of times a second and made the audio stutter.
            Message::SeekDragStart => {
                self.seek_dragging = true;
            }
            Message::SeekPreview(pos) => {
                if self.seek_dragging {
                    self.seek_preview = Some(pos);
                } else {
                    return self.update_inner(Message::Seek(pos));
                }
            }
            Message::SeekCommit => {
                self.seek_dragging = false;
                // The preview holds at the released position until playback
                // reports it (see `PositionUpdated`), so the bar does not
                // flick back to the old position for the seek's few frames.
                if let Some(pos) = self.seek_preview {
                    self.seek_settle_until = Some(std::time::Instant::now() + SEEK_SETTLE_TIMEOUT);
                    return self.update_inner(Message::Seek(pos));
                }
            }
            Message::Seek(pos) => {
                if pos.is_finite()
                    && let Ok(position) = std::time::Duration::try_from_secs_f64(pos.max(0.0))
                {
                    self.request_seek(position);
                }
            }
            Message::PlaybackCompleted { generation, reason } => {
                self.apply_playback_completion(generation, reason);
            }
            Message::WindowResized(size) => {
                // Ignore degenerate sizes: `Opened` forwards a zero-size sentinel,
                // so keep the tracked size until a real `Resized` arrives.
                if size.width > 0.0 && size.height > 0.0 {
                    self.window_size = size;
                }
            }
            Message::BottomChromeMeasured(size) => {
                // Whether a layout has that chrome is decided by
                // `toast_bottom_inset`.
                self.bottom_chrome_height = size.height;
            }
            Message::AddFolder => {
                // A chooser request that never starts (no portal) takes the same
                // finish path as a late reply, so it fails visibly. A new attempt
                // retires the persistent failure banner; transient
                // acknowledgements expire on their own.
                self.folder_failure = None;
                if let Err(outcome) = self.folder_chooser.start(picker::PickerTarget::AddFolder) {
                    return self.finish_folder_pick(picker::PickerTarget::AddFolder, outcome);
                }
            }
            Message::RemoveFolder(path) => {
                // Remove the source folder; scanned tracks stay in the library.
                if self.library.remove_folder(&path) {
                    self.mark_persistence_dirty();
                    // Forgetting the source also stops watching it.
                    self.library_watch.remove_root(&path);
                    // A root removed before its initial scan finished retires its
                    // boot-finalization debt.
                    self.retire_initial_scan(&path);
                    // Polite, bounded and coalesced, so a double-press announces once.
                    self.post_folder_status(
                        format!("Removed {}", path.to_string_lossy()),
                        ToastTone::Success,
                    );
                }
            }
            Message::PortalTick => {
                if let Some((target, outcome)) = self.folder_chooser.pump() {
                    return self.finish_folder_pick(target, outcome);
                }
            }
            Message::PortalFolderOutcome(target, outcome) => {
                return self.finish_folder_pick(target, outcome);
            }
            // The host's lifecycle: the deadline-keyed auto-dismiss wake, hover
            // pause/resume, and the surface's close affordance.
            Message::Toast(event) => {
                if matches!(event, enzui_widgets::toast_host::ToastEvent::Tick(_)) {
                    // Disarm the delivered one-shot so the tail re-arms only for
                    // the remaining deadline.
                    self.toast_expiry = enzui_widgets::toast_host::ToastExpirySchedule::default();
                }
                self.toasts.update(event);
            }
            // The persistent failure banner is retired by acknowledgement or the
            // next Add Folder attempt, never by a timer.
            Message::DismissFolderFailure => {
                self.folder_failure = None;
            }
            Message::HideNotice(id) => self.hide_notice(id),
            Message::ScanComplete(tracks) => {
                self.absorb_scanned_tracks(tracks);
            }
            Message::BootStateSettled(report) => {
                // Only the current generation may apply, and applying advances
                // it, so a re-delivered report cannot clobber a newer session.
                if report.generation != self.boot_load_generation {
                    return Task::none();
                }
                self.boot_load_generation = self.boot_load_generation.wrapping_add(1);
                self.boot_load_worker_spawned = false;
                // The bounded owner owns worker lifetime; nothing is joined here.
                return self.apply_boot_document(*report);
            }
            _ => unreachable!("reduce_batch1 handles only its own variants"),
        }
        Task::none()
    }
}

/// The longest a released seek bar waits for playback to reach its position.
const SEEK_SETTLE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);
