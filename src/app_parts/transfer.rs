//! Playlist transfer, import/export, and library scans.

use super::*;

impl MusicApp {
    /// Report a portal start refusal on the shared library status line.
    pub(crate) fn report_transfer_refusal(&mut self, refusal: playlist_transfer::StartRefusal) {
        use playlist_transfer::StartRefusal;
        let text = match refusal {
            StartRefusal::GenerationsExhausted => {
                "Too many playlist operations this session — please restart.".to_string()
            }
            StartRefusal::Import(outcome) => playlist_transfer::import_failure_text(&outcome)
                .unwrap_or_else(|| "The playlist could not be imported.".to_string()),
            StartRefusal::Export(outcome) => playlist_transfer::export_failure_text(&outcome)
                .unwrap_or_else(|| "The playlist could not be exported.".to_string()),
        };
        self.post_folder_status(text, ToastTone::Warning);
    }

    /// Pump both playlist portals on their completion wake, routing each newly
    /// terminal outcome.
    pub(crate) fn pump_playlist_transfer(&mut self) -> Task<Message> {
        let mut tasks = Vec::new();
        if let Some((generation, outcome)) = self.playlist_transfer.pump_import() {
            tasks.push(self.finish_import_portal(generation, outcome));
        }
        for (generation, outcome) in self.playlist_transfer.pump_export() {
            tasks.push(self.finish_export_portal(generation, outcome));
        }
        Task::batch(tasks)
    }

    /// Route a terminal `OpenFile` outcome: a completed local file becomes the
    /// accepted import and its read/parse is submitted; cancellation is silent and
    /// every failure is a visible refusal.
    fn finish_import_portal(
        &mut self,
        generation: u64,
        outcome: enzui_widgets::file_chooser::PortalOutcome<
            enzui_widgets::file_chooser::FileResult,
        >,
    ) -> Task<Message> {
        use enzui_widgets::file_chooser::PortalOutcome;
        match outcome {
            PortalOutcome::Completed(result) => {
                match playlist_transfer::first_local_file(&result) {
                    Some(path) => self.submit_import_read(generation, path),
                    None => {
                        self.post_folder_status(
                            "That location is not a local playlist file.",
                            ToastTone::Warning,
                        );
                        Task::none()
                    }
                }
            }
            PortalOutcome::Cancelled(_) => Task::none(),
            other => {
                if let Some(text) = playlist_transfer::import_failure_text(&other) {
                    self.post_folder_status(text, ToastTone::Warning);
                }
                Task::none()
            }
        }
    }

    /// Submit the accepted import's read/parse to the owner, recording the job
    /// so a stale settlement is inert and a Busy refusal can be retried.
    fn submit_import_read(&mut self, generation: u64, path: std::path::PathBuf) -> Task<Message> {
        self.import_job = Some(ImportJob {
            generation,
            path: path.clone(),
            state: JobState::InFlight,
        });
        let task = self.playlist_transfer.submit_import(generation, path);
        Task::perform(task, move |outcome| Message::PlaylistImportSettled {
            generation,
            settlement: playlist_transfer::import_settlement(outcome),
        })
    }

    /// Route a terminal `SaveFile` outcome for an export job: a completed grant
    /// is stored and its publication submitted; a cancellation drops the job
    /// inertly; a failure drops the job and is a visible refusal.
    fn finish_export_portal(
        &mut self,
        generation: u64,
        outcome: enzui_core::download_destination::DestinationOutcome,
    ) -> Task<Message> {
        use enzui_core::download_destination::DestinationOutcome;
        match outcome {
            DestinationOutcome::Completed(grant) => {
                let Some(job) = self.export_jobs.get_mut(&generation) else {
                    return Task::none();
                };
                job.grant = Some(grant.clone());
                job.state = JobState::InFlight;
                let paths = job.paths.clone();
                let task = self
                    .playlist_transfer
                    .submit_export(generation, grant, paths);
                Task::perform(task, move |outcome| Message::PlaylistExportSettled {
                    generation,
                    settlement: playlist_transfer::export_settlement(outcome),
                })
            }
            DestinationOutcome::Cancelled(_) => {
                self.export_jobs.remove(&generation);
                Task::none()
            }
            other => {
                self.export_jobs.remove(&generation);
                if let Some(text) = playlist_transfer::export_failure_text(&other) {
                    self.post_folder_status(text, ToastTone::Warning);
                }
                Task::none()
            }
        }
    }

    /// Apply a settled import read/parse; stale generations are inert.
    ///
    /// A complete parse creates and selects one named playlist, keeping missing
    /// durable paths for later reconciliation. A malformed document changes
    /// nothing, and a Busy refusal keeps the intent for one coalesced retry.
    pub(crate) fn finish_import(
        &mut self,
        generation: u64,
        settlement: playlist_transfer::ImportSettlement,
    ) -> Task<Message> {
        use playlist_transfer::ImportSettlement;
        let Some(job) = self.import_job.as_ref() else {
            return Task::none();
        };
        if job.generation != generation {
            // A newer import superseded this one; the stale settlement is inert.
            return Task::none();
        }
        match settlement {
            ImportSettlement::Parsed(report) => {
                let name = self.import_playlist_name(&report);
                let outcome = self.library.import_paths_as_playlist(name, &report);
                self.import_job = None;
                match outcome {
                    Some((id, counts)) => {
                        self.mark_persistence_dirty();
                        self.post_folder_status(import_counts_summary(&counts), ToastTone::Success);
                        self.filter = LibraryFilter::Playlist(id);
                        self.current_view = View::Playlist;
                        self.refresh_view_projections();
                    }
                    None => {
                        self.post_folder_status(
                            "Could not create a playlist — the id space is full.",
                            ToastTone::Warning,
                        );
                    }
                }
                Task::none()
            }
            ImportSettlement::Failed(error) => {
                // A malformed or unreadable document mutates nothing.
                self.import_job = None;
                self.post_folder_status(error.summary(), ToastTone::Warning);
                Task::none()
            }
            ImportSettlement::Busy => {
                // Retain the exact intent; a coalesced wake retries it.
                if let Some(job) = self.import_job.as_mut() {
                    job.state = JobState::AwaitingRetry;
                }
                Task::none()
            }
            ImportSettlement::Aborted => {
                // Cancelled/shutdown: inert, no library change.
                self.import_job = None;
                Task::none()
            }
        }
    }

    /// Apply a settled export publication.
    ///
    /// Only `Published` reports success. `Busy` ran nothing and keeps the job with
    /// its grant for the coalesced retry. `DurabilityUncertain` means the rename
    /// landed but its sync was unconfirmed, so the job clears with a caveat. Other
    /// failures clear the job, since the no-clobber reservation is not safe to
    /// re-drive, and ask for a re-export. `Aborted` drops the job silently.
    pub(crate) fn finish_export(
        &mut self,
        generation: u64,
        settlement: playlist_transfer::ExportSettlement,
    ) {
        use playlist_transfer::{ExportPublishError, ExportSettlement};
        if !self.export_jobs.contains_key(&generation) {
            // A stale settlement for a job already published or discarded.
            return;
        }
        match settlement {
            ExportSettlement::Published => {
                let name = self
                    .export_jobs
                    .remove(&generation)
                    .map(|j| j.display_name)
                    .unwrap_or_default();
                self.post_folder_status(format!("Exported {name}"), ToastTone::Success);
            }
            ExportSettlement::Busy => {
                // The publication never ran; retain the exact intent for the one
                // coalesced retry wake.
                if let Some(job) = self.export_jobs.get_mut(&generation) {
                    job.state = JobState::AwaitingRetry;
                }
            }
            ExportSettlement::Failed(error) => {
                let durability_unconfirmed =
                    matches!(error, ExportPublishError::DurabilityUncertain(_));
                let name = self
                    .export_jobs
                    .remove(&generation)
                    .map(|j| j.display_name)
                    .unwrap_or_default();
                if durability_unconfirmed {
                    self.post_folder_status(
                        format!("Exported {name}, but its durability could not be confirmed."),
                        ToastTone::Warning,
                    );
                } else {
                    self.post_folder_status(error.summary(), ToastTone::Warning);
                }
            }
            ExportSettlement::Aborted => {
                // Cancelled/shutdown: drop the job inertly.
                self.export_jobs.remove(&generation);
            }
        }
    }

    /// Re-submit a retained import/export intent from the coalesced retry wake,
    /// the only path besides a later owner settlement that drives a retry.
    pub(crate) fn retry_playlist_transfer(&mut self) -> Task<Message> {
        let mut tasks = Vec::new();
        // Take the intent out first so no borrow of `self.import_job` spans the
        // resubmission.
        let import_retry = self
            .import_job
            .as_ref()
            .filter(|j| j.state == JobState::AwaitingRetry)
            .map(|j| (j.generation, j.path.clone()));
        if let Some((generation, path)) = import_retry {
            tasks.push(self.submit_import_read(generation, path));
        }
        // Export retry: every awaiting-retry job that still holds its grant.
        let retry_exports: Vec<(u64, ExportJob)> = self
            .export_jobs
            .iter()
            .filter(|(_, j)| j.grant.is_some() && j.state == JobState::AwaitingRetry)
            .map(|(g, j)| (*g, j.clone()))
            .collect();
        for (generation, job) in retry_exports {
            if let Some(grant) = job.grant.clone() {
                if let Some(slot) = self.export_jobs.get_mut(&generation) {
                    slot.state = JobState::InFlight;
                }
                let task =
                    self.playlist_transfer
                        .submit_export(generation, grant, job.paths.clone());
                tasks.push(Task::perform(task, move |outcome| {
                    Message::PlaylistExportSettled {
                        generation,
                        settlement: playlist_transfer::export_settlement(outcome),
                    }
                }));
            }
        }
        Task::batch(tasks)
    }

    /// The auto-generated name for an imported playlist: the file stem when it
    /// is a usable non-empty name, else a bounded fallback.
    fn import_playlist_name(&self, report: &playlist_io::ParseReport) -> String {
        let _ = report;
        let stem = self
            .import_job
            .as_ref()
            .and_then(|j| j.path.file_stem())
            .and_then(|s| s.to_str())
            .map(str::trim)
            .filter(|s| !s.is_empty());
        let name = stem.unwrap_or("Imported playlist");
        let bounded: String = name
            .chars()
            .map(|c| if c.is_control() { '_' } else { c })
            .take(domain::MAX_PLAYLIST_NAME_LEN)
            .collect();
        let bounded = bounded.trim();
        if bounded.is_empty() {
            "Imported playlist".to_string()
        } else {
            bounded.to_string()
        }
    }

    /// Cancel expendable playlist work before an exact close: the import (never
    /// committed) and any export whose dialog is still open (no grant yet). An
    /// export that already holds a grant is retained so its publication is not
    /// abandoned mid-write.
    pub(crate) fn begin_playlist_close(&mut self) {
        self.playlist_transfer.cancel_import();
        self.import_job = None;
        let ungranted: Vec<u64> = self
            .export_jobs
            .iter()
            .filter(|(_, j)| j.grant.is_none())
            .map(|(g, _)| *g)
            .collect();
        for generation in ungranted {
            self.playlist_transfer.cancel_export(generation);
            self.export_jobs.remove(&generation);
        }
    }

    pub(crate) fn maybe_finish_close(&mut self) -> Option<Task<Message>> {
        if self.close_after_save
            && !self.native_import_pending
            && self.pending_save.is_none()
            && !self.has_export_publication_debt()
        {
            self.prepare_for_shutdown();
            // Worker/reaper panic or lost-custody poison permanently vetoes close.
            let catalog = self.radio_catalog_service.close_debt();
            let search = self.podcast_search.close_debt();
            let artwork = self.podcast_artwork.close_debt();
            let details = self.track_details_service.close_debt();
            let mpris = self.mpris.poll_close();
            // Report a permanent veto once so the close wake does not spin silently.
            if !self.fetch_close_veto_reported
                && (catalog.is_terminally_vetoed()
                    || search.is_terminally_vetoed()
                    || artwork.is_terminally_vetoed()
                    || details.is_terminally_vetoed()
                    || mpris == mpris_runtime::CloseReadiness::Vetoed)
            {
                self.fetch_close_veto_reported = true;
                eprintln!(
                    "music close: retained owner retirement permanently vetoed \
                     (catalog={catalog:?} search={search:?} artwork={artwork:?} details={details:?} mpris={mpris:?})"
                );
            }
            if self.podcast_service.close_debt().is_settled()
                && catalog.is_settled()
                && search.is_settled()
                && artwork.is_settled()
                && details.is_settled()
                && mpris == mpris_runtime::CloseReadiness::Ready
            {
                self.close_after_save = false;
                #[cfg(feature = "center")]
                self.shutdown_center();
                return Some(self.close_window_task());
            }
        }
        None
    }

    /// Submit each due root scan to the watcher's bounded owner and map its
    /// settlement to a typed completion or a retained-intent refusal. The walk
    /// runs on the owner's worker.
    pub(crate) fn dispatch_root_scans(
        &self,
        dispatches: Vec<library_watch::ScanDispatch>,
    ) -> Task<Message> {
        if dispatches.is_empty() {
            return Task::none();
        }
        Task::batch(dispatches.into_iter().map(|dispatch| {
            let root = dispatch.root.clone();
            let generation = dispatch.generation;
            let task = self.library_watch.submit_scan(dispatch);
            Task::perform(task, move |settlement| match settlement.into_result() {
                Ok(scan) => Message::RootScanComplete(Box::new(scan)),
                Err(_) => Message::RootScanFailed {
                    root: root.clone(),
                    generation,
                },
            })
        }))
    }

    /// Post one path-free terminal status for a manual Rescan, coalesced onto
    /// the shared library-status line so it replaces the in-flight notice.
    pub(crate) fn post_manual_rescan_status(&mut self, completion: library_watch::ScanCompletion) {
        use library_watch::ScanCompletion;
        match completion {
            ScanCompletion::Complete => self.post_folder_status(
                format!(
                    "Library rescan complete — {} tracks",
                    self.library.track_count()
                ),
                ToastTone::Success,
            ),
            ScanCompletion::Refused(_) => self.post_folder_status(
                "Library rescan finished — some folders could not be fully read",
                ToastTone::Warning,
            ),
            ScanCompletion::Vanished => self.post_folder_status(
                "Library rescan finished — a folder is unavailable",
                ToastTone::Warning,
            ),
        }
    }

    /// Draw the next value from the single availability ordering source.
    ///
    /// Uses a checked add and never reuses generation 0. On exhaustion it returns
    /// `None` after entering the terminal exhausted state once, and callers treat
    /// the decision as inert.
    fn next_availability_generation(&mut self) -> Option<u64> {
        match self.availability_generation.checked_add(1) {
            Some(next) => {
                self.availability_generation = next;
                Some(next)
            }
            None => {
                self.enter_availability_exhaustion();
                None
            }
        }
    }

    /// Enter the terminal availability-exhaustion state exactly once: stop and
    /// refuse local playback, drop unresolvable restore and loaded-source records,
    /// and show one disclosure. Radio and podcast streams are unaffected.
    fn enter_availability_exhaustion(&mut self) {
        if self.availability_exhausted {
            return;
        }
        self.availability_exhausted = true;
        // These can never resolve after exhaustion.
        self.restore_intent = None;
        self.pending_source_open = None;
        let current_is_local =
            self.playback.current_track_id.is_some() && self.playback.stream_selection().is_none();
        if current_is_local && self.playback.playing {
            self.playback.playing = false;
            if let Some(audio) = self.audio.as_ref() {
                let _ = audio.stop(&mut self.media);
            }
            self.sync_playback_generation_from_media();
        }
        self.post_folder_status(
            domain::AVAILABILITY_EXHAUSTED_DISCLOSURE,
            ToastTone::Warning,
        );
    }

    /// Apply one bounded root scan to the library by configured-root ownership.
    ///
    /// A complete snapshot merges new and changed files, preserving ids, counters,
    /// playlist membership and current playback, marks admitted files
    /// [`Present`](domain::TrackAvailability::Present) or
    /// [`Undecodable`](domain::TrackAvailability::Undecodable), and marks absent
    /// root-owned paths [`Missing`](domain::TrackAvailability::Missing). Rows are
    /// never deleted, so a remount and rescan restores them. A partial scan never
    /// marks absence, and nested roots each own their own subtree.
    pub(crate) fn reconcile_root_scan(&mut self, scan: library_watch::RootScan) {
        if self
            .track_details
            .as_ref()
            .is_some_and(|details| details.path.starts_with(&scan.root))
        {
            self.cancel_track_details(track_details::Failure::Stale);
        }
        use library_watch::ScanCompletion;
        let root = scan.root.clone();
        let scope = scan.scope.clone();
        // Stamp from the single availability ordering source, not the watcher's
        // scan generation, so a later reconcile or backend outcome outranks it.
        let Some(generation) = self.next_availability_generation() else {
            return;
        };
        match scan.completion {
            ScanCompletion::Complete => {
                let mut present: std::collections::HashSet<std::path::PathBuf> =
                    std::collections::HashSet::new();
                let merged_any = !scan.tracks.is_empty();
                for mut track in scan.tracks {
                    present.insert(track.path.clone());
                    self.persist.apply_track_state(&mut track);
                    let path = track.path.clone();
                    self.library.merge_scanned_track(track);
                    self.library.decide_availability(
                        path,
                        domain::TrackAvailability::Present,
                        generation,
                    );
                }
                for path in &scan.undecodable {
                    self.library.decide_availability(
                        path.clone(),
                        domain::TrackAvailability::Undecodable,
                        generation,
                    );
                }
                let undecodable_set: std::collections::HashSet<&std::path::Path> =
                    scan.undecodable.iter().map(|p| p.as_path()).collect();
                let folders = self.library.folders.clone();
                let owned_absent: Vec<std::path::PathBuf> = self
                    .library
                    .track_ids_under(&root)
                    .into_iter()
                    .filter_map(|id| self.library.tracks.get(&id).map(|t| t.path.clone()))
                    .filter(|p| {
                        !present.contains(p)
                            && scope
                                .as_ref()
                                .is_none_or(|dirs| dirs.iter().any(|dir| p.starts_with(dir)))
                            && !undecodable_set.contains(p.as_path())
                            && domain::owning_root(p, &folders) == Some(root.as_path())
                    })
                    .collect();
                for path in owned_absent {
                    self.library.decide_availability(
                        path,
                        domain::TrackAvailability::Missing,
                        generation,
                    );
                }
                self.library.reindex();
                self.enforce_current_track_availability();
                if merged_any {
                    self.mark_persistence_dirty();
                }
                self.library_watch.reconcile_scan_watches(
                    &root,
                    scope.as_deref(),
                    &scan.directories,
                );
            }
            ScanCompletion::Vanished => {
                self.library_watch
                    .mark_unavailable(&root, "A library folder is no longer available.");
                // A vanished root marks every track it owns Missing, respecting nested-root
                // ownership.
                let folders = self.library.folders.clone();
                let owned: Vec<std::path::PathBuf> = self
                    .library
                    .track_ids_under(&root)
                    .into_iter()
                    .filter_map(|id| self.library.tracks.get(&id).map(|t| t.path.clone()))
                    .filter(|p| domain::owning_root(p, &folders) == Some(root.as_path()))
                    .collect();
                for path in owned {
                    self.library.decide_availability(
                        path,
                        domain::TrackAvailability::Missing,
                        generation,
                    );
                }
                self.enforce_current_track_availability();
            }
            ScanCompletion::Refused(_) => {
                let merged_any = !scan.tracks.is_empty();
                for mut track in scan.tracks {
                    self.persist.apply_track_state(&mut track);
                    let path = track.path.clone();
                    self.library.merge_scanned_track(track);
                    self.library.decide_availability(
                        path,
                        domain::TrackAvailability::Present,
                        generation,
                    );
                }
                for path in &scan.undecodable {
                    self.library.decide_availability(
                        path.clone(),
                        domain::TrackAvailability::Undecodable,
                        generation,
                    );
                }
                if merged_any {
                    self.library.reindex();
                    self.mark_persistence_dirty();
                }
                // A partial scan never marks absence or downgrades a Present row, but
                // escalates the root's Checking rows to Unverified until a manual Rescan.
                let folders = self.library.folders.clone();
                let unverifiable: Vec<std::path::PathBuf> = self
                    .library
                    .track_ids_under(&root)
                    .into_iter()
                    .filter_map(|id| self.library.tracks.get(&id).map(|t| t.path.clone()))
                    .filter(|p| {
                        self.library.availability_of_path(p) == domain::TrackAvailability::Unknown
                            && domain::owning_root(p, &folders) == Some(root.as_path())
                    })
                    .collect();
                for path in unverifiable {
                    self.library.decide_availability(
                        path,
                        domain::TrackAvailability::Unverified,
                        generation,
                    );
                }
                self.enforce_current_track_availability();
                self.library_watch
                    .mark_partial(&root, "A library folder could not be fully scanned.");
            }
        }
        // Fire the deferred restore load on Present, or retire it on a terminal
        // state.
        self.resolve_restore_intent();
    }

    /// Refuse a manual activation of a track not proved `Present`, announcing one
    /// disclosure and returning `true`. After generation exhaustion every local
    /// activation is refused.
    pub(crate) fn refuse_unplayable_activation(&mut self, id: u64) -> bool {
        if self.availability_exhausted {
            self.post_folder_status(
                domain::AVAILABILITY_EXHAUSTED_DISCLOSURE,
                ToastTone::Warning,
            );
            return true;
        }
        let state = self.library.availability_of(id);
        if state.is_present() {
            return false;
        }
        if let Some(text) = state.disclosure() {
            self.post_folder_status(text, ToastTone::Warning);
        }
        true
    }

    /// Seed the queue from a bulk set (Play All, Shuffle Library, a container's
    /// Play), admitting only playable ids and starting the first. Returns whether
    /// playback started; with nothing playable it refuses with one disclosure.
    pub(crate) fn start_playable_seed(&mut self, ids: Vec<u64>, shuffle: bool) -> bool {
        if self.availability_exhausted {
            self.post_folder_status(
                domain::AVAILABILITY_EXHAUSTED_DISCLOSURE,
                ToastTone::Warning,
            );
            return false;
        }
        let playable: Vec<u64> = ids
            .into_iter()
            .filter(|&id| self.library.is_playable_now(id))
            .collect();
        if playable.is_empty() {
            self.post_folder_status(domain::BULK_NONE_PLAYABLE_DISCLOSURE, ToastTone::Warning);
            return false;
        }
        self.playback.seed_queue_with(&playable, &self.library);
        self.playback.shuffle = shuffle;
        if shuffle {
            self.playback.shuffle_queue_from(0);
            if let Some(&first) = self.playback.queue.first() {
                self.playback.load_current_track(first, &self.library);
            }
        }
        let Some(first) = self.playback.current_track_id else {
            return false;
        };
        self.route_local_track_to_audio(first, true);
        self.mark_persistence_dirty();
        true
    }

    /// Route the current track to audio only when it is playable; otherwise stop
    /// the route through [`Self::enforce_current_track_availability`].
    pub(crate) fn route_current_if_playable(&mut self, autoplay: bool) {
        let Some(id) = self.playback.current_track_id else {
            return;
        };
        if self.library.is_playable_now(id) {
            self.route_local_track_to_audio(id, autoplay);
        } else {
            self.enforce_current_track_availability();
        }
    }

    /// If the current library track became
    /// [`Missing`](domain::TrackAvailability::Missing) or
    /// [`Undecodable`](domain::TrackAvailability::Undecodable), stop the audio
    /// route, keep the row and queue identity, and announce one disclosure.
    pub(crate) fn enforce_current_track_availability(&mut self) {
        let Some(id) = self.playback.current_track_id else {
            return;
        };
        let state = self.library.availability_of(id);
        if !state.blocks_playback() {
            return;
        }
        let was_playing = self.playback.playing;
        if was_playing {
            self.playback.playing = false;
            if let Some(audio) = self.audio.as_ref() {
                let _ = audio.stop(&mut self.media);
            }
            self.sync_playback_generation_from_media();
            if let Some(text) = state.disclosure() {
                self.post_folder_status(text, ToastTone::Warning);
            }
        }
    }

    /// Confirm the current track `Present` on a `SourceReady` for `generation`.
    ///
    /// Applies only when the admitted source open matches and its durable path is
    /// still current, stamped with the availability generation in force at
    /// admission so a newer scan `Missing` wins. The record is then marked
    /// confirmed but kept, so a later typed error for this generation can demote.
    pub(crate) fn confirm_source_ready_present(&mut self, generation: u64) {
        if self.availability_exhausted {
            return;
        }
        let Some(pending) = self.pending_source_open.clone() else {
            return;
        };
        if pending.media_generation != generation || pending.confirmed {
            return;
        }
        let still_current = self.playback.current_track_id == Some(pending.track_id)
            && self
                .library
                .tracks
                .get(&pending.track_id)
                .map(|t| t.path.as_path())
                == Some(pending.path.as_path());
        if !still_current {
            return;
        }
        self.library.decide_availability(
            pending.path,
            domain::TrackAvailability::Present,
            pending.admission_generation,
        );
        // Keep the record as the current loaded identity for later error folding.
        if let Some(open) = self.pending_source_open.as_mut() {
            open.confirmed = true;
        }
    }

    /// Fold a typed backend error for `generation` into availability.
    ///
    /// Returns `true` when the error proves the admitted source
    /// [`SourceNotFound`](enzui_services::media::MediaErrorKind::SourceNotFound)
    /// (`Missing`) or
    /// [`UnsupportedFormat`](enzui_services::media::MediaErrorKind::UnsupportedFormat)
    /// (`Undecodable`); the demote takes a fresh generation, clears the record and
    /// stops playback. Other errors, stale generations and exhaustion return
    /// `false` without demoting.
    pub(crate) fn fold_backend_error_into_availability(
        &mut self,
        generation: u64,
        error: &enzui_services::media::MediaError,
    ) -> bool {
        use enzui_services::media::MediaErrorKind;
        let demoted = match error.kind {
            MediaErrorKind::SourceNotFound => domain::TrackAvailability::Missing,
            MediaErrorKind::UnsupportedFormat => domain::TrackAvailability::Undecodable,
            _ => return false,
        };
        let Some(pending) = self.pending_source_open.clone() else {
            return false;
        };
        if pending.media_generation != generation {
            return false;
        }
        let still_current = self.playback.current_track_id == Some(pending.track_id)
            && self
                .library
                .tracks
                .get(&pending.track_id)
                .map(|t| t.path.as_path())
                == Some(pending.path.as_path());
        if !still_current {
            return false;
        }
        // A demote needs a fresh generation to outrank any prior confirm. After
        // exhaustion no decision is admitted, so the demote is inert.
        let Some(decision_gen) = self.next_availability_generation() else {
            return false;
        };
        self.library
            .decide_availability(pending.path, demoted, decision_gen);
        // Clear the loaded identity so a stray event for this generation cannot
        // re-decide it.
        self.pending_source_open = None;
        // `enforce` only announces when it stops a playing source, so a demote during
        // preroll announces here.
        let was_playing = self.playback.playing;
        self.enforce_current_track_availability();
        if !was_playing && let Some(text) = demoted.disclosure() {
            self.post_folder_status(text, ToastTone::Warning);
        }
        true
    }

    /// Merge freshly scanned tracks and mark each path `Present` under one
    /// checked availability generation, then reindex.
    ///
    /// The set names no root, so it marks nothing absent. After generation
    /// exhaustion the merge still happens but no row becomes Present.
    pub(crate) fn absorb_scanned_tracks(&mut self, tracks: Vec<domain::Track>) {
        if self
            .track_details
            .as_ref()
            .is_some_and(|details| tracks.iter().any(|track| track.path == details.path))
        {
            self.cancel_track_details(track_details::Failure::Stale);
        }
        if tracks.is_empty() {
            return;
        }
        self.mark_persistence_dirty();
        let generation = self.next_availability_generation();
        for mut track in tracks {
            self.persist.apply_track_state(&mut track);
            let path = track.path.clone();
            self.library.merge_scanned_track(track);
            if let Some(generation) = generation {
                self.library.decide_availability(
                    path,
                    domain::TrackAvailability::Present,
                    generation,
                );
            }
        }
        self.library.reindex();
        self.resolve_restore_intent();
    }

    /// Retire a configured root's initial-scan debt on its first accepted
    /// completion or on removal. Draining the last entry finalizes the pending
    /// play stats and playback session and arms the restored paused source, once.
    pub(crate) fn retire_initial_scan(&mut self, root: &std::path::Path) {
        if self.initial_scan_roots.remove(root) && self.initial_scan_roots.is_empty() {
            self.finalize_pending_track_stats();
            self.finalize_pending_playback_session();
            self.arm_restored_paused_source();
        }
    }
}

impl MusicApp {
    pub(crate) fn drain_radio_catalog_results(&mut self) {
        for result in self.radio_catalog_service.drain() {
            self.apply_radio_catalog_result(result);
        }
    }

    /// Drain the bounded search worker pool without blocking the UI.
    pub(crate) fn drain_podcast_search_results(&mut self) {
        for response in self.podcast_search.drain() {
            self.apply_podcast_search_response(response);
        }
    }
}
