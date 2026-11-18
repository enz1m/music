//! Routed persistence-failure evidence.
//!
//! Each test observes a distinct shipping mechanism: typed task outcomes and
//! retained debt, stale/superseded reduction, root-level feedback and retry,
//! truthful boot origins, and the close veto/discard paths.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use enzui_core::collect::collect_frame;
use enzui_core::semantics::Role;
use iced::Size;

use crate::domain::View;
use crate::persistence::{self, PersistenceCompletion, PersistenceFailure, PersistenceQueue};
use crate::{
    DISCARD_AND_CLOSE_ACTION, Message, MusicApp, PERSISTENCE_RETRY_ACTION, PersistenceNotice,
};

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let path = std::env::temp_dir().join(format!(
            "music-app-2568-{tag}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("create persistence sandbox");
        Self(path)
    }

    fn state_file(&self) -> PathBuf {
        self.0.join("music-app/state.json")
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn app_at(path: &Path, queue: PersistenceQueue) -> MusicApp {
    // Seed before opening so the authority retains the existing inode rather
    // than granting create-on-first-publication custody.
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("create state parent dir");
    }
    let seed = MusicApp::boot_with(false, false, Vec::new()).0;
    persistence::save_to(path, &persistence::snapshot(&seed)).expect("seed old state");
    let mut app = MusicApp::boot_with(false, false, Vec::new())
        .0
        .with_persistence(path.to_path_buf());
    app.persistence_queue = queue;
    app
}

fn nodes(app: &MusicApp, size: Size) -> Vec<enzui_core::collect::CollectedNode> {
    collect_frame(MusicApp::view(app), size)
        .collected()
        .to_vec()
}

fn persistence_alerts(app: &MusicApp, size: Size) -> Vec<String> {
    nodes(app, size)
        .into_iter()
        .filter(|node| node.semantics.role == Role::Alert)
        .filter_map(|node| node.semantics.label)
        // Localized severity labels wrap their arguments in bidi isolates.
        .map(|label| label.replace(['\u{2068}', '\u{2069}'], ""))
        .filter(|label| {
            label.contains("Changes are not saved")
                || label.contains("Music data is read-only")
                || label.contains("saved music data could not be read")
        })
        .collect()
}

fn button_count(app: &MusicApp, size: Size, label: &str) -> usize {
    nodes(app, size)
        .into_iter()
        .filter(|node| {
            node.semantics.role == Role::Button && node.semantics.label.as_deref() == Some(label)
        })
        .count()
}

/// Whether the storage notice publishes its own close control: every notice
/// can be closed (it then stays hidden until the storage state changes).
fn persistence_notice_closable(app: &MusicApp, size: Size) -> bool {
    let wanted = enzui_core::identity::derive_id(crate::feedback::PERSISTENCE_NOTICE_ID, "dismiss");
    nodes(app, size).into_iter().any(|node| {
        node.id.as_ref() == Some(&wanted)
            && node.semantics.role == Role::Button
            && node.semantics.supports_default_action
    })
}

fn route_retry(app: &mut MusicApp, size: Size) {
    let mut simulator =
        iced_test::Simulator::with_size(iced::Settings::default(), size, MusicApp::view(app));
    simulator
        .click(PERSISTENCE_RETRY_ACTION)
        .expect("the rendered persistence Retry action must be clickable");
    let messages: Vec<_> = simulator.into_messages().collect();
    assert!(
        matches!(messages.as_slice(), [Message::RetryPersistence]),
        "Retry must route exactly once, got {messages:?}"
    );
    app.step(messages.into_iter().next().expect("one Retry message"));
}

fn task_window_actions(task: iced::Task<Message>) -> (usize, usize) {
    use iced_test::futures::futures::StreamExt;
    use iced_test::runtime::{Action, window::Action as WindowAction};

    let Some(mut stream) = iced_test::runtime::task::into_stream(task) else {
        return (0, 0);
    };
    let mut latest = 0;
    let mut close = 0;
    while let Some(action) =
        iced_test::futures::futures::executor::block_on(StreamExt::next(&mut stream))
    {
        match action {
            Action::Window(WindowAction::GetLatest(sender)) => {
                latest += 1;
                let _ = sender.send(Some(iced::window::Id::unique()));
            }
            Action::Window(WindowAction::Close(_)) => close += 1,
            _ => {}
        }
    }
    (latest, close)
}

// Close waits for the podcast pool to retire, so it arrives across poll wakes
// rather than inside the triggering update.
fn drive_close(app: &mut MusicApp, initial: iced::Task<Message>) -> (usize, usize) {
    use std::time::{Duration, Instant};
    let (mut latest, mut close) = task_window_actions(initial);
    let deadline = Instant::now() + Duration::from_secs(2);
    while close == 0 && app.close_after_save && Instant::now() < deadline {
        // Each retained pool keeps its own wake while its close debt is unsettled.
        for wake in [
            Message::PodcastPoll,
            Message::RadioCatalogPoll,
            Message::PodcastSearchPoll,
            Message::PodcastArtworkPoll,
        ] {
            let (l, c) = task_window_actions(app.update(wake));
            latest += l;
            close += c;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    (latest, close)
}

#[test]
fn failed_playlist_save_retains_newest_debt_and_routed_retry_repairs_disk() {
    let dir = TempDir::new("retry");
    let path = dir.state_file();
    let queue = PersistenceQueue::default();
    let mut app = app_at(&path, queue.clone());
    let old_bytes = std::fs::read(&path).expect("seed bytes");
    let old_baseline = app
        .persist
        .last_saved_for_test()
        .expect("seed baseline")
        .materialize();
    let hostile = format!(
        "{}\nPermission denied (os error 13)\u{202e}hidden",
        path.display()
    );
    queue.inject_write_error(std::io::ErrorKind::PermissionDenied, hostile.clone());

    app.step(Message::CreatePlaylist);
    app.step(Message::CreatePlaylist);

    assert_eq!(
        app.library.playlists.len(),
        2,
        "both edits remain in memory"
    );
    assert_eq!(std::fs::read(&path).expect("old bytes survive"), old_bytes);
    assert_eq!(
        app.persist
            .last_saved_for_test()
            .expect("durable baseline")
            .materialize(),
        old_baseline,
        "failed I/O must not advance the durable baseline"
    );
    let debt = app
        .pending_save
        .as_ref()
        .expect("newest snapshot remains debt");
    assert_eq!(
        debt.document().materialize().playlists.len(),
        2,
        "the retained debt must be the newest failed snapshot"
    );

    for (route, size) in [
        (View::Home, Size::new(1400.0, 900.0)),
        (View::Radio, Size::new(1400.0, 900.0)),
        (View::Home, Size::new(375.0, 812.0)),
        (View::Radio, Size::new(375.0, 812.0)),
    ] {
        let _ = app.update(Message::SelectView(route));
        let _ = app.update(Message::WindowResized(size));
        let alerts = persistence_alerts(&app, size);
        assert_eq!(alerts.len(), 1, "{route:?} at {size:?}: {alerts:?}");
        let alert = &alerts[0];
        assert!(
            alert.starts_with("Error:"),
            "the failure banner must use Danger urgency: {alert:?}"
        );
        assert!(
            alert.len() <= 160,
            "safe feedback must stay bounded: {alert:?}"
        );
        assert!(!alert.contains(&hostile));
        assert!(!alert.contains(&*path.to_string_lossy()));
        assert!(!alert.contains("os error") && !alert.contains("errno"));
        assert!(!alert.chars().any(char::is_control));
        assert_eq!(button_count(&app, size, PERSISTENCE_RETRY_ACTION), 1);
        assert!(persistence_notice_closable(&app, size));
    }

    queue.clear_injected_write_error();
    route_retry(&mut app, Size::new(375.0, 812.0));

    assert!(
        app.pending_save.is_none(),
        "Saved retires the matching debt"
    );
    assert!(
        app.persistence_notice.is_none(),
        "Saved clears failure feedback"
    );
    let reloaded = MusicApp::boot_with(false, false, Vec::new())
        .0
        .with_persistence(path);
    assert_eq!(
        reloaded.library.playlists.len(),
        2,
        "Retry must land the newest snapshot on disk"
    );
}

#[test]
fn superseded_and_stale_completions_are_quiet_and_keep_newest_debt() {
    let dir = TempDir::new("ordering");
    let path = dir.state_file();
    let queue = PersistenceQueue::default();
    let mut app = app_at(&path, queue);

    let older_task = app.update(Message::CreatePlaylist);
    let older = app.pending_save.as_ref().unwrap().ticket().generation();
    let newer_task = app.update(Message::CreatePlaylist);
    let newest = app.pending_save.as_ref().unwrap().ticket().generation();
    assert!(newest > older);

    let older_messages = crate::test_support::drain_task(older_task);
    assert!(matches!(
        older_messages.as_slice(),
        [Message::PersistenceFinished {
            outcome: PersistenceCompletion::Superseded,
            ..
        }]
    ));
    for message in older_messages {
        app.step(message);
    }
    assert_eq!(
        app.pending_save.as_ref().unwrap().ticket().generation(),
        newest
    );
    assert!(app.persistence_notice.is_none(), "Superseded is quiet");

    app.step(Message::PersistenceFinished {
        generation: newest,
        outcome: PersistenceCompletion::Superseded,
    });
    assert_eq!(
        app.pending_save.as_ref().unwrap().ticket().generation(),
        newest
    );
    assert!(
        app.persistence_notice.is_none(),
        "matching Superseded is quiet"
    );

    app.step(Message::PersistenceFinished {
        generation: older,
        outcome: PersistenceCompletion::Error(PersistenceFailure::injected(
            std::io::ErrorKind::Other,
            "stale hostile diagnostic\n/tmp/private (os error 5)",
        )),
    });
    assert!(app.persistence_notice.is_none(), "stale Error is ignored");
    assert_eq!(
        app.pending_save.as_ref().unwrap().ticket().generation(),
        newest
    );

    for message in crate::test_support::drain_task(newer_task) {
        app.step(message);
    }
    assert!(app.pending_save.is_none());
}

#[test]
fn future_and_corrupt_boot_origins_are_visible_and_future_data_stays_read_only() {
    let future_dir = TempDir::new("future");
    let future_path = future_dir.state_file();
    std::fs::create_dir_all(future_path.parent().unwrap()).expect("future parent");
    let future = format!(
        r#"{{"version":{},"playlists":[]}}"#,
        persistence::FORMAT_VERSION + 1
    );
    std::fs::write(&future_path, &future).expect("future state");
    let mut future_app = MusicApp::boot_with(false, false, Vec::new())
        .0
        .with_persistence(future_path.clone());

    let alerts = persistence_alerts(&future_app, Size::new(1400.0, 900.0));
    assert_eq!(
        alerts.len(),
        1,
        "future boot needs one visible alert: {alerts:?}"
    );
    assert!(alerts[0].starts_with("Error:"));
    assert!(alerts[0].contains("read-only"));
    assert!(alerts[0].contains("Nothing you change in this session will be saved"));
    assert!(persistence_notice_closable(
        &future_app,
        Size::new(1400.0, 900.0)
    ));
    // Closing it only hides it: the session stays read-only.
    future_app.step(Message::HideNotice(crate::feedback::PERSISTENCE_NOTICE_ID));
    assert!(persistence_alerts(&future_app, Size::new(1400.0, 900.0)).is_empty());
    future_app.step(Message::CreatePlaylist);
    assert_eq!(std::fs::read_to_string(&future_path).unwrap(), future);

    let corrupt_dir = TempDir::new("corrupt");
    let corrupt_path = corrupt_dir.state_file();
    std::fs::create_dir_all(corrupt_path.parent().unwrap()).expect("corrupt parent");
    std::fs::write(&corrupt_path, b"{not json").expect("corrupt state");
    // Owner-only, so the publication authority retains the file before the read
    // exposes its corruption; corrupt bytes then stay read-only until Retire.
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&corrupt_path, std::fs::Permissions::from_mode(0o600))
            .expect("chmod 0600");
    }
    let mut corrupt_app = MusicApp::boot_with(false, false, Vec::new())
        .0
        .with_persistence(corrupt_path);
    assert!(matches!(
        corrupt_app.persistence_notice.as_ref(),
        Some(PersistenceNotice::RefusedReadOnly(
            persistence::RefusalKind::Corrupt
        ))
    ));
    // Collected at the compact shell the state describes, so the wide now-playing
    // row cannot starve the origin banner out of layout.
    let compact = Size::new(375.0, 812.0);
    let _ = corrupt_app.update(Message::WindowResized(compact));
    let alerts = persistence_alerts(&corrupt_app, compact);
    assert_eq!(
        alerts.len(),
        1,
        "the damage banner must be visible: {alerts:?}"
    );
    assert!(alerts[0].contains("damaged"));
}

#[test]
fn failed_close_is_vetoed_until_retry_succeeds_or_the_user_discards() {
    let dir = TempDir::new("close");
    let path = dir.state_file();
    let queue = PersistenceQueue::default();
    let mut app = app_at(&path, queue.clone());
    drop(app.update(Message::CreatePlaylist));
    queue.inject_write_error(std::io::ErrorKind::Other, "close write failed");

    let failed = app.update(Message::Window(
        enzui_widgets::title_bar_gesture::WindowCommand::Close,
    ));
    assert_eq!(
        task_window_actions(failed),
        (0, 0),
        "failed save vetoes close"
    );
    assert!(app.close_after_save);
    assert!(app.pending_save.is_some());
    assert_eq!(
        button_count(&app, Size::new(1400.0, 900.0), DISCARD_AND_CLOSE_ACTION),
        1,
        "a vetoed close must offer an explicit discard route"
    );

    queue.clear_injected_write_error();
    let retry = app.update(Message::RetryPersistence);
    let completion = crate::test_support::drain_task(retry)
        .into_iter()
        .next()
        .expect("retry completion");
    let close = app.update(completion);
    assert_eq!(
        drive_close(&mut app, close),
        (1, 1),
        "successful Retry resumes close once the podcast pool retires"
    );

    let discard_dir = TempDir::new("discard");
    let discard_path = discard_dir.state_file();
    let discard_queue = PersistenceQueue::default();
    let mut discard_app = app_at(&discard_path, discard_queue.clone());
    drop(discard_app.update(Message::CreatePlaylist));
    discard_queue.inject_write_error(std::io::ErrorKind::Other, "still failed");
    drop(discard_app.update(Message::Window(
        enzui_widgets::title_bar_gesture::WindowCommand::Close,
    )));
    let mut simulator = iced_test::Simulator::with_size(
        iced::Settings::default(),
        Size::new(1400.0, 900.0),
        MusicApp::view(&discard_app),
    );
    simulator
        .click(DISCARD_AND_CLOSE_ACTION)
        .expect("the explicit discard control must be clickable");
    let messages: Vec<_> = simulator.into_messages().collect();
    assert!(
        matches!(messages.as_slice(), [Message::DiscardUnsavedAndClose]),
        "discard must route exactly once, got {messages:?}"
    );
    let close = discard_app.update(messages.into_iter().next().unwrap());
    assert_eq!(
        drive_close(&mut discard_app, close),
        (1, 1),
        "explicit discard permits close once the podcast pool retires"
    );
}
