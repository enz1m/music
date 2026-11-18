//! Automatic library watching and reconciliation over a real watch service with an
//! in-memory backend, a real scan owner, and the real reducer. Deadlines are explicit
//! `Instant`s, so debounce assertions do not sleep.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use enzui_core::blocking_task::BlockingTaskOwner;
use enzui_core::file_tree_watch::{
    DirectoryChange, InMemoryDirectoryBackend, RawEventKind, WatchOutcome, WatchRefusal,
};

use crate::domain::Track;
use crate::library_watch::{
    DEBOUNCE, LibraryWatcher, RefusalReason, RootScan, RootScanProvider, ScanCompletion,
    ScanLimits, scan_root_bounded,
};
use crate::{Message, MusicApp};

fn track(path: &str, title: &str, play_count: u32) -> Track {
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
        play_count,
        last_played_seq: 0,
        last_played_unix: 0,
        replay_gain: crate::domain::ReplayGain::default(),
    }
}

fn owner() -> BlockingTaskOwner {
    BlockingTaskOwner::new(
        "test-scan",
        std::num::NonZeroUsize::new(1).unwrap(),
        std::num::NonZeroUsize::new(1).unwrap(),
    )
    .expect("spawn scan owner")
}

type StagedScan = (Vec<Track>, Vec<PathBuf>, ScanCompletion);

#[derive(Default)]
struct StagedScanProvider {
    staged: Mutex<VecDeque<StagedScan>>,
    calls: Mutex<Vec<(PathBuf, u64)>>,
}

impl StagedScanProvider {}

impl RootScanProvider for StagedScanProvider {
    fn scan(&self, root: &Path, generation: u64, _cancelled: &dyn Fn() -> bool) -> RootScan {
        self.calls
            .lock()
            .unwrap()
            .push((root.to_path_buf(), generation));
        let (tracks, directories, completion) =
            self.staged.lock().unwrap().pop_front().unwrap_or_else(|| {
                (
                    Vec::new(),
                    vec![root.to_path_buf()],
                    ScanCompletion::Complete,
                )
            });
        RootScan {
            root: root.to_path_buf(),
            generation,
            tracks,
            undecodable: Vec::new(),
            directories,
            completion,
        }
    }
}

// Leaves a live sink for the backend to emit against.
fn install_root_watch(watcher: &mut LibraryWatcher, root: &Path, now: Instant) {
    watcher.register_root(root.to_path_buf(), now);
    let dispatch = watcher.dispatch_due(now);
    assert_eq!(
        dispatch.len(),
        1,
        "the registered root arms one initial scan"
    );
    let generation = dispatch[0].generation;
    let complete = RootScan {
        root: root.to_path_buf(),
        generation,
        tracks: Vec::new(),
        undecodable: Vec::new(),
        directories: vec![root.to_path_buf()],
        completion: ScanCompletion::Complete,
    };
    assert!(watcher.accept_completion(&complete, now));
    watcher.reconcile_watches(root, &complete.directories);
    assert!(
        watcher.watch_count(root) >= 1,
        "the root directory is watched"
    );
}

#[test]
fn bounded_scan_reports_vanished_missing_complete_empty_and_depth_refusal() {
    let base = std::env::temp_dir().join(format!("music-watch-scan-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);

    let missing = base.join("does-not-exist");
    let vanished = scan_root_bounded(&missing, 1, &ScanLimits::default(), &|| false);
    assert_eq!(
        vanished.completion,
        ScanCompletion::Vanished,
        "a missing root is Vanished, not an empty Complete"
    );

    let empty = base.join("empty");
    std::fs::create_dir_all(&empty).unwrap();
    let ok = scan_root_bounded(&empty, 2, &ScanLimits::default(), &|| false);
    assert_eq!(
        ok.completion,
        ScanCompletion::Complete,
        "an empty readable folder is a successful Complete"
    );
    assert!(ok.tracks.is_empty(), "an empty folder admits no tracks");
    assert_eq!(ok.directories, vec![empty.clone()], "the root is traversed");

    let deep = base.join("deep");
    std::fs::create_dir_all(deep.join("a").join("b").join("c")).unwrap();
    let limits = ScanLimits {
        max_depth: 1,
        ..ScanLimits::default()
    };
    let refused = scan_root_bounded(&deep, 3, &limits, &|| false);
    assert_eq!(
        refused.completion,
        ScanCompletion::Refused(RefusalReason::Depth),
        "traversal beyond the depth cap is Refused, not Complete"
    );

    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn bounded_scan_honours_cooperative_cancellation() {
    let base = std::env::temp_dir().join(format!("music-watch-cancel-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(base.join("child")).unwrap();
    let refused = scan_root_bounded(&base, 1, &ScanLimits::default(), &|| true);
    assert_eq!(
        refused.completion,
        ScanCompletion::Refused(RefusalReason::Cancelled),
        "a cancelled scan is Refused(Cancelled), never Complete"
    );
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn a_delivery_burst_coalesces_into_one_debounced_scan() {
    let root = PathBuf::from("/music/coalesce");
    let backend = Arc::new(InMemoryDirectoryBackend::new());
    let mut watcher = LibraryWatcher::with_parts(
        backend.clone(),
        Arc::new(StagedScanProvider::default()),
        owner(),
    );
    let now = Instant::now();
    install_root_watch(&mut watcher, &root, now);

    backend.emit(
        &root,
        RawEventKind::Change(DirectoryChange::Created(root.join("a.mp3"))),
    );
    backend.emit(
        &root,
        RawEventKind::Change(DirectoryChange::Created(root.join("b.mp3"))),
    );
    backend.emit(
        &root,
        RawEventKind::Change(DirectoryChange::Modified(root.join("a.mp3"))),
    );
    let deliveries = watcher.service().take_deliveries();
    watcher.on_deliveries(&deliveries, now);

    assert!(
        watcher.dispatch_due(now).is_empty(),
        "no scan runs before the debounce deadline"
    );
    let after = now + DEBOUNCE + Duration::from_millis(1);
    let due = watcher.dispatch_due(after);
    assert_eq!(
        due.len(),
        1,
        "the whole burst coalesces into exactly one debounced scan"
    );
}

#[test]
fn overflow_schedules_a_full_authoritative_rescan() {
    let root = PathBuf::from("/music/overflow");
    let backend = Arc::new(InMemoryDirectoryBackend::new());
    let mut watcher = LibraryWatcher::with_parts(
        backend.clone(),
        Arc::new(StagedScanProvider::default()),
        owner(),
    );
    let now = Instant::now();
    install_root_watch(&mut watcher, &root, now);

    backend.emit(&root, RawEventKind::Overflow);
    let deliveries = watcher.service().take_deliveries();
    assert!(
        deliveries
            .iter()
            .any(|d| d.outcome == WatchOutcome::Overflow),
        "the backend overflow reaches the coordinator"
    );
    watcher.on_deliveries(&deliveries, now);
    let due = watcher.dispatch_due(now + DEBOUNCE + Duration::from_millis(1));
    assert_eq!(due.len(), 1, "overflow arms one full rescan");
}

fn app_with_root(root: &Path) -> (MusicApp, u64) {
    let (app, _task) = MusicApp::boot_with(false, false, vec![root.to_path_buf()]);
    // Boot already has the root in flight; a second scan would never be accepted.
    let generation = app
        .library_watch
        .in_flight_generation(root)
        .expect("boot dispatches the configured root's initial scan");
    (app, generation)
}

#[test]
fn a_complete_scan_adds_new_preserves_survivors_and_marks_absent_missing() {
    use crate::domain::TrackAvailability;
    let root = PathBuf::from("/music/lib");
    let (mut app, generation) = app_with_root(&root);

    let id_a = app.library.add_track(track("/music/lib/a.mp3", "A", 3));
    let id_b = app.library.add_track(track("/music/lib/b.mp3", "B", 7));
    app.library.reindex();
    app.playback.queue = vec![id_a, id_b];
    app.playback.resync_queue_entry_ids();
    app.playback.load_current_track(id_a, &app.library);

    // a is marked Missing, not deleted: its row and queue slot are retained.
    let scan = RootScan {
        root: root.clone(),
        generation,
        tracks: vec![
            track("/music/lib/b.mp3", "B v2", 0),
            track("/music/lib/c.mp3", "C", 0),
        ],
        undecodable: Vec::new(),
        directories: vec![root.clone()],
        completion: ScanCompletion::Complete,
    };
    let _ = app.update(Message::RootScanComplete(Box::new(scan)));

    assert!(
        app.library.tracks.contains_key(&id_a),
        "the absent path's row is retained, not deleted, by a complete scan"
    );
    assert_eq!(
        app.library.availability_of(id_a),
        TrackAvailability::Missing,
        "the absent, root-owned path is classified Missing"
    );
    let b = app
        .library
        .tracks
        .get(&id_b)
        .expect("the surviving path keeps its stable id");
    assert_eq!(b.play_count, 7, "the survivor keeps its play counter");
    assert_eq!(b.title, "B v2", "the survivor's tags are refreshed");
    assert_eq!(
        app.library.availability_of(id_b),
        TrackAvailability::Present,
        "the surviving, re-probed path is classified Present"
    );
    let c_id = app
        .library
        .tracks
        .values()
        .find(|t| t.title == "C")
        .map(|t| t.id)
        .expect("the newly discovered file is added");
    assert_eq!(
        app.library.availability_of(c_id),
        TrackAvailability::Present,
        "a freshly probed file is Present"
    );
    assert_eq!(
        app.playback.current_track_id,
        Some(id_a),
        "a current track that became Missing keeps its identity (no auto-advance)"
    );
    assert_eq!(
        app.playback.queue,
        vec![id_a, id_b],
        "the Missing id stays in the queue"
    );
}

#[test]
fn a_refused_scan_merges_discoveries_but_never_removes() {
    let root = PathBuf::from("/music/partial");
    let (mut app, generation) = app_with_root(&root);
    let id_a = app.library.add_track(track("/music/partial/a.mp3", "A", 1));
    app.library.add_track(track("/music/partial/b.mp3", "B", 1));
    app.library.reindex();

    let scan = RootScan {
        root: root.clone(),
        generation,
        tracks: vec![track("/music/partial/c.mp3", "C", 0)],
        undecodable: Vec::new(),
        directories: vec![root.clone()],
        completion: ScanCompletion::Refused(RefusalReason::Directories),
    };
    let _ = app.update(Message::RootScanComplete(Box::new(scan)));

    assert!(
        app.library.tracks.contains_key(&id_a),
        "a refused/partial scan does not delete an absent-from-partial path"
    );
    assert!(
        app.library.tracks.values().any(|t| t.title == "C"),
        "a refused scan still merges the files it did admit"
    );
    assert!(
        matches!(
            app.library_watch.availability(&root),
            Some(crate::library_watch::RootAvailability::Partial(_))
        ),
        "a refused root is marked partial"
    );
}

#[test]
fn a_removed_folder_makes_its_late_completion_inert_through_the_reducer() {
    let root = PathBuf::from("/music/gone");
    let (mut app, generation) = app_with_root(&root);
    let id = app.library.add_track(track("/music/gone/a.mp3", "A", 4));
    app.library.reindex();
    let before = app.library.track_count();

    let _ = app.update(Message::RemoveFolder(root.clone()));

    let scan = RootScan {
        root: root.clone(),
        generation,
        tracks: Vec::new(),
        undecodable: Vec::new(),
        directories: vec![root.clone()],
        completion: ScanCompletion::Complete,
    };
    let _ = app.update(Message::RootScanComplete(Box::new(scan)));

    assert_eq!(
        app.library.track_count(),
        before,
        "a stale completion for a removed folder mutates nothing"
    );
    assert!(
        app.library.tracks.contains_key(&id),
        "the removed folder's already-scanned tracks are retained"
    );
}

#[test]
fn bounded_scan_charges_path_bytes_for_nonaudio_entries() {
    let base = std::env::temp_dir().join(format!("music-watch-bytes-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();
    std::fs::write(base.join("a-long-readme-name.txt"), b"x").unwrap();
    // The budget fits the root directory path exactly, so only a charged
    // non-audio entry can push it over — a scan that charged only admitted
    // tracks would report Complete here.
    let limits = ScanLimits {
        max_path_bytes: base.as_os_str().len(),
        ..ScanLimits::default()
    };
    let scan = scan_root_bounded(&base, 1, &limits, &|| false);
    assert_eq!(
        scan.completion,
        ScanCompletion::Refused(RefusalReason::PathBytes),
        "a non-audio entry must be charged against the path-byte cap"
    );
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn a_failed_watch_install_retains_the_exact_prior_safe_set() {
    let root = PathBuf::from("/music/rollback");
    let backend = Arc::new(InMemoryDirectoryBackend::new());
    let mut watcher = LibraryWatcher::with_parts(
        backend.clone(),
        Arc::new(StagedScanProvider::default()),
        owner(),
    );
    let now = Instant::now();
    install_root_watch(&mut watcher, &root, now);
    let safe = watcher.watch_count(&root);
    assert_eq!(safe, 1, "the root is the prior safe set");

    backend.force_next_refusal(WatchRefusal::BackendError("boom".into()));
    watcher.reconcile_watches(&root, &[root.clone(), root.join("child")]);

    assert_eq!(
        watcher.watch_count(&root),
        safe,
        "a failed install rolls back and leaves the exact prior safe set"
    );
    assert!(
        matches!(
            watcher.availability(&root),
            Some(crate::library_watch::RootAvailability::Partial(_))
        ),
        "a failed install degrades visibly to Partial"
    );
}
