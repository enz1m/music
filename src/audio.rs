//! App-side wiring around `enzui_services::media::AudioService`.
//!
//! Owns the framework [`AudioService`] (with `MediaGate::DEFAULT` remote
//! policy), a once-per-[`Generation`] threshold-completion gate, and the bridge
//! that applies accepted events to the caller's [`MediaState`] and translates
//! them into [`BridgeEvent`]s. The reducer owns the [`MediaState`]; a
//! `Message::PlaybackCompleted` whose generation is not current is rejected.
//!
//! Every source swap goes through
//! [`AudioService::load_into`](enzui_services::media::AudioService::load_into),
//! which advances both generations atomically on admission and cancels the
//! previous source's gate and preroll work. A refused admission changes
//! neither generation.
//!
//! Autoplay loads request [`MediaCommand::Play`] only on the matching
//! `SourceReady`, so a replaced source cannot start late.
//! [`CompletionReason::ThresholdCrossed`] fires at most once per generation.

use std::path::PathBuf;
use std::time::Duration;

use enzui_core::url_policy::{UrlPolicy, UrlVerdict};
use enzui_services::media::{
    AudioEventReceiver, AudioService, AudioServiceConfig, Generation, LoadRefused, MediaCommand,
    MediaError, MediaEvent, MediaEventKind, MediaSource, MediaState, PlaybackState, SendError,
};

/// Absolute floor in seconds for a `ThresholdCrossed` completion.
///
/// The effective threshold is `min(SCROBBLE_ABSOLUTE_FLOOR_SECS,
/// duration_secs * 0.5)`; without a confirmed duration the floor applies
/// alone. [`PlaybackCompletionReason`](crate::PlaybackCompletionReason)
/// documents the same rule.
pub const SCROBBLE_ABSOLUTE_FLOOR_SECS: f64 = 240.0;

// Public bridge types

/// A translated backend event the reducer applies to app-visible state.
///
/// Each variant maps to an event [`MediaState::apply`] already accepted, so it
/// always belongs to the current generation. Times are `f64` seconds.
#[derive(Debug, Clone, PartialEq)]
pub enum BridgeEvent {
    /// The source reached preroll; `duration_secs` is `0.0` when unknown.
    /// `generation` lets the reducer confirm availability only for the
    /// matching source open.
    SourceReady { generation: u64, duration_secs: f64 },
    /// The pipeline entered `Playing`.
    PlayingConfirmed,
    /// The pipeline entered `Paused` or `Stopped`.
    NotPlaying,
    /// The pipeline reported a new position.
    PositionUpdated { secs: f64 },
    /// The volume element reported a change (mixer or explicit command).
    VolumeChanged { volume: f64 },
    /// The volume element reported a mute change.
    MuteChanged(bool),
    /// Emitted only after the native rate change succeeds.
    RateChanged { rate: f64 },
    /// An authoritative completion for the framework `Generation::raw()`.
    Completed {
        generation: u64,
        reason: CompletionReason,
    },
    /// A typed backend error for the given `Generation::raw()`.
    ///
    /// The reducer reports the failure, preserves playback for refused controls,
    /// and folds source-missing or unsupported outcomes into availability.
    Error { generation: u64, error: MediaError },
}

/// The completion reason, shape-identical to
/// [`PlaybackCompletionReason`](crate::PlaybackCompletionReason).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CompletionReason {
    EndOfStream,
    ThresholdCrossed {
        position_secs: f64,
        duration_secs: f64,
    },
}

/// A refused remote load.
#[derive(Debug)]
pub enum LoadRemoteError {
    /// The URL failed pre-DNS policy in [`MediaSource::remote`].
    Url(UrlVerdict),
    /// The service refused admission (channel full or disconnected).
    Load(LoadRefused),
}

// Playback

/// The app-side owner of the framework audio service.
pub struct Playback {
    service: AudioService,
    effects_requested: Option<(Generation, enzui_services::media::AudioEffects)>,
    controls_requested_generation: Option<Generation>,
    /// Set by an autoplay load on admission and cleared when its `SourceReady`
    /// arrives; a newer load overwrites it, so a stale `SourceReady` cannot
    /// start the wrong track.
    pending_play_generation: Option<Generation>,
    /// The generation that already emitted
    /// [`CompletionReason::ThresholdCrossed`].
    threshold_fired_generation: Option<Generation>,
    /// Pre-DNS URL policy for remote source strings; the service applies the
    /// resolved-address and redirect policy itself.
    url_policy: UrlPolicy,
}

/// The worker's remote-media gate: the framework default with `url_policy`'s
/// URL rules, so the gate admits what the pre-DNS check admitted.
fn stream_gate(url_policy: UrlPolicy) -> enzui_core::media::remote_gate::MediaGate {
    enzui_core::media::remote_gate::MediaGate {
        policy: url_policy,
        ..enzui_core::media::remote_gate::MediaGate::DEFAULT
    }
}

impl Playback {
    /// Spawn the pipeline thread.
    ///
    /// Returns the owner and the receiver whose
    /// [`AudioEventReceiver::subscription`] the app must subscribe to.
    ///
    /// # Errors
    ///
    /// Returns [`MediaError`] when GStreamer cannot initialise or the worker
    /// thread cannot spawn; the app then runs without audio.
    pub fn spawn(url_policy: UrlPolicy) -> Result<(Self, AudioEventReceiver), MediaError> {
        let (service, receiver) = AudioService::new(AudioServiceConfig {
            gate: stream_gate(url_policy),
            ..AudioServiceConfig::default()
        })?;
        Ok((
            Playback {
                service,
                effects_requested: None,
                controls_requested_generation: None,
                pending_play_generation: None,
                threshold_fired_generation: None,
                url_policy,
            },
            receiver,
        ))
    }

    /// The receiver-driven subscription that wakes the runtime on accepted
    /// events without a polling timer.
    ///
    /// See [`enzui_services::media::audio_service`] for the single-waker
    /// contract.
    pub fn subscription(
        receiver: &AudioEventReceiver,
    ) -> iced::Subscription<enzui_services::media::AudioServiceWake> {
        receiver.subscription()
    }

    /// Load a local track and play once its `SourceReady` arrives.
    ///
    /// On admission both generations advance and the previous source's work
    /// is cancelled.
    ///
    /// # Errors
    ///
    /// [`LoadRefused::ChannelFull`] on backpressure and
    /// [`LoadRefused::Disconnected`] when the service thread has exited.
    /// Neither changes any generation.
    pub fn load_local_and_autoplay(
        &mut self,
        path: PathBuf,
        state: &mut MediaState,
    ) -> Result<Generation, LoadRefused> {
        let source = MediaSource::local(path);
        let generation = self.service.load_into(source, state)?;
        self.pending_play_generation = Some(generation);
        self.threshold_fired_generation = None;
        Ok(generation)
    }

    /// Load a local track and stay paused, for session restore.
    ///
    /// The reducer seeks to the saved playhead with [`Self::seek`] once
    /// `SourceReady` arrives.
    ///
    /// # Errors
    ///
    /// Same as [`Self::load_local_and_autoplay`].
    pub fn load_local_paused(
        &mut self,
        path: PathBuf,
        state: &mut MediaState,
    ) -> Result<Generation, LoadRefused> {
        let source = MediaSource::local(path);
        let generation = self.service.load_into(source, state)?;
        // Restore never autoplays.
        self.pending_play_generation = None;
        self.threshold_fired_generation = None;
        Ok(generation)
    }

    /// Load a remote source such as a podcast episode, then autoplay.
    ///
    /// [`MediaSource::remote`] applies the pre-DNS URL policy; the worker then
    /// applies `MediaGate::DEFAULT`, and a newer load cancels in-flight gate work.
    ///
    /// # Errors
    ///
    /// [`LoadRemoteError::Url`] when the URL policy refuses the string, or
    /// [`LoadRemoteError::Load`] when admission fails.
    pub fn load_remote_and_autoplay(
        &mut self,
        url: &str,
        state: &mut MediaState,
    ) -> Result<Generation, LoadRemoteError> {
        let source = MediaSource::remote(url, &self.url_policy).map_err(LoadRemoteError::Url)?;
        let generation = self
            .service
            .load_into(source, state)
            .map_err(LoadRemoteError::Load)?;
        self.pending_play_generation = Some(generation);
        self.threshold_fired_generation = None;
        Ok(generation)
    }

    /// Send [`MediaCommand::Play`], stamped with the current generation so a
    /// source swap invalidates it.
    pub fn play(&self, state: &mut MediaState) -> Result<(), SendError> {
        self.service.send(&state.dispatch(&MediaCommand::Play))
    }

    /// Send [`MediaCommand::Pause`]. Same stamping rule as [`Self::play`].
    pub fn pause(&self, state: &mut MediaState) -> Result<(), SendError> {
        self.service.send(&state.dispatch(&MediaCommand::Pause))
    }

    /// Send [`MediaCommand::Stop`]. Same stamping rule as [`Self::play`].
    pub fn stop(&self, state: &mut MediaState) -> Result<(), SendError> {
        self.service.send(&state.dispatch(&MediaCommand::Stop))
    }

    /// Send [`MediaCommand::Seek`]. An unseekable source reports
    /// `MediaErrorKind::SeekFailed`.
    pub fn seek(&self, position: Duration, state: &mut MediaState) -> Result<(), SendError> {
        self.service
            .send(&state.dispatch(&MediaCommand::Seek(position)))
    }

    pub(crate) fn sync_listener_controls(
        &mut self,
        volume: f64,
        muted: bool,
        state: &mut MediaState,
    ) -> Result<(), SendError> {
        let snapshot = state.snapshot();
        if snapshot.volume as f32 == volume as f32 && snapshot.muted == muted {
            return Ok(());
        }
        let generation = state.generation();
        if self.controls_requested_generation == Some(generation) {
            return Ok(());
        }
        self.set_volume(volume, state)?;
        self.set_mute(muted, state)?;
        self.controls_requested_generation = Some(generation);
        Ok(())
    }

    pub(crate) fn set_effects(
        &mut self,
        effects: enzui_services::media::AudioEffects,
        state: &MediaState,
    ) -> Result<(), SendError> {
        let generation = state.generation();
        if self.effects_requested == Some((generation, effects)) {
            return Ok(());
        }
        self.service.set_effects(generation, effects)?;
        self.effects_requested = Some((generation, effects));
        Ok(())
    }

    /// Send [`MediaCommand::SetVolume`]; the framework clamps to `[0.0, 1.0]`
    /// and maps NaN to 0.0.
    pub fn set_volume(&self, volume: f64, state: &mut MediaState) -> Result<(), SendError> {
        self.service
            .send(&state.dispatch(&MediaCommand::SetVolume(volume)))
    }

    /// Send [`MediaCommand::SetMute`].
    pub fn set_mute(&self, muted: bool, state: &mut MediaState) -> Result<(), SendError> {
        self.service
            .send(&state.dispatch(&MediaCommand::SetMute(muted)))
    }

    /// Request a playback rate; reduced state changes only after confirmation.
    ///
    /// Admission rejects non-finite and out-of-range values with
    /// `SendError::InvalidRate`.
    pub fn set_rate(&self, rate: f64, state: &mut MediaState) -> Result<(), SendError> {
        self.service
            .send(&state.dispatch(&MediaCommand::SetRate(rate)))
    }

    /// Drain the receiver, apply each event to `state`, and translate accepted
    /// events into [`BridgeEvent`]s in arrival order.
    ///
    /// `confirmed_duration_secs` is called only when a position update may
    /// cross the play-credit threshold. Stale events emit nothing. Each
    /// admitted autoplay load issues exactly one Play, and a threshold
    /// completion fires at most once per generation.
    pub fn drain_and_translate<F>(
        &mut self,
        receiver: &AudioEventReceiver,
        state: &mut MediaState,
        confirmed_duration_secs: F,
    ) -> Vec<BridgeEvent>
    where
        F: Fn() -> f64,
    {
        let events = receiver.drain();
        let mut out = Vec::with_capacity(events.len());
        for event in events {
            self.translate_one(&event, state, &confirmed_duration_secs, &mut out);
        }
        out
    }

    fn translate_one<F>(
        &mut self,
        event: &MediaEvent,
        state: &mut MediaState,
        confirmed_duration_secs: &F,
        out: &mut Vec<BridgeEvent>,
    ) where
        F: Fn() -> f64,
    {
        // A stale event neither mutates reduced state nor emits a bridge event.
        if !state.apply(event) {
            return;
        }
        match &event.kind {
            MediaEventKind::SourceReady { duration, .. } => {
                let duration_secs = duration.map(duration_to_secs).unwrap_or(0.0);
                out.push(BridgeEvent::SourceReady {
                    generation: event.generation.raw(),
                    duration_secs,
                });
                if self.pending_play_generation == Some(event.generation) {
                    self.pending_play_generation = None;
                    // A disconnected worker surfaces through a later event.
                    let _ = self.service.send(&state.dispatch(&MediaCommand::Play));
                }
            }
            MediaEventKind::StateChanged(PlaybackState::Playing) => {
                out.push(BridgeEvent::PlayingConfirmed);
            }
            MediaEventKind::StateChanged(PlaybackState::Paused)
            | MediaEventKind::StateChanged(PlaybackState::Stopped) => {
                out.push(BridgeEvent::NotPlaying);
            }
            MediaEventKind::StateChanged(PlaybackState::EndOfStream) => {
                out.push(BridgeEvent::NotPlaying);
                out.push(BridgeEvent::Completed {
                    generation: event.generation.raw(),
                    reason: CompletionReason::EndOfStream,
                });
            }
            MediaEventKind::PositionUpdated(pos) => {
                let position_secs = duration_to_secs(*pos);
                out.push(BridgeEvent::PositionUpdated {
                    secs: position_secs,
                });
                let already_fired = self.threshold_fired_generation == Some(event.generation);
                let duration_secs = confirmed_duration_secs();
                if state.snapshot().playback == PlaybackState::Playing
                    && let Some(true) =
                        threshold_decision(already_fired, position_secs, duration_secs)
                {
                    self.threshold_fired_generation = Some(event.generation);
                    out.push(BridgeEvent::Completed {
                        generation: event.generation.raw(),
                        reason: CompletionReason::ThresholdCrossed {
                            position_secs,
                            duration_secs,
                        },
                    });
                }
            }
            // A confirmed seek lands here: the position moves, but a seek is
            // not listening, so it never completes the track.
            MediaEventKind::PositionDiscontinuity(pos) => {
                out.push(BridgeEvent::PositionUpdated {
                    secs: duration_to_secs(*pos),
                });
            }
            MediaEventKind::VolumeChanged { volume } => {
                out.push(BridgeEvent::VolumeChanged { volume: *volume });
            }
            MediaEventKind::MuteChanged(m) => {
                out.push(BridgeEvent::MuteChanged(*m));
            }
            MediaEventKind::RateChanged { rate } => {
                if enzui_services::media::sanitize_rate(*rate).is_some() {
                    out.push(BridgeEvent::RateChanged { rate: *rate });
                }
            }
            MediaEventKind::Error(err) => {
                out.push(BridgeEvent::Error {
                    generation: event.generation.raw(),
                    error: err.clone(),
                });
            }
        }
    }
}

#[inline]
fn duration_to_secs(d: Duration) -> f64 {
    d.as_secs_f64()
}

/// Whether a position qualifies for play credit.
///
/// Returns `Some(true)` when it qualifies, `Some(false)` when not, and `None`
/// when the generation already fired.
#[must_use]
pub(crate) fn threshold_decision(
    already_fired: bool,
    position_secs: f64,
    duration_secs: f64,
) -> Option<bool> {
    if already_fired {
        return None;
    }
    let half = if duration_secs > 0.0 {
        duration_secs * 0.5
    } else {
        f64::INFINITY
    };
    let threshold = SCROBBLE_ABSOLUTE_FLOOR_SECS.min(half);
    Some(position_secs >= threshold)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn end_of_stream_projects_not_playing_before_completion() {
        let (mut playback, _) = Playback::spawn(UrlPolicy::default()).expect("idle audio service");
        let mut state = MediaState::new();
        let generation =
            state.replace_source(MediaSource::local(std::path::PathBuf::from("/unused.wav")));
        let mut events = Vec::new();
        playback.translate_one(
            &MediaEvent {
                generation,
                kind: MediaEventKind::StateChanged(PlaybackState::EndOfStream),
            },
            &mut state,
            &|| 3.0,
            &mut events,
        );
        assert!(matches!(
            events.as_slice(),
            [BridgeEvent::NotPlaying, BridgeEvent::Completed { .. }]
        ));
    }

    #[test]
    fn threshold_qualifies_at_or_above_the_documented_scrobble_point() {
        // With a 600s duration, threshold is min(240, 300) = 240.
        assert_eq!(threshold_decision(false, 239.9, 600.0), Some(false));
        assert_eq!(threshold_decision(false, 240.0, 600.0), Some(true));
        assert_eq!(threshold_decision(false, 500.0, 600.0), Some(true));

        // With a 300s duration, threshold is min(240, 150) = 150.
        assert_eq!(threshold_decision(false, 149.9, 300.0), Some(false));
        assert_eq!(threshold_decision(false, 150.0, 300.0), Some(true));

        // With no known duration, only the 240s floor gates.
        assert_eq!(threshold_decision(false, 239.9, 0.0), Some(false));
        assert_eq!(threshold_decision(false, 240.0, 0.0), Some(true));

        // Already fired for this generation — no more decisions.
        assert_eq!(threshold_decision(true, 999.0, 600.0), None);
    }
}
