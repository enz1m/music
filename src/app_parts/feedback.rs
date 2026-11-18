//! Chrome queries and notice feedback rows.

use super::*;

/// Spotify notifications replace one another.
const SPOTIFY_COALESCE_KEY: &str = "music-spotify";

impl MusicApp {
    // Also sizes `content_pane`, so a measured inset cannot outlive its chrome.
    pub(crate) fn shows_bottom_chrome(&self) -> bool {
        true
    }

    // Bottom chrome height, or 0; `ToastOverlayPlacement::bottom_inset` rejects
    // non-finite and negative values.
    pub(crate) fn toast_bottom_inset(&self) -> f32 {
        if !self.shows_bottom_chrome() {
            return 0.0;
        }
        self.bottom_chrome_height
    }

    /// The only writer of the shared transient-feedback host.
    ///
    /// `Warning` announces as `Role::Alert`; `Info` and `Success` as
    /// `Role::Status` (see `ToastTone::announcement`). The host's shared dwell
    /// applies, and [`LIBRARY_STATUS_COALESCE_KEY`] makes a newer notice
    /// replace the visible one. `_shared/check-toast-expiry-adoption.py`
    /// rejects producers that bypass this.
    pub(crate) fn post_folder_status(&mut self, text: impl Into<String>, tone: ToastTone) {
        self.toasts.push(
            enzui_widgets::toast_host::ToastSpec::new(text)
                .tone(tone)
                .coalesce_key(LIBRARY_STATUS_COALESCE_KEY),
        );
    }

    /// Spotify's outcomes and failures, as transient notifications. A newer
    /// one replaces the visible one.
    pub(crate) fn post_spotify_notices(&mut self) {
        for text in self.spotify.take_notices() {
            let tone = if text.starts_with("Added to") {
                ToastTone::Success
            } else {
                ToastTone::Warning
            };
            self.toasts.push(
                enzui_widgets::toast_host::ToastSpec::new(text)
                    .tone(tone)
                    .coalesce_key(SPOTIFY_COALESCE_KEY),
            );
        }
    }

    /// The standing notices that are not hidden, in stack order.
    ///
    /// They float bottom-right with the transient toasts (see `view_root`), so
    /// every destination sees the same single instance. Storage notices come
    /// first because only the first error (or the first notice) is the live
    /// region.
    pub(crate) fn service_notices(&self) -> Vec<ServiceNotice> {
        self.standing_notices()
            .filter(|notice| self.notice_applies_here(notice.id))
            .filter(|notice| !self.hidden_notices.contains(&notice.hide_key()))
            .collect()
    }

    /// Whether a standing notice belongs to the current destination: the radio
    /// catalog status only on Radio Discover, and the missing audio service
    /// everywhere but Spotify (which plays on its own) unless the player is up.
    fn notice_applies_here(&self, id: &str) -> bool {
        match id {
            RADIO_CATALOG_NOTICE_ID => self.current_view == View::Radio,
            NO_AUDIO_OUTPUT_NOTICE_ID => {
                self.current_view != View::Spotify || self.now_playing_open
            }
            _ => true,
        }
    }

    /// Every standing notice, hidden or not and wherever it applies, in stack
    /// order.
    fn standing_notices(&self) -> impl Iterator<Item = ServiceNotice> {
        [
            self.persistence_notice_card(),
            self.folder_failure_notice(),
            self.audio_output_notice(),
            self.watch_status_notice(),
            self.mpris_failure_notice(),
            self.radio_catalog_notice(),
        ]
        .into_iter()
        .flatten()
    }

    /// The standing notices as one bottom-right stack, or `None`.
    pub(crate) fn notices_feedback(&self) -> Option<Element<'_, Message>> {
        use enzui_widgets::notice_stack::{EnzNotice, enz_notice_stack};

        let notices: Vec<EnzNotice<Message>> = self
            .service_notices()
            .into_iter()
            .map(|notice| {
                let mut card = EnzNotice::new(notice.id, notice.title, notice.tone)
                    .description(notice.body)
                    .on_dismiss(notice.dismiss);
                if let Some((label, message)) = notice.action {
                    card = card.action(label, message);
                }
                if let Some((label, message)) = notice.secondary {
                    card = card.secondary_action(label, message);
                }
                card
            })
            .collect();
        (!notices.is_empty()).then(|| enz_notice_stack(notices).into_element())
    }

    /// Hide the notice `id` until what it reports changes.
    pub(crate) fn hide_notice(&mut self, id: &'static str) {
        let key = self
            .standing_notices()
            .find(|notice| notice.id == id)
            .map(|notice| notice.hide_key());
        if let Some(key) = key
            && !self.hidden_notices.contains(&key)
        {
            self.hidden_notices.push(key);
        }
    }

    /// Forget hidden notices whose state has changed or cleared, so a new
    /// failure (or the same one coming back later) shows again. Moving between
    /// destinations does not count as a change.
    pub(crate) fn prune_hidden_notices(&mut self) {
        if self.hidden_notices.is_empty() {
            return;
        }
        let live: Vec<(&'static str, String)> = self
            .standing_notices()
            .map(|notice| notice.hide_key())
            .collect();
        self.hidden_notices.retain(|key| live.contains(key));
    }

    fn radio_catalog_notice(&self) -> Option<ServiceNotice> {
        use crate::radio::RadioCatalogState;

        let notice = match &self.radio.catalog_status {
            RadioCatalogState::Idle | RadioCatalogState::Loading { .. } => ServiceNotice::new(
                RADIO_CATALOG_NOTICE_ID,
                "Loading radio stations",
                "Finding stations for you. You can browse your library while this finishes.",
                ToastTone::Info,
            ),
            RadioCatalogState::Failed { message, retryable } => {
                let notice = ServiceNotice::new(
                    RADIO_CATALOG_NOTICE_ID,
                    "Radio stations unavailable",
                    message.clone(),
                    ToastTone::Warning,
                );
                if *retryable {
                    notice.action("Retry", Message::RadioCatalogRetry)
                } else {
                    notice
                }
            }
            RadioCatalogState::Loaded { .. } => return None,
        };
        Some(notice)
    }

    fn mpris_failure_notice(&self) -> Option<ServiceNotice> {
        let notice = self.mpris.notice()?;
        Some(ServiceNotice::new(
            MPRIS_NOTICE_ID,
            "Desktop media controls unavailable",
            notice,
            ToastTone::Warning,
        ))
    }

    /// The standing "automatic updates unavailable or partial" notice with a
    /// manual Rescan; absent while every configured root is live.
    fn watch_status_notice(&self) -> Option<ServiceNotice> {
        let text = self.library_watch.status_notice()?;
        Some(
            ServiceNotice::new(
                WATCH_STATUS_BANNER_ID,
                "Automatic library updates are limited",
                text,
                ToastTone::Warning,
            )
            .action("Rescan Library", Message::RescanLibrary),
        )
    }

    /// The missing audio service, disclosed even before a track is selected.
    fn audio_output_notice(&self) -> Option<ServiceNotice> {
        self.audio.is_none().then(|| {
            ServiceNotice::new(
                NO_AUDIO_OUTPUT_NOTICE_ID,
                crate::views::NO_AUDIO_OUTPUT_TITLE,
                crate::views::NO_AUDIO_OUTPUT_BODY,
                ToastTone::Warning,
            )
        })
    }

    /// The standing folder-chooser failure, or `None`.
    ///
    /// It uses `Warning` (assertive `Role::Alert`), not `Error`, which is
    /// reserved for storage notices where saved data is at stake. Closing it
    /// clears the failure; the next Add Folder attempt also does.
    pub(crate) fn folder_failure_notice(&self) -> Option<ServiceNotice> {
        let text = self.folder_failure.as_ref()?;
        let mut notice = ServiceNotice::new(
            FOLDER_FAILURE_BANNER_ID,
            FOLDER_FAILURE_TITLE,
            text.clone(),
            ToastTone::Warning,
        )
        // Retry repeats Add Folder: it clears the failure and asks the portal
        // again.
        .action(FOLDER_FAILURE_RETRY_ACTION, Message::AddFolder);
        notice.dismiss = Message::DismissFolderFailure;
        Some(notice)
    }

    /// The single storage notice; raw I/O diagnostics never enter it.
    fn persistence_notice_card(&self) -> Option<ServiceNotice> {
        let notice = self.persistence_notice.as_ref()?;
        let card = |title: &'static str, body: String, tone: ToastTone| {
            ServiceNotice::new(PERSISTENCE_NOTICE_ID, title, body, tone)
        };
        let card = match notice {
            PersistenceNotice::RecoveredFromCorrupt(Some(_)) => {
                card(
                    "Your saved music data could not be read",
                    "The unreadable file was kept as a recovery copy. Music started with empty saved data, and new changes can be saved.".to_owned(),
                    ToastTone::Warning,
                )
            }
            PersistenceNotice::RecoveredFromCorrupt(None) => {
                card(
                    "Your saved music data could not be read",
                    "The unreadable file could not be moved aside. Saving may overwrite it; copy it before making changes if you need the original data.".to_owned(),
                    ToastTone::Error,
                )
            }
            PersistenceNotice::ReadOnlyFutureVersion(version) => {
                card(
                    "Music data is read-only",
                    format!(
                        "This file was written by a newer app format (v{version}). Nothing you change in this session will be saved."
                    ),
                    ToastTone::Error,
                )
            }
            PersistenceNotice::RefusedReadOnly(kind) => {
                let reason = match kind {
                    persistence::RefusalKind::Unreadable => {
                        "Your saved music file is not a normal readable file, so it was left untouched."
                    }
                    persistence::RefusalKind::TooLarge => {
                        "Your saved music file is too large to load safely, so it was left untouched."
                    }
                    persistence::RefusalKind::OverLimit => {
                        "Your saved music file holds more than this app can load, so it was left untouched."
                    }
                    persistence::RefusalKind::Corrupt => {
                        "Your saved music file is damaged and could not be read, so it was left untouched."
                    }
                };
                let card = card(
                    "Music data is read-only",
                    format!("{reason} Nothing you change in this session will be saved."),
                    ToastTone::Error,
                );
                // Damaged bytes are recoverable only by an explicit Retire, which
                // moves them aside through the identity-bound quarantine.
                if matches!(kind, persistence::RefusalKind::Corrupt) {
                    card.action(PERSISTENCE_RETIRE_ACTION, Message::RetirePersistence)
                } else {
                    card
                }
            }
            PersistenceNotice::RetireRefused(reason) => {
                let detail = match reason {
                    persistence::RetireRefusal::NoExpectedIdentity => {
                        "Music no longer knows which file to recover, so it was left untouched."
                    }
                    persistence::RetireRefusal::SourceMismatch => {
                        "The file changed since it was read, so it was left untouched."
                    }
                    persistence::RetireRefusal::CollisionExhausted => {
                        "Music could not find a free recovery name."
                    }
                    persistence::RetireRefusal::SourceReplaced => {
                        "The file was replaced by another program before it could be moved."
                    }
                    persistence::RetireRefusal::DurabilityUncertain => {
                        "The recovery move could not be confirmed on disk."
                    }
                    persistence::RetireRefusal::PartialCustody => {
                        "The recovery move could only be partly verified."
                    }
                    persistence::RetireRefusal::Unsupported => {
                        "This system does not support safe recovery of the file."
                    }
                    persistence::RetireRefusal::Refused => {
                        "The saved file could not be recovered safely."
                    }
                    persistence::RetireRefusal::AuthorityUnavailable => {
                        "A new saved file could not be opened after recovery."
                    }
                };
                card(
                    "Music data could not be recovered",
                    format!("{detail} Nothing you change in this session will be saved."),
                    ToastTone::Error,
                )
                .action(PERSISTENCE_RETIRE_ACTION, Message::RetirePersistence)
            }
            PersistenceNotice::PersistenceRootUnavailable(refusal) => {
                let reason = match refusal {
                    persistence::RootRefusal::NoDataHome => {
                        "This system has no private data folder for Music to use."
                    }
                    persistence::RootRefusal::Unusable => {
                        "Music's data folder can't be used safely on this system."
                    }
                };
                card(
                    "Changes are not saved",
                    format!("{reason} Nothing you change in this session will be saved."),
                    ToastTone::Error,
                )
            }
            PersistenceNotice::SaveFailed(summary) => {
                let card = card("Changes are not saved", summary.clone(), ToastTone::Error)
                    .action(PERSISTENCE_RETRY_ACTION, Message::RetryPersistence);
                // A vetoed close offers an explicit discard route beside Retry.
                if self.close_after_save {
                    card.secondary(DISCARD_AND_CLOSE_ACTION, Message::DiscardUnsavedAndClose)
                } else {
                    card
                }
            }
        };
        Some(card)
    }
}

/// One standing notice in the bottom-right stack.
///
/// Closing a notice hides it until what it reports changes (its title or body
/// differs), unless [`Self::dismiss`] clears the underlying state itself.
pub(crate) struct ServiceNotice {
    pub(crate) id: &'static str,
    pub(crate) title: &'static str,
    pub(crate) body: String,
    pub(crate) tone: ToastTone,
    pub(crate) action: Option<(&'static str, Message)>,
    pub(crate) secondary: Option<(&'static str, Message)>,
    pub(crate) dismiss: Message,
}

impl ServiceNotice {
    fn new(
        id: &'static str,
        title: &'static str,
        body: impl Into<String>,
        tone: ToastTone,
    ) -> Self {
        Self {
            id,
            title,
            body: body.into(),
            tone,
            action: None,
            secondary: None,
            dismiss: Message::HideNotice(id),
        }
    }

    fn action(mut self, label: &'static str, message: Message) -> Self {
        self.action = Some((label, message));
        self
    }

    fn secondary(mut self, label: &'static str, message: Message) -> Self {
        self.secondary = Some((label, message));
        self
    }

    /// What a hidden notice remembers: its slot, what it said and whether it
    /// offered a second route (a vetoed close adds Discard, which shows again).
    fn hide_key(&self) -> (&'static str, String) {
        (
            self.id,
            format!(
                "{}\n{}\n{}",
                self.title,
                self.body,
                self.secondary.is_some()
            ),
        )
    }
}

const RADIO_CATALOG_NOTICE_ID: &str = "music-radio-catalog-status";
const MPRIS_NOTICE_ID: &str = "music-mpris-unavailable";
const NO_AUDIO_OUTPUT_NOTICE_ID: &str = "music-no-audio-output";
pub(crate) const PERSISTENCE_NOTICE_ID: &str = "music-persistence";
