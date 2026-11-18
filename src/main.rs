//! Music player for local tracks, podcasts, and internet radio.

#![deny(unconditional_recursion)]

mod art;
mod audio;
mod audio_processing;
#[cfg(feature = "center")]
mod center;
mod context_menu;
mod dispatch;
mod domain;
mod fetch_reaper;
mod layout;
mod library_root;
mod library_watch;
mod message;
pub use message::Message;
mod desktop_open;
mod modal_focus;
mod mpris_runtime;
mod native_drop;
mod opml_transfer;
mod persistence;
mod picker;
mod playlist_io;
mod playlist_transfer;
mod podcasts;
mod radio;
mod radio_catalog;
#[path = "app_parts/reduce_batch1.rs"]
mod reduce_batch1;
#[path = "app_parts/reduce_batch2.rs"]
mod reduce_batch2;
#[path = "app_parts/reduce_batch3.rs"]
mod reduce_batch3;
#[path = "app_parts/reduce_batch4.rs"]
mod reduce_batch4;
mod scanner;
mod shortcuts;
mod spotify;
mod theme;
mod track_details;
mod track_experience;
mod update;
mod views;
mod waveform;

#[cfg(test)]
mod track_availability_tests;

#[cfg(test)]
mod persistence_failure_tests;
#[cfg(test)]
mod test_support;

#[cfg(test)]
mod playlist_import_tests;
#[cfg(test)]
mod playlist_io_tests;
#[cfg(test)]
mod playlist_transfer_tests;
#[cfg(test)]
mod podcast_page_tests;

use enzui_core::focus::{Traversal, traversal_operation as traverse};
use iced::widget::container;
use iced::widget::text_editor;
use iced::{Element, Length, Size, Subscription, Task, Theme, advanced::widget::operate};

use domain::*;
use enzui_core::menu_model::{EntryId, MenuEntryModel, MenuModel, MenuState};
use enzui_core::theme_file::integration::{ThemeEvent, ThemeIntegration};
use enzui_widgets::app_shell::{ShellLayout, ShellTokens, enz_window_settings};
use enzui_widgets::context_menu_host::enz_optional_point_context_menu_host;
use enzui_widgets::menu_button::{self, MenuButtonEvent};
use enzui_widgets::modal_host::{ModalPresentation, ModalPresentationKey};
use enzui_widgets::nav_split::{
    NavSplitGeometry, NavSplitState, SidebarPresentation, SidebarWidth, resolve_for_width,
};
use enzui_widgets::sheet::SheetEvent;
// `ToastTone` decides the live-region politeness the host publishes; the host
// and its expiry schedule are named through module paths at their use sites.
use enzui_widgets::toast::ToastTone;
use library_root::{LibraryEnv, LibraryRoot, missing_fixture_report, resolve_library_root_now};

/// The sidebar's fixed inline width, shared by the widget and every
/// width-resolution call.
///
/// An app-chosen dimension, not a framework token: narrower than
/// `SizingTokens::sidebar_width` (260), and equal to `sidebar_min_width` only
/// by coincidence, since this is a default width rather than a resize minimum.
pub(crate) const SIDEBAR_WIDTH: f32 = 200.0;

/// About 4:3: wide enough to open with the browse pane and the Now Playing
/// column side by side.
pub(crate) const DEFAULT_WINDOW_SIZE: Size = Size::new(1040.0, 780.0);

pub(crate) fn shell_tokens() -> ShellTokens {
    // Flush: the page and the Now Playing column run to the window's edges,
    // and the floating titlebar's frosted band runs square to them.
    ShellTokens {
        titlebar: ShellTokens::default().titlebar.radius(0.0),
        ..ShellTokens::flat_page()
    }
    .edge_inset(0.0)
}

// ---------------------------------------------------------------------------
// Message
// ---------------------------------------------------------------------------

/// Why the backend is telling the reducer to credit a play.
///
/// A play is credited only when the current source's playback reaches one of
/// these authoritative backend outcomes; a button press, queue jump or
/// visibility change is not enough.
// `PartialEq` only: `ThresholdCrossed` carries backend `f64` positions, matching
// every other transport scalar in the app.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PlaybackCompletionReason {
    /// The backend's source reached end of stream; no threshold interpretation.
    EndOfStream,
    /// The backend reports the confirmed position crossed the scrobble
    /// threshold, the smaller of 240 seconds or 50% of the confirmed duration,
    /// while still on the current source. `duration_secs` lets the reducer
    /// reject an event whose source has since changed.
    ThresholdCrossed {
        position_secs: f64,
        duration_secs: f64,
    },
}

/// The podcast discovery surface's state.
///
/// An empty `Results` renders as "no matches"; `Error` is a retryable
/// transport failure and `Unavailable` a worker pool that could not start.
#[derive(Debug, Clone, PartialEq)]
pub enum PodcastSearchState {
    Idle,
    Searching,
    Results(Vec<podcasts::search::PodcastSearchResult>),
    Error { message: String, retryable: bool },
    Unavailable(String),
}

/// Apple's top podcasts, by genre (`None` is the overall chart), for the
/// desktop's country. Each chart loads once per run.
#[derive(Debug, Default)]
pub struct PodcastCharts {
    pub country: String,
    pub genre: Option<u32>,
    pub lists: std::collections::HashMap<Option<u32>, Vec<podcasts::search::PodcastSearchResult>>,
    pub loading: Option<Option<u32>>,
    pub failed: Option<String>,
    /// The country the user picked (saved), as opposed to the guessed one.
    pub chosen_country: Option<String>,
    /// Whether the country list is open.
    pub picking_country: bool,
    /// The podcast whose page is open, by feed URL.
    pub open: Option<String>,
    /// A podcast opened without subscribing: fetched to show its episodes.
    /// Kept after its page closes, so an episode playing from it still
    /// shows in the player.
    pub preview: Option<podcasts::PodcastFeed>,
    /// The podcast of the episode playing, when it came from a preview: it
    /// stays when another podcast is opened, so the player keeps it.
    pub playing_feed: Option<podcasts::PodcastFeed>,
    /// How many episodes the open page lists.
    pub episodes_shown: usize,
    /// Where the podcasts list was scrolled, to return there from a page.
    pub list_scroll: f32,
}

impl PodcastCharts {
    /// The chart on show, once loaded.
    #[must_use]
    pub fn current(&self) -> Option<&Vec<podcasts::search::PodcastSearchResult>> {
        self.lists.get(&self.genre)
    }

    /// The open podcast: a subscription, else the preview.
    #[must_use]
    pub fn opened_feed<'a>(
        &'a self,
        subscriptions: &'a [podcasts::PodcastFeed],
    ) -> Option<&'a podcasts::PodcastFeed> {
        let url = self.open.as_deref()?;
        subscriptions
            .iter()
            .find(|f| f.feed_url == url)
            .or_else(|| self.preview.as_ref().filter(|p| p.feed_url == url))
    }

    /// Podcasts known besides the subscriptions: the preview, and the one an
    /// episode is playing from.
    pub fn unsubscribed_feeds(&self) -> impl Iterator<Item = &podcasts::PodcastFeed> {
        self.preview
            .iter()
            .chain(self.playing_feed.iter().filter(|playing| {
                self.preview
                    .as_ref()
                    .is_none_or(|p| p.feed_url != playing.feed_url)
            }))
    }

    /// A charted show by its feed, from any loaded chart.
    #[must_use]
    pub fn find(&self, feed_url: &str) -> Option<&podcasts::search::PodcastSearchResult> {
        self.lists
            .values()
            .flatten()
            .find(|s| s.feed_url == feed_url)
    }
}

// ---------------------------------------------------------------------------
// App state
// ---------------------------------------------------------------------------

pub(crate) use modal_focus::{ModalPresentationId, manage_feeds_button_id, move_row_focus_task};

/// Which queue row keyboard focus moves to after the row at `removed_pos` is
/// deleted from a queue that now holds `new_len` entries.
///
/// Focus lands on the successor (the occurrence that slid into the gap), or
/// the new last row when the last row was deleted, keyed as
/// [`context_menu::queue_entry_id`] keys the row. Returns `None` for an empty
/// queue: the empty-state hero is not focusable, so traversal stays where it
/// is. Returns the id rather than a position so the focus gate reads the
/// reducer's own decision off the rendered tree.
fn queue_focus_after_removal(
    playback: &domain::PlaybackState,
    removed_pos: usize,
    new_len: usize,
) -> Option<iced::advanced::widget::Id> {
    enzui_core::focus_after_removal::focus_pos_after_removal(removed_pos, new_len)
        .and_then(|pos| playback.entry_id_at(pos))
        .map(crate::context_menu::queue_entry_id)
}

/// The library root the test-only demo seed pretends to have scanned.
///
/// Synthetic so the fixture is independent of the machine running the suite.
#[cfg(test)]
pub(crate) const DEMO_LIBRARY_ROOT: &str = "/demo/Music";

// Only the literal "1" opts into the settle gate: a permissive parse would let a
// stray `MUSIC_APP_SCAN_SETTLED=false` turn a user's startup into a blocking walk.
fn scan_settled_env(value: Option<String>) -> bool {
    matches!(value.as_deref(), Some("1"))
}

/// Wall-clock seconds since the Unix epoch, stamped on a credited play.
///
/// Non-monotonic by design: last-played time follows the user's clock. A
/// pre-1970 clock reads as `0`, the never-played sentinel.
#[must_use]
pub(crate) fn wall_clock_now_unix_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Build the boot [`radio::RadioState`], honouring the dev-only
/// `MUSIC_APP_START_RADIO_MODE` override.
///
/// Takes the value as an argument so tests need not mutate process
/// environment. An unrecognised value leaves the default `Discover`.
fn radio_state_with(_mode_slug: Option<&str>) -> radio::RadioState {
    #[allow(unused_mut)]
    let mut state = radio::RadioState::default();
    // Tests seed the fixture catalog so no test reaches the Radio Browser
    // endpoint; the shipping build starts empty and the boot
    // `Message::RadioCatalogLoad` task fills it.
    #[cfg(test)]
    {
        state
            .lists
            .insert(("test".into(), None), radio::seed_test_catalog());
        state.catalog_status = radio::RadioCatalogState::Loaded { at_unix: 0 };
    }
    state
}

/// [`radio_state_with`] reading the process environment.
fn radio_state_from_env() -> radio::RadioState {
    radio_state_with(
        std::env::var("MUSIC_APP_START_RADIO_MODE")
            .ok()
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty()),
    )
}

/// Walk every configured folder synchronously, folding the results into
/// `library` and reindexing. Returns the number of tracks added.
///
/// Body of the `MUSIC_APP_SCAN_SETTLED` settle gate. It runs the same bounded
/// [`library_watch::scan_root_bounded`] traversal as the off-thread and
/// watcher paths, so the settled frame and a streamed scan agree.
fn settle_scan_now(library: &mut Library) -> usize {
    let folders: Vec<std::path::PathBuf> = library.folders.clone();
    let limits = library_watch::ScanLimits::default();
    let mut added = 0;
    for dir in folders {
        let scan = library_watch::scan_root_bounded(&dir, 0, &limits, &|| false);
        // The walk just read these files, so it settles their availability as
        // an off-thread scan does; generation 0 is outranked by any reconcile.
        // Path order, not `read_dir` order, so a capture's first track is the
        // same on every machine.
        let mut tracks = scan.tracks;
        tracks.sort_by(|a, b| a.path.cmp(&b.path));
        for track in tracks {
            let path = track.path.clone();
            library.add_track(track);
            library.decide_availability(path, domain::TrackAvailability::Present, 0);
            added += 1;
        }
        for path in scan.undecodable {
            library.decide_availability(path, domain::TrackAvailability::Undecodable, 0);
        }
    }
    library.reindex();
    added
}

const PODCAST_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(50);

/// How close to the confirmed episode end a resume point is cleared rather
/// than saved, so a finished episode restarts from the beginning.
const EPISODE_RESUME_END_MARGIN_SECS: f64 = 15.0;
const SLEEP_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(1);

/// Retry cadence for a Busy-refused playlist import or export; the wake is
/// armed only while an intent awaits retry.
const PLAYLIST_RETRY_INTERVAL: std::time::Duration = std::time::Duration::from_millis(300);

/// The suggested `SaveFile` name for a playlist export: the display name as a
/// `.m3u8` filename with path separators and control bytes replaced by `_`.
fn suggested_export_name(display_name: &str) -> String {
    let trimmed = display_name.trim();
    let base = if trimmed.is_empty() {
        "playlist"
    } else {
        trimmed
    };
    let safe: String = base
        .chars()
        .map(|c| {
            if c == '/' || c == '\\' || c.is_control() {
                '_'
            } else {
                c
            }
        })
        .collect();
    format!("{safe}.m3u8")
}

/// A bounded one-line summary of an import's counts.
fn import_counts_summary(counts: &domain::ImportCounts) -> String {
    let mut parts = vec![format!("Imported {}", counts.imported)];
    if counts.missing > 0 {
        parts.push(format!("{} missing", counts.missing));
    }
    if counts.duplicate > 0 {
        parts.push(format!("{} duplicate", counts.duplicate));
    }
    if counts.refused > 0 {
        parts.push(format!("{} refused", counts.refused));
    }
    parts.join(", ")
}

#[path = "app_parts/boot_a.rs"]
mod boot_a;
#[path = "app_parts/boot_b.rs"]
mod boot_b;

#[path = "app_parts/menus.rs"]
mod menus;
#[path = "app_parts/persist.rs"]
mod persist;
#[path = "app_parts/playback.rs"]
mod playback;
#[path = "app_parts/transfer.rs"]
mod transfer;

#[path = "app_parts/content_pane.rs"]
mod content_pane;
#[path = "app_parts/feedback.rs"]
mod feedback;
#[path = "app_parts/podcast_discover.rs"]
mod podcast_discover;
#[path = "app_parts/shell.rs"]
mod shell;
#[path = "app_parts/view_root.rs"]
mod view_root;

pub(crate) use shell::{
    SORT_MENU_RATED_ID, nav_menu_model, nav_menu_view, now_playing_menu_model,
    queue_more_menu_model, sort_menu_entries, sort_menu_model, sort_mode_from_entry_id,
};
pub(crate) use shell::{
    clamp_playlist_rename_draft, context_menu_action_message, dispatch_now_playing_menu,
    dispatch_queue_more_menu,
};

#[path = "app_parts/app_state.rs"]
mod app_state;
#[cfg(test)]
#[path = "app_parts/fixtures.rs"]
mod fixtures;
#[path = "app_parts/jobs.rs"]
mod jobs;
#[path = "app_parts/message_kinds.rs"]
mod message_kinds;
#[path = "app_parts/state_types.rs"]
mod state_types;
pub(crate) use app_state::MusicApp;
pub(crate) use jobs::{ExportJob, ImportJob, JobState, MprisSourceIdentity};
pub(crate) use state_types::{
    AuthorityRefresh, BootStateReport, DISCARD_AND_CLOSE_ACTION, FOLDER_FAILURE_BANNER_ID,
    FOLDER_FAILURE_RETRY_ACTION, FOLDER_FAILURE_TITLE, LIBRARY_STATUS_COALESCE_KEY,
    PERSISTENCE_RETIRE_ACTION, PERSISTENCE_RETRY_ACTION, PLAYLIST_ID_EXHAUSTED_STATUS,
    PendingSourceOpen, PersistenceNotice, ReacquireKind, ReacquireOutcome, RestoreIntent,
    TOAST_ID_PREFIX, WATCH_STATUS_BANNER_ID,
};

fn main() -> iced::Result {
    enzui_core::crash_report::CrashReporter::for_app("music-app", env!("CARGO_PKG_VERSION"));
    use enzui_widgets::single_instance::{InstanceStart, SingleInstance};
    let (instance, intent) = match SingleInstance::start(DESKTOP_APP_ID) {
        InstanceStart::Forwarded => return Ok(()),
        InstanceStart::Owner(instance, intent) => (instance, intent),
    };
    let initial = std::cell::RefCell::new(Some((instance, intent)));
    enzui_widgets::app_setup::run_with_services(
        iced::application(
            move || {
                let (mut app, task) = MusicApp::boot_persisting();
                let (instance, intent) =
                    initial.borrow_mut().take().expect("application boots once");
                app.desktop_instance = Some(instance);
                let open = app.desktop_intents([intent]);
                (app, Task::batch([task, open]))
            },
            MusicApp::update,
            MusicApp::view,
        )
        .title("Music")
        .theme(MusicApp::theme)
        .subscription(MusicApp::subscription)
        .window(with_desktop_identity(music_window_settings())),
        enzui_widgets::app_setup::AppServices::default()
            .traversal(enzui_widgets::app_setup::TraversalBinding::Disabled)
            .accessibility_actions(false),
    )
}

// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------

#[cfg(test)]
mod library_watch_tests;

// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
//

// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
//

/// The Wayland application identity, the basename of the shipped
/// `io.github.enzui.Music.desktop`, so the compositor and the desktop entry
/// agree on window grouping, icon and `StartupWMClass`.
pub(crate) const DESKTOP_APP_ID: &str = "io.github.enzui.Music";

fn with_desktop_identity(settings: iced::window::Settings) -> iced::window::Settings {
    enzui_widgets::app_shell::DesktopApplicationIdentity::new(DESKTOP_APP_ID)
        .expect("BUG: DESKTOP_APP_ID must be a portable desktop-entry basename")
        .apply_to_window_settings(settings)
}

fn music_window_settings() -> iced::window::Settings {
    iced::window::Settings {
        size: DEFAULT_WINDOW_SIZE,
        min_size: Some(Size::new(340.0, 480.0)),
        ..enz_window_settings(ShellLayout::FloatingTitlebar)
    }
}
