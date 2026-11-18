use std::collections::BTreeMap;
use std::fs::{Metadata, OpenOptions};
use std::io::{Cursor, Read};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};

use lofty::config::ParseOptions;
use lofty::file::{AudioFile, TaggedFileExt};
use lofty::prelude::*;
use lofty::probe::Probe;
use lofty::tag::ItemValue;

use crate::fetch_reaper::{CloseDebt, WorkerReaper};

const MEDIA_BYTES: usize = 64 * 1024 * 1024;
const TEXT_BYTES: usize = 256 * 1024;
const ROW_LIMIT: usize = 4096;
const ADMISSION_LIMIT: usize = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Failure {
    Absent,
    Malformed,
    TooLarge,
    Cancelled,
    Stale,
    Unavailable,
    Busy,
    Io,
}

impl Failure {
    pub fn label(self) -> &'static str {
        match self {
            Self::Absent => "No lyrics or metadata found.",
            Self::Malformed => "Malformed lyrics or metadata.",
            Self::TooLarge => {
                "Inspection limit exceeded (64 MiB audio, 256 KiB per text surface, 4096 rows)."
            }
            Self::Cancelled => "Inspection cancelled.",
            Self::Stale => "Inspection became stale. Reload to inspect the current version.",
            Self::Unavailable => "The inspection worker is unavailable.",
            Self::Busy => "The inspection worker is busy. Retry when it finishes.",
            Self::Io => "The file could not be read.",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LyricLine {
    pub millis: Option<u64>,
    pub text: String,
}

#[derive(Debug)]
pub struct Lyrics {
    pub origin: String,
    pub lines: Vec<LyricLine>,
}

#[derive(Debug)]
pub struct Inspection {
    pub fields: Vec<(String, String)>,
    pub tags: Result<Vec<(String, String)>, Failure>,
    pub lyrics: Result<Lyrics, Failure>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Page {
    #[default]
    Info,
    Lyrics,
    Tags,
}

#[derive(Debug)]
pub enum State {
    Loading,
    Ready(Inspection),
    Failed(Failure),
}

#[derive(Debug)]
pub struct Presentation {
    pub generation: u64,
    pub path: PathBuf,
    pub page: Page,
    pub state: State,
}

struct Request {
    generation: u64,
    path: PathBuf,
    cancel: Arc<AtomicBool>,
}

pub struct Completion {
    pub generation: u64,
    pub path: PathBuf,
    pub result: Result<Inspection, Failure>,
}

pub struct Service {
    jobs: Option<mpsc::SyncSender<Request>>,
    results: mpsc::Receiver<Completion>,
    active: BTreeMap<u64, Arc<AtomicBool>>,
    reaper: WorkerReaper,
    started: bool,
    closing: bool,
}

impl Default for Service {
    fn default() -> Self {
        let (_, results) = mpsc::sync_channel(ADMISSION_LIMIT);
        Self {
            jobs: None,
            results,
            active: BTreeMap::new(),
            reaper: WorkerReaper::settled("music-details-reaper"),
            started: false,
            closing: false,
        }
    }
}

impl Service {
    fn start(&mut self) -> Result<(), Failure> {
        self.start_with(inspect)
    }

    fn start_with(
        &mut self,
        read: impl Fn(&Path, &AtomicBool) -> Result<Inspection, Failure> + Send + 'static,
    ) -> Result<(), Failure> {
        if self.closing {
            return Err(Failure::Unavailable);
        }
        if self.started {
            return self.jobs.as_ref().map(|_| ()).ok_or(Failure::Unavailable);
        }
        self.started = true;
        let (jobs, requests) = mpsc::sync_channel::<Request>(ADMISSION_LIMIT);
        // Each admitted job reserves a completion slot until the reducer drains it.
        let (results, completed) = mpsc::sync_channel(ADMISSION_LIMIT);
        let worker = std::thread::Builder::new()
            .name("music-details".into())
            .spawn(move || {
                while let Ok(request) = requests.recv() {
                    let result = read(&request.path, &request.cancel);
                    if results
                        .send(Completion {
                            generation: request.generation,
                            path: request.path,
                            result,
                        })
                        .is_err()
                    {
                        break;
                    }
                }
            })
            .map_err(|_| Failure::Unavailable)?;
        self.jobs = Some(jobs);
        self.results = completed;
        self.reaper = WorkerReaper::new(vec![worker], "music-details-reaper");
        Ok(())
    }

    pub fn submit(&mut self, generation: u64, path: PathBuf) -> Result<(), Failure> {
        self.start()?;
        if self.active.len() >= ADMISSION_LIMIT || self.active.contains_key(&generation) {
            return Err(Failure::Busy);
        }
        if path.as_os_str().len() > crate::domain::MAX_PERSISTED_PATH_BYTES {
            return Err(Failure::TooLarge);
        }
        let cancel = Arc::new(AtomicBool::new(false));
        self.reaper
            .try_note_accepted()
            .map_err(|_| Failure::Unavailable)?;
        let request = Request {
            generation,
            path,
            cancel: Arc::clone(&cancel),
        };
        let result = self
            .jobs
            .as_ref()
            .ok_or(Failure::Unavailable)
            .and_then(|jobs| {
                jobs.try_send(request).map_err(|error| match error {
                    mpsc::TrySendError::Full(_) => Failure::Busy,
                    mpsc::TrySendError::Disconnected(_) => Failure::Unavailable,
                })
            });
        if result.is_ok() {
            self.active.insert(generation, cancel);
        } else {
            self.reaper.note_retired();
        }
        result
    }

    pub fn cancel(&self) {
        for cancel in self.active.values() {
            cancel.store(true, Ordering::Release);
        }
    }

    pub fn drain(&mut self) -> Vec<Completion> {
        let mut completed = Vec::new();
        while let Ok(completion) = self.results.try_recv() {
            self.active.remove(&completion.generation);
            self.reaper.note_retired();
            completed.push(completion);
        }
        completed
    }

    pub fn needs_poll(&self) -> bool {
        !self.active.is_empty() || !self.reaper.close_debt().is_settled()
    }

    pub fn close_debt(&self) -> CloseDebt {
        self.reaper.close_debt()
    }

    pub fn shutdown(&mut self) {
        self.closing = true;
        self.cancel();
        self.jobs.take();
        self.reaper.begin_close();
        self.reaper.launch_reaper();
    }

    pub fn progress(&mut self) {
        self.reaper.shutdown_progress();
    }
}

impl Drop for Service {
    fn drop(&mut self) {
        self.cancel();
        self.jobs.take();
        self.reaper.begin_close();
    }
}

fn cancelled(cancel: &AtomicBool) -> Result<(), Failure> {
    if cancel.load(Ordering::Acquire) {
        Err(Failure::Cancelled)
    } else {
        Ok(())
    }
}

fn same_file(a: &Metadata, b: &Metadata) -> bool {
    a.dev() == b.dev()
        && a.ino() == b.ino()
        && a.len() == b.len()
        && a.mtime() == b.mtime()
        && a.mtime_nsec() == b.mtime_nsec()
        && a.ctime() == b.ctime()
        && a.ctime_nsec() == b.ctime_nsec()
}

fn read_bounded(
    path: &Path,
    cap: usize,
    cancel: &AtomicBool,
) -> Result<(Vec<u8>, Metadata), Failure> {
    cancelled(cancel)?;
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(path)
        .map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                Failure::Absent
            } else {
                Failure::Io
            }
        })?;
    let before = file.metadata().map_err(|_| Failure::Io)?;
    if !before.is_file() {
        return Err(Failure::Io);
    }
    if before.len() > cap as u64 {
        return Err(Failure::TooLarge);
    }
    let mut bytes = Vec::new();
    let mut chunk = [0; 8192];
    loop {
        cancelled(cancel)?;
        let count = file.read(&mut chunk).map_err(|_| Failure::Io)?;
        if count == 0 {
            break;
        }
        if bytes.len().saturating_add(count) > cap {
            return Err(Failure::TooLarge);
        }
        bytes.extend_from_slice(&chunk[..count]);
    }
    let after = file.metadata().map_err(|_| Failure::Io)?;
    let named = std::fs::metadata(path).map_err(|_| Failure::Stale)?;
    if !same_file(&before, &after)
        || !same_file(&after, &named)
        || bytes.len() as u64 != after.len()
    {
        return Err(Failure::Stale);
    }
    Ok((bytes, after))
}

fn verify_identity(path: &Path, identity: &Metadata) -> Result<(), Failure> {
    let named = std::fs::metadata(path).map_err(|_| Failure::Stale)?;
    if same_file(identity, &named) {
        Ok(())
    } else {
        Err(Failure::Stale)
    }
}

fn push_row(
    rows: &mut Vec<(String, String)>,
    budget: &mut usize,
    key: String,
    value: &str,
) -> Result<(), Failure> {
    let size = key.len().saturating_add(value.len());
    if rows.len() >= ROW_LIMIT || size > *budget {
        return Err(Failure::TooLarge);
    }
    *budget -= size;
    rows.push((key, value.to_owned()));
    Ok(())
}

fn hex(bytes: &[u8], budget: usize) -> Result<String, Failure> {
    use std::fmt::Write;
    if bytes.len() > budget / 2 {
        return Err(Failure::TooLarge);
    }
    let mut value = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(value, "{byte:02x}");
    }
    Ok(value)
}

fn collect_tags(
    tagged: &lofty::file::TaggedFile,
    cancel: &AtomicBool,
) -> Result<Vec<(String, String)>, Failure> {
    let mut budget = TEXT_BYTES;
    let mut tags = Vec::new();
    for tag in tagged.tags() {
        for item in tag.items() {
            cancelled(cancel)?;
            let key = format!(
                "{:?} / {:?} / {:?} / {}",
                tag.tag_type(),
                item.key(),
                item.lang(),
                item.description()
            );
            match item.value() {
                ItemValue::Text(value) | ItemValue::Locator(value) => {
                    push_row(&mut tags, &mut budget, key, value)?;
                }
                ItemValue::Binary(value) => {
                    let value = hex(value, budget)?;
                    push_row(&mut tags, &mut budget, key, &value)?;
                }
            }
        }
        for picture in tag.pictures() {
            let key = format!(
                "{:?} / Picture {:?} ({:?}) {}",
                tag.tag_type(),
                picture.pic_type(),
                picture.mime_type(),
                picture.description().unwrap_or("")
            );
            let value = hex(picture.data(), budget)?;
            push_row(&mut tags, &mut budget, key, &value)?;
        }
    }
    Ok(tags)
}

fn inspect(path: &Path, cancel: &AtomicBool) -> Result<Inspection, Failure> {
    let (bytes, identity) = read_bounded(path, MEDIA_BYTES, cancel)?;
    cancelled(cancel)?;
    let tagged = Probe::new(Cursor::new(bytes))
        .options(ParseOptions::new().read_properties(true))
        .guess_file_type()
        .map_err(|_| Failure::Malformed)?
        .read()
        .map_err(|_| Failure::Malformed)?;
    cancelled(cancel)?;
    let tags = collect_tags(&tagged, cancel);
    let mut budget = TEXT_BYTES;
    let mut fields = Vec::new();
    let embedded = tagged
        .tags()
        .iter()
        .flat_map(|tag| tag.items())
        .find_map(|item| {
            if item.key() != &ItemKey::Lyrics {
                return None;
            }
            match item.value() {
                ItemValue::Text(text) | ItemValue::Locator(text) => Some(text.as_str()),
                _ => None,
            }
        });
    if let Some(tag) = tagged.primary_tag().or_else(|| tagged.first_tag()) {
        for (key, value) in [
            ("Title", tag.title()),
            ("Artist", tag.artist()),
            ("Album", tag.album()),
            ("Genre", tag.genre()),
        ] {
            if let Some(value) = value {
                push_row(&mut fields, &mut budget, key.into(), &value)?;
            }
        }
        {
            let (label, key) = ("Album Artist", ItemKey::AlbumArtist);
            if let Some(value) = tag.get_string(&key) {
                push_row(&mut fields, &mut budget, label.into(), value)?;
            }
        }
        push_row(
            &mut fields,
            &mut budget,
            "Position".into(),
            &format!(
                "Track {} · Disc {}",
                tag.track().unwrap_or(0),
                tag.disk().unwrap_or(0)
            ),
        )?;
        if let Some(year) = tag.year() {
            push_row(&mut fields, &mut budget, "Year".into(), &year.to_string())?;
        }
    }
    let properties = tagged.properties();
    push_row(
        &mut fields,
        &mut budget,
        "Duration".into(),
        &crate::domain::format_duration(properties.duration().as_secs_f64()),
    )?;
    push_row(
        &mut fields,
        &mut budget,
        "Format".into(),
        &format!(
            "{:?} · {} kbps · {} Hz · {} bit · {} channels",
            tagged.file_type(),
            properties.overall_bitrate().unwrap_or(0),
            properties.sample_rate().unwrap_or(0),
            properties.bit_depth().unwrap_or(0),
            properties.channels().unwrap_or(0)
        ),
    )?;
    push_row(
        &mut fields,
        &mut budget,
        "File".into(),
        &path.to_string_lossy(),
    )?;
    let lyrics = load_lyrics(path, embedded, cancel);
    cancelled(cancel)?;
    verify_identity(path, &identity)?;
    Ok(Inspection {
        fields,
        tags,
        lyrics,
    })
}

fn timestamp(text: &str) -> Option<u64> {
    let (minutes, seconds) = text.split_once(':')?;
    if minutes.is_empty() || !minutes.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let minutes: u64 = minutes.parse().ok()?;
    let (seconds, fraction) = seconds.split_once('.').unwrap_or((seconds, ""));
    if seconds.len() != 2
        || !seconds.bytes().all(|b| b.is_ascii_digit())
        || fraction.len() > 3
        || !fraction.bytes().all(|b| b.is_ascii_digit())
    {
        return None;
    }
    let seconds: u64 = seconds.parse().ok()?;
    if seconds >= 60 {
        return None;
    }
    let fraction: u64 = if fraction.is_empty() {
        0
    } else {
        fraction.parse::<u64>().ok()? * 10u64.pow(3 - fraction.len() as u32)
    };
    minutes
        .checked_mul(60_000)?
        .checked_add(seconds * 1000)?
        .checked_add(fraction)
}

pub fn parse_lyrics(text: &str, lrc: bool) -> Result<Vec<LyricLine>, Failure> {
    if text.len() > TEXT_BYTES {
        return Err(Failure::TooLarge);
    }
    if text.trim().is_empty() {
        return Err(Failure::Absent);
    }
    if text
        .chars()
        .any(|c| c.is_control() && c != '\n' && c != '\r' && c != '\t')
    {
        return Err(Failure::Malformed);
    }
    let mut lines = Vec::new();
    let mut budget = TEXT_BYTES;
    let mut offset: i64 = 0;
    for line in text.trim_start_matches('\u{feff}').lines() {
        let mut rest = line.trim_end();
        let mut times = Vec::new();
        if lrc {
            while let Some(after) = rest.strip_prefix('[') {
                let (prefix, tail) = after.split_once(']').ok_or(Failure::Malformed)?;
                if let Some(time) = timestamp(prefix) {
                    if times.len() >= ROW_LIMIT {
                        return Err(Failure::TooLarge);
                    }
                    times.push(time);
                } else if let Some(value) = prefix.strip_prefix("offset:") {
                    offset = value.parse().map_err(|_| Failure::Malformed)?;
                } else if !["ar:", "al:", "ti:", "au:", "by:", "re:", "ve:", "length:"]
                    .iter()
                    .any(|key| prefix.starts_with(key))
                {
                    return Err(Failure::Malformed);
                }
                rest = tail;
            }
            if times.is_empty() && !rest.trim().is_empty() {
                return Err(Failure::Malformed);
            }
        }
        let count = if lrc { times.len() } else { 1 };
        if lines.len().saturating_add(count) > ROW_LIMIT
            || rest.len().saturating_mul(count) > budget
        {
            return Err(Failure::TooLarge);
        }
        budget -= rest.len() * count;
        if !lrc {
            lines.push(LyricLine {
                millis: None,
                text: rest.to_owned(),
            });
        }
        for time in times {
            lines.push(LyricLine {
                millis: Some(time),
                text: rest.to_owned(),
            });
        }
    }
    for line in &mut lines {
        if let Some(time) = line.millis {
            line.millis = Some(time.saturating_add_signed(offset));
        }
    }
    lines.sort_by_key(|line| line.millis);
    if lines.is_empty() {
        Err(Failure::Absent)
    } else {
        Ok(lines)
    }
}

fn load_lyrics(
    path: &Path,
    embedded: Option<&str>,
    cancel: &AtomicBool,
) -> Result<Lyrics, Failure> {
    for extension in ["lrc", "lyrics", "txt"] {
        let sidecar = path.with_extension(extension);
        match read_bounded(&sidecar, TEXT_BYTES, cancel) {
            Err(Failure::Absent) => continue,
            Err(error) => return Err(error),
            Ok((bytes, identity)) => {
                let text = std::str::from_utf8(&bytes).map_err(|_| Failure::Malformed)?;
                let lines = match parse_lyrics(text, extension == "lrc") {
                    Ok(lines) => lines,
                    Err(Failure::Absent) => continue,
                    Err(error) => return Err(error),
                };
                verify_identity(&sidecar, &identity)?;
                cancelled(cancel)?;
                return Ok(Lyrics {
                    origin: format!("Sidecar .{extension}"),
                    lines,
                });
            }
        }
    }
    let text = embedded.ok_or(Failure::Absent)?;
    let lrc = text.lines().any(|line| {
        line.starts_with("[offset:")
            || line
                .strip_prefix('[')
                .and_then(|tail| tail.split_once(':'))
                .is_some_and(|(prefix, _)| {
                    !prefix.is_empty() && prefix.bytes().all(|byte| byte.is_ascii_digit())
                })
    });
    Ok(Lyrics {
        origin: "Embedded lyrics".into(),
        lines: parse_lyrics(text, lrc)?,
    })
}
