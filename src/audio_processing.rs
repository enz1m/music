use crate::domain::SleepTimerState;
use crate::{MusicApp, ToastTone};
use enzui_services::media::{AudioEffects, MAX_PROCESSING_GAIN, MIN_PROCESSING_GAIN};

impl MusicApp {
    pub(crate) fn desired_audio_effects(&self) -> AudioEffects {
        let equalizer_gains = self.eq.bands.map(|gain| {
            if gain.is_finite() {
                f64::from(gain.clamp(-12.0, 12.0))
            } else {
                0.0
            }
        });
        let equalizer_gain = if self.eq.enabled {
            let preamp = if self.eq.preamp_db.is_finite() {
                self.eq.preamp_db.clamp(-12.0, 12.0)
            } else {
                0.0
            };
            10.0_f32.powf(preamp / 20.0) * self.eq.clip_scale()
        } else {
            1.0
        };
        let tags = self
            .playback
            .current_track(&self.library)
            .map(|track| track.replay_gain)
            .unwrap_or_default();
        let fade = if self.sleep_pause_pending {
            0.0
        } else {
            match self.sleep_timer {
                SleepTimerState::Armed {
                    remaining_secs,
                    fade: true,
                } => {
                    let elapsed = self
                        .sleep_last_tick
                        .map(|at| at.elapsed().as_secs_f64())
                        .unwrap_or(0.0);
                    ((remaining_secs - elapsed) / crate::domain::SLEEP_FADE_SECS).clamp(0.0, 1.0)
                }
                SleepTimerState::UntilTrackEnd { fade: true }
                    if self.playback.duration_secs > 0.0 =>
                {
                    ((self.playback.duration_secs - self.playback.position_secs)
                        / crate::domain::SLEEP_FADE_SECS)
                        .clamp(0.0, 1.0)
                }
                _ => 1.0,
            }
        };
        let gain = (f64::from(equalizer_gain * self.rg.scalar_for(&tags)) * fade)
            .clamp(MIN_PROCESSING_GAIN, MAX_PROCESSING_GAIN);
        AudioEffects::new(self.eq.enabled, equalizer_gains, gain)
            .expect("processing policy emits finite bounded effects")
    }

    pub(crate) fn sync_audio_processing(&mut self) {
        // Only a pipeline that exists takes them: sent while a stream still
        // connects they were refused, and never sent again for that source.
        if !self.audio_pipeline_live()
            || self.media.snapshot().source.is_none()
            || self.media.snapshot().playback.is_terminal()
        {
            return;
        }
        let effects = self.desired_audio_effects();
        if let Some(audio) = self.audio.as_mut()
            && (audio
                .sync_listener_controls(
                    f64::from(self.playback.volume),
                    self.playback.muted,
                    &mut self.media,
                )
                .and_then(|()| audio.set_effects(effects, &self.media)))
            .is_err()
        {
            self.post_folder_status(
                "Audio processing is busy; the settings will be retried.",
                ToastTone::Warning,
            );
        }
    }
}
