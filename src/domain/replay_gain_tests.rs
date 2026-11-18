use super::*;

fn playing() -> PlaybackState {
    PlaybackState {
        volume: 1.0,
        muted: false,
        ..PlaybackState::default()
    }
}

fn tagged() -> ReplayGain {
    ReplayGain {
        track_gain_db: Some(-6.0),
        track_peak: Some(0.98),
        album_gain_db: Some(-3.0),
        album_peak: Some(0.99),
    }
}

#[test]
fn prevent_clipping_actually_prevents_clipping() {
    let pb = playing();
    let eq = EqState::default(); // EQ off: isolate the RG stage
    // +6 dB on a 0.98 peak is 0.98 * 1.995 = 1.955, which clips.
    let rg = ReplayGain {
        track_gain_db: Some(6.0),
        track_peak: Some(0.98),
        ..ReplayGain::default()
    };
    let peak = 0.98_f32;

    let on = ReplayGainState {
        mode: ReplayGainMode::Track,
        prevent_clip: true,
        ..ReplayGainState::default()
    };
    let scale_on = effective_sample_scale_with_rg(&pb, &eq, &on, &rg);
    assert!(
        peak * scale_on <= 1.0 + 1e-6,
        "with `Prevent clipping` ON the loudest sample must land at or \
         below full scale; got peak {peak} * scale {scale_on} = {}",
        peak * scale_on
    );

    // Negative control: off, the same track clips.
    let mut off = on;
    off.prevent_clip = false;
    let scale_off = effective_sample_scale_with_rg(&pb, &eq, &off, &rg);
    assert!(
        peak * scale_off > 1.0,
        "with `Prevent clipping` OFF this +6 dB track must exceed full \
         scale, or the toggle is not gating anything: peak {peak} * scale \
         {scale_off} = {}",
        peak * scale_off
    );
    assert!(
        scale_on < scale_off,
        "the protected scale must be strictly smaller ({scale_on} < {scale_off})"
    );
}

#[test]
fn every_replay_gain_mode_reaches_the_sink_with_a_different_scale() {
    let pb = playing();
    let eq = EqState::default();
    let rg = tagged();

    let scale = |mode| {
        let s = ReplayGainState {
            mode,
            prevent_clip: false,
            ..ReplayGainState::default()
        };
        effective_sample_scale_with_rg(&pb, &eq, &s, &rg)
    };

    let off = scale(ReplayGainMode::Off);
    let track = scale(ReplayGainMode::Track);
    let album = scale(ReplayGainMode::Album);

    assert_eq!(
        off, 1.0,
        "`Off` must be an exact no-op, not approximately 1.0"
    );
    assert!(track < off, "a -6 dB track gain must cut ({track} < {off})");
    assert!(album < off, "a -3 dB album gain must cut ({album} < {off})");
    assert!(
        track < album,
        "-6 dB (track) must cut MORE than -3 dB (album); if these were \
         equal the mode selection would not be reaching the gain choice \
         ({track} vs {album})"
    );
}

#[test]
fn an_untagged_track_is_never_rescaled() {
    let pb = playing();
    let eq = EqState::default();
    let bare = ReplayGain::default();
    assert!(!bare.has_any());
    for mode in ReplayGainMode::SELECTABLE {
        let s = ReplayGainState {
            mode: *mode,
            preamp_db: 9.0,
            ..ReplayGainState::default()
        };
        assert_eq!(
            effective_sample_scale_with_rg(&pb, &eq, &s, &bare),
            1.0,
            "mode {mode:?}: an untagged track must play exactly as before"
        );
    }
}

#[test]
fn a_mode_falls_back_to_the_other_gain_when_its_own_is_missing() {
    let pb = playing();
    let eq = EqState::default();

    let album_only = ReplayGain {
        album_gain_db: Some(-6.0),
        ..ReplayGain::default()
    };
    let track_mode = ReplayGainState {
        mode: ReplayGainMode::Track,
        prevent_clip: false,
        ..ReplayGainState::default()
    };
    assert!(
        effective_sample_scale_with_rg(&pb, &eq, &track_mode, &album_only) < 1.0,
        "Track mode must fall back to the album gain rather than no-op"
    );

    let track_only = ReplayGain {
        track_gain_db: Some(-6.0),
        ..ReplayGain::default()
    };
    let album_mode = ReplayGainState {
        mode: ReplayGainMode::Album,
        prevent_clip: false,
        ..ReplayGainState::default()
    };
    assert!(
        effective_sample_scale_with_rg(&pb, &eq, &album_mode, &track_only) < 1.0,
        "Album mode must fall back to the track gain"
    );
}

#[test]
fn the_preamp_clamps_to_its_stated_range_and_refuses_poison() {
    let mut s = ReplayGainState::default();

    s.set_preamp(3.5);
    assert_eq!(s.preamp_db, 3.5);

    s.set_preamp(RG_PREAMP_MIN_DB);
    assert_eq!(s.preamp_db, RG_PREAMP_MIN_DB);
    s.set_preamp(RG_PREAMP_MAX_DB);
    assert_eq!(s.preamp_db, RG_PREAMP_MAX_DB);

    s.set_preamp(40.0);
    assert_eq!(s.preamp_db, RG_PREAMP_MAX_DB, "+40 dB must clamp to +15");
    s.set_preamp(-40.0);
    assert_eq!(s.preamp_db, RG_PREAMP_MIN_DB, "-40 dB must clamp to -15");

    for poison in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
        let before = s.preamp_db;
        s.set_preamp(poison);
        assert_eq!(
            s.preamp_db, before,
            "{poison} must not reach the gain stage"
        );
    }

    let pb = playing();
    let eq = EqState::default();
    let rg = ReplayGain {
        track_gain_db: Some(-6.0),
        ..ReplayGain::default()
    };
    let lo = ReplayGainState {
        mode: ReplayGainMode::Track,
        prevent_clip: false,
        ..ReplayGainState::default()
    };
    let mut hi = lo;
    hi.set_preamp(6.0);
    assert!(
        effective_sample_scale_with_rg(&pb, &eq, &hi, &rg)
            > effective_sample_scale_with_rg(&pb, &eq, &lo, &rg),
        "a +6 dB preamp must raise the sink's scale"
    );
}

#[test]
fn a_corrupt_tag_cannot_produce_a_destructive_or_infinite_scale() {
    let pb = playing();
    let eq = EqState::default();
    let s = ReplayGainState {
        mode: ReplayGainMode::Track,
        ..ReplayGainState::default()
    };

    let loud = ReplayGain {
        track_gain_db: Some(60.0),
        ..ReplayGain::default()
    };
    let scale = effective_sample_scale_with_rg(&pb, &eq, &s, &loud);
    assert!(scale.is_finite());
    assert!(
        scale <= RG_MAX_SCALAR,
        "a +60 dB tag must be capped at {RG_MAX_SCALAR}, got {scale}"
    );

    for peak in [0.0_f32, -0.5, f32::NAN, f32::INFINITY] {
        let odd = ReplayGain {
            track_gain_db: Some(6.0),
            track_peak: Some(peak),
            ..ReplayGain::default()
        };
        let sc = effective_sample_scale_with_rg(&pb, &eq, &s, &odd);
        assert!(
            sc.is_finite(),
            "peak {peak} produced a non-finite scale {sc}"
        );
        assert!(sc <= RG_MAX_SCALAR);
    }
}

#[test]
fn mute_silences_regardless_of_replay_gain() {
    let mut pb = playing();
    pb.muted = true;
    let mut s = ReplayGainState {
        mode: ReplayGainMode::Track,
        ..ReplayGainState::default()
    };
    s.set_preamp(15.0);
    assert_eq!(
        effective_sample_scale_with_rg(&pb, &EqState::default(), &s, &tagged()),
        0.0
    );
}

#[test]
fn the_equalizer_and_replay_gain_stages_compose() {
    let pb = playing();
    let rg = ReplayGain {
        track_gain_db: Some(-6.0),
        ..ReplayGain::default()
    };
    let s = ReplayGainState {
        mode: ReplayGainMode::Track,
        prevent_clip: false,
        ..ReplayGainState::default()
    };

    let mut eq_on = EqState {
        enabled: true,
        clip_protection: true,
        ..EqState::default()
    };
    eq_on.set_band(0, 12.0); // scales DOWN under clip protection

    let eq_off = EqState::default();

    let rg_only = effective_sample_scale_with_rg(&pb, &eq_off, &s, &rg);
    let both = effective_sample_scale_with_rg(&pb, &eq_on, &s, &rg);
    assert!(
        both < rg_only,
        "the EQ stage must still apply on top of ReplayGain ({both} < {rg_only})"
    );

    let mut off = s;
    off.mode = ReplayGainMode::Off;
    assert_eq!(
        effective_sample_scale(&pb, &eq_on),
        effective_sample_scale_with_rg(&pb, &eq_on, &off, &ReplayGain::default()),
        "the legacy two-arg scale must equal the full one with RG off"
    );
}
