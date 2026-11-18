//! Shutdown, persistence scheduling, and transfer debt.

use super::*;

impl MusicApp {
    pub(crate) fn prepare_for_shutdown(&mut self) {
        self.podcast_requests
            .cancel_all(enzui_core::cancellation::CancelCause::Shutdown);
        self.podcast_service.shutdown();
        self.podcast_search.shutdown();
        self.podcast_artwork.shutdown();
        self.radio_catalog_service.shutdown();
        self.track_details_service.shutdown();
        self.mpris.begin_close();
        // Stop playlist portal admission and cancel the expendable
        // import; an accepted export publication that reached this point has
        // already settled (close vetoes until then). The playlist owner's own
        // Drop performs the bounded worker join off the reducer thread.
        self.playlist_transfer.cancel_import();
        self.import_job = None;
        self.playlist_transfer.shutdown();
    }

    pub(crate) fn close_window_task(&self) -> Task<Message> {
        iced::window::latest().then(|maybe_id| match maybe_id {
            Some(id) => enzui_widgets::title_bar_gesture::WindowCommand::Close.into_task(id),
            None => Task::none(),
        })
    }

    pub(crate) fn apply_persistence_completion(
        &mut self,
        generation: u64,
        outcome: persistence::PersistenceCompletion,
    ) -> bool {
        let Some(pending) = self.pending_save.as_ref() else {
            return false;
        };
        if pending.ticket().generation() != generation {
            return false;
        }
        match outcome {
            persistence::PersistenceCompletion::Saved => {
                let pending = self.pending_save.take().expect("matching persistence debt");
                self.persist.acknowledge_saved(&pending);
                let mut waiting = Vec::new();
                for (stamp, receipt) in self.native_import_receipts.drain(..) {
                    if stamp <= generation {
                        receipt.finish();
                    } else {
                        waiting.push((stamp, receipt));
                    }
                }
                self.native_import_receipts = waiting;
                if matches!(
                    self.persistence_notice.as_ref(),
                    Some(PersistenceNotice::SaveFailed(_))
                ) {
                    self.persistence_notice = None;
                }
                true
            }
            persistence::PersistenceCompletion::Superseded => false,
            persistence::PersistenceCompletion::Error(failure) => {
                self.native_import_receipts
                    .retain(|(stamp, _)| *stamp > generation);
                // Preserve full detail only on the diagnostic channel. Escaping
                // prevents terminal controls while retaining every scalar.
                let diagnostic: String = failure
                    .diagnostic()
                    .chars()
                    .flat_map(char::escape_default)
                    .collect();
                eprintln!("music persistence failed: diagnostic={diagnostic}");
                self.persistence_notice =
                    Some(PersistenceNotice::SaveFailed(failure.safe_summary()));
                false
            }
        }
    }

    pub(crate) fn retry_persistence(&mut self) -> Task<Message> {
        // Never reopen the same authority after custody is lost; an
        // ordinary retry rereads and reacquires off-thread instead.
        if self.persist.authority_lost_custody() {
            return self.reacquire_authority(false);
        }
        let Some(pending) = self.pending_save.clone() else {
            return Task::none();
        };
        let renewed = pending.renewed(&self.persistence_queue);
        self.dispatch_persistence(renewed)
    }

    /// Reacquire the publication authority off-thread after custody was lost or
    /// on explicit Retire.
    ///
    /// A reacquisition already in flight refuses a second. Retire quarantines
    /// the exact retained path and read identity; only a proven move of that
    /// inode creates a fresh authority, and any other outcome, including a
    /// later substitution, stays disabled with a typed custody reason. The
    /// result installs under the boot generation guard.
    pub(crate) fn reacquire_authority(&mut self, retire: bool) -> Task<Message> {
        if self.boot_load_worker_spawned {
            return Task::none();
        }
        self.boot_load_worker_spawned = true;
        self.boot_load_generation = self.boot_load_generation.wrapping_add(1);
        let generation = self.boot_load_generation;
        // Retire operates on the exact retained path and identity carried by the
        // session; retry re-resolves and reacquires from the environment.
        let plan = if retire {
            ReacquireKind::Retire {
                path: self.persist.path().to_path_buf(),
                expected: self.persist.retire_identity(),
            }
        } else {
            ReacquireKind::Retry(enzui_core::app_data::AppDataResolver::from_env())
        };
        let task = self.boot_load_owner.submit(generation, move |context| {
            if context.is_cancelled() {
                return AuthorityRefresh::unavailable(generation, None);
            }
            let outcome = match plan {
                ReacquireKind::Retire { path, expected } => {
                    match persistence::retire_and_reacquire(&path, expected) {
                        persistence::RetireOutcome::Reacquired {
                            authority,
                            recovered,
                        } => ReacquireOutcome::Acquired {
                            authority: std::sync::Arc::new(std::sync::Mutex::new(authority)),
                            recovered,
                            retired: true,
                        },
                        persistence::RetireOutcome::Refused(reason) => {
                            ReacquireOutcome::Unavailable {
                                retire_refusal: Some(reason),
                            }
                        }
                    }
                }
                ReacquireKind::Retry(resolver) => {
                    match persistence::resolve_state_location(&resolver) {
                        Ok(path) => {
                            let (_doc, _origin, authority, _identity) = persistence::acquire(&path);
                            match authority {
                                Some(authority) => ReacquireOutcome::Acquired {
                                    authority: std::sync::Arc::new(std::sync::Mutex::new(
                                        authority,
                                    )),
                                    recovered: None,
                                    retired: false,
                                },
                                None => ReacquireOutcome::Unavailable {
                                    retire_refusal: None,
                                },
                            }
                        }
                        Err(_) => ReacquireOutcome::Unavailable {
                            retire_refusal: None,
                        },
                    }
                }
            };
            AuthorityRefresh {
                generation,
                outcome,
            }
        });
        Task::perform(task, move |settlement| {
            let refresh = settlement
                .into_result()
                .unwrap_or_else(|_| AuthorityRefresh::unavailable(generation, None));
            Message::AuthorityReacquired(Box::new(refresh))
        })
    }

    /// Install a reacquired authority under the boot generation guard.
    ///
    /// A stale refresh is dropped. A fresh authority re-enables publication and
    /// re-dispatches the outstanding save through it; a Retire that moved bytes
    /// surfaces the recovery notice; any refusal disables saving and surfaces
    /// the bounded typed custody reason. The owner owns worker lifetime, so no
    /// handle is joined here.
    pub(crate) fn apply_authority_refresh(&mut self, refresh: AuthorityRefresh) -> Task<Message> {
        if refresh.generation != self.boot_load_generation {
            return Task::none();
        }
        self.boot_load_generation = self.boot_load_generation.wrapping_add(1);
        self.boot_load_worker_spawned = false;
        match refresh.outcome {
            ReacquireOutcome::Acquired {
                authority,
                recovered,
                retired,
            } => {
                self.persist.install_authority(authority);
                if matches!(
                    self.persistence_notice.as_ref(),
                    Some(
                        PersistenceNotice::SaveFailed(_)
                            | PersistenceNotice::RefusedReadOnly(_)
                            | PersistenceNotice::RetireRefused(_)
                    )
                ) {
                    self.persistence_notice = None;
                }
                if retired {
                    self.persistence_notice =
                        Some(PersistenceNotice::RecoveredFromCorrupt(recovered));
                }
                if let Some(pending) = self.pending_save.clone() {
                    let renewed = pending.renewed(&self.persistence_queue);
                    return self.dispatch_persistence(renewed);
                }
                Task::none()
            }
            ReacquireOutcome::Unavailable { retire_refusal } => {
                self.persist = persistence::Persist::inert();
                self.persistence_notice = Some(match retire_refusal {
                    Some(reason) => PersistenceNotice::RetireRefused(reason),
                    None => {
                        PersistenceNotice::RefusedReadOnly(persistence::RefusalKind::Unreadable)
                    }
                });
                Task::none()
            }
        }
    }

    // The single admission point for automatically-named playlists. `build`
    // receives the reserved sequence value and returns the new id, or `None`
    // when id space is exhausted. Admission is atomic: if the sequence has no
    // successor (counter at `u64::MAX`) or id space is exhausted, nothing is
    // created and the counter, library and persistence debt are unchanged.
    pub(crate) fn create_auto_named_playlist(
        &mut self,
        build: impl FnOnce(&mut Library, u64) -> Option<u64>,
    ) -> Option<u64> {
        let n = self.next_playlist_seq;
        // `build` creates the playlist, so it must not run when the sequence
        // cannot advance.
        let created = match n.checked_add(1) {
            Some(next) => build(&mut self.library, n).map(|id| (id, next)),
            None => None,
        };
        match created {
            Some((id, next)) => {
                self.next_playlist_seq = next;
                self.mark_persistence_dirty();
                Some(id)
            }
            None => {
                self.post_folder_status(PLAYLIST_ID_EXHAUSTED_STATUS, ToastTone::Warning);
                None
            }
        }
    }

    /// Turn a dirtied state into a background write.
    ///
    /// Serialisation and the atomic write run inside the returned `Task`;
    /// nothing on the reducer thread is O(tracks).
    pub(crate) fn schedule_persistence(&mut self) -> Option<Task<Message>> {
        if !std::mem::take(&mut self.persistence_dirty) || !self.persist.is_enabled() {
            return None;
        }
        let document = self.persisted_document();
        let pending = self.persist.prepare(document, &self.persistence_queue)?;
        Some(self.dispatch_persistence(pending))
    }

    /// The persisted projection as a cheaply comparable, cheaply shareable
    /// value. O(what the listener typed), whatever the library size.
    pub(crate) fn persisted_document(&mut self) -> persistence::SharedDocument {
        let bounded = persistence::bounded_half(self);
        let track_added_at = self.persist.track_added_at_for(&self.library);
        // Shared by pointer and rebuilt only when `track_stats_revision`
        // moves, so `SharedDocument::unchanged_from` can compare with
        // `Arc::ptr_eq`.
        let track_stats = self.persist.track_stats_for(&self.library);
        persistence::SharedDocument::new(bounded, track_added_at, track_stats)
    }

    fn dispatch_persistence(&mut self, pending: persistence::PendingSave) -> Task<Message> {
        let generation = pending.ticket().generation();
        self.pending_save = Some(pending.clone());
        let queue = self.persistence_queue.clone();
        Task::perform(pending.complete_off_executor(queue), move |outcome| {
            Message::PersistenceFinished {
                generation,
                outcome,
            }
        })
    }

    /// The blocking close drain.
    ///
    /// Re-registering the outstanding document makes every task that has not
    /// started stale, and taking the queue's writer lock waits for one already
    /// inside its atomic rename. Without it, the last edit before the window
    /// closes is a write nobody ever polls.
    pub(crate) fn drain_persistence_blocking(&mut self) -> bool {
        // Position ticks do not dirty persistence, so capture the playhead
        // here to keep the newest episode position.
        if self.record_episode_resume() {
            self.mark_persistence_dirty();
        }
        if self.persistence_dirty && self.persist.is_enabled() {
            self.persistence_dirty = false;
            let document = self.persisted_document();
            if let Some(pending) = self.persist.prepare(document, &self.persistence_queue) {
                self.pending_save = Some(pending);
            }
        }
        let Some(pending) = self.pending_save.clone() else {
            return true;
        };
        let renewed = pending.renewed(&self.persistence_queue);
        let generation = renewed.ticket().generation();
        self.pending_save = Some(renewed.clone());
        let outcome = renewed.complete(&self.persistence_queue);
        self.apply_persistence_completion(generation, outcome)
    }

    pub(crate) fn mark_persistence_dirty(&mut self) {
        self.persistence_dirty = true;
    }

    // ---- Playlist Import/Export route -----------------------------

    /// Whether a Busy-refused import or export intent is awaiting one retry, so
    /// the coalesced retry wake should be armed this frame.
    pub(crate) fn has_awaiting_retry(&self) -> bool {
        let import = matches!(
            self.import_job.as_ref().map(|j| j.state),
            Some(JobState::AwaitingRetry)
        );
        let export = self
            .export_jobs
            .values()
            .any(|j| j.grant.is_some() && j.state == JobState::AwaitingRetry);
        import || export
    }

    /// Whether any export job holds a portal grant, i.e. carries publication
    /// debt (in flight, awaiting retry, or retained after a failure). An exact
    /// close vetoes while this is true.
    pub(crate) fn has_export_publication_debt(&self) -> bool {
        self.export_jobs.values().any(|j| j.grant.is_some())
    }
}
