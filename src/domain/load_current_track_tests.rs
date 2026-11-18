use super::*;
use std::path::PathBuf;

pub(super) fn track_with_duration(secs: f64) -> Track {
    Track {
        id: 0,
        path: PathBuf::from("/fixture/song.mp3"),
        title: "Fixture".into(),
        artist: "Fixture Artist".into(),
        album: "Fixture Album".into(),
        album_artist: "Fixture Artist".into(),
        track_no: 1,
        disc_no: 1,
        year: 2024,
        genre: "Test".into(),
        format: "MP3".into(),
        duration_secs: secs,
        bitrate_kbps: 320,
        sample_rate: 44100,
        channels: 2,
        bit_depth: 16,
        cover_path: None,
        cover: None,
        added_at: 0,
        play_count: 0,
        last_played_seq: 0,
        last_played_unix: 0,
        replay_gain: ReplayGain::default(),
    }
}

#[test]
fn loading_preserves_play_intent_and_replaces_stale_timing() {
    let mut library = Library::new();
    let id = library.add_track(track_with_duration(214.0));
    library.reindex();
    for playing in [false, true] {
        let mut playback = PlaybackState {
            playing,
            ..Default::default()
        };
        playback.load_current_track(id, &library);
        assert_eq!(playback.current_track_id, Some(id));
        assert_eq!(playback.duration_secs, 214.0);
        assert_eq!(playback.position_secs, 0.0);
        assert_eq!(playback.playing, playing);
        playback.load_current_track(9999, &library);
        assert_eq!(playback.duration_secs, 0.0);
    }
}
