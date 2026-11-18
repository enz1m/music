//! Opening a podcast: a preview for a show not subscribed, its episodes
//! playable, and subscribing from it keeping what was fetched.

use super::*;

const FEED: &str = "https://example.com/show.rss";

fn fetched() -> podcasts::PodcastFeed {
    podcasts::PodcastFeed {
        feed_url: FEED.into(),
        title: "The Show".into(),
        episodes: vec![podcasts::PodcastEpisode {
            guid: "ep-1".into(),
            title: "Episode one".into(),
            audio_url: Some("https://example.com/1.mp3".into()),
            ..podcasts::PodcastEpisode::default()
        }],
        ..podcasts::PodcastFeed::default()
    }
}

fn answer(app: &mut MusicApp) {
    let generation = app
        .podcast_requests
        .generation_of(FEED)
        .expect("a fetch in flight");
    app.apply_podcast_completion(podcasts::FetchResult {
        url: FEED.into(),
        generation,
        result: Ok(fetched()),
    });
}

#[test]
fn an_unsubscribed_podcast_opens_as_a_preview_with_its_episodes() {
    let mut app = MusicApp::boot_with_demo().0;
    let _ = app.update(Message::PodcastOpen(FEED.into()));
    assert_eq!(app.podcast_charts.open.as_deref(), Some(FEED));
    assert!(
        app.subscriptions.iter().all(|f| f.feed_url != FEED),
        "opening doesn't subscribe"
    );
    answer(&mut app);
    let feed = app
        .podcast_charts
        .opened_feed(&app.subscriptions)
        .expect("the page has its feed");
    assert_eq!(feed.status, podcasts::FeedStatus::Loaded);
    assert_eq!(feed.playable_count(), 1);
    // Its episode resolves for the player.
    let selection = StreamSelection::Episode {
        feed_url: FEED.into(),
        guid: "ep-1".into(),
    };
    assert_eq!(
        app.resolve_stream(&selection).map(|s| s.title),
        Some("Episode one".into())
    );
}

#[test]
fn subscribing_from_a_preview_keeps_its_episodes_and_back_shows_the_list() {
    let mut app = MusicApp::boot_with_demo().0;
    let _ = app.update(Message::PodcastOpen(FEED.into()));
    answer(&mut app);
    let _ = app.update(Message::PodcastSubscribeFromResult(FEED.into()));
    let subscribed = app
        .subscriptions
        .iter()
        .find(|f| f.feed_url == FEED)
        .expect("subscribed");
    assert_eq!(subscribed.playable_count(), 1);
    let _ = app.update(Message::PodcastClose);
    assert!(app.podcast_charts.opened_feed(&app.subscriptions).is_none());
}

#[test]
fn the_player_keeps_an_episode_playing_from_a_preview_when_another_podcast_opens() {
    const OTHER: &str = "https://example.com/other.rss";
    let mut app = MusicApp::boot_with_demo().0;
    let _ = app.update(Message::PodcastOpen(FEED.into()));
    answer(&mut app);
    let _ = app.update(Message::PodcastEpisodeSelect(FEED.into(), "ep-1".into()));
    // Another podcast, not subscribed either, replaces the preview.
    let _ = app.update(Message::PodcastOpen(OTHER.into()));
    assert_eq!(
        app.podcast_charts
            .preview
            .as_ref()
            .map(|p| p.feed_url.as_str()),
        Some(OTHER)
    );
    let selection = StreamSelection::Episode {
        feed_url: FEED.into(),
        guid: "ep-1".into(),
    };
    let playing = app
        .resolve_stream(&selection)
        .expect("the player still has the episode");
    assert_eq!(playing.title, "Episode one");
    assert_eq!(playing.subtitle, "The Show");
}

#[test]
fn an_episode_whose_podcast_is_gone_still_gets_a_player_card() {
    let app = MusicApp::boot_with_demo().0;
    let selection = StreamSelection::Episode {
        feed_url: "https://gone.example.com/feed.rss".into(),
        guid: "x".into(),
    };
    let card = app
        .resolve_stream(&selection)
        .expect("a card with controls");
    assert_eq!(card.subtitle, "gone.example.com");
}
