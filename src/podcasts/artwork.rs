//! Policy-safe remote podcast artwork: an identity-keyed cache fed by a bounded
//! blocking-worker fetch through the shared [`Transport`] seam.
//!
//! Artwork is keyed by channel (`feed_url`) or episode (`feed_url` + `guid`).
//! Each slot records its URL and a monotonic token, so a changed URL re-fetches
//! and a superseded reply cannot land. Unlike the local [`crate::art`]
//! `CoverCache`, identities sharing one image decode separately. Values are
//! downscaled [`crate::art::CoverArt`] thumbnails.
//!
//! Fetches use the audited [`Transport`] and the shared [`UrlPolicy`] with HTTP
//! and private-network addresses refused. Bodies are capped at
//! `MAX_ARTWORK_BYTES` and decoded under [`crate::art::REMOTE_COVER_DECODE_LIMITS`]; a
//! non-image or over-budget body becomes a failure slot, never a stand-in.

use enzui_core::cancellation::CancelCause;
use enzui_core::url_policy::{TransportLimits, UrlPolicy, UrlVerdict};
use enzui_resources::{CancelSignal, RemoteRequest, Transport, TransportError, TransportResponse};
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError};
use std::sync::{Arc, Mutex};

use crate::art::{CoverArt, CoverSource, decode_cover_within};
use crate::fetch_reaper::{CloseDebt, WorkerReaper};
use crate::podcasts::{REMOTE_POLICY, default_transport};

/// Absolute ceiling on an artwork body; podcast covers are a few hundred KiB.
pub const MAX_ARTWORK_BYTES: u64 = 4 * 1024 * 1024;

/// Transport limits for one artwork fetch; conservative because a long tail is
/// worse than a missing tile.
pub const ARTWORK_LIMITS: TransportLimits = TransportLimits {
    max_bytes: MAX_ARTWORK_BYTES,
    connect_timeout_ms: 5_000,
    read_timeout_ms: 10_000,
    total_timeout_ms: 20_000,
};

pub const ARTWORK_WORKERS: usize = 2;
pub const ARTWORK_QUEUE_CAPACITY: usize = 64;

/// Per-feed cap on distinct episode artwork identities in the live set, so
/// cache cardinality scales with subscriptions rather than feed length.
pub const ARTWORK_MAX_EPISODES_PER_FEED: usize = 200;

// ---------------------------------------------------------------------------
// Identity
// ---------------------------------------------------------------------------

/// The identity a piece of artwork belongs to. `episode_guid == None` is the
/// channel/show cover; `Some(guid)` is a per-episode cover. Both are scoped by
/// `feed_url`, the durable subscription identity.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ArtworkKey {
    pub feed_url: String,
    pub episode_guid: Option<String>,
}

impl ArtworkKey {
    #[must_use]
    pub fn channel(feed_url: impl Into<String>) -> Self {
        Self {
            feed_url: feed_url.into(),
            episode_guid: None,
        }
    }

    #[must_use]
    pub fn episode(feed_url: impl Into<String>, guid: impl Into<String>) -> Self {
        Self {
            feed_url: feed_url.into(),
            episode_guid: Some(guid.into()),
        }
    }
}

// ---------------------------------------------------------------------------
// Cache
// ---------------------------------------------------------------------------

/// One cache slot, recording the `url` it was produced for so a changed URL
/// re-fetches. `Pending` carries the `token` that authorises exactly one
/// response; `Failed` is a terminal absence for its URL.
#[derive(Debug)]
enum ArtworkEntry {
    Pending { token: u64, url: String },
    Ready { url: String, art: CoverArt },
    Failed { url: String },
}

impl ArtworkEntry {
    fn url(&self) -> &str {
        match self {
            ArtworkEntry::Pending { url, .. }
            | ArtworkEntry::Ready { url, .. }
            | ArtworkEntry::Failed { url } => url,
        }
    }
}

/// The app-owned, identity-keyed artwork cache; lookups are cheap during
/// `view` because decoding happens on a worker.
#[derive(Debug, Default)]
pub struct PodcastArtworkCache {
    entries: HashMap<ArtworkKey, ArtworkEntry>,
    next_token: u64,
}

impl PodcastArtworkCache {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The decoded thumbnail for an identity, or `None` when it is still
    /// pending, failed, or was never requested.
    #[must_use]
    pub fn get(&self, key: &ArtworkKey) -> Option<&CoverArt> {
        match self.entries.get(key) {
            Some(ArtworkEntry::Ready { art, .. }) => Some(art),
            _ => None,
        }
    }

    /// The channel/show cover for a feed.
    #[must_use]
    pub fn channel_art(&self, feed_url: &str) -> Option<&CoverArt> {
        self.get(&ArtworkKey::channel(feed_url))
    }

    /// An episode cover, falling back to the channel cover when the episode
    /// declared none.
    #[must_use]
    pub fn episode_art(&self, feed_url: &str, guid: &str) -> Option<&CoverArt> {
        self.get(&ArtworkKey::episode(feed_url, guid))
            .or_else(|| self.channel_art(feed_url))
    }

    /// Whether the reducer should fetch `url` for `key`: `true` when no slot
    /// exists or the slot was produced for a different URL. A failed slot for the
    /// same URL is not retried.
    #[must_use]
    pub fn needs_fetch(&self, key: &ArtworkKey, url: &str) -> bool {
        match self.entries.get(key) {
            None => true,
            Some(entry) => entry.url() != url,
        }
    }

    /// Reserve the next fetch token. Callers pair this with a service `submit`
    /// and only [`Self::mark_pending`] on success, so a dropped submit leaves
    /// the slot re-armed for the next pass.
    pub fn allocate_token(&mut self) -> u64 {
        self.next_token = self.next_token.wrapping_add(1);
        self.next_token
    }

    /// Record that a fetch of `url` was dispatched under `token`.
    pub fn mark_pending(&mut self, key: ArtworkKey, token: u64, url: String) {
        self.entries
            .insert(key, ArtworkEntry::Pending { token, url });
    }

    /// Fold one completed fetch into the cache only when it answers the slot's
    /// current pending `token`; superseded replies are dropped.
    pub fn apply(&mut self, response: ArtworkResponse) {
        let ArtworkResponse {
            key,
            url,
            token,
            result,
        } = response;
        match self.entries.get(&key) {
            Some(ArtworkEntry::Pending { token: current, .. }) if *current == token => {}
            _ => return,
        }
        let entry = match result {
            Ok(art) => ArtworkEntry::Ready { url, art },
            Err(_) => ArtworkEntry::Failed { url },
        };
        self.entries.insert(key, entry);
    }

    /// Reconcile the cache down to the desired `(identity, url)` set.
    ///
    /// A slot survives only when its key is desired with exactly its stored URL.
    /// Returns the `(key, token)` of each dropped pending entry so the reducer can
    /// retire its in-flight fetch.
    pub fn reconcile(
        &mut self,
        desired: &std::collections::HashMap<ArtworkKey, String>,
    ) -> Vec<(ArtworkKey, u64)> {
        let mut retired = Vec::new();
        self.entries.retain(|key, entry| {
            if desired.get(key).map(String::as_str) == Some(entry.url()) {
                return true;
            }
            if let ArtworkEntry::Pending { token, .. } = entry {
                retired.push((key.clone(), *token));
            }
            false
        });
        retired
    }
}

// ---------------------------------------------------------------------------
// Request / response
// ---------------------------------------------------------------------------

/// One artwork fetch. `token` is the cache authority for this exact `url`, so a
/// reply is applied only while it is still the identity's current request.
#[derive(Clone, Debug)]
pub struct ArtworkRequest {
    pub key: ArtworkKey,
    pub url: String,
    pub token: u64,
    cancel: CancelSignal,
}

/// A completed artwork fetch delivered to the reducer. Echoes the `url` and
/// `token` so the cache applies it only against the matching pending slot.
#[derive(Debug)]
pub struct ArtworkResponse {
    pub key: ArtworkKey,
    pub url: String,
    pub token: u64,
    pub result: Result<CoverArt, String>,
}

// ---------------------------------------------------------------------------
// The service
// ---------------------------------------------------------------------------

/// One outstanding fetch, keyed by request token. A cancelled request stays as
/// `retired` until its transfer drains, counting as pool occupancy without
/// blocking a fresh request for the same identity.
struct ActiveFetch {
    key: ArtworkKey,
    cancel: CancelSignal,
    retired: bool,
}

/// The bounded blocking transport owner for artwork; two workers and a bounded
/// queue absorb a screen of thumbnails.
pub struct PodcastArtworkService {
    jobs: Option<SyncSender<ArtworkRequest>>,
    results: Receiver<ArtworkResponse>,
    // Keyed by token because a retired request and a fresh one for the same
    // identity can both be outstanding.
    active: Arc<Mutex<HashMap<u64, ActiveFetch>>>,
    // UI active-tile count for the poll wake; shutdown clears it while the
    // reaper holds the exact close charge.
    pending: AtomicUsize,
    reaper: WorkerReaper,
    unavailable: bool,
}

impl std::fmt::Debug for PodcastArtworkService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PodcastArtworkService")
            .field("pending", &self.pending.load(Ordering::Acquire))
            .field("unavailable", &self.unavailable)
            .finish_non_exhaustive()
    }
}

impl PodcastArtworkService {
    /// Shipping constructor. Reuses [`crate::podcasts::default_transport`] so
    /// artwork shares the one audited transport and its test seam.
    #[must_use]
    pub fn shipping() -> Self {
        Self::new(
            default_transport(),
            REMOTE_POLICY,
            ARTWORK_WORKERS,
            ARTWORK_QUEUE_CAPACITY,
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
            return Err("Podcast artwork worker pool is unavailable.".into());
        }
        let (job_tx, job_rx) = mpsc::sync_channel::<ArtworkRequest>(queue_capacity);
        let job_rx = Arc::new(Mutex::new(job_rx));
        // Capacity equals the admission cap, so each accepted request has a
        // result slot and a worker mid-send never blocks.
        let (result_tx, result_rx) = mpsc::sync_channel::<ArtworkResponse>(ARTWORK_QUEUE_CAPACITY);
        let active: Arc<Mutex<HashMap<u64, ActiveFetch>>> = Arc::new(Mutex::new(HashMap::new()));
        let mut handles = Vec::with_capacity(workers);

        for index in 0..workers {
            let jobs = job_rx.clone();
            let results = result_tx.clone();
            let transport = transport.clone();
            let builder = std::thread::Builder::new().name(format!("podcast-artwork-{index}"));
            match builder.spawn(move || {
                loop {
                    let job = match jobs.lock() {
                        Ok(rx) => rx.recv(),
                        Err(_) => return,
                    };
                    let Ok(request) = job else { return };
                    let response = fetch_with_transport(request, transport.as_ref(), policy);
                    if results.send(response).is_err() {
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
                        "Podcast artwork worker could not start: {}.",
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
            reaper: WorkerReaper::new(handles, "podcast-artwork-reaper"),
            unavailable: false,
        })
    }

    fn unavailable(_message: String) -> Self {
        let (_job_tx, job_rx) = mpsc::sync_channel::<ArtworkRequest>(0);
        let (_result_tx, result_rx) = mpsc::sync_channel::<ArtworkResponse>(1);
        drop(job_rx);
        Self {
            jobs: None,
            results: result_rx,
            active: Arc::new(Mutex::new(HashMap::new())),
            pending: AtomicUsize::new(0),
            reaper: WorkerReaper::settled("podcast-artwork-reaper"),
            unavailable: true,
        }
    }

    /// Submit an artwork fetch, returning `false` when the service is unavailable
    /// or the queue is full. Call `mark_pending` only on `true` so a dropped
    /// request is re-armed on the next pass.
    pub fn submit(&self, request: ArtworkRequest) -> bool {
        if self.unavailable {
            return false;
        }
        let token = request.token;
        let key = request.key.clone();
        let cancel = request.cancel.clone();
        if let Ok(mut active) = self.active.lock() {
            // Refuse a duplicate of a live fetch; a retired one does not block.
            if active
                .values()
                .any(|fetch| !fetch.retired && fetch.key == key)
            {
                request.cancel.cancel(CancelCause::Cancelled);
                return false;
            }
            // Admission counts retired-but-undrained transfers as real
            // occupancy, so rapid URL churn on one identity stays bounded.
            if active.len() >= ARTWORK_QUEUE_CAPACITY {
                request.cancel.cancel(CancelCause::Cancelled);
                return false;
            }
            active.insert(
                token,
                ActiveFetch {
                    key,
                    cancel,
                    retired: false,
                },
            );
            self.pending.store(active.len(), Ordering::Release);
        } else {
            return false;
        }
        let Some(jobs) = &self.jobs else {
            if let Ok(mut active) = self.active.lock() {
                active.remove(&token);
                self.pending.store(active.len(), Ordering::Release);
            }
            request.cancel.cancel(CancelCause::Shutdown);
            return false;
        };
        // Reserve the occupancy charge before enqueuing; the checked reservation
        // refuses without wrapping and touches no queue when it refuses.
        if self.reaper.try_note_accepted().is_err() {
            if let Ok(mut active) = self.active.lock() {
                active.remove(&token);
                self.pending.store(active.len(), Ordering::Release);
            }
            request.cancel.cancel(CancelCause::Cancelled);
            return false;
        }
        match jobs.try_send(request) {
            Ok(()) => true,
            Err(TrySendError::Full(req)) => {
                self.reaper.note_retired();
                if let Ok(mut active) = self.active.lock() {
                    active.remove(&req.token);
                    self.pending.store(active.len(), Ordering::Release);
                }
                req.cancel.cancel(CancelCause::Cancelled);
                false
            }
            Err(TrySendError::Disconnected(req)) => {
                self.reaper.note_retired();
                if let Ok(mut active) = self.active.lock() {
                    active.remove(&req.token);
                    self.pending.store(active.len(), Ordering::Release);
                }
                req.cancel.cancel(CancelCause::Shutdown);
                false
            }
        }
    }

    /// Retire every applicable in-flight fetch for one identity: fire each cancel
    /// signal and mark it `retired` until its response drains. Idempotent.
    pub fn cancel(&self, key: &ArtworkKey) -> bool {
        if let Ok(mut active) = self.active.lock() {
            let mut any = false;
            for fetch in active.values_mut() {
                if !fetch.retired && &fetch.key == key {
                    fetch.retired = true;
                    fetch.cancel.cancel(CancelCause::Cancelled);
                    any = true;
                }
            }
            return any;
        }
        false
    }

    /// Outstanding requests, including cancelled ones still draining; admission
    /// and the poll subscription are bounded by this count.
    #[must_use]
    pub fn pending_count(&self) -> usize {
        self.pending.load(Ordering::Acquire)
    }

    /// Drain completed artwork responses non-blockingly. A response frees its
    /// token's accounting slot whether the request was applicable or retired.
    pub fn drain(&self) -> Vec<ArtworkResponse> {
        let mut ready = Vec::new();
        loop {
            match self.results.try_recv() {
                Ok(response) => {
                    if let Ok(mut active) = self.active.lock() {
                        active.remove(&response.token);
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

    /// Build a request for one identity + URL. The cancel token is minted here
    /// so a shutdown can abort the in-flight transfer.
    #[must_use]
    pub fn build_request(key: ArtworkKey, url: impl Into<String>, token: u64) -> ArtworkRequest {
        ArtworkRequest {
            key,
            url: url.into(),
            token,
            cancel: CancelSignal::new(),
        }
    }

    /// Begin retirement: close admission, cancel in-flight and queued fetches,
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
            for (_, fetch) in active.drain() {
                fetch.cancel.cancel(CancelCause::Shutdown);
            }
            self.pending.store(0, Ordering::Release);
        }
        self.jobs.take();
        self.reaper.begin_close();
    }
}

impl Drop for PodcastArtworkService {
    // Never joins: a stalled transport must not block the dropping thread.
    // Retirement completes through `shutdown_progress` on the reducer.
    fn drop(&mut self) {
        self.signal_close();
    }
}

// ---------------------------------------------------------------------------
// Transport-driven fetch + decode (worker side)
// ---------------------------------------------------------------------------

/// Fetch one artwork URL through the injected transport and decode it with
/// [`decode_cover_within`] under the remote artwork budget.
pub fn fetch_with_transport(
    request: ArtworkRequest,
    transport: &dyn Transport,
    policy: UrlPolicy,
) -> ArtworkResponse {
    let key = request.key.clone();
    let url = request.url.clone();
    let token = request.token;
    let result = fetch_bytes(&request, transport, policy).and_then(|bytes| {
        // `Embedded` is the closest source tag; the distinction is not surfaced for
        // podcast covers.
        decode_cover_within(
            &bytes,
            CoverSource::Embedded,
            &crate::art::REMOTE_COVER_DECODE_LIMITS,
        )
        .ok_or_else(|| "Artwork was not a decodable image.".to_string())
    });
    ArtworkResponse {
        key,
        url,
        token,
        result,
    }
}

fn fetch_bytes(
    request: &ArtworkRequest,
    transport: &dyn Transport,
    policy: UrlPolicy,
) -> Result<Vec<u8>, String> {
    if request.cancel.is_cancelled() {
        return Err("Artwork request was cancelled.".into());
    }
    let remote = RemoteRequest::checked(&request.url, &policy).map_err(url_error)?;
    let response = transport
        .fetch(
            &remote,
            &policy,
            &ARTWORK_LIMITS,
            &request.cancel,
            &mut |_| {},
        )
        .map_err(transport_error)?;
    validate_response(response)
}

fn validate_response(response: TransportResponse) -> Result<Vec<u8>, String> {
    if !(200..300).contains(&response.status) {
        return Err(format!("Artwork server returned HTTP {}.", response.status));
    }
    if response.body.len() as u64 > MAX_ARTWORK_BYTES {
        return Err(format!(
            "Artwork exceeds the {} MiB limit.",
            MAX_ARTWORK_BYTES / 1024 / 1024
        ));
    }
    Ok(response.body)
}

fn url_error(verdict: UrlVerdict) -> String {
    format!("Artwork URL refused by policy: {}.", verdict.reason())
}

fn transport_error(err: TransportError) -> String {
    match err {
        TransportError::Url(v) => url_error(v),
        TransportError::AddressBlocked(_) => "Artwork host resolves to a blocked address.".into(),
        TransportError::Dns => "Could not resolve the artwork host.".into(),
        other => format!("Could not download artwork: {other}."),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------
