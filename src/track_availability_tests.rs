//! Track-availability controls through the real reducer and bounded scan/reconcile path,
//! never a hand-poked model field.

use std::path::PathBuf;

use crate::domain::{ContextAction, ContextTarget, Track, TrackAvailability};
use crate::library_watch::{RootScan, ScanCompletion};
use crate::{Message, MusicApp};

fn track(path: &str, title: &str) -> Track {
    Track {
        id: 0,
        path: PathBuf::from(path),
        title: title.to_string(),
        artist: "Artist".into(),
        album: "Album".into(),
        album_artist: "Artist".into(),
        track_no: 1,
        disc_no: 1,
        year: 2024,
        genre: "Rock".into(),
        format: "MP3".into(),
        duration_secs: 180.0,
        bitrate_kbps: 320,
        sample_rate: 44_100,
        channels: 2,
        bit_depth: 16,
        cover_path: None,
        cover: None,
        added_at: 1_700_000_000,
        play_count: 0,
        last_played_seq: 0,
        last_played_unix: 0,
        replay_gain: crate::domain::ReplayGain::default(),
    }
}

fn app_with_root(root: &std::path::Path) -> (MusicApp, u64) {
    let (app, _task) = MusicApp::boot_with(false, false, vec![root.to_path_buf()]);
    let generation = app
        .library_watch
        .in_flight_generation(root)
        .expect("boot dispatches the configured root's initial scan");
    (app, generation)
}

fn complete_scan(root: &std::path::Path, generation: u64, tracks: Vec<Track>) -> RootScan {
    RootScan {
        root: root.to_path_buf(),
        generation,
        tracks,
        undecodable: Vec::new(),
        directories: vec![root.to_path_buf()],
        completion: ScanCompletion::Complete,
        scope: None,
        probed: 0,
    }
}

fn deliver_rescan(app: &mut MusicApp, root: &std::path::Path, tracks: Vec<Track>) {
    let now = std::time::Instant::now();
    app.library_watch.request_rescan(root.to_path_buf(), now);
    let dispatched = app.library_watch.dispatch_due(now);
    let g = dispatched
        .first()
        .expect("a re-armed root is due for dispatch")
        .generation;
    let scan = complete_scan(root, g, tracks);
    let _ = app.update(Message::RootScanComplete(Box::new(scan)));
}

#[test]
fn only_a_scanned_present_track_plays_and_a_missing_one_stops() {
    let root = PathBuf::from("/music/lib");
    let (mut app, generation) = app_with_root(&root);
    let id = app.library.add_track(track("/music/lib/a.mp3", "A"));
    app.library.reindex();

    let _ = app.update(Message::PlayTrack(id));
    assert!(
        !app.playback.playing,
        "a not-yet-verified (Checking) track refuses playback until a scan proves it Present"
    );
    assert_eq!(
        app.library.availability_of(id),
        TrackAvailability::Unknown,
        "an unscanned row is Unknown/Checking, never a fabricated Present"
    );

    let scan = complete_scan(&root, generation, vec![track("/music/lib/a.mp3", "A")]);
    let _ = app.update(Message::RootScanComplete(Box::new(scan)));
    assert_eq!(
        app.library.availability_of(id),
        TrackAvailability::Present,
        "an authoritative scan proves the source Present"
    );
    let _ = app.update(Message::PlayTrack(id));
    assert!(app.playback.playing, "a scanned-Present track plays");
    assert_eq!(app.playback.current_track_id, Some(id));

    deliver_rescan(&mut app, &root, Vec::new());
    assert_eq!(
        app.library.availability_of(id),
        TrackAvailability::Missing,
        "the absent, root-owned source is classified Missing"
    );
    assert!(
        !app.playback.playing,
        "a current source that became Missing stops the audio route"
    );
    assert_eq!(
        app.playback.current_track_id,
        Some(id),
        "the row identity is retained, not advanced away"
    );
    assert!(
        app.library.tracks.contains_key(&id),
        "the durable row is retained, never auto-deleted"
    );

    let _ = app.update(Message::PlayTrack(id));
    assert!(
        !app.playback.playing,
        "a manual activation of a Missing source is refused, not played"
    );

    // a.mp3 stays Missing, so Play Library must skip it to reach b.mp3.
    let playable = app.library.add_track(track("/music/lib/b.mp3", "B"));
    app.library.reindex();
    deliver_rescan(&mut app, &root, vec![track("/music/lib/b.mp3", "B")]);
    assert_eq!(
        app.library.availability_of(id),
        TrackAvailability::Missing,
        "the still-absent first row stays Missing across the rescan"
    );
    assert_eq!(
        app.library.availability_of(playable),
        TrackAvailability::Present,
        "the newly-scanned row is proven Present"
    );
    let _ = app.update(Message::PlayLibrary);
    assert!(
        app.playback.playing,
        "Play Library starts the first playable"
    );
    assert_eq!(
        app.playback.current_track_id,
        Some(playable),
        "the blocked first row is skipped to the first Present id"
    );
    assert!(
        !app.playback.queue.contains(&id),
        "only Present ids are seeded; the Missing id is not queued by Play Library"
    );

    // Only a user Remove deletes a queued Missing row.
    deliver_rescan(&mut app, &root, Vec::new());
    assert_eq!(
        app.library.availability_of(playable),
        TrackAvailability::Missing,
        "the queued row that vanished reads Missing (the Up Next badge's data)"
    );
    assert!(
        app.playback.queue.contains(&playable) || app.playback.queue.is_empty(),
        "a queued row keeps its slot; only a user Remove deletes it"
    );
    let _ = app.update(Message::PlayLibrary);
    assert!(
        !app.playback.playing,
        "an all-blocked library refuses Play Library and marks nothing playing"
    );

    let (mut app, generation) = app_with_root(&root);
    let rid = app.library.add_track(track("/music/lib/restored.mp3", "R"));
    app.library.reindex();
    app.playback.current_track_id = Some(rid);
    app.playback.position_secs = 33.0;
    app.arm_restored_paused_source();
    assert!(
        app.restore_intent.is_some(),
        "a restored not-yet-Present current track defers its load behind an intent"
    );
    assert!(
        app.pending_source_open.is_none(),
        "the restored file is not opened before a scan proves it Present"
    );
    let scan = complete_scan(
        &root,
        generation,
        vec![track("/music/lib/restored.mp3", "R")],
    );
    let _ = app.update(Message::RootScanComplete(Box::new(scan)));
    assert_eq!(
        app.library.availability_of(rid),
        TrackAvailability::Present,
        "the owning-root scan proves the restored track Present"
    );
    assert!(
        app.restore_intent.is_none(),
        "the matching Present reconcile fires the one paused load and clears the intent"
    );

    let (mut app, generation) = app_with_root(&root);
    let gid = app.library.add_track(track("/music/lib/gone.mp3", "G"));
    app.library.reindex();
    app.playback.current_track_id = Some(gid);
    app.arm_restored_paused_source();
    assert!(app.restore_intent.is_some());
    let scan = complete_scan(&root, generation, Vec::new());
    let _ = app.update(Message::RootScanComplete(Box::new(scan)));
    assert_eq!(
        app.library.availability_of(gid),
        TrackAvailability::Missing,
        "the absent restored row is proved Missing"
    );
    assert!(
        app.restore_intent.is_none(),
        "a Missing decision retires the deferred restore"
    );
    assert!(
        app.pending_source_open.is_none(),
        "the removed/unmounted file is never opened"
    );

    let (mut app, _g) = app_with_root(&root);
    let tid = app.library.add_track(track("/music/lib/t.mp3", "T"));
    app.library.reindex();
    app.playback.current_track_id = Some(tid);
    assert!(!app.playback.playing);
    let _ = app.update(Message::TogglePlayPause);
    assert!(
        !app.playback.playing,
        "TogglePlayPause refuses starting a not-yet-Present local current track"
    );

    let (mut app, generation) = app_with_root(&root);
    let eid = app.library.add_track(track("/music/lib/e.mp3", "E"));
    app.library.reindex();
    app.availability_generation = u64::MAX;
    let scan = complete_scan(&root, generation, vec![track("/music/lib/e.mp3", "E")]);
    let _ = app.update(Message::RootScanComplete(Box::new(scan)));
    assert!(
        app.availability_exhausted,
        "a decision at the generation ceiling exhausts the counter"
    );
    assert_ne!(
        app.library.availability_of(eid),
        TrackAvailability::Present,
        "a reconcile after exhaustion is inert — it decides nothing Present"
    );
    app.playback.current_track_id = Some(eid);
    let _ = app.update(Message::TogglePlayPause);
    assert!(
        !app.playback.playing,
        "after generation exhaustion TogglePlayPause refuses local playback"
    );

    let (mut app, _g) = app_with_root(&root);
    let untouched = app
        .library
        .add_track(track("/music/lib/untouched.mp3", "U"));
    app.library.reindex();
    let _ = app.update(Message::ScanComplete(vec![track(
        "/music/lib/added.mp3",
        "A",
    )]));
    let added = app
        .library
        .track_id_for_path(&PathBuf::from("/music/lib/added.mp3"))
        .expect("ScanComplete merges the returned track into the library");
    assert_eq!(
        app.library.availability_of(added),
        TrackAvailability::Present,
        "a ScanComplete-returned track is proven Present"
    );
    assert!(
        app.library.is_playable_now(added),
        "and is therefore playable"
    );
    assert_eq!(
        app.library.availability_of(untouched),
        TrackAvailability::Unknown,
        "a row ScanComplete did not return is neither fabricated Present nor absent"
    );

    let _ = app.update(Message::RescanComplete(vec![track(
        "/music/lib/untouched.mp3",
        "U",
    )]));
    assert_eq!(
        app.library.availability_of(untouched),
        TrackAvailability::Present,
        "a RescanComplete-returned track is proven Present and playable"
    );
    assert_eq!(
        app.library.availability_of(added),
        TrackAvailability::Present,
        "the earlier row keeps Present — RescanComplete authorizes no absence"
    );
}

#[test]
fn an_undecodable_row_is_non_actionable_with_actions_reachable() {
    let root = PathBuf::from("/music/lib");
    let (mut app, generation) = app_with_root(&root);
    let id = app.library.add_track(track("/music/lib/b.mp3", "B"));
    app.library.reindex();

    let mut scan = complete_scan(&root, generation, Vec::new());
    scan.undecodable = vec![PathBuf::from("/music/lib/b.mp3")];
    let _ = app.update(Message::RootScanComplete(Box::new(scan)));

    assert_eq!(
        app.library.availability_of(id),
        TrackAvailability::Undecodable,
        "an admitted-but-undecodable candidate is classified Undecodable, not Missing"
    );
    assert!(
        !app.library.is_playable_now(id),
        "an undecodable row is non-actionable for playback"
    );

    let _ = app.update(Message::PlayTrack(id));
    assert!(
        !app.playback.playing,
        "activating an undecodable row does not mark it playing"
    );

    let items =
        crate::context_menu::items_for(ContextTarget::Track(id), &app.library, &app.playback);
    assert!(
        items.contains(&ContextAction::Rescan),
        "the Rescan recovery row is reachable on an undecodable track"
    );
    assert!(
        items.contains(&ContextAction::Remove),
        "the Remove recovery row is reachable on an undecodable track"
    );
}

#[test]
fn a_stale_rescan_cannot_transfer_availability() {
    let mut lib = crate::domain::Library::new();
    let path = PathBuf::from("/music/lib/c.mp3");
    assert!(
        lib.decide_availability(path.clone(), TrackAvailability::Present, 5),
        "a first decision is recorded"
    );
    assert!(
        !lib.decide_availability(path.clone(), TrackAvailability::Missing, 3),
        "a stale (older-generation) decision is inert"
    );
    assert_eq!(
        lib.availability_of_path(&path),
        TrackAvailability::Present,
        "the fresher decision survives a stale one"
    );
    assert!(
        lib.decide_availability(path.clone(), TrackAvailability::Missing, 6),
        "a newer-generation decision does apply (positive control)"
    );
    assert_eq!(lib.availability_of_path(&path), TrackAvailability::Missing);

    // The watcher rejects a stale completion before the reconcile.
    let root = PathBuf::from("/music/lib");
    let (mut app, generation) = app_with_root(&root);
    let id = app.library.add_track(track("/music/lib/d.mp3", "D"));
    app.library.reindex();
    let scan = complete_scan(&root, generation, vec![track("/music/lib/d.mp3", "D")]);
    let _ = app.update(Message::RootScanComplete(Box::new(scan)));
    assert_eq!(
        app.library.availability_of(id),
        TrackAvailability::Present,
        "the authoritative scan proved the source Present"
    );
    let stale = complete_scan(&root, generation.wrapping_sub(1), Vec::new());
    let _ = app.update(Message::RootScanComplete(Box::new(stale)));
    assert_eq!(
        app.library.availability_of(id),
        TrackAvailability::Present,
        "a stale scan is rejected before reconcile and cannot transfer Missing"
    );
    assert!(
        app.library.tracks.contains_key(&id),
        "the stale scan mutates nothing"
    );
}

#[test]
fn a_folder_rescan_classifies_absence_only_inside_that_folder() {
    let root = PathBuf::from("/music/lib");
    let (mut app, generation) = app_with_root(&root);
    let outside = app.library.add_track(track("/music/lib/a.mp3", "A"));
    let inside = app.library.add_track(track("/music/lib/sub/b.mp3", "B"));
    app.library.reindex();
    let scan = complete_scan(
        &root,
        generation,
        vec![
            track("/music/lib/a.mp3", "A"),
            track("/music/lib/sub/b.mp3", "B"),
        ],
    );
    let _ = app.update(Message::RootScanComplete(Box::new(scan)));

    let now = std::time::Instant::now();
    app.library_watch.request_rescan(root.clone(), now);
    let g = app.library_watch.dispatch_due(now)[0].generation;
    let scan = RootScan {
        scope: Some(vec![root.join("sub")]),
        ..complete_scan(&root, g, Vec::new())
    };
    let _ = app.update(Message::RootScanComplete(Box::new(scan)));
    assert_eq!(
        app.library.availability_of(outside),
        TrackAvailability::Present
    );
    assert_eq!(
        app.library.availability_of(inside),
        TrackAvailability::Missing
    );
}
