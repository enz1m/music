//! Owned modal-focus state for Music.
//!
//! Music can retain more than one presentation at once (for example Settings
//! beneath a picker). A single `Option<ModalFocusLifecycle>` loses the covered
//! presentation when the topmost one opens. `ModalFocusSet` is therefore the
//! app's one focus-state field, with one lifecycle slot per presentation.

use crate::{Message, MusicApp};
use enzui_core::modal::DismissCause;
use enzui_widgets::modal_host::{ModalFocusLifecycle, ModalHostEvent};
use iced::Task;
use iced::advanced::widget::Id;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModalPresentationId {
    ManageFeeds,
    Shortcuts,
    Equalizer,
    PlaybackPreferences,
    SleepTimer,
    PlaylistPicker,
    PlaylistRename,
    /// The playlist-delete confirmation.
    PlaylistDelete,
    TrackInfo,
    /// A podcast episode's details and full show notes.
    EpisodeDetails,
}

impl ModalPresentationId {
    pub const fn scope_key(self) -> &'static str {
        match self {
            Self::ManageFeeds => "music-manage-feeds",
            Self::Shortcuts => "music-shortcuts",
            Self::Equalizer => "music-equalizer",
            Self::PlaybackPreferences => crate::views::PLAYBACK_PREFERENCES_ID_PREFIX,
            Self::SleepTimer => "music-sleep-timer",
            Self::PlaylistPicker => "music-playlist-picker",
            Self::PlaylistRename => "music-playlist-rename",
            Self::PlaylistDelete => "music-playlist-delete",
            Self::TrackInfo => "music-track-info",
            Self::EpisodeDetails => "music-episode-details",
        }
    }
}

#[derive(Debug, Default)]
pub(crate) struct ModalFocusSet {
    manage_feeds: Option<ModalFocusLifecycle>,
    shortcuts: Option<ModalFocusLifecycle>,
    equalizer: Option<ModalFocusLifecycle>,
    playback_preferences: Option<ModalFocusLifecycle>,
    sleep_timer: Option<ModalFocusLifecycle>,
    playlist_picker: Option<ModalFocusLifecycle>,
    playlist_rename: Option<ModalFocusLifecycle>,
    playlist_delete: Option<ModalFocusLifecycle>,
    track_info: Option<ModalFocusLifecycle>,
    episode_details: Option<ModalFocusLifecycle>,
}

impl ModalFocusSet {
    fn slot_mut(&mut self, id: ModalPresentationId) -> &mut Option<ModalFocusLifecycle> {
        match id {
            ModalPresentationId::ManageFeeds => &mut self.manage_feeds,
            ModalPresentationId::Shortcuts => &mut self.shortcuts,
            ModalPresentationId::Equalizer => &mut self.equalizer,
            ModalPresentationId::PlaybackPreferences => &mut self.playback_preferences,
            ModalPresentationId::SleepTimer => &mut self.sleep_timer,
            ModalPresentationId::PlaylistPicker => &mut self.playlist_picker,
            ModalPresentationId::PlaylistRename => &mut self.playlist_rename,
            ModalPresentationId::PlaylistDelete => &mut self.playlist_delete,
            ModalPresentationId::TrackInfo => &mut self.track_info,
            ModalPresentationId::EpisodeDetails => &mut self.episode_details,
        }
    }

    pub(crate) fn get(&self, id: ModalPresentationId) -> Option<&ModalFocusLifecycle> {
        match id {
            ModalPresentationId::ManageFeeds => self.manage_feeds.as_ref(),
            ModalPresentationId::Shortcuts => self.shortcuts.as_ref(),
            ModalPresentationId::Equalizer => self.equalizer.as_ref(),
            ModalPresentationId::PlaybackPreferences => self.playback_preferences.as_ref(),
            ModalPresentationId::SleepTimer => self.sleep_timer.as_ref(),
            ModalPresentationId::PlaylistPicker => self.playlist_picker.as_ref(),
            ModalPresentationId::PlaylistRename => self.playlist_rename.as_ref(),
            ModalPresentationId::PlaylistDelete => self.playlist_delete.as_ref(),
            ModalPresentationId::TrackInfo => self.track_info.as_ref(),
            ModalPresentationId::EpisodeDetails => self.episode_details.as_ref(),
        }
    }

    /// A rendered presentation must have been armed by its open route.
    pub(crate) fn require(&self, id: ModalPresentationId) -> &ModalFocusLifecycle {
        self.get(id).expect("open modal has no focus lifecycle")
    }

    fn open(&mut self, id: ModalPresentationId, invoker: Option<Id>) -> Task<Message> {
        let (lifecycle, operation) = ModalFocusLifecycle::open::<Message>(id.scope_key(), invoker);
        *self.slot_mut(id) = Some(lifecycle);
        iced::advanced::widget::operate(operation)
    }

    fn take(&mut self, id: ModalPresentationId) -> Option<ModalFocusLifecycle> {
        self.slot_mut(id).take()
    }
}

/// Stable id of the Manage Feeds invoker. The view and lifecycle open route
/// share this constructor, so exact restoration cannot drift from the button.
pub(crate) fn manage_feeds_button_id() -> Id {
    enzui_core::identity::derive_id(crate::views::MANAGE_FEEDS_BUTTON_ID, "")
}

/// Focus repair for a non-modal queue-row removal. Modal restoration never
/// uses this escape hatch; it always goes through the stored lifecycle.
pub(crate) fn move_row_focus_task(id: Id) -> Task<Message> {
    use enzui_widgets::modal_host::restore_focus_operation;
    restore_focus_operation::<Message>(ModalHostEvent::Dismissed(DismissCause::Escape), Some(&id))
        .map_or_else(Task::none, iced::advanced::widget::operate)
}

impl MusicApp {
    pub(crate) fn any_modal_open(&self) -> bool {
        self.manage_feeds_open
            || self.shortcuts_open
            || self.eq_open
            || self.rg_presentation.is_open()
            || self.sleep_open
            || self.playlist_picker.is_some()
            || self.playlist_rename.is_some()
            || self.playlist_delete.is_some()
            || self.track_info_id.is_some()
            || self.episode_details.is_some()
    }

    pub(crate) fn open_modal_focus(
        &mut self,
        id: ModalPresentationId,
        invoker: Option<Id>,
    ) -> Task<Message> {
        self.modal_focus.open(id, invoker)
    }

    /// Route the exact event emitted by one host through that presentation's
    /// lifecycle. A dismissal tears down only this presentation, preserving a
    /// covered lifecycle so it becomes scoped again when revealed.
    pub(crate) fn reduce_modal_host(
        &mut self,
        id: ModalPresentationId,
        event: ModalHostEvent,
    ) -> Task<Message> {
        let Some(lifecycle) = self.modal_focus.get(id) else {
            return Task::none();
        };
        let outcome = lifecycle.reduce::<Message>(event);
        if outcome.dismissed().is_some() {
            self.close_modal_state(id);
            self.modal_focus.take(id);
        }
        outcome.into_task()
    }

    pub(crate) fn dismiss_modal_focus(
        &mut self,
        id: ModalPresentationId,
        cause: DismissCause,
    ) -> Task<Message> {
        self.reduce_modal_host(id, ModalHostEvent::Dismissed(cause))
    }

    /// Model/host disappearance deliberately performs no restore operation.
    pub(crate) fn close_modal_focus_for_host(&mut self, id: ModalPresentationId) -> Task<Message> {
        let Some(lifecycle) = self.modal_focus.take(id) else {
            return Task::none();
        };
        self.close_modal_state(id);
        lifecycle.close_for_host::<Message>().into_task()
    }

    fn close_modal_state(&mut self, id: ModalPresentationId) {
        match id {
            ModalPresentationId::ManageFeeds => self.manage_feeds_open = false,
            ModalPresentationId::Shortcuts => self.shortcuts_open = false,
            ModalPresentationId::Equalizer => self.eq_open = false,
            ModalPresentationId::PlaybackPreferences => {
                self.rg_presentation.dismiss();
            }
            ModalPresentationId::SleepTimer => self.sleep_open = false,
            ModalPresentationId::PlaylistPicker => self.playlist_picker = None,
            ModalPresentationId::PlaylistRename => self.playlist_rename = None,
            ModalPresentationId::PlaylistDelete => self.playlist_delete = None,
            ModalPresentationId::EpisodeDetails => self.episode_details = None,
            ModalPresentationId::TrackInfo => {
                self.track_info_id = None;
                self.track_details_service.cancel();
                self.track_details = None;
            }
        }
    }

    /// Run initial-focus operations for presentations seeded open at boot.
    pub(crate) fn arm_boot_modal_focus(&mut self) -> Task<Message> {
        let mut tasks = Vec::new();
        if self.manage_feeds_open {
            tasks.push(self.open_modal_focus(
                ModalPresentationId::ManageFeeds,
                Some(manage_feeds_button_id()),
            ));
        }
        if self.shortcuts_open {
            tasks.push(self.open_modal_focus(ModalPresentationId::Shortcuts, None));
        }
        if self.eq_open {
            tasks.push(self.open_modal_focus(ModalPresentationId::Equalizer, None));
        }
        if self.rg_presentation.is_open() {
            tasks.push(self.open_modal_focus(ModalPresentationId::PlaybackPreferences, None));
        }
        if self.sleep_open {
            tasks.push(self.open_modal_focus(ModalPresentationId::SleepTimer, None));
        }
        if self.playlist_picker.is_some() {
            tasks.push(self.open_modal_focus(ModalPresentationId::PlaylistPicker, None));
        }
        if self.playlist_rename.is_some() {
            tasks.push(self.open_modal_focus(ModalPresentationId::PlaylistRename, None));
        }
        if self.playlist_delete.is_some() {
            tasks.push(self.open_modal_focus(ModalPresentationId::PlaylistDelete, None));
        }
        if self.track_info_id.is_some() {
            tasks.push(self.open_modal_focus(ModalPresentationId::TrackInfo, None));
        }
        Task::batch(tasks)
    }

    /// Close only the painted topmost presentation. Covered lifecycle slots
    /// remain intact and resume ownership when their host is revealed.
    pub(crate) fn close_topmost_modal(&mut self, cause: DismissCause) -> Option<Task<Message>> {
        let id = if self.manage_feeds_open {
            ModalPresentationId::ManageFeeds
        } else if self.shortcuts_open {
            ModalPresentationId::Shortcuts
        } else if self.eq_open {
            ModalPresentationId::Equalizer
        } else if self.rg_presentation.is_open() {
            ModalPresentationId::PlaybackPreferences
        } else if self.sleep_open {
            ModalPresentationId::SleepTimer
        } else if self.playlist_picker.is_some() {
            ModalPresentationId::PlaylistPicker
        } else if self.playlist_rename.is_some() {
            ModalPresentationId::PlaylistRename
        } else if self.playlist_delete.is_some() {
            ModalPresentationId::PlaylistDelete
        } else if self.track_info_id.is_some() {
            ModalPresentationId::TrackInfo
        } else if self.episode_details.is_some() {
            ModalPresentationId::EpisodeDetails
        } else {
            return None;
        };
        Some(self.dismiss_modal_focus(id, cause))
    }

    pub(crate) fn reconcile_track_info_model(&mut self) -> Task<Message> {
        if self
            .track_info_id
            .is_some_and(|id| !self.library.tracks.contains_key(&id))
        {
            return self.close_modal_focus_for_host(ModalPresentationId::TrackInfo);
        }
        Task::none()
    }

    pub(crate) fn toggle_modal_focus(
        &mut self,
        now_open: bool,
        id: ModalPresentationId,
        invoker: Option<Id>,
    ) -> Task<Message> {
        if now_open {
            self.open_modal_focus(id, invoker)
        } else {
            self.dismiss_modal_focus(id, DismissCause::CloseButton)
        }
    }
}
