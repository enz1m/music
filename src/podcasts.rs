//! Podcasts — bounded remote feed loading, RSS/Atom parsing, and OPML.
//!
//! Remote bytes travel through the framework's typed [`Transport`] seam. The
//! shipping implementation is libcurl, while tests inject either an in-memory
//! transport or a deadline-bounded loopback HTTP fixture. [`UrlPolicy`] runs
//! before DNS and the transport revalidates every resolved/connected address
//! and redirect. No feed body is logged or persisted.
//!
//! # Distinct "nothing" states
//!
//! Loading, no subscriptions, and a subscribed feed with no playable episodes
//! each carry a different label ([`podcast_library_empty_label`],
//! [`feed_content_empty_label`]), so loading never reads as broken and an empty
//! feed never reads as a lost subscription.

use enzui_core::cancellation::CancelCause;
use enzui_core::url_policy::{TransportLimits, UrlPolicy, UrlVerdict};
use enzui_resources::{CancelSignal, RemoteRequest, Transport, TransportError, TransportResponse};
use quick_xml::Reader;
use quick_xml::events::{BytesStart, Event};
use std::collections::HashMap;
use std::io::Read;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;

// ---------------------------------------------------------------------------
// Data model
// ---------------------------------------------------------------------------
#[path = "podcasts_parts/feed_ops.rs"]
mod feed_ops;
use feed_ops::actionable_error;
#[cfg(test)]
pub use feed_ops::parse_feed;
#[cfg(test)]
pub use feed_ops::parse_rss_feed;
pub use feed_ops::{
    MAX_META_CHARS, export_opml, fetch_with_transport, format_duration, import_opml,
};

/// A subscribed podcast feed.
///
/// `feed_url` is the durable identity: `PodcastLibrary::subscribe` collapses
/// feeds with the same URL, since a feed may change its `<title>` between
/// fetches.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PodcastFeed {
    pub feed_url: String,
    pub title: String,
    pub description: String,
    /// The channel author (`itunes:author`, else `managingEditor`); empty when
    /// the feed declared none.
    pub author: String,
    /// The channel website (`<link>` for RSS, `<link rel="alternate">` for
    /// Atom); empty when absent. Displayed only, never fetched.
    pub link: String,
    /// The channel artwork URL (`itunes:image href`, else the RSS
    /// `<image><url>`); `None` when the feed declared none. Fetched through
    /// [`artwork`].
    pub artwork_url: Option<String>,
    pub episodes: Vec<PodcastEpisode>,
    /// Where this subscription is in its fetch lifecycle.
    pub status: FeedStatus,
}

/// A subscription's fetch lifecycle; the row renders each state distinctly.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub enum FeedStatus {
    /// A `fetch` is in flight; the row shows [`feed_loading_phase_label`].
    /// The default, since every new feed is dispatched into a fetch.
    #[default]
    Loading,
    /// Fetched and parsed; the row shows [`format_episode_count`].
    Loaded,
    /// The fetch or parse failed; the row shows the message and a Retry control.
    Failed(String),
}

/// A single episode from a feed. `audio_url` is `None` for entries without an
/// audio `<enclosure>`; those are not playable.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PodcastEpisode {
    pub guid: String,
    pub title: String,
    pub audio_url: Option<String>,
    /// The raw published-date string (`pubDate` for RSS, `published`/`updated`
    /// for Atom), kept verbatim and bounded rather than parsed. Empty when absent.
    pub published: String,
    /// The declared episode duration in whole seconds (`itunes:duration`),
    /// accepting `SS`, `MM:SS` and `HH:MM:SS`. `None` when absent or unparsable.
    pub duration_secs: Option<u64>,
    /// The bounded plain-text episode description / show notes
    /// (`description`, else `itunes:summary`, else Atom `summary`). Empty when
    /// absent.
    pub description: String,
    /// The per-episode artwork URL (`itunes:image href`). `None` when absent;
    /// the channel artwork is the render-time fallback.
    pub artwork_url: Option<String>,
}

impl PodcastFeed {
    /// Episodes that carry a playable audio enclosure, the count behind the
    /// `N episodes` chrome. Enclosure-less entries are not counted.
    pub fn playable_episodes(&self) -> impl Iterator<Item = &PodcastEpisode> {
        self.episodes.iter().filter(|e| e.audio_url.is_some())
    }

    pub fn playable_count(&self) -> usize {
        self.playable_episodes().count()
    }
}

// ---------------------------------------------------------------------------
// The three "nothing" states
// ---------------------------------------------------------------------------

/// Which podcast empty state is showing; each variant has a distinct label.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PodcastLibraryState {
    /// The user has subscribed to nothing. Label: `"No subscriptions yet"`.
    NoSubscriptions,
    /// Feeds panel: the sidebar list of RSS feeds is empty. Label: `"No feeds
    /// yet. Paste an RSS URL above to subscribe."`.
    ///
    /// Never constructed: the app renders one subscriptions list, so only
    /// `NoSubscriptions` is reachable.
    #[allow(dead_code)]
    NoFeeds,
}

/// Which loading phase a feed fetch is in.
///
/// Its labels differ from the empty-content strings so a loading feed never
/// reads as broken.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FeedLoadingPhase {
    /// The row shows this while `status` is [`FeedStatus::Loading`].
    Loading,
    /// The network-layer sub-status. The UI keeps one stable Loading row rather
    /// than repainting on every transport progress chunk, so this is not shown.
    #[allow(dead_code)]
    Fetching,
}

/// The label for a library-level empty state, one string per variant.
pub fn podcast_library_empty_label(state: PodcastLibraryState) -> &'static str {
    match state {
        PodcastLibraryState::NoSubscriptions => "No subscriptions yet",
        PodcastLibraryState::NoFeeds => "No feeds yet. Paste an RSS URL above to subscribe.",
    }
}

/// The label for a feed that loaded but has nothing playable: concise chrome
/// (`"No episodes"`) without a description, otherwise the hero sentence.
#[allow(dead_code)] // Read only by the empty-state distinctness tests.
pub fn feed_content_empty_label(with_description: bool) -> &'static str {
    if with_description {
        "This feed didn’t return any playable audio episodes."
    } else {
        "No episodes"
    }
}

/// The label for a feed-loading phase.
pub fn feed_loading_phase_label(phase: FeedLoadingPhase) -> &'static str {
    match phase {
        FeedLoadingPhase::Loading => "Loading feed…",
        FeedLoadingPhase::Fetching => "Fetching the RSS feed.",
    }
}

/// Search suggestions for podcast discovery.
pub const PODCAST_SEARCH_PLACEHOLDER: &str = "Try “Reply All”, “Radiolab”, “In Our Time”…";

/// The discovery status while its query is in flight.
pub const PODCAST_SEARCH_IN_FLIGHT_LABEL: &str = "Searching…";

// ---------------------------------------------------------------------------
// N-episodes formatter
// ---------------------------------------------------------------------------

/// The composed episode-count chrome: `"No episodes"` at zero, `"1 episode"`
/// at one, otherwise `"{n} episodes"`.
pub fn format_episode_count(n: usize) -> String {
    match n {
        0 => "No episodes".to_string(),
        1 => "1 episode".to_string(),
        n => format!("{n} episodes"),
    }
}

// ---------------------------------------------------------------------------
// RSS / Atom parsing and bounded transport
// ---------------------------------------------------------------------------

/// Long-running shows publish every episode in one feed: several megabytes
/// for a daily show with thousands of episodes.
pub const MAX_FEED_BYTES: u64 = 16 * 1024 * 1024;
pub const MAX_ERROR_CHARS: usize = 180;

pub const REMOTE_POLICY: UrlPolicy = UrlPolicy {
    allow_http: false,
    allow_private_network: false,
    max_redirects: 4,
};

pub const FEED_LIMITS: TransportLimits = TransportLimits {
    max_bytes: MAX_FEED_BYTES,
    connect_timeout_ms: 5_000,
    read_timeout_ms: 10_000,
    total_timeout_ms: 60_000,
};

/// Exactly two blocking workers execute transfers. Admission and OPML import
/// both stop at sixteen active requests; the queue can hold that full bounded
/// set even before a newly started worker has dequeued its first job.
pub const PODCAST_WORKERS: usize = 2;
pub const PODCAST_QUEUE_CAPACITY: usize = 16;
pub const MAX_ACTIVE_FETCHES: usize = 16;
pub const MAX_IMPORT_FEEDS: usize = MAX_ACTIVE_FETCHES;

/// Shipping transport, replaced by a fail-closed injected transport under
/// `cfg(test)` so an unrelated reducer/persistence test can never reach DNS.
pub fn default_transport() -> Arc<dyn Transport> {
    #[cfg(not(test))]
    {
        Arc::new(enzui_resources::curl_transport::CurlTransport::new())
    }
    #[cfg(test)]
    {
        Arc::new(TestTransport)
    }
}

#[cfg(test)]
#[derive(Debug)]
struct TestTransport;

#[cfg(test)]
impl Transport for TestTransport {
    fn fetch(
        &self,
        _request: &RemoteRequest,
        _policy: &UrlPolicy,
        _limits: &TransportLimits,
        _cancel: &CancelSignal,
        _progress: &mut dyn FnMut(enzui_resources::TransferProgress),
    ) -> Result<TransportResponse, TransportError> {
        Err(TransportError::Dns)
    }
}

struct FetchJob {
    request: FetchRequest,
}

/// Bounded owner of the explicitly blocking [`Transport`] seam.
///
/// Exactly `workers` named threads call [`Transport::fetch`]. The UI submits
/// with `try_send`, polls completions without blocking, and never polls a
/// libcurl call as an async future. The active-signal table lets shutdown abort
/// running and queued work; a single off-UI reaper, never the UI reducer, joins
/// the blocking workers after they eventually leave the transport.
///
/// The completion channel is a bounded [`SyncSender`] of capacity
/// [`MAX_ACTIVE_FETCHES`]: admission caps concurrently accepted requests at that
/// same limit and each accepted request produces exactly one completion, so a
/// worker mid-send always has a free slot and never blocks. Close keeps draining
/// until the channel is empty.
///
/// Close is retirement, not detachment: [`Self::shutdown`] closes admission and
/// cancels in-flight work, then [`Self::shutdown_progress`] must be pumped from
/// the close wake until [`Self::close_debt`] settles. Retirement owns the reaper
/// [`JoinHandle`] and joins it off the reducer; nothing is claimed settled until
/// every accepted request is retired and every worker and the reaper are joined.
pub struct PodcastService {
    jobs: Option<SyncSender<FetchJob>>,
    results: Receiver<FetchResult>,
    active: Arc<Mutex<HashMap<(String, u64), CancelSignal>>>,
    // UI active-row count for the cancellation display and poll wake. Shutdown
    // clears it even though accepted work is still retiring; `occupancy` carries
    // the exact close charge.
    pending: AtomicUsize,
    // Accepted requests not yet retired by a drained completion. Unlike
    // `pending` it survives shutdown, so close cannot settle while a queued or
    // running job still owes its result.
    occupancy: AtomicUsize,
    // A reaper thread-spawn failure leaves the handles in this shared cell so a
    // later non-blocking retry can still claim them; a lost spawn never detaches
    // a live worker.
    workers: Arc<Mutex<Vec<JoinHandle<()>>>>,
    // The reaper decrements this per join; a worker stuck inside the transport
    // holds it above zero, so the pool is never reported settled before exit.
    unjoined: Arc<AtomicUsize>,
    // Move-only retirement owner of the one reaper thread, joined off the reducer
    // once it finishes. `None` before launch, after a spawn failure, and after
    // the join.
    reaper: Option<JoinHandle<()>>,
    // Set by the first shutdown; keeps admission closed and bounds the pool to a
    // single reaper across every later close or drop.
    closing: bool,
    // Shutdown reached workers but the reaper thread could not be spawned; a
    // close-wake retry owns the next attempt.
    reaper_owed: bool,
    // The reaper thread was joined without unwinding: retirement is complete.
    reaper_joined: bool,
    // The reaper thread unwound on join: permanent, visible close debt.
    reaper_panicked: bool,
    unavailable: Option<String>,
}

/// Occupancy that must retire before the window may close.
///
/// [`Self::is_settled`] is the production close authorization: it is false while
/// any accepted request, unjoined worker, pending reaper join, or reaper panic
/// remains.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CloseDebt {
    /// Accepted requests whose completion has not been drained.
    pub accepted: usize,
    /// Worker threads the reaper has not yet joined.
    pub unjoined_workers: usize,
    /// A reaper join is still owed (spawn retry pending or thread not finished).
    pub reaper_pending: bool,
    /// The reaper thread unwound on join; retirement can never settle.
    pub reaper_panicked: bool,
}

impl CloseDebt {
    /// Whether nothing remains to retire: no accepted request, every worker and
    /// the reaper joined, and no reaper panic.
    #[must_use]
    pub fn is_settled(&self) -> bool {
        self.accepted == 0
            && self.unjoined_workers == 0
            && !self.reaper_pending
            && !self.reaper_panicked
    }
}

impl PodcastService {
    pub fn shipping() -> Self {
        Self::new(
            default_transport(),
            REMOTE_POLICY,
            PODCAST_WORKERS,
            PODCAST_QUEUE_CAPACITY,
        )
        .unwrap_or_else(Self::unavailable)
    }

    pub fn new(
        transport: Arc<dyn Transport>,
        policy: UrlPolicy,
        workers: usize,
        queue_capacity: usize,
    ) -> Result<Self, String> {
        if workers == 0 {
            return Err("Podcast worker pool is unavailable.".into());
        }
        let (job_tx, job_rx) = mpsc::sync_channel::<FetchJob>(queue_capacity);
        let job_rx = Arc::new(Mutex::new(job_rx));
        let (result_tx, result_rx) = mpsc::sync_channel::<FetchResult>(MAX_ACTIVE_FETCHES);
        let active = Arc::new(Mutex::new(HashMap::new()));
        let mut handles = Vec::with_capacity(workers);

        for index in 0..workers {
            let jobs = job_rx.clone();
            let results = result_tx.clone();
            let transport = transport.clone();
            let builder = std::thread::Builder::new().name(format!("podcast-fetch-{index}"));
            match builder.spawn(move || {
                loop {
                    let job = match jobs.lock() {
                        Ok(receiver) => receiver.recv(),
                        Err(_) => return,
                    };
                    let Ok(job) = job else { return };
                    let result = fetch_with_transport(job.request, transport.as_ref(), policy);
                    if results.send(result).is_err() {
                        return;
                    }
                }
            }) {
                Ok(handle) => handles.push(handle),
                Err(error) => {
                    drop(job_tx);
                    for handle in handles {
                        let _ = handle.join();
                    }
                    return Err(format!("Podcast worker could not start: {}.", error.kind()));
                }
            }
        }
        drop(result_tx);
        let unjoined = Arc::new(AtomicUsize::new(handles.len()));
        Ok(Self {
            jobs: Some(job_tx),
            results: result_rx,
            active,
            pending: AtomicUsize::new(0),
            occupancy: AtomicUsize::new(0),
            workers: Arc::new(Mutex::new(handles)),
            unjoined,
            reaper: None,
            closing: false,
            reaper_owed: false,
            reaper_joined: false,
            reaper_panicked: false,
            unavailable: None,
        })
    }

    fn unavailable(message: String) -> Self {
        let (_job_tx, job_rx) = mpsc::sync_channel::<FetchJob>(0);
        let (_result_tx, result_rx) = mpsc::channel();
        drop(job_rx);
        Self {
            jobs: None,
            results: result_rx,
            active: Arc::new(Mutex::new(HashMap::new())),
            pending: AtomicUsize::new(0),
            occupancy: AtomicUsize::new(0),
            workers: Arc::new(Mutex::new(Vec::new())),
            unjoined: Arc::new(AtomicUsize::new(0)),
            reaper: None,
            closing: false,
            reaper_owed: false,
            reaper_joined: true,
            reaper_panicked: false,
            unavailable: Some(actionable_error(message)),
        }
    }

    pub fn submit(&self, request: FetchRequest) -> Result<(), String> {
        if let Some(message) = &self.unavailable {
            return Err(message.clone());
        }
        let key = (request.url.clone(), request.generation);
        if let Ok(mut active) = self.active.lock() {
            if active.len() >= MAX_ACTIVE_FETCHES {
                request.cancel.cancel(CancelCause::Cancelled);
                return Err(format!(
                    "Too many podcast feeds are loading. Wait for one of the {MAX_ACTIVE_FETCHES} active requests and retry."
                ));
            }
            active.insert(key.clone(), request.cancel.clone());
            self.pending.store(active.len(), Ordering::Release);
        } else {
            return Err("Podcast worker pool is unavailable.".into());
        }
        let Some(jobs) = &self.jobs else {
            if let Ok(mut active) = self.active.lock() {
                active.remove(&key);
                self.pending.store(active.len(), Ordering::Release);
            }
            request.cancel.cancel(CancelCause::Shutdown);
            return Err("Podcast worker pool stopped. Retry after restarting the app.".into());
        };
        match jobs.try_send(FetchJob { request }) {
            Ok(()) => {
                self.occupancy.fetch_add(1, Ordering::Release);
                Ok(())
            }
            Err(TrySendError::Full(job)) => {
                if let Ok(mut active) = self.active.lock() {
                    active.remove(&key);
                    self.pending.store(active.len(), Ordering::Release);
                }
                job.request.cancel.cancel(CancelCause::Cancelled);
                Err(format!(
                    "Too many podcast feeds are loading. Wait for one of the {MAX_IMPORT_FEEDS} active requests and retry."
                ))
            }
            Err(TrySendError::Disconnected(job)) => {
                if let Ok(mut active) = self.active.lock() {
                    active.remove(&key);
                    self.pending.store(active.len(), Ordering::Release);
                }
                job.request.cancel.cancel(CancelCause::Shutdown);
                Err("Podcast worker pool stopped. Retry after restarting the app.".into())
            }
        }
    }

    /// Successfully submitted requests whose completion has not been drained.
    /// This lock-free count deliberately outlives app-level cancellation: the
    /// poll wake must remain installed until the stale worker result arrives
    /// and [`Self::drain`] clears its service-owned admission row.
    pub fn pending_count(&self) -> usize {
        self.pending.load(Ordering::Acquire)
    }

    pub fn drain(&self) -> Vec<FetchResult> {
        let mut ready = Vec::new();
        loop {
            match self.results.try_recv() {
                Ok(result) => {
                    if let Ok(mut active) = self.active.lock() {
                        active.remove(&(result.url.clone(), result.generation));
                        self.pending.store(active.len(), Ordering::Release);
                    }
                    let _ = self
                        .occupancy
                        .try_update(Ordering::Release, Ordering::Acquire, |v| {
                            Some(v.saturating_sub(1))
                        });
                    ready.push(result);
                }
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => return ready,
            }
        }
    }

    /// Exact close occupancy: accepted requests still owing a completion, worker
    /// threads the reaper has not joined, and whether the reaper join is still
    /// owed or panicked. Read-only; [`Self::shutdown_progress`] advances it.
    ///
    /// The worker-join and reaper fields are charged only once retirement has
    /// begun, so a live pool whose workers are simply idle reports no join debt.
    pub fn close_debt(&self) -> CloseDebt {
        CloseDebt {
            accepted: self.occupancy.load(Ordering::Acquire),
            unjoined_workers: if self.closing {
                self.unjoined.load(Ordering::Acquire)
            } else {
                0
            },
            reaper_pending: self.closing && !self.reaper_joined && !self.reaper_panicked,
            reaper_panicked: self.reaper_panicked,
        }
    }

    /// Begin retirement: close admission and cancel every in-flight and queued
    /// transfer, then launch the single reaper that joins the workers off the
    /// reducer. Never blocks on a held transport.
    ///
    /// Idempotent. A repeated close or drop keeps admission closed and never
    /// opens a second reaper. If the reaper thread cannot be spawned the handles
    /// stay retained for a non-blocking retry from [`Self::shutdown_progress`];
    /// they are never detached here.
    pub fn shutdown(&mut self) {
        self.signal_close();
        self.launch_reaper_via(spawn_reaper);
    }

    // Signal-only teardown shared by shutdown and Drop: close admission and
    // cancel active work. It performs no join and spawns no reaper.
    fn signal_close(&mut self) {
        self.closing = true;
        if let Ok(mut active) = self.active.lock() {
            for (_, cancel) in active.drain() {
                cancel.cancel(CancelCause::Shutdown);
            }
            self.pending.store(0, Ordering::Release);
        }
        self.jobs.take();
    }

    // Launch exactly one reaper. On a spawn failure the handles remain in the
    // shared cell and `reaper_owed` requests a later retry; the reaper is never
    // joined on this thread.
    fn launch_reaper_via(
        &mut self,
        spawn: impl FnOnce(
            &Arc<Mutex<Vec<JoinHandle<()>>>>,
            &Arc<AtomicUsize>,
        ) -> std::io::Result<JoinHandle<()>>,
    ) {
        if self.reaper.is_some() || self.reaper_joined || self.reaper_panicked {
            return;
        }
        let empty = self
            .workers
            .lock()
            .map(|guard| guard.is_empty())
            .unwrap_or(true);
        if empty {
            self.reaper_joined = true;
            self.reaper_owed = false;
            self.unjoined.store(0, Ordering::Release);
            return;
        }
        match spawn(&self.workers, &self.unjoined) {
            Ok(handle) => {
                self.reaper = Some(handle);
                self.reaper_owed = false;
            }
            Err(_) => self.reaper_owed = true,
        }
    }

    /// Advance retirement without blocking and return the remaining
    /// [`CloseDebt`]. Retries an owed reaper spawn and joins the reaper once it
    /// has finished. The reducer pumps this from the close wake and separately
    /// keeps [`Self::drain`]ing completions, which retires accepted occupancy
    /// and lets blocked workers send and exit until the channel is empty.
    pub fn shutdown_progress(&mut self) -> CloseDebt {
        if self.reaper_owed {
            self.launch_reaper_via(spawn_reaper);
        }
        if self.reaper.as_ref().is_some_and(JoinHandle::is_finished)
            && let Some(handle) = self.reaper.take()
        {
            match handle.join() {
                Ok(()) => self.reaper_joined = true,
                Err(_) => self.reaper_panicked = true,
            }
        }
        self.close_debt()
    }

    // Close admission and cancel active work with an injected failing spawn, so
    // a test can exercise the reaper-owed retry path deterministically.
    #[cfg(test)]
    pub fn shutdown_with_failed_reaper_spawn(&mut self) {
        self.signal_close();
        self.launch_reaper_via(|_, _| Err(std::io::Error::from(std::io::ErrorKind::WouldBlock)));
    }
}

// Spawn the reaper that takes the retained handles out of the shared cell and
// joins each exactly once. On a spawn failure the closure is dropped, but it
// captured only Arc clones of the cell and the counter, so no handle is
// detached.
fn spawn_reaper(
    workers: &Arc<Mutex<Vec<JoinHandle<()>>>>,
    unjoined: &Arc<AtomicUsize>,
) -> std::io::Result<JoinHandle<()>> {
    let reaper_workers = Arc::clone(workers);
    let reaper_unjoined = Arc::clone(unjoined);
    std::thread::Builder::new()
        .name("podcast-fetch-reaper".into())
        .spawn(move || {
            let handles = {
                let mut guard = reaper_workers
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner);
                std::mem::take(&mut *guard)
            };
            for worker in handles {
                let _ = worker.join();
                reaper_unjoined.fetch_sub(1, Ordering::Release);
            }
        })
}

impl Drop for PodcastService {
    // Signal only. A clean close retires through shutdown_progress on the
    // reducer; Drop never joins here, so the UI thread cannot stall on a held
    // transport. An owned reaper handle and its workers may detach at process
    // teardown by design.
    fn drop(&mut self) {
        self.signal_close();
    }
}

/// Identity and cancellation for one feed attempt.
#[derive(Clone, Debug)]
pub struct FetchRequest {
    pub url: String,
    pub generation: u64,
    cancel: CancelSignal,
}

/// Generation-stamped completion delivered to the reducer.
#[derive(Clone, Debug)]
pub struct FetchResult {
    pub url: String,
    pub generation: u64,
    pub result: Result<PodcastFeed, String>,
}

#[derive(Clone, Debug)]
struct InFlight {
    generation: u64,
    cancel: CancelSignal,
}

/// Application-owned request registry. Replacement, unsubscribe and shutdown
/// cancel the actual transfer; late completions are rejected by generation.
#[derive(Debug, Default)]
pub struct PodcastRequests {
    next_generation: u64,
    in_flight: HashMap<String, InFlight>,
}

impl PodcastRequests {
    pub fn begin(&mut self, url: String) -> FetchRequest {
        if let Some(previous) = self.in_flight.remove(&url) {
            previous.cancel.cancel(CancelCause::Cancelled);
        }
        self.next_generation = self.next_generation.saturating_add(1);
        let generation = self.next_generation;
        let cancel = CancelSignal::new();
        self.in_flight.insert(
            url.clone(),
            InFlight {
                generation,
                cancel: cancel.clone(),
            },
        );
        FetchRequest {
            url,
            generation,
            cancel,
        }
    }

    pub fn accept(&mut self, result: &FetchResult) -> bool {
        let current = self
            .in_flight
            .get(&result.url)
            .is_some_and(|flight| flight.generation == result.generation);
        if current {
            self.in_flight.remove(&result.url);
        }
        current
    }

    pub fn cancel(&mut self, url: &str, cause: CancelCause) {
        if let Some(flight) = self.in_flight.remove(url) {
            flight.cancel.cancel(cause);
        }
    }

    pub fn cancel_all(&mut self, cause: CancelCause) {
        for (_, flight) in self.in_flight.drain() {
            flight.cancel.cancel(cause);
        }
    }
}

impl Drop for PodcastRequests {
    fn drop(&mut self) {
        self.cancel_all(CancelCause::Shutdown);
    }
}

/// Show notes as a short plain line: tags dropped, entities decoded,
/// whitespace collapsed, cut at `max` characters.
#[must_use]
pub fn plain_summary(html: &str, max: usize) -> String {
    let mut text = String::with_capacity(html.len().min(max * 2));
    let mut inside = false;
    for c in html.chars() {
        match c {
            '<' => {
                inside = true;
                text.push(' ');
            }
            '>' => inside = false,
            c if !inside => text.push(c),
            _ => {}
        }
    }
    // Entities may arrive escaped twice (`&amp;aacute;`).
    let text = decode_entities(&decode_entities(&text));
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= max {
        return collapsed;
    }
    let cut: String = collapsed.chars().take(max).collect();
    let cut = cut
        .rsplit_once(' ')
        .map_or(cut.as_str(), |(head, _)| head)
        .to_owned();
    format!("{cut}\u{2026}")
}

/// Show notes as readable text for the episode details: paragraphs and
/// list items on their own lines, tags dropped, entities decoded.
#[must_use]
pub fn plain_paragraphs(html: &str) -> String {
    // Block ends become line breaks before the tags go.
    let mut marked = String::with_capacity(html.len());
    let lower = html.to_ascii_lowercase();
    let mut at = 0;
    while let Some(start) = lower[at..].find('<').map(|i| i + at) {
        marked.push_str(&html[at..start]);
        let end = lower[start..]
            .find('>')
            .map_or(lower.len(), |i| start + i + 1);
        let tag = &lower[start..end];
        if tag.starts_with("<br")
            || tag.starts_with("</p")
            || tag.starts_with("</div")
            || tag.starts_with("</h")
        {
            marked.push('\n');
        } else if tag.starts_with("<li") {
            marked.push_str("\n\u{2022} ");
        } else if tag.starts_with("<p") {
            marked.push('\n');
        }
        at = end;
    }
    marked.push_str(&html[at.min(html.len())..]);
    let text = decode_entities(&decode_entities(&marked));
    text.lines()
        .map(|line| line.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

/// An episode's published date in the desktop's time zone ("12 Nov 2026",
/// with the time "12 Nov 2026, 18:10"), or the feed's text as given when it
/// doesn't parse.
#[must_use]
pub fn local_date(published: &str, with_time: bool) -> String {
    match published_epoch(published).and_then(local_parts) {
        Some((year, month, day, hour, minute)) => {
            const MONTHS: [&str; 12] = [
                "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
            ];
            let month = MONTHS
                .get(month.saturating_sub(1) as usize)
                .copied()
                .unwrap_or("");
            if with_time {
                format!("{day} {month} {year}, {hour:02}:{minute:02}")
            } else {
                format!("{day} {month} {year}")
            }
        }
        None => published.trim().to_owned(),
    }
}

/// Seconds since 1970 of an RSS (RFC 2822) or Atom (RFC 3339) date.
fn published_epoch(text: &str) -> Option<i64> {
    let text = text.trim();
    if text.len() >= 10 && text.as_bytes()[4] == b'-' {
        // 2026-11-12T17:10:00Z, 2026-11-12T17:10:00.000+01:00, 2026-11-12
        let (date, rest) = text.split_at(10);
        let mut parts = date.split('-').map(|p| p.parse::<i64>().ok());
        let (year, month, day) = (parts.next()??, parts.next()??, parts.next()??);
        let rest = rest.trim_start_matches(['T', 't', ' ']);
        let (clock, zone) = match rest.find(['Z', 'z', '+', '-']) {
            Some(at) => rest.split_at(at),
            None => (rest, ""),
        };
        let mut clock = clock
            .split(':')
            .map(|p| p.split('.').next().unwrap_or("").parse::<i64>().ok());
        let hour = clock.next().flatten().unwrap_or(0);
        let minute = clock.next().flatten().unwrap_or(0);
        let second = clock.next().flatten().unwrap_or(0);
        let offset = zone_minutes(&zone.replace(':', ""))?;
        return Some(epoch(year, month, day, hour, minute, second, offset));
    }
    // Thu, 12 Nov 2026 17:10:00 +0000
    let text = text.split_once(',').map_or(text, |(_, rest)| rest);
    let mut fields = text.split_whitespace();
    let day: i64 = fields.next()?.parse().ok()?;
    let month = match fields.next()?.get(..3)?.to_ascii_lowercase().as_str() {
        "jan" => 1,
        "feb" => 2,
        "mar" => 3,
        "apr" => 4,
        "may" => 5,
        "jun" => 6,
        "jul" => 7,
        "aug" => 8,
        "sep" => 9,
        "oct" => 10,
        "nov" => 11,
        "dec" => 12,
        _ => return None,
    };
    let mut year: i64 = fields.next()?.parse().ok()?;
    if year < 100 {
        year += if year < 50 { 2000 } else { 1900 };
    }
    let mut clock = fields
        .next()
        .unwrap_or("00:00")
        .split(':')
        .map(|p| p.parse::<i64>().ok());
    let hour = clock.next().flatten().unwrap_or(0);
    let minute = clock.next().flatten().unwrap_or(0);
    let second = clock.next().flatten().unwrap_or(0);
    let offset = zone_minutes(fields.next().unwrap_or("GMT")).unwrap_or(0);
    Some(epoch(year, month, day, hour, minute, second, offset))
}

/// A zone's offset from UTC in minutes: `+0100`, `-05:00` (colon removed),
/// `Z`, `GMT` and the US abbreviations RSS feeds use.
fn zone_minutes(zone: &str) -> Option<i64> {
    let zone = zone.trim();
    if let Some(sign) = zone.chars().next().filter(|c| *c == '+' || *c == '-') {
        let digits = &zone[1..];
        if digits.len() < 4 || !digits.bytes().take(4).all(|b| b.is_ascii_digit()) {
            return None;
        }
        let minutes = digits[..2].parse::<i64>().ok()? * 60 + digits[2..4].parse::<i64>().ok()?;
        return Some(if sign == '-' { -minutes } else { minutes });
    }
    Some(match zone.to_ascii_uppercase().as_str() {
        "" | "Z" | "GMT" | "UT" | "UTC" => 0,
        "EST" => -300,
        "EDT" => -240,
        "CST" => -360,
        "CDT" => -300,
        "MST" => -420,
        "MDT" => -360,
        "PST" => -480,
        "PDT" => -420,
        _ => 0,
    })
}

fn epoch(
    year: i64,
    month: i64,
    day: i64,
    hour: i64,
    minute: i64,
    second: i64,
    offset_minutes: i64,
) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * ((month + 9) % 12) + 2) / 5 + day - 1;
    let days = era * 146_097 + yoe * 365 + yoe / 4 - yoe / 100 + doy - 719_468;
    days * 86_400 + hour * 3_600 + minute * 60 + second - offset_minutes * 60
}

/// Year, month, day, hour and minute of `epoch` in the desktop's time zone.
#[allow(unsafe_code, reason = "localtime_r reads the system time zone")]
fn local_parts(epoch: i64) -> Option<(i32, u32, u32, u32, u32)> {
    // `time_t` is narrower than `i64` on some platforms.
    #[allow(clippy::useless_conversion)]
    let time: libc::time_t = epoch.try_into().ok()?;
    // SAFETY: `localtime_r` writes only into the `tm` it is given.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    let converted = unsafe { libc::localtime_r(&time, &mut tm) };
    if converted.is_null() {
        return None;
    }
    Some((
        tm.tm_year + 1900,
        u32::try_from(tm.tm_mon + 1).ok()?,
        u32::try_from(tm.tm_mday).ok()?,
        u32::try_from(tm.tm_hour).ok()?,
        u32::try_from(tm.tm_min).ok()?,
    ))
}

/// Decode HTML character references: numeric ones and the named ones show
/// notes use (accented letters, quotes, dashes).
fn decode_entities(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        let tail = &rest[at..];
        let decoded = tail[1..]
            .find(';')
            .filter(|end| *end <= 10)
            .and_then(|end| {
                let name = &tail[1..=end];
                let c = if let Some(num) =
                    name.strip_prefix("#x").or_else(|| name.strip_prefix("#X"))
                {
                    u32::from_str_radix(num, 16).ok().and_then(char::from_u32)
                } else if let Some(num) = name.strip_prefix('#') {
                    num.parse::<u32>().ok().and_then(char::from_u32)
                } else {
                    named_entity(name)
                }?;
                Some((c, end + 2))
            });
        match decoded {
            Some((c, len)) => {
                out.push(c);
                rest = &tail[len..];
            }
            None => {
                out.push('&');
                rest = &tail[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

fn named_entity(name: &str) -> Option<char> {
    Some(match name {
        "amp" => '&',
        "lt" => '<',
        "gt" => '>',
        "quot" => '"',
        "apos" => '\'',
        "nbsp" => ' ',
        "ndash" => '\u{2013}',
        "mdash" => '\u{2014}',
        "hellip" => '\u{2026}',
        "lsquo" => '\u{2018}',
        "rsquo" => '\u{2019}',
        "sbquo" => '\u{201a}',
        "ldquo" => '\u{201c}',
        "rdquo" => '\u{201d}',
        "bdquo" => '\u{201e}',
        "laquo" => '\u{ab}',
        "raquo" => '\u{bb}',
        "copy" => '\u{a9}',
        "reg" => '\u{ae}',
        "trade" => '\u{2122}',
        "deg" => '\u{b0}',
        "euro" => '\u{20ac}',
        "szlig" => '\u{df}',
        "odblac" => '\u{151}',
        "Odblac" => '\u{150}',
        "udblac" => '\u{171}',
        "Udblac" => '\u{170}',
        _ => return accented(name),
    })
}

/// `aacute`, `Ouml`, `egrave`, `ccedil`, `ntilde` and the like.
fn accented(name: &str) -> Option<char> {
    let mut chars = name.chars();
    let base = chars.next()?;
    let mark = chars.as_str();
    let table: &[(&str, &str, &str)] = &[
        ("acute", "aeiouyAEIOUY", "áéíóúýÁÉÍÓÚÝ"),
        ("grave", "aeiouAEIOU", "àèìòùÀÈÌÒÙ"),
        ("uml", "aeiouyAEIOU", "äëïöüÿÄËÏÖÜ"),
        ("circ", "aeiouAEIOU", "âêîôûÂÊÎÔÛ"),
        ("tilde", "anoANO", "ãñõÃÑÕ"),
        ("cedil", "cC", "çÇ"),
        ("ring", "aA", "åÅ"),
        ("slash", "oO", "øØ"),
        ("caron", "csznrCSZNR", "čšžňřČŠŽŇŘ"),
    ];
    let (_, from, to) = table.iter().find(|(m, _, _)| *m == mark)?;
    let index = from.chars().position(|c| c == base)?;
    to.chars().nth(index)
}

#[cfg(test)]
mod summary_tests {
    use super::{plain_paragraphs, plain_summary, published_epoch};

    #[test]
    fn dates_read_from_rss_and_atom_feeds() {
        let utc = published_epoch("Thu, 12 Nov 2026 17:10:00 GMT").unwrap();
        assert_eq!(
            published_epoch("Thu, 12 Nov 2026 17:10:00 +0000"),
            Some(utc)
        );
        assert_eq!(
            published_epoch("Thu, 12 Nov 2026 18:10:00 +0100"),
            Some(utc)
        );
        assert_eq!(published_epoch("Thu, 12 Nov 2026 12:10:00 EST"), Some(utc));
        assert_eq!(published_epoch("2026-11-12T17:10:00Z"), Some(utc));
        assert_eq!(published_epoch("2026-11-12T18:10:00.000+01:00"), Some(utc));
        assert_eq!(published_epoch("someday"), None);
    }

    #[test]
    fn show_notes_keep_their_paragraphs_for_the_details() {
        assert_eq!(
            plain_paragraphs("<p>First&nbsp;one</p><p>Second</p><ul><li>a</li><li>b</li></ul>"),
            "First one\nSecond\n\u{2022} a\n\u{2022} b"
        );
    }

    #[test]
    fn show_notes_become_a_short_plain_line() {
        assert_eq!(
            plain_summary("<p>Hello&nbsp;<b>world</b></p><p><br></p>&amp; more", 100),
            "Hello world & more"
        );
        assert_eq!(
            plain_summary(
                "csatorn&amp;aacute;ja &#337; &odblac;r&ouml;m &#x171; R&D &unknown;",
                100
            ),
            "csatornája ő őröm ű R&D &unknown;"
        );
        let long = plain_summary(&"word ".repeat(100), 20);
        assert!(long.ends_with('\u{2026}') && long.chars().count() <= 21);
    }
}

// ---------------------------------------------------------------------------
// Sub-modules
// ---------------------------------------------------------------------------

/// Policy-safe remote artwork fetch + decode-once cache, keyed by
/// feed/channel/episode identity.
pub mod artwork;
/// Apple's open top-podcast charts, by country and genre.
pub mod charts;
/// Keyless podcast discovery (the iTunes Search provider).
pub mod search;

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests;

#[cfg(test)]
impl PodcastRequests {
    /// The generation of the fetch in flight for `url`, to answer it.
    pub fn generation_of(&self, url: &str) -> Option<u64> {
        self.in_flight.get(url).map(|flight| flight.generation)
    }
}
