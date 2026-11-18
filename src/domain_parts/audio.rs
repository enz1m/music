//! Equalizer, replay gain, and sleep timer.

use super::*;

// ---------------------------------------------------------------------------
// Equalizer
// ---------------------------------------------------------------------------

/// The number of graphic-EQ bands, 31 Hz to 16 kHz.
pub const EQ_BAND_COUNT: usize = 10;

/// The centre frequency label of each band, low → high.
pub const EQ_BAND_LABELS: [&str; EQ_BAND_COUNT] = [
    "31", "62", "125", "250", "500", "1k", "2k", "4k", "8k", "16k",
];

/// Per-band gain is clamped to ±12 dB, the conventional graphic-EQ range.
///
/// Decibel bounds, not pixel dimensions, so no `SizingTokens` value applies.
pub const EQ_GAIN_MIN_DB: f32 = -12.0;
pub const EQ_GAIN_MAX_DB: f32 = 12.0;

/// A named equalizer preset. `Custom` has no curve: it is the state the bands
/// fall into when they match no preset.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EqPreset {
    Flat,
    BassBoost,
    /// The mirror of [`Self::BassBoost`]: lifts the top of the band range.
    TrebleBoost,
    Classical,
    /// Not a preset with a curve: the bands do not match any named preset.
    Custom,
}

impl EqPreset {
    /// The presets the UI offers, in order, excluding `Custom`.
    /// `enz_radio_group` selects by index, so a test pins this order.
    pub const SELECTABLE: &'static [EqPreset] = &[
        EqPreset::Flat,
        EqPreset::BassBoost,
        EqPreset::TrebleBoost,
        EqPreset::Classical,
    ];

    pub fn label(self) -> &'static str {
        match self {
            EqPreset::Flat => "Flat",
            EqPreset::BassBoost => "Bass Boost",
            EqPreset::TrebleBoost => "Treble Boost",
            EqPreset::Classical => "Classical",
            EqPreset::Custom => "Custom",
        }
    }

    /// The band curve this preset applies, or `None` for `Custom`.
    pub fn curve(self) -> Option<[f32; EQ_BAND_COUNT]> {
        match self {
            // Zero gain is a literal curve, not "unset".
            EqPreset::Flat => Some([0.0; EQ_BAND_COUNT]),
            EqPreset::BassBoost => Some([8.0, 6.0, 4.0, 2.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0]),
            // Bass Boost mirrored onto the top four bands. Curves must be
            // unique: `classify` matches exactly and resolves to the first hit.
            EqPreset::TrebleBoost => Some([0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 2.0, 4.0, 6.0, 8.0]),
            EqPreset::Classical => Some([0.0, 0.0, 0.0, 0.0, 0.0, 0.0, -4.0, -4.0, -4.0, -6.0]),
            EqPreset::Custom => None,
        }
    }

    /// Resolve `bands` to the named preset whose curve matches exactly, or
    /// `Custom` when none does. A literal all-zero curve is `Flat`.
    pub fn classify(bands: &[f32; EQ_BAND_COUNT]) -> EqPreset {
        for p in Self::SELECTABLE {
            if let Some(curve) = p.curve()
                && curve == *bands
            {
                return *p;
            }
        }
        EqPreset::Custom
    }
}

/// Graphic-equalizer policy, saved with the playback settings.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EqState {
    pub enabled: bool,
    /// Pre-amplifier gain applied before the bands, in dB.
    pub preamp_db: f32,
    /// Per-band gains in dB, low → high.
    pub bands: [f32; EQ_BAND_COUNT],
    /// The named preset the bands currently equal, or `Custom`. Derived by
    /// [`Self::set_band`], never set independently.
    pub preset: EqPreset,
    /// Attenuate output to leave headroom for positive band and preamp gains.
    pub clip_protection: bool,
}

impl Default for EqState {
    fn default() -> Self {
        Self {
            enabled: false,
            preamp_db: 0.0,
            bands: [0.0; EQ_BAND_COUNT],
            preset: EqPreset::Flat,
            clip_protection: true,
        }
    }
}

impl EqState {
    /// Apply a named preset's curve to the bands and record the selection.
    /// `Custom` is a no-op.
    pub fn apply_preset(&mut self, preset: EqPreset) {
        if let Some(curve) = preset.curve() {
            self.bands = curve;
            self.preset = preset;
        }
    }

    /// Set one band's gain, clamped to ±12 dB, and re-resolve the preset:
    /// leaving a preset's curve selects `Custom`, and returning to one
    /// re-selects it.
    pub fn set_band(&mut self, index: usize, gain_db: f32) {
        if index >= EQ_BAND_COUNT {
            return;
        }
        // Non-finite input keeps the current value rather than poisoning the
        // array; 0 is literal.
        let g = if gain_db.is_finite() {
            gain_db.clamp(EQ_GAIN_MIN_DB, EQ_GAIN_MAX_DB)
        } else {
            self.bands[index]
        };
        self.bands[index] = g;
        self.preset = EqPreset::classify(&self.bands);
    }

    /// Attenuation for the strongest positive band and preamp gains.
    /// Returns `1.0` when disabled or when nothing is boosted.
    pub fn clip_scale(&self) -> f32 {
        if !self.enabled || !self.clip_protection {
            return 1.0;
        }
        let max_boost_db = self.preamp_db.max(0.0)
            + self
                .bands
                .iter()
                .copied()
                .fold(0.0_f32, |acc, b| acc.max(b));
        if max_boost_db <= 0.0 {
            return 1.0;
        }
        let linear = 10.0_f32.powf(max_boost_db / 20.0);
        1.0 / linear
    }
}

/// The linear sample multiplier for a track, folding volume, mute and the
/// equalizer (preamp and clip protection) into one number.
///
/// Equivalent to [`effective_sample_scale_with_rg`] with ReplayGain `Off` and
/// an untagged track.
#[cfg(test)]
pub fn effective_sample_scale(playback: &PlaybackState, eq: &EqState) -> f32 {
    effective_sample_scale_with_rg(
        playback,
        eq,
        &ReplayGainState {
            mode: ReplayGainMode::Off,
            preamp_db: 0.0,
            prevent_clip: true,
        },
        &ReplayGain::default(),
    )
}

/// The full sample multiplier: volume × mute × equalizer × ReplayGain.
///
/// Combines user volume, EQ preamp and attenuation, and the independently
/// peak-limited ReplayGain scalar; mute short-circuits the result.
///
/// Used by reducer tests to compare the combined policy with user volume.
#[cfg(test)]
pub fn effective_sample_scale_with_rg(
    playback: &PlaybackState,
    eq: &EqState,
    rg_settings: &ReplayGainState,
    rg: &ReplayGain,
) -> f32 {
    if playback.muted {
        return 0.0;
    }
    let vol = playback.volume.clamp(0.0, 1.0);
    let eq_scale = if eq.enabled {
        // Preamp is a linear gain stage; clip protection scales back down.
        10.0_f32.powf(eq.preamp_db / 20.0) * eq.clip_scale()
    } else {
        1.0
    };
    vol * eq_scale * rg_settings.scalar_for(rg)
}

// ---------------------------------------------------------------------------
// ReplayGain loudness normalisation (Settings › Playback)
// ---------------------------------------------------------------------------

/// The listener's ReplayGain preference.
///
/// `Track` normalises each song independently (suited to shuffle); `Album`
/// applies one whole-album gain, preserving loud/quiet contrast within an
/// album.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ReplayGainMode {
    /// Skip normalisation entirely; the volume slider rules alone.
    Off,
    /// Per-track gain. The default, because most listening is shuffled.
    #[default]
    Track,
    /// Whole-album gain.
    Album,
}

impl ReplayGainMode {
    /// The modes the UI offers, in order. `enz_radio_group` selects by index,
    /// so a test pins this order.
    pub const SELECTABLE: &'static [ReplayGainMode] = &[
        ReplayGainMode::Off,
        ReplayGainMode::Track,
        ReplayGainMode::Album,
    ];

    pub fn label(self) -> &'static str {
        match self {
            ReplayGainMode::Off => "Off",
            ReplayGainMode::Track => "Track",
            ReplayGainMode::Album => "Album",
        }
    }

    /// This mode's index within [`Self::SELECTABLE`].
    pub fn selectable_index(self) -> Option<usize> {
        Self::SELECTABLE.iter().position(|m| *m == self)
    }
}

/// The ReplayGain preamp range in dB, as the settings copy states it:
/// −15 to +15 dB.
///
/// Decibels, not pixels, so no `SizingTokens` value applies.
/// `the_preamp_description_quotes_the_real_range` pins the copy to these values.
pub const RG_PREAMP_MIN_DB: f32 = -15.0;
pub const RG_PREAMP_MAX_DB: f32 = 15.0;

/// The hard ceiling on the linear scalar ReplayGain may produce (~+12 dB).
///
/// A backstop against malformed tags, applied after clip protection.
pub const RG_MAX_SCALAR: f32 = 4.0;

/// The ReplayGain values a track carries in its tags.
///
/// All four are independently optional: older files often carry only track
/// values, and classical rips often only album values.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ReplayGain {
    pub track_gain_db: Option<f32>,
    pub track_peak: Option<f32>,
    pub album_gain_db: Option<f32>,
    pub album_peak: Option<f32>,
}

impl ReplayGain {
    /// True when at least one gain is present; a peak alone cannot normalise.
    pub fn has_any(&self) -> bool {
        self.track_gain_db.is_some() || self.album_gain_db.is_some()
    }
}

/// The listener's ReplayGain settings, as edited in Settings › Playback.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ReplayGainState {
    pub mode: ReplayGainMode,
    /// A listener-wide offset added to whichever gain the mode selects.
    pub preamp_db: f32,
    /// Prevent clipping: caps the scalar so `scalar × peak <= 1.0`; see
    /// [`Self::scalar_for`].
    pub prevent_clip: bool,
}

impl Default for ReplayGainState {
    fn default() -> Self {
        Self {
            mode: ReplayGainMode::default(),
            preamp_db: 0.0,
            prevent_clip: true,
        }
    }
}

impl ReplayGainState {
    /// Set the preamp, clamped to the stated −15 to +15 dB range.
    ///
    /// A non-finite value keeps the current setting, since `NaN` would pass
    /// through `clamp` into the sample multiplier.
    pub fn set_preamp(&mut self, db: f32) {
        if db.is_finite() {
            self.preamp_db = db.clamp(RG_PREAMP_MIN_DB, RG_PREAMP_MAX_DB);
        }
    }

    /// The linear multiplier ReplayGain contributes for a track carrying `rg`.
    ///
    /// `1.0` whenever the mode is `Off` or the track carries no gain.
    /// Otherwise:
    /// 1. Pick the mode's gain, falling back to the other when it is absent.
    /// 2. Add [`Self::preamp_db`] and convert to linear.
    /// 3. If [`Self::prevent_clip`] is set and a matching peak is known, cap
    ///    the scalar at `1.0 / peak`.
    /// 4. Clamp to `[0, RG_MAX_SCALAR]`.
    ///
    /// A non-positive or non-finite peak is ignored.
    pub fn scalar_for(&self, rg: &ReplayGain) -> f32 {
        if self.mode == ReplayGainMode::Off {
            return 1.0;
        }
        let (gain_db, peak) = match self.mode {
            ReplayGainMode::Track => (
                rg.track_gain_db.or(rg.album_gain_db),
                rg.track_peak.or(rg.album_peak),
            ),
            ReplayGainMode::Album => (
                rg.album_gain_db.or(rg.track_gain_db),
                rg.album_peak.or(rg.track_peak),
            ),
            ReplayGainMode::Off => return 1.0,
        };
        let Some(gain_db) = gain_db else {
            return 1.0;
        };
        let effective_db = gain_db + self.preamp_db;
        if !effective_db.is_finite() {
            return 1.0;
        }
        let mut scalar = 10.0_f32.powf(effective_db / 20.0);
        if self.prevent_clip
            && let Some(peak) = peak
            && peak > 0.0
            && peak.is_finite()
        {
            let max_scalar = 1.0 / peak;
            if scalar > max_scalar {
                scalar = max_scalar;
            }
        }
        scalar.clamp(0.0, RG_MAX_SCALAR)
    }
}

// ---------------------------------------------------------------------------
// ReplayGain (Settings › Playback)
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Sleep timer
// ---------------------------------------------------------------------------

/// A selectable sleep-timer duration, or "end of the current track", which
/// has no minutes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SleepPreset {
    Minutes(u32),
    EndOfTrack,
}

impl SleepPreset {
    /// The presets the UI offers, in order.
    pub const SELECTABLE: &'static [SleepPreset] = &[
        SleepPreset::Minutes(5),
        SleepPreset::Minutes(10),
        SleepPreset::Minutes(15),
        SleepPreset::Minutes(30),
        SleepPreset::Minutes(45),
        SleepPreset::Minutes(60),
        SleepPreset::EndOfTrack,
    ];

    /// This preset's position within [`Self::SELECTABLE`], or `None` if absent.
    pub fn index(self) -> Option<usize> {
        Self::SELECTABLE.iter().position(|p| *p == self)
    }
}

/// The armed/off state of the sleep timer.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum SleepTimerState {
    #[default]
    Off,
    /// A running countdown; `fade` fades out over the final
    /// [`SLEEP_FADE_SECS`] seconds.
    Armed { remaining_secs: f64, fade: bool },
    /// Stop when the currently-playing track ends. No countdown.
    UntilTrackEnd { fade: bool },
}

/// The length of the fade-out at the end of a sleep timer, in seconds.
pub const SLEEP_FADE_SECS: f64 = 20.0;

impl SleepTimerState {
    pub fn is_armed(self) -> bool {
        !matches!(self, SleepTimerState::Off)
    }

    /// Arm the timer from a preset, replacing any existing timer.
    pub fn arm(&mut self, preset: SleepPreset, fade: bool) {
        *self = match preset {
            SleepPreset::Minutes(m) => SleepTimerState::Armed {
                remaining_secs: (m as f64) * 60.0,
                fade,
            },
            SleepPreset::EndOfTrack => SleepTimerState::UntilTrackEnd { fade },
        };
    }

    pub fn cancel(&mut self) {
        *self = SleepTimerState::Off;
    }

    /// Advance a running countdown by `dt` seconds, returning `true` once, on
    /// the tick that expires it. `UntilTrackEnd` fires from the track-end
    /// event instead, and `Off` never fires.
    pub fn tick(&mut self, dt: f64) -> bool {
        if let SleepTimerState::Armed { remaining_secs, .. } = self {
            if *remaining_secs <= 0.0 {
                return false;
            }
            *remaining_secs -= dt.max(0.0);
            if *remaining_secs <= 0.0 {
                *remaining_secs = 0.0;
                *self = SleepTimerState::Off;
                return true;
            }
        }
        false
    }
}

/// Compose the "Fading out at the end of `<label>`" status line for an armed
/// fading timer, or `None` when the timer is off or not fading.
pub fn sleep_fade_status(state: SleepTimerState) -> Option<String> {
    const PREFIX: &str = "Fading out at the end of ";
    match state {
        SleepTimerState::Armed {
            remaining_secs,
            fade: true,
        } => Some(format!("{PREFIX}{}", format_duration(remaining_secs))),
        SleepTimerState::UntilTrackEnd { fade: true } => Some(format!("{PREFIX}the current track")),
        _ => None,
    }
}
