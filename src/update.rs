//! Update entry, core dispatch, and refresh helpers.

use super::*;

impl MusicApp {
    pub(crate) fn update(&mut self, message: Message) -> Task<Message> {
        // The crash breadcrumb (payload-free kind) is recorded before any early
        // return can refuse or reroute the message.
        enzui_core::crash_report::observe(&message);
        // Wake turns consume their receipt in the arm; ordinary turns drain
        // between-turn actions. An empty batch adds no Task allocation.
        let pending = if matches!(message, Message::A11yWake(_)) {
            Vec::new()
        } else {
            enzui_core::app_runtime::drain_accessibility_actions()
        };
        let was_playing = self.playback.playing;
        let task = self.update_core(message);
        // The library started playing: Spotify steps aside.
        let task = if !was_playing && self.playback.playing {
            Task::batch([task, self.spotify.release().map(Message::Spotify)])
        } else {
            task
        };
        let task = match self.request_waveform() {
            Some(waveform) => Task::batch([task, waveform]),
            None => task,
        };
        self.prune_hidden_notices();
        self.sync_audio_processing();
        self.publish_mpris_properties();
        if pending.is_empty() {
            task
        } else {
            Task::batch([
                enzui_widgets::a11y_actions::apply_accessibility_actions::<Message>(pending),
                task,
            ])
        }
    }

    /// Connector traffic is not user interaction: it must not cancel a
    /// retained close intent.
    #[cfg(feature = "center")]
    fn is_center_traffic(message: &Message) -> bool {
        matches!(message, Message::Center(_))
    }

    #[cfg(not(feature = "center"))]
    fn is_center_traffic(_message: &Message) -> bool {
        false
    }

    fn update_core(&mut self, message: Message) -> Task<Message> {
        if self.close_after_save
            && !matches!(
                &message,
                Message::Window(enzui_widgets::title_bar_gesture::WindowCommand::Close)
                    | Message::RetryPersistence
                    | Message::DiscardUnsavedAndClose
                    | Message::PersistenceFinished { .. }
                    | Message::FilesImported(..)
                    // A retained close is waiting on export publication
                    // debt; these messages settle or retry it rather than
                    // cancelling the close intent.
                    | Message::PlaylistTransferTick
                    | Message::PlaylistTransferRetry
                    | Message::PlaylistImportSettled { .. }
                    | Message::PlaylistExportSettled { .. }
                    // Retirement wakes must not cancel a retained close.
                    | Message::PodcastPoll
                    | Message::RadioCatalogPoll
                    | Message::PodcastSearchPoll
                    | Message::PodcastArtworkPoll
                    | Message::TrackDetailsPoll
                    | Message::MprisConnectSettled(_)
                    | Message::MprisCallWake(_)
                    | Message::MprisLifecycleWake
                    | Message::AudioServiceWake(..)
                    | Message::SleepTick
                    | Message::PlaybackCompleted { .. }
                    | Message::TrackEnded { .. }
            )
            && !Self::is_center_traffic(&message)
        {
            // Any further interaction cancels the pending close intent, so a
            // later autosave cannot make the window disappear unexpectedly.
            self.close_after_save = false;
        }

        // A close is vetoable: no window action is emitted until the newest
        // snapshot is durably written and no export publication is in flight or
        // awaiting retry. Unstarted imports and open export dialogs are cancelled.
        if matches!(
            &message,
            Message::Window(enzui_widgets::title_bar_gesture::WindowCommand::Close)
        ) {
            let persistence_drained = self.drain_persistence_blocking();
            self.begin_playlist_close();
            self.cancel_track_details(track_details::Failure::Cancelled);
            self.close_after_save = true;
            if !persistence_drained || self.has_export_publication_debt() {
                return Task::none();
            }
            // The window closes only once the podcast pool has retired;
            // `maybe_finish_close` vetoes until `close_debt` settles.
            return self.maybe_finish_close().unwrap_or_else(Task::none);
        }

        match message {
            Message::FilesDropped(drop, target) => self.native_files_dropped(drop, target),
            Message::FilesImported(target, known, drop, result) => {
                self.native_imported(drop, target, known, result)
            }
            #[cfg(feature = "center")]
            Message::Center(event) => self.center_event(event),
            Message::MprisConnectSettled(delivery) => {
                self.mpris.finish_connect(&delivery);
                if let Some(close) = self.maybe_finish_close() {
                    return close;
                }
                Task::none()
            }
            Message::MprisLifecycleWake => {
                self.mpris.poll_lifecycle();
                if let Some(close) = self.maybe_finish_close() {
                    return close;
                }
                Task::none()
            }
            Message::PersistenceFinished {
                generation,
                outcome,
            } => {
                let _saved = self.apply_persistence_completion(generation, outcome);
                if let Some(close) = self.maybe_finish_close() {
                    return close;
                }
                Task::none()
            }
            Message::RetryPersistence => self.retry_persistence(),
            Message::RetirePersistence => self.reacquire_authority(true),
            Message::AuthorityReacquired(refresh) => self.apply_authority_refresh(*refresh),
            Message::DiscardUnsavedAndClose => {
                if !self.close_after_save {
                    return Task::none();
                }
                // Register-and-drop a newer ticket so any old async task is
                // stale before the window is allowed to close.
                if let Some(pending) = self.pending_save.take() {
                    let _superseding_ticket = pending.renewed(&self.persistence_queue);
                }
                // An accepted export is external publication debt and must
                // settle before the window can close.
                self.import_job = None;
                if self.has_export_publication_debt() {
                    self.post_folder_status(
                        "Waiting for playlist export to settle before closing.",
                        ToastTone::Warning,
                    );
                    return Task::none();
                }
                self.close_after_save = true;
                self.maybe_finish_close().unwrap_or_else(Task::none)
            }
            other => {
                let refresh_projections = matches!(
                    &other,
                    Message::ScanComplete(_)
                        | Message::RescanComplete(_)
                        | Message::SelectAlbum(_)
                        | Message::SelectArtist(_)
                        | Message::ClearFilter
                        | Message::SetSort(_)
                );
                let task = self.update_inner(other);
                let reset_track_sort =
                    self.current_view != View::Home && self.sort_mode == SortMode::Rating;
                if reset_track_sort {
                    self.sort_mode = SortMode::Album;
                }
                if refresh_projections || reset_track_sort {
                    self.refresh_view_projections();
                }
                self.reconcile_open_context_menu();
                // The single reconcile point for the transient-feedback expiry
                // wake: an unchanged earliest deadline keeps the armed identity,
                // a moved one re-keys it, and a drained host disarms it.
                self.toasts
                    .sync_expiry(&mut self.toast_expiry, std::time::Instant::now());
                // A settled playlist worker may have cleared the last
                // export publication debt a retained close was waiting on.
                if let Some(close) = self.maybe_finish_close() {
                    return close;
                }
                // `Task::batch` allocates, and `perf_tests` pins UI-only
                // messages at zero allocations.
                match self.schedule_persistence() {
                    Some(persistence) => Task::batch([persistence, task]),
                    None => task,
                }
            }
        }
    }

    /// Reconcile the artwork cache to the desired `(identity, url)` set, then
    /// fetch what is missing or stale.
    ///
    /// A slot is retained only while its exact URL is still desired, so an
    /// identity whose artwork was dropped renders absence rather than a stale
    /// image. A subscribed feed owns its channel identity over any search
    /// result, so a shared `feed_url` dispatches at most one request. Episodes
    /// are capped per feed.
    pub(crate) fn reconcile_and_dispatch_artwork(&mut self) {
        // The trimmed URL is both fetch target and freshness key, so trimming
        // must be idempotent; a blank URL prunes the identity.
        fn trimmed_nonempty(url: &Option<String>) -> Option<String> {
            let value = url.as_deref()?.trim();
            (!value.is_empty()).then(|| value.to_string())
        }
        use podcasts::artwork::{ARTWORK_MAX_EPISODES_PER_FEED, ArtworkKey};
        let mut desired: std::collections::HashMap<ArtworkKey, String> =
            std::collections::HashMap::new();
        let mut subscribed: std::collections::HashSet<&str> = std::collections::HashSet::new();
        for feed in &self.subscriptions {
            subscribed.insert(feed.feed_url.as_str());
            if let Some(url) = trimmed_nonempty(&feed.artwork_url) {
                desired.insert(ArtworkKey::channel(&feed.feed_url), url);
            }
            for episode in feed
                .episodes
                .iter()
                .filter(|e| e.artwork_url.is_some())
                .take(ARTWORK_MAX_EPISODES_PER_FEED)
            {
                if let Some(url) = trimmed_nonempty(&episode.artwork_url) {
                    desired.insert(ArtworkKey::episode(&feed.feed_url, &episode.guid), url);
                }
            }
        }
        if let PodcastSearchState::Results(results) = &self.podcast_search_state {
            for result in results {
                // A subscribed feed owns its channel identity, including having
                // no artwork; discovery never fills or overrides it.
                if subscribed.contains(result.feed_url.as_str()) {
                    continue;
                }
                if let Some(url) = trimmed_nonempty(&result.artwork_url) {
                    desired
                        .entry(ArtworkKey::channel(&result.feed_url))
                        .or_insert(url);
                }
            }
        }

        // The previewed podcast's, and the playing preview's, cover and
        // episode art.
        for preview in self
            .podcast_charts
            .unsubscribed_feeds()
            .filter(|p| !subscribed.contains(p.feed_url.as_str()))
        {
            if let Some(url) = trimmed_nonempty(&preview.artwork_url) {
                desired.insert(ArtworkKey::channel(&preview.feed_url), url);
            }
            for episode in preview
                .episodes
                .iter()
                .filter(|e| e.artwork_url.is_some())
                .take(ARTWORK_MAX_EPISODES_PER_FEED)
            {
                if let Some(url) = trimmed_nonempty(&episode.artwork_url) {
                    desired.insert(ArtworkKey::episode(&preview.feed_url, &episode.guid), url);
                }
            }
        }

        // Covers of the charted shows on show (a subscribed feed owns its own).
        if let Some(shows) = self.podcast_charts.current() {
            for show in shows {
                if subscribed.contains(show.feed_url.as_str()) {
                    continue;
                }
                if let Some(url) = trimmed_nonempty(&show.artwork_url) {
                    desired
                        .entry(ArtworkKey::channel(&show.feed_url))
                        .or_insert(url);
                }
            }
        }

        // Station logos for the stations on screen, keyed apart from feeds.
        for station in self
            .radio
            .visible(&self.podcast_charts.country)
            .into_iter()
            .take(RADIO_ART_MAX)
        {
            let url = station.favicon_url.trim();
            if !url.is_empty() {
                desired.insert(
                    ArtworkKey::channel(radio_art_key(&station.remote_uuid)),
                    url.to_owned(),
                );
            }
        }

        // Spotify library thumbnails for the rows on screen.
        for (key, url) in self.spotify.artwork().into_iter().take(SPOTIFY_ART_MAX) {
            desired.insert(ArtworkKey::channel(key), url);
        }

        // Drop every slot whose exact `(key, url)` is no longer desired and
        // retire its in-flight fetch before dispatching new work.
        for (retired_key, _token) in self.podcast_art_cache.reconcile(&desired) {
            self.podcast_artwork.cancel(&retired_key);
        }

        for (key, url) in desired {
            if !self.podcast_art_cache.needs_fetch(&key, &url) {
                continue;
            }
            // A changed URL retires the prior fetch so its reply is discarded.
            self.podcast_artwork.cancel(&key);
            let token = self.podcast_art_cache.allocate_token();
            let request = podcasts::artwork::PodcastArtworkService::build_request(
                key.clone(),
                url.clone(),
                token,
            );
            if self.podcast_artwork.submit(request) {
                self.podcast_art_cache.mark_pending(key, token, url);
            }
        }
    }

    pub(crate) fn refresh_view_projections(&self) {
        let key = (self.library.revision(), self.filter, self.sort_mode);
        if self.projection_key.get() == Some(key) {
            return;
        }
        self.album_projection.replace(views::album_projection(
            &self.library,
            self.filter,
            self.sort_mode,
        ));
        self.artist_projection
            .replace(views::artist_projection(&self.library, self.sort_mode));
        self.projection_key.set(Some(key));
    }

    #[cfg(test)]
    pub(crate) fn step(&mut self, message: Message) {
        let mut pending = std::collections::VecDeque::from([message]);
        let mut budget = 64usize;
        while let Some(message) = pending.pop_front() {
            budget = budget
                .checked_sub(1)
                .expect("MusicApp::step exceeded its message budget: an arm is cycling");
            let task = self.update(message);
            pending.extend(crate::test_support::drain_task(task));
        }
    }

    #[cfg(test)]
    pub(crate) fn with_persistence(mut self, path: std::path::PathBuf) -> Self {
        let (doc, mut persist, origin) = persistence::open_at(&path);
        if let Some(doc) = &doc {
            persistence::restore(&mut self, doc);
        }
        // Rebaseline only when the sandbox held no document, so cleanup
        // mutations against a loaded document stay visible to the detector.
        if doc.is_none() {
            persist.rebaseline(&self);
        }
        self.persist = persist;
        self.persistence_notice = Self::notice_for_origin(origin);
        self.persistence_dirty = false;
        self
    }

    pub(crate) fn notice_for_origin(origin: persistence::Origin) -> Option<PersistenceNotice> {
        match origin {
            persistence::Origin::Missing | persistence::Origin::Restored => None,
            persistence::Origin::ReadOnlyFutureVersion(version) => {
                Some(PersistenceNotice::ReadOnlyFutureVersion(version))
            }
            persistence::Origin::RefusedReadOnly(kind) => {
                Some(PersistenceNotice::RefusedReadOnly(kind))
            }
            persistence::Origin::PersistenceRootUnavailable(refusal) => {
                Some(PersistenceNotice::PersistenceRootUnavailable(refusal))
            }
        }
    }
}

/// How many Spotify thumbnails the Spotify page keeps loaded at once.
const SPOTIFY_ART_MAX: usize = 400;

/// How many station logos the Radio page keeps loaded at once.
const RADIO_ART_MAX: usize = 120;

/// The artwork-cache key of a station's logo: apart from any feed URL.
pub(crate) fn radio_art_key(remote_uuid: &str) -> String {
    format!("radio:{remote_uuid}")
}

/// How many decoded waveforms are kept before the cache starts over.
pub(crate) const WAVEFORM_CACHE: usize = 32;

impl MusicApp {
    /// Start decoding the current track's waveform while the now-playing
    /// panel is open and it has none yet, off the UI thread.
    fn request_waveform(&mut self) -> Option<Task<Message>> {
        // Now Playing shows in the sheet, or always in the split layout.
        if !self.now_playing_open && crate::views::split_widths(self.window_size.width).is_none() {
            return None;
        }
        let id = self.playback.current_track_id?;
        if self.waveforms.contains_key(&id) || self.waveform_pending == Some(id) {
            return None;
        }
        let path = self.library.tracks.get(&id)?.path.clone();
        self.waveform_pending = Some(id);
        Some(Task::perform(
            async move {
                tokio::task::spawn_blocking(move || crate::waveform::extract(&path))
                    .await
                    .ok()
                    .flatten()
                    .map(std::sync::Arc::<[f32]>::from)
            },
            move |peaks| Message::WaveformReady(id, peaks),
        ))
    }
}
