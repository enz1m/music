//! Boot constructors through settled boot.

use super::*;

impl MusicApp {
    /// The application's boot: [`Self::boot_from_env`], then the enzui-center
    /// connector when this build opted in.
    pub(crate) fn boot_persisting() -> (Self, Task<Message>) {
        let boot = Self::boot_from_env();
        #[cfg(feature = "center")]
        let boot = {
            let (mut app, task) = boot;
            app.start_center();
            (app, task)
        };
        boot
    }

    /// The shipping boot before the connector.
    ///
    /// A declared `MUSIC_APP_LIBRARY_DIR` is an isolated capture fixture that
    /// neither loads nor saves persisted state. Otherwise the launch paints an
    /// empty library immediately and settles state resolve and read off-thread
    /// under a generation guard ([`Message::BootStateSettled`] /
    /// [`Self::apply_boot_document`]); a fixture capture or the settle gate uses
    /// the synchronous [`Self::boot_persisting_with`].
    ///
    /// A missing or relative data home, or an unsafe app root, disables saving
    /// with a path-free notice and never falls back to a shared `/tmp`.
    pub(crate) fn boot_from_env() -> (Self, Task<Message>) {
        let scan_settled = scan_settled_env(std::env::var("MUSIC_APP_SCAN_SETTLED").ok());
        let start_playing = scan_settled_env(std::env::var("MUSIC_APP_START_PLAYING").ok());
        let env = LibraryEnv::from_env();
        let resolver = enzui_core::app_data::AppDataResolver::from_env();
        if env.fixture.is_some() {
            // Capture fixture: persistence is short-circuited, so the resolver
            // and state path are unused.
            return Self::boot_persisting_with(
                scan_settled,
                start_playing,
                &env,
                std::path::PathBuf::new(),
            );
        }
        if scan_settled {
            // Settle gate: resolve synchronously so a capture shot is taken
            // against a deterministic frame.
            let boot = match persistence::resolve_state_location(&resolver) {
                Ok(path) => Self::boot_persisting_with(scan_settled, start_playing, &env, path),
                Err(refusal) => {
                    Self::boot_persistence_refused(scan_settled, start_playing, &env, refusal)
                }
            };
            return Self::start_mpris_after_boot(boot);
        }

        let root = resolve_library_root_now(&env);
        // `boot_tasks` still carries theme init and boot modal focus; only the
        // folder scan and radio fetch wait for the settlement.
        let (mut app, boot_tasks) = Self::boot_with(scan_settled, start_playing, Vec::new());
        app.fallback_folders = root.folders();
        // Bring the audio backend up now so the settlement's
        // `arm_restored_paused_source` has a pipeline to load into.
        app.try_spawn_audio();
        // Resolve/ensure and the bounded read/parse run on a dedicated worker,
        // never on the async executor; the settlement stays generation-guarded.
        let load = app.dispatch_boot_load(resolver);
        let mpris = app.start_mpris();
        (app, Task::batch([boot_tasks, load, mpris]))
    }

    fn start_mpris_after_boot((mut app, task): (Self, Task<Message>)) -> (Self, Task<Message>) {
        let mpris = app.start_mpris();
        (app, Task::batch([task, mpris]))
    }

    pub(crate) fn start_mpris(&mut self) -> Task<Message> {
        let Some((task, delivery)) = self.mpris.start() else {
            return Task::none();
        };
        Task::perform(
            async move {
                let _settlement = task.await;
                delivery
            },
            Message::MprisConnectSettled,
        )
    }

    /// Dispatch the bounded off-thread boot-state load.
    ///
    /// One dedicated worker performs the blocking resolve and bounded read; a
    /// second dispatch while one is in flight is refused. The report applies
    /// only when its generation still matches ([`Message::BootStateSettled`]),
    /// so a stale worker installs nothing.
    pub(crate) fn dispatch_boot_load(
        &mut self,
        resolver: enzui_core::app_data::AppDataResolver,
    ) -> Task<Message> {
        if self.boot_load_worker_spawned {
            return Task::none();
        }
        self.boot_load_worker_spawned = true;
        self.boot_load_generation = self.boot_load_generation.wrapping_add(1);
        let generation = self.boot_load_generation;
        // The future settles exactly once; dropping it cancels the generation,
        // but the running read keeps its slot until it exits.
        let task = self.boot_load_owner.submit(generation, move |context| {
            if context.is_cancelled() {
                return BootStateReport::root_refused(
                    generation,
                    persistence::RootRefusal::Unusable,
                );
            }
            match persistence::resolve_state_location(&resolver) {
                Ok(path) => {
                    let (doc, origin, authority, retire_identity) = persistence::acquire(&path);
                    BootStateReport {
                        generation,
                        path,
                        doc,
                        origin,
                        authority: authority.map(|a| std::sync::Arc::new(std::sync::Mutex::new(a))),
                        retire_identity,
                    }
                }
                // A refused root settles visibly with no worker read.
                Err(refusal) => BootStateReport::root_refused(generation, refusal),
            }
        });
        Task::perform(task, move |settlement| {
            let report = settlement.into_result().unwrap_or_else(|_| {
                // Cancelled, shut down, refused for capacity, or the worker
                // panicked: settle to a disabled read-only session.
                BootStateReport::root_refused(generation, persistence::RootRefusal::Unusable)
            });
            Message::BootStateSettled(Box::new(report))
        })
    }

    /// Shipping startup with ambient inputs supplied as data, keeping the
    /// capture fixture exclusive even when a populated state file exists.
    pub(crate) fn boot_persisting_with(
        scan_settled: bool,
        start_playing: bool,
        env: &LibraryEnv,
        state_path: std::path::PathBuf,
    ) -> (Self, Task<Message>) {
        let root = resolve_library_root_now(env);
        if env.fixture.is_some() {
            if let LibraryRoot::MissingFixture(path) = &root {
                eprintln!("{}", missing_fixture_report(path));
            }
            return Self::boot_with(scan_settled, start_playing, root.folders());
        }

        let (doc, mut persist, origin) = persistence::open_at(&state_path);
        let folders = doc
            .as_ref()
            .map(|saved| saved.folders.clone())
            .unwrap_or_else(|| root.folders());
        let (mut app, scan) = Self::boot_with(scan_settled, start_playing, folders);
        if let Some(doc) = &doc {
            persistence::restore(&mut app, doc);
            // Episodes are not persisted; refresh each restored subscription
            // once through the bounded managed service.
            app.start_restored_podcast_fetches();
        }
        // Settle-gate path: every contributing track is already scanned, so
        // finalize now. Stats go first so the resumed now-playing row shows its
        // Most-Played rank on the first frame. The async path defers both until
        // the last initial root's completion.
        if app.initial_scan_roots.is_empty() {
            app.finalize_pending_track_stats();
            app.finalize_pending_playback_session();
        }
        // A document loaded from disk is already the baseline. Rebaselining to
        // `bounded_half(&app)` would record restore-time cleanup (filtered
        // favourites, dropped missing paths) as saved, so `Persist::prepare`
        // would never rewrite the stale file. Only the no-bytes path primes the
        // baseline, so open-and-close does not create a state file.
        if doc.is_none() {
            persist.rebaseline(&app);
        }
        app.persist = persist;
        app.persistence_notice = Self::notice_for_origin(origin);

        // Only the shipping boot starts audio; test boots must not spawn a
        // GStreamer worker. A `MediaError` leaves `app.audio == None` and the
        // transport shows a disclosure instead of pretending to play.
        app.try_spawn_audio();

        // Load a restored current track paused (session restore is load+seek,
        // never autoplay) so a later Play has a source.
        app.arm_restored_paused_source();

        // The initial catalog fetch is a one-shot `Task::done`, so it arrives
        // as an ordinary reducer message on the first frame.
        let radio_boot = Task::done(Message::RadioCatalogLoad);
        (app, Task::batch([scan, radio_boot]))
    }

    /// Synchronous disabled-persistence boot for a refused data root on the
    /// settle-gate path, matching the async refused branch: the fallback
    /// library is scanned, one path-free notice shows, and no save is written
    /// for the session.
    fn boot_persistence_refused(
        scan_settled: bool,
        start_playing: bool,
        env: &LibraryEnv,
        refusal: persistence::RootRefusal,
    ) -> (Self, Task<Message>) {
        let root = resolve_library_root_now(env);
        let origin = persistence::Origin::PersistenceRootUnavailable(refusal);
        let (mut app, scan) = Self::boot_with(scan_settled, start_playing, root.folders());
        app.persist = persistence::persist_for(&std::path::PathBuf::new(), None, &origin);
        app.persistence_notice = Self::notice_for_origin(origin);
        if app.initial_scan_roots.is_empty() {
            app.finalize_pending_track_stats();
            app.finalize_pending_playback_session();
        }
        app.try_spawn_audio();
        app.arm_restored_paused_source();
        (
            app,
            Task::batch([
                scan,
                Task::done(Message::RadioCatalogLoad),
                Task::done(Message::Spotify(crate::spotify::Event::Restore)),
            ]),
        )
    }

    /// Apply a settled boot-state report on the responsive boot path.
    ///
    /// Restores the document or the fresh-install fallback, installs the
    /// session's persistence and notice, and dispatches each configured root's
    /// initial bounded scan. Returns the boot tasks. Play stats and the
    /// transport session finalize here only when no initial scan is in flight;
    /// otherwise the [`Message::RootScanComplete`] that drains the last initial
    /// root resolves them.
    pub(crate) fn apply_boot_document(&mut self, report: BootStateReport) -> Task<Message> {
        let BootStateReport {
            path,
            doc,
            origin,
            authority,
            retire_identity,
            ..
        } = report;

        let mut persist = persistence::persist_for(&path, doc.as_ref(), &origin);
        // Install the identity-bound authority acquired for this exact
        // generation; without it the enabled session stays read-only.
        if let Some(authority) = authority {
            persist.install_authority(authority);
        }
        // An explicit Retire quarantines exactly this inode.
        persist.install_retire_identity(retire_identity);
        if let Some(doc) = &doc {
            persistence::restore(self, doc);
            self.start_restored_podcast_fetches();
        } else {
            // No document: scan the fresh-install fallback and prime the
            // baseline so a mere open-and-close does not manufacture a file.
            self.library.folders = std::mem::take(&mut self.fallback_folders);
            persist.rebaseline(self);
        }
        self.persist = persist;
        self.persistence_notice = Self::notice_for_origin(origin);

        let now = std::time::Instant::now();
        for root in self.library.folders.clone() {
            self.library_watch.register_root(root, now);
        }
        let dispatches = self.library_watch.dispatch_due(now);
        self.initial_scan_roots = dispatches.iter().map(|d| d.root.clone()).collect();
        let scan = self.dispatch_root_scans(dispatches);

        if self.initial_scan_roots.is_empty() {
            self.finalize_pending_track_stats();
            self.finalize_pending_playback_session();
        }
        // Audio is spawned once on the responsive boot before this settles; here
        // only the restored paused source is armed (a no-op without a backend).
        self.arm_restored_paused_source();

        let opened = self.pump_desktop_paths();
        Task::batch([
            scan,
            opened,
            Task::done(Message::RadioCatalogLoad),
            Task::done(Message::Spotify(crate::spotify::Event::Restore)),
        ])
    }

    /// Try to spawn the framework audio service.
    ///
    /// `Ok` populates [`Self::audio`] and [`Self::audio_receiver`]; `Err`
    /// leaves both `None` and prints one diagnostic, and the app keeps running
    /// with the disclosure banner.
    pub(crate) fn try_spawn_audio(&mut self) {
        // Block private ranges, credentials and non-global destinations before
        // DNS; the worker's gate revalidates every resolved address and
        // redirect. Cleartext is allowed: most internet radio (Icecast,
        // Shoutcast) and many podcast hosts still serve plain HTTP.
        let policy = enzui_core::url_policy::UrlPolicy {
            allow_http: true,
            ..enzui_core::url_policy::UrlPolicy::DEFAULT
        };
        match audio::Playback::spawn(policy) {
            Ok((playback, receiver)) => {
                self.audio = Some(playback);
                self.audio_receiver = Some(receiver);
            }
            Err(err) => {
                // Escape terminal controls before logging; the message is
                // backend-supplied.
                let diagnostic: String =
                    err.message.chars().flat_map(char::escape_default).collect();
                eprintln!(
                    "music audio backend unavailable: kind={:?} message={}",
                    err.kind, diagnostic
                );
                self.audio = None;
                self.audio_receiver = None;
            }
        }
    }

    pub(crate) fn drain_mpris_calls(&mut self) -> Task<Message> {
        let calls = self.mpris.drain_calls();
        let mut tasks = Vec::new();
        for call in calls {
            match self.mpris.translate(&call, &self.media.snapshot()) {
                Ok(command) => tasks.push(self.route_mpris_command(command)),
                Err(refusal) => self.mpris.refuse(&call, refusal),
            }
        }
        Task::batch(tasks)
    }

    fn route_mpris_command(
        &mut self,
        command: enzui_services::media::MediaCommand,
    ) -> Task<Message> {
        use enzui_services::media::MediaCommand;
        // Spotify, while it is the player's source.
        if let Some(remote) = self.spotify.remote() {
            use crate::spotify::Event;
            let playing = remote.playing;
            let event = match command {
                MediaCommand::Play if !playing => Some(Event::TogglePlay),
                MediaCommand::Pause | MediaCommand::Stop if playing => Some(Event::TogglePlay),
                MediaCommand::Play | MediaCommand::Pause | MediaCommand::Stop => None,
                MediaCommand::NextTrack => Some(Event::Next),
                MediaCommand::PreviousTrack => Some(Event::Previous),
                MediaCommand::Seek(position) => Some(Event::Seek(position.as_secs_f64())),
                MediaCommand::SetVolume(volume) => Some(Event::Volume(volume)),
                MediaCommand::SetMute(_) => Some(Event::ToggleMute),
                MediaCommand::ToggleShuffle => Some(Event::Shuffle),
                MediaCommand::CycleRepeat => Some(Event::CycleRepeat),
                MediaCommand::SetRate(_) => None,
            };
            return match event {
                Some(event) => self.update_inner(Message::Spotify(event)),
                None => Task::none(),
            };
        }
        match command {
            MediaCommand::Play => self.request_play(),
            MediaCommand::Pause => self.request_pause(),
            MediaCommand::Stop => self.request_stop(),
            MediaCommand::NextTrack => return self.reduce_batch1(Message::NextTrack),
            MediaCommand::PreviousTrack => return self.reduce_batch1(Message::PreviousTrack),
            MediaCommand::Seek(position) => {
                if self.audio.is_some() {
                    self.request_seek(position);
                }
            }
            MediaCommand::SetVolume(volume) => {
                if self.audio.is_some() {
                    self.request_volume(volume);
                }
            }
            MediaCommand::SetMute(muted) => {
                if self.audio.is_some() {
                    self.request_mute(muted);
                }
            }
            MediaCommand::SetRate(rate) => self.request_playback_rate(rate),
            MediaCommand::ToggleShuffle => return self.reduce_batch1(Message::ToggleShuffle),
            MediaCommand::CycleRepeat => return self.reduce_batch1(Message::CycleRepeat),
        }
        Task::none()
    }

    pub(crate) fn request_play(&mut self) {
        self.sleep_pause_pending = false;
        if self.playback.stream_selection().is_none()
            && let Some(id) = self.playback.current_track_id
            && self.refuse_unplayable_activation(id)
        {
            return;
        }
        let snapshot = self.media.snapshot();
        let ended = snapshot.playback.is_terminal();
        let reload = ended
            || snapshot.error.as_ref().is_some_and(|error| {
                error.kind != enzui_services::media::MediaErrorKind::SeekFailed
            });
        if self.audio.is_some() && reload {
            if let Some(id) = self.playback.current_track_id {
                let resume = if ended {
                    0.0
                } else {
                    self.playback.position_secs
                };
                self.playback.load_current_track(id, &self.library);
                self.playback.position_secs = resume;
                self.route_local_track_to_audio(id, true);
            } else if let Some(stream) = self.playback.stream_selection().cloned() {
                match stream {
                    StreamSelection::Station(id) => self.tune_radio_station(id),
                    StreamSelection::Episode { feed_url, guid } => {
                        let _ = self.update_inner(Message::PodcastEpisodeSelect(feed_url, guid));
                    }
                }
            }
            return;
        }
        if let Some(audio) = self.audio.as_ref()
            && audio.play(&mut self.media).is_err()
        {
            eprintln!("music audio: play command refused");
        }
    }

    pub(crate) fn request_pause(&mut self) {
        if let Some(audio) = self.audio.as_ref()
            && audio.pause(&mut self.media).is_err()
        {
            eprintln!("music audio: pause command refused");
        }
    }

    fn request_stop(&mut self) {
        if let Some(audio) = self.audio.as_ref()
            && audio.stop(&mut self.media).is_err()
        {
            eprintln!("music audio: stop command refused");
        }
    }

    pub(crate) fn request_seek(&mut self, position: std::time::Duration) {
        if let Some(audio) = self.audio.as_ref() {
            if audio.seek(position, &mut self.media).is_err() {
                eprintln!("music audio: seek command refused");
            }
        } else {
            self.playback.position_secs = position.as_secs_f64();
            self.mark_persistence_dirty();
        }
    }

    pub(crate) fn request_volume(&mut self, volume: f64) {
        let volume = if volume.is_nan() {
            0.0
        } else {
            volume.clamp(0.0, 1.0)
        };
        // The slider follows the hand at once; the pipeline gets the value
        // now if it exists, else as soon as it does.
        self.playback.volume = volume as f32;
        if volume > 0.0 {
            self.playback.muted = false;
        }
        self.mark_persistence_dirty();
        if self.audio_pipeline_live()
            && let Some(audio) = self.audio.as_ref()
        {
            self.volume_pending = Some(volume as f32);
            if audio.set_volume(volume, &mut self.media).is_err() {
                eprintln!("music audio: volume command refused");
            }
            if self.media.snapshot().muted && audio.set_mute(false, &mut self.media).is_err() {
                eprintln!("music audio: unmute command refused");
            }
        }
    }

    pub(crate) fn request_mute(&mut self, muted: bool) {
        self.playback.muted = muted;
        self.mark_persistence_dirty();
        if self.audio_pipeline_live()
            && let Some(audio) = self.audio.as_ref()
            && audio.set_mute(muted, &mut self.media).is_err()
        {
            eprintln!("music audio: mute command refused");
        }
    }

    /// Whether the current source's audio pipeline exists, so controls can
    /// reach it.
    pub(crate) fn audio_pipeline_live(&self) -> bool {
        self.audio.is_some() && self.audio_live_generation == Some(self.media.generation().raw())
    }

    fn mpris_navigation(&self) -> enzui_services::media::MprisNavigation {
        if self.playback.stream_selection().is_some() || self.playback.current_track_id.is_none() {
            return enzui_services::media::MprisNavigation::NONE;
        }
        let current = self.playback.queue_index;
        let can_go_next = self
            .playback
            .present_index_forward(
                &self.library,
                matches!(self.playback.repeat, RepeatMode::All),
            )
            .is_some_and(|index| index != current);
        let can_go_previous = self
            .playback
            .present_index_backward(&self.library, true)
            .is_some_and(|index| index != current);
        enzui_services::media::MprisNavigation::new(can_go_next, can_go_previous)
    }

    fn mpris_title(&self, generation: u64) -> Option<String> {
        let identity = self
            .mpris_source
            .as_ref()
            .filter(|identity| identity.generation() == generation)?;
        match identity {
            MprisSourceIdentity::Local {
                track_id,
                occurrence,
                ..
            } => {
                if self.playback.current_track_id != Some(*track_id)
                    || self.playback.current_entry_id() != *occurrence
                {
                    return None;
                }
                self.library
                    .tracks
                    .get(track_id)
                    .map(|track| track.title.clone())
            }
            MprisSourceIdentity::Radio { station_uuid, .. } => {
                if self.radio.current_tuned_uuid.as_ref() != Some(station_uuid) {
                    return None;
                }
                self.radio
                    .by_uuid(station_uuid)
                    .map(|station| station.name.clone())
            }
            MprisSourceIdentity::Episode { feed_url, guid, .. } => {
                if !self.playback.is_current_episode(feed_url, guid) {
                    return None;
                }
                self.episode(feed_url, guid)
                    .map(|episode| episode.title.clone())
            }
        }
    }

    pub(crate) fn mpris_properties(&self) -> enzui_services::media::MprisProperties {
        let snapshot = self.media.snapshot();
        let mut properties = enzui_services::media::MprisProperties::from_snapshot_with_navigation(
            &snapshot,
            self.mpris_navigation(),
        );
        properties.title = self.mpris_title(snapshot.generation.raw());
        // Spotify, while it is the player's source.
        if let Some(remote) = self.spotify.remote() {
            use enzui_services::media::PlaybackStatus;
            properties.playback_status = if remote.playing {
                PlaybackStatus::Playing
            } else {
                PlaybackStatus::Paused
            };
            properties.position_micros = (remote.position * 1_000_000.0) as i64;
            properties.length_micros = Some((remote.duration * 1_000_000.0) as i64);
            if let Some(volume) = remote.volume {
                properties.volume = volume;
            }
            properties.muted = false;
            properties.can_seek = true;
            properties.can_control = true;
            properties.can_go_next = true;
            properties.can_go_previous = true;
            properties.title = Some(format!(
                "{} \u{2014} {}",
                remote.track.name,
                remote.track.artist_names()
            ));
        }
        properties
    }

    pub(crate) fn publish_mpris_properties(&mut self) {
        if self
            .media
            .snapshot()
            .error
            .as_ref()
            .is_some_and(|error| error.kind == enzui_services::media::MediaErrorKind::DeviceLost)
        {
            self.mpris.begin_close();
            self.mpris_source = None;
            return;
        }
        let properties = self.mpris_properties();
        self.mpris.publish(properties);
    }

    /// Arrange for the pipeline to hold the restored current track once its
    /// owning-root scan proves it `Present`.
    ///
    /// An already `Present` track loads paused now. Otherwise a bounded
    /// [`RestoreIntent`] is recorded under a single-use session token and
    /// [`Self::resolve_restore_intent`] fires or retires it on reconcile.
    /// Opening an unverified file would bypass the availability authority.
    /// No current track or an exhausted token space admits no restore.
    pub(crate) fn arm_restored_paused_source(&mut self) {
        let Some(id) = self.playback.current_track_id else {
            return;
        };
        let Some(path) = self.library.tracks.get(&id).map(|t| t.path.clone()) else {
            return;
        };
        if self.library.is_present(id) {
            self.route_local_track_to_audio(id, false);
            return;
        }
        if self.availability_exhausted {
            return;
        }
        // Defer behind a single-use checked session token; a token is never
        // wrapped or reused, so a late reconcile cannot fire a stale restore.
        let Some(session) = self.restore_session.checked_add(1) else {
            return;
        };
        self.restore_session = session;
        self.restore_intent = Some(RestoreIntent {
            track_id: id,
            path,
            playhead: self.playback.position_secs.max(0.0),
            session,
        });
        // The initial scans already reconciled this path, so a terminal state
        // must retire the intent now or it waits forever.
        self.resolve_restore_intent();
    }

    /// Resolve a deferred [`RestoreIntent`] against the current availability of
    /// its exact path. Called after each reconcile:
    ///
    /// * `Present`: fire the paused load at the persisted playhead and clear.
    /// * `Missing`/`Undecodable`/`Unverified`: retire with one bounded, visible
    ///   refusal; the source is not opened.
    /// * `Unknown`: keep waiting.
    ///
    /// A superseded session or replaced row identity drops the intent.
    pub(crate) fn resolve_restore_intent(&mut self) {
        let Some(intent) = self.restore_intent.clone() else {
            return;
        };
        // Stale session or a replaced row identity → inert.
        let identity_live = self
            .library
            .tracks
            .get(&intent.track_id)
            .map(|t| t.path.as_path())
            == Some(intent.path.as_path());
        if intent.session != self.restore_session || !identity_live {
            self.restore_intent = None;
            return;
        }
        match self.library.availability_of_path(&intent.path) {
            domain::TrackAvailability::Present => {
                self.restore_intent = None;
                self.playback.position_secs = intent.playhead.max(0.0);
                self.route_local_track_to_audio(intent.track_id, false);
            }
            domain::TrackAvailability::Missing
            | domain::TrackAvailability::Undecodable
            | domain::TrackAvailability::Unverified => {
                let disclosure = self.library.availability_of_path(&intent.path).disclosure();
                self.restore_intent = None;
                if let Some(text) = disclosure {
                    self.post_folder_status(text, ToastTone::Warning);
                }
            }
            domain::TrackAvailability::Unknown => {
                // Still Checking: the owning-root scan decides.
            }
        }
    }
}
