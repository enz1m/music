//! Test-only demo-library fixtures.

use super::*;

impl MusicApp {
    /// Seed a synthetic demo library and return its playlist id.
    ///
    /// Six albums, `Track {n}` titles, a `Favourites` playlist, and the first
    /// track loaded as now playing.
    pub(crate) fn seed_demo_library(&mut self) -> u64 {
        let demo_albums = [
            ("Morning Light", "Horizon Collective", 2024, "Pop"),
            ("Deep Currents", "Ocean Waves", 2023, "Electronic"),
            ("Rust & Steel", "Iron Orchestra", 2024, "Rock"),
            ("Whispers", "Midnight Echo", 2022, "Jazz"),
            ("Solar Flare", "Cosmic Drift", 2023, "Ambient"),
            ("City Pulse", "Metro Beat", 2024, "Hip-Hop"),
        ];

        for (album_idx, (album, artist, year, genre)) in demo_albums.iter().enumerate() {
            let track_count = 8 + (album_idx % 5);
            for track_no in 1..=track_count {
                let track = domain::Track {
                    id: 0,
                    path: std::path::PathBuf::from(format!(
                        "{DEMO_LIBRARY_ROOT}/{artist}/{album}/{track_no:02}.mp3"
                    )),
                    title: format!("Track {track_no}"),
                    artist: artist.to_string(),
                    album: album.to_string(),
                    album_artist: artist.to_string(),
                    track_no: track_no as u32,
                    disc_no: 1,
                    year: *year,
                    genre: genre.to_string(),
                    format: "MP3".into(),
                    duration_secs: 180.0 + (track_no as f64 * 15.0),
                    bitrate_kbps: 320,
                    sample_rate: 44100,
                    channels: 2,
                    bit_depth: 16,
                    cover_path: None,
                    cover: None,
                    added_at: 1700000000 + (album_idx as u64 * 86400),
                    play_count: (10u32).saturating_sub((album_idx * 5 + track_no) as u32),
                    last_played_seq: 0,
                    last_played_unix: 0,
                    replay_gain: if album_idx % 2 == 0 {
                        domain::ReplayGain {
                            track_gain_db: Some(-6.0 - (track_no as f32) * 0.25),
                            track_peak: Some(0.98),
                            album_gain_db: Some(-5.5),
                            album_peak: Some(0.99),
                        }
                    } else {
                        domain::ReplayGain::default()
                    },
                };
                self.library.add_track(track);
            }
        }
        // Present at the lowest generation, so the fixture is playable and a
        // real scan of `DEMO_LIBRARY_ROOT` still supersedes it.
        let present_paths: Vec<std::path::PathBuf> = self
            .library
            .tracks
            .values()
            .map(|t| t.path.clone())
            .collect();
        for path in present_paths {
            self.library
                .decide_availability(path, domain::TrackAvailability::Present, 0);
        }
        self.library.reindex();

        if let Some(&first_id) = self.library.track_order.first() {
            self.playback.current_track_id = Some(first_id);
            if let Some(t) = self.library.tracks.get(&first_id) {
                self.playback.duration_secs = t.duration_secs;
            }
            self.playback.position_secs = 42.0;
            // Mint occurrence ids for the seeded queue in lockstep.
            self.playback
                .reset_queue_from_ids(&self.library.track_order);
        }

        let demo_pid = self
            .library
            .create_playlist("Favourites".into())
            .expect("fresh demo library has an unexhausted playlist id space");
        let seed_tids: Vec<u64> = self.library.track_order.iter().take(6).copied().collect();
        for tid in seed_tids {
            self.library.add_track_to_playlist(demo_pid, tid);
        }
        demo_pid
    }

    #[cfg(test)]
    pub(crate) fn boot_with_demo() -> (Self, Task<Message>) {
        // A fixed folder rather than the environment-resolved one, so the
        // render tests do not depend on the host's music folders.
        let (mut app, task) = Self::boot_with(
            false,
            false,
            vec![std::path::PathBuf::from(DEMO_LIBRARY_ROOT)],
        );
        let demo_pid = app.seed_demo_library();
        // A fixed catalog so render tests have rows without the network;
        // `Loaded { at_unix: 0 }` matches a successful boot fetch.
        app.radio
            .lists
            .insert(("test".into(), None), radio::seed_test_catalog());
        app.radio.catalog_status = radio::RadioCatalogState::Loaded { at_unix: 0 };
        if let Ok(v) = std::env::var("MUSIC_APP_START_VIEW")
            && v.to_lowercase() == "playlists"
        {
            app.filter = LibraryFilter::Playlist(demo_pid);
        }
        (app, task)
    }
}
