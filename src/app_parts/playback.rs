//! Playback sessions, catalog, and podcast fetching.

use super::*;

impl MusicApp {
    /// Apply the pending playback session once the boot scan has resolved
    /// paths to ids.
    ///
    /// Called from [`Self::retire_initial_scan`] when the last initial root
    /// completes, or from [`MusicApp::boot_persisting_with`] when
    /// `initial_scan_roots` is already empty. `MusicApp::with_persistence` does
    /// not finalize, so tests can seed the library before calling this.
    ///
    /// Scalar preferences always apply. Queue and current-track paths resolve
    /// through the live library; an unresolved path is dropped, never
    /// retargeted. The saved `queue_index` follows the resolved current track,
    /// else is clamped. Playback stays paused until the backend confirms play.
    /// Dropped entries raise one Library notice and schedule a rewrite of the
    /// persisted session, so the next launch is quiet.
    pub(crate) fn finalize_pending_playback_session(&mut self) {
        let Some(session) = self.pending_playback_session.take() else {
            return;
        };

        // Scalar preferences: no path identity, always safe to apply.
        self.playback.volume = session.volume.clamp(0.0, 1.0);
        self.playback.muted = session.muted;
        self.playback.shuffle = session.shuffle;
        self.playback.repeat = session.repeat.into();

        // Scoped so the immutable library borrow ends before mutating
        // `self.playback`; unresolved paths are skipped, never retargeted.
        let (
            resolved_queue,
            resolved_current,
            dropped_from_queue,
            dropped_current,
            resolved_order,
            saved_cursor,
        ) = {
            let library = &self.library;
            let resolve = |path: &std::path::Path| library.track_id_for_path(path);
            let saved_queue_len = session.queue_paths.len();
            let mut queue = Vec::with_capacity(saved_queue_len);
            let mut positions = vec![None; saved_queue_len];
            for (index, path) in session.queue_paths.iter().enumerate() {
                if let Some(id) = resolve(path) {
                    positions[index] = Some(queue.len());
                    queue.push(id);
                }
            }
            let resolved_order: Vec<_> = session
                .unshuffled_order
                .iter()
                .filter_map(|index| positions.get(*index).copied().flatten())
                .collect();
            let saved_cursor = positions.get(session.queue_index).copied().flatten();
            let dropped_queue = saved_queue_len - queue.len();
            let saved_current_present = session.current_track_path.is_some();
            let current = session.current_track_path.as_deref().and_then(resolve);
            let dropped_current = saved_current_present && current.is_none();
            (
                queue,
                current,
                dropped_queue,
                dropped_current,
                resolved_order,
                saved_cursor,
            )
        };

        // Queue-index resolution: the resolved current track's position wins
        // when it is in the queue (so a resumed Now Playing row aligns with
        // its `queue_index`), else the saved index is clamped in.
        let resolved_index = if resolved_queue.is_empty() {
            0
        } else if let Some(current_id) = resolved_current {
            saved_cursor
                .filter(|index| resolved_queue.get(*index) == Some(&current_id))
                .or_else(|| resolved_queue.iter().position(|&id| id == current_id))
                .unwrap_or_else(|| session.queue_index.min(resolved_queue.len() - 1))
        } else {
            session.queue_index.min(resolved_queue.len() - 1)
        };

        // Sessions persist only ordered paths, so occurrence ids are re-minted
        // deterministically; `resolved_index` still names the current entry.
        self.playback.reset_queue_from_ids(&resolved_queue);
        self.playback.unshuffled_order = resolved_order
            .iter()
            .filter_map(|index| self.playback.entry_id_at(*index))
            .collect();
        self.playback.queue_index = resolved_index.min(self.playback.queue.len().saturating_sub(1));

        if let Some(id) = resolved_current {
            // `load_current_track` zeroes the playhead, so restore it afterwards,
            // clamped to a known duration (`0.0` means unknown; leave it alone).
            self.playback.load_current_track(id, &self.library);
            let saved = std::time::Duration::try_from_secs_f64(session.position_secs.max(0.0))
                .map(|position| position.as_secs_f64())
                .unwrap_or(0.0);
            let duration = self.playback.duration_secs;
            self.playback.position_secs = if duration > 0.0 {
                saved.min(duration)
            } else {
                saved
            };
        } else {
            self.playback.current_track_id = None;
            self.playback.current_stream = None;
            self.playback.position_secs = 0.0;
            self.playback.duration_secs = 0.0;
        }

        // One coalesced notice; a fully successful resume stays silent.
        let missing_total = dropped_from_queue + usize::from(dropped_current);
        if missing_total > 0 {
            let text = if dropped_from_queue > 0 && dropped_current {
                format!(
                    "Resumed session — skipped {} queued {} and the previously playing \
                     track because their files are no longer in the library",
                    dropped_from_queue,
                    if dropped_from_queue == 1 {
                        "track"
                    } else {
                        "tracks"
                    },
                )
            } else if dropped_from_queue > 0 {
                format!(
                    "Resumed session — skipped {} queued {} whose {} no longer in the library",
                    dropped_from_queue,
                    if dropped_from_queue == 1 {
                        "track"
                    } else {
                        "tracks"
                    },
                    if dropped_from_queue == 1 {
                        "file is"
                    } else {
                        "files are"
                    },
                )
            } else {
                "Resumed session — the previously playing track's file is no longer \
                 in the library"
                    .to_string()
            };
            self.post_folder_status(text, ToastTone::Warning);
            // Rewrite the queue so the next launch does not re-drop the same entries.
            self.mark_persistence_dirty();
        }
    }

    /// Restore persisted play stats onto the freshly scanned library.
    ///
    /// Runs on the same edge as [`Self::finalize_pending_playback_session`],
    /// before the first view projection, so recency and play-count ordering
    /// see restored values. Path is the identity: counts are restored, not
    /// incremented, and `Track.id` is never persisted. A path with no scanned
    /// track is dropped, raises one Library notice, and marks persistence
    /// dirty so the next launch is quiet.
    pub(crate) fn finalize_pending_track_stats(&mut self) {
        let Some(stats) = self.pending_track_stats.take() else {
            return;
        };

        // Resolve records under a read borrow first, then apply them under a
        // mutable borrow; O(tracks + stats) without cloning every path.
        let mut resolved: Vec<(u64, u32, u64)> = Vec::with_capacity(stats.len());
        let mut dropped: usize = 0;
        {
            let by_path: std::collections::HashMap<&std::path::Path, u64> = self
                .library
                .tracks
                .values()
                .map(|t| (t.path.as_path(), t.id))
                .collect();
            for rec in &stats {
                if let Some(&id) = by_path.get(rec.path.as_path()) {
                    resolved.push((id, rec.play_count, rec.last_played_unix));
                } else if rec.play_count > 0 || rec.last_played_unix > 0 {
                    dropped += 1;
                }
            }
        }
        for (id, play_count, last_played_unix) in resolved {
            self.library
                .set_play_stats(id, play_count, last_played_unix);
        }

        if dropped > 0 {
            // Shares the coalescing key with folder-status and resume notices.
            let text = format!(
                "Reconciled play stats — {dropped} {} no longer in the \
                 library {} skipped",
                if dropped == 1 { "track" } else { "tracks" },
                if dropped == 1 { "was" } else { "were" },
            );
            self.post_folder_status(text, ToastTone::Warning);
            self.mark_persistence_dirty();
        }
        // No `library.reindex()`: at boot no cached projection exists to
        // invalidate, so a reindex would only rebuild sort keys for nothing.
    }

    /// Apply an authoritative backend completion event.
    ///
    /// A play is credited only when `generation` matches the current source,
    /// the generation is not yet credited, a current track exists, and, for
    /// [`PlaybackCompletionReason::ThresholdCrossed`], `position_secs` reaches
    /// `min(240.0, duration_secs * 0.5)`. End-of-stream needs no threshold.
    pub(crate) fn sync_playback_generation_from_media(&mut self) {
        self.playback.completion_generation = self.media.generation().raw();
        self.playback.completion_credited = false;
    }

    /// Open a local track in the audio service and realign the app generation.
    ///
    /// `autoplay` selects `load_local_and_autoplay` over `load_local_paused`.
    /// This is the only local-source entry point: it refuses unless a completed
    /// scan proved the track `Present` under a live availability generation.
    /// Without an audio backend it does nothing. Streams are not gated here.
    pub(crate) fn route_local_track_to_audio(&mut self, track_id: u64, autoplay: bool) {
        self.sleep_pause_pending = false;
        if self.availability_exhausted || !self.library.is_present(track_id) {
            return;
        }
        let Some(path) = self
            .library
            .tracks
            .get(&track_id)
            .map(|track| track.path.clone())
        else {
            return;
        };
        let opened_path = path.clone();
        let admitted = {
            let Some(audio) = self.audio.as_mut() else {
                return;
            };
            if autoplay {
                audio.load_local_and_autoplay(path, &mut self.media)
            } else {
                audio.load_local_paused(path, &mut self.media)
            }
        };
        if let Ok(generation) = admitted {
            self.sync_playback_generation_from_media();
            self.mpris_source = Some(MprisSourceIdentity::Local {
                generation: generation.raw(),
                track_id,
                occurrence: self.playback.current_entry_id(),
            });
            // Bind this generation to the opened identity; later outcomes fold
            // onto this path only while it is still the current track.
            self.pending_source_open = Some(PendingSourceOpen {
                media_generation: generation.raw(),
                track_id,
                path: opened_path,
                admission_generation: self.availability_generation,
                confirmed: false,
            });
            self.sync_audio_processing();
        }
    }

    /// Drain backend audio events into [`Self::media`] and fold the outcomes
    /// into UI state and follow-up messages.
    ///
    /// Transport state mirrors confirmed backend events, never optimistic
    /// flags. A restored generation seeks to its saved playhead on
    /// `SourceReady`. Completion schedules `Message::PlaybackCompleted`, and
    /// end-of-stream also advances the queue. An error shows one retryable
    /// Warning banner and leaves playback paused.
    pub(crate) fn drain_audio_events(&mut self) -> Task<Message> {
        let translated: Vec<audio::BridgeEvent> = {
            let (Some(audio), Some(receiver)) = (self.audio.as_mut(), self.audio_receiver.as_ref())
            else {
                return Task::none();
            };
            let confirmed_duration = self.playback.duration_secs;
            audio.drain_and_translate(receiver, &mut self.media, || confirmed_duration)
        };
        let mut followups: Vec<Task<Message>> = Vec::new();
        for event in translated {
            match event {
                audio::BridgeEvent::SourceReady {
                    generation,
                    duration_secs,
                } => {
                    // The pipeline exists: the listener's volume, mute and
                    // effects can reach it (see `sync_audio_processing`).
                    self.audio_live_generation = Some(generation);
                    // Keep the tag-reader duration when the backend reports none.
                    if duration_secs > 0.0 {
                        self.playback.duration_secs = duration_secs;
                    }
                    // Confirm Present only for the current matching open under its
                    // admission generation, so a later Missing scan still wins.
                    self.confirm_source_ready_present(generation);
                    // Seek a restored generation to its saved playhead; no Play.
                    if self.playback.position_secs > 0.0 {
                        let target = if self.playback.duration_secs > 0.0 {
                            self.playback.position_secs.min(self.playback.duration_secs)
                        } else {
                            self.playback.position_secs
                        };
                        let pos =
                            std::time::Duration::try_from_secs_f64(target).unwrap_or_default();
                        self.playback.position_secs = pos.as_secs_f64();
                        if let Some(a) = self.audio.as_ref() {
                            let _ = a.seek(pos, &mut self.media);
                        }
                    }
                    // A live station has no speed: a rate change on it is
                    // refused by the backend.
                    let live_station = matches!(
                        self.playback.stream_selection(),
                        Some(crate::StreamSelection::Station(_))
                    );
                    if !live_station
                        && self.rate_requested_generation != Some(generation)
                        && self.rate_policy != self.media.snapshot().rate
                    {
                        self.request_playback_rate(self.rate_policy);
                    }
                }
                audio::BridgeEvent::PlayingConfirmed => {
                    self.playback.playing = true;
                    self.playback.stream_phase = crate::StreamPhase::Idle;
                }
                audio::BridgeEvent::NotPlaying => {
                    self.sleep_pause_pending = false;
                    self.playback.playing = false;
                    // Pause and stop are resume boundaries: flush the podcast
                    // playhead so a quit while paused keeps it.
                    if self.record_episode_resume() {
                        self.mark_persistence_dirty();
                    }
                }
                audio::BridgeEvent::PositionUpdated { secs } => {
                    self.playback.position_secs = secs;
                    // A released seek bar shows its target until playback is
                    // there (or the wait runs out).
                    if !self.seek_dragging
                        && let Some(target) = self.seek_preview
                        && ((secs - target).abs() < SEEK_SETTLED_WITHIN
                            || self
                                .seek_settle_until
                                .is_none_or(|until| std::time::Instant::now() >= until))
                    {
                        self.seek_preview = None;
                        self.seek_settle_until = None;
                    }
                    // Track the resume point only after confirmed playback, so a
                    // pre-roll `PositionUpdated(0)` cannot clear a seeded resume.
                    // Persistence happens at boundaries, not per tick.
                    if self.playback.playing {
                        self.record_episode_resume();
                    }
                }
                audio::BridgeEvent::VolumeChanged { volume } => {
                    let volume = volume as f32;
                    match self.volume_pending {
                        // An earlier step of a drag: the newer value stands.
                        Some(pending) if (pending - volume).abs() > 0.001 => {}
                        _ => {
                            self.volume_pending = None;
                            self.playback.volume = volume;
                            self.mark_persistence_dirty();
                        }
                    }
                }
                audio::BridgeEvent::RateChanged { rate } => {
                    if enzui_services::media::sanitize_rate(rate).is_some() {
                        self.rate_policy = rate;
                        self.rate_requested_generation = None;
                        self.mark_persistence_dirty();
                    }
                }
                audio::BridgeEvent::MuteChanged(muted) => {
                    self.playback.muted = muted;
                    self.mark_persistence_dirty();
                }
                audio::BridgeEvent::Completed { generation, reason } => {
                    let app_reason = match reason {
                        audio::CompletionReason::EndOfStream => {
                            PlaybackCompletionReason::EndOfStream
                        }
                        audio::CompletionReason::ThresholdCrossed {
                            position_secs,
                            duration_secs,
                        } => PlaybackCompletionReason::ThresholdCrossed {
                            position_secs,
                            duration_secs,
                        },
                    };
                    let is_eos = matches!(app_reason, PlaybackCompletionReason::EndOfStream);
                    if is_eos
                        && self.playback.stream_selection().is_some()
                        && matches!(self.sleep_timer, SleepTimerState::UntilTrackEnd { .. })
                    {
                        self.sleep_timer.cancel();
                        self.sleep_last_tick = None;
                        self.sleep_armed_preset = None;
                    }
                    // A finished episode restarts from the beginning next time.
                    if is_eos
                        && let Some(StreamSelection::Episode { feed_url, guid }) =
                            self.playback.stream_selection().cloned()
                        && self.clear_episode_resume(&feed_url, &guid)
                    {
                        self.mark_persistence_dirty();
                    }
                    let completion = Task::done(Message::PlaybackCompleted {
                        generation,
                        reason: app_reason,
                    });
                    // Advancing changes the generation used to credit this play.
                    followups.push(if is_eos {
                        completion.chain(Task::done(Message::TrackEnded { generation }))
                    } else {
                        completion
                    });
                }
                audio::BridgeEvent::Error { generation, error } => {
                    // Only `SourceNotFound` or `UnsupportedFormat` for the matching
                    // open demotes the path and offers Rescan/Remove; other errors
                    // keep the file and show the retryable banner.
                    if self.fold_backend_error_into_availability(generation, &error) {
                        continue;
                    }
                    // A command the backend refused because the stream is not
                    // live yet (or any more) is sequencing, not a fault: the
                    // load's own error, if any, is what the user sees.
                    if is_command_refusal(&error) {
                        eprintln!("music audio: refused: {}", error.message);
                        continue;
                    }
                    // A stream that fails while connecting or playing says so
                    // on its own row, softly, rather than in a toast.
                    if generation == self.playback.completion_generation
                        && self.playback.stream_selection().is_some()
                    {
                        eprintln!("music audio: stream failed: {}", error.message);
                        self.playback.stream_phase =
                            crate::StreamPhase::Failed(stream_failure_reason(&error));
                        self.playback.playing = false;
                        self.sleep_pause_pending = false;
                        continue;
                    }
                    // The backend message is pre-redacted by the framework.
                    let diagnostic: String = error
                        .message
                        .chars()
                        .flat_map(char::escape_default)
                        .collect();
                    self.post_folder_status(
                        format!("Playback error: {diagnostic}"),
                        ToastTone::Warning,
                    );
                    if !matches!(
                        error.kind,
                        enzui_services::media::MediaErrorKind::Refused
                            | enzui_services::media::MediaErrorKind::SeekFailed
                    ) {
                        self.playback.playing = false;
                        self.sleep_pause_pending = false;
                    }
                }
            }
        }
        if followups.is_empty() {
            Task::none()
        } else {
            Task::batch(followups)
        }
    }

    /// Credit a play for the current generation if it qualifies.
    ///
    /// Stamps the track's play time, marks persistence dirty, and sets
    /// `completion_credited` so the generation cannot credit again.
    pub(crate) fn apply_playback_completion(
        &mut self,
        generation: u64,
        reason: PlaybackCompletionReason,
    ) {
        if generation != self.playback.completion_generation {
            return;
        }
        if self.playback.completion_credited {
            return;
        }
        let Some(id) = self.playback.current_track_id else {
            return;
        };
        if let PlaybackCompletionReason::ThresholdCrossed {
            position_secs,
            duration_secs,
        } = reason
        {
            // Threshold: min(240 s, 50% of duration). An unknown duration
            // collapses it to 0; a negative position never qualifies.
            let half = if duration_secs > 0.0 {
                duration_secs * 0.5
            } else {
                f64::INFINITY
            };
            let threshold = 240.0_f64.min(half);
            if position_secs < threshold {
                return;
            }
        }
        let unix_now = wall_clock_now_unix_secs();
        self.playback.completion_credited = true;
        self.library.credit_play(id, unix_now);
        self.mark_persistence_dirty();
    }

    /// Record the current podcast episode's playhead in the resume table.
    ///
    /// A no-op for non-episode sources. A non-positive playhead, or one within
    /// [`EPISODE_RESUME_END_MARGIN_SECS`] of the end, clears the stored point.
    /// Returns whether the table changed.
    pub(crate) fn record_episode_resume(&mut self) -> bool {
        let Some(StreamSelection::Episode { feed_url, guid }) =
            self.playback.stream_selection().cloned()
        else {
            return false;
        };
        let position = self.playback.position_secs;
        let duration = self.playback.duration_secs;
        let key = (feed_url, guid);
        let near_end = duration > 0.0 && position >= duration - EPISODE_RESUME_END_MARGIN_SECS;
        if !position.is_finite() || position <= 0.0 || near_end {
            self.episode_resume.remove(&key).is_some()
        } else {
            // Cap the table: an over-ceiling document is refused by
            // `over_domain_limit` and would block every save.
            if !self.episode_resume.contains_key(&key)
                && self.episode_resume.len() >= crate::domain::MAX_EPISODE_RESUME
            {
                self.episode_resume.pop_first();
            }
            self.episode_resume.insert(key, position) != Some(position)
        }
    }

    /// Clear a finished episode's resume point.
    fn clear_episode_resume(&mut self, feed_url: &str, guid: &str) -> bool {
        self.episode_resume
            .remove(&(feed_url.to_string(), guid.to_string()))
            .is_some()
    }

    /// Submit one radio catalog request under the next generation, which
    /// every radio request shares so a superseded reply is dropped. A
    /// refused submit completes at once, so the page always settles.
    fn submit_radio_request(
        &mut self,
        kind: radio_catalog::CatalogRequestKind,
        cancel_older: bool,
    ) -> u64 {
        self.radio_top_list_generation = self.radio_top_list_generation.wrapping_add(1);
        let generation = self.radio_top_list_generation;
        let request = radio_catalog::build_request(generation, kind);
        if cancel_older {
            self.radio_catalog_service.cancel_older_than(generation);
        }
        let kind = request.kind.clone();
        if let Err(message) = self.radio_catalog_service.submit(request) {
            self.apply_radio_catalog_result(radio_catalog::CatalogResult {
                generation,
                kind,
                result: Err(radio_catalog::CatalogError {
                    message,
                    retryable: true,
                    kind: radio_catalog::CatalogErrorKind::Transport,
                }),
            });
        }
        generation
    }

    /// Load the popular stations for the country and genre on show, unless
    /// loaded; and the starred stations' records, unless known.
    pub(crate) fn dispatch_catalog_top_list(&mut self) {
        let country = self.podcast_charts.country.clone();
        let key: radio::BrowseKey = (country.clone(), self.radio.genre);
        if !self.radio.lists.contains_key(&key) && self.radio.loading.as_ref() != Some(&key) {
            self.radio.loading = Some(key);
            let generation = self.submit_radio_request(
                radio_catalog::CatalogRequestKind::Browse {
                    country,
                    genre: self.radio.genre,
                },
                true,
            );
            self.radio.catalog_status = radio::RadioCatalogState::Loading { generation };
        }
        self.resolve_radio_favorites();
    }

    /// Fetch the records of starred stations not known yet.
    pub(crate) fn resolve_radio_favorites(&mut self) {
        let uuids = self.radio.unresolved_favorites();
        if uuids.is_empty() || self.radio_favorites_pending {
            return;
        }
        self.radio_favorites_pending = true;
        self.submit_radio_request(radio_catalog::CatalogRequestKind::ByUuid { uuids }, false);
    }

    /// Search every station by name.
    pub(crate) fn dispatch_catalog_search(&mut self, _generation: u64, query: String) {
        let generation =
            self.submit_radio_request(radio_catalog::CatalogRequestKind::Search { query }, true);
        self.radio.search_generation = generation;
    }

    /// Fold one catalog completion into `RadioState`; stale replies are
    /// dropped, and starred stations are never dropped by a reload.
    pub(crate) fn apply_radio_catalog_result(&mut self, result: radio_catalog::CatalogResult) {
        match result.kind {
            radio_catalog::CatalogRequestKind::Browse { country, genre } => {
                let key: radio::BrowseKey = (country, genre);
                if self.radio.loading.as_ref() == Some(&key) {
                    self.radio.loading = None;
                }
                match result.result {
                    Ok(stations) => {
                        self.radio.lists.insert(key, stations);
                        self.radio.catalog_status = radio::RadioCatalogState::Loaded {
                            at_unix: wall_clock_now_unix_secs(),
                        };
                        // A cancelled favourites fetch is asked again.
                        self.radio_favorites_pending = false;
                        self.resolve_radio_favorites();
                    }
                    Err(err) => {
                        self.radio.catalog_status = radio::RadioCatalogState::Failed {
                            message: err.message.clone(),
                            retryable: err.retryable,
                        };
                    }
                }
            }
            radio_catalog::CatalogRequestKind::Search { .. } => {
                if result.generation != self.radio.search_generation
                    || self.radio.submitted.is_none()
                {
                    return;
                }
                match result.result {
                    Ok(stations) => self.radio.results = Some(stations),
                    Err(err) => {
                        self.radio.results = Some(Vec::new());
                        self.post_folder_status(
                            format!("Radio search: {}", err.message),
                            ToastTone::Warning,
                        );
                    }
                }
            }
            radio_catalog::CatalogRequestKind::ByUuid { .. } => {
                self.radio_favorites_pending = false;
                if let Ok(stations) = result.result {
                    for station in stations {
                        if self.radio.favorites.contains(&station.remote_uuid)
                            && !self
                                .radio
                                .favorite_stations
                                .iter()
                                .any(|s| s.remote_uuid == station.remote_uuid)
                        {
                            self.radio.favorite_stations.push(station);
                        }
                    }
                }
            }
        }
    }

    /// Tune a station and route it to the audio service.
    ///
    /// The selection survives a catalog reload and clears the playing flag
    /// until the pipeline confirms. A URL-policy refusal posts one warning;
    /// without an audio backend the station stays selected.
    pub(crate) fn tune_radio_station(&mut self, id: u64) {
        let Some((stream_url, station_uuid)) = self
            .radio
            .tune(id)
            .map(|station| (station.stream_url.clone(), station.remote_uuid.clone()))
        else {
            return;
        };
        self.sleep_pause_pending = false;
        self.playback.tune_station(id);
        self.playback.playing = false;
        if let Some(audio) = self.audio.as_mut() {
            match audio.load_remote_and_autoplay(&stream_url, &mut self.media) {
                Ok(generation) => {
                    self.sync_playback_generation_from_media();
                    self.mpris_source = Some(MprisSourceIdentity::Radio {
                        generation: generation.raw(),
                        station_uuid,
                    });
                }
                Err(audio::LoadRemoteError::Url(verdict)) => {
                    self.post_folder_status(
                        format!("Radio station URL blocked by policy: {verdict:?}"),
                        ToastTone::Warning,
                    );
                }
                Err(audio::LoadRemoteError::Load(refused)) => {
                    eprintln!("music audio: radio load refused: {refused}");
                }
            }
        }
    }

    /// Submit a feed fetch to the bounded blocking pool; refusal completes
    /// through the same generation path immediately.
    pub(crate) fn start_podcast_fetch(&mut self, url: String) {
        let request = self.podcast_requests.begin(url);
        let completion_url = request.url.clone();
        let generation = request.generation;
        if let Err(message) = self.podcast_service.submit(request) {
            self.apply_podcast_completion(podcasts::FetchResult {
                url: completion_url,
                generation,
                result: Err(message),
            });
        }
    }

    pub(crate) fn apply_podcast_completion(&mut self, completion: podcasts::FetchResult) {
        if !self.podcast_requests.accept(&completion) {
            return;
        }
        if let Some(feed) = self
            .subscriptions
            .iter_mut()
            .find(|feed| feed.feed_url == completion.url)
        {
            self.persistence_dirty = true;
            match completion.result {
                Ok(fetched) => {
                    feed.title = if fetched.title.trim().is_empty() {
                        feed.feed_url.clone()
                    } else {
                        fetched.title
                    };
                    feed.description = fetched.description;
                    // The feed's own cover, author and site, when it has
                    // them (a restored subscription keeps none of these).
                    // A cover discovery already fetched stays (the feed's is
                    // often a 3000-pixel original).
                    if feed.artwork_url.is_none() {
                        feed.artwork_url = fetched.artwork_url;
                    }
                    if !fetched.author.trim().is_empty() {
                        feed.author = fetched.author;
                    }
                    if !fetched.link.trim().is_empty() {
                        feed.link = fetched.link;
                    }
                    feed.episodes = fetched.episodes;
                    feed.status = podcasts::FeedStatus::Loaded;
                }
                Err(message) => feed.status = podcasts::FeedStatus::Failed(message),
            }
        } else if let Some(preview) = self
            .podcast_charts
            .preview
            .as_mut()
            .filter(|p| p.feed_url == completion.url)
        {
            // A podcast opened without subscribing.
            match completion.result {
                Ok(fetched) => {
                    let seeded_art = preview.artwork_url.take();
                    *preview = podcasts::PodcastFeed {
                        status: podcasts::FeedStatus::Loaded,
                        ..fetched
                    };
                    if preview.title.trim().is_empty() {
                        preview.title = preview.feed_url.clone();
                    }
                    if seeded_art.is_some() {
                        preview.artwork_url = seeded_art;
                    }
                }
                Err(message) => preview.status = podcasts::FeedStatus::Failed(message),
            }
        }
        // Refreshed feeds may change artwork URLs.
        self.reconcile_and_dispatch_artwork();
    }

    /// Queue each restored subscription once; repeated calls while loading do
    /// nothing.
    pub(crate) fn start_restored_podcast_fetches(&mut self) -> usize {
        let urls: Vec<String> = self
            .subscriptions
            .iter_mut()
            .filter(|feed| !matches!(feed.status, podcasts::FeedStatus::Loading))
            .map(|feed| {
                feed.status = podcasts::FeedStatus::Loading;
                feed.feed_url.clone()
            })
            .collect();
        let count = urls.len();
        for url in urls {
            self.start_podcast_fetch(url);
        }
        count
    }

    /// Return search to `Idle`, invalidate in-flight replies, and prune
    /// result-only artwork while keeping subscription covers.
    pub(crate) fn retire_podcast_search(&mut self) {
        self.podcast_search_state = PodcastSearchState::Idle;
        self.podcast_search_generation = self.podcast_search_generation.wrapping_add(1);
        self.podcast_search
            .cancel_older_than(self.podcast_search_generation);
        self.reconcile_and_dispatch_artwork();
    }

    /// Dispatch a discovery search for the current query.
    ///
    /// An empty query retires the search. An unavailable worker pool reports
    /// `Unavailable`; older searches are cancelled and a submit failure
    /// completes through the same path, so the UI always reaches a status.
    pub(crate) fn dispatch_podcast_search(&mut self) {
        let query = self.podcast_search_query.trim().to_string();
        if query.is_empty() {
            // An empty submit owns the search surface exactly like Clear.
            self.retire_podcast_search();
            return;
        }
        let unavailable = self.podcast_search.unavailable_reason().map(str::to_string);
        if let Some(reason) = unavailable {
            self.podcast_search_state = PodcastSearchState::Unavailable(reason);
            return;
        }
        self.podcast_search_generation = self.podcast_search_generation.wrapping_add(1);
        let generation = self.podcast_search_generation;
        let request = podcasts::search::build_request(
            generation,
            &query,
            podcasts::search::SEARCH_ENDPOINT_TEMPLATE,
        );
        self.podcast_search.cancel_older_than(generation);
        self.podcast_search_state = PodcastSearchState::Searching;
        if let Err(message) = self.podcast_search.submit(request) {
            self.apply_podcast_search_response(podcasts::search::SearchResponse {
                generation,
                result: Err(podcasts::search::SearchError {
                    message,
                    retryable: true,
                    kind: podcasts::search::SearchErrorKind::Transport,
                }),
            });
        }
    }

    /// Fold one discovery completion into the search state; stale
    /// generations are ignored.
    pub(crate) fn apply_podcast_search_response(
        &mut self,
        response: podcasts::search::SearchResponse,
    ) {
        if response.generation < self.podcast_search_generation {
            return;
        }
        self.podcast_search_state = match response.result {
            Ok(results) => PodcastSearchState::Results(results),
            Err(err) => PodcastSearchState::Error {
                message: err.message,
                retryable: err.retryable,
            },
        };
        self.reconcile_and_dispatch_artwork();
    }
}

/// Whether `error` is a command (play, rate, effects) the backend refused
/// because no pipeline is live for the generation, as opposed to a refused
/// source. The framework names only the former "no live audio pipeline".
fn is_command_refusal(error: &enzui_services::media::MediaError) -> bool {
    matches!(error.kind, enzui_services::media::MediaErrorKind::Refused)
        && error.message.contains("no live audio pipeline")
}

/// A short, plain reason a stream could not play, for its row.
fn stream_failure_reason(error: &enzui_services::media::MediaError) -> String {
    use enzui_services::media::MediaErrorKind;
    match error.kind {
        MediaErrorKind::UnsupportedFormat => "Unsupported stream format".to_owned(),
        MediaErrorKind::SourceNotFound => "Stream not found".to_owned(),
        _ => "Can't connect to this stream".to_owned(),
    }
}

/// How close playback must come to a released seek's target, in seconds.
const SEEK_SETTLED_WITHIN: f64 = 1.0;
