use super::*;

fn t(path: &str, title: &str, artist: &str, album: &str, added: u64) -> Track {
    Track {
        id: 0,
        path: PathBuf::from(path),
        title: title.into(),
        artist: artist.into(),
        album: album.into(),
        album_artist: artist.into(),
        track_no: 1,
        disc_no: 1,
        year: 2024,
        genre: "Pop".into(),
        format: "MP3".into(),
        duration_secs: 200.0,
        bitrate_kbps: 320,
        sample_rate: 44100,
        channels: 2,
        bit_depth: 16,
        cover_path: None,
        cover: None,
        added_at: added,
        play_count: 0,
        last_played_seq: 0,
        last_played_unix: 0,
        replay_gain: ReplayGain::default(),
    }
}

include!("playback_queue_tests.rs");
