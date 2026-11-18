//! Music folder discovery with an exclusive capture fixture override.
//!
//! Normal launches prefer `XDG_MUSIC_DIR`, then the configured freedesktop
//! music directory, then `~/Music`. Only existing directories are accepted.
//! A missing explicit fixture produces `MissingFixture` without falling back
//! to the user's collection.

use std::path::{Path, PathBuf};

/// The environment values that can name a music folder, read once in
/// [`LibraryEnv::from_env`] and passed as data so tests need not mutate
/// process environment.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct LibraryEnv {
    /// `MUSIC_APP_LIBRARY_DIR`, the capture/dev fixture override.
    pub fixture: Option<PathBuf>,
    /// The freedesktop music folder, from the environment or user-dirs settings.
    pub xdg_music_dir: Option<PathBuf>,
    /// `HOME`.
    pub home: Option<PathBuf>,
}

/// The exact variable names [`LibraryEnv::from_env`] reads, in priority order.
/// Public so a test can assert the names rather than restate them.
pub(crate) const LIBRARY_ENV_KEYS: [&str; 3] = ["MUSIC_APP_LIBRARY_DIR", "XDG_MUSIC_DIR", "HOME"];

impl LibraryEnv {
    /// Read environment overrides and the configured freedesktop music directory.
    pub fn from_env() -> Self {
        let mut env = Self::from_lookup(|key| std::env::var_os(key).map(PathBuf::from));
        if env.xdg_music_dir.is_none() {
            env.xdg_music_dir = directories::UserDirs::new()
                .and_then(|directories| directories.audio_dir().map(Path::to_path_buf));
        }
        env
    }

    /// [`LibraryEnv::from_env`] against an arbitrary lookup, so a test can pin
    /// *which keys* are read without touching the process environment.
    pub fn from_lookup(lookup: impl Fn(&str) -> Option<PathBuf>) -> Self {
        let get = |key: &str| lookup(key).filter(|p| !p.as_os_str().is_empty());
        Self {
            fixture: get(LIBRARY_ENV_KEYS[0]),
            xdg_music_dir: get(LIBRARY_ENV_KEYS[1]),
            home: get(LIBRARY_ENV_KEYS[2]),
        }
    }
}

/// What the resolution decided. An enum rather than `Option<PathBuf>` so the
/// boot path can fail a capture on `MissingFixture` and configure nothing on
/// `NoMusicDir`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LibraryRoot {
    /// `MUSIC_APP_LIBRARY_DIR` named an existing directory. Captures and dev
    /// runs land here; nothing under `$HOME` is scanned.
    Fixture(PathBuf),
    /// `MUSIC_APP_LIBRARY_DIR` was set and is not a directory; never falls
    /// back.
    MissingFixture(PathBuf),
    /// The user's own music folder, which exists.
    UserMusic(PathBuf),
    /// No music folder could be resolved. Configure nothing.
    NoMusicDir,
}

impl LibraryRoot {
    /// The folders `Library::folders` should be seeded with; empty for
    /// `MissingFixture` and `NoMusicDir`.
    pub fn folders(&self) -> Vec<PathBuf> {
        match self {
            LibraryRoot::Fixture(p) | LibraryRoot::UserMusic(p) => vec![p.clone()],
            LibraryRoot::MissingFixture(_) | LibraryRoot::NoMusicDir => Vec::new(),
        }
    }
}

/// Resolve [`LibraryEnv`] against a directory-existence predicate.
///
/// A relative fixture path is resolved against the crate root compiled in at
/// build time, so `capture.env` can say
/// `MUSIC_APP_LIBRARY_DIR=fixture-library`.
pub(crate) fn resolve_library_root(
    env: &LibraryEnv,
    crate_root: &Path,
    is_dir: &dyn Fn(&Path) -> bool,
) -> LibraryRoot {
    if let Some(fixture) = &env.fixture {
        let path = if fixture.is_absolute() {
            fixture.clone()
        } else {
            crate_root.join(fixture)
        };
        // Exclusive: no fallback to $HOME.
        return if is_dir(&path) {
            LibraryRoot::Fixture(path)
        } else {
            LibraryRoot::MissingFixture(path)
        };
    }

    if let Some(xdg) = &env.xdg_music_dir
        && is_dir(xdg)
    {
        return LibraryRoot::UserMusic(xdg.clone());
    }

    if let Some(home) = &env.home {
        let music = home.join("Music");
        if is_dir(&music) {
            return LibraryRoot::UserMusic(music);
        }
    }

    LibraryRoot::NoMusicDir
}

/// [`resolve_library_root`] with the real filesystem and this crate's root.
pub(crate) fn resolve_library_root_now(env: &LibraryEnv) -> LibraryRoot {
    resolve_library_root(env, Path::new(env!("CARGO_MANIFEST_DIR")), &|p: &Path| {
        p.is_dir()
    })
}

// An explicit missing library must never fall back to the user collection.
pub(crate) fn missing_fixture_report(path: &Path) -> String {
    format!(
        "MUSIC_APP_LIBRARY_DIR names no directory: {} — \
         refusing to fall back to the user's music folder",
        path.display()
    )
}

/// A library folder as the Library card shows it: a folder inside `app_dir`
/// relative to it, `$HOME` collapsed to `~`, everything else in full.
///
/// Committed screenshots must not carry the capture machine's filesystem or
/// user name. A folder under the crate directory can only be a developer
/// fixture, since real folders arrive from the XDG portal picker.
pub(crate) fn display_folder_path(
    path: &Path,
    home: Option<&Path>,
    app_dir: Option<&Path>,
) -> String {
    if let Some(app_dir) = app_dir.filter(|d| !d.as_os_str().is_empty())
        && let Ok(rest) = path.strip_prefix(app_dir)
        && !rest.as_os_str().is_empty()
    {
        return rest.display().to_string();
    }
    if let Some(home) = home.filter(|h| !h.as_os_str().is_empty()) {
        if path == home {
            return "~".to_string();
        }
        if let Ok(rest) = path.strip_prefix(home) {
            return format!("~/{}", rest.display());
        }
    }
    path.display().to_string()
}

/// [`display_folder_path`] with this build's crate directory, the only
/// `app_dir` the shipping code passes.
pub(crate) fn display_folder_path_now(path: &Path, home: Option<&Path>) -> String {
    display_folder_path(path, home, Some(Path::new(env!("CARGO_MANIFEST_DIR"))))
}
