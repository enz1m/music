use super::*;
use std::io::{Read, Write};
use std::net::TcpListener;

const TRACK: &str = "spotify:track:0123456789ABCDEFGHIJKL";
const ALBUM: &str = "spotify:album:0123456789ABCDEFGHIJKL";
const ARTIST: &str = "spotify:artist:0123456789ABCDEFGHIJKL";
const PLAYLIST: &str = "spotify:playlist:abcdefghijklmnopqrstuv";

fn session() -> Arc<Mutex<Session>> {
    Arc::new(Mutex::new(Session {
        client: ClientConfig::new(AUTHORIZE, TOKEN, "fixture-client").unwrap(),
        client_id: "fixture-client".into(),
        token: TokenResponse::parse(r#"{"access_token":"fixture-access","token_type":"Bearer","refresh_token":"fixture-refresh","expires_in":3600}"#).unwrap(),
        expires: Instant::now() + Duration::from_secs(3600),
    }))
}

fn track_json(uri: &str) -> Value {
    json!({
        "uri": uri, "name": "Song", "duration_ms": 185000, "track_number": 3,
        "artists": [{"uri": ARTIST, "name": "Azahriah"}],
        "album": {"uri": ALBUM, "name": "Album", "images": [
            {"url": "https://i.scdn.co/640", "width": 640},
            {"url": "https://i.scdn.co/300", "width": 300},
            {"url": "https://i.scdn.co/64", "width": 64}
        ]}
    })
}

#[test]
fn pagination_rejects_foreign_hosts_and_keeps_spotify_paths() {
    for next in [
        "https://attacker.test/v1/search?x",
        "https://api.spotify.com.evil/v1/x",
        "https://api.spotify.com/v1//evil",
    ] {
        assert_eq!(model::next_path(&json!({ "next": next })), None, "{next}");
    }
    assert_eq!(
        model::next_path(
            &json!({"next": "https://api.spotify.com/v1/me/tracks?offset=50&limit=50"})
        )
        .as_deref(),
        Some("/me/tracks?offset=50&limit=50")
    );
}

#[test]
fn tracks_read_artists_album_and_sized_art() {
    let track = model::track(&track_json(TRACK), None).unwrap();
    assert_eq!(track.artist_names(), "Azahriah");
    assert_eq!(track.album.as_ref().unwrap().uri, ALBUM);
    // The thumbnail nearest 128 px, preferring larger.
    assert_eq!(track.thumb.as_deref(), Some("https://i.scdn.co/300"));
    assert_eq!(model::clock(track.duration_ms), "3:05");
    let playing = model::playing(
        &json!({"item": track_json(TRACK), "is_playing": true, "progress_ms": 1000, "repeat_state": "context"}),
    );
    assert_eq!(playing.cover.as_deref(), Some("https://i.scdn.co/640"));
}

#[test]
fn playlist_items_unwrap_the_february_2026_shape() {
    let page = model::page(
        &json!({"items": [{"item": track_json(TRACK)}, {"track": track_json(TRACK)}, {"item": {"uri": "spotify:episode:x"}}], "total": 3}),
        |v| model::track(v, None),
    )
    .unwrap();
    assert_eq!(page.items.len(), 2);
    let playlist = model::playlist(&json!({
        "uri": PLAYLIST, "name": "Mix", "owner": {"id": "me", "display_name": "Me"},
        "items": {"total": 12}, "description": "Best <a href=\"x\">of</a> &amp; more"
    }))
    .unwrap();
    assert_eq!(playlist.total, 12);
    assert_eq!(playlist.description, "Best of & more");
}

#[test]
fn followed_artists_and_saved_albums_parse_with_thumbnails() {
    let artists = json!({"artists": {"items": [{"uri": ARTIST, "name": "Azahriah", "images": [{"url": "https://i.scdn.co/a", "width": 320}]}], "next": null}});
    let page = model::page(&artists["artists"], model::artist).unwrap();
    assert_eq!(page.items[0].image.as_deref(), Some("https://i.scdn.co/a"));
    let albums = json!({"items": [{"album": {"uri": ALBUM, "name": "Album", "artists": [{"uri": ARTIST, "name": "Azahriah"}], "images": [{"url": "http://insecure/x"}], "release_date": "2023-05-01", "album_type": "album"}}]});
    let page = model::page(&albums, model::album).unwrap();
    assert_eq!(page.items[0].year, "2023");
    // Only https artwork is fetched.
    assert_eq!(page.items[0].image, None);
}

#[test]
fn a_song_in_a_list_plays_in_that_list_from_where_it_was_clicked() {
    let tracks: Vec<Track> = (0..30)
        .map(|i| model::track(&track_json(&format!("spotify:track:{i:0>22}")), None).unwrap())
        .collect();
    let body = uris_from(&tracks, 25).unwrap();
    let uris = body["uris"].as_array().unwrap();
    let offset = body["offset"]["position"].as_u64().unwrap() as usize;
    assert_eq!(uris[offset], json!(tracks[25].uri));
    assert!(uris_from(&tracks, 30).is_none());
}

#[test]
fn an_album_track_plays_the_album_from_that_track() {
    let mut state = State::default();
    state.session = Some(session());
    state.devices = vec![Device {
        id: "d1".into(),
        name: "Phone".into(),
        kind: "Smartphone".into(),
        active: true,
        volume: Some(50),
        supports_volume: true,
    }];
    let album = model::album(&json!({"uri": ALBUM, "name": "Album"})).unwrap();
    let track = model::track(&track_json(TRACK), Some(&album)).unwrap();
    state.stack.push(Screen {
        serial: 1,
        route: Route::Album(ALBUM.into()),
        content: Content::Album(Box::new(AlbumPage {
            album,
            tracks: Page {
                items: vec![track],
                next: None,
                total: 1,
            },
            label_date: String::new(),
        })),
        loading_more: false,
        scroll_y: 0.0,
    });
    let _ = state.update(Event::PlayTrack(List::Main, 0));
    assert!(state.claimed, "Spotify becomes the player's source");
    assert_eq!(state.current_track_uri(), Some(TRACK));
}

#[test]
fn playing_with_no_device_says_so_instead_of_sending() {
    let mut state = State::default();
    state.session = Some(session());
    let _ = state.update(Event::PlayUri(TRACK.into()));
    assert_eq!(
        state.take_notices(),
        vec![Failure::NoDevice.text().to_owned()]
    );
    assert!(!state.claimed);
}

#[test]
fn the_player_shows_spotify_only_once_claimed() {
    let mut state = State::default();
    state.session = Some(session());
    state.playing = Some(model::playing(
        &json!({"item": track_json(TRACK), "is_playing": false, "progress_ms": 5000}),
    ));
    assert!(state.remote().is_none());
    state.claimed = true;
    let remote = state.remote().unwrap();
    assert!((remote.position - 5.0).abs() < 0.01);
    assert_eq!(remote.repeat, Repeat::Off);
    let _ = state.release();
    assert!(state.remote().is_none());
}

#[test]
fn invalid_client_id_does_not_open_a_listener() {
    let mut state = State::default();
    state.client_id = "short".into();
    let _ = state.update(Event::Connect);
    assert!(!state.busy);
    assert_eq!(state.status.as_deref(), Some(Failure::ClientId.text()));
}

fn serve(status: u16, headers: &str, body: &str) -> (String, std::thread::JoinHandle<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/request", listener.local_addr().unwrap());
    let response = format!(
        "HTTP/1.1 {status} Test\r\nConnection: close\r\nContent-Length: {}\r\n{headers}\r\n{body}",
        body.len()
    );
    let worker = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let mut bytes = Vec::new();
        let mut buf = [0u8; 4096];
        loop {
            let count = stream.read(&mut buf).unwrap();
            if count == 0 {
                break;
            }
            bytes.extend_from_slice(&buf[..count]);
            if let Some(end) = bytes.windows(4).position(|v| v == b"\r\n\r\n") {
                let head = String::from_utf8_lossy(&bytes[..end]);
                let length = head
                    .lines()
                    .find_map(|l| {
                        l.split_once(':')
                            .filter(|(n, _)| n.eq_ignore_ascii_case("content-length"))
                            .and_then(|(_, v)| v.trim().parse::<usize>().ok())
                    })
                    .unwrap_or(0);
                if bytes.len() >= end + 4 + length {
                    break;
                }
            }
        }
        stream.write_all(response.as_bytes()).unwrap();
        String::from_utf8(bytes).unwrap()
    });
    (url, worker)
}

#[test]
fn native_http_sends_bearer_and_json_body_and_accepts_empty_success() {
    let (url, server) = serve(204, "", "");
    let cancel = AtomicBool::new(false);
    let token = SecretText::new("fixture-access".into());
    let body = br#"{"uris":["spotify:track:0123456789ABCDEFGHIJKL"]}"#;
    assert!(
        http(
            "PUT",
            &url,
            Some(&token),
            Some(("application/json", body)),
            &cancel
        )
        .unwrap()
        .is_empty()
    );
    let request = server.join().unwrap();
    assert!(request.starts_with("PUT /request HTTP/1.1\r\n"));
    assert!(request.contains("Authorization: Bearer fixture-access\r\n"));
    assert!(request.ends_with(std::str::from_utf8(body).unwrap()));
}

#[test]
fn search_query_cannot_inject_another_parameter() {
    assert_eq!(encode("A&B /é"), "A%26B%20%2F%C3%A9");
    assert!(spotify_id("spotify:track:../../secret", "track").is_none());
}

#[test]
fn public_message_debug_redacts_tokens() {
    let event = Event::Authed(Ok(Auth::Connected(session())));
    let debug = format!("{event:?}");
    assert!(!debug.contains("fixture-access"));
    assert!(!debug.contains("fixture-refresh"));
}

#[test]
fn refresh_exchange_uses_the_registered_public_client_without_a_secret() {
    let (endpoint, server) = serve(
        200,
        "Content-Type: application/json\r\n",
        r#"{"access_token":"new-access","token_type":"Bearer","expires_in":3600}"#,
    );
    let config = ClientConfig::new(AUTHORIZE, endpoint, "fixture-client").unwrap();
    let request = config.refresh_request(&SecretText::new("fixture-refresh".into()), &[]);
    let token = exchange(&request, &AtomicBool::new(false)).unwrap();
    assert_eq!(token.access_token.expose_secret(), "new-access");
    let request = server.join().unwrap();
    assert!(request.starts_with("POST /request HTTP/1.1\r\n"));
    let body = request.split_once("\r\n\r\n").unwrap().1;
    assert!(body.contains("grant_type=refresh_token"));
    assert!(body.contains("client_id=fixture-client"));
    assert!(body.contains("refresh_token=fixture-refresh"));
    assert!(!body.contains("client_secret"));
}

#[test]
fn a_tab_shows_its_cached_page_and_only_a_change_replaces_it() {
    let mut state = State::default();
    state.session = Some(session());
    let track = model::track(&track_json(TRACK), None).unwrap();
    let page = Content::Tracks(Page {
        items: vec![track.clone()],
        next: None,
        total: 1,
    });
    let _ = state.update(Event::Prefetched(Route::Tab(Tab::Recent), Ok(page.clone())));
    let _ = state.update(Event::Tab(Tab::Recent));
    // Shown at once from the cache, not a spinner.
    assert_eq!(state.screen().unwrap().content, page);
    let serial = state.screen().unwrap().serial;
    // A failed refresh keeps the cached page and says so in a notification.
    let _ = state.update(Event::Loaded(serial, Err(Failure::Network)));
    assert_eq!(state.screen().unwrap().content, page);
    assert_eq!(state.take_notices().len(), 1);
    // A changed list replaces it.
    let mut more = track.clone();
    more.uri = "spotify:track:ZZZZZZZZZZZZZZZZZZZZZZ".into();
    let changed = Content::Tracks(Page {
        items: vec![more, track],
        next: None,
        total: 2,
    });
    let _ = state.update(Event::Loaded(serial, Ok(changed.clone())));
    assert_eq!(state.screen().unwrap().content, changed);
}

#[test]
fn synced_lyrics_light_the_line_being_sung() {
    use crate::track_details::LyricLine;
    let mut state = State::default();
    state.session = Some(session());
    state.claimed = true;
    state.playing = Some(model::playing(
        &json!({"item": track_json(TRACK), "is_playing": false, "progress_ms": 5000}),
    ));
    state.lyrics_shown = true;
    let lines = vec![
        LyricLine {
            millis: Some(1000),
            text: "one".into(),
        },
        LyricLine {
            millis: Some(4900),
            text: "two".into(),
        },
        LyricLine {
            millis: Some(9000),
            text: "three".into(),
        },
    ];
    let _ = state.update(Event::LyricsLoaded(
        TRACK.into(),
        Lyrics::Found {
            lines: lines.clone(),
            synced: true,
        },
    ));
    // Not asked for this song yet: ignored.
    assert!(state.remote().unwrap().lyrics.is_none());
    state.lyrics = Some((
        TRACK.into(),
        Lyrics::Found {
            lines,
            synced: true,
        },
    ));
    assert_eq!(state.remote().unwrap().lyrics.unwrap().current, Some(1));
}
