//! Import/export portal jobs and MPRIS source identity.

use super::*;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum MprisSourceIdentity {
    Local {
        generation: u64,
        track_id: u64,
        occurrence: Option<domain::QueueEntryId>,
    },
    Radio {
        generation: u64,
        station_uuid: String,
    },
    Episode {
        generation: u64,
        feed_url: String,
        guid: String,
    },
}

impl MprisSourceIdentity {
    pub(crate) fn generation(&self) -> u64 {
        match self {
            Self::Local { generation, .. }
            | Self::Radio { generation, .. }
            | Self::Episode { generation, .. } => *generation,
        }
    }
}

/// Whether a retained playlist worker intent is running or awaiting a retry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum JobState {
    /// Submitted to the owner and awaiting its settlement.
    InFlight,
    /// Refused as Busy; awaiting one coalesced retry wake.
    AwaitingRetry,
}

/// The accepted import file, its request generation, and its retry state.
#[derive(Debug, Clone)]
pub(crate) struct ImportJob {
    pub(crate) generation: u64,
    pub(crate) path: std::path::PathBuf,
    pub(crate) state: JobState,
}

/// An owned export snapshot keyed by request generation.
#[derive(Debug, Clone)]
pub(crate) struct ExportJob {
    /// The playlist this export was requested for (stable id).
    pub(crate) playlist_id: u64,
    /// The playlist's display name at snapshot time, for the success message.
    pub(crate) display_name: String,
    /// The exact durable rows captured before the portal opened.
    pub(crate) paths: Vec<std::path::PathBuf>,
    /// The portal grant, set once `SaveFile` completes. `None` while the dialog
    /// is still open (expendable on close); `Some` carries publication debt.
    pub(crate) grant: Option<enzui_core::download_destination::DownloadDestination>,
    /// The publication retry state, meaningful once `grant` is set.
    pub(crate) state: JobState,
}
