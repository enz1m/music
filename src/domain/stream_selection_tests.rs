use super::*;
use crate::podcasts::{FeedStatus, PodcastEpisode, PodcastFeed};
use crate::radio::RadioState;

fn one_track_library() -> Library {
    let mut lib = Library::new();
    lib.add_track(super::load_current_track_tests::track_with_duration(123.0));
    lib.reindex();
    lib
}

fn feed() -> PodcastFeed {
    PodcastFeed {
        feed_url: "https://example.com/show.rss".into(),
        title: "Example Show".into(),
        description: String::new(),
        episodes: vec![PodcastEpisode {
            guid: "ep-1".into(),
            title: "The First Episode".into(),
            audio_url: Some("https://x/1.mp3".into()),
            ..Default::default()
        }],
        status: FeedStatus::Loaded,
        ..Default::default()
    }
}

#[test]
fn a_stream_and_a_track_are_mutually_exclusive_in_both_directions() {
    let lib = one_track_library();
    let track_id = *lib.track_order.first().expect("one track");

    let mut pb = PlaybackState::default();
    pb.play_track(track_id, &lib);
    assert_eq!(pb.current_track_id, Some(track_id));
    assert_eq!(pb.current_stream, None, "a track alone at rest");

    pb.tune_station(7);
    assert_eq!(pb.current_stream, Some(StreamSelection::Station(7)));
    assert_eq!(
        pb.current_track_id, None,
        "tuning must drop the track, or two surfaces name two things"
    );
    assert_eq!(
        pb.duration_secs, 0.0,
        "a station has no scrubbable duration"
    );
    assert!(
        pb.playing,
        "the tuned station is what the app is pointed at"
    );

    pb.load_current_track(track_id, &lib);
    assert_eq!(
        pb.current_stream, None,
        "loading a track must drop the stream"
    );
    assert_eq!(pb.current_track_id, Some(track_id));
}

#[test]
fn a_loaded_track_wins_over_a_stale_stream_selection() {
    let mut pb = PlaybackState::default();
    pb.select_episode("https://example.com/show.rss".into(), "ep-1".into());
    assert!(
        pb.stream_selection().is_some(),
        "control: the stream is current"
    );
    // Hand-assigned, as a path that ignored the invariant would.
    pb.current_track_id = Some(0);
    assert!(
        pb.stream_selection().is_none(),
        "a loaded track must win the precedence rule"
    );
}

#[test]
fn resolve_stream_names_the_catalog_entry_or_nothing() {
    // `RadioState::default()` has an empty catalog.
    let mut radio = RadioState::default();
    let seed = crate::radio::seed_test_catalog();
    radio.lists.insert(("test".into(), None), seed.clone());
    let subs = vec![feed()];
    let station = seed[1].clone();

    let card = resolve_stream(&StreamSelection::Station(station.id), &radio, &subs)
        .expect("a catalog station resolves");
    assert_eq!(card.title, station.name);
    assert_eq!(card.subtitle, station.description);
    assert_eq!(card.detail, station.tags);

    let ep = resolve_stream(
        &StreamSelection::Episode {
            feed_url: "https://example.com/show.rss".into(),
            guid: "ep-1".into(),
        },
        &radio,
        &subs,
    )
    .expect("a subscribed episode resolves");
    assert_eq!(ep.title, "The First Episode");
    assert_eq!(ep.subtitle, "Example Show");
    assert_eq!(ep.detail, PODCAST_EPISODE_DETAIL);

    // Negative controls: ids that name nothing.
    assert!(resolve_stream(&StreamSelection::Station(9_999_999), &radio, &subs).is_none());
    assert!(
        resolve_stream(
            &StreamSelection::Episode {
                feed_url: "https://example.com/show.rss".into(),
                guid: "gone".into(),
            },
            &radio,
            &subs,
        )
        .is_none()
    );
    assert!(
        resolve_stream(
            &StreamSelection::Episode {
                feed_url: "https://unsubscribed.example/x.rss".into(),
                guid: "ep-1".into(),
            },
            &radio,
            &subs,
        )
        .is_none()
    );
}
