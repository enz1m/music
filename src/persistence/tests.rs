//! Persistence gates observe real files and real relaunches.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use enzui_widgets::file_chooser::{FileResult, FileUri, PortalOutcome};

use super::*;
use crate::domain::{ReplayGain, Track};
use crate::persistence;
use crate::{Message, MusicApp};

static NEXT_TMP: AtomicU32 = AtomicU32::new(0);

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "music-app-1275-{tag}-{}-{}",
            std::process::id(),
            NEXT_TMP.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("create sandbox");
        Self(path)
    }

    fn state_file(&self) -> PathBuf {
        self.0.join("music-app").join("state.json")
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn app_at(path: &Path) -> MusicApp {
    // Production supplies an existing private parent before opening authority.
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("create state parent dir");
    }
    MusicApp::boot_with(false, false, Vec::new())
        .0
        .with_persistence(path.to_path_buf())
}

fn seed_state(path: &Path, bytes: impl AsRef<[u8]>) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::write(path, bytes).expect("seed state file");
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).expect("chmod 0600");
}

fn file_identity(path: &Path) -> persistence::ExpectedSourceIdentity {
    use std::os::unix::fs::MetadataExt;
    let meta = std::fs::metadata(path).expect("state file metadata");
    persistence::ExpectedSourceIdentity::from_dev_ino(meta.dev(), meta.ino())
}

fn recovery_sibling(state_path: &Path, bytes: &[u8]) -> Option<PathBuf> {
    let parent = state_path.parent()?;
    for entry in std::fs::read_dir(parent).ok()?.flatten() {
        let candidate = entry.path();
        if candidate == *state_path {
            continue;
        }
        if std::fs::read(&candidate).ok().as_deref() == Some(bytes) {
            return Some(candidate);
        }
    }
    None
}

fn track(path: &str, added_at: u64) -> Track {
    Track {
        id: 0,
        path: PathBuf::from(path),
        title: "Persist Me".into(),
        artist: "Fixture Artist".into(),
        album: "Fixture Album".into(),
        album_artist: "Fixture Artist".into(),
        track_no: 1,
        disc_no: 1,
        year: 2026,
        genre: "Test".into(),
        format: "FLAC".into(),
        duration_secs: 180.0,
        bitrate_kbps: 900,
        sample_rate: 48_000,
        channels: 2,
        bit_depth: 24,
        cover_path: None,
        cover: None,
        added_at,
        play_count: 0,
        last_played_seq: 0,
        last_played_unix: 0,
        replay_gain: ReplayGain::default(),
    }
}

#[derive(Debug, PartialEq, Eq)]
enum FileSnapshot {
    Missing,
    Present {
        bytes: Vec<u8>,
        inode: u64,
        modified: std::time::SystemTime,
    },
}

fn file_snapshot(path: &Path) -> FileSnapshot {
    use std::os::unix::fs::MetadataExt;
    match std::fs::metadata(path) {
        Ok(meta) => FileSnapshot::Present {
            bytes: std::fs::read(path).expect("read snapshotted state"),
            inode: meta.ino(),
            modified: meta.modified().expect("mtime"),
        },
        Err(_) => FileSnapshot::Missing,
    }
}

fn serialized(doc: &PersistedState) -> Vec<u8> {
    serde_json::to_vec_pretty(doc).expect("serialize fixture")
}

#[path = "tests/authority_tests.rs"]
mod authority_tests;
#[path = "tests/bounds_tests.rs"]
mod bounds_tests;
#[path = "tests/roundtrip_tests.rs"]
mod roundtrip_tests;
