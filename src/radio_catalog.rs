//! The Radio Browser catalog client.
//!
//! The default endpoint is Radio Browser (<https://www.radio-browser.info/>), a
//! keyless public JSON API, fetched off-thread through [`RadioCatalogService`].
//! Tests drive [`parse_stations`] with fixtures and perform no DNS.
//!
//! Privacy:
//!
//! * No account, no key, no cookie: the service sends no credentials.
//! * Every URL is validated by the shared
//!   [`enzui_core::url_policy::UrlPolicy`] before DNS: `allow_http = false`,
//!   `allow_private_network = false`, `max_redirects = 4`. A URL that
//!   resolves to a private-network address is refused as
//!   `CatalogError::Blocked` and never becomes a station row.
//! * Every stream URL a catalog entry carries is re-validated by the
//!   same policy inside [`parse_stations`] before the row reaches the
//!   model; a station whose stream URL fails is dropped, not surfaced
//!   as a broken tune.
//!
//! There is no on-disk catalog cache; each boot fetches the top list once.
//! Favourites persist by UUID ([`crate::radio::RadioState::favorites`]) and are
//! reconciled by [`crate::radio::RadioState::reconcile_favorites_against_catalog`].

use enzui_core::cancellation::CancelCause;
use enzui_core::url_policy::{TransportLimits, UrlPolicy, UrlVerdict};
use enzui_resources::{CancelSignal, RemoteRequest, Transport, TransportError, TransportResponse};
use std::collections::HashMap;
use std::str::FromStr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError};
use std::sync::{Arc, Mutex};

use crate::fetch_reaper::{CloseDebt, WorkerReaper};
use crate::radio::{RadioStation, derive_local_id_from_uuid};

// ---------------------------------------------------------------------------
// Endpoints
// ---------------------------------------------------------------------------

/// Radio Browser's station search, keyless. [`REMOTE_POLICY`] still validates
/// every URL built on it before DNS.
pub const API_ROOT: &str = "https://de1.api.radio-browser.info/json/stations";

/// How many stations a browse list or search asks for: plenty, since many
/// stations stream only over plain HTTP, which the policy refuses.
pub const LIST_SIZE: usize = 100;

/// Absolute ceiling on returned rows regardless of the endpoint's reply.
pub const MAX_STATIONS_PER_REPLY: usize = 100;

/// Ceiling on the JSON body the transport accepts before truncating.
///
/// A hundred-station reply fits well below it.
pub const MAX_CATALOG_BYTES: u64 = 1024 * 1024;

/// The URL policy the transport enforces; deliberately identical to podcasts'.
pub const REMOTE_POLICY: UrlPolicy = UrlPolicy {
    allow_http: false,
    allow_private_network: false,
    max_redirects: 4,
};

/// Transport limits; short deadlines because a long tail at boot is worse than
/// a Retry.
pub const CATALOG_LIMITS: TransportLimits = TransportLimits {
    max_bytes: MAX_CATALOG_BYTES,
    connect_timeout_ms: 5_000,
    read_timeout_ms: 10_000,
    total_timeout_ms: 15_000,
};

pub const CATALOG_WORKERS: usize = 1;
pub const CATALOG_QUEUE_CAPACITY: usize = 4;

// ---------------------------------------------------------------------------
// Request / result shapes
// ---------------------------------------------------------------------------

/// What the reducer wants the service to fetch.
#[derive(Clone, Debug)]
pub enum CatalogRequestKind {
    /// A country's most listened stations, optionally of one genre tag.
    Browse {
        country: String,
        genre: Option<&'static str>,
    },
    /// A name search; the raw `query` is percent-encoded.
    Search { query: String },
    /// The user's starred stations, by UUID.
    ByUuid { uuids: Vec<String> },
}

impl CatalogRequestKind {
    #[must_use]
    pub fn tag(&self) -> &'static str {
        match self {
            CatalogRequestKind::Browse { .. } => "browse",
            CatalogRequestKind::Search { .. } => "search",
            CatalogRequestKind::ByUuid { .. } => "byuuid",
        }
    }
}

/// One catalog fetch. Generation-stamped for reject-stale.
#[derive(Clone, Debug)]
pub struct CatalogRequest {
    /// Bumped on every submit so a superseded reply is rejected.
    pub generation: u64,
    pub kind: CatalogRequestKind,
    /// The endpoint URL after search-query substitution.
    pub endpoint_url: String,
    cancel: CancelSignal,
}

impl CatalogRequest {}

/// Completion delivered to the reducer.
#[derive(Clone, Debug)]
pub struct CatalogResult {
    pub generation: u64,
    pub kind: CatalogRequestKind,
    pub result: Result<Vec<RadioStation>, CatalogError>,
}

/// A typed failure, mapped to `RadioCatalogState::Failed { message, retryable }`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogError {
    pub message: String,
    /// `true` for network / HTTP / timeout errors (Retry helps);
    /// `false` for URL-policy refusal or schema failure (Retry cannot
    /// help until the endpoint / config changes).
    pub retryable: bool,
    pub kind: CatalogErrorKind,
}

/// The failure category the reducer needs to distinguish.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CatalogErrorKind {
    /// The URL string failed pre-DNS policy.
    Url,
    /// The resolved destination was blocked by the address policy
    /// (private network, blocked range).
    Blocked,
    /// DNS resolution failed.
    Dns,
    /// The transport call itself failed (connect, TLS, read).
    Transport,
    /// The endpoint returned a non-2xx HTTP status.
    Http,
    /// The reply body was not valid JSON or did not match the shape
    /// [`parse_stations`] expects.
    Malformed,
    /// The reducer / caller cancelled the request.
    Cancelled,
}

// ---------------------------------------------------------------------------
// The service
// ---------------------------------------------------------------------------

/// The bounded blocking transport owner for catalog fetches.
///
/// The queue holds 4 requests, enough for retry or search bursts. On close the
/// workers retire through [`WorkerReaper`]: admission closes, occupancy is held
/// until every completion drains, and one off-reducer reaper joins the workers.
pub struct RadioCatalogService {
    jobs: Option<SyncSender<CatalogRequest>>,
    results: Receiver<CatalogResult>,
    active: Arc<Mutex<HashMap<u64, CancelSignal>>>,
    // UI active-row count for the poll wake; shutdown clears it while the reaper
    // holds the exact close charge.
    pending: AtomicUsize,
    reaper: WorkerReaper,
    unavailable: Option<String>,
}

impl std::fmt::Debug for RadioCatalogService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RadioCatalogService")
            .field("pending", &self.pending.load(Ordering::Acquire))
            .field("unavailable", &self.unavailable)
            .finish_non_exhaustive()
    }
}

impl RadioCatalogService {
    /// Shipping constructor, sharing [`crate::podcasts::default_transport`].
    #[must_use]
    pub fn shipping() -> Self {
        Self::new(
            crate::podcasts::default_transport(),
            REMOTE_POLICY,
            CATALOG_WORKERS,
            CATALOG_QUEUE_CAPACITY,
        )
        .unwrap_or_else(Self::unavailable)
    }

    /// Constructor with an injected transport, used by loopback fixtures.
    pub fn new(
        transport: Arc<dyn Transport>,
        policy: UrlPolicy,
        workers: usize,
        queue_capacity: usize,
    ) -> Result<Self, String> {
        if workers == 0 {
            return Err("Radio catalog worker pool is unavailable.".into());
        }
        let (job_tx, job_rx) = mpsc::sync_channel::<CatalogRequest>(queue_capacity);
        let job_rx = Arc::new(Mutex::new(job_rx));
        // Capacity equals the admission cap, so each accepted request has a
        // result slot and a worker mid-send never blocks.
        let (result_tx, result_rx) = mpsc::sync_channel::<CatalogResult>(CATALOG_QUEUE_CAPACITY);
        let active: Arc<Mutex<HashMap<u64, CancelSignal>>> = Arc::new(Mutex::new(HashMap::new()));
        let mut handles = Vec::with_capacity(workers);

        for index in 0..workers {
            let jobs = job_rx.clone();
            let results = result_tx.clone();
            let transport = transport.clone();
            let builder = std::thread::Builder::new().name(format!("radio-catalog-{index}"));
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
                        "Radio catalog worker could not start: {}.",
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
            reaper: WorkerReaper::new(handles, "radio-catalog-reaper"),
            unavailable: None,
        })
    }

    fn unavailable(message: String) -> Self {
        let (_job_tx, job_rx) = mpsc::sync_channel::<CatalogRequest>(0);
        let (_result_tx, result_rx) = mpsc::sync_channel::<CatalogResult>(1);
        drop(job_rx);
        Self {
            jobs: None,
            results: result_rx,
            active: Arc::new(Mutex::new(HashMap::new())),
            pending: AtomicUsize::new(0),
            reaper: WorkerReaper::settled("radio-catalog-reaper"),
            unavailable: Some(message),
        }
    }

    /// Submit a request.
    ///
    /// Returns an error string when the service is closed or the bounded queue
    /// is full.
    pub fn submit(&self, request: CatalogRequest) -> Result<(), String> {
        if let Some(message) = &self.unavailable {
            return Err(message.clone());
        }
        let generation = request.generation;
        let cancel = request.cancel.clone();
        if let Ok(mut active) = self.active.lock() {
            if active.len() >= CATALOG_QUEUE_CAPACITY {
                request.cancel.cancel(CancelCause::Cancelled);
                return Err("Radio catalog request refused: queue full.".into());
            }
            active.insert(generation, cancel);
            self.pending.store(active.len(), Ordering::Release);
        } else {
            return Err("Radio catalog worker pool is unavailable.".into());
        }
        let Some(jobs) = &self.jobs else {
            if let Ok(mut active) = self.active.lock() {
                active.remove(&generation);
                self.pending.store(active.len(), Ordering::Release);
            }
            request.cancel.cancel(CancelCause::Shutdown);
            return Err("Radio catalog worker pool stopped.".into());
        };
        // Reserve the occupancy charge before enqueuing; the checked reservation
        // refuses without wrapping and touches no queue when it refuses.
        if self.reaper.try_note_accepted().is_err() {
            if let Ok(mut active) = self.active.lock() {
                active.remove(&generation);
                self.pending.store(active.len(), Ordering::Release);
            }
            request.cancel.cancel(CancelCause::Cancelled);
            return Err("Radio catalog request refused: worker accounting exhausted.".into());
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
                Err("Radio catalog request refused: queue full.".into())
            }
            Err(TrySendError::Disconnected(req)) => {
                self.reaper.note_retired();
                if let Ok(mut active) = self.active.lock() {
                    active.remove(&generation);
                    self.pending.store(active.len(), Ordering::Release);
                }
                req.cancel.cancel(CancelCause::Shutdown);
                Err("Radio catalog worker pool stopped.".into())
            }
        }
    }

    /// Cancel any in-flight request whose generation is older than
    /// `keep_generation`, signalling the transport to abort.
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

    /// Drain all completed results without blocking.
    pub fn drain(&self) -> Vec<CatalogResult> {
        let mut ready = Vec::new();
        loop {
            match self.results.try_recv() {
                Ok(result) => {
                    if let Ok(mut active) = self.active.lock() {
                        active.remove(&result.generation);
                        self.pending.store(active.len(), Ordering::Release);
                    }
                    self.reaper.note_retired();
                    ready.push(result);
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

    /// Begin retirement: close admission, cancel in-flight and queued requests,
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

impl Drop for RadioCatalogService {
    // Never joins: a stalled transport must not block the dropping thread.
    // Retirement completes through `shutdown_progress` on the reducer.
    fn drop(&mut self) {
        self.signal_close();
    }
}

// ---------------------------------------------------------------------------
// Request builder
// ---------------------------------------------------------------------------

/// Build a generation-stamped request.
#[must_use]
pub fn build_request(generation: u64, kind: CatalogRequestKind) -> CatalogRequest {
    // Most listened first; streams Radio Browser found broken are left out.
    let common = format!("order=clickcount&reverse=true&hidebroken=true&limit={LIST_SIZE}");
    let endpoint_url = match &kind {
        CatalogRequestKind::Browse { country, genre } => {
            let mut url = format!(
                "{API_ROOT}/search?countrycode={}&{common}",
                percent_encode(&country.to_ascii_uppercase())
            );
            if let Some(tag) = genre {
                url.push_str(&format!("&tag={}", percent_encode(tag)));
            }
            url
        }
        CatalogRequestKind::Search { query } => {
            format!("{API_ROOT}/search?name={}&{common}", percent_encode(query))
        }
        CatalogRequestKind::ByUuid { uuids } => {
            let joined = uuids
                .iter()
                .take(MAX_STATIONS_PER_REPLY)
                .map(|u| percent_encode(u))
                .collect::<Vec<_>>()
                .join(",");
            format!("{API_ROOT}/byuuid?uuids={joined}")
        }
    };
    CatalogRequest {
        generation,
        kind,
        endpoint_url,
        cancel: CancelSignal::new(),
    }
}

/// Minimal RFC 3986 percent-encoder for the query segment.
///
/// Only unreserved ASCII passes through; every other byte becomes `%XX`.
#[must_use]
pub fn percent_encode(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for byte in raw.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(*byte as char);
            }
            _ => out.push_str(&format!("%{:02X}", byte)),
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Transport-driven fetch (worker side)
// ---------------------------------------------------------------------------

/// Execute one request through the injected transport.
pub fn fetch_with_transport(
    request: CatalogRequest,
    transport: &dyn Transport,
    policy: UrlPolicy,
) -> CatalogResult {
    let generation = request.generation;
    let kind = request.kind.clone();
    let result = fetch_body(&request, transport, policy).and_then(|body| {
        parse_stations(&body, &policy).map_err(|message| CatalogError {
            message,
            retryable: false,
            kind: CatalogErrorKind::Malformed,
        })
    });
    CatalogResult {
        generation,
        kind,
        result,
    }
}

fn fetch_body(
    request: &CatalogRequest,
    transport: &dyn Transport,
    policy: UrlPolicy,
) -> Result<Vec<u8>, CatalogError> {
    if request.cancel.is_cancelled() {
        return Err(CatalogError {
            message: "Radio catalog request was cancelled.".into(),
            retryable: true,
            kind: CatalogErrorKind::Cancelled,
        });
    }
    let remote = RemoteRequest::checked(&request.endpoint_url, &policy).map_err(url_error)?;
    let response = transport
        .fetch(
            &remote,
            &policy,
            &CATALOG_LIMITS,
            &request.cancel,
            &mut |_| {},
        )
        .map_err(transport_error)?;
    validate_response(response)
}

fn validate_response(response: TransportResponse) -> Result<Vec<u8>, CatalogError> {
    if !(200..300).contains(&response.status) {
        return Err(CatalogError {
            message: format!(
                "Radio catalog server returned HTTP {}. Retry may help.",
                response.status
            ),
            retryable: true,
            kind: CatalogErrorKind::Http,
        });
    }
    if response.body.len() as u64 > MAX_CATALOG_BYTES {
        return Err(CatalogError {
            message: format!(
                "Radio catalog reply exceeds the {} KiB limit.",
                MAX_CATALOG_BYTES / 1024
            ),
            retryable: false,
            kind: CatalogErrorKind::Malformed,
        });
    }
    Ok(response.body)
}

fn url_error(verdict: UrlVerdict) -> CatalogError {
    CatalogError {
        message: format!("Radio catalog URL refused by policy: {}.", verdict.reason()),
        retryable: false,
        kind: CatalogErrorKind::Url,
    }
}

fn transport_error(err: TransportError) -> CatalogError {
    match err {
        TransportError::Url(v) => url_error(v),
        TransportError::AddressBlocked(_) => CatalogError {
            message: "Radio catalog destination is a blocked address.".into(),
            retryable: false,
            kind: CatalogErrorKind::Blocked,
        },
        TransportError::Dns => CatalogError {
            message: "Could not resolve the Radio catalog host.".into(),
            retryable: true,
            kind: CatalogErrorKind::Dns,
        },
        other => CatalogError {
            message: format!("Radio catalog transport failed: {other}."),
            retryable: true,
            kind: CatalogErrorKind::Transport,
        },
    }
}

// ---------------------------------------------------------------------------
// Parser
// ---------------------------------------------------------------------------

/// Parse a Radio Browser stations JSON reply into a bounded list of
/// [`RadioStation`]s.
///
/// * Truncates at [`MAX_STATIONS_PER_REPLY`] rows.
/// * Drops any row without a `stationuuid` (no durable identity).
/// * Drops any row without a `url_resolved` / `url` (no playable stream).
/// * Drops any row whose stream URL fails the shared
///   [`enzui_core::url_policy::UrlPolicy`] — a
///   private-network stream URL cannot become a Tune target.
/// * De-duplicates by `stationuuid` (first-wins) so a paginated reply
///   with an overlap does not blow the bounded list.
///
/// Only the UUID and stream URL are required; every other field defaults.
pub fn parse_stations(body: &[u8], policy: &UrlPolicy) -> Result<Vec<RadioStation>, String> {
    let value: serde_json::Value = serde_json::from_slice(body)
        .map_err(|e| format!("Radio catalog reply was not valid JSON: {e}"))?;
    let array = value
        .as_array()
        .ok_or_else(|| "Radio catalog reply top-level was not a JSON array.".to_string())?;
    let mut out: Vec<RadioStation> = Vec::new();
    let mut seen_uuids: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for entry in array.iter().take(MAX_STATIONS_PER_REPLY) {
        let Some(uuid) = entry
            .get("stationuuid")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
        else {
            continue;
        };
        if !seen_uuids.insert(uuid.to_string()) {
            continue;
        }
        // Prefer `url_resolved` (RB's post-redirect canonical URL) but
        // fall back to `url` — both must pass the URL policy.
        let stream_url = entry
            .get("url_resolved")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .or_else(|| entry.get("url").and_then(|v| v.as_str()))
            .map(str::trim)
            .filter(|s| !s.is_empty());
        let Some(stream_url) = stream_url else {
            continue;
        };
        // Refuse a stream URL a Tune could not dial, including plaintext HTTP.
        if !stream_url_admitted(stream_url, policy) {
            continue;
        }
        let name = entry
            .get("name")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or("Unknown station")
            .to_string();
        let country = entry
            .get("country")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let codec = entry
            .get("codec")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let bitrate = entry
            .get("bitrate")
            .and_then(|v| v.as_u64())
            .map(|b| b.min(u32::MAX as u64) as u32)
            .unwrap_or(0);
        let votes = entry.get("votes").and_then(|v| v.as_i64()).unwrap_or(0);
        let tags_field = entry
            .get("tags")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        // RB delivers tags as a comma list; the row renders `·`-joined.
        let tags = tags_field
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(" · ");
        // Fallback description: `<country> · <codec> · <bitrate>k`.
        let description = synthesize_description(&country, &codec, bitrate);
        let homepage = entry
            .get("homepage")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let favicon_url = entry
            .get("favicon")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let last_check_ok = entry
            .get("lastcheckok")
            .and_then(|v| v.as_u64())
            .map(|n| n != 0)
            .unwrap_or(false);
        let last_check_time_unix = entry
            .get("lastchecktime_iso8601")
            .and_then(|v| v.as_str())
            .and_then(parse_iso8601_to_unix)
            .unwrap_or(0);
        out.push(RadioStation {
            id: derive_local_id_from_uuid(uuid),
            remote_uuid: uuid.to_string(),
            name,
            description,
            tags,
            stream_url: stream_url.to_string(),
            homepage,
            favicon_url,
            country,
            codec,
            bitrate,
            votes,
            last_check_ok,
            last_check_time_unix,
        });
    }
    Ok(out)
}

/// Pre-DNS admission for a station's stream URL.
///
/// The transport's [`UrlPolicy::check_address`] only sees DNS-resolved
/// addresses, so a literal-IP host such as `https://192.168.0.1/stream.mp3`
/// is classified here. DNS names are admitted; the transport's post-DNS gate
/// stays authoritative for them.
fn stream_url_admitted(url: &str, policy: &UrlPolicy) -> bool {
    let Ok(safe) = policy.check_url(url) else {
        return false;
    };
    if let Ok(addr) = std::net::IpAddr::from_str(safe.host()) {
        return policy.check_address(addr).is_allowed();
    }
    // DNS-name hosts: the shape gate passed, and the transport will
    // re-classify the resolved address post-DNS. Admit here.
    true
}

fn synthesize_description(country: &str, codec: &str, bitrate: u32) -> String {
    let mut parts: Vec<String> = Vec::with_capacity(3);
    if !country.trim().is_empty() {
        parts.push(country.to_string());
    }
    if !codec.trim().is_empty() {
        parts.push(codec.to_string());
    }
    if bitrate > 0 {
        parts.push(format!("{bitrate}k"));
    }
    if parts.is_empty() {
        "Internet radio station".to_string()
    } else {
        parts.join(" · ")
    }
}

/// Minimal ISO-8601 to Unix seconds parser for Radio Browser timestamps.
///
/// Accepts `YYYY-MM-DD HH:MM:SS` or `YYYY-MM-DDTHH:MM:SS` in UTC. An
/// unrecognised or out-of-range value returns `None`; the timestamp is
/// diagnostic-only, so the row keeps `last_check_time_unix = 0`.
fn parse_iso8601_to_unix(s: &str) -> Option<u64> {
    // Radio Browser stamps in UTC, so no timezone handling.
    let s = s.trim();
    if s.len() < 19 {
        return None;
    }
    match s.as_bytes().get(10) {
        Some(b'T' | b' ') => {}
        _ => return None,
    }
    let year: i64 = s.get(0..4)?.parse().ok()?;
    let month: u32 = s.get(5..7)?.parse().ok()?;
    let day: u32 = s.get(8..10)?.parse().ok()?;
    let hour: u64 = s.get(11..13)?.parse().ok()?;
    let minute: u64 = s.get(14..16)?.parse().ok()?;
    let second: u64 = s.get(17..19)?.parse().ok()?;

    // Range-validate before arithmetic so malformed input cannot panic.
    if !(1..=12).contains(&month) {
        return None;
    }
    if !(1..=31).contains(&day) {
        return None;
    }
    // Per-month day ceiling, including a full Gregorian leap-year rule.
    let max_day: u32 = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            let leap = (year % 4 == 0 && year % 100 != 0) || (year % 400 == 0);
            if leap { 29 } else { 28 }
        }
        _ => return None,
    };
    if day > max_day {
        return None;
    }
    // Allow the leap second (`:60`) but nothing above it.
    if hour > 23 || minute > 59 || second > 60 {
        return None;
    }

    // Days-from-civil (Howard Hinnant): in the March-based calendar `m` is
    // already in `[0, 11]`, so `doy = (153 * m + 2) / 5 + d - 1`.
    let (y, m) = if month <= 2 {
        (year - 1, month + 9)
    } else {
        (year, month - 3)
    };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as u64;
    let doy = (153 * (m as u64) + 2) / 5 + (day as u64) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe as i64 - 719_468;
    if days < 0 {
        return Some(0);
    }
    Some((days as u64) * 86_400 + hour * 3600 + minute * 60 + second)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------
