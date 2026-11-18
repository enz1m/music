//! Authority and publication: rotation, substitution, races, retire,
//! and reacquisition.

use super::*;

// The nofollow bounded reader owns decoding; the retained `StateAuthority` owns publication.

fn doc_with_folder(folder: &str) -> PersistedState {
    PersistedState {
        version: FORMAT_VERSION,
        folders: vec![PathBuf::from(folder)],
        playlists: Vec::new(),
        subscriptions: Vec::new(),
        radio_favorites: Vec::new(),
        radio_favorites_uuids: Vec::new(),
        track_added_at: Vec::new(),
        episode_resume: Vec::new(),
        podcast_country: None,
        radio_custom_stations: Vec::new(),
        next_playlist_seq: 1,
        playback: PlaybackRec::default(),
        track_stats: Vec::new(),
    }
}

#[test]
fn the_authority_publishes_an_existing_file_and_rotates_across_repeated_saves() {
    let dir = TempDir::new("authority-existing");
    let path = dir.state_file();
    std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    seed_state(&path, serialized(&doc_with_folder("/seed")));

    let (doc, mut persist, origin) = open_at(&path);
    assert!(matches!(origin, Origin::Restored));
    assert!(
        persist.is_enabled() && doc.is_some(),
        "an owner-only existing file acquires an authority"
    );

    let queue = persistence::PersistenceQueue::default();
    for folder in ["/one", "/two"] {
        let pending = persist
            .prepare(doc_with_folder(folder), &queue)
            .expect("a differing document prepares a save");
        assert_eq!(
            pending.complete(&queue),
            persistence::PersistenceCompletion::Saved,
            "publication through the retained authority succeeds"
        );
        persist.acknowledge_saved(&pending);
    }
    assert_eq!(
        load_from(&path).expect("reload").folders,
        vec![PathBuf::from("/two")],
        "the latest publication is the durable one"
    );
}

#[test]
fn a_substituted_destination_refuses_publication_and_preserves_foreign_bytes() {
    let dir = TempDir::new("authority-substitution");
    let path = dir.state_file();
    std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    seed_state(&path, serialized(&doc_with_folder("/seed")));

    let (_doc, mut persist, _origin) = open_at(&path);
    let queue = persistence::PersistenceQueue::default();

    let foreign = path.with_file_name("foreign.json");
    seed_state(&foreign, serialized(&doc_with_folder("/foreign")));
    std::fs::rename(&foreign, &path).expect("substitute the destination inode");
    let foreign_bytes = std::fs::read(&path).expect("foreign bytes");

    let pending = persist
        .prepare(doc_with_folder("/mine"), &queue)
        .expect("a save prepares");
    assert!(
        matches!(
            pending.complete(&queue),
            persistence::PersistenceCompletion::Error(_)
        ),
        "a substituted destination refuses publication"
    );
    assert_eq!(
        std::fs::read(&path).expect("after"),
        foreign_bytes,
        "the foreign writer's bytes are left byte-identical"
    );
    assert!(
        !persist.authority_lost_custody(),
        "a substituted-destination refusal is a safe refusal, not lost custody"
    );

    // The authority never reopens over an inode it cannot prove.
    let again = persist
        .prepare(doc_with_folder("/mine-again"), &queue)
        .expect("a second save prepares");
    assert!(matches!(
        again.complete(&queue),
        persistence::PersistenceCompletion::Error(_)
    ));
    assert_eq!(
        std::fs::read(&path).expect("after second"),
        foreign_bytes,
        "the foreign bytes survive a repeated refused publication"
    );
}

#[test]
fn a_group_accessible_state_file_stays_read_only_with_no_authority() {
    use std::os::unix::fs::PermissionsExt;
    let dir = TempDir::new("authority-mode");
    let path = dir.state_file();
    std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    std::fs::write(&path, serialized(&doc_with_folder("/seed"))).expect("write");
    // Plain group-readable files are repaired to 0600; a setuid file is still refused.
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o4644)).expect("widen mode");
    assert_eq!(
        std::fs::metadata(&path).expect("stat").permissions().mode() & 0o7777,
        0o4644,
        "positive control: the fixture carries the special bit"
    );

    let (_doc, mut persist, origin) = open_at(&path);
    assert!(
        matches!(origin, Origin::RefusedReadOnly(_)),
        "a file the authority cannot retain refuses read-only"
    );
    assert!(
        !persist.is_enabled(),
        "a refused acquisition leaves the session read-only"
    );
    let queue = persistence::PersistenceQueue::default();
    assert!(
        persist.prepare(doc_with_folder("/x"), &queue).is_none(),
        "a read-only session prepares no write"
    );
}

#[test]
fn a_stale_authority_reacquisition_is_dropped() {
    let mut app = MusicApp::boot_with(false, false, Vec::new()).0;
    app.boot_load_generation = 3;
    let stale = crate::AuthorityRefresh {
        generation: 1,
        outcome: crate::ReacquireOutcome::Unavailable {
            retire_refusal: None,
        },
    };
    let _ = app.update_inner(Message::AuthorityReacquired(Box::new(stale)));
    assert_eq!(
        app.boot_load_generation, 3,
        "a stale reacquisition does not advance the guard"
    );
    assert!(
        !app.boot_load_worker_spawned,
        "a stale reacquisition leaves the worker bound untouched"
    );
}

#[test]
fn a_current_authority_reacquisition_installs_and_resumes_saving() {
    let dir = TempDir::new("reacquire-current");
    let path = dir.state_file();
    std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    seed_state(&path, serialized(&doc_with_folder("/seed")));

    let (_doc, mut persist, _origin) = open_at(&path);
    let fresh = enzui_core::state_authority::StateAuthority::open_or_create(&path)
        .expect("a fresh authority acquires the same owner-only file");
    persist.install_authority(std::sync::Arc::new(std::sync::Mutex::new(fresh)));

    let queue = persistence::PersistenceQueue::default();
    let pending = persist
        .prepare(doc_with_folder("/resumed"), &queue)
        .expect("a save prepares through the reinstalled authority");
    assert_eq!(
        pending.complete(&queue),
        persistence::PersistenceCompletion::Saved
    );
    assert_eq!(
        load_from(&path).expect("reload").folders,
        vec![PathBuf::from("/resumed")],
        "publication resumes through the reinstalled authority"
    );
}

#[test]
fn a_retire_with_the_expected_identity_moves_and_reacquires() {
    let dir = TempDir::new("retire-match");
    let path = dir.state_file();
    std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    let seed = serialized(&doc_with_folder("/old"));
    seed_state(&path, &seed);
    let expected = file_identity(&path);

    match persistence::retire_and_reacquire(&path, Some(expected)) {
        persistence::RetireOutcome::Reacquired {
            authority,
            recovered,
        } => {
            drop(authority);
            let recovery = recovered
                .or_else(|| recovery_sibling(&path, &seed))
                .expect("the retired bytes are preserved under a recovery name");
            assert_eq!(
                std::fs::read(&recovery).expect("recovery bytes"),
                seed,
                "the retired bytes are byte-identical under recovery custody"
            );
        }
        persistence::RetireOutcome::Refused(reason) => {
            panic!("a matching-identity retire must move the file: {reason:?}")
        }
    }
}

#[test]
fn a_substitution_before_retire_is_refused_and_preserves_the_foreign_inode() {
    let dir = TempDir::new("retire-substitution");
    let path = dir.state_file();
    std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    seed_state(&path, serialized(&doc_with_folder("/read")));
    let expected = file_identity(&path);

    let foreign_bytes = serialized(&doc_with_folder("/foreign"));
    let foreign = path.with_file_name("foreign.json");
    seed_state(&foreign, &foreign_bytes);
    std::fs::rename(&foreign, &path).expect("substitute the destination inode");

    let outcome = persistence::retire_and_reacquire(&path, Some(expected));
    assert!(
        matches!(
            outcome,
            persistence::RetireOutcome::Refused(persistence::RetireRefusal::SourceMismatch)
        ),
        "a substituted inode is refused as a source mismatch"
    );
    assert_eq!(
        std::fs::read(&path).expect("after"),
        foreign_bytes,
        "the foreign inode is left byte-identical"
    );
}
