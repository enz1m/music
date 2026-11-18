//! Spotify in Music: the user's library and Spotify's catalog to browse,
//! playback on any Spotify Connect device (this computer through
//! [`soloist`]), and what is playing, shown in the app's own player.
//!
//! Requests run off the UI thread, each kind on its own track: a page load
//! answers the screen that asked (by serial), the player poll and commands
//! answer the player. A token about to expire is refreshed by whichever
//! request needs it first.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use curl::easy::{Easy, List as Headers};
use enzui_services::credential_store::{CredentialKey, CredentialOutcome, Secret, connect_for_app};
use enzui_services::oauth::loopback::{LoopbackCancel, LoopbackRedirect};
use enzui_services::oauth::{ClientConfig, SecretText, TokenRequest, TokenResponse};
use iced::Task;
use serde_json::{Value, json};

mod lyrics;
pub mod model;
pub mod soloist;
#[cfg(test)]
mod tests;
mod view;

pub use model::{Album, Artist, Device, Link, Page, Playing, Playlist, Track};
pub use soloist::DEVICE_NAME;

const AUTHORIZE: &str = "https://accounts.spotify.com/authorize";
const TOKEN: &str = "https://accounts.spotify.com/api/token";
const API: &str = "https://api.spotify.com/v1";
const MAX_REPLY: usize = 1024 * 1024;

/// Where a Spotify developer app is created.
pub const DASHBOARD_URL: &str = "https://developer.spotify.com/dashboard";
/// The port registered for Music's Spotify sign-in callback.
pub const REDIRECT_PORT: u16 = 8932;
const REDIRECT_PATH: &str = "/spotify/callback";
/// The redirect URI to register in the Spotify developer application.
pub const REDIRECT_URI: &str = "http://127.0.0.1:8932/spotify/callback";
/// Suggested values for the app form.
pub const APP_NAME: &str = "Music";
pub const APP_DESCRIPTION: &str = "My music player";
const SCOPES: [&str; 11] = [
    "user-read-playback-state",
    "user-modify-playback-state",
    "user-read-currently-playing",
    "user-read-recently-played",
    "user-library-read",
    "user-library-modify",
    "user-follow-read",
    "user-follow-modify",
    "playlist-read-private",
    "playlist-read-collaborative",
    "user-read-private",
];

/// How often the player is asked what it's doing.
const POLL_PLAYING: Duration = Duration::from_secs(3);
const POLL_IDLE: Duration = Duration::from_secs(15);
/// How soon after a command the player is asked again.
const POLL_AFTER_COMMAND: Duration = Duration::from_millis(600);
/// How often the device list is refreshed.
const DEVICES_EVERY: Duration = Duration::from_secs(20);
/// Search answers at most this many of each kind (development-mode apps).
const SEARCH_LIMIT: u32 = 10;
/// How many URIs one library request carries.
const LIBRARY_BATCH: usize = 40;

/// The library tabs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Tab {
    Liked,
    Playlists,
    Artists,
    Albums,
    Recent,
}

impl Tab {
    pub const ALL: [Tab; 5] = [
        Tab::Liked,
        Tab::Playlists,
        Tab::Artists,
        Tab::Albums,
        Tab::Recent,
    ];

    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Tab::Liked => "Liked Songs",
            Tab::Playlists => "Playlists",
            Tab::Artists => "Artists",
            Tab::Albums => "Albums",
            Tab::Recent => "Recent",
        }
    }
}

/// Where the Spotify page is.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Route {
    Tab(Tab),
    Search(String),
    Artist(String),
    Album(String),
    Playlist(String),
    /// Setting up playback on this computer.
    SetUp,
}

/// What a screen shows.
#[derive(Debug, Clone, Default, PartialEq)]
pub enum Content {
    #[default]
    Loading,
    Failed(String),
    Tracks(Page<Track>),
    Albums(Page<Album>),
    Artists(Page<Artist>),
    Playlists(Page<Playlist>),
    Search(Box<SearchResults>),
    Artist(Box<ArtistPage>),
    Album(Box<AlbumPage>),
    Playlist(Box<PlaylistPage>),
    /// A screen with nothing to load.
    Ready,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct SearchResults {
    pub tracks: Vec<Track>,
    pub artists: Vec<Artist>,
    pub albums: Vec<Album>,
    pub playlists: Vec<Playlist>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ArtistPage {
    pub artist: Artist,
    /// The artist's best-known songs, from search (Spotify no longer lists
    /// top tracks for apps in development mode).
    pub popular: Vec<Track>,
    pub albums: Vec<Album>,
    pub singles: Vec<Album>,
    pub compilations: Vec<Album>,
    pub appears_on: Vec<Album>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AlbumPage {
    pub album: Album,
    pub tracks: Page<Track>,
    pub label_date: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PlaylistPage {
    pub playlist: Playlist,
    pub tracks: Page<Track>,
    /// Spotify lists the songs only of playlists the user owns or
    /// collaborates on; others still play.
    pub restricted: bool,
}

/// One entry of the navigation stack.
#[derive(Debug, Clone)]
pub struct Screen {
    pub serial: u64,
    pub route: Route,
    pub content: Content,
    pub loading_more: bool,
    /// Where the screen was scrolled, to return there with Back.
    pub scroll_y: f32,
}

/// Which list a track row belongs to, to play it in that list's order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum List {
    /// The screen's main list: liked songs, recent, an album, a playlist.
    Main,
    /// An artist's popular songs.
    Popular,
    /// The songs in search results.
    SearchSongs,
}

/// Playback on this computer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Local {
    /// Not looked at yet (or signed out).
    Unknown,
    /// Looking for the program and its key.
    Checking,
    /// Soloist isn't downloaded.
    NotInstalled,
    /// Downloading Soloist from Spotify.
    Installing,
    /// The user hasn't given a Soloist API key yet.
    NeedsKey,
    /// The player is starting.
    Starting,
    /// Running, waiting to be paired from a Spotify app.
    Pairing,
    /// Running and signed in: "Music" plays.
    Ready,
    /// The build is too old for Spotify.
    Expired,
    Failed(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Repeat {
    Off,
    Context,
    Track,
}

#[derive(Debug, Clone)]
pub enum Event {
    // Sign-in.
    ClientId(String),
    Restore,
    Connect,
    Disconnect,
    Authed(Result<Auth, Failure>),
    OpenLink(&'static str),
    Copy(&'static str),
    PasteClientId,
    PastedClientId(Option<String>),
    // Browsing.
    /// The page became visible.
    Shown,
    Tab(Tab),
    Query(String),
    Search,
    ClearSearch,
    Open(Route),
    Back,
    /// The page scrolled to this offset.
    Scrolled(f32),
    More,
    Reload,
    Loaded(u64, Result<Content, Failure>),
    /// A tab loaded ahead of being opened.
    Prefetched(Route, Result<Content, Failure>),
    ToggleLyrics,
    LyricsLoaded(String, Lyrics),
    Appended(u64, Result<Content, Failure>),
    OpenOnSpotify(String),
    // Library.
    ToggleSaved(String),
    SavedKnown(Vec<(String, bool)>),
    // Playback.
    PlayTrack(List, usize),
    PlayUri(String),
    Queue(String),
    TogglePlay,
    Next,
    Previous,
    Seek(f64),
    Volume(f64),
    ToggleMute,
    Shuffle,
    CycleRepeat,
    PickDevice(String),
    ToggleDevices,
    Tick,
    Polled(Result<Box<Poll>, Failure>),
    Commanded(Result<(), Failure>),
    // This computer.
    /// The program, the key, and the build's age in days.
    LocalChecked(Option<PathBuf>, Option<String>, Option<u64>),
    Install,
    Installed(Result<(), String>),
    KeyDraft(String),
    PasteKey,
    PastedKey(Option<String>),
    SaveKey,
    KeySaved(Result<String, String>),
    Soloist(soloist::Report),
    RestartPlayer,
    /// Show the key field again, to replace the saved key.
    ChangeKey,
    /// Change the key from Settings: the setup page, with the key field.
    ChangeKeyAndOpen,
    Dismiss,
}

/// What signing in came to.
#[derive(Debug, Clone)]
pub enum Auth {
    Connected(Arc<Mutex<Session>>),
    Restored(Arc<Mutex<Session>>, String),
    NotRestored,
    /// A saved login from before Music asked for more access.
    Renew(String),
}

/// A song's lyrics, from LRCLIB (Spotify offers apps none).
#[derive(Debug, Clone, PartialEq)]
pub enum Lyrics {
    Loading,
    Found {
        lines: Vec<crate::track_details::LyricLine>,
        /// Whether lines carry times, to follow the song.
        synced: bool,
    },
    Missing,
}

/// The lyrics the player shows.
#[derive(Debug, Clone, Copy)]
pub struct LyricsView<'a> {
    pub lyrics: &'a Lyrics,
    /// The line being sung, for synced lyrics.
    pub current: Option<usize>,
}

/// One answer from the player poll.
#[derive(Debug, Clone)]
pub struct Poll {
    pub playing: Option<Playing>,
    pub devices: Option<Vec<Device>>,
    pub me: Option<model::Me>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failure {
    Cancelled,
    Network,
    InvalidReply,
    SignIn,
    /// Another program holds the sign-in callback port.
    PortInUse,
    ClientId,
    NoDevice,
    NotFound,
    Forbidden,
    Rejected,
    Expired,
    Busy(Duration),
    Quota,
}

impl Failure {
    fn text(&self) -> &'static str {
        match self {
            Self::Cancelled => "Sign-in cancelled.",
            Self::Network => "Could not reach Spotify. Check your connection and try again.",
            Self::InvalidReply => "Spotify returned an unreadable reply. Try again.",
            Self::SignIn => {
                "Sign-in did not complete. Check the registered redirect URI and allowed users, then try again."
            }
            Self::PortInUse => {
                "Another program is using Music’s sign-in port 8932. Close it and try again."
            }
            Self::ClientId => "Enter the 32-character Client ID from your Spotify developer app.",
            Self::NotFound => "This is no longer available on Spotify.",
            Self::NoDevice => {
                "No Spotify device is available. Set up this computer, or open Spotify on another device."
            }
            Self::Forbidden => "Spotify refused this action.",
            Self::Rejected => "Spotify didn't accept this request.",
            Self::Expired => "Your Spotify session has expired. Disconnect and sign in again.",
            Self::Busy(_) => "Spotify is limiting requests. Wait a moment and try again.",
            Self::Quota => "This developer app has reached its Spotify API quota. Try again later.",
        }
    }

    /// The failure in words, with Spotify's own reason when it gave one.
    fn explain(&self) -> String {
        if matches!(self, Self::Forbidden | Self::Rejected)
            && let Some(advice) = refusal_advice()
        {
            return advice;
        }
        self.text().to_owned()
    }
}

#[derive(Debug)]
pub struct Session {
    client: ClientConfig,
    /// The Client ID the session signed in with, saved with its login.
    client_id: String,
    token: TokenResponse,
    expires: Instant,
}

/// What the app's player shows while Spotify is the source.
#[derive(Debug, Clone)]
pub struct Remote<'a> {
    pub track: &'a Track,
    pub cover_key: Option<String>,
    pub position: f64,
    pub duration: f64,
    pub playing: bool,
    pub shuffle: bool,
    pub repeat: Repeat,
    pub volume: Option<f64>,
    pub device: Option<&'a str>,
    pub liked: Option<bool>,
    pub local: bool,
    /// Whether the player shows lyrics in place of the cover.
    pub lyrics_shown: bool,
    /// The lyrics, once known.
    pub lyrics: Option<LyricsView<'a>>,
}

pub struct State {
    pub client_id: String,
    pub query: String,
    pub busy: bool,
    pub status: Option<String>,
    /// Messages for the app's notifications, taken by [`State::take_notices`].
    notices: Vec<String>,
    pub copied: Option<&'static str>,
    pub tab: Tab,
    pub stack: Vec<Screen>,
    /// A saved login needs approving again (Music asks for more access).
    pub renew: bool,
    serial: u64,
    auth_generation: u64,
    session: Option<Arc<Mutex<Session>>>,
    cancelled: Arc<AtomicBool>,
    login_cancel: Option<LoopbackCancel>,
    retry_at: Option<Instant>,
    pub me: Option<model::Me>,
    /// Liked songs, saved albums, followed artists: what Music knows.
    pub saved: HashMap<String, bool>,
    // The player.
    pub playing: Option<Playing>,
    /// The song just asked for, shown as current until Spotify reports it
    /// (or gives up): the list follows the click, not the buffering.
    switching: Option<(String, Instant)>,
    polled_at: Instant,
    poll_in_flight: bool,
    next_poll: Instant,
    devices_at: Option<Instant>,
    pub devices: Vec<Device>,
    pub devices_open: bool,
    /// The device the user picked to play on.
    picked: Option<String>,
    /// Whether Spotify is what the app's player shows and controls.
    claimed: bool,
    muted_from: Option<u8>,
    volume_in_flight: bool,
    volume_wanted: Option<u8>,
    // This computer.
    pub local: Local,
    player: Option<soloist::Player>,
    pub key_draft: String,
    /// Whether a Soloist API key is saved.
    pub has_key: bool,
    /// Pages already loaded, shown at once while a fresh copy loads.
    cache: HashMap<Route, Content>,
    cache_order: std::collections::VecDeque<Route>,
    /// Whether the player shows lyrics in place of the cover.
    pub lyrics_shown: bool,
    /// The current song's lyrics, by track URI.
    lyrics: Option<(String, Lyrics)>,
    /// Whether the page is wide enough for every row action (set by the view).
    wide: std::cell::Cell<bool>,
    paths: Option<soloist::Paths>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            client_id: std::env::var("ENZUI_MUSIC_SPOTIFY_CLIENT_ID").unwrap_or_default(),
            query: String::new(),
            busy: false,
            status: None,
            notices: Vec::new(),
            copied: None,
            tab: Tab::Liked,
            stack: Vec::new(),
            renew: false,
            serial: 0,
            auth_generation: 0,
            session: None,
            cancelled: Arc::new(AtomicBool::new(false)),
            login_cancel: None,
            retry_at: None,
            me: None,
            saved: HashMap::new(),
            playing: None,
            switching: None,
            polled_at: Instant::now(),
            poll_in_flight: false,
            next_poll: Instant::now(),
            devices_at: None,
            devices: Vec::new(),
            devices_open: false,
            picked: None,
            claimed: false,
            muted_from: None,
            volume_in_flight: false,
            volume_wanted: None,
            local: Local::Unknown,
            player: None,
            key_draft: String::new(),
            has_key: false,
            cache: HashMap::new(),
            cache_order: std::collections::VecDeque::new(),
            lyrics_shown: false,
            lyrics: None,
            wide: std::cell::Cell::new(true),
            paths: soloist::Paths::standard(),
        }
    }
}

impl std::fmt::Debug for State {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("spotify::State")
            .field("connected", &self.connected())
            .field("local", &self.local)
            .finish_non_exhaustive()
    }
}

impl Drop for State {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Release);
        if let Some(cancel) = self.login_cancel.take() {
            cancel.cancel();
        }
    }
}

impl State {
    pub fn connected(&self) -> bool {
        self.session.is_some()
    }

    /// The screen on top of the stack.
    #[must_use]
    pub fn screen(&self) -> Option<&Screen> {
        self.stack.last()
    }

    /// Whether Spotify is what the app's player shows and controls.
    #[must_use]
    pub fn is_active(&self) -> bool {
        self.claimed && self.connected() && self.playing.as_ref().is_some_and(|p| p.track.is_some())
    }

    /// Whether the event starts Spotify playback (the app pauses its own).
    #[must_use]
    pub fn starts_playback(&self, event: &Event) -> bool {
        self.connected()
            && match event {
                Event::PlayTrack(..) | Event::PlayUri(_) => true,
                Event::TogglePlay => !self.playing.as_ref().is_some_and(|p| p.is_playing),
                _ => false,
            }
    }

    /// The app started its own playback: Spotify steps aside.
    pub fn release(&mut self) -> Task<Event> {
        if !self.claimed {
            return Task::none();
        }
        self.claimed = false;
        if self.playing.as_ref().is_some_and(|p| p.is_playing) {
            if let Some(p) = self.playing.as_mut() {
                p.is_playing = false;
            }
            return self.command("PUT", "/me/player/pause".into(), None, true);
        }
        Task::none()
    }

    /// How often the app should send [`Event::Tick`]: often while synced
    /// lyrics follow the song, every second otherwise.
    #[must_use]
    pub fn tick_interval(&self) -> Option<Duration> {
        if !self.connected() {
            return None;
        }
        let following = self.lyrics_shown
            && self.playing.as_ref().is_some_and(|p| p.is_playing)
            && matches!(&self.lyrics, Some((_, Lyrics::Found { synced: true, .. })));
        Some(if following {
            Duration::from_millis(250)
        } else {
            Duration::from_secs(1)
        })
    }

    /// Messages for the app's notifications.
    pub fn take_notices(&mut self) -> Vec<String> {
        std::mem::take(&mut self.notices)
    }

    fn lyrics_view(&self, uri: &str) -> Option<LyricsView<'_>> {
        let (for_uri, lyrics) = self.lyrics.as_ref()?;
        if for_uri != uri {
            return None;
        }
        let current = match lyrics {
            Lyrics::Found {
                lines,
                synced: true,
            } => {
                // A little ahead, so a line lights as it starts.
                let at = self.position_ms() + 250;
                lines
                    .iter()
                    .rposition(|l| l.millis.is_some_and(|m| m <= at))
            }
            _ => None,
        };
        Some(LyricsView { lyrics, current })
    }

    /// Fetch the current song's lyrics when they're shown and not loaded.
    fn ensure_lyrics(&mut self) -> Task<Event> {
        if !self.lyrics_shown {
            return Task::none();
        }
        let Some(track) = self.playing.as_ref().and_then(|p| p.track.clone()) else {
            return Task::none();
        };
        if self
            .lyrics
            .as_ref()
            .is_some_and(|(uri, _)| *uri == track.uri)
        {
            return Task::none();
        }
        self.lyrics = Some((track.uri.clone(), Lyrics::Loading));
        let uri = track.uri.clone();
        background(
            move || lyrics::fetch(&track),
            move |found| Event::LyricsLoaded(uri, found),
        )
    }

    /// Keep a loaded page for next time.
    fn remember(&mut self, route: Route, content: Content) {
        if matches!(
            content,
            Content::Loading | Content::Failed(_) | Content::Ready
        ) {
            return;
        }
        if self.cache.insert(route.clone(), content).is_none() {
            self.cache_order.push_back(route);
            while self.cache_order.len() > 40 {
                if let Some(old) = self.cache_order.pop_front() {
                    self.cache.remove(&old);
                }
            }
        }
    }

    /// Where playback position is now, interpolated between polls.
    fn position_ms(&self) -> u64 {
        let Some(p) = &self.playing else { return 0 };
        let elapsed = if p.is_playing {
            self.polled_at.elapsed().as_millis() as u64
        } else {
            0
        };
        let duration = p.track.as_ref().map_or(u64::MAX, |t| t.duration_ms);
        (p.progress_ms + elapsed).min(duration)
    }

    /// What the app's player shows while Spotify is the source.
    #[must_use]
    pub fn remote(&self) -> Option<Remote<'_>> {
        if !self.is_active() {
            return None;
        }
        let p = self.playing.as_ref()?;
        let track = p.track.as_ref()?;
        Some(Remote {
            track,
            cover_key: p.cover.as_ref().map(|_| cover_key(track)),
            position: self.position_ms() as f64 / 1000.0,
            duration: track.duration_ms as f64 / 1000.0,
            playing: p.is_playing,
            shuffle: p.shuffle,
            repeat: match p.repeat.as_str() {
                "context" => Repeat::Context,
                "track" => Repeat::Track,
                _ => Repeat::Off,
            },
            volume: p
                .device
                .as_ref()
                .filter(|d| d.supports_volume)
                .and_then(|d| d.volume)
                .map(|v| f64::from(v) / 100.0),
            device: p.device.as_ref().map(|d| d.name.as_str()),
            liked: self.saved.get(&track.uri).copied(),
            local: p.device.as_ref().is_some_and(|d| self.is_local_device(d)),
            lyrics_shown: self.lyrics_shown,
            lyrics: self.lyrics_view(&track.uri),
        })
    }

    fn is_local_device(&self, device: &Device) -> bool {
        self.local == Local::Ready && device.name == DEVICE_NAME
    }

    /// Covers and thumbnails to fetch: what is on screen first, then the
    /// pages kept for later, so going back shows their pictures at once.
    #[must_use]
    pub fn artwork(&self) -> Vec<(String, String)> {
        let mut out = Vec::new();
        if let Some(p) = &self.playing
            && let (Some(track), Some(cover)) = (&p.track, &p.cover)
        {
            out.push((cover_key(track), cover.clone()));
        }
        if let Some(screen) = self.screen() {
            content_art(&screen.content, &mut out);
        }
        for route in self.cache_order.iter().rev() {
            if let Some(content) = self.cache.get(route) {
                content_art(content, &mut out);
            }
        }
        let mut seen = std::collections::HashSet::new();
        out.retain(|(key, _)| seen.insert(key.clone()));
        out.truncate(400);
        out
    }

    pub fn update(&mut self, event: Event) -> Task<Event> {
        match event {
            // ---------------------------------------------------------- sign-in
            Event::ClientId(value) if !self.busy && !self.connected() => {
                self.client_id = value.chars().take(32).collect();
            }
            Event::Restore if !self.busy && !self.connected() => {
                self.busy = true;
                self.auth_generation = self.auth_generation.wrapping_add(1);
                return auth_task(restore_login);
            }
            Event::Connect if !self.busy && !self.connected() => return self.connect(),
            Event::Authed(result) => return self.authed(result),
            Event::Disconnect => return self.disconnect(),
            Event::OpenLink(url) => {
                return background(
                    move || open_browser(url, &AtomicBool::new(false)),
                    |_| Event::Dismiss,
                );
            }
            Event::Copy(value) => {
                self.copied = Some(value);
                return iced::clipboard::write(value.to_owned()).then(|_| Task::none());
            }
            Event::PasteClientId if !self.busy && !self.connected() => {
                return iced::clipboard::read_text()
                    .map(|text| Event::PastedClientId(text.ok().map(|t| t.as_str().to_owned())));
            }
            Event::PastedClientId(Some(text)) if !self.busy && !self.connected() => {
                if let Some(id) = client_id_in(&text) {
                    self.client_id = id;
                    self.status = None;
                } else {
                    self.status = Some(Failure::ClientId.text().into());
                }
            }

            // --------------------------------------------------------- browsing
            Event::Shown if self.connected() && self.stack.is_empty() => {
                return self.go(Route::Tab(self.tab), true);
            }
            Event::Tab(tab) if self.connected() => {
                self.tab = tab;
                self.query.clear();
                return self.go(Route::Tab(tab), true);
            }
            Event::Query(value) => self.query = value.chars().take(200).collect(),
            Event::Search if self.connected() => {
                let query = self.query.trim().to_owned();
                if query.is_empty() {
                    return Task::none();
                }
                // A new search replaces the last one rather than stacking.
                if matches!(self.screen().map(|s| &s.route), Some(Route::Search(_))) {
                    self.stack.pop();
                }
                return self.go(Route::Search(query), false);
            }
            Event::ClearSearch => {
                self.query.clear();
                if matches!(self.screen().map(|s| &s.route), Some(Route::Search(_))) {
                    self.stack.pop();
                }
                if self.stack.is_empty() && self.connected() {
                    return self.go(Route::Tab(self.tab), true);
                }
            }
            Event::Open(route) if self.connected() => {
                if self.screen().is_some_and(|s| s.route == route) {
                    return Task::none();
                }
                return self.go(route, false);
            }
            Event::Back => {
                if self.stack.len() > 1 {
                    let left = self.stack.pop();
                    if matches!(left.map(|s| s.route), Some(Route::Search(_))) {
                        self.query.clear();
                    }
                    // Back where that page was.
                    let y = self.screen().map_or(0.0, |s| s.scroll_y);
                    return iced::widget::operation::scroll_to(
                        scroll_id(),
                        iced::widget::scrollable::AbsoluteOffset {
                            x: None,
                            y: Some(y),
                        },
                        iced::widget::operation::Animation::Instant,
                    );
                }
            }
            Event::Scrolled(y) => {
                if let Some(screen) = self.stack.last_mut() {
                    screen.scroll_y = y;
                }
            }
            Event::Reload => {
                if let Some(route) = self.screen().map(|s| s.route.clone()) {
                    self.stack.pop();
                    return self.go(route, false);
                }
            }
            Event::More => return self.more(),
            Event::Loaded(serial, result) => {
                let mut know = Vec::new();
                let mut keep = None;
                if let Err(error) = &result {
                    self.retry_after(error);
                }
                if let Some(screen) = self.stack.iter_mut().find(|s| s.serial == serial) {
                    match result {
                        Ok(content) => {
                            know = known_saved(&screen.route, &content);
                            keep = Some((screen.route.clone(), content.clone()));
                            // Only a change replaces what is on screen (a
                            // page loading more keeps its rows).
                            if screen.content != content && !screen.loading_more {
                                screen.content = content;
                            }
                        }
                        // A cached page stays; the failure is a notification.
                        Err(error) if !matches!(screen.content, Content::Loading) => {
                            self.notices.push(error.explain());
                        }
                        Err(error) => screen.content = Content::Failed(error.explain()),
                    }
                }
                if let Some((route, content)) = keep {
                    self.remember(route, content);
                }
                for (uri, saved) in know {
                    self.saved.insert(uri, saved);
                }
                return self.check_saved();
            }
            Event::Appended(serial, result) => {
                let mut keep = None;
                if let Some(screen) = self.stack.iter_mut().find(|s| s.serial == serial) {
                    screen.loading_more = false;
                    match result {
                        Ok(more) => {
                            append(&mut screen.content, more);
                            keep = Some((screen.route.clone(), screen.content.clone()));
                        }
                        Err(error) => self.notices.push(error.explain()),
                    }
                }
                if let Some((route, content)) = keep {
                    self.remember(route, content);
                }
                if self.tab == Tab::Liked
                    && let Some(Screen {
                        route: Route::Tab(Tab::Liked),
                        content: Content::Tracks(page),
                        ..
                    }) = self.stack.last()
                {
                    let uris: Vec<String> = page.items.iter().map(|t| t.uri.clone()).collect();
                    for uri in uris {
                        self.saved.insert(uri, true);
                    }
                }
                return self.check_saved();
            }
            Event::Prefetched(route, Ok(content)) => {
                for (uri, saved) in known_saved(&route, &content) {
                    self.saved.insert(uri, saved);
                }
                self.remember(route, content);
            }
            Event::Prefetched(_, Err(error)) => self.retry_after(&error),
            Event::ToggleLyrics => {
                self.lyrics_shown = !self.lyrics_shown;
                return self.ensure_lyrics();
            }
            Event::LyricsLoaded(uri, lyrics) => {
                if self
                    .lyrics
                    .as_ref()
                    .is_some_and(|(for_uri, _)| *for_uri == uri)
                {
                    self.lyrics = Some((uri, lyrics));
                }
            }
            Event::OpenOnSpotify(uri) => {
                let Some((kind, id)) = uri
                    .strip_prefix("spotify:")
                    .and_then(|rest| rest.split_once(':'))
                    .filter(|(kind, id)| {
                        ["track", "album", "artist", "playlist"].contains(kind)
                            && spotify_id(&uri, kind) == Some(*id)
                    })
                else {
                    return Task::none();
                };
                let url = format!("https://open.spotify.com/{kind}/{id}");
                return background(
                    move || open_browser(&url, &AtomicBool::new(false)),
                    |_| Event::Dismiss,
                );
            }

            // ---------------------------------------------------------- library
            Event::ToggleSaved(uri) if self.connected() => {
                let now = !self.saved.get(&uri).copied().unwrap_or(false);
                self.saved.insert(uri.clone(), now);
                let method = if now { "PUT" } else { "DELETE" };
                let path = format!("/me/library?uris={}", encode(&uri));
                return self.call(
                    move |api| api.send(method, &path, None).map(|_| ()),
                    Event::Commanded,
                );
            }
            Event::SavedKnown(known) => {
                for (uri, saved) in known {
                    self.saved.insert(uri, saved);
                }
            }

            // --------------------------------------------------------- playback
            Event::PlayTrack(list, index) => return self.play_track(list, index),
            Event::PlayUri(uri) => {
                let body = if uri.starts_with("spotify:track:") {
                    json!({ "uris": [uri] })
                } else {
                    json!({ "context_uri": uri })
                };
                return self.play(body);
            }
            Event::Queue(uri) if spotify_id(&uri, "track").is_some() => {
                self.notices.push("Added to the queue".into());
                let device = self.device_param();
                return self.command(
                    "POST",
                    format!("/me/player/queue?uri={}{device}", encode(&uri)),
                    None,
                    false,
                );
            }
            Event::TogglePlay => {
                let playing = self.playing.as_ref().is_some_and(|p| p.is_playing);
                let position = self.position_ms();
                if let Some(p) = self.playing.as_mut() {
                    p.progress_ms = position;
                    p.is_playing = !playing;
                    self.polled_at = Instant::now();
                }
                if playing {
                    return self.command("PUT", "/me/player/pause".into(), None, true);
                }
                self.claimed = true;
                let device = self.device_param_first();
                return self.command("PUT", format!("/me/player/play{device}"), None, true);
            }
            Event::Next => return self.command("POST", "/me/player/next".into(), None, true),
            Event::Previous => {
                // Like other players: past a few seconds, back to the start.
                if self.position_ms() > 3000 {
                    return self.update(Event::Seek(0.0));
                }
                return self.command("POST", "/me/player/previous".into(), None, true);
            }
            Event::Seek(secs) if secs.is_finite() => {
                let ms = (secs.max(0.0) * 1000.0) as u64;
                if let Some(p) = self.playing.as_mut() {
                    p.progress_ms = ms;
                    self.polled_at = Instant::now();
                }
                return self.command(
                    "PUT",
                    format!("/me/player/seek?position_ms={ms}"),
                    None,
                    true,
                );
            }
            Event::Volume(volume) => {
                let percent = (volume.clamp(0.0, 1.0) * 100.0).round() as u8;
                self.muted_from = None;
                return self.set_volume(percent);
            }
            Event::ToggleMute => {
                let current = self
                    .playing
                    .as_ref()
                    .and_then(|p| p.device.as_ref())
                    .and_then(|d| d.volume)
                    .unwrap_or(0);
                if let Some(before) = self.muted_from.take() {
                    return self.set_volume(before.max(10));
                }
                self.muted_from = Some(current);
                return self.set_volume(0);
            }
            Event::Shuffle => {
                let on = !self.playing.as_ref().is_some_and(|p| p.shuffle);
                if let Some(p) = self.playing.as_mut() {
                    p.shuffle = on;
                }
                return self.command("PUT", format!("/me/player/shuffle?state={on}"), None, false);
            }
            Event::CycleRepeat => {
                let next = match self.playing.as_ref().map(|p| p.repeat.as_str()) {
                    Some("off") | None => "context",
                    Some("context") => "track",
                    _ => "off",
                };
                if let Some(p) = self.playing.as_mut() {
                    p.repeat = next.to_owned();
                }
                return self.command(
                    "PUT",
                    format!("/me/player/repeat?state={next}"),
                    None,
                    false,
                );
            }
            Event::PickDevice(id) => {
                self.picked = Some(id.clone());
                self.devices_open = false;
                // Playing moves there; otherwise it's where play goes next.
                let playing = self.playing.as_ref().is_some_and(|p| p.is_playing);
                return self.command(
                    "PUT",
                    "/me/player".into(),
                    Some(json!({ "device_ids": [id], "play": playing })),
                    true,
                );
            }
            Event::ToggleDevices => {
                self.devices_open = !self.devices_open;
                if self.devices_open {
                    self.devices_at = None;
                    self.next_poll = Instant::now();
                    return self.poll();
                }
            }
            Event::Tick => {
                if self.connected() && Instant::now() >= self.next_poll {
                    return self.poll();
                }
            }
            Event::Polled(result) => {
                self.poll_in_flight = false;
                match result {
                    Ok(poll) => {
                        if let Some(me) = poll.me {
                            self.me = Some(me);
                        }
                        if let Some(devices) = poll.devices {
                            self.devices = devices;
                            self.devices_at = Some(Instant::now());
                        }
                        // Something already playing on Spotify when nothing
                        // else is: the player shows it.
                        let was_known = self.playing.is_some();
                        if !was_known && poll.playing.as_ref().is_some_and(|p| p.is_playing) {
                            self.claimed = true;
                        }
                        self.playing = poll.playing;
                        self.polled_at = Instant::now();
                        // Spotify reports the new song once it starts; an
                        // older poll still shows the previous one.
                        let reported = self
                            .playing
                            .as_ref()
                            .and_then(|p| p.track.as_ref())
                            .map(|t| t.uri.as_str());
                        if self.switching.as_ref().is_some_and(|(uri, at)| {
                            reported == Some(uri.as_str()) || at.elapsed() > SWITCH_PATIENCE
                        }) {
                            self.switching = None;
                        }
                        let saved_check = self
                            .playing
                            .as_ref()
                            .and_then(|p| p.track.as_ref())
                            .filter(|t| !self.saved.contains_key(&t.uri))
                            .map(|t| t.uri.clone());
                        let interval = if self.playing.as_ref().is_some_and(|p| p.is_playing) {
                            POLL_PLAYING
                        } else {
                            POLL_IDLE
                        };
                        self.next_poll = Instant::now() + interval;
                        let lyrics = self.ensure_lyrics();
                        if let Some(uri) = saved_check {
                            return Task::batch([lyrics, self.contains(vec![uri])]);
                        }
                        return lyrics;
                    }
                    Err(error) => {
                        self.retry_after(&error);
                        self.next_poll = Instant::now() + POLL_IDLE;
                        if error == Failure::Expired {
                            self.notices.push(error.text().into());
                        }
                    }
                }
            }
            Event::Commanded(result) => {
                self.volume_in_flight = false;
                if let Err(error) = result {
                    self.switching = None;
                    self.retry_after(&error);
                    self.notices.push(error.explain());
                }
                if let Some(wanted) = self.volume_wanted.take() {
                    return self.set_volume(wanted);
                }
                self.next_poll = self.next_poll.min(Instant::now() + POLL_AFTER_COMMAND);
            }

            // ----------------------------------------------------- this computer
            Event::LocalChecked(program, key, age) => {
                self.has_key = key.is_some();
                return self.local_checked(program, key, age);
            }
            Event::Install => {
                let Some(paths) = self.paths.clone() else {
                    self.local =
                        Local::Failed("Music couldn't find a folder for the player.".into());
                    return Task::none();
                };
                self.player = None;
                self.local = Local::Installing;
                return background(move || soloist::install(&paths), Event::Installed);
            }
            Event::Installed(Ok(())) => return self.check_local(),
            Event::Installed(Err(reason)) => self.local = Local::Failed(reason),
            Event::KeyDraft(value) => self.key_draft = value.chars().take(512).collect(),
            Event::PasteKey => {
                return iced::clipboard::read_text()
                    .map(|text| Event::PastedKey(text.ok().map(|t| t.as_str().to_owned())));
            }
            Event::PastedKey(Some(text)) => {
                if let Some(key) = soloist::key_in(&text) {
                    self.key_draft = key;
                }
            }
            Event::SaveKey => {
                let Some(key) = soloist::key_in(&self.key_draft) else {
                    self.notices
                        .push("That doesn't look like a Soloist API key.".into());
                    return Task::none();
                };
                return background(
                    move || soloist::save_key(&key).map(|()| key),
                    Event::KeySaved,
                );
            }
            Event::KeySaved(Ok(_)) => {
                self.key_draft.clear();
                self.has_key = true;
                self.player = None;
                return self.check_local();
            }
            Event::ChangeKey => self.has_key = false,
            Event::ChangeKeyAndOpen => {
                self.has_key = false;
                if self.connected() {
                    return self.go(Route::SetUp, false);
                }
            }
            Event::KeySaved(Err(reason)) => self.notices.push(reason),
            Event::Soloist(report) => return self.soloist_report(report),
            Event::RestartPlayer => {
                self.player = None;
                return self.check_local();
            }
            Event::Dismiss => {}
            _ => {}
        }
        Task::none()
    }

    // ------------------------------------------------------------------ auth

    fn authed(&mut self, result: Result<Auth, Failure>) -> Task<Event> {
        self.busy = false;
        self.login_cancel = None;
        match result {
            Ok(Auth::Connected(session)) => {
                self.session = Some(session);
                self.status = None;
                self.signed_in()
            }
            Ok(Auth::Restored(session, client_id)) => {
                self.client_id = client_id;
                self.session = Some(session);
                self.status = None;
                self.signed_in()
            }
            Ok(Auth::NotRestored) => Task::none(),
            Ok(Auth::Renew(client_id)) => {
                self.client_id = client_id;
                self.renew = true;
                Task::none()
            }
            Err(error) => {
                self.status = Some(error.explain());
                Task::none()
            }
        }
    }

    fn signed_in(&mut self) -> Task<Event> {
        self.renew = false;
        self.next_poll = Instant::now();
        self.devices_at = None;
        let mut tasks = vec![self.poll(), self.check_local()];
        if self.stack.is_empty() {
            tasks.push(self.go(Route::Tab(self.tab), true));
        }
        // The other tabs load in the background, so switching shows them at once.
        let me = self.me.as_ref().map(|m| m.id.clone());
        for tab in Tab::ALL.into_iter().filter(|t| *t != self.tab) {
            let route = Route::Tab(tab);
            let me = me.clone();
            let load_route = route.clone();
            tasks.push(self.call(
                move |api| load(api, &load_route, me.as_deref()),
                move |result| Event::Prefetched(route, result),
            ));
        }
        Task::batch(tasks)
    }

    fn disconnect(&mut self) -> Task<Event> {
        if self.session.is_some() {
            tokio::task::spawn_blocking(forget_login);
        }
        self.cancelled.store(true, Ordering::Release);
        let cancel = self.login_cancel.take();
        self.session = None;
        self.busy = false;
        self.status = None;
        self.notices.clear();
        self.stack.clear();
        self.cache.clear();
        self.cache_order.clear();
        self.lyrics = None;
        self.saved.clear();
        self.playing = None;
        self.devices.clear();
        self.picked = None;
        self.claimed = false;
        self.me = None;
        self.player = None;
        self.local = Local::Unknown;
        self.cancelled = Arc::new(AtomicBool::new(false));
        if let Some(cancel) = cancel {
            cancel.cancel();
        }
        Task::none()
    }

    fn connect(&mut self) -> Task<Event> {
        let id = self.client_id.trim();
        if id.len() != 32 || !id.bytes().all(|b| b.is_ascii_hexdigit()) {
            self.status = Some(Failure::ClientId.text().into());
            return Task::none();
        }
        let client_id = id.to_owned();
        let config = ClientConfig::new(AUTHORIZE, TOKEN, id).and_then(|c| c.with_scopes(SCOPES));
        let Ok(listener) = LoopbackRedirect::bind_port(REDIRECT_PATH, REDIRECT_PORT) else {
            self.status = Some(Failure::PortInUse.text().into());
            return Task::none();
        };
        let Ok(client) = config else {
            self.status = Some(Failure::SignIn.text().into());
            return Task::none();
        };
        let Ok(pending) = client.begin(listener.redirect_uri(), &[]) else {
            self.status = Some(Failure::SignIn.text().into());
            return Task::none();
        };
        self.login_cancel = Some(listener.canceller());
        self.busy = true;
        self.status = Some("Finish signing in in your browser.".into());
        let cancelled = Arc::clone(&self.cancelled);
        auth_task(move || {
            open_browser(pending.authorization_url(), &cancelled)?;
            let callback = listener
                .wait_for(Instant::now() + Duration::from_secs(180), |url| {
                    pending.is_callback(url)
                })
                .map_err(|_| Failure::SignIn)?;
            let code = pending.complete(&callback).map_err(|_| Failure::SignIn)?;
            let token = exchange(&pending.token_request(&code), &cancelled)?;
            let expires = expiry(&token);
            if let Some(refresh) = &token.refresh_token {
                save_login(&client_id, refresh);
            }
            Ok(Auth::Connected(Arc::new(Mutex::new(Session {
                client,
                client_id,
                token,
                expires,
            }))))
        })
    }

    // ------------------------------------------------------------ requests

    fn retry_after(&mut self, error: &Failure) {
        match error {
            Failure::Busy(wait) => self.retry_at = Instant::now().checked_add(*wait),
            Failure::Quota => self.retry_at = Instant::now().checked_add(Duration::from_secs(3600)),
            _ => {}
        }
    }

    /// Run `work` with a fresh access token, off the UI thread.
    fn call<T: Send + 'static>(
        &self,
        work: impl FnOnce(&Api) -> Result<T, Failure> + Send + 'static,
        done: impl FnOnce(Result<T, Failure>) -> Event + Send + 'static,
    ) -> Task<Event> {
        if let Some(at) = self.retry_at
            && Instant::now() < at
        {
            let wait = at - Instant::now();
            return Task::done(done(Err(Failure::Busy(wait))));
        }
        let Some(session) = self.session.clone() else {
            return Task::none();
        };
        let cancelled = Arc::clone(&self.cancelled);
        background(
            move || {
                let token = fresh_token(&session, &cancelled)?;
                work(&Api { token, cancelled })
            },
            done,
        )
    }

    /// Push a screen for `route` (or reset the stack to it) and load it.
    fn go(&mut self, route: Route, reset: bool) -> Task<Event> {
        if reset {
            self.stack.clear();
        }
        self.devices_open = false;
        self.serial += 1;
        let serial = self.serial;
        let me = self.me.as_ref().map(|m| m.id.clone());
        let content = if route == Route::SetUp {
            Content::Ready
        } else {
            // What was loaded before shows at once; a fresh copy replaces it
            // only if something changed.
            self.cache.get(&route).cloned().unwrap_or(Content::Loading)
        };
        self.stack.push(Screen {
            serial,
            route: route.clone(),
            content,
            loading_more: false,
            scroll_y: 0.0,
        });
        // A page opens at its top.
        let to_top = iced::widget::operation::snap_to(
            scroll_id(),
            iced::widget::scrollable::RelativeOffset::START,
            iced::widget::operation::Animation::Instant,
        );
        if route == Route::SetUp {
            return to_top;
        }
        Task::batch([
            to_top,
            self.call(
                move |api| load(api, &route, me.as_deref()),
                move |r| Event::Loaded(serial, r),
            ),
        ])
    }

    fn more(&mut self) -> Task<Event> {
        let Some(screen) = self.stack.last_mut() else {
            return Task::none();
        };
        if screen.loading_more {
            return Task::none();
        }
        let next = match &screen.content {
            Content::Tracks(p) => p.next.clone().map(|n| (n, Kind::Tracks)),
            Content::Albums(p) => p.next.clone().map(|n| (n, Kind::Albums)),
            Content::Artists(p) => p.next.clone().map(|n| (n, Kind::Artists)),
            Content::Playlists(p) => p.next.clone().map(|n| (n, Kind::Playlists)),
            Content::Album(p) => p.tracks.next.clone().map(|n| (n, Kind::AlbumTracks)),
            Content::Playlist(p) => p.tracks.next.clone().map(|n| (n, Kind::Tracks)),
            _ => None,
        };
        let Some((path, kind)) = next else {
            return Task::none();
        };
        screen.loading_more = true;
        let serial = screen.serial;
        let album = match &screen.content {
            Content::Album(p) => Some(p.album.clone()),
            _ => None,
        };
        self.call(
            move |api| {
                let value = api.get(&path)?;
                // Followed artists nest their page under "artists".
                let page_value = if value["artists"].is_object() {
                    &value["artists"]
                } else {
                    &value
                };
                Ok(match kind {
                    Kind::Tracks => {
                        Content::Tracks(model::page(page_value, |v| model::track(v, None))?)
                    }
                    Kind::AlbumTracks => Content::Tracks(model::page(page_value, |v| {
                        model::track(v, album.as_ref())
                    })?),
                    Kind::Albums => Content::Albums(model::page(page_value, model::album)?),
                    Kind::Artists => Content::Artists(model::page(page_value, model::artist)?),
                    Kind::Playlists => {
                        Content::Playlists(model::page(page_value, model::playlist)?)
                    }
                })
            },
            move |r| Event::Appended(serial, r),
        )
    }

    /// Ask Spotify which of the visible songs, albums and artists are saved.
    fn check_saved(&mut self) -> Task<Event> {
        let Some(screen) = self.stack.last() else {
            return Task::none();
        };
        let mut uris: Vec<String> = Vec::new();
        let mut add = |uri: &str| uris.push(uri.to_owned());
        match &screen.content {
            Content::Tracks(p) => p.items.iter().for_each(|t| add(&t.uri)),
            Content::Search(r) => r.tracks.iter().for_each(|t| add(&t.uri)),
            Content::Artist(p) => {
                add(&p.artist.uri);
                p.popular.iter().for_each(|t| add(&t.uri));
            }
            Content::Album(p) => {
                add(&p.album.uri);
                p.tracks.items.iter().for_each(|t| add(&t.uri));
            }
            Content::Playlist(p) => p.tracks.items.iter().for_each(|t| add(&t.uri)),
            _ => {}
        }
        uris.retain(|u| !self.saved.contains_key(u));
        uris.dedup();
        self.contains(uris)
    }

    fn contains(&self, uris: Vec<String>) -> Task<Event> {
        if uris.is_empty() {
            return Task::none();
        }
        self.call(
            move |api| {
                let mut known = Vec::new();
                for chunk in uris.chunks(LIBRARY_BATCH) {
                    let joined = chunk.join(",");
                    let value =
                        api.get(&format!("/me/library/contains?uris={}", encode(&joined)))?;
                    if let Some(flags) = value.as_array() {
                        known.extend(chunk.iter().cloned().zip(flags.iter().map(|f| f == true)));
                    }
                }
                Ok(known)
            },
            |r| Event::SavedKnown(r.unwrap_or_default()),
        )
    }

    // -------------------------------------------------------------- player

    fn poll(&mut self) -> Task<Event> {
        if self.poll_in_flight {
            return Task::none();
        }
        self.poll_in_flight = true;
        self.next_poll = Instant::now() + POLL_IDLE;
        let want_devices = self.devices_open
            || self
                .devices_at
                .is_none_or(|at| at.elapsed() >= DEVICES_EVERY)
            || self.local == Local::Pairing;
        let want_me = self.me.is_none();
        self.call(
            move |api| {
                let player = api.get("/me/player")?;
                let playing = (!player.is_null()).then(|| model::playing(&player));
                let devices = if want_devices {
                    Some(model::devices(&api.get("/me/player/devices")?)?)
                } else {
                    None
                };
                let me = if want_me {
                    api.get("/me").ok().and_then(|v| model::me(&v))
                } else {
                    None
                };
                Ok(Box::new(Poll {
                    playing,
                    devices,
                    me,
                }))
            },
            Event::Polled,
        )
    }

    /// `?device_id=` for the device playback should go to, when it isn't
    /// already the active one.
    fn device_param(&self) -> String {
        self.target_device()
            .filter(|d| !d.active)
            .map(|d| format!("&device_id={}", encode(&d.id)))
            .unwrap_or_default()
    }

    fn device_param_first(&self) -> String {
        self.device_param().replacen('&', "?", 1)
    }

    /// The device to play on: the one picked, the active one, this
    /// computer, then any.
    /// The song shown as current: the one just chosen, else what Spotify
    /// reports playing.
    #[must_use]
    pub fn current_track_uri(&self) -> Option<&str> {
        self.switching
            .as_ref()
            .filter(|(_, at)| at.elapsed() <= SWITCH_PATIENCE)
            .map(|(uri, _)| uri.as_str())
            .or_else(|| {
                self.playing
                    .as_ref()
                    .and_then(|p| p.track.as_ref())
                    .map(|t| t.uri.as_str())
            })
    }

    fn target_device(&self) -> Option<&Device> {
        let picked = self
            .picked
            .as_ref()
            .and_then(|id| self.devices.iter().find(|d| &d.id == id));
        picked
            .or_else(|| self.devices.iter().find(|d| d.active))
            .or_else(|| self.devices.iter().find(|d| self.is_local_device(d)))
            .or_else(|| self.devices.first())
    }

    fn play(&mut self, body: Value) -> Task<Event> {
        let device = self.device_param_first();
        if self.target_device().is_none()
            && self
                .playing
                .as_ref()
                .and_then(|p| p.device.as_ref())
                .is_none()
        {
            self.notices.push(Failure::NoDevice.text().into());
            self.devices_at = None;
            self.next_poll = Instant::now();
            return Task::none();
        }
        self.claimed = true;
        if let Some(p) = self.playing.as_mut() {
            p.is_playing = true;
        }
        self.switching = chosen_track(&body).map(|uri| (uri, Instant::now()));
        self.command("PUT", format!("/me/player/play{device}"), Some(body), true)
    }

    fn play_track(&mut self, list: List, index: usize) -> Task<Event> {
        let Some(screen) = self.screen() else {
            return Task::none();
        };
        let body =
            match (&screen.content, list) {
                (Content::Album(page), List::Main) => {
                    page.tracks.items.get(index).map(
                        |t| json!({ "context_uri": page.album.uri, "offset": { "uri": t.uri } }),
                    )
                }
                (Content::Playlist(page), List::Main) => page.tracks.items.get(index).map(
                    |t| json!({ "context_uri": page.playlist.uri, "offset": { "uri": t.uri } }),
                ),
                (Content::Tracks(page), List::Main) => uris_from(&page.items, index),
                (Content::Artist(page), List::Popular) => uris_from(&page.popular, index),
                (Content::Search(results), List::SearchSongs) => uris_from(&results.tracks, index),
                _ => None,
            };
        match body {
            Some(body) => self.play(body),
            None => Task::none(),
        }
    }

    /// A player command; `optimistic` commands poll again soon after.
    fn command(
        &mut self,
        method: &'static str,
        path: String,
        body: Option<Value>,
        _optimistic: bool,
    ) -> Task<Event> {
        self.call(
            move |api| api.send(method, &path, body).map(|_| ()),
            Event::Commanded,
        )
    }

    fn set_volume(&mut self, percent: u8) -> Task<Event> {
        if let Some(d) = self.playing.as_mut().and_then(|p| p.device.as_mut()) {
            d.volume = Some(percent);
        }
        if self.volume_in_flight {
            self.volume_wanted = Some(percent);
            return Task::none();
        }
        self.volume_in_flight = true;
        self.command(
            "PUT",
            format!("/me/player/volume?volume_percent={percent}"),
            None,
            false,
        )
    }

    // -------------------------------------------------------- this computer

    fn check_local(&mut self) -> Task<Event> {
        if self.player.is_some() {
            return Task::none();
        }
        let Some(paths) = self.paths.clone() else {
            self.local = Local::Failed("Music couldn't find a folder for the player.".into());
            return Task::none();
        };
        self.local = Local::Checking;
        background(
            move || {
                let program = paths.program();
                let age = program.as_deref().and_then(soloist::build_age_days);
                (program, soloist::load_key(), age)
            },
            |(program, key, age)| Event::LocalChecked(program, key, age),
        )
    }

    fn local_checked(
        &mut self,
        program: Option<PathBuf>,
        key: Option<String>,
        age: Option<u64>,
    ) -> Task<Event> {
        let (Some(program), Some(paths)) = (program, self.paths.clone()) else {
            self.local = Local::NotInstalled;
            return Task::none();
        };
        if age.is_some_and(|d| d >= soloist::BUILD_LIFETIME_DAYS) {
            self.local = Local::Expired;
            return Task::none();
        }
        let Some(key) = key else {
            self.local = Local::NeedsKey;
            return Task::none();
        };
        if !self.connected() {
            return Task::none();
        }
        let (sender, receiver) = iced::futures::channel::mpsc::unbounded();
        match soloist::Player::start(&program, &key, &paths, move |report| {
            let _ = sender.unbounded_send(report);
        }) {
            Ok(player) => {
                self.player = Some(player);
                self.local = Local::Starting;
                Task::run(receiver, Event::Soloist)
            }
            Err(reason) => {
                self.local = Local::Failed(reason);
                Task::none()
            }
        }
    }

    fn soloist_report(&mut self, report: soloist::Report) -> Task<Event> {
        match report {
            soloist::Report::Auth { logged_in: true } => {
                self.local = Local::Ready;
                // Spotify lists the device a moment after it signs in.
                self.devices_at = None;
                self.next_poll = Instant::now() + Duration::from_secs(1);
                if matches!(self.screen().map(|s| &s.route), Some(Route::SetUp)) {
                    self.stack.pop();
                    if self.stack.is_empty() {
                        return self.go(Route::Tab(self.tab), true);
                    }
                }
            }
            soloist::Report::Auth { logged_in: false } => self.local = Local::Pairing,
            soloist::Report::Exited(stop) => {
                self.player = None;
                self.local = match stop {
                    soloist::Stop::Expired => Local::Expired,
                    soloist::Stop::BadKey => {
                        tokio::task::spawn_blocking(soloist::forget_key);
                        self.notices.push(
                            "Spotify didn't accept the Soloist API key. Paste it again.".into(),
                        );
                        Local::NeedsKey
                    }
                    soloist::Stop::Failed(reason) => Local::Failed(reason),
                };
            }
        }
        Task::none()
    }
}

/// The pictures a page shows.
fn content_art(content: &Content, out: &mut Vec<(String, String)>) {
    let mut push = |uri: &str, url: &Option<String>| {
        if let Some(url) = url {
            out.push((art_key(uri), url.clone()));
        }
    };
    match content {
        Content::Tracks(page) => page.items.iter().for_each(|t| push(&t.uri, &t.thumb)),
        Content::Albums(page) => page.items.iter().for_each(|a| push(&a.uri, &a.image)),
        Content::Artists(page) => page.items.iter().for_each(|a| push(&a.uri, &a.image)),
        Content::Playlists(page) => page.items.iter().for_each(|p| push(&p.uri, &p.image)),
        Content::Search(r) => {
            r.tracks.iter().for_each(|t| push(&t.uri, &t.thumb));
            r.artists.iter().for_each(|a| push(&a.uri, &a.image));
            r.albums.iter().for_each(|a| push(&a.uri, &a.image));
            r.playlists.iter().for_each(|p| push(&p.uri, &p.image));
        }
        Content::Artist(page) => {
            push(&page.artist.uri, &page.artist.image);
            page.popular.iter().for_each(|t| push(&t.uri, &t.thumb));
            for list in [
                &page.albums,
                &page.singles,
                &page.compilations,
                &page.appears_on,
            ] {
                list.iter().for_each(|a| push(&a.uri, &a.image));
            }
        }
        Content::Album(page) => push(&page.album.uri, &page.album.image),
        Content::Playlist(page) => {
            push(&page.playlist.uri, &page.playlist.image);
            page.tracks
                .items
                .iter()
                .for_each(|t| push(&t.uri, &t.thumb));
        }
        Content::Loading | Content::Failed(_) | Content::Ready => {}
    }
}

/// The kind of list a "show more" page holds.
#[derive(Debug, Clone, Copy)]
enum Kind {
    Tracks,
    AlbumTracks,
    Albums,
    Artists,
    Playlists,
}

/// The Spotify page's scroll view.
pub(crate) fn scroll_id() -> iced::widget::Id {
    iced::widget::Id::new("music-spotify-scroll")
}

/// A track list played from `index`, in the list's order.
/// How long a chosen song shows as current before Spotify reports it.
const SWITCH_PATIENCE: Duration = Duration::from_secs(10);

/// The song a play request starts with, when it names one.
fn chosen_track(body: &Value) -> Option<String> {
    let uri = match (&body["offset"]["uri"], &body["uris"]) {
        (Value::String(uri), _) => uri.as_str(),
        (_, Value::Array(uris)) => {
            let at = body["offset"]["position"].as_u64().unwrap_or(0);
            uris.get(usize::try_from(at).ok()?)?.as_str()?
        }
        _ => return None,
    };
    uri.starts_with("spotify:track:").then(|| uri.to_owned())
}

fn uris_from(tracks: &[Track], index: usize) -> Option<Value> {
    tracks.get(index)?;
    // Spotify takes a bounded list; keep the clicked song inside it.
    let start = index.saturating_sub(20);
    let uris: Vec<&str> = tracks
        .iter()
        .skip(start)
        .take(100)
        .map(|t| t.uri.as_str())
        .collect();
    Some(json!({ "uris": uris, "offset": { "position": index - start } }))
}

/// What a loaded screen already says about the library.
fn known_saved(route: &Route, content: &Content) -> Vec<(String, bool)> {
    match (route, content) {
        (Route::Tab(Tab::Liked), Content::Tracks(page)) => {
            page.items.iter().map(|t| (t.uri.clone(), true)).collect()
        }
        (Route::Tab(Tab::Artists), Content::Artists(page)) => {
            page.items.iter().map(|a| (a.uri.clone(), true)).collect()
        }
        (Route::Tab(Tab::Albums), Content::Albums(page)) => {
            page.items.iter().map(|a| (a.uri.clone(), true)).collect()
        }
        _ => Vec::new(),
    }
}

/// Add a "show more" page to a screen's list.
fn append(content: &mut Content, more: Content) {
    fn extend<T>(page: &mut Page<T>, more: Page<T>) {
        page.items.extend(more.items);
        page.next = more.next;
    }
    match (content, more) {
        (Content::Tracks(page), Content::Tracks(more)) => extend(page, more),
        (Content::Albums(page), Content::Albums(more)) => extend(page, more),
        (Content::Artists(page), Content::Artists(more)) => extend(page, more),
        (Content::Playlists(page), Content::Playlists(more)) => extend(page, more),
        (Content::Album(page), Content::Tracks(more)) => extend(&mut page.tracks, more),
        (Content::Playlist(page), Content::Tracks(more)) => extend(&mut page.tracks, more),
        _ => {}
    }
}

/// Load what a route shows.
fn load(api: &Api, route: &Route, me: Option<&str>) -> Result<Content, Failure> {
    match route {
        Route::Tab(Tab::Liked) => Ok(Content::Tracks(model::page(
            &api.get("/me/tracks?limit=50")?,
            |v| model::track(v, None),
        )?)),
        Route::Tab(Tab::Recent) => {
            let mut page = model::page(&api.get("/me/player/recently-played?limit=50")?, |v| {
                model::track(v, None)
            })?;
            // The same song played twice in a row shows once.
            page.items.dedup_by(|a, b| a.uri == b.uri);
            page.next = None;
            Ok(Content::Tracks(page))
        }
        Route::Tab(Tab::Playlists) => Ok(Content::Playlists(model::page(
            &api.get("/me/playlists?limit=50")?,
            model::playlist,
        )?)),
        Route::Tab(Tab::Artists) => {
            let value = api.get("/me/following?type=artist&limit=50")?;
            Ok(Content::Artists(model::page(
                &value["artists"],
                model::artist,
            )?))
        }
        Route::Tab(Tab::Albums) => Ok(Content::Albums(model::page(
            &api.get("/me/albums?limit=50")?,
            model::album,
        )?)),
        Route::Search(query) => {
            let value = api.get(&format!(
                "/search?type=track,artist,album,playlist&limit={SEARCH_LIMIT}&q={}",
                encode(query)
            ))?;
            let list = |key: &str| value[key].clone();
            Ok(Content::Search(Box::new(SearchResults {
                tracks: model::page(&list("tracks"), |v| model::track(v, None))
                    .map(|p| p.items)
                    .unwrap_or_default(),
                artists: model::page(&list("artists"), model::artist)
                    .map(|p| p.items)
                    .unwrap_or_default(),
                albums: model::page(&list("albums"), model::album)
                    .map(|p| p.items)
                    .unwrap_or_default(),
                playlists: model::page(&list("playlists"), model::playlist)
                    .map(|p| p.items)
                    .unwrap_or_default(),
            })))
        }
        Route::Artist(uri) => {
            let id = spotify_id(uri, "artist").ok_or(Failure::NotFound)?;
            let artist =
                model::artist(&api.get(&format!("/artists/{id}"))?).ok_or(Failure::InvalidReply)?;
            // Ten releases a request for development-mode apps: each group
            // asks on its own, the big ones for a few pages.
            let releases = |group: &str, pages: usize| -> Vec<Album> {
                let mut found = Vec::new();
                let mut path = Some(format!(
                    "/artists/{id}/albums?include_groups={group}&limit=10"
                ));
                for _ in 0..pages {
                    let Some(next) = path.take() else { break };
                    let Ok(page) = api.get(&next).and_then(|v| model::page(&v, model::album))
                    else {
                        break;
                    };
                    found.extend(page.items);
                    path = page.next;
                }
                found.dedup_by(|a, b| a.name == b.name);
                found
            };
            // Top tracks went away for development-mode apps: the artist's
            // songs from search stand in, in Spotify's relevance order.
            let popular = api
                .get(&format!(
                    "/search?type=track&limit={SEARCH_LIMIT}&q={}",
                    encode(&format!("artist:\"{}\"", artist.name.replace('"', "")))
                ))
                .and_then(|v| model::page(&v["tracks"], |t| model::track(t, None)))
                .map(|p| {
                    p.items
                        .into_iter()
                        .filter(|t| t.artists.iter().any(|a| a.uri == artist.uri))
                        .collect()
                })
                .unwrap_or_default();
            Ok(Content::Artist(Box::new(ArtistPage {
                popular,
                albums: releases("album", 3),
                singles: releases("single", 3),
                compilations: releases("compilation", 1),
                appears_on: releases("appears_on", 1),
                artist,
            })))
        }
        Route::Album(uri) => {
            let id = spotify_id(uri, "album").ok_or(Failure::NotFound)?;
            let value = api.get(&format!("/albums/{id}"))?;
            let album = model::album(&value).ok_or(Failure::InvalidReply)?;
            let tracks = model::page(&value["tracks"], |v| model::track(v, Some(&album)))?;
            Ok(Content::Album(Box::new(AlbumPage {
                label_date: value["release_date"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned(),
                album,
                tracks,
            })))
        }
        Route::Playlist(uri) => {
            let id = spotify_id(uri, "playlist").ok_or(Failure::NotFound)?;
            let value = api.get(&format!("/playlists/{id}"))?;
            let playlist = model::playlist(&value).ok_or(Failure::InvalidReply)?;
            // The playlist reply carries the first songs when Spotify lets
            // the app list them.
            let embedded = ["items", "tracks"]
                .iter()
                .map(|k| &value[*k])
                .find(|v| v["items"].is_array())
                .and_then(|v| model::page(v, |t| model::track(t, None)).ok());
            let owned = me.is_some_and(|me| me == playlist.owner_id) || playlist.collaborative;
            let (tracks, restricted) = match embedded {
                Some(page) if !page.items.is_empty() || playlist.total == 0 => (page, false),
                _ => match api.get(&format!("/playlists/{id}/items?limit=50")) {
                    Ok(v) => (model::page(&v, |t| model::track(t, None))?, false),
                    Err(Failure::Forbidden | Failure::NotFound) => (Page::default(), true),
                    Err(e) if owned => return Err(e),
                    Err(_) => (Page::default(), true),
                },
            };
            Ok(Content::Playlist(Box::new(PlaylistPage {
                playlist,
                tracks,
                restricted,
            })))
        }
        Route::SetUp => Ok(Content::Ready),
    }
}

/// An access token, refreshed first when it is about to expire.
fn fresh_token(session: &Mutex<Session>, cancelled: &AtomicBool) -> Result<SecretText, Failure> {
    let mut session = session.lock().map_err(|_| Failure::Expired)?;
    if Instant::now() >= session.expires {
        let refresh = session
            .token
            .refresh_token
            .clone()
            .ok_or(Failure::Expired)?;
        let mut token = exchange(&session.client.refresh_request(&refresh, &[]), cancelled)?;
        match &token.refresh_token {
            // Spotify may rotate the refresh token: keep the saved login current.
            Some(rotated) if rotated.expose_secret() != refresh.expose_secret() => {
                save_login(&session.client_id, rotated);
            }
            Some(_) => {}
            None => token.refresh_token = Some(refresh),
        }
        session.expires = expiry(&token);
        session.token = token;
    }
    Ok(session.token.access_token.clone())
}

/// Web API calls with one access token.
pub struct Api {
    token: SecretText,
    cancelled: Arc<AtomicBool>,
}

impl Api {
    fn get(&self, path: &str) -> Result<Value, Failure> {
        api(API, "GET", path, None, &self.token, &self.cancelled)
    }

    fn send(&self, method: &str, path: &str, body: Option<Value>) -> Result<Value, Failure> {
        api(API, method, path, body, &self.token, &self.cancelled)
    }
}

/// Run blocking work off the UI thread.
fn background<T: Send + 'static>(
    work: impl FnOnce() -> T + Send + 'static,
    done: impl FnOnce(T) -> Event + Send + 'static,
) -> Task<Event> {
    Task::perform(
        async move { tokio::task::spawn_blocking(work).await },
        move |result| match result {
            Ok(value) => done(value),
            Err(_) => Event::Dismiss,
        },
    )
}

fn auth_task(work: impl FnOnce() -> Result<Auth, Failure> + Send + 'static) -> Task<Event> {
    Task::perform(
        async move {
            tokio::task::spawn_blocking(work)
                .await
                .unwrap_or(Err(Failure::Network))
        },
        Event::Authed,
    )
}

fn expiry(token: &TokenResponse) -> Instant {
    Instant::now()
        + Duration::from_secs(
            token
                .expires_in
                .unwrap_or(3600)
                .saturating_sub(60)
                .min(86400),
        )
}

fn exchange(request: &TokenRequest, cancelled: &AtomicBool) -> Result<TokenResponse, Failure> {
    let reply = http(
        "POST",
        request.endpoint(),
        None,
        Some((
            TokenRequest::CONTENT_TYPE,
            request.body().expose_secret().as_bytes(),
        )),
        cancelled,
    )?;
    let text = std::str::from_utf8(&reply).map_err(|_| Failure::InvalidReply)?;
    TokenResponse::parse(text).map_err(|_| Failure::Expired)
}

/// The artwork cache key of a Spotify item's picture.
#[must_use]
pub fn art_key(uri: &str) -> String {
    format!("spotify:{uri}")
}

/// The artwork cache key of the now-playing cover.
#[must_use]
pub fn cover_key(track: &Track) -> String {
    let uri = track
        .album
        .as_ref()
        .map_or(track.uri.as_str(), |a| a.uri.as_str());
    format!("spotify-cover:{uri}")
}

/// Where the Spotify login is kept in the desktop keyring.
fn login_key() -> CredentialKey {
    CredentialKey::new(crate::DESKTOP_APP_ID, "spotify", "login")
}

/// The keyring entry the old built-in player used.
fn old_playback_key() -> CredentialKey {
    CredentialKey::new(crate::DESKTOP_APP_ID, "spotify", "playback")
}

/// Run a keyring operation from a blocking worker.
fn keyring<T>(op: impl std::future::Future<Output = T>) -> T {
    tokio::runtime::Handle::current().block_on(op)
}

/// Save the Client ID and refresh token to the desktop keyring.
fn save_login(client_id: &str, refresh: &SecretText) {
    let value = json!({
        "client_id": client_id,
        "refresh_token": refresh.expose_secret(),
        "scopes": SCOPES.join(" "),
    });
    let outcome = keyring(async {
        connect_for_app(crate::DESKTOP_APP_ID)
            .await
            .set(login_key(), Secret::from_string(value.to_string()))
            .await
    });
    if !outcome.is_success() {
        eprintln!("music: Spotify login not saved: {}", outcome.user_summary());
    }
}

/// Remove the saved login from the desktop keyring.
fn forget_login() {
    let _ = keyring(async {
        let store = connect_for_app(crate::DESKTOP_APP_ID).await;
        store.delete(old_playback_key()).await;
        store.delete(login_key()).await
    });
}

/// Sign back in from the saved login: refresh its token, keep a rotated
/// one. A login Spotify no longer honours is forgotten.
fn restore_login() -> Result<Auth, Failure> {
    let outcome = keyring(async {
        connect_for_app(crate::DESKTOP_APP_ID)
            .await
            .get(login_key())
            .await
    });
    // The librespot player's login is no longer used.
    let _ = keyring(async {
        connect_for_app(crate::DESKTOP_APP_ID)
            .await
            .delete(old_playback_key())
            .await
    });
    let CredentialOutcome::Retrieved(secret) = outcome else {
        return Ok(Auth::NotRestored);
    };
    let Ok(saved) = serde_json::from_slice::<Value>(secret.expose_secret()) else {
        return Ok(Auth::NotRestored);
    };
    let (Some(client_id), Some(refresh)) =
        (saved["client_id"].as_str(), saved["refresh_token"].as_str())
    else {
        return Ok(Auth::NotRestored);
    };
    let client_id = client_id.to_owned();
    // A refreshed token keeps the scopes first approved.
    if saved["scopes"].as_str() != Some(SCOPES.join(" ").as_str()) {
        return Ok(Auth::Renew(client_id));
    }
    let refresh = SecretText::new(refresh.to_owned());
    let client = ClientConfig::new(AUTHORIZE, TOKEN, client_id.as_str())
        .and_then(|c| c.with_scopes(SCOPES))
        .map_err(|_| Failure::SignIn)?;
    let cancelled = AtomicBool::new(false);
    let mut token = match exchange(&client.refresh_request(&refresh, &[]), &cancelled) {
        Ok(token) => token,
        Err(Failure::Network) => return Err(Failure::Network),
        Err(_) => {
            forget_login();
            return Ok(Auth::NotRestored);
        }
    };
    match &token.refresh_token {
        Some(rotated) => save_login(&client_id, rotated),
        None => token.refresh_token = Some(refresh),
    }
    let expires = expiry(&token);
    Ok(Auth::Restored(
        Arc::new(Mutex::new(Session {
            client,
            client_id: client_id.clone(),
            token,
            expires,
        })),
        client_id,
    ))
}

/// The first 32-character hexadecimal run in `text`: a Client ID copied
/// along with a label or spaces.
fn client_id_in(text: &str) -> Option<String> {
    text.split(|c: char| !c.is_ascii_hexdigit())
        .find(|run| run.len() == 32)
        .map(str::to_ascii_lowercase)
}

fn open_browser(url: &str, cancelled: &AtomicBool) -> Result<(), Failure> {
    use enzui_core::portal::open_uri::{OpenUri, OpenUriRequest};
    use enzui_core::portal::{PortalOutcome, PortalParent};
    let mut backend =
        enzui_core::portal::dbus_backend::DbusBackend::new().map_err(|_| Failure::SignIn)?;
    let uri = OpenUri::parse(url).map_err(|_| Failure::SignIn)?;
    let mut pending = OpenUriRequest::new(uri, PortalParent::None)
        .start(&mut backend, |_| false)
        .map_err(|_| Failure::SignIn)?;
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if cancelled.load(Ordering::Acquire) {
            return Err(Failure::Cancelled);
        }
        if let Some(result) = pending.poll(&mut backend) {
            return if matches!(result, PortalOutcome::Completed(())) {
                Ok(())
            } else {
                Err(Failure::SignIn)
            };
        }
        if Instant::now() >= deadline {
            return Err(Failure::SignIn);
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

fn api(
    root: &str,
    method: &str,
    path: &str,
    body: Option<Value>,
    token: &SecretText,
    cancelled: &AtomicBool,
) -> Result<Value, Failure> {
    if !path.starts_with('/') || path.starts_with("//") || path.contains(['\r', '\n', '#']) {
        return Err(Failure::InvalidReply);
    }
    let body = body.map(|value| value.to_string());
    // Spotify wants a length on bodiless PUT/POST.
    let body = body.or_else(|| (method != "GET" && method != "DELETE").then(String::new));
    let bytes = http(
        method,
        &format!("{root}{path}"),
        Some(token),
        body.as_deref().map(|s| ("application/json", s.as_bytes())),
        cancelled,
    )
    .map_err(|error| {
        if error == Failure::NotFound && path.starts_with("/me/player") {
            Failure::NoDevice
        } else {
            error
        }
    })?;
    if bytes.is_empty() {
        Ok(Value::Null)
    } else {
        // Some commands answer with a bare id rather than JSON.
        Ok(serde_json::from_slice(&bytes).unwrap_or(Value::Null))
    }
}

fn http(
    method: &str,
    url: &str,
    token: Option<&SecretText>,
    body: Option<(&str, &[u8])>,
    cancelled: &AtomicBool,
) -> Result<Vec<u8>, Failure> {
    if cancelled.load(Ordering::Acquire) {
        return Err(Failure::Cancelled);
    }
    let mut easy = Easy::new();
    let mut bytes = Vec::new();
    let mut retry = 60;
    easy.url(url).map_err(|_| Failure::Network)?;
    easy.follow_location(false).map_err(|_| Failure::Network)?;
    easy.connect_timeout(Duration::from_secs(5))
        .map_err(|_| Failure::Network)?;
    easy.timeout(Duration::from_secs(15))
        .map_err(|_| Failure::Network)?;
    easy.useragent("enzUI-Music/0.2")
        .map_err(|_| Failure::Network)?;
    easy.custom_request(method).map_err(|_| Failure::Network)?;
    easy.progress(true).map_err(|_| Failure::Network)?;
    let mut headers = Headers::new();
    if let Some(token) = token {
        headers
            .append(&format!("Authorization: Bearer {}", token.expose_secret()))
            .map_err(|_| Failure::Expired)?;
    }
    if let Some((kind, data)) = body {
        headers
            .append(&format!("Content-Type: {kind}"))
            .map_err(|_| Failure::Network)?;
        easy.post_fields_copy(data).map_err(|_| Failure::Network)?;
    }
    easy.http_headers(headers).map_err(|_| Failure::Network)?;
    {
        let mut transfer = easy.transfer();
        transfer
            .write_function(|data| {
                if bytes.len().saturating_add(data.len()) > MAX_REPLY {
                    return Ok(0);
                }
                bytes.extend_from_slice(data);
                Ok(data.len())
            })
            .map_err(|_| Failure::Network)?;
        transfer
            .header_function(|line| {
                if let Ok(line) = std::str::from_utf8(line)
                    && let Some((name, value)) = line.split_once(':')
                    && name.eq_ignore_ascii_case("retry-after")
                {
                    retry = value.trim().parse::<u64>().unwrap_or(60).clamp(1, 86400);
                }
                true
            })
            .map_err(|_| Failure::Network)?;
        transfer
            .progress_function(|_, _, _, _| !cancelled.load(Ordering::Acquire))
            .map_err(|_| Failure::Network)?;
        transfer.perform().map_err(|_| {
            if cancelled.load(Ordering::Acquire) {
                Failure::Cancelled
            } else {
                Failure::Network
            }
        })?;
    }
    let status = easy.response_code().map_err(|_| Failure::Network)?;
    if (200..300).contains(&status) {
        return Ok(bytes);
    }
    // Spotify says why in the body; keep it for the page and the log.
    let reply = serde_json::from_slice::<Value>(&bytes).ok();
    let reason = reply
        .as_ref()
        .and_then(|v| {
            v["error"]["message"]
                .as_str()
                .or_else(|| v["error_description"].as_str())
                .or_else(|| v["error"].as_str())
                .map(str::to_owned)
        })
        .unwrap_or_else(|| String::from_utf8_lossy(&bytes).chars().take(200).collect());
    let code = reply
        .as_ref()
        .and_then(|v| {
            v["error"]["reason"]
                .as_str()
                .or_else(|| v["reason"].as_str())
        })
        .unwrap_or_default()
        .to_owned();
    eprintln!(
        "music: Spotify answered {status} to {method} {}: {reason} {code}",
        redact_query(url)
    );
    match status {
        401 => Err(Failure::Expired),
        403 | 400 => {
            if let Ok(mut last) = LAST_REFUSAL.lock() {
                *last = Some(format!("{reason} {code}").trim().to_owned());
            }
            Err(if status == 403 {
                Failure::Forbidden
            } else {
                Failure::Rejected
            })
        }
        404 if code == "NO_ACTIVE_DEVICE" => Err(Failure::NoDevice),
        404 => Err(Failure::NotFound),
        429 => Err(if code == "QUOTA_EXCEEDED" {
            Failure::Quota
        } else {
            Failure::Busy(Duration::from_secs(retry))
        }),
        _ => Err(Failure::Network),
    }
}

/// Spotify's reason for the last refused request.
static LAST_REFUSAL: Mutex<Option<String>> = Mutex::new(None);

/// A request URL for the log: its path, without the query.
fn redact_query(url: &str) -> &str {
    url.split('?').next().unwrap_or(url)
}

/// What to tell the user about Spotify's last refusal: plain guidance for
/// the reasons a personal app meets, Spotify's own words otherwise.
fn refusal_advice() -> Option<String> {
    let reason = LAST_REFUSAL.lock().ok()?.clone()?;
    let lower = reason.to_ascii_lowercase();
    Some(
        if lower.contains("not be registered") || lower.contains("not registered") {
            "Spotify doesn't list your account as a user of this app. In the developer dashboard, \
         open the app's settings, then User Management, and add your Spotify email."
                .to_owned()
        } else if lower.contains("premium") {
            "Spotify requires Premium for this.".to_owned()
        } else if lower.contains("restriction") {
            "Spotify doesn't allow that right now on this device.".to_owned()
        } else if lower.contains("scope") {
            "Music needs more access for this. Disconnect Spotify and sign in again.".to_owned()
        } else {
            format!("Spotify refused this: {reason}")
        },
    )
}

fn spotify_id<'a>(uri: &'a str, kind: &str) -> Option<&'a str> {
    let id = uri
        .strip_prefix("spotify:")?
        .strip_prefix(kind)?
        .strip_prefix(':')?;
    (id.len() == 22 && id.bytes().all(|b| b.is_ascii_alphanumeric())).then_some(id)
}

fn encode(value: &str) -> String {
    let mut result = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
            result.push(char::from(byte));
        } else {
            use std::fmt::Write;
            let _ = write!(result, "%{byte:02X}");
        }
    }
    result
}

#[cfg(test)]
mod client_id_tests {
    use super::client_id_in;

    #[test]
    fn a_pasted_label_and_spaces_leave_just_the_id() {
        assert_eq!(
            client_id_in("Client ID:  0123456789ABCDEF0123456789abcdef \n").as_deref(),
            Some("0123456789abcdef0123456789abcdef")
        );
    }

    #[test]
    fn text_without_an_id_yields_none() {
        assert_eq!(client_id_in("not an id 1234"), None);
        assert_eq!(client_id_in(&"a".repeat(40)), None);
    }
}
