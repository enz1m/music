//! The built-in player: Spotify Soloist, Spotify's own headless player,
//! run by Music as a Spotify Connect device named [`DEVICE_NAME`].
//!
//! Spotify refuses audio to unofficial players, so playback on this
//! computer needs Soloist. Its terms ask apps not to ship it, so Music
//! downloads the official build from Spotify on request, keeps the user's
//! Soloist API key in the desktop keyring, and starts the player whenever
//! Spotify is signed in. Soloist pairs once, from a Spotify app on the same
//! network, and keeps that session in its data directory.
//!
//! Music watches the player's local WebSocket API only for its sign-in
//! state; playback itself is driven through the Web API like any device.

use std::io::{BufRead, BufReader, Read};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use enzui_services::credential_store::{CredentialKey, CredentialOutcome, Secret, connect_for_app};

/// The name the device shows under in every Spotify app.
pub const DEVICE_NAME: &str = "Music";
/// Where a Soloist API key is made.
pub const API_KEY_URL: &str = "https://developer.spotify.com/dashboard/soloist";
/// Spotify's page about Soloist builds.
pub const DOWNLOADS_URL: &str =
    "https://developer.spotify.com/documentation/soloist/reference/downloads-and-updates";
/// How long Spotify's builds keep working.
pub const BUILD_LIFETIME_DAYS: u64 = 90;
/// Soloist's exit status for an expired build.
const EXIT_EXPIRED: i32 = 10;

/// Where Music keeps Soloist: the program, its data (device identity and
/// the paired session) and its cache.
#[derive(Debug, Clone)]
pub struct Paths {
    pub bin: PathBuf,
    pub data: PathBuf,
    pub cache: PathBuf,
}

impl Paths {
    /// Under the user's data and cache homes.
    #[must_use]
    pub fn standard() -> Option<Self> {
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let data_home = std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .or_else(|| home.as_ref().map(|h| h.join(".local/share")))?;
        let cache_home = std::env::var_os("XDG_CACHE_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .or_else(|| home.as_ref().map(|h| h.join(".cache")))?;
        let root = data_home.join("music-app").join("soloist");
        Some(Self {
            bin: root.join("bin"),
            data: root.join("data"),
            cache: cache_home.join("music-app").join("soloist"),
        })
    }

    /// The installed program, if there is one.
    #[must_use]
    pub fn program(&self) -> Option<PathBuf> {
        let ours = self.bin.join("soloist");
        if is_executable(&ours) {
            return Some(ours);
        }
        // One the user installed themselves, as Spotify's guide shows.
        ["/usr/local/bin/soloist", "/usr/bin/soloist"]
            .iter()
            .map(PathBuf::from)
            .find(|p| is_executable(p))
    }
}

fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

/// The official build for this machine, when Spotify makes one.
#[must_use]
pub fn build_url() -> Option<&'static str> {
    match std::env::consts::ARCH {
        "x86_64" => Some("https://soloist-builds.spotifycdn.com/soloist_release_x86_64.tar.gz"),
        "aarch64" => Some("https://soloist-builds.spotifycdn.com/soloist_release_arm64.tar.gz"),
        "arm" => Some("https://soloist-builds.spotifycdn.com/soloist_release_arm32.tar.gz"),
        _ => None,
    }
}

/// Download Spotify's build into `paths.bin`, replacing an older one.
///
/// # Errors
/// A sentence for the user when the download or unpacking fails.
pub fn install(paths: &Paths) -> Result<(), String> {
    let url = build_url().ok_or("Spotify doesn't make Soloist for this kind of computer.")?;
    std::fs::create_dir_all(&paths.bin).map_err(|_| "Music couldn't create its player folder.")?;
    let archive = paths.bin.join("soloist.tar.gz.part");
    {
        let mut file = std::fs::File::create(&archive)
            .map_err(|_| "Music couldn't save the download.".to_owned())?;
        let mut easy = curl::easy::Easy::new();
        easy.url(url).map_err(|_| "The download couldn't start.")?;
        easy.follow_location(true)
            .map_err(|_| "The download couldn't start.")?;
        easy.fail_on_error(true)
            .map_err(|_| "The download couldn't start.")?;
        easy.connect_timeout(Duration::from_secs(10))
            .map_err(|_| "The download couldn't start.")?;
        easy.timeout(Duration::from_secs(300))
            .map_err(|_| "The download couldn't start.")?;
        let mut transfer = easy.transfer();
        transfer
            .write_function(|data| {
                use std::io::Write;
                Ok(file.write_all(data).map_or(0, |()| data.len()))
            })
            .map_err(|_| "The download couldn't start.")?;
        transfer
            .perform()
            .map_err(|_| "Couldn't download Soloist from Spotify. Check your connection.")?;
    }
    let unpack = paths.bin.join("unpack");
    let _ = std::fs::remove_dir_all(&unpack);
    std::fs::create_dir_all(&unpack).map_err(|_| "Music couldn't unpack the download.")?;
    let status = Command::new("tar")
        .arg("-xzf")
        .arg(&archive)
        .arg("-C")
        .arg(&unpack)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|_| "Music needs the tar program to unpack Soloist.")?;
    let _ = std::fs::remove_file(&archive);
    if !status.success() {
        return Err("The downloaded Soloist archive was damaged. Try again.".into());
    }
    let found = walkdir::WalkDir::new(&unpack)
        .max_depth(3)
        .into_iter()
        .filter_map(Result::ok)
        .find(|e| e.file_type().is_file() && e.file_name() == "soloist")
        .map(walkdir::DirEntry::into_path)
        .ok_or("The download didn't contain the Soloist program.")?;
    let target = paths.bin.join("soloist");
    std::fs::rename(&found, &target).map_err(|_| "Music couldn't install Soloist.")?;
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o755));
    }
    let _ = std::fs::remove_dir_all(&unpack);
    Ok(())
}

/// The installed build's age in days, from the build date its
/// `--version` prints (`... (20261117) ...`); archives don't keep file times.
#[must_use]
pub fn build_age_days(program: &Path) -> Option<u64> {
    let out = Command::new(program)
        .arg("--version")
        .stdin(Stdio::null())
        .output()
        .ok()?;
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let built = build_day(&text)?;
    let today = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs()
        / 86_400;
    Some(today.saturating_sub(built))
}

/// The build date in a version line, as days since 1970-01-01.
fn build_day(text: &str) -> Option<u64> {
    text.split(['(', ')'])
        .find(|part| part.len() == 8 && part.bytes().all(|b| b.is_ascii_digit()))
        .and_then(|digits| {
            let y: i64 = digits[0..4].parse().ok()?;
            let m: i64 = digits[4..6].parse().ok()?;
            let d: i64 = digits[6..8].parse().ok()?;
            ((1..=12).contains(&m) && (1..=31).contains(&d)).then(|| days_from_civil(y, m, d))
        })
        .and_then(|days| u64::try_from(days).ok())
}

/// Days since 1970-01-01 of a proleptic Gregorian date.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn key_slot() -> CredentialKey {
    CredentialKey::new(crate::DESKTOP_APP_ID, "spotify", "soloist-api-key")
}

/// The Soloist API key from the desktop keyring.
#[must_use]
pub fn load_key() -> Option<String> {
    let outcome = super::keyring(async {
        connect_for_app(crate::DESKTOP_APP_ID)
            .await
            .get(key_slot())
            .await
    });
    let CredentialOutcome::Retrieved(secret) = outcome else {
        return None;
    };
    std::str::from_utf8(secret.expose_secret())
        .ok()
        .map(str::trim)
        .filter(|k| !k.is_empty())
        .map(str::to_owned)
}

/// Keep the Soloist API key in the desktop keyring.
///
/// # Errors
/// The keyring's own summary when it refuses.
pub fn save_key(key: &str) -> Result<(), String> {
    let outcome = super::keyring(async {
        connect_for_app(crate::DESKTOP_APP_ID)
            .await
            .set(key_slot(), Secret::from_string(key.to_owned()))
            .await
    });
    if outcome.is_success() {
        Ok(())
    } else {
        Err(format!(
            "The desktop keyring refused the key: {}",
            outcome.user_summary()
        ))
    }
}

/// Forget the key (on disconnect, or when Soloist rejects it).
pub fn forget_key() {
    let _ = super::keyring(async {
        connect_for_app(crate::DESKTOP_APP_ID)
            .await
            .delete(key_slot())
            .await
    });
}

/// An API key out of whatever was copied with it: the longest run of
/// characters a key uses.
#[must_use]
pub fn key_in(text: &str) -> Option<String> {
    text.split(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.'))
        .max_by_key(|run| run.len())
        .filter(|run| run.len() >= 16 && run.len() <= 512)
        .map(str::to_owned)
}

/// What the running player reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Report {
    /// Soloist's sign-in state (paired and logged in, or waiting to pair).
    Auth { logged_in: bool },
    /// The player stopped.
    Exited(Stop),
}

/// Why the player stopped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Stop {
    /// The build is older than Spotify allows.
    Expired,
    /// Spotify rejected the API key.
    BadKey,
    /// Any other reason, with Soloist's last words.
    Failed(String),
}

/// A running Soloist; dropping it stops the player.
pub struct Player {
    child: Arc<Mutex<Option<Child>>>,
}

impl std::fmt::Debug for Player {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Player").finish_non_exhaustive()
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        if let Ok(mut child) = self.child.lock()
            && let Some(mut child) = child.take()
        {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl Player {
    /// Start Soloist and report its sign-in state and exit to `report`.
    ///
    /// # Errors
    /// A sentence for the user when the program can't start.
    pub fn start(
        program: &Path,
        key: &str,
        paths: &Paths,
        report: impl Fn(Report) + Send + Sync + 'static,
    ) -> Result<Self, String> {
        std::fs::create_dir_all(&paths.data)
            .map_err(|_| "Music couldn't create the player's data folder.")?;
        std::fs::create_dir_all(&paths.cache)
            .map_err(|_| "Music couldn't create the player's cache.")?;
        let _ = std::fs::remove_file(paths.data.join("ws.port"));
        let mut command = Command::new(program);
        command
            .arg("--device-name")
            .arg(DEVICE_NAME)
            .arg("--api-key")
            .arg(key)
            .arg("--data-dir")
            .arg(&paths.data)
            .arg("--cache-dir")
            .arg(&paths.cache)
            .arg("--ws")
            .arg("127.0.0.1:0")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        // The player goes when Music goes, however Music ends.
        // SAFETY: `prctl` is async-signal-safe and touches no Rust state.
        #[allow(unsafe_code)]
        unsafe {
            use std::os::unix::process::CommandExt;
            command.pre_exec(|| {
                libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
                Ok(())
            });
        }
        let mut child = command
            .spawn()
            .map_err(|_| "Soloist couldn't start. Download it again.")?;
        let tail = Arc::new(Mutex::new(Vec::<String>::new()));
        let report = Arc::new(report);
        for stream in [
            child
                .stdout
                .take()
                .map(|s| Box::new(s) as Box<dyn Read + Send>),
            child
                .stderr
                .take()
                .map(|s| Box::new(s) as Box<dyn Read + Send>),
        ]
        .into_iter()
        .flatten()
        {
            let tail = Arc::clone(&tail);
            std::thread::spawn(move || {
                for line in BufReader::new(stream).lines().map_while(Result::ok) {
                    // Soloist never prints the key, but make sure.
                    let line: String = line.chars().take(400).collect();
                    eprintln!("soloist: {line}");
                    if let Ok(mut tail) = tail.lock() {
                        tail.push(line);
                        if tail.len() > 20 {
                            tail.remove(0);
                        }
                    }
                }
            });
        }
        let child = Arc::new(Mutex::new(Some(child)));
        // Exit watcher.
        {
            let child = Arc::clone(&child);
            let report = Arc::clone(&report);
            let tail = Arc::clone(&tail);
            std::thread::spawn(move || {
                loop {
                    std::thread::sleep(Duration::from_millis(500));
                    let status = {
                        let Ok(mut guard) = child.lock() else { return };
                        let Some(running) = guard.as_mut() else {
                            return;
                        };
                        match running.try_wait() {
                            Ok(Some(status)) => status,
                            Ok(None) => continue,
                            Err(_) => return,
                        }
                    };
                    let last = tail.lock().map(|t| t.join("\n")).unwrap_or_default();
                    let lower = last.to_ascii_lowercase();
                    let stop = if status.code() == Some(EXIT_EXPIRED) || lower.contains("expired") {
                        Stop::Expired
                    } else if lower.contains("api key")
                        || lower.contains("api-key")
                        || lower.contains("unauthorized")
                    {
                        Stop::BadKey
                    } else {
                        Stop::Failed(
                            tail.lock()
                                .ok()
                                .and_then(|t| t.last().cloned())
                                .unwrap_or_else(|| format!("Soloist stopped ({status}).")),
                        )
                    };
                    report(Report::Exited(stop));
                    return;
                }
            });
        }
        // Sign-in watcher, over the local WebSocket.
        {
            let data = paths.data.clone();
            let report = Arc::clone(&report);
            let child = Arc::clone(&child);
            std::thread::spawn(move || watch_auth(&data, &child, &*report));
        }
        Ok(Self { child })
    }
}

/// Follow Soloist's `auth_state` events until the player stops.
fn watch_auth(data: &Path, child: &Mutex<Option<Child>>, report: &(dyn Fn(Report) + Send + Sync)) {
    let alive = || child.lock().is_ok_and(|c| c.is_some());
    let started = Instant::now();
    let port = loop {
        if !alive() || started.elapsed() > Duration::from_secs(20) {
            return;
        }
        if let Some(port) = std::fs::read_to_string(data.join("ws.port"))
            .ok()
            .and_then(|p| p.trim().parse::<u16>().ok())
        {
            break port;
        }
        std::thread::sleep(Duration::from_millis(200));
    };
    let mut last = None;
    while alive() {
        let Ok(stream) = TcpStream::connect(("127.0.0.1", port)) else {
            std::thread::sleep(Duration::from_secs(1));
            continue;
        };
        let _ = stream.set_read_timeout(Some(Duration::from_secs(30)));
        let Ok((mut socket, _)) = tungstenite::client(format!("ws://127.0.0.1:{port}/"), stream)
        else {
            std::thread::sleep(Duration::from_secs(1));
            continue;
        };
        let _ = socket.send(tungstenite::Message::text(
            r#"{"type":"command","command":"get_auth_state"}"#,
        ));
        loop {
            match socket.read() {
                Ok(tungstenite::Message::Text(text)) => {
                    let Ok(event) = serde_json::from_str::<serde_json::Value>(text.as_str()) else {
                        continue;
                    };
                    if event["type"] == "auth_state" {
                        let logged_in = event["logged_in"] == true;
                        if last != Some(logged_in) {
                            last = Some(logged_in);
                            report(Report::Auth { logged_in });
                        }
                    }
                }
                Ok(_) => {}
                Err(tungstenite::Error::Io(e))
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) =>
                {
                    if !alive() {
                        return;
                    }
                }
                Err(_) => break,
            }
        }
        std::thread::sleep(Duration::from_secs(1));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pasted_key_loses_its_label() {
        assert_eq!(
            key_in("API key: sk_live-AbC123def456GhI789 ").as_deref(),
            Some("sk_live-AbC123def456GhI789")
        );
        assert_eq!(key_in("short"), None);
    }

    #[test]
    fn the_build_date_comes_from_the_version_line() {
        let line = "soloist 1.3.10.79 build 1794938472 (20261117) (g19412df6ea) (linux/x86_64)";
        assert_eq!(build_day(line), Some(days_from_civil(2026, 11, 17) as u64));
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(days_from_civil(2000, 3, 1), 11_017);
        assert_eq!(build_day("no date"), None);
    }
}
