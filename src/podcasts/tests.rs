use super::*;

const FIXTURE_RSS: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<rss version="2.0">
  <channel>
    <title>Test Show</title>
    <description>A test podcast.</description>
    <item>
      <title>Episode 1</title>
      <guid>ep-1</guid>
      <enclosure url="https://example.com/ep1.mp3" length="12345" type="audio/mpeg"/>
    </item>
    <item>
      <title>Episode 2 (no audio)</title>
      <guid>ep-2</guid>
    </item>
    <item>
      <title>Episode 3</title>
      <guid>ep-3</guid>
      <enclosure url="https://example.com/ep3.mp3" type="audio/mpeg"/>
    </item>
  </channel>
</rss>"#;

#[test]
fn parse_rss_fixture_yields_three_items_two_playable() {
    let feed = parse_rss_feed("https://example.com/rss", FIXTURE_RSS).unwrap();
    assert_eq!(feed.feed_url, "https://example.com/rss");
    assert_eq!(feed.title, "Test Show");
    assert_eq!(feed.description, "A test podcast.");
    assert_eq!(feed.episodes.len(), 3);

    assert_eq!(feed.playable_count(), 2);
    let urls: Vec<_> = feed
        .playable_episodes()
        .map(|e| e.audio_url.as_deref().unwrap())
        .collect();
    assert_eq!(
        urls,
        vec!["https://example.com/ep1.mp3", "https://example.com/ep3.mp3"]
    );
    assert!(feed.episodes[1].audio_url.is_none());
}

#[test]
fn opml_round_trip_preserves_url_and_carries_absolute_anchor() {
    let feeds = vec![
        PodcastFeed {
            feed_url: "https://example.com/reply-all".to_string(),
            title: "Reply All".to_string(),
            description: String::new(),
            episodes: Vec::new(),
            status: FeedStatus::Loaded,
            ..Default::default()
        },
        PodcastFeed {
            feed_url: "https://bbc.co.uk/iot.rss".to_string(),
            title: "In Our Time".to_string(),
            description: String::new(),
            episodes: Vec::new(),
            status: FeedStatus::Loaded,
            ..Default::default()
        },
    ];
    let opml = export_opml(&feeds);

    assert_eq!(
        opml.matches(r#"xmlUrl="https://example.com/reply-all""#)
            .count(),
        1,
        "exported OPML must carry xmlUrl= as a literal attribute"
    );
    assert_eq!(
        opml.matches(r#"xmlUrl="https://bbc.co.uk/iot.rss""#)
            .count(),
        1
    );
    assert!(opml.contains(r#"text="Reply All""#));
    assert!(opml.contains(r#"text="In Our Time""#));

    let parsed = import_opml(&opml);
    assert_eq!(parsed.len(), 2);
    assert_eq!(
        parsed[0],
        (
            "Reply All".to_string(),
            "https://example.com/reply-all".to_string()
        )
    );
    assert_eq!(
        parsed[1],
        (
            "In Our Time".to_string(),
            "https://bbc.co.uk/iot.rss".to_string()
        )
    );
}

#[derive(Clone)]
struct FixtureTransport {
    calls: Arc<std::sync::atomic::AtomicUsize>,
    status: u16,
    content_type: &'static str,
    body: Vec<u8>,
}

impl Transport for FixtureTransport {
    fn fetch(
        &self,
        request: &RemoteRequest,
        _policy: &UrlPolicy,
        limits: &TransportLimits,
        cancel: &CancelSignal,
        _progress: &mut dyn FnMut(enzui_resources::TransferProgress),
    ) -> Result<TransportResponse, TransportError> {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        assert_eq!(limits, &FEED_LIMITS);
        if cancel.is_cancelled() {
            return Err(TransportError::Cancelled(CancelCause::Cancelled));
        }
        Ok(TransportResponse {
            status: self.status,
            body: self.body.clone(),
            final_url: request.safe_url().clone(),
            peer_class: enzui_core::url_policy::AddressClass::Public,
            headers: vec![("content-type".into(), self.content_type.into())],
        })
    }
}

fn request(url: &str) -> FetchRequest {
    FetchRequest {
        url: url.to_string(),
        generation: 1,
        cancel: CancelSignal::new(),
    }
}

#[test]
fn atom_feed_parses_namespaces_entities_and_audio_enclosure() {
    let atom = br#"<?xml version="1.0"?><feed xmlns="http://www.w3.org/2005/Atom">
<title type="text">A &amp; B</title><subtitle>Generated fixture</subtitle>
<entry><id>tag:fixture,1</id><title>Episode One</title>
<link rel="enclosure" type="audio/ogg" href="https://cdn.example/one.ogg"/></entry>
</feed>"#;
    let feed = parse_feed("https://feeds.example/atom", atom).unwrap();
    assert_eq!(feed.title, "A & B");
    assert_eq!(feed.description, "Generated fixture");
    assert_eq!(feed.playable_count(), 1);
    assert_eq!(feed.episodes[0].guid, "tag:fixture,1");
    assert_eq!(
        feed.episodes[0].audio_url.as_deref(),
        Some("https://cdn.example/one.ogg")
    );
}

#[test]
fn unsafe_urls_are_refused_before_the_injected_transport_moves_bytes() {
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let transport = FixtureTransport {
        calls: calls.clone(),
        status: 200,
        content_type: "application/rss+xml",
        body: FIXTURE_RSS.as_bytes().to_vec(),
    };
    for url in [
        "http://example.com/feed",
        "https://user:secret@example.com/feed",
        "filex://example.com/feed",
    ] {
        let result = fetch_with_transport(request(url), &transport, REMOTE_POLICY);
        assert!(result.result.is_err(), "{url} must be refused");
    }
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
}

#[test]
fn response_status_type_size_and_error_text_are_bounded() {
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let base = |status, content_type, body: Vec<u8>| FixtureTransport {
        calls: calls.clone(),
        status,
        content_type,
        body,
    };
    let bad_status = fetch_with_transport(
        request("https://example.com/status"),
        &base(503, "application/rss+xml", FIXTURE_RSS.as_bytes().to_vec()),
        REMOTE_POLICY,
    );
    assert!(bad_status.result.unwrap_err().contains("HTTP 503"));

    let binary = fetch_with_transport(
        request("https://example.com/binary"),
        &base(
            200,
            "application/octet-stream",
            FIXTURE_RSS.as_bytes().to_vec(),
        ),
        REMOTE_POLICY,
    );
    assert!(
        binary
            .result
            .unwrap_err()
            .contains("not an RSS or Atom feed")
    );

    let huge = fetch_with_transport(
        request("https://example.com/huge"),
        &base(
            200,
            "application/rss+xml",
            vec![b'x'; MAX_FEED_BYTES as usize + 1],
        ),
        REMOTE_POLICY,
    );
    assert!(huge.result.unwrap_err().contains("larger than"));
    assert!(actionable_error("x".repeat(1000)).chars().count() <= MAX_ERROR_CHARS);
}

#[test]
fn retry_and_unsubscribe_cancel_old_generations() {
    let mut requests = PodcastRequests::default();
    let first = requests.begin("https://example.com/feed".into());
    let second = requests.begin("https://example.com/feed".into());
    assert!(
        first.cancel.is_cancelled(),
        "retry must cancel the old transfer"
    );
    assert!(!second.cancel.is_cancelled());

    let stale = FetchResult {
        url: first.url,
        generation: first.generation,
        result: Err("stale".into()),
    };
    assert!(!requests.accept(&stale));
    requests.cancel(&second.url, CancelCause::Cancelled);
    assert!(
        second.cancel.is_cancelled(),
        "unsubscribe must cancel transfer"
    );

    let shutdown = requests.begin("https://example.com/shutdown".into());
    let shutdown_signal = shutdown.cancel.clone();
    drop(requests);
    assert_eq!(
        shutdown_signal.cause(),
        Some(CancelCause::Shutdown),
        "dropping the app-owned registry must stop managed transfers"
    );
}

#[test]
fn pool_caps_active_imports_and_blocking_concurrency() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    struct BlockingCountTransport {
        active: Arc<AtomicUsize>,
        peak: Arc<AtomicUsize>,
    }
    impl Transport for BlockingCountTransport {
        fn fetch(
            &self,
            _request: &RemoteRequest,
            _policy: &UrlPolicy,
            _limits: &TransportLimits,
            cancel: &CancelSignal,
            _progress: &mut dyn FnMut(enzui_resources::TransferProgress),
        ) -> Result<TransportResponse, TransportError> {
            let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
            self.peak.fetch_max(active, Ordering::SeqCst);
            while !cancel.is_cancelled() {
                std::thread::sleep(Duration::from_millis(2));
            }
            self.active.fetch_sub(1, Ordering::SeqCst);
            Err(TransportError::Cancelled(CancelCause::Shutdown))
        }
    }

    let active = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    let mut service = PodcastService::new(
        Arc::new(BlockingCountTransport {
            active: active.clone(),
            peak: peak.clone(),
        }),
        REMOTE_POLICY,
        PODCAST_WORKERS,
        PODCAST_QUEUE_CAPACITY,
    )
    .unwrap();
    for n in 0..MAX_ACTIVE_FETCHES {
        service
            .submit(request(&format!("https://example.com/{n}.rss")))
            .unwrap();
    }
    let refused = service
        .submit(request("https://example.com/overflow.rss"))
        .unwrap_err();
    assert!(refused.contains("Too many podcast feeds"));

    let deadline = Instant::now() + Duration::from_secs(1);
    while active.load(Ordering::SeqCst) < PODCAST_WORKERS && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(active.load(Ordering::SeqCst), PODCAST_WORKERS);
    assert_eq!(peak.load(Ordering::SeqCst), PODCAST_WORKERS);
    service.shutdown();
}

struct CancelAwareTransport;
impl Transport for CancelAwareTransport {
    fn fetch(
        &self,
        _request: &RemoteRequest,
        _policy: &UrlPolicy,
        _limits: &TransportLimits,
        cancel: &CancelSignal,
        _progress: &mut dyn FnMut(enzui_resources::TransferProgress),
    ) -> Result<TransportResponse, TransportError> {
        while !cancel.is_cancelled() {
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        Err(TransportError::Cancelled(
            cancel.cause().unwrap_or(CancelCause::Shutdown),
        ))
    }
}

#[test]
fn reaper_spawn_failure_keeps_handles_then_retires_on_close_wake() {
    use std::time::{Duration, Instant};

    let mut service =
        PodcastService::new(Arc::new(CancelAwareTransport), REMOTE_POLICY, 1, 1).unwrap();
    service
        .submit(request("https://example.com/spawn-fail.rss"))
        .unwrap();
    assert_eq!(service.close_debt().accepted, 1);

    service.shutdown_with_failed_reaper_spawn();
    let debt = service.close_debt();
    assert!(
        debt.reaper_pending,
        "a failed reaper spawn leaves the join owed"
    );
    assert!(
        !debt.is_settled(),
        "close cannot settle while the reaper is owed"
    );
    assert_eq!(
        service.workers.lock().unwrap().len(),
        1,
        "the worker handle is retained, not detached, on spawn failure"
    );
    assert!(service.reaper.is_none() && service.reaper_owed);

    let deadline = Instant::now() + Duration::from_secs(2);
    let mut debt = service.close_debt();
    while !debt.is_settled() && Instant::now() < deadline {
        let _ = service.drain();
        debt = service.shutdown_progress();
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(
        debt.is_settled(),
        "the retry helper joins the worker and the reaper"
    );
    assert_eq!(debt.accepted, 0);
    assert_eq!(debt.unjoined_workers, 0);
    assert!(service.reaper_joined && service.reaper.is_none());
}

#[test]
fn held_transport_vetoes_close_until_worker_and_reaper_join() {
    use std::sync::Condvar;
    use std::time::{Duration, Instant};

    #[derive(Default)]
    struct HeldState {
        started: bool,
        released: bool,
    }
    struct HeldTransport(Arc<(Mutex<HeldState>, Condvar)>);
    impl Transport for HeldTransport {
        fn fetch(
            &self,
            _request: &RemoteRequest,
            _policy: &UrlPolicy,
            _limits: &TransportLimits,
            cancel: &CancelSignal,
            _progress: &mut dyn FnMut(enzui_resources::TransferProgress),
        ) -> Result<TransportResponse, TransportError> {
            let (lock, wake) = &*self.0;
            let mut state = lock.lock().unwrap();
            state.started = true;
            wake.notify_all();
            while !state.released {
                state = wake.wait(state).unwrap();
            }
            Err(TransportError::Cancelled(
                cancel.cause().unwrap_or(CancelCause::Shutdown),
            ))
        }
    }

    let barrier = Arc::new((Mutex::new(HeldState::default()), Condvar::new()));
    let mut service = PodcastService::new(
        Arc::new(HeldTransport(barrier.clone())),
        REMOTE_POLICY,
        1,
        1,
    )
    .unwrap();
    service
        .submit(request("https://example.com/held.rss"))
        .unwrap();

    let (lock, wake) = &*barrier;
    let (guard, timeout) = wake
        .wait_timeout_while(lock.lock().unwrap(), Duration::from_secs(1), |s| !s.started)
        .unwrap();
    assert!(
        !timeout.timed_out() && guard.started,
        "worker never entered the transport"
    );
    drop(guard);

    let before = Instant::now();
    service.shutdown();
    assert!(
        before.elapsed() < Duration::from_millis(200),
        "shutdown blocked on the held worker for {:?}",
        before.elapsed()
    );
    assert_eq!(
        service.pending_count(),
        0,
        "shutdown clears the UI active rows"
    );
    let debt = service.close_debt();
    assert_eq!(
        debt.accepted, 1,
        "the held worker still owes its completion"
    );
    assert_eq!(debt.unjoined_workers, 1);
    assert!(
        !debt.is_settled(),
        "close stays vetoed while the transport is held"
    );
    assert!(
        service.reaper.is_some(),
        "one reaper owns the worker handle"
    );

    service.shutdown();
    assert_eq!(service.close_debt().unjoined_workers, 1);
    assert!(
        service.reaper.is_some(),
        "a repeated close starts no parallel reaper"
    );
    assert!(!service.shutdown_progress().is_settled());

    lock.lock().unwrap().released = true;
    wake.notify_all();
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut debt = service.close_debt();
    while !debt.is_settled() && Instant::now() < deadline {
        let _ = service.drain();
        debt = service.shutdown_progress();
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(
        debt.is_settled(),
        "the released worker and reaper both join, authorizing one close"
    );
    assert!(service.reaper.is_none() && service.reaper_joined);
}

#[test]
fn curl_transport_fetches_a_deterministic_local_http_feed() {
    use std::io::{Read as _, Write as _};
    use std::net::TcpListener;
    use std::time::{Duration, Instant};

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let address = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    stream
                        .set_read_timeout(Some(Duration::from_secs(1)))
                        .unwrap();
                    let mut request = [0_u8; 2048];
                    let _ = stream.read(&mut request);
                    let body = FIXTURE_RSS.as_bytes();
                    write!(
                        stream,
                        "HTTP/1.1 200 OK\r\nContent-Type: application/rss+xml\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    ).unwrap();
                    stream.write_all(body).unwrap();
                    return;
                }
                Err(error)
                    if error.kind() == std::io::ErrorKind::WouldBlock
                        && Instant::now() < deadline =>
                {
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(error) => panic!("local feed fixture failed: {error}"),
            }
        }
    });
    let policy = UrlPolicy {
        allow_http: true,
        allow_private_network: true,
        max_redirects: 1,
    };
    // Shipping uses REMOTE_POLICY with no environment downgrade.
    let service = PodcastService::new(
        Arc::new(enzui_resources::curl_transport::CurlTransport::new()),
        policy,
        1,
        1,
    )
    .unwrap();
    service
        .submit(request(&format!("http://{address}/feed.xml")))
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    let result = loop {
        if let Some(result) = service.drain().into_iter().next() {
            break result;
        }
        assert!(Instant::now() < deadline, "bounded worker did not complete");
        std::thread::sleep(Duration::from_millis(5));
    };
    server.join().unwrap();
    let feed = result.result.unwrap();
    assert_eq!(feed.title, "Test Show");
    assert_eq!(feed.playable_count(), 2);
}
