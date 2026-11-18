//! Reducer batch 4: TogglePlaybackPreferences through ContextAction.

use super::*;

impl MusicApp {
    pub(crate) fn reduce_batch4(&mut self, message: Message) -> Task<Message> {
        let previous_rg = self.rg;
        match message {
            Message::TogglePlaybackPreferences => {
                if self.rg_presentation.is_open() {
                    return self.dismiss_modal_focus(
                        ModalPresentationId::PlaybackPreferences,
                        enzui_core::modal::DismissCause::CloseButton,
                    );
                }
                self.rg_presentation.present(());
                self.rg_mode_menu_open = false;
                return self.open_modal_focus(
                    ModalPresentationId::PlaybackPreferences,
                    Some(views::now_playing_menu_trigger_id()),
                );
            }
            Message::OpenLibraryPreferences => {
                if self.rg_presentation.is_open() {
                    return Task::none();
                }
                self.settings_page = views::SETTINGS_LIBRARY_PAGE.to_owned();
                self.settings_page_open = true;
                self.rg_mode_menu_open = false;
                self.rg_presentation.present(());
                return self.open_modal_focus(
                    ModalPresentationId::PlaybackPreferences,
                    Some(views::now_playing_menu_trigger_id()),
                );
            }
            Message::TitlebarOverflow(open) => {
                self.titlebar_overflow_open = open;
            }
            Message::PlaybackPreferencesExitSettled(key) => {
                self.rg_presentation.finish_exit(key);
            }
            Message::SelectSettingsPage(page) => {
                self.settings_page = page;
                self.settings_page_open = true;
                self.rg_mode_menu_open = false;
            }
            Message::SettingsStackedBack => {
                self.settings_page_open = false;
                self.rg_mode_menu_open = false;
            }
            Message::RgModeMenu(open) => {
                self.rg_mode_menu_open = open;
            }
            Message::RgSelectMode(index) => {
                self.rg_mode_menu_open = false;
                // `enz_radio_group` selects by index; an out-of-range index is
                // ignored rather than falling back to `Off`.
                if let Some(&mode) = ReplayGainMode::SELECTABLE.get(index) {
                    self.rg.mode = mode;
                }
            }
            Message::RgSetPreamp(db) => {
                // `set_preamp` clamps to −15..+15 dB and refuses non-finite values.
                self.rg.set_preamp(db);
            }
            Message::RgSetPreventClip(on) => {
                self.rg.prevent_clip = on;
            }

            // Sleep timer
            Message::ToggleSleepTimer => {
                self.sleep_open = !self.sleep_open;
                return self.toggle_modal_focus(
                    self.sleep_open,
                    ModalPresentationId::SleepTimer,
                    None,
                );
            }
            Message::SleepArm(index) => {
                if let Some(&preset) = SleepPreset::SELECTABLE.get(index) {
                    // `arm()` replaces any existing timer.
                    self.sleep_timer.arm(preset, self.sleep_fade_choice);
                    self.sleep_armed_preset = Some(preset);
                    self.sleep_last_tick =
                        matches!(self.sleep_timer, SleepTimerState::Armed { .. })
                            .then(std::time::Instant::now);
                }
            }
            Message::SleepTick => {
                if !matches!(self.sleep_timer, SleepTimerState::Armed { .. }) {
                    return Task::none();
                }
                // Subtract measured elapsed time, not one assumed minute, so a
                // delayed wake cannot make the timer run slow.
                let now = std::time::Instant::now();
                let dt = self
                    .sleep_last_tick
                    .map(|then| now.duration_since(then).as_secs_f64())
                    .unwrap_or(0.0);
                self.sleep_last_tick = Some(now);
                if self.sleep_timer.tick(dt) {
                    // `tick` returns true exactly once, on expiry.
                    if self.audio.is_some() && self.playback.playing {
                        self.sleep_pause_pending = true;
                        self.request_pause();
                    } else {
                        self.playback.playing = false;
                    }
                    self.sleep_last_tick = None;
                    self.sleep_armed_preset = None;
                }
            }
            Message::SleepSetFade(on) => {
                self.sleep_fade_choice = on;
            }
            Message::SleepCancel => {
                self.sleep_timer.cancel();
                self.sleep_last_tick = None;
                self.sleep_armed_preset = None;
            }

            // Overflow menus: `menu_button::apply_event` is the single place a
            // duplicate activation could occur; fired rows map to the same
            // messages as the equivalent affordances.
            Message::NowPlayingMenu(event) => {
                // The speed submenu's check follows the current rate, refreshed as the menu opens.
                if !self.now_playing_menu.is_open() {
                    self.now_playing_menu_model = now_playing_menu_model(self.rate_policy);
                }
                let activated = menu_button::apply_event(
                    &mut self.now_playing_menu_model,
                    &mut self.now_playing_menu,
                    &event,
                );
                if let Some(id) = activated {
                    return dispatch_now_playing_menu(self, id);
                }
            }
            Message::NavMenu(event) => {
                // The check follows the current destination, refreshed as the menu opens.
                if !self.nav_menu.is_open() {
                    self.nav_menu_model = nav_menu_model(self.current_view);
                }
                let activated =
                    menu_button::apply_event(&mut self.nav_menu_model, &mut self.nav_menu, &event);
                if let Some(view) = activated.as_ref().and_then(|id| nav_menu_view(id.as_str())) {
                    return self.update_inner(Message::SelectView(view));
                }
            }
            Message::QueueMoreMenu(event) => {
                let activated = menu_button::apply_event(
                    &mut self.queue_more_menu_model,
                    &mut self.queue_more_menu,
                    &event,
                );
                if let Some(id) = activated {
                    return dispatch_queue_more_menu(self, id);
                }
            }
            // Sort dropdown: only Activated rows map to `SetSort`.
            Message::SortMenu(event) => {
                // Rows carry the live check state, refreshed as the menu opens.
                if !self.sort_menu.is_open() {
                    let _ = self.sort_menu_model.reconcile(sort_menu_entries(
                        self.current_view,
                        self.sort_mode,
                        self.filter,
                    ));
                }
                let activated = menu_button::apply_event(
                    &mut self.sort_menu_model,
                    &mut self.sort_menu,
                    &event,
                );
                if let Some(id) = activated {
                    if id.as_str() == SORT_MENU_RATED_ID {
                        return Task::done(Message::SetRatedFilter(
                            self.filter != LibraryFilter::Rated,
                        ));
                    }
                    if let Some(mode) = sort_mode_from_entry_id(id.as_str()) {
                        return Task::done(Message::SetSort(mode));
                    }
                }
            }

            // Right-click context menus
            Message::PlayTrackSet(ids) => {
                // Bulk Play seeds only playable ids; an all-blocked set refuses
                // and an empty container is a silent no-op.
                if !ids.is_empty() {
                    self.start_playable_seed(ids, false);
                }
                // Seeding is not a play; credit arrives with
                // `Message::PlaybackCompleted`.
            }
            Message::AddTracksToQueue(ids) => {
                // Bulk enqueue admits only playable ids; an all-blocked set
                // refuses and an empty container is a silent no-op.
                if !ids.is_empty() {
                    let playable: Vec<u64> = ids
                        .into_iter()
                        .filter(|&id| self.library.is_playable_now(id))
                        .collect();
                    if playable.is_empty() {
                        self.post_folder_status(
                            domain::BULK_NONE_PLAYABLE_DISCLOSURE,
                            ToastTone::Warning,
                        );
                    } else {
                        let mut refused = false;
                        for id in playable {
                            if !self.playback.add_to_queue(id) {
                                // The queue filled mid-bulk; disclose once.
                                refused = true;
                                break;
                            }
                        }
                        if refused {
                            self.post_folder_status(
                                domain::QUEUE_FULL_DISCLOSURE,
                                ToastTone::Warning,
                            );
                        }
                        self.mark_persistence_dirty();
                    }
                }
            }
            Message::TrackDetailsPage(page) => {
                if let Some(details) = self.track_details.as_mut() {
                    details.page = page;
                }
            }
            Message::TrackDetailsReload => self.reload_track_details(),
            Message::TrackDetailsCancel => {
                self.cancel_track_details(track_details::Failure::Cancelled)
            }
            Message::TrackDetailsPoll => {
                self.drain_track_details();
                self.track_details_service.progress();
            }
            Message::SetTrackRating(path, rating) => {
                if self
                    .track_info_id
                    .and_then(|id| self.library.tracks.get(&id))
                    .is_some_and(|track| track.path == path)
                    && let Ok(rating) = u8::try_from(rating)
                    && rating <= 5
                {
                    if self.library.set_rating(path.clone(), rating) {
                        self.mark_persistence_dirty();
                    } else if self.library.rating_for_path(&path) != rating {
                        self.post_folder_status("The saved-rating limit was reached. Clear another rating before adding one.", ToastTone::Warning);
                    }
                }
            }
            Message::SetRatedFilter(rated) => {
                self.filter = if rated {
                    LibraryFilter::Rated
                } else {
                    LibraryFilter::None
                };
                self.current_view = View::Home;
            }
            Message::SetPlaybackRate(index) => {
                if let Some(rate) = track_experience::RATES.get(index) {
                    self.request_playback_rate(*rate);
                }
            }
            Message::ShowTrackInfo(id) => {
                let was_open = self.track_info_id.is_some();
                self.track_details_service.cancel();
                self.track_details = None;
                self.track_info_id = id.filter(|id| self.library.tracks.contains_key(id));
                if self.track_info_id.is_some() {
                    self.reload_track_details();
                }
                return match self.track_info_id {
                    Some(_) => self.open_modal_focus(ModalPresentationId::TrackInfo, None),
                    None if was_open => self.dismiss_modal_focus(
                        ModalPresentationId::TrackInfo,
                        enzui_core::modal::DismissCause::CloseButton,
                    ),
                    None => Task::none(),
                };
            }
            Message::OpenContextMenu(target, at) => {
                // The app-level resolver applies one availability, focus and
                // naming rule to every target, and opens nothing without a
                // focusable row.
                self.context_menu = self.build_context_menu(target, at, None, None);
            }
            Message::OpenContextMenuFrom(target, at, invoker) => {
                self.context_menu = self.build_context_menu(target, at, None, Some(invoker));
            }
            Message::OpenPodcastEpisodeMenu(feed_url, guid, at) => {
                // Episodes are keyed by `(feed_url, guid)`; refuse a key that no
                // longer resolves, then mint this opening's generation.
                if self.episode(&feed_url, &guid).is_none() {
                    return Task::none();
                }
                // `checked_add` refuses on exhaustion instead of reusing a token.
                let Some(next) = self.context_menu_generation.checked_add(1) else {
                    return Task::none();
                };
                self.context_menu_generation = next;
                let target = domain::ContextTarget::PodcastEpisode(next);
                self.context_menu =
                    self.build_context_menu(target, at, Some((feed_url, guid)), None);
            }
            // Every framework host event arrives here. `apply_event_restoring`
            // opens and closes atomically, skips separators when navigating,
            // and re-resolves activations against the live surface.
            Message::ContextMenuEvent(event) => {
                use enzui_widgets::context_menu_host::apply_event_restoring;
                let Some(open) = self.context_menu.as_mut() else {
                    // A dismissal after close is a no-op.
                    return Task::none();
                };
                let target = open.target;
                // Captured before the menu may be dropped, so podcast Play and
                // radio actions can re-verify their durable keys.
                let podcast_key = open.podcast_key.clone();
                let radio_key = open.radio_key.clone();
                let update =
                    apply_event_restoring::<Message>(&mut open.model, &mut open.state, &event);
                let fired = update.activated().cloned();
                // Drop the whole menu on close so no stale model outlives it.
                if !self
                    .context_menu
                    .as_ref()
                    .is_some_and(|m| m.state.is_open())
                {
                    self.context_menu = None;
                }
                // Re-dispatch through the typed arm against live state, so menu
                // and in-page actions share one reducer body.
                if let Some(id) = fired {
                    // Episode Play needs the durable key, which `ContextAction`
                    // cannot carry, so it routes directly; a gone episode is inert.
                    if let domain::ContextTarget::PodcastEpisode(_) = target {
                        let action = self.context_menu_row_for_id(
                            target,
                            radio_key.as_ref(),
                            podcast_key.as_ref(),
                            &id,
                        );
                        if let (Some(action), Some((feed_url, guid))) = (action, podcast_key) {
                            let message = if action == domain::ContextAction::TrackInfo {
                                Message::PodcastEpisodeDetails(feed_url, guid)
                            } else {
                                Message::PodcastEpisodeSelect(feed_url, guid)
                            };
                            return update.finish(self.update(message));
                        }
                        return update.finish(Task::none());
                    }
                    if let Some(action) = self.context_menu_row_for_id(
                        target,
                        radio_key.as_ref(),
                        podcast_key.as_ref(),
                        &id,
                    ) {
                        return update.finish(self.update(Message::ContextAction(target, action)));
                    }
                }
                return update.finish(Task::none());
            }
            Message::ContextAction(target, action) => {
                // Close the menu, then re-dispatch the in-page control's message.
                self.context_menu = None;
                if let Some(msg) = context_menu_action_message(self, target, action) {
                    return self.update(msg);
                }
            }
            _ => unreachable!("reduce_batch4 handles only its own variants"),
        }
        if self.rg != previous_rg {
            self.mark_persistence_dirty();
        }
        Task::none()
    }
}
