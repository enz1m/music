//! Per-file scan results reused while a file's modification time and size are
//! unchanged, kept in memory across scans and in the user cache across runs.

use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::art::{CoverArt, CoverKey, CoverMeta};
use crate::domain::{ReplayGain, Track};

const FORMAT_VERSION: u32 = 1;
const MAX_INDEX_BYTES: u64 = 256 * 1024 * 1024;
const MAX_ENTRIES: usize = 400_000;

/// The modification time and size a cached result was read at.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileStamp {
    secs: i64,
    nanos: i64,
    len: u64,
}

impl FileStamp {
    pub fn of(meta: &std::fs::Metadata) -> Self {
        Self {
            secs: meta.mtime(),
            nanos: meta.mtime_nsec(),
            len: meta.len(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct IndexedTrack {
    title: String,
    artist: String,
    album: String,
    album_artist: String,
    track_no: u32,
    disc_no: u32,
    year: u32,
    genre: String,
    format: String,
    duration_secs: f64,
    bitrate_kbps: u32,
    sample_rate: u32,
    channels: u8,
    bit_depth: u8,
    cover_path: Option<PathBuf>,
    cover: Option<CoverMeta>,
    // A sidecar cover is reused only while the sidecar itself is unchanged.
    sidecar_stamp: Option<FileStamp>,
    // Without an embedded picture, a sidecar added to the folder changes the cover.
    folder_stamp: Option<FileStamp>,
    added_at: u64,
    track_gain_db: Option<f32>,
    track_peak: Option<f32>,
    album_gain_db: Option<f32>,
    album_peak: Option<f32>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
enum Probe {
    Track(Box<IndexedTrack>),
    Undecodable,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Entry {
    path: PathBuf,
    stamp: FileStamp,
    probe: Probe,
}

#[derive(Serialize, Deserialize)]
struct Document {
    version: u32,
    entries: Vec<Entry>,
}

/// A remembered probe of one file.
pub enum Cached {
    Track(Box<Track>),
    Undecodable,
}

/// Scan results keyed by path.
#[derive(Default)]
pub struct ScanIndex {
    entries: HashMap<PathBuf, (FileStamp, Probe)>,
    file: Option<PathBuf>,
    dirty: bool,
}

impl ScanIndex {
    /// An index that lives only in memory.
    pub fn ephemeral() -> Self {
        Self::default()
    }

    /// Load the index persisted at `file`, starting empty when it is absent,
    /// oversized or unreadable.
    pub fn load(file: PathBuf) -> Self {
        let entries = read_document(&file)
            .map(|document| {
                document
                    .entries
                    .into_iter()
                    .take(MAX_ENTRIES)
                    .map(|entry| (entry.path, (entry.stamp, entry.probe)))
                    .collect()
            })
            .unwrap_or_default();
        Self {
            entries,
            file: Some(file),
            dirty: false,
        }
    }

    /// The remembered probe of `path` when `stamp` still matches it.
    pub fn lookup(&self, path: &Path, stamp: FileStamp) -> Option<Cached> {
        let (known, probe) = self.entries.get(path)?;
        if *known != stamp {
            return None;
        }
        match probe {
            Probe::Undecodable => Some(Cached::Undecodable),
            Probe::Track(indexed) => {
                restore(path, indexed).map(|track| Cached::Track(Box::new(track)))
            }
        }
    }

    /// Remember a successful probe.
    pub fn record_track(&mut self, track: &Track, stamp: FileStamp) {
        let cover = track.cover.as_ref().map(CoverArt::meta);
        // A cover that cannot be revived from the thumbnail cache needs a fresh probe.
        if cover.is_some_and(|meta| meta.is_none()) {
            return;
        }
        let sidecar_stamp = track
            .cover_path
            .as_deref()
            .filter(|_| {
                track
                    .cover
                    .as_ref()
                    .is_some_and(|cover| cover.source() == crate::art::CoverSource::Sidecar)
            })
            .and_then(|path| std::fs::metadata(path).ok())
            .map(|meta| FileStamp::of(&meta));
        let folder_stamp = if track
            .cover
            .as_ref()
            .is_some_and(|cover| cover.source() == crate::art::CoverSource::Embedded)
        {
            None
        } else {
            let Some(stamp) = folder_stamp(&track.path) else {
                return;
            };
            Some(stamp)
        };
        let indexed = IndexedTrack {
            title: track.title.clone(),
            artist: track.artist.clone(),
            album: track.album.clone(),
            album_artist: track.album_artist.clone(),
            track_no: track.track_no,
            disc_no: track.disc_no,
            year: track.year,
            genre: track.genre.clone(),
            format: track.format.clone(),
            duration_secs: track.duration_secs,
            bitrate_kbps: track.bitrate_kbps,
            sample_rate: track.sample_rate,
            channels: track.channels,
            bit_depth: track.bit_depth,
            cover_path: track.cover_path.clone(),
            cover: cover.flatten(),
            sidecar_stamp,
            folder_stamp,
            added_at: track.added_at,
            track_gain_db: track.replay_gain.track_gain_db,
            track_peak: track.replay_gain.track_peak,
            album_gain_db: track.replay_gain.album_gain_db,
            album_peak: track.replay_gain.album_peak,
        };
        self.insert(&track.path, stamp, Probe::Track(Box::new(indexed)));
    }

    /// Remember that `path` failed to probe at `stamp`.
    pub fn record_undecodable(&mut self, path: &Path, stamp: FileStamp) {
        self.insert(path, stamp, Probe::Undecodable);
    }

    fn insert(&mut self, path: &Path, stamp: FileStamp, probe: Probe) {
        if self.entries.len() >= MAX_ENTRIES && !self.entries.contains_key(path) {
            return;
        }
        self.entries.insert(path.to_path_buf(), (stamp, probe));
        self.dirty = true;
    }

    /// Forget every entry below one of `scope` that was not `seen`.
    pub fn retain_seen(&mut self, scope: &[PathBuf], seen: &HashSet<PathBuf>) {
        let before = self.entries.len();
        self.entries.retain(|path, _| {
            seen.contains(path) || !scope.iter().any(|dir| path.starts_with(dir))
        });
        self.dirty |= self.entries.len() != before;
    }

    /// The cover thumbnails that indexed tracks reference.
    pub fn cover_keys(&self) -> HashSet<CoverKey> {
        self.entries
            .values()
            .filter_map(|(_, probe)| match probe {
                Probe::Track(indexed) => indexed.cover.as_ref().map(CoverMeta::key),
                Probe::Undecodable => None,
            })
            .collect()
    }

    /// Write the index to its file when it changed since the last save.
    pub fn save_if_dirty(&mut self) {
        let Some(file) = self.file.as_deref() else {
            return;
        };
        if !self.dirty {
            return;
        }
        let document = Document {
            version: FORMAT_VERSION,
            entries: self
                .entries
                .iter()
                .map(|(path, (stamp, probe))| Entry {
                    path: path.clone(),
                    stamp: *stamp,
                    probe: probe.clone(),
                })
                .collect(),
        };
        if write_document(file, &document).is_ok() {
            self.dirty = false;
        }
    }
}

fn restore(path: &Path, indexed: &IndexedTrack) -> Option<Track> {
    if let Some(expected) = indexed.sidecar_stamp {
        let current = indexed
            .cover_path
            .as_deref()
            .and_then(|sidecar| std::fs::metadata(sidecar).ok())
            .map(|meta| FileStamp::of(&meta));
        if current != Some(expected) {
            return None;
        }
    }
    if indexed.folder_stamp.is_some() && folder_stamp(path) != indexed.folder_stamp {
        return None;
    }
    let cover = match &indexed.cover {
        Some(meta) => Some(crate::art::revive_cover(meta)?),
        None => None,
    };
    Some(Track {
        id: 0,
        path: path.to_path_buf(),
        title: indexed.title.clone(),
        artist: indexed.artist.clone(),
        album: indexed.album.clone(),
        album_artist: indexed.album_artist.clone(),
        track_no: indexed.track_no,
        disc_no: indexed.disc_no,
        year: indexed.year,
        genre: indexed.genre.clone(),
        format: indexed.format.clone(),
        duration_secs: indexed.duration_secs,
        bitrate_kbps: indexed.bitrate_kbps,
        sample_rate: indexed.sample_rate,
        channels: indexed.channels,
        bit_depth: indexed.bit_depth,
        cover_path: indexed.cover_path.clone(),
        cover,
        added_at: indexed.added_at,
        play_count: 0,
        last_played_seq: 0,
        last_played_unix: 0,
        replay_gain: ReplayGain {
            track_gain_db: indexed.track_gain_db,
            track_peak: indexed.track_peak,
            album_gain_db: indexed.album_gain_db,
            album_peak: indexed.album_peak,
        },
    })
}

fn folder_stamp(path: &Path) -> Option<FileStamp> {
    let meta = std::fs::metadata(path.parent()?).ok()?;
    Some(FileStamp::of(&meta))
}

fn read_document(file: &Path) -> Option<Document> {
    let handle = std::fs::File::open(file).ok()?;
    let meta = handle.metadata().ok()?;
    if !meta.is_file() || meta.len() > MAX_INDEX_BYTES {
        return None;
    }
    let mut bytes = Vec::new();
    handle.take(MAX_INDEX_BYTES).read_to_end(&mut bytes).ok()?;
    let document: Document = serde_json::from_slice(&bytes).ok()?;
    (document.version == FORMAT_VERSION).then_some(document)
}

fn write_document(file: &Path, document: &Document) -> std::io::Result<()> {
    let dir = file.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir)?;
    let staged = file.with_extension("json.part");
    let bytes = serde_json::to_vec(document).map_err(std::io::Error::other)?;
    std::fs::write(&staged, bytes)?;
    std::fs::rename(&staged, file)
}
