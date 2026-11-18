use crate::track_details::{Failure, Page, Presentation, State};
use crate::{MusicApp, ToastTone};

pub const RATES: &[f64] = &[0.5, 0.75, 1.0, 1.25, 1.5, 2.0];
pub const RATE_LABELS: &[&str] = &["0.5×", "0.75×", "1×", "1.25×", "1.5×", "2×"];

impl MusicApp {
    pub(crate) fn reload_track_details(&mut self) {
        self.track_details_service.cancel();
        let Some(track) = self
            .track_info_id
            .and_then(|id| self.library.tracks.get(&id))
        else {
            self.track_details = None;
            return;
        };
        let page = self
            .track_details
            .as_ref()
            .map(|details| details.page)
            .unwrap_or(Page::Info);
        let path = track.path.clone();
        let Some(generation) = self.track_details_generation.checked_add(1) else {
            self.cancel_track_details(Failure::Unavailable);
            return;
        };
        self.track_details_generation = generation;
        let state = match self.track_details_service.submit(generation, path.clone()) {
            Ok(()) => State::Loading,
            Err(error) => State::Failed(error),
        };
        self.track_details = Some(Presentation {
            generation,
            path,
            page,
            state,
        });
    }

    pub(crate) fn cancel_track_details(&mut self, reason: Failure) {
        self.track_details_service.cancel();
        if let Some(details) = self.track_details.as_mut() {
            details.state = State::Failed(reason);
        }
    }

    pub(crate) fn drain_track_details(&mut self) {
        for completion in self.track_details_service.drain() {
            self.apply_track_details(completion);
        }
    }

    pub(crate) fn apply_track_details(&mut self, completion: crate::track_details::Completion) {
        let Some(details) = self.track_details.as_mut() else {
            return;
        };
        if completion.generation != details.generation
            || completion.path != details.path
            || !matches!(details.state, State::Loading)
        {
            return;
        }
        if !self
            .track_info_id
            .and_then(|id| self.library.tracks.get(&id))
            .is_some_and(|track| track.path == details.path)
        {
            details.state = State::Failed(Failure::Stale);
            return;
        }
        details.state = match completion.result {
            Ok(inspection) => State::Ready(inspection),
            Err(error) => State::Failed(error),
        };
    }

    pub(crate) fn request_playback_rate(&mut self, rate: f64) {
        let result = self
            .audio
            .as_ref()
            .ok_or("Audio output is unavailable.")
            .and_then(|audio| {
                audio.set_rate(rate, &mut self.media).map_err(
                    |_| "Playback speed request was refused. Try again from the Playback speed menu.",
                )
            });
        match result {
            Ok(()) => {
                // An admitted user choice during preroll supersedes automatic policy reissue.
                self.rate_requested_generation = Some(self.media.generation().raw());
            }
            Err(error) => self.post_folder_status(error, ToastTone::Warning),
        }
    }
}
