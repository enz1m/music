//! Model-level controls for importing a settled parser report through
//! [`crate::domain::Library::import_paths_as_playlist`], without a portal or worker.

use std::path::PathBuf;

use crate::domain::{Library, Playlist, ReplayGain, Track};
use crate::playlist_io::{ParseReport, PlaylistFormat};

fn track(path: &str) -> Track {
    Track {
        id: 0,
        path: PathBuf::from(path),
        title: "T".into(),
        artist: "A".into(),
        album: "AL".into(),
        album_artist: "A".into(),
        track_no: 1,
        disc_no: 1,
        year: 2024,
        genre: "Pop".into(),
        format: "MP3".into(),
        duration_secs: 100.0,
        bitrate_kbps: 320,
        sample_rate: 44_100,
        channels: 2,
        bit_depth: 16,
        cover_path: None,
        cover: None,
        added_at: 1,
        play_count: 0,
        last_played_seq: 0,
        last_played_unix: 0,
        replay_gain: ReplayGain::default(),
    }
}

fn report(paths: &[&str], duplicate: usize, refused: usize) -> ParseReport {
    ParseReport {
        format: PlaylistFormat::M3u8,
        paths: paths.iter().map(PathBuf::from).collect(),
        refused,
        duplicate,
    }
}

#[test]
fn import_splits_imported_and_missing_with_counts() {
    let mut lib = Library::new();
    let present = lib.add_track(track("/music/a.mp3"));

    let (id, counts) = lib
        .import_paths_as_playlist(
            "Imported".into(),
            &report(&["/music/a.mp3", "/music/missing.mp3"], 1, 2),
        )
        .expect("id space is not exhausted");

    assert_eq!(counts.imported, 1, "one row resolved to a library track");
    assert_eq!(
        counts.missing, 1,
        "one row is retained but not in the library"
    );
    assert_eq!(counts.duplicate, 1, "parser duplicate count passes through");
    assert_eq!(counts.refused, 2, "parser refused count passes through");

    let export = lib
        .playlist_export_paths(id)
        .expect("the created playlist exists");
    assert_eq!(
        export,
        vec![
            PathBuf::from("/music/a.mp3"),
            PathBuf::from("/music/missing.mp3")
        ],
        "both valid rows are retained durably, in order"
    );
    let slot_ids: Vec<u64> = lib
        .playlists
        .iter()
        .find(|p| p.id == id)
        .map(|p| p.track_ids.clone())
        .unwrap_or_default();
    assert_eq!(slot_ids, vec![present], "only the in-library row has an id");
}

#[test]
fn import_retains_missing_for_later_reconciliation() {
    let mut lib = Library::new();
    let (id, counts) = lib
        .import_paths_as_playlist("Later".into(), &report(&["/music/later.mp3"], 0, 0))
        .expect("id space is not exhausted");
    assert_eq!(counts.missing, 1);
    assert_eq!(counts.imported, 0);

    let new_id = lib.add_track(track("/music/later.mp3"));

    let ids: Vec<u64> = lib
        .playlists
        .iter()
        .find(|p| p.id == id)
        .map(|p| p.track_ids.clone())
        .unwrap_or_default();
    assert_eq!(
        ids,
        vec![new_id],
        "the retained path reconciles into the playlist on the later add"
    );
}

#[test]
fn import_creates_nothing_when_id_space_exhausted() {
    let mut lib = Library::new();
    lib.restore_playlists(vec![Playlist {
        id: u64::MAX,
        name: "Only".into(),
        track_paths: Vec::new(),
        track_ids: Vec::new(),
    }]);
    let before = lib.playlists.len();

    let outcome = lib.import_paths_as_playlist("Nope".into(), &report(&["/music/a.mp3"], 0, 0));

    assert!(
        outcome.is_none(),
        "an exhausted id space refuses the import"
    );
    assert_eq!(
        lib.playlists.len(),
        before,
        "the refused import added no playlist"
    );
}
