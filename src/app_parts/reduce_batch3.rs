//! Reducer batch 3: ImportPlaylist through EqSetBand.

use super::*;

impl MusicApp {
    pub(crate) fn reduce_batch3(&mut self, message: Message) -> Task<Message> {
        let previous_eq = self.eq;
        match message {
            Message::ImportPlaylist => {
                // Mint and check a generation before any portal work; a request
                // that never opens a dialog is reported at once.
                match self.playlist_transfer.start_import() {
                    Ok(_generation) => {}
                    Err(refusal) => self.report_transfer_refusal(refusal),
                }
            }
            Message::ExportPlaylist(playlist_id) => {
                // Snapshot the exact durable rows and display name BEFORE the
                // portal opens, so a later edit cannot change what is exported.
                let Some(paths) = self.library.playlist_export_paths(playlist_id) else {
                    return Task::none();
                };
                let display_name = self
                    .library
                    .playlists
                    .iter()
                    .find(|p| p.id == playlist_id)
                    .map(|p| p.name.clone())
                    .unwrap_or_default();
                let suggested = suggested_export_name(&display_name);
                match self.playlist_transfer.start_export(suggested) {
                    Ok(generation) => {
                        self.export_jobs.insert(
                            generation,
                            ExportJob {
                                playlist_id,
                                display_name,
                                paths,
                                grant: None,
                                state: JobState::InFlight,
                            },
                        );
                    }
                    Err(refusal) => self.report_transfer_refusal(refusal),
                }
            }
            Message::PlaylistTransferTick => {
                return self.pump_playlist_transfer();
            }
            Message::PlaylistTransferRetry => {
                return self.retry_playlist_transfer();
            }
            Message::PlaylistImportSettled {
                generation,
                settlement,
            } => {
                return self.finish_import(generation, settlement);
            }
            Message::PlaylistExportSettled {
                generation,
                settlement,
            } => {
                self.finish_export(generation, settlement);
            }
            Message::ExpandTrackList(scope) => {
                self.track_list_expansion.set(scope, true);
            }
            Message::SearchQueryChanged(q) => {
                self.search_query = q;
                // A reveal belongs to the result set it was taken against, so a
                // new query collapses the Search section to its first page.
                self.track_list_expansion.reset_search();
                // Typing outside Search navigates into it.
                if self.current_view != View::Search {
                    if self.view_before_modal.is_none() {
                        self.view_before_modal = Some(self.current_view);
                    }
                    self.current_view = View::Search;
                }
            }

            // Podcasts
            // ---- Radio ----
            Message::RadioGenre(genre) => {
                self.radio.genre = genre;
                self.dispatch_catalog_top_list();
                self.reconcile_and_dispatch_artwork();
            }
            Message::RadioSearchChanged(q) => {
                // An empty field leaves the search for the browse lists.
                if q.trim().is_empty() {
                    self.radio.submitted = None;
                    self.radio.results = None;
                }
                self.radio.query = q;
            }
            Message::RadioSearchClear => {
                self.radio.query.clear();
                self.radio.submitted = None;
                self.radio.results = None;
                self.reconcile_and_dispatch_artwork();
            }
            Message::RadioAddName(name) => self.radio.add_name = name.chars().take(128).collect(),
            Message::RadioAddUrl(url) => self.radio.add_url = url.chars().take(2048).collect(),
            Message::RadioAddSubmit => {
                let url = self.radio.add_url.trim().to_owned();
                if radio_catalog::REMOTE_POLICY.check_url(&url).is_err() {
                    self.post_folder_status(
                        "Enter the station's https:// stream address.",
                        ToastTone::Warning,
                    );
                    return Task::none();
                }
                let station = radio::custom_station(&self.radio.add_name, &url);
                if !self
                    .radio
                    .custom
                    .iter()
                    .any(|c| c.remote_uuid == station.remote_uuid)
                {
                    self.radio.custom.push(station);
                    self.mark_persistence_dirty();
                }
                self.radio.add_name.clear();
                self.radio.add_url.clear();
            }
            Message::RadioSearchSubmit => {
                // `submit_search` returns the generation the completion arm
                // compares against.
                let generation = self.radio.submit_search();
                if let Some(query) = self.radio.submitted.clone() {
                    self.dispatch_catalog_search(generation, query);
                }
            }
            Message::RadioToggleFavorite(id) => {
                // Favourites persist by durable `remote_uuid`; an unknown id is a
                // no-op.
                self.radio.toggle_favorite(id);
                self.mark_persistence_dirty();
            }
            Message::RadioTune(id) => {
                // `MediaSource::remote`'s pre-DNS policy and `MediaGate::DEFAULT`
                // reject disallowed URLs. Admission leaves the station Selected
                // and not playing; only the pipeline's `SourceReady → Playing`
                // claims playback (see `MusicApp::drain_audio_events`).
                self.tune_radio_station(id);
            }
            Message::RadioCatalogLoad => {
                // Boot entry for the initial top-list fetch; the service rejects
                // a duplicate in-flight generation.
                self.dispatch_catalog_top_list();
            }
            Message::RadioCatalogRetry => {
                // User retry; same path as `RadioCatalogLoad`.
                self.dispatch_catalog_top_list();
            }
            Message::RadioCatalogPoll => {
                self.drain_radio_catalog_results();
                self.reconcile_and_dispatch_artwork();
                // Advance retirement off the reducer so a close in flight joins
                // the reaper and the poll wake eventually stops.
                let _ = self.radio_catalog_service.shutdown_progress();
                if let Some(close) = self.maybe_finish_close() {
                    return close;
                }
            }
            Message::RadioCatalogLoaded(result) => {
                // Test entry that hands the reducer a pre-built `CatalogResult`
                // through the poll arm's completion path.
                self.apply_radio_catalog_result(*result);
            }

            Message::PodcastSearchInputChanged(v) => {
                self.podcast_search_query = v;
            }
            Message::PodcastSearchSubmit => {
                self.dispatch_podcast_search();
            }
            Message::PodcastSearchPoll => {
                self.drain_podcast_search_results();
                let _ = self.podcast_search.shutdown_progress();
                if let Some(close) = self.maybe_finish_close() {
                    return close;
                }
            }
            Message::PodcastArtworkPoll => {
                for response in self.podcast_artwork.drain() {
                    self.podcast_art_cache.apply(response);
                }
                let _ = self.podcast_artwork.shutdown_progress();
                if let Some(close) = self.maybe_finish_close() {
                    return close;
                }
            }
            Message::PodcastSearchClear => {
                self.podcast_search_query.clear();
                self.retire_podcast_search();
            }
            Message::PodcastSubscribeFromResult(feed_url)
                if self.podcast_charts.preview.as_ref().is_some_and(|p| {
                    p.feed_url == feed_url.trim()
                        && p.status == podcasts::FeedStatus::Loaded
                        && !self.subscriptions.iter().any(|f| f.feed_url == p.feed_url)
                }) =>
            {
                // The opened preview already has its episodes.
                if let Some(preview) = self.podcast_charts.preview.clone() {
                    self.subscriptions.push(preview);
                    self.reconcile_and_dispatch_artwork();
                    self.mark_persistence_dirty();
                }
            }
            Message::PodcastSubscribeFromResult(feed_url) => {
                let url = feed_url.trim().to_string();
                if !url.is_empty() && !self.subscriptions.iter().any(|f| f.feed_url == url) {
                    // Seed the discovered title, author and artwork so the row is
                    // not blank while its RSS fetch runs; the feed overwrites them.
                    // `feed_url` is the durable key shared with URL and OPML
                    // subscribe.
                    let discovered = match &self.podcast_search_state {
                        PodcastSearchState::Results(results) => {
                            results.iter().find(|r| r.feed_url == url).cloned()
                        }
                        _ => None,
                    }
                    .or_else(|| self.podcast_charts.find(&url).cloned());
                    let (title, author, artwork_url) = match discovered {
                        Some(result) => (
                            if result.title.trim().is_empty() {
                                url.clone()
                            } else {
                                result.title
                            },
                            result.author,
                            result.artwork_url,
                        ),
                        None => (url.clone(), String::new(), None),
                    };
                    self.subscriptions.push(podcasts::PodcastFeed {
                        feed_url: url.clone(),
                        title,
                        author,
                        artwork_url,
                        status: podcasts::FeedStatus::Loading,
                        ..podcasts::PodcastFeed::default()
                    });
                    self.mark_persistence_dirty();
                    self.start_podcast_fetch(url);
                    self.reconcile_and_dispatch_artwork();
                }
            }
            Message::PodcastSubscribeInputChanged(v) => {
                self.subscribe_input = v;
            }
            Message::PodcastSubscribeSubmit => {
                let url = self.subscribe_input.trim().to_string();
                if !url.is_empty() && !self.subscriptions.iter().any(|f| f.feed_url == url) {
                    // Subscribe queues a bounded blocking-worker job. The row
                    // stays Loading while the UI remains free to process input;
                    // PodcastPoll drains its generation-stamped completion.
                    self.subscriptions.push(podcasts::PodcastFeed {
                        feed_url: url.clone(),
                        title: url.clone(),
                        description: String::new(),
                        episodes: Vec::new(),
                        status: podcasts::FeedStatus::Loading,
                        ..podcasts::PodcastFeed::default()
                    });
                    self.subscribe_input.clear();
                    self.mark_persistence_dirty();
                    self.start_podcast_fetch(url);
                }
            }
            Message::PodcastFeedFetched(completion) => {
                self.apply_podcast_completion(completion);
            }
            Message::PodcastPoll => {
                for completion in self.podcast_service.drain() {
                    self.apply_podcast_completion(completion);
                }
                // Advance pool retirement whenever a close is in flight, even a
                // cancelled one, so the reaper is joined and the poll wake stops.
                let _ = self.podcast_service.shutdown_progress();
                if let Some(close) = self.maybe_finish_close() {
                    return close;
                }
            }
            Message::PodcastSubscribeRetry(idx) => {
                if let Some(feed) = self.subscriptions.get_mut(idx) {
                    let url = feed.feed_url.clone();
                    feed.status = podcasts::FeedStatus::Loading;
                    self.start_podcast_fetch(url);
                }
            }
            Message::PodcastUnsubscribe(idx) => {
                if idx < self.subscriptions.len() {
                    let url = self.subscriptions[idx].feed_url.clone();
                    self.podcast_requests
                        .cancel(&url, enzui_core::cancellation::CancelCause::Cancelled);
                    let removed = self.subscriptions.remove(idx);
                    if self.podcast_charts.open.as_deref() == Some(url.as_str()) {
                        self.podcast_charts.preview = Some(removed);
                    }
                    // Reconcile drops the feed's cached covers and retires any
                    // in-flight thumbnail fetch.
                    self.reconcile_and_dispatch_artwork();
                    self.mark_persistence_dirty();
                }
            }
            Message::PodcastEpisodeSelect(feed_url, guid) => {
                // Only an existing episode with an enclosure becomes current. The
                // `audio_url` goes through `MediaGate` under the shared
                // `UrlPolicy`, and `load_into`'s per-generation token cancels a
                // superseded selection.
                let episode_url = self
                    .subscriptions
                    .iter()
                    .chain(self.podcast_charts.unsubscribed_feeds())
                    .find(|f| f.feed_url == feed_url)
                    .and_then(|f| {
                        f.playable_episodes()
                            .find(|e| e.guid == guid)
                            .and_then(|e| e.audio_url.clone())
                    });
                let has_url = episode_url.is_some();
                // Playing from a preview: keep that podcast for the player,
                // whatever is opened next.
                if has_url
                    && !self.subscriptions.iter().any(|f| f.feed_url == feed_url)
                    && let Some(preview) = self
                        .podcast_charts
                        .preview
                        .as_ref()
                        .filter(|p| p.feed_url == feed_url)
                {
                    self.podcast_charts.playing_feed = Some(preview.clone());
                }
                if has_url {
                    // Read the resume point before `select_episode` moves the key.
                    // It seeds the playhead only when an audio route exists, so a
                    // backend-less build never shows a fabricated elapsed time.
                    let resume_secs = self
                        .episode_resume
                        .get(&(feed_url.clone(), guid.clone()))
                        .copied()
                        .filter(|position| position.is_finite() && *position > 0.0);
                    let has_audio = self.audio.is_some();
                    self.sleep_pause_pending = false;
                    self.playback.select_episode(feed_url.clone(), guid.clone());
                    // Only the pipeline's state event may set `playing` again.
                    self.playback.playing = false;
                    if has_audio && let Some(secs) = resume_secs {
                        // A non-zero playhead arms the `SourceReady` seek, as for a
                        // restored local track.
                        self.playback.position_secs = secs;
                    }
                    if let (Some(audio), Some(url)) = (self.audio.as_mut(), episode_url) {
                        match audio.load_remote_and_autoplay(&url, &mut self.media) {
                            Ok(generation) => {
                                self.sync_playback_generation_from_media();
                                self.mpris_source = Some(MprisSourceIdentity::Episode {
                                    generation: generation.raw(),
                                    feed_url,
                                    guid,
                                });
                            }
                            Err(err) => {
                                // Clear the seeded playhead; `episode_resume` keeps
                                // the durable point so Retry resumes there.
                                self.playback.position_secs = 0.0;
                                self.playback.duration_secs = 0.0;
                                self.playback.playing = false;
                                let reason = match err {
                                    audio::LoadRemoteError::Url(_) => {
                                        "This episode's address was refused by the \
                                         network policy."
                                    }
                                    audio::LoadRemoteError::Load(_) => {
                                        "This episode could not be opened for playback."
                                    }
                                };
                                self.post_folder_status(reason.to_string(), ToastTone::Warning);
                            }
                        }
                    }
                }
            }

            // Manage Feeds dialog
            Message::OpenManageFeeds => {
                self.manage_feeds_open = true;
                return self.open_modal_focus(
                    ModalPresentationId::ManageFeeds,
                    Some(manage_feeds_button_id()),
                );
            }
            Message::CloseManageFeeds => {
                return self.dismiss_modal_focus(
                    ModalPresentationId::ManageFeeds,
                    enzui_core::modal::DismissCause::CloseButton,
                );
            }
            Message::OpmlImportAction(action) => {
                // The editor owns caret and undo; the reducer applies the action.
                self.opml_import.perform(action);
            }
            Message::OpmlImportSubmit => {
                // Subscribe to each pasted OPML feed, deduplicated by `feed_url`
                // as in `PodcastSubscribeSubmit`.
                let opml = self.opml_import.text();
                let (added, truncated) = self.subscribe_from_opml(&opml);
                // An over-limit OPML document stays visible so its remaining
                // batch can be imported deliberately.
                if added > 0 && !truncated {
                    self.opml_import = text_editor::Content::new();
                }
            }
            Message::OpmlExport => {
                // Snapshot the current subscriptions as OPML for copy.
                self.opml_export = podcasts::export_opml(&self.subscriptions);
            }

            // Equalizer
            Message::ToggleEqualizer => {
                self.eq_open = !self.eq_open;
                return self.toggle_modal_focus(self.eq_open, ModalPresentationId::Equalizer, None);
            }
            Message::EqSetEnabled(on) => {
                self.eq.enabled = on;
            }
            Message::EqSetClipProtection(on) => {
                self.eq.clip_protection = on;
            }
            Message::EqPresetMenu(open) => {
                self.eq_preset_menu_open = open;
            }
            Message::EqSelectPreset(index) => {
                self.eq_preset_menu_open = false;
                if let Some(&preset) = EqPreset::SELECTABLE.get(index) {
                    self.eq.apply_preset(preset);
                }
            }
            Message::EqSetBand { index, gain_db } => {
                // Routes through set_band, which re-resolves preset ↔ Custom.
                self.eq.set_band(index, gain_db);
            }

            // ReplayGain / Settings › Playback
            _ => unreachable!("reduce_batch3 handles only its own variants"),
        }
        if self.eq != previous_eq {
            self.mark_persistence_dirty();
        }
        Task::none()
    }
}
