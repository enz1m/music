//! Shared worker-retirement owner for the bounded blocking-fetch services:
//! podcast artwork, podcast discovery, and the radio catalog.
//!
//! Each service owns one [`WorkerReaper`], a move-only owner of finite worker
//! handles with no detached reaper. Accepted occupancy is a checked reservation
//! ([`WorkerReaper::try_note_accepted`]) taken before a job is enqueued and held
//! until its completion drains; it never wraps and latches on exhaustion.
//! Shutdown closes admission, then moves the workers into one off-reducer reaper
//! the owner joins. [`WorkerReaper::close_debt`] authorizes close and settles
//! only when no accepted request, unjoined worker, pending reaper join, reaper
//! panic, or worker panic / lost custody remains; a worker whose join unwinds
//! and a poisoned worker cell are permanent vetoes. `Drop` never joins.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;

/// A checked admission refused: the occupancy charge is saturated and has
/// latched. The owner never mutates its job queue on this path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AdmissionRefused;

/// Outstanding retirement debt for one fetch service; close is authorized only
/// when [`Self::is_settled`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CloseDebt {
    /// Accepted requests whose completion has not yet drained.
    pub accepted: usize,
    /// Worker threads the reaper has not yet joined.
    pub unjoined_workers: usize,
    /// A reaper join is still owed: a spawn retry is pending or the thread has
    /// not finished.
    pub reaper_pending: bool,
    /// The reaper thread itself unwound on join.
    pub reaper_panicked: bool,
    /// Worker joins that unwound, plus a poisoned (lost-custody) worker cell; a
    /// permanent close veto.
    pub worker_panicked: usize,
}

impl CloseDebt {
    /// Whether nothing remains to retire: no accepted request, every worker and
    /// the reaper joined cleanly, and no reaper or worker panic.
    #[must_use]
    pub fn is_settled(&self) -> bool {
        self.accepted == 0
            && self.unjoined_workers == 0
            && !self.reaper_pending
            && !self.reaper_panicked
            && self.worker_panicked == 0
    }

    /// Whether a reaper or worker panic or a lost-custody poison vetoes close
    /// permanently, as opposed to a pending retirement that settles once the
    /// workers leave.
    #[must_use]
    pub fn is_terminally_vetoed(&self) -> bool {
        self.reaper_panicked || self.worker_panicked > 0
    }
}

/// Move-only retirement owner for one bounded blocking-worker pool.
///
/// The owning service holds admission, the active-signal table, and the cancel
/// discipline; this owns the accepted-occupancy charge and the single reaper
/// that joins the workers off the reducer.
pub struct WorkerReaper {
    // Charged before a job is enqueued and held until its completion drains.
    // Unlike the service's UI `pending` count it survives shutdown, so close
    // cannot settle while a queued or running job still owes its result.
    occupancy: AtomicUsize,
    // Latched once a checked admission would wrap the charge; thereafter every
    // admission is refused rather than overflowing.
    admission_exhausted: AtomicBool,
    // A reaper spawn failure leaves the handles here for a later retry; a lost
    // spawn never detaches a live worker.
    workers: Arc<Mutex<Vec<JoinHandle<()>>>>,
    // The reaper decrements this per join; a worker stuck in the transport holds
    // it above zero, so the pool is never settled before exit.
    unjoined: Arc<AtomicUsize>,
    // Incremented once per worker whose join unwound; permanent close debt.
    panicked: Arc<AtomicUsize>,
    reaper: Option<JoinHandle<()>>,
    // Set by the first shutdown; bounds the pool to a single reaper across every
    // later close or drop.
    closing: bool,
    // The reaper thread could not be spawned; a close-wake retry owns the next
    // attempt.
    reaper_owed: bool,
    reaper_joined: bool,
    reaper_panicked: bool,
    // The worker cell was found poisoned: a holder panicked with custody of the
    // handles. Permanent lost-custody debt, never a clean join.
    custody_lost: bool,
    reaper_name: &'static str,
}

impl std::fmt::Debug for WorkerReaper {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkerReaper")
            .field("accepted", &self.occupancy.load(Ordering::Acquire))
            .field(
                "admission_exhausted",
                &self.admission_exhausted.load(Ordering::Acquire),
            )
            .field("closing", &self.closing)
            .field("reaper_owed", &self.reaper_owed)
            .field("reaper_joined", &self.reaper_joined)
            .field("reaper_panicked", &self.reaper_panicked)
            .field("worker_panicked", &self.panicked.load(Ordering::Acquire))
            .field("custody_lost", &self.custody_lost)
            .finish_non_exhaustive()
    }
}

impl WorkerReaper {
    /// A live pool that owns `handles` worker threads and will spawn its one
    /// reaper under `reaper_name`.
    #[must_use]
    pub fn new(handles: Vec<JoinHandle<()>>, reaper_name: &'static str) -> Self {
        let unjoined = Arc::new(AtomicUsize::new(handles.len()));
        Self {
            occupancy: AtomicUsize::new(0),
            admission_exhausted: AtomicBool::new(false),
            workers: Arc::new(Mutex::new(handles)),
            unjoined,
            panicked: Arc::new(AtomicUsize::new(0)),
            reaper: None,
            closing: false,
            reaper_owed: false,
            reaper_joined: false,
            reaper_panicked: false,
            custody_lost: false,
            reaper_name,
        }
    }

    /// A never-started pool (an unavailable service): nothing to retire, so it
    /// reports settled from birth.
    #[must_use]
    pub fn settled(reaper_name: &'static str) -> Self {
        Self {
            occupancy: AtomicUsize::new(0),
            admission_exhausted: AtomicBool::new(false),
            workers: Arc::new(Mutex::new(Vec::new())),
            unjoined: Arc::new(AtomicUsize::new(0)),
            panicked: Arc::new(AtomicUsize::new(0)),
            reaper: None,
            closing: false,
            reaper_owed: false,
            reaper_joined: true,
            reaper_panicked: false,
            custody_lost: false,
            reaper_name,
        }
    }

    /// Reserve one accepted request with a checked charge, taken before the job
    /// is enqueued. Refuses without wrapping once the charge is saturated, and
    /// latches. The caller must roll the reservation back with
    /// [`Self::note_retired`] if the enqueue then fails, so every drained
    /// completion decrements exactly one charged request.
    pub fn try_note_accepted(&self) -> Result<(), AdmissionRefused> {
        if self.admission_exhausted.load(Ordering::Acquire) {
            return Err(AdmissionRefused);
        }
        match self
            .occupancy
            .try_update(Ordering::Release, Ordering::Acquire, |v| v.checked_add(1))
        {
            Ok(_) => Ok(()),
            Err(_) => {
                self.admission_exhausted.store(true, Ordering::Release);
                Err(AdmissionRefused)
            }
        }
    }

    /// Retire one accepted request as its completion drains, or roll back a
    /// reservation whose enqueue failed. Saturating so an uncharged request can
    /// never underflow the charge.
    pub fn note_retired(&self) {
        let _ = self
            .occupancy
            .try_update(Ordering::Release, Ordering::Acquire, |v| {
                Some(v.saturating_sub(1))
            });
    }

    #[cfg(test)]
    #[must_use]
    pub fn admission_exhausted(&self) -> bool {
        self.admission_exhausted.load(Ordering::Acquire)
    }

    /// Mark admission closed. Idempotent; the owning service is responsible for
    /// cancelling active work and dropping the job sender.
    pub fn begin_close(&mut self) {
        self.closing = true;
    }

    /// The remaining retirement debt. The worker-join and reaper fields are
    /// charged only after retirement begins, so a live pool whose workers are
    /// idle reports no join debt; a worker panic or lost custody is always
    /// charged.
    #[must_use]
    pub fn close_debt(&self) -> CloseDebt {
        CloseDebt {
            accepted: self.occupancy.load(Ordering::Acquire),
            unjoined_workers: if self.closing {
                self.unjoined.load(Ordering::Acquire)
            } else {
                0
            },
            reaper_pending: self.closing
                && !self.reaper_joined
                && !self.reaper_panicked
                && !self.custody_lost,
            reaper_panicked: self.reaper_panicked,
            worker_panicked: self.panicked.load(Ordering::Acquire) + usize::from(self.custody_lost),
        }
    }

    /// Launch exactly one reaper under the owner's thread name. On a spawn
    /// failure the handles stay retained and a retry is owed.
    pub fn launch_reaper(&mut self) {
        let name = self.reaper_name;
        self.launch_reaper_via(move |workers, unjoined, panicked| {
            spawn_reaper(name, workers, unjoined, panicked)
        });
    }

    // Launch at most one reaper. On a spawn failure the handles stay in the
    // shared cell and a retry is owed; the reaper is never joined on this
    // thread. A poisoned worker cell latches lost custody instead of joining.
    fn launch_reaper_via(
        &mut self,
        spawn: impl FnOnce(
            &Arc<Mutex<Vec<JoinHandle<()>>>>,
            &Arc<AtomicUsize>,
            &Arc<AtomicUsize>,
        ) -> std::io::Result<JoinHandle<()>>,
    ) {
        if self.reaper.is_some() || self.reaper_joined || self.reaper_panicked || self.custody_lost
        {
            return;
        }
        // A poisoned cell means a holder panicked with custody of the handles;
        // treating it as empty would fabricate a clean joined state, so latch
        // lost custody instead.
        let empty = match self.workers.lock() {
            Ok(guard) => guard.is_empty(),
            Err(_) => {
                self.custody_lost = true;
                self.reaper_owed = false;
                return;
            }
        };
        if empty {
            self.reaper_joined = true;
            self.reaper_owed = false;
            self.unjoined.store(0, Ordering::Release);
            return;
        }
        match spawn(&self.workers, &self.unjoined, &self.panicked) {
            Ok(handle) => {
                self.reaper = Some(handle);
                self.reaper_owed = false;
            }
            Err(_) => self.reaper_owed = true,
        }
    }

    /// Advance retirement without blocking and return the remaining
    /// [`CloseDebt`]. Retries an owed reaper spawn and joins the reaper once it
    /// has finished; the caller pumps this from the close wake while separately
    /// draining completions.
    pub fn shutdown_progress(&mut self) -> CloseDebt {
        if self.reaper_owed {
            self.launch_reaper();
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

    #[cfg(test)]
    pub fn force_failed_reaper_spawn(&mut self) {
        self.begin_close();
        self.launch_reaper_via(|_, _, _| Err(std::io::Error::from(std::io::ErrorKind::WouldBlock)));
    }

    #[cfg(test)]
    pub fn seed_occupancy(&self, value: usize) {
        self.occupancy.store(value, Ordering::Release);
    }

    #[cfg(test)]
    #[must_use]
    pub fn retained_worker_count(&self) -> usize {
        self.workers.lock().map(|guard| guard.len()).unwrap_or(0)
    }

    #[cfg(test)]
    #[must_use]
    pub fn reaper_present(&self) -> bool {
        self.reaper.is_some()
    }

    #[cfg(test)]
    #[must_use]
    pub fn reaper_owed(&self) -> bool {
        self.reaper_owed
    }

    #[cfg(test)]
    #[must_use]
    pub fn reaper_joined(&self) -> bool {
        self.reaper_joined
    }
}

// Spawn the reaper that drains the shared handle cell and joins each worker
// once, charging a worker panic. On a spawn failure the closure is dropped, but
// it captured only `Arc` clones, so no handle is detached.
fn spawn_reaper(
    name: &'static str,
    workers: &Arc<Mutex<Vec<JoinHandle<()>>>>,
    unjoined: &Arc<AtomicUsize>,
    panicked: &Arc<AtomicUsize>,
) -> std::io::Result<JoinHandle<()>> {
    let reaper_workers = Arc::clone(workers);
    let reaper_unjoined = Arc::clone(unjoined);
    let reaper_panicked = Arc::clone(panicked);
    std::thread::Builder::new()
        .name(name.to_string())
        .spawn(move || {
            let handles = {
                let mut guard = reaper_workers
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner);
                std::mem::take(&mut *guard)
            };
            for worker in handles {
                if worker.join().is_err() {
                    reaper_panicked.fetch_add(1, Ordering::Release);
                }
                reaper_unjoined.fetch_sub(1, Ordering::Release);
            }
        })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    // Workers that block until their returned sender drops: the stand-in for a
    // worker held inside a blocking transport until cancellation.
    fn parked_workers(n: usize, name: &'static str) -> (WorkerReaper, Vec<mpsc::Sender<()>>) {
        let mut handles = Vec::with_capacity(n);
        let mut releases = Vec::with_capacity(n);
        for _ in 0..n {
            let (tx, rx) = mpsc::channel::<()>();
            releases.push(tx);
            handles.push(std::thread::spawn(move || {
                let _ = rx.recv();
            }));
        }
        (WorkerReaper::new(handles, name), releases)
    }

    fn drain_to_settled(reaper: &mut WorkerReaper) -> CloseDebt {
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut debt = reaper.close_debt();
        while !debt.is_settled() && Instant::now() < deadline {
            debt = reaper.shutdown_progress();
            std::thread::sleep(Duration::from_millis(2));
        }
        debt
    }

    // Pump retirement until the reaper thread finishes its join pass, bounded by
    // a deadline. For the panicking-worker case, where close never settles.
    fn pump_until_reaper_done(reaper: &mut WorkerReaper) -> CloseDebt {
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut debt = reaper.shutdown_progress();
        while debt.reaper_pending && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(2));
            debt = reaper.shutdown_progress();
        }
        debt
    }

    #[test]
    fn reaper_spawn_failure_keeps_handles_then_retires_on_progress() {
        let (mut reaper, releases) = parked_workers(1, "test-reaper-spawn-fail");
        reaper.force_failed_reaper_spawn();

        let debt = reaper.close_debt();
        assert!(debt.reaper_pending, "a failed spawn leaves the join owed");
        assert!(
            !debt.is_settled(),
            "close cannot settle while the reaper is owed"
        );
        assert_eq!(debt.unjoined_workers, 1);
        assert_eq!(
            reaper.retained_worker_count(),
            1,
            "the worker handle is retained, not detached, on spawn failure"
        );
        assert!(!reaper.reaper_present() && reaper.reaper_owed());

        drop(releases);
        let debt = drain_to_settled(&mut reaper);
        assert!(
            debt.is_settled(),
            "the retry joins the worker and the reaper"
        );
        assert_eq!(debt.accepted, 0);
        assert_eq!(debt.unjoined_workers, 0);
        assert_eq!(
            debt.worker_panicked, 0,
            "a clean worker join is no panic debt"
        );
        assert!(reaper.reaper_joined() && !reaper.reaper_present());

        // A saturated charge refuses without wrapping and mutates no occupancy.
        let admission = WorkerReaper::settled("test-admission-exhaustion");
        admission.seed_occupancy(usize::MAX);
        assert_eq!(
            admission.try_note_accepted(),
            Err(AdmissionRefused),
            "a saturated charge refuses admission, it does not wrap"
        );
        assert_eq!(
            admission.close_debt().accepted,
            usize::MAX,
            "the refused admission mutates no charge"
        );
        assert!(
            admission.admission_exhausted(),
            "exhaustion latches terminally"
        );
    }

    #[test]
    fn held_worker_vetoes_close_until_reaper_joins() {
        let (mut reaper, releases) = parked_workers(1, "test-reaper-held");
        reaper.try_note_accepted().expect("first charge accepted");
        reaper.begin_close();
        reaper.launch_reaper();

        let debt = reaper.close_debt();
        assert_eq!(
            debt.accepted, 1,
            "the held worker still owes its completion"
        );
        assert_eq!(debt.unjoined_workers, 1);
        assert!(
            !debt.is_settled(),
            "close stays vetoed while the worker is held"
        );
        assert!(reaper.reaper_present(), "one reaper owns the worker handle");

        reaper.launch_reaper();
        assert!(
            reaper.reaper_present(),
            "a repeated close starts no parallel reaper"
        );
        assert!(!reaper.shutdown_progress().is_settled());

        reaper.note_retired();
        drop(releases);
        let debt = drain_to_settled(&mut reaper);
        assert!(
            debt.is_settled(),
            "the released worker and reaper both join, authorizing one close"
        );
        assert!(reaper.reaper_joined() && !reaper.reaper_present());

        // A worker whose join unwinds vetoes close permanently even though the
        // reaper thread itself finishes cleanly.
        let panicker = std::thread::spawn(|| panic!("worker unwind"));
        let mut poisoned = WorkerReaper::new(vec![panicker], "test-reaper-panic");
        poisoned.begin_close();
        poisoned.launch_reaper();
        let debt = pump_until_reaper_done(&mut poisoned);
        assert!(
            debt.worker_panicked >= 1,
            "the unwound worker is charged as panic debt"
        );
        assert!(
            !debt.is_settled(),
            "a worker panic never settles to a clean close"
        );
        assert!(
            debt.is_terminally_vetoed(),
            "a worker panic is a terminal Unknown veto"
        );
    }
}
