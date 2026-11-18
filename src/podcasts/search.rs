//! Keyless podcast discovery through the iTunes Search provider.
//!
//! Discovery uses Apple's public iTunes Search JSON API
//! (<https://itunes.apple.com/search>): no account, key or cookie. Tests drive
//! `parse_search_results` with canned bodies and perform no DNS.
//!
//! The fetch runs on the bounded blocking worker shape shared with
//! [`crate::podcasts::PodcastService`] and
//! [`crate::radio_catalog::RadioCatalogService`]: one audited [`Transport`],
//! one [`UrlPolicy`], with generation-stamped request/response types.
//!
//! Privacy matches the RSS transport:
//!
//! * Every URL is validated by the shared [`UrlPolicy`] before DNS
//!   (`allow_http = false`, `allow_private_network = false`,
//!   `max_redirects = 4`).
//! * Each result's `feedUrl` passes the same pre-DNS check (scheme,
//!   credentials, literal IP); a hostname resolving to a private address is
//!   screened later by the RSS and artwork transport, as for a hand-typed
//!   feed URL.
//!
//! Bounds: `MAX_SEARCH_RESULTS` rows, `MAX_SEARCH_BYTES` body, and
//! [`crate::podcasts::MAX_META_CHARS`] per text field. A reply above the byte
//! cap is refused as malformed.

use enzui_core::cancellation::CancelCause;
use enzui_core::url_policy::{TransportLimits, UrlPolicy, UrlVerdict};
use enzui_resources::{CancelSignal, RemoteRequest, Transport, TransportError, TransportResponse};
use std::collections::HashMap;
use std::str::FromStr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError};
use std::sync::{Arc, Mutex};

use crate::fetch_reaper::{CloseDebt, WorkerReaper};
use crate::podcasts::{MAX_META_CHARS, REMOTE_POLICY, default_transport};

/// The keyless iTunes Search endpoint template. `{query}` is percent-encoded
/// before substitution. `media=podcast` restricts the reply to shows;
/// `limit` caps the reply at the model's ceiling.
pub const SEARCH_ENDPOINT_TEMPLATE: &str =
    "https://itunes.apple.com/search?media=podcast&entity=podcast&limit=25&term={query}";

/// Absolute ceiling on returned discovery rows regardless of the reply.
pub const MAX_SEARCH_RESULTS: usize = 25;

/// Absolute ceiling on the JSON body the transport accepts. Apple's `limit=25`
/// reply fits well below 512 KiB.
pub const MAX_SEARCH_BYTES: u64 = 512 * 1024;

/// Transport limits. Interactive: a search that hangs is worse than a Retry.
pub const SEARCH_LIMITS: TransportLimits = TransportLimits {
    max_bytes: MAX_SEARCH_BYTES,
    connect_timeout_ms: 5_000,
    read_timeout_ms: 10_000,
    total_timeout_ms: 15_000,
};

pub const SEARCH_WORKERS: usize = 1;
pub const SEARCH_QUEUE_CAPACITY: usize = 4;

// ---------------------------------------------------------------------------
// Result / request / response shapes
// ---------------------------------------------------------------------------

/// One discovery result — enough to render a row with artwork and to subscribe
/// from it. `feed_url` is the durable identity handed to the RSS transport;
/// `artwork_url` is `None` when the reply carried none (never fabricated).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PodcastSearchResult {
    pub title: String,
    pub author: String,
    pub feed_url: String,
    pub artwork_url: Option<String>,
    pub description: String,
}

/// One discovery fetch. Generation-stamped so a superseded reply is rejected.
#[derive(Clone, Debug)]
pub struct SearchRequest {
    pub generation: u64,
    pub endpoint_url: String,
    cancel: CancelSignal,
}

/// Completion delivered to the reducer.
#[derive(Clone, Debug)]
pub struct SearchResponse {
    pub generation: u64,
    pub result: Result<Vec<PodcastSearchResult>, SearchError>,
}

/// A typed, actionable discovery failure. `retryable` is `true` for network /
/// HTTP / timeout errors (Retry helps) and `false` for a URL-policy refusal or
/// schema failure (Retry cannot help until the endpoint changes).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchError {
    pub message: String,
    pub retryable: bool,
    pub kind: SearchErrorKind,
}

/// The failure category the reducer distinguishes when rendering the
/// unavailable or error state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchErrorKind {
    /// The endpoint URL string failed pre-DNS policy.
    Url,
    /// The resolved destination was blocked (private network, blocked range).
    Blocked,
    /// DNS resolution failed.
    Dns,
    /// The transport call itself failed (connect, TLS, read).
    Transport,
    /// The endpoint returned a non-2xx HTTP status.
    Http,
    /// The reply was not valid JSON or did not match the expected shape.
    Malformed,
    /// The reducer / caller cancelled the request.
    Cancelled,
}

// ---------------------------------------------------------------------------
// The service
// ---------------------------------------------------------------------------

/// The bounded blocking transport owner for discovery. One worker runs one
/// search at a time; a queue of 4 absorbs a retype/retry burst.
///
/// On close the worker retires through [`WorkerReaper`]: admission closes,
/// occupancy is held until every completion drains, and one off-reducer
/// reaper joins the worker.
pub struct PodcastSearchService {
    jobs: Option<SyncSender<SearchRequest>>,
    results: Receiver<SearchResponse>,
    active: Arc<Mutex<HashMap<u64, CancelSignal>>>,
    // UI active-row count for the poll wake; shutdown clears it while the reaper
    // holds the exact close charge.
    pending: AtomicUsize,
    reaper: WorkerReaper,
    unavailable: Option<String>,
}

impl std::fmt::Debug for PodcastSearchService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PodcastSearchService")
            .field("pending", &self.pending.load(Ordering::Acquire))
            .field("unavailable", &self.unavailable)
            .finish_non_exhaustive()
    }
}

impl PodcastSearchService {
    /// Shipping constructor. Reuses [`crate::podcasts::default_transport`] so
    /// discovery shares the one audited transport and its test seam.
    #[must_use]
    pub fn shipping() -> Self {
        Self::new(
            default_transport(),
            REMOTE_POLICY,
            SEARCH_WORKERS,
            SEARCH_QUEUE_CAPACITY,
        )
        .unwrap_or_else(Self::unavailable)
    }

    /// Explicit constructor for the loopback fixtures.
    pub fn new(
        transport: Arc<dyn Transport>,
        policy: UrlPolicy,
        workers: usize,
        queue_capacity: usize,
    ) -> Result<Self, String> {
        if workers == 0 {
            return Err("Podcast search worker pool is unavailable.".into());
        }
        let (job_tx, job_rx) = mpsc::sync_channel::<SearchRequest>(queue_capacity);
        let job_rx = Arc::new(Mutex::new(job_rx));
        // Capacity equals the admission cap, so each accepted request has a
        // result slot and a worker mid-send never blocks.
        let (result_tx, result_rx) = mpsc::sync_channel::<SearchResponse>(SEARCH_QUEUE_CAPACITY);
        let active: Arc<Mutex<HashMap<u64, CancelSignal>>> = Arc::new(Mutex::new(HashMap::new()));
        let mut handles = Vec::with_capacity(workers);

        for index in 0..workers {
            let jobs = job_rx.clone();
            let results = result_tx.clone();
            let transport = transport.clone();
            let builder = std::thread::Builder::new().name(format!("podcast-search-{index}"));
            match builder.spawn(move || {
                loop {
                    let job = match jobs.lock() {
                        Ok(rx) => rx.recv(),
                        Err(_) => return,
                    };
                    let Ok(request) = job else { return };
                    let result = fetch_with_transport(request, transport.as_ref(), policy);
                    if results.send(result).is_err() {
                        return;
                    }
                }
            }) {
                Ok(handle) => handles.push(handle),
                Err(err) => {
                    drop(job_tx);
                    for h in handles {
                        let _ = h.join();
                    }
                    return Err(format!(
                        "Podcast search worker could not start: {}.",
                        err.kind()
                    ));
                }
            }
        }
        drop(result_tx);

        Ok(Self {
            jobs: Some(job_tx),
            results: result_rx,
            active,
            pending: AtomicUsize::new(0),
            reaper: WorkerReaper::new(handles, "podcast-search-reaper"),
            unavailable: None,
        })
    }

    fn unavailable(message: String) -> Self {
        let (_job_tx, job_rx) = mpsc::sync_channel::<SearchRequest>(0);
        let (_result_tx, result_rx) = mpsc::sync_channel::<SearchResponse>(1);
        drop(job_rx);
        Self {
            jobs: None,
            results: result_rx,
            active: Arc::new(Mutex::new(HashMap::new())),
            pending: AtomicUsize::new(0),
            reaper: WorkerReaper::settled("podcast-search-reaper"),
            unavailable: Some(message),
        }
    }

    /// Whether the service could not start its worker pool; the reducer renders
    /// "search is unavailable" rather than an endless spinner.
    #[must_use]
    pub fn unavailable_reason(&self) -> Option<&str> {
        self.unavailable.as_deref()
    }

    /// Submit a search. Returns an error string when the service is unavailable
    /// or the bounded queue is full; the reducer surfaces that as a retryable
    /// failure.
    pub fn submit(&self, request: SearchRequest) -> Result<(), String> {
        if let Some(message) = &self.unavailable {
            return Err(message.clone());
        }
        let generation = request.generation;
        let cancel = request.cancel.clone();
        if let Ok(mut active) = self.active.lock() {
            if active.len() >= SEARCH_QUEUE_CAPACITY {
                request.cancel.cancel(CancelCause::Cancelled);
                return Err("Podcast search refused: queue full. Wait and retry.".into());
            }
            active.insert(generation, cancel);
            self.pending.store(active.len(), Ordering::Release);
        } else {
            return Err("Podcast search worker pool is unavailable.".into());
        }
        let Some(jobs) = &self.jobs else {
            if let Ok(mut active) = self.active.lock() {
                active.remove(&generation);
                self.pending.store(active.len(), Ordering::Release);
            }
            request.cancel.cancel(CancelCause::Shutdown);
            return Err("Podcast search worker pool stopped.".into());
        };
        // Reserve the occupancy charge before enqueuing; the checked reservation
        // refuses without wrapping and touches no queue when it refuses.
        if self.reaper.try_note_accepted().is_err() {
            if let Ok(mut active) = self.active.lock() {
                active.remove(&generation);
                self.pending.store(active.len(), Ordering::Release);
            }
            request.cancel.cancel(CancelCause::Cancelled);
            return Err("Podcast search refused: worker accounting exhausted.".into());
        }
        match jobs.try_send(request) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(req)) => {
                self.reaper.note_retired();
                if let Ok(mut active) = self.active.lock() {
                    active.remove(&generation);
                    self.pending.store(active.len(), Ordering::Release);
                }
                req.cancel.cancel(CancelCause::Cancelled);
                Err("Podcast search refused: queue full. Wait and retry.".into())
            }
            Err(TrySendError::Disconnected(req)) => {
                self.reaper.note_retired();
                if let Ok(mut active) = self.active.lock() {
                    active.remove(&generation);
                    self.pending.store(active.len(), Ordering::Release);
                }
                req.cancel.cancel(CancelCause::Shutdown);
                Err("Podcast search worker pool stopped.".into())
            }
        }
    }

    /// Cancel any in-flight search older than `keep_generation`; the
    /// [`CancelSignal`] tells the worker's transport call to abort.
    pub fn cancel_older_than(&self, keep_generation: u64) {
        if let Ok(mut active) = self.active.lock() {
            active.retain(|generation, cancel| {
                let keep = *generation >= keep_generation;
                if !keep {
                    cancel.cancel(CancelCause::Cancelled);
                }
                keep
            });
            self.pending.store(active.len(), Ordering::Release);
        }
    }

    #[must_use]
    pub fn pending_count(&self) -> usize {
        self.pending.load(Ordering::Acquire)
    }

    /// Drain all completed responses non-blockingly. The reducer polls this on
    /// its search-poll wake and applies each response.
    pub fn drain(&self) -> Vec<SearchResponse> {
        let mut ready = Vec::new();
        loop {
            match self.results.try_recv() {
                Ok(response) => {
                    if let Ok(mut active) = self.active.lock() {
                        active.remove(&response.generation);
                        self.pending.store(active.len(), Ordering::Release);
                    }
                    self.reaper.note_retired();
                    ready.push(response);
                }
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => return ready,
            }
        }
    }

    /// The remaining retirement debt; close is authorized when it settles.
    #[must_use]
    pub fn close_debt(&self) -> CloseDebt {
        self.reaper.close_debt()
    }

    /// Advance retirement without blocking and return the remaining
    /// [`CloseDebt`]. The reducer pumps this from the poll wake while separately
    /// draining completions.
    pub fn shutdown_progress(&mut self) -> CloseDebt {
        self.reaper.shutdown_progress()
    }

    /// Begin retirement: close admission, cancel in-flight and queued searches,
    /// then launch the one reaper. Idempotent and never blocks on a held
    /// transport.
    pub fn shutdown(&mut self) {
        self.signal_close();
        self.reaper.launch_reaper();
    }

    // Shared by shutdown and Drop: close admission and cancel active work; no
    // join, no reaper spawn.
    fn signal_close(&mut self) {
        if let Ok(mut active) = self.active.lock() {
            for (_, cancel) in active.drain() {
                cancel.cancel(CancelCause::Shutdown);
            }
            self.pending.store(0, Ordering::Release);
        }
        self.jobs.take();
        self.reaper.begin_close();
    }
}

impl Drop for PodcastSearchService {
    // Never joins: a stalled transport must not block the dropping thread.
    // Retirement completes through `shutdown_progress` on the reducer.
    fn drop(&mut self) {
        self.signal_close();
    }
}

// ---------------------------------------------------------------------------
// Request builder
// ---------------------------------------------------------------------------

/// Build a generation-stamped search request with the query percent-encoded
/// into the endpoint. The reducer bumps `generation` monotonically so a
/// superseded reply is rejected.
#[must_use]
pub fn build_request(generation: u64, query: &str, endpoint_template: &str) -> SearchRequest {
    let endpoint_url =
        endpoint_template.replace("{query}", &crate::radio_catalog::percent_encode(query));
    SearchRequest {
        generation,
        endpoint_url,
        cancel: CancelSignal::new(),
    }
}

// ---------------------------------------------------------------------------
// Transport-driven fetch (worker side)
// ---------------------------------------------------------------------------

/// Execute one search through the injected transport. Same shape as
/// [`crate::podcasts::fetch_with_transport`], adapted to the search types.
pub fn fetch_with_transport(
    request: SearchRequest,
    transport: &dyn Transport,
    policy: UrlPolicy,
) -> SearchResponse {
    let generation = request.generation;
    let result = fetch_body(&request, transport, policy).and_then(|body| {
        parse_search_results(&body, &policy).map_err(|message| SearchError {
            message,
            retryable: false,
            kind: SearchErrorKind::Malformed,
        })
    });
    SearchResponse { generation, result }
}

fn fetch_body(
    request: &SearchRequest,
    transport: &dyn Transport,
    policy: UrlPolicy,
) -> Result<Vec<u8>, SearchError> {
    if request.cancel.is_cancelled() {
        return Err(SearchError {
            message: "Podcast search was cancelled.".into(),
            retryable: true,
            kind: SearchErrorKind::Cancelled,
        });
    }
    let remote = RemoteRequest::checked(&request.endpoint_url, &policy).map_err(url_error)?;
    let response = transport
        .fetch(
            &remote,
            &policy,
            &SEARCH_LIMITS,
            &request.cancel,
            &mut |_| {},
        )
        .map_err(transport_error)?;
    validate_response(response)
}

fn validate_response(response: TransportResponse) -> Result<Vec<u8>, SearchError> {
    if !(200..300).contains(&response.status) {
        return Err(SearchError {
            message: format!(
                "Podcast search server returned HTTP {}. Retry may help.",
                response.status
            ),
            retryable: true,
            kind: SearchErrorKind::Http,
        });
    }
    if response.body.len() as u64 > MAX_SEARCH_BYTES {
        return Err(SearchError {
            message: format!(
                "Podcast search reply exceeds the {} KiB limit.",
                MAX_SEARCH_BYTES / 1024
            ),
            retryable: false,
            kind: SearchErrorKind::Malformed,
        });
    }
    Ok(response.body)
}

fn url_error(verdict: UrlVerdict) -> SearchError {
    SearchError {
        message: format!(
            "Podcast search URL refused by policy: {}.",
            verdict.reason()
        ),
        retryable: false,
        kind: SearchErrorKind::Url,
    }
}

fn transport_error(err: TransportError) -> SearchError {
    match err {
        TransportError::Url(v) => url_error(v),
        TransportError::AddressBlocked(_) => SearchError {
            message: "Podcast search destination is a blocked address.".into(),
            retryable: false,
            kind: SearchErrorKind::Blocked,
        },
        TransportError::Dns => SearchError {
            message: "Could not resolve the podcast search host.".into(),
            retryable: true,
            kind: SearchErrorKind::Dns,
        },
        TransportError::Cancelled(_) => SearchError {
            message: "Podcast search was cancelled.".into(),
            retryable: true,
            kind: SearchErrorKind::Cancelled,
        },
        other => SearchError {
            message: format!("Podcast search transport failed: {other}."),
            retryable: true,
            kind: SearchErrorKind::Transport,
        },
    }
}

// ---------------------------------------------------------------------------
// Parser
// ---------------------------------------------------------------------------

/// Parse an iTunes Search podcast reply into a bounded list of
/// [`PodcastSearchResult`]s.
///
/// * Truncates at [`MAX_SEARCH_RESULTS`] rows.
/// * Drops rows without a usable `feedUrl`, or whose `feedUrl` fails the
///   shared [`UrlPolicy`] pre-DNS admission (plain `http`, credentials, or a
///   literal private-network IP).
/// * De-duplicates by `feedUrl`, first wins.
/// * Collapses whitespace and caps every text field at [`MAX_META_CHARS`];
///   the artwork URL is likewise bounded.
pub fn parse_search_results(
    body: &[u8],
    policy: &UrlPolicy,
) -> Result<Vec<PodcastSearchResult>, String> {
    let value: serde_json::Value = serde_json::from_slice(body)
        .map_err(|e| format!("Podcast search reply was not valid JSON: {e}"))?;
    let array = value
        .get("results")
        .and_then(|v| v.as_array())
        .ok_or_else(|| "Podcast search reply carried no `results` array.".to_string())?;
    let mut out: Vec<PodcastSearchResult> = Vec::new();
    let mut seen: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for entry in array.iter().take(MAX_SEARCH_RESULTS) {
        let feed_url = entry
            .get("feedUrl")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty());
        let Some(feed_url) = feed_url else { continue };
        if !feed_url_admitted(feed_url, policy) {
            continue;
        }
        if !seen.insert(feed_url.to_string()) {
            continue;
        }
        let title = entry
            .get("collectionName")
            .or_else(|| entry.get("trackName"))
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(bounded_text)
            // A show with no name is still subscribable; fall back to its host.
            .unwrap_or_else(|| host_label(feed_url));
        let author = entry
            .get("artistName")
            .and_then(|v| v.as_str())
            .map(bounded_text)
            .unwrap_or_default();
        let artwork_url = entry
            .get("artworkUrl600")
            .or_else(|| entry.get("artworkUrl100"))
            .or_else(|| entry.get("artworkUrl60"))
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(bounded_text);
        let description = synthesize_description(entry);
        out.push(PodcastSearchResult {
            title,
            author,
            feed_url: feed_url.to_string(),
            artwork_url,
            description,
        });
    }
    Ok(out)
}

/// Pre-DNS admission for a discovery result's `feedUrl`: scheme and
/// credentials plus a literal-IP check, since the transport's post-DNS
/// `check_address` never runs on a literal.
fn feed_url_admitted(url: &str, policy: &UrlPolicy) -> bool {
    let Ok(safe) = policy.check_url(url) else {
        return false;
    };
    if let Ok(addr) = std::net::IpAddr::from_str(safe.host()) {
        return policy.check_address(addr).is_allowed();
    }
    true
}

/// A stable, bounded label derived from a feed URL's host, used only when the
/// reply carried no show name.
fn host_label(feed_url: &str) -> String {
    policy_host(feed_url).unwrap_or_else(|| "Untitled podcast".to_string())
}

fn policy_host(url: &str) -> Option<String> {
    REMOTE_POLICY
        .check_url(url)
        .ok()
        .map(|safe| safe.host().to_string())
}

/// Compose a bounded description from `primaryGenreName` and `trackCount`,
/// since the reply carries no long-form description.
fn synthesize_description(entry: &serde_json::Value) -> String {
    let mut parts: Vec<String> = Vec::with_capacity(2);
    if let Some(genre) = entry
        .get("primaryGenreName")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        parts.push(genre.to_string());
    }
    if let Some(count) = entry.get("trackCount").and_then(|v| v.as_u64())
        && count > 0
    {
        parts.push(if count == 1 {
            "1 episode".to_string()
        } else {
            format!("{count} episodes")
        });
    }
    bounded_text(&parts.join(" · "))
}

fn bounded_text(value: &str) -> String {
    let collapsed = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= MAX_META_CHARS {
        return collapsed;
    }
    let mut bounded: String = collapsed.chars().take(MAX_META_CHARS - 1).collect();
    bounded.push('\u{2026}');
    bounded
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------
