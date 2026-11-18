//! Reducer batch 2: RescanComplete through PickerAddToExisting.

use super::*;

impl MusicApp {
    pub(crate) fn reduce_batch2(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::RescanComplete(tracks) => {
                // Unscoped legacy rescan: merge and prove each returned path
                // Present, authorizing no absence.
                self.absorb_scanned_tracks(tracks);
                // Shares the coalesce key with the "Rescanning library…"
                // in-flight line, so the completion replaces it in place.
                self.post_folder_status(
                    format!(
                        "Library rescan complete — {} tracks",
                        self.library.track_count()
                    ),
                    ToastTone::Success,
                );
                return self.reconcile_track_info_model();
            }
            Message::WatchDeliveries(deliveries) => {
                // Coalesce the burst into per-root debounce deadlines; the
                // wake tick dispatches the due scans. No filesystem work runs
                // on this frame.
                self.library_watch
                    .on_deliveries(&deliveries, std::time::Instant::now());
            }
            Message::LibraryWatchTick => {
                let dispatches = self.library_watch.dispatch_due(std::time::Instant::now());
                return self.dispatch_root_scans(dispatches);
            }
            Message::RootScanComplete(scan) => {
                let scan = *scan;
                let root = scan.root.clone();
                let completion = scan.completion;
                let now = std::time::Instant::now();
                if self.library_watch.accept_completion(&scan, now) {
                    self.reconcile_root_scan(scan);
                    // An accepted current completion is this root's initial
                    // terminal; retiring it finalizes the boot session once
                    // the last initial root drains. A follow-up completion
                    // finds the root already retired and does not double-fire.
                    self.retire_initial_scan(&root);
                    // A follow-up armed while this scan was in flight is now
                    // due; dispatch it immediately rather than waiting a tick.
                    let follow = self.library_watch.dispatch_due(now);
                    let follow_task = self.dispatch_root_scans(follow);
                    // A manual Rescan posts one terminal status once its root
                    // has fully settled; automatic scans stay silent.
                    if self.library_watch.take_manual_if_settled(&root) {
                        self.post_manual_rescan_status(completion);
                    }
                    return follow_task;
                }
            }
            Message::RootScanFailed { root, generation } => {
                // A busy owner refusal is not a terminal: the watcher retains
                // the intent and re-arms a bounded retry, and the root stays in
                // `initial_scan_roots` so a later accepted completion, never
                // this refusal, finalizes the boot session.
                self.library_watch
                    .on_scan_refused(&root, generation, std::time::Instant::now());
            }
            Message::SelectAlbum(id) => {
                if self.selected_album_id != Some(id) {
                    self.album_scroll.set(iced::Vector::ZERO);
                    *self.album_anchor.borrow_mut() = None;
                    self.album_anchor_columns.set(0);
                }
                self.current_view = View::Albums;
                self.selected_album_id = Some(id);
                self.filter = LibraryFilter::Album(id);
            }
            Message::SelectArtist(id) => {
                self.filter = LibraryFilter::Artist(id);
                self.current_view = View::Albums;
            }
            Message::SelectGenre(id) => {
                self.filter = LibraryFilter::Genre(id);
                self.current_view = View::Home;
            }
            Message::SelectTag(id) => {
                self.filter = LibraryFilter::Tag(id);
                self.current_view = View::Home;
            }
            Message::SelectPlaylist(id) => {
                self.filter = LibraryFilter::Playlist(id);
                self.current_view = View::Playlist;
            }
            Message::ClearFilter => {
                self.filter = LibraryFilter::None;
                self.selected_album_id = None;
            }
            Message::SetSort(mode) => {
                if mode == SortMode::Rating {
                    self.current_view = View::Home;
                }
                self.sort_mode = mode;
            }
            Message::PlayTrack(id) => {
                // A manual row activation refuses a Missing/Undecodable
                // source with one bounded disclosure and never marks it
                // playing. Rescan/Remove remain reachable from the row.
                if self.refuse_unplayable_activation(id) {
                    return Task::none();
                }
                if !self.playback.play_track(id, &self.library) {
                    // A full queue / exhausted id counter refuses before
                    // mutating; disclose it and route nothing.
                    self.post_folder_status(domain::QUEUE_FULL_DISCLOSURE, ToastTone::Warning);
                    return Task::none();
                }
                // A no-op without an audio backend.
                self.route_local_track_to_audio(id, true);
                // Starting playback credits no play; only an authoritative
                // `Message::PlaybackCompleted` for this generation does.
                self.mark_persistence_dirty();
            }
            Message::PlayTrackNext(id) => {
                // A stale "Play next" for a row that became
                // Missing/Undecodable after its menu opened must not create a
                // fresh actionable queue promise. Refuse with one bounded
                // disclosure; route nothing to audio.
                if self.refuse_unplayable_activation(id) {
                    return Task::none();
                }
                if !self.playback.play_next(id) {
                    self.post_folder_status(domain::QUEUE_FULL_DISCLOSURE, ToastTone::Warning);
                    return Task::none();
                }
                self.mark_persistence_dirty();
            }
            Message::AddTrackToQueue(id) => {
                // Same as `PlayTrackNext` — a blocked row is refused,
                // never enqueued as an unplayable promise.
                if self.refuse_unplayable_activation(id) {
                    return Task::none();
                }
                if !self.playback.add_to_queue(id) {
                    self.post_folder_status(domain::QUEUE_FULL_DISCLOSURE, ToastTone::Warning);
                    return Task::none();
                }
                self.mark_persistence_dirty();
            }
            Message::RemoveFromQueue(pos) => {
                let was_current =
                    self.playback.current_track_id.is_some() && pos == self.playback.queue_index;
                self.playback.remove_from_queue(pos, &self.library);
                if was_current {
                    // Either the queue is empty (stop the pipeline) or a new current has taken
                    // its place. The successor may be a retained-but-blocked row, so route it
                    // only when playable; otherwise stop the route and keep its identity via the
                    // availability guard.
                    if self.playback.queue.get(self.playback.queue_index).is_some() {
                        self.route_current_if_playable(self.playback.playing);
                    } else {
                        if let Some(audio) = self.audio.as_ref() {
                            let _ = audio.stop(&mut self.media);
                        }
                        self.media.clear_source();
                        self.mpris_source = None;
                    }
                }
                self.mark_persistence_dirty();
                // The deleted row held focus, so move it to the successor row.
                if let Some(target) =
                    queue_focus_after_removal(&self.playback, pos, self.playback.queue.len())
                {
                    return move_row_focus_task(target);
                }
            }
            Message::MoveToTopOfQueue(pos) => {
                self.playback.move_to_top(pos);
                self.mark_persistence_dirty();
            }
            Message::ReorderQueue { entry, slot } => {
                if let Some(entry) = crate::QueueEntryId::new(entry) {
                    self.playback.move_entry(entry, slot);
                    self.mark_persistence_dirty();
                }
            }
            Message::MoveUpInQueue(pos) => {
                self.playback.move_up(pos);
                self.mark_persistence_dirty();
            }
            Message::MoveDownInQueue(pos) => {
                self.playback.move_down(pos);
                self.mark_persistence_dirty();
            }
            Message::JumpToQueueEntry(pos) => {
                // Refuse a manual jump onto a Missing/Undecodable queue
                // entry with one bounded disclosure; the entry keeps its slot.
                if let Some(&target) = self.playback.queue.get(pos)
                    && self.refuse_unplayable_activation(target)
                {
                    return Task::none();
                }
                self.playback.jump_to(pos, &self.library);
                if let Some(&id) = self.playback.queue.get(self.playback.queue_index) {
                    self.route_local_track_to_audio(id, true);
                }
                // `jump_to` bumps the completion generation, so a late
                // completion for the outgoing track lands stale.
                self.mark_persistence_dirty();
            }
            Message::ClearQueue => {
                self.playback.clear_queue();
                if let Some(audio) = self.audio.as_ref() {
                    let _ = audio.stop(&mut self.media);
                }
                self.media.clear_source();
                self.mpris_source = None;
                self.sync_playback_generation_from_media();
                self.mark_persistence_dirty();
            }
            Message::SaveQueueAsPlaylist => {
                if !self.playback.queue.is_empty() {
                    let queue = self.playback.queue.clone();
                    self.create_auto_named_playlist(|library, n| {
                        library.save_queue_as_playlist(format!("Queue {n}"), &queue)
                    });
                }
            }
            Message::PlayLibrary => {
                // An all-blocked library refuses with one disclosure; an empty
                // one is a silent no-op.
                let ids = self.library.ordered_tracks(self.sort_mode, self.filter);
                if !ids.is_empty() {
                    self.start_playable_seed(ids, false);
                }
            }
            Message::ShuffleLibrary => {
                // Shuffle turns on only when playback actually started.
                let ids = self.library.ordered_tracks(self.sort_mode, self.filter);
                if !ids.is_empty() {
                    self.start_playable_seed(ids, true);
                }
            }
            Message::RescanLibrary => {
                self.filter = LibraryFilter::None;
                let folders = self.library.folders.clone();
                // A refusal ("nothing configured to rescan") is a failed
                // request and announces assertively; the in-flight line is
                // advisory and is superseded in place by the completion.
                if folders.is_empty() {
                    self.post_folder_status("No library folders to rescan", ToastTone::Warning);
                    return Task::none();
                }
                self.post_folder_status("Rescanning library…", ToastTone::Info);
                // Manual rescan uses the same bounded, generation-checked scan
                // and reconcile path automatic updates use: a complete scan
                // classifies survivors Present and absent, root-owned paths
                // Missing (retained, not deleted), rather than merging forever.
                let now = std::time::Instant::now();
                for folder in folders {
                    self.library_watch.request_rescan(folder, now);
                }
                let dispatches = self.library_watch.dispatch_due(now);
                return self.dispatch_root_scans(dispatches);
            }
            Message::RescanTrack(id) => {
                // Re-probes through the same watcher, scan owner and generation
                // as the library rescan; a stale completion is inert.
                let Some(root) = self
                    .library
                    .tracks
                    .get(&id)
                    .map(|t| t.path.clone())
                    .and_then(|path| {
                        domain::owning_root(&path, &self.library.folders)
                            .map(std::path::Path::to_path_buf)
                    })
                else {
                    return Task::none();
                };
                let now = std::time::Instant::now();
                self.library_watch.request_rescan(root, now);
                let dispatches = self.library_watch.dispatch_due(now);
                return self.dispatch_root_scans(dispatches);
            }
            Message::RemoveTrack(id) => {
                // The user's file is never touched.
                if !self.library.tracks.contains_key(&id) {
                    return Task::none();
                }
                let removed: std::collections::HashSet<u64> = std::iter::once(id).collect();
                let action = self.playback.drop_tracks(&removed, &self.library);
                self.library.remove_track(id);
                self.library.reindex();
                match action {
                    domain::PlaybackDropAction::Stopped => {
                        if let Some(audio) = self.audio.as_ref() {
                            let _ = audio.stop(&mut self.media);
                        }
                        self.media.clear_source();
                        self.mpris_source = None;
                        self.sync_playback_generation_from_media();
                    }
                    domain::PlaybackDropAction::Loaded(_) => {
                        // The surviving successor drop_tracks chose may
                        // itself be a retained-but-blocked row; route it only
                        // when playable, else stop through the availability guard.
                        self.route_current_if_playable(self.playback.playing);
                    }
                    domain::PlaybackDropAction::None => {}
                }
                self.mark_persistence_dirty();
                self.post_folder_status("Removed track from library", ToastTone::Success);
            }
            Message::CreatePlaylist => {
                self.create_auto_named_playlist(|library, n| {
                    library.create_playlist(format!("Playlist {n}"))
                });
            }
            Message::AddTrackToPlaylist {
                playlist_id,
                track_id,
            } => {
                self.library.add_track_to_playlist(playlist_id, track_id);
                self.mark_persistence_dirty();
            }
            Message::DeletePlaylist(id) => {
                self.library.delete_playlist(id);
                self.mark_persistence_dirty();
                if let LibraryFilter::Playlist(pid) = self.filter
                    && pid == id
                {
                    self.filter = LibraryFilter::None;
                }
                // Settle the modal for the deleted row; the overlay's existence
                // guard would otherwise strand an invisible modal and focus slot.
                if self
                    .playlist_rename
                    .as_ref()
                    .is_some_and(|r| r.playlist_id == id)
                {
                    return self.dismiss_modal_focus(
                        ModalPresentationId::PlaylistRename,
                        enzui_core::modal::DismissCause::CancelAction,
                    );
                }
            }
            Message::ConfirmDeletePlaylist(id) => {
                // Refuse to arm on a playlist that has since vanished.
                if !self.library.playlists.iter().any(|p| p.id == id) {
                    return Task::none();
                }
                self.playlist_delete = Some(id);
                self.playlist_delete_confirm = enzui_core::confirm::ConfirmState::new(None);
                let invoker = context_menu::menu_invoker_id(
                    domain::ContextTarget::PlaylistRow(id),
                    &self.library,
                    &self.playback,
                );
                return self.open_modal_focus(ModalPresentationId::PlaylistDelete, invoker);
            }
            Message::PlaylistDeleteConfirm(event) => {
                use enzui_core::confirm::ConfirmDecision;
                use enzui_widgets::confirm_dialog::ConfirmDialogEvent;
                match event {
                    // No checkbox on this dialog, so a toggle is inert.
                    ConfirmDialogEvent::CheckboxToggled(_) => {}
                    // Tab cycles within the dialog's presentation scope, so a
                    // covered or dismissed scope declines.
                    ConfirmDialogEvent::FocusNext | ConfirmDialogEvent::FocusPrevious => {
                        // The same scope id the modal host uses.
                        let scope = enzui_widgets::modal_host::presentation_scope_id(
                            ModalPresentationId::PlaylistDelete.scope_key(),
                        );
                        if let Some(op) = event.scoped_focus_operation::<Message>(scope) {
                            return iced::advanced::widget::operate(op);
                        }
                    }
                    // One exit for the buttons, Escape and the scrim alike.
                    ConfirmDialogEvent::Resolved(cause) => {
                        let pid = self.playlist_delete;
                        let confirmed = ConfirmDecision::from_cause(cause).is_confirmed();
                        // Dismiss first: it restores focus to the invoker (the
                        // playlist row) and clears `playlist_delete`.
                        let dismiss =
                            self.dismiss_modal_focus(ModalPresentationId::PlaylistDelete, cause);
                        if confirmed && let Some(pid) = pid {
                            // The same reducer body as the in-detail Delete.
                            let follow = self.update(Message::DeletePlaylist(pid));
                            return Task::batch([dismiss, follow]);
                        }
                        return dismiss;
                    }
                }
            }
            Message::RenamePlaylistOpen(id) => {
                let Some(current) = self
                    .library
                    .playlists
                    .iter()
                    .find(|p| p.id == id)
                    .map(|p| p.name.clone())
                else {
                    return Task::none();
                };
                self.playlist_rename = Some(PlaylistRename::new(id, current));
                let invoker = views::rename_playlist_button_id(id);
                return self.open_modal_focus(ModalPresentationId::PlaylistRename, Some(invoker));
            }
            Message::RenamePlaylistDraftChanged(name) => {
                if let Some(rename) = self.playlist_rename.as_mut() {
                    rename.draft = clamp_playlist_rename_draft(&name);
                }
            }
            Message::RenamePlaylistCommit => {
                let Some((id, draft)) = self
                    .playlist_rename
                    .as_ref()
                    .map(|r| (r.playlist_id, r.draft.clone()))
                else {
                    return Task::none();
                };
                // Target deleted while open: settle the modal without renaming
                // another row or dirtying persistence.
                if !self.library.playlists.iter().any(|p| p.id == id) {
                    return self.dismiss_modal_focus(
                        ModalPresentationId::PlaylistRename,
                        enzui_core::modal::DismissCause::CancelAction,
                    );
                }
                if let Ok(name) = self.library.validate_playlist_name(id, &draft) {
                    self.library.rename_playlist(id, name);
                    self.mark_persistence_dirty();
                    return self.dismiss_modal_focus(
                        ModalPresentationId::PlaylistRename,
                        enzui_core::modal::DismissCause::CloseButton,
                    );
                }
            }
            Message::RenamePlaylistCancel => {
                if self.playlist_rename.is_some() {
                    return self.dismiss_modal_focus(
                        ModalPresentationId::PlaylistRename,
                        enzui_core::modal::DismissCause::CancelAction,
                    );
                }
            }
            Message::OpenPlaylistPicker(target) => {
                self.playlist_picker = Some(PlaylistPicker::new(target));
                return self.open_modal_focus(ModalPresentationId::PlaylistPicker, None);
            }
            Message::ClosePlaylistPicker => {
                return self.dismiss_modal_focus(
                    ModalPresentationId::PlaylistPicker,
                    enzui_core::modal::DismissCause::CloseButton,
                );
            }
            Message::PickerNameChanged(name) => {
                if let Some(p) = self.playlist_picker.as_mut() {
                    p.new_name = name;
                }
            }
            Message::PickerCreateAndAdd => {
                // The "New playlist…" create is explicitly named, so a blank
                // name is a no-op.
                if let Some(picker) = self.playlist_picker.take() {
                    let name = picker.new_name.trim().to_string();
                    if name.is_empty() {
                        // Re-open with the (blank) draft preserved so the user
                        // is not silently dropped out of the dialog.
                        self.playlist_picker = Some(picker);
                    } else {
                        let current = self.playback.current_track_id;
                        let created =
                            self.library
                                .create_playlist_with_target(name, picker.target, current);
                        if created.is_some() {
                            self.mark_persistence_dirty();
                            return self.dismiss_modal_focus(
                                ModalPresentationId::PlaylistPicker,
                                enzui_core::modal::DismissCause::DefaultAction,
                            );
                        }
                        self.playlist_picker = Some(picker);
                        self.post_folder_status(PLAYLIST_ID_EXHAUSTED_STATUS, ToastTone::Warning);
                    }
                }
            }
            Message::PickerAddToExisting(playlist_id) => {
                if let Some(picker) = self.playlist_picker.take() {
                    let current = self.playback.current_track_id;
                    self.library
                        .add_target_to_playlist(playlist_id, picker.target, current);
                    self.mark_persistence_dirty();
                    return self.dismiss_modal_focus(
                        ModalPresentationId::PlaylistPicker,
                        enzui_core::modal::DismissCause::DefaultAction,
                    );
                }
            }
            _ => unreachable!("reduce_batch2 handles only its own variants"),
        }
        Task::none()
    }
}
