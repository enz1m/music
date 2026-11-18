//! Boot authority, restore intent, and notice types.

use super::*;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum PersistenceNotice {
    RecoveredFromCorrupt(Option<std::path::PathBuf>),
    ReadOnlyFutureVersion(u32),
    RefusedReadOnly(persistence::RefusalKind),
    /// The data root itself was refused, so no file was read and saving is off
    /// for the session. Path-free.
    PersistenceRootUnavailable(persistence::RootRefusal),
    /// An explicit Retire could not prove a clean quarantine move, so saving
    /// stays off. Carries a bounded, path-free typed custody reason.
    RetireRefused(persistence::RetireRefusal),
    SaveFailed(String),
}

/// The source-open admitted to the audio backend at load time.
///
/// Binds a media generation to the exact durable track it named so a later
/// `SourceReady`/`Error` for that generation folds its typed outcome onto the
/// same path — and only while that identity is still current, so a stale
/// backend event can never transfer availability to a replacement row.
#[derive(Debug, Clone)]
pub(crate) struct PendingSourceOpen {
    /// The framework media generation this open was stamped with.
    pub(crate) media_generation: u64,
    /// The durable id of the track opened.
    pub(crate) track_id: u64,
    /// The exact durable path opened, checked against the current track's path
    /// before any confirm/demote is applied.
    pub(crate) path: std::path::PathBuf,
    /// The availability generation in force at admission. A `SourceReady`
    /// confirm uses this, so a scan that decided `Missing` afterward (a higher
    /// generation) wins over a late backend success.
    pub(crate) admission_generation: u64,
    /// Set once the matching `SourceReady` confirmed this open Present.
    /// A confirmed record is no longer a *pending* admission awaiting preroll,
    /// but is retained as the current loaded identity so a later typed
    /// source/decode error for the same generation can still demote it.
    pub(crate) confirmed: bool,
}

/// A bounded intent to load a restored current track paused once — but only
/// after its owning-root scan proves it `Present`.
///
/// Opening the file before a scan rules on it would bypass the availability
/// authority, so the load fires on the matching `Present` reconcile. A
/// `Missing`/`Undecodable`/`Unverified` decision retires it with a visible
/// refusal; a stale path, replaced row or superseded session leaves it inert.
#[derive(Debug, Clone)]
pub(crate) struct RestoreIntent {
    /// The durable id of the restored current track.
    pub(crate) track_id: u64,
    /// The exact durable path restored, checked against the live row before the
    /// deferred load fires so a replaced row cannot inherit the intent.
    pub(crate) path: std::path::PathBuf,
    /// The persisted playhead to seek to once the paused load prerolls.
    pub(crate) playhead: f64,
    /// The restore session this intent belongs to; a newer session supersedes
    /// it, so a late reconcile cannot fire a stale restore.
    pub(crate) session: u64,
}

/// The settled result of the deferred boot-time state load.
///
/// Carried from the off-thread [`persistence::acquire`] to the reducer;
/// `generation` is matched against [`MusicApp::boot_load_generation`] to drop a
/// stale completion.
#[derive(Debug, Clone)]
pub struct BootStateReport {
    pub(crate) generation: u64,
    pub(crate) path: std::path::PathBuf,
    pub(crate) doc: Option<persistence::PersistedState>,
    pub(crate) origin: persistence::Origin,
    /// The identity-bound publication authority acquired off-thread,
    /// installed only when this report's generation is still current.
    pub(crate) authority: Option<persistence::SharedAuthority>,
    /// The identity the read observed, carried into the session so an
    /// explicit Retire quarantines exactly that inode and refuses a later
    /// substitution. Installed only for the current generation.
    pub(crate) retire_identity: Option<persistence::ExpectedSourceIdentity>,
}

impl BootStateReport {
    /// A settled report for a refused persistence root: no path, no
    /// document read, and a path-free root-unavailable origin that disables
    /// saving. A stale one is still dropped by the generation guard, so a late
    /// refusal cannot revoke a newer session's authority and a late success
    /// cannot restore it.
    pub(crate) fn root_refused(generation: u64, refusal: persistence::RootRefusal) -> Self {
        Self {
            generation,
            path: std::path::PathBuf::new(),
            doc: None,
            origin: persistence::Origin::PersistenceRootUnavailable(refusal),
            authority: None,
            retire_identity: None,
        }
    }
}

/// The outcome of an off-thread reacquisition after custody loss, an ordinary
/// retry, or an explicit Retire.
#[derive(Debug, Clone)]
pub(crate) enum ReacquireOutcome {
    /// A fresh identity-bound authority. `retired`/`recovered` are set when a
    /// Retire moved the prior bytes into recovery custody.
    Acquired {
        authority: persistence::SharedAuthority,
        recovered: Option<std::path::PathBuf>,
        retired: bool,
    },
    /// No authority: the session stays read-only. A Retire refusal carries a
    /// bounded typed custody reason; a plain retry failure carries none.
    Unavailable {
        retire_refusal: Option<persistence::RetireRefusal>,
    },
}

/// Which reacquisition an off-thread worker performs: an ordinary retry that
/// re-resolves and rereads, or an explicit Retire that quarantines the exact
/// retained path with the exact retained identity.
pub(crate) enum ReacquireKind {
    Retry(enzui_core::app_data::AppDataResolver),
    Retire {
        path: std::path::PathBuf,
        expected: Option<persistence::ExpectedSourceIdentity>,
    },
}

/// A reacquisition result carried under the boot generation guard so a stale
/// refresh drops.
#[derive(Debug, Clone)]
pub struct AuthorityRefresh {
    pub(crate) generation: u64,
    pub(crate) outcome: ReacquireOutcome,
}

impl AuthorityRefresh {
    pub(crate) fn unavailable(
        generation: u64,
        retire_refusal: Option<persistence::RetireRefusal>,
    ) -> Self {
        Self {
            generation,
            outcome: ReacquireOutcome::Unavailable { retire_refusal },
        }
    }
}

pub(crate) const PERSISTENCE_RETRY_ACTION: &str = "Retry";
pub(crate) const PERSISTENCE_RETIRE_ACTION: &str = "Reset saved data";
pub(crate) const DISCARD_AND_CLOSE_ACTION: &str = "Discard unsaved changes and close";

pub(crate) const PLAYLIST_ID_EXHAUSTED_STATUS: &str = "Cannot create another playlist.";

/// The widget-id namespace of the one shared transient-feedback host.
///
/// `ToastHost` derives every id it publishes from this prefix, including the
/// fixed `"{prefix}-live-region"` node that carries the announcement, so a gate
/// looks the region up by a stable id rather than by position.
pub(crate) const TOAST_ID_PREFIX: &str = "music-library-status";

/// The coalesce key every Library acknowledgement shares.
///
/// A push under this key replaces the visible acknowledgement in place, so the
/// newest one wins. A differently keyed push while one is visible is refused
/// (`OverflowPolicy::RejectNew`) rather than queued.
pub(crate) const LIBRARY_STATUS_COALESCE_KEY: &str = "music-library-status";

/// The stable instance key of the standing folder-chooser failure notice.
///
/// The notice derives the ids of its action and dismiss controls from this,
/// so the two controls are routable for an assistive technology.
pub(crate) const FOLDER_FAILURE_BANNER_ID: &str = "music-folder-failure";
/// Id of the standing notice shown while automatic library updates are
/// unavailable or partial.
pub(crate) const WATCH_STATUS_BANNER_ID: &str = "music-watch-status";

/// The authored title of the standing folder-chooser failure notice.
///
/// One constant so the notice and the gate that looks its live region up read
/// the same string; the region's aggregate announcement is
/// `"{severity}: {title}. {body}"`.
pub(crate) const FOLDER_FAILURE_TITLE: &str = "A music folder could not be chosen";

/// The notice action that retries the folder chooser.
pub(crate) const FOLDER_FAILURE_RETRY_ACTION: &str = "Try again";
