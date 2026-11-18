//! Round trips: collections, relaunch, renames, and load defaults.

use super::*;

#[test]
fn folders_playlists_subscriptions_and_favorites_round_trip() {
    let dir = TempDir::new("collections");
    let path = dir.state_file();
    let folder = dir.0.join("Library");
    std::fs::create_dir_all(&folder).expect("fixture folder");

    // Station ids are hash-derived, so take a seeded one rather than a literal.
    let seeded_id = crate::radio::seed_test_catalog()[1].id;
    let seeded_uuid = crate::radio::seed_test_catalog()[1].remote_uuid.clone();
    {
        let mut app = app_at(&path);
        let uri = FileUri::new(format!("file://{}", folder.display()));
        app.step(Message::PortalFolderOutcome(
            crate::picker::PickerTarget::AddFolder,
            PortalOutcome::Completed(FileResult { uris: vec![uri] }),
        ));

        app.step(Message::ScanComplete(vec![track(
            "/fixture/persist-me.flac",
            123,
        )]));
        app.step(Message::CreatePlaylist);
        let playlist_id = app.library.playlists[0].id;
        let track_id = app.library.track_order[0];
        app.step(Message::AddTrackToPlaylist {
            playlist_id,
            track_id,
        });

        app.step(Message::PodcastSubscribeInputChanged(
            "https://example.test/show.xml".into(),
        ));
        app.step(Message::PodcastSubscribeSubmit);
        app.step(Message::RadioToggleFavorite(seeded_id));
        assert!(
            path.is_file(),
            "positive control: the sandbox must be written"
        );
    }

    let reloaded = app_at(&path);
    assert_eq!(reloaded.library.folders, vec![folder]);
    assert_eq!(reloaded.library.playlists.len(), 1);
    assert_eq!(
        reloaded.library.playlists[0].track_paths,
        vec![PathBuf::from("/fixture/persist-me.flac")]
    );
    assert_eq!(reloaded.subscriptions.len(), 1);
    assert_eq!(
        reloaded.subscriptions[0].feed_url,
        "https://example.test/show.xml"
    );
    // Favourites persist by UUID; `restore` folds them onto the seeded catalog synchronously.
    assert!(
        reloaded.radio.favorites.contains(&seeded_uuid),
        "star must survive relaunch by UUID: {:?}",
        reloaded.radio.favorites
    );
}

#[test]
fn a_corrupt_file_stays_read_only_until_an_explicit_retire() {
    let dir = TempDir::new("corrupt");
    let path = dir.state_file();
    std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    let broken = br#"{"version":1,"playlists":[{"name":"Precious"}"#;
    // Owner-only, or the authority would refuse it before the content check.
    seed_state(&path, broken);

    let (doc, origin, authority, retire_identity) = persistence::acquire(&path);
    assert!(
        doc.is_none() && authority.is_none(),
        "corrupt bytes grant neither a document nor an authority"
    );
    assert!(
        matches!(origin, Origin::RefusedReadOnly(RefusalKind::Corrupt)),
        "corrupt input is refused read-only"
    );
    assert!(
        retire_identity.is_some(),
        "the coherent read identity is retained for an explicit Retire"
    );
    assert_eq!(
        std::fs::read(&path).expect("after boot"),
        broken,
        "boot never quarantines or rewrites the corrupt bytes"
    );
    assert!(
        recovery_sibling(&path, broken).is_none(),
        "boot creates no recovery sibling"
    );

    match persistence::retire_and_reacquire(&path, retire_identity) {
        persistence::RetireOutcome::Reacquired {
            authority,
            recovered,
        } => {
            drop(authority);
            let recovery = recovered
                .or_else(|| recovery_sibling(&path, broken))
                .expect("the corrupt bytes are preserved under a recovery name");
            assert_eq!(
                std::fs::read(&recovery).expect("recovery"),
                broken,
                "the corrupt bytes are byte-identical under recovery custody"
            );
        }
        persistence::RetireOutcome::Refused(reason) => {
            panic!("an explicit retire of the read inode must move it: {reason:?}")
        }
    }
}

#[test]
fn duplicate_playlist_ids_are_refused_and_left_read_only() {
    let dir = TempDir::new("dup-id");
    let path = dir.state_file();
    std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    let durable = br#"{"version":1,"playlists":[{"id":7,"name":"A","track_paths":["/a.flac"]},{"id":7,"name":"B","track_paths":["/b.flac"]}]}"#;
    seed_state(&path, durable);

    assert!(
        matches!(load_from(&path), Err(LoadError::Corrupt(_))),
        "a shared playlist id must be refused, not admitted"
    );

    let (doc, origin, authority, _identity) = persistence::acquire(&path);
    assert!(
        doc.is_none() && authority.is_none(),
        "the ambiguous bytes grant neither a document nor an authority"
    );
    assert!(
        matches!(origin, Origin::RefusedReadOnly(RefusalKind::Corrupt)),
        "a shared playlist id is refused read-only"
    );
    assert_eq!(
        std::fs::read(&path).expect("after boot"),
        durable,
        "boot leaves the ambiguous bytes untouched"
    );
    assert!(
        recovery_sibling(&path, durable).is_none(),
        "boot creates no recovery sibling for the ambiguous bytes"
    );

    let mut app = app_at(&path);
    assert!(
        app.library.playlists.is_empty(),
        "neither ambiguous owner is adopted into the library"
    );
    let _ = app.update_inner(Message::RenamePlaylistOpen(7));
    assert!(
        app.playlist_rename.is_none(),
        "no owner can be opened for rename"
    );
    let _ = app.update_inner(Message::DeletePlaylist(7));
    assert!(app.library.playlists.is_empty(), "no owner can be deleted");
}

#[test]
fn distinct_playlist_ids_load_and_publish_as_a_positive_control() {
    let dir = TempDir::new("distinct-id");
    let path = dir.state_file();
    std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    let durable = br#"{"version":1,"playlists":[{"id":1,"name":"A","track_paths":[]},{"id":2,"name":"B","track_paths":[]}]}"#;
    seed_state(&path, durable);

    let doc = load_from(&path).expect("distinct ids load");
    assert_eq!(doc.playlists.len(), 2);
    assert!(
        !path.with_extension("json.corrupt").exists(),
        "distinct ids are not quarantined"
    );

    let app = app_at(&path);
    assert_eq!(
        app.library.playlists.len(),
        2,
        "both distinct owners are adopted"
    );

    let republished = path.with_file_name("republished.json");
    save_to(&republished, &doc).expect("a distinct-id snapshot publishes");
    let reloaded = load_from(&republished).expect("the published snapshot loads back");
    assert_eq!(
        reloaded.playlists.iter().map(|p| p.id).collect::<Vec<_>>(),
        vec![1, 2],
        "distinct ids survive publication and reload"
    );
    assert_eq!(
        std::fs::read(&path).expect("input fixture"),
        durable,
        "the positive control leaves its input fixture untouched"
    );
}
