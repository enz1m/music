//! Bounds: cardinality, retention, ceilings, and refusal symmetry.

use super::*;

#[test]
fn an_oversize_state_file_is_refused_before_allocation_and_the_boundary_is_exact() {
    use std::os::unix::fs::PermissionsExt;
    let dir = TempDir::new("oversize");
    let path = dir.state_file();
    std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");

    // Owner-only mode isolates the size gate from the permission gate.
    let f = std::fs::File::create(&path).expect("create");
    f.set_len(MAX_STATE_FILE_BYTES + 1).expect("grow sparse");
    drop(f);
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).expect("chmod 0600");
    assert!(
        matches!(load_from(&path), Err(LoadError::TooLarge(_))),
        "one byte over the ceiling is refused before allocation"
    );
    let (_loaded, persist, origin) = open_at(&path);
    assert!(!persist.is_enabled(), "an oversize file disables saving");
    assert!(matches!(
        origin,
        Origin::RefusedReadOnly(RefusalKind::TooLarge)
    ));

    // Exactly at the ceiling reaches the reader and fails as JSON, so the boundary
    // is not off by one.
    let f = std::fs::File::create(&path).expect("recreate");
    f.set_len(MAX_STATE_FILE_BYTES)
        .expect("size to the ceiling");
    drop(f);
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).expect("chmod 0600");
    assert!(
        matches!(load_from(&path), Err(LoadError::Corrupt(_))),
        "exactly the ceiling is read, then refused as non-JSON, not as TooLarge"
    );
}

#[test]
fn each_persisted_collection_family_refuses_over_its_cardinality() {
    use crate::domain::{
        MAX_FEED_URL_BYTES, MAX_LIBRARY_FOLDERS, MAX_PERSISTED_PATH_BYTES, MAX_PERSISTED_TRACKS,
        MAX_QUEUE_PATHS, MAX_RADIO_FAVORITES, MAX_SUBSCRIPTIONS,
    };
    let dir = TempDir::new("families");
    let path = dir.state_file();
    std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");

    let arr = |entry: &str, n: usize| -> String {
        let mut s = String::from("[");
        for i in 0..n {
            if i > 0 {
                s.push(',');
            }
            s.push_str(entry);
        }
        s.push(']');
        s
    };
    let over_long_path = format!("\"/{}\"", "a".repeat(MAX_PERSISTED_PATH_BYTES));
    let over_long_url = "u".repeat(MAX_FEED_URL_BYTES + 1);

    let cases: Vec<(String, &str)> = vec![
        (
            format!(
                "{{\"version\":1,\"folders\":{}}}",
                arr("\"/f\"", MAX_LIBRARY_FOLDERS + 1)
            ),
            "folders cardinality",
        ),
        (
            format!("{{\"version\":1,\"folders\":[{over_long_path}]}}"),
            "folder path length",
        ),
        (
            format!(
                "{{\"version\":1,\"subscriptions\":{}}}",
                arr("{\"feed_url\":\"u\"}", MAX_SUBSCRIPTIONS + 1)
            ),
            "subscriptions cardinality",
        ),
        (
            format!("{{\"version\":1,\"subscriptions\":[{{\"feed_url\":\"{over_long_url}\"}}]}}"),
            "feed url length",
        ),
        (
            format!(
                "{{\"version\":1,\"radio_favorites_uuids\":{}}}",
                arr("\"u\"", MAX_RADIO_FAVORITES + 1)
            ),
            "radio uuid cardinality",
        ),
        (
            format!(
                "{{\"version\":1,\"track_stats\":{}}}",
                arr("{\"path\":\"/t\"}", MAX_PERSISTED_TRACKS + 1)
            ),
            "play-stats cardinality",
        ),
        (
            format!(
                "{{\"version\":1,\"track_added_at\":{}}}",
                arr("{\"path\":\"/t\"}", MAX_PERSISTED_TRACKS + 1)
            ),
            "first-seen cardinality",
        ),
        (
            format!(
                "{{\"version\":1,\"playback\":{{\"queue_paths\":{}}}}}",
                arr("\"/q\"", MAX_QUEUE_PATHS + 1)
            ),
            "queue cardinality",
        ),
    ];

    for (json, label) in &cases {
        seed_state(&path, json.as_bytes());
        let before = file_snapshot(&path);
        assert!(
            matches!(load_from(&path), Err(LoadError::OverLimit(_))),
            "{label} is refused over the limit"
        );
        let (loaded, persist, origin) = open_at(&path);
        assert!(loaded.is_none(), "{label}: not adopted");
        assert!(!persist.is_enabled(), "{label}: saving disabled");
        assert!(
            matches!(origin, Origin::RefusedReadOnly(RefusalKind::OverLimit)),
            "{label}: read-only over-limit origin"
        );
        assert_eq!(file_snapshot(&path), before, "{label}: byte-identical");
    }

    // Positive control: exactly the folder cardinality limit loads.
    std::fs::write(
        &path,
        format!(
            "{{\"version\":1,\"folders\":{}}}",
            arr("\"/f\"", MAX_LIBRARY_FOLDERS)
        )
        .as_bytes(),
    )
    .expect("write at-limit");
    assert_eq!(
        load_from(&path)
            .expect("exactly the folder limit loads")
            .folders
            .len(),
        MAX_LIBRARY_FOLDERS
    );
}

#[test]
fn a_replaced_or_vanished_state_path_is_refused_after_the_read() {
    let dir = TempDir::new("reverify");
    let path = dir.state_file();
    std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    std::fs::write(&path, b"{\"version\":1}").expect("seed");
    let admitted = std::fs::symlink_metadata(&path).expect("stat admitted");

    reverify_pathname(&path, &admitted).expect("an unchanged path re-verifies");

    // Created while the original exists, so the replacement has a distinct inode.
    let replacement = path.with_file_name("replacement.json");
    std::fs::write(&replacement, b"{\"version\":1}").expect("replacement file");
    std::fs::rename(&replacement, &path).expect("swap in replacement");
    assert!(
        matches!(
            reverify_pathname(&path, &admitted),
            Err(LoadError::Unreadable(_))
        ),
        "a replacement inode at the path is refused read-only"
    );

    std::fs::remove_file(&path).expect("unlink");
    assert!(
        matches!(
            reverify_pathname(&path, &admitted),
            Err(LoadError::Unreadable(_))
        ),
        "a disappeared path is refused read-only"
    );
}

#[test]
fn the_save_side_refuses_exactly_what_the_loader_refuses() {
    use crate::domain::MAX_PERSISTED_PATH_BYTES;
    let dir = TempDir::new("save-agreement");
    let path = dir.state_file();
    std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");

    let doc = |folder_len: usize| PersistedState {
        version: 1,
        folders: vec![PathBuf::from(format!("/{}", "a".repeat(folder_len - 1)))],
        playlists: Vec::new(),
        subscriptions: Vec::new(),
        radio_favorites: Vec::new(),
        radio_favorites_uuids: Vec::new(),
        track_added_at: Vec::new(),
        episode_resume: Vec::new(),
        podcast_country: None,
        radio_custom_stations: Vec::new(),
        next_playlist_seq: 1,
        track_stats: Vec::new(),
        playback: PlaybackRec::default(),
    };

    let at_cap = doc(MAX_PERSISTED_PATH_BYTES);
    save_to(&path, &at_cap).expect("a path at the authority publishes");
    assert_eq!(
        load_from(&path).expect("and reloads").folders[0]
            .as_os_str()
            .len(),
        MAX_PERSISTED_PATH_BYTES
    );

    let before = file_snapshot(&path);
    let over = doc(MAX_PERSISTED_PATH_BYTES + 1);
    assert!(
        serialize_pretty(&over).is_err(),
        "an over-length path is refused by the serialiser"
    );
    assert!(
        save_to(&path, &over).is_err(),
        "and the save refuses it rather than writing an unreloadable file"
    );
    assert_eq!(
        file_snapshot(&path),
        before,
        "the last-good bytes are preserved on a refused save"
    );
}

#[test]
fn the_aggregate_retained_byte_budget_refuses_before_publication_and_on_load() {
    use crate::domain::{MAX_AGGREGATE_STRING_BYTES, MAX_PERSISTED_PATH_BYTES};
    let dir = TempDir::new("aggregate");
    let path = dir.state_file();
    std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");

    let seed = PersistedState {
        version: 1,
        folders: vec![PathBuf::from("/music")],
        playlists: Vec::new(),
        subscriptions: Vec::new(),
        radio_favorites: Vec::new(),
        radio_favorites_uuids: Vec::new(),
        track_added_at: Vec::new(),
        episode_resume: Vec::new(),
        podcast_country: None,
        radio_custom_stations: Vec::new(),
        next_playlist_seq: 1,
        track_stats: Vec::new(),
        playback: PlaybackRec::default(),
    };
    save_to(&path, &seed).expect("seed the last-good file");
    let before = file_snapshot(&path);

    // Each path is at the per-path cap, but the sum is one record over the
    // aggregate budget.
    let per_path = MAX_PERSISTED_PATH_BYTES;
    let records = MAX_AGGREGATE_STRING_BYTES / per_path + 1;
    let long = PathBuf::from(format!("/{}", "a".repeat(per_path - 1)));
    assert_eq!(long.as_os_str().len(), per_path);
    let stats: Vec<TrackStatsRec> = (0..records)
        .map(|_| TrackStatsRec {
            path: long.clone(),
            rating: 0,
            play_count: 1,
            last_played_unix: 0,
        })
        .collect();
    let over = PersistedState {
        version: 1,
        folders: Vec::new(),
        playlists: Vec::new(),
        subscriptions: Vec::new(),
        radio_favorites: Vec::new(),
        radio_favorites_uuids: Vec::new(),
        track_added_at: Vec::new(),
        episode_resume: Vec::new(),
        podcast_country: None,
        radio_custom_stations: Vec::new(),
        next_playlist_seq: 1,
        track_stats: stats,
        playback: PlaybackRec::default(),
    };

    assert!(
        serialize_pretty(&over).is_err(),
        "an over-aggregate document is refused by the serialiser"
    );
    assert!(save_to(&path, &over).is_err(), "and the save refuses it");
    assert_eq!(
        file_snapshot(&path),
        before,
        "the last-good bytes survive a refused over-aggregate save"
    );

    // Raw JSON bypasses the save guard, so this exercises the load-side refusal.
    let raw = serde_json::to_vec(&over).expect("raw json bypasses the save guard");
    std::fs::write(&path, &raw).expect("write oversize raw");
    assert!(
        matches!(load_from(&path), Err(LoadError::OverLimit(_))),
        "the aggregate budget refuses during the bounded parse"
    );
}

#[test]
fn a_large_escaped_json_string_is_refused_before_its_decoded_allocation_is_retained() {
    let dir = TempDir::new("escaped");
    let path = dir.state_file();
    std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");

    // Six-byte `\u0041` escapes decode to far more than the description cap; the
    // load path has no byte backstop, so only the bounded visitor can refuse it.
    let decoded_len = 100_000;
    assert!(decoded_len > crate::domain::MAX_FEED_DESCRIPTION_BYTES);
    let escaped = "\\u0041".repeat(decoded_len);
    let json = format!(
        "{{\"version\":1,\"subscriptions\":[{{\"feed_url\":\"u\",\"description\":\"{escaped}\"}}]}}"
    );
    std::fs::write(&path, json.as_bytes()).expect("write escaped");
    assert!(
        matches!(load_from(&path), Err(LoadError::OverLimit(_))),
        "an escaped string over the field cap is refused during parse"
    );

    // Positive control: a description of exactly the cap loads.
    let ok = "\\u0041".repeat(crate::domain::MAX_FEED_DESCRIPTION_BYTES);
    let json_ok = format!(
        "{{\"version\":1,\"subscriptions\":[{{\"feed_url\":\"u\",\"description\":\"{ok}\"}}]}}"
    );
    std::fs::write(&path, json_ok.as_bytes()).expect("write ok");
    assert_eq!(
        load_from(&path)
            .expect("a description at the cap loads")
            .subscriptions[0]
            .description
            .len(),
        crate::domain::MAX_FEED_DESCRIPTION_BYTES
    );
}
