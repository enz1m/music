//! Cover art — decode embedded / sidecar pictures into small, shared thumbnails.
//!
//! A file's embedded picture (via `lofty`) or `cover.jpg` sidecar is decoded
//! here, inside the off-thread scanner, into a downscaled shared [`CoverArt`].
//! The encoded bytes are used only to decode and dedup, then dropped.
//!
//! * Decoding never happens on the UI thread: a full-size JPEG decoded during
//!   `view` or draw stutters the frame.
//! * Covers are downscaled to at most [`COVER_THUMB_PX`] on the long edge, and
//!   [`CoverCache`] decodes byte-identical art once and hands out clones that
//!   share pixels and the renderer's texture.
//!
//! A file with no picture and no sidecar yields `None`, and views draw the
//! themed placeholder rather than a stock image.

use std::collections::{HashMap, HashSet, VecDeque};
use std::fs::{File, OpenOptions};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex, OnceLock, Weak};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

use enzui_core::decode_limits::DecodeLimits;
use enzui_resources::decode_static;

#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;

/// The iced image handle type, available through iced's
/// `image-without-codecs` feature enabled by `enzui-widgets`.
pub type CoverHandle = iced::widget::image::Handle;

/// The longest edge, in pixels, a stored cover thumbnail may have.
///
/// Sized for the largest art surface, the now-playing disc (up to 280 px),
/// at 256 KB of RGBA per unique cover.
pub const COVER_THUMB_PX: u32 = 256;

/// Cover-specific admission policy. Album art has no reason to approach the
/// framework's general-purpose image ceilings, so the scanner uses a tighter
/// encoded and decoded memory budget while retaining the framework's
/// header-before-allocation checks.
pub const COVER_DECODE_LIMITS: DecodeLimits = DecodeLimits {
    max_encoded_bytes: 8 * 1024 * 1024,
    max_width: 8_192,
    max_height: 8_192,
    max_pixels: 4 * 1024 * 1024,
    max_rgba_bytes: 16 * 1024 * 1024,
};

/// Decode limits for podcast and Spotify artwork fetched from the web. Apple
/// asks podcasters for 1400 to 3000 pixel covers, past the local budget; they
/// decode on an artwork worker and are cut to a thumbnail at once.
pub const REMOTE_COVER_DECODE_LIMITS: DecodeLimits = DecodeLimits {
    max_encoded_bytes: 8 * 1024 * 1024,
    max_width: 8_192,
    max_height: 8_192,
    max_pixels: 4_096 * 4_096,
    max_rgba_bytes: 4_096 * 4_096 * 4,
};

/// Where a cover came from: embedded in the file or found beside it, which
/// are different claims about the user's library.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CoverSource {
    /// A picture frame embedded in the audio file's tags (ID3 `APIC`, …).
    Embedded,
    /// A `cover.jpg` / `folder.png` style file sitting beside the audio file.
    Sidecar,
}

/// A decoded, downscaled cover ready to draw.
///
/// Cloning shares pixels and the handle id, so every track of an album points
/// the renderer at one uploaded texture.
#[derive(Clone, Debug)]
pub struct CoverArt {
    pixels: Pixels,
    source: CoverSource,
    width: u32,
    height: u32,
    /// The cover's leading colours, most prominent first, for the Now
    /// Playing backdrop.
    palette: [[u8; 3]; 3],
}

impl CoverArt {
    /// The drawable handle. Clones share pixels.
    ///
    /// A library cover evicted from memory returns a transparent pixel while it
    /// reloads from the thumbnail cache, then the next frame draws it.
    pub fn handle(&self) -> CoverHandle {
        match &self.pixels {
            Pixels::Resident(handle) => handle.clone(),
            Pixels::Stored(stored) => stored.handle(),
        }
    }
    /// Whether this art was embedded in the audio file or found beside it.
    pub fn source(&self) -> CoverSource {
        self.source
    }
    /// The stored thumbnail's pixel dimensions (never larger than
    /// [`COVER_THUMB_PX`] on either axis).
    pub fn dimensions(&self) -> (u32, u32) {
        (self.width, self.height)
    }
    /// The cover's three leading colours, most prominent first (repeated
    /// when the cover has fewer distinct ones).
    pub fn palette(&self) -> [[u8; 3]; 3] {
        self.palette
    }
    /// Bytes of RGBA held by this thumbnail: `width * height * 4`.
    pub fn rgba_bytes(&self) -> usize {
        self.width as usize * self.height as usize * 4
    }

    /// Persist the already admitted thumbnail without decoding its source a
    /// second time. The scanner's cache file is always named `.jpg`.
    pub(crate) fn save_jpeg(&self, path: &Path) -> bool {
        let Pixels::Resident(CoverHandle::Rgba {
            width,
            height,
            pixels,
            ..
        }) = &self.pixels
        else {
            return false;
        };
        let Some(rgba) = image::RgbaImage::from_raw(*width, *height, pixels.to_vec()) else {
            return false;
        };
        image::DynamicImage::ImageRgba8(rgba)
            .save_with_format(path, image::ImageFormat::Jpeg)
            .is_ok()
    }
}

/// Decode encoded image bytes into a downscaled [`CoverArt`].
///
/// Returns `None` for undecodable, zero-sized, or corrupt images, so a bad
/// `APIC` frame neither fails the scan nor produces a fake tile.
pub fn decode_cover(bytes: &[u8], source: CoverSource) -> Option<CoverArt> {
    decode_cover_within(bytes, source, &COVER_DECODE_LIMITS)
}

/// [`decode_cover`] under other limits (see [`REMOTE_COVER_DECODE_LIMITS`]).
pub fn decode_cover_within(
    bytes: &[u8],
    source: CoverSource,
    limits: &DecodeLimits,
) -> Option<CoverArt> {
    let decoded = decode_static(bytes, limits, || false).ok()?;
    let (width, height) = (decoded.width, decoded.height);

    // Downscale only. Small covers keep the decoder's RGBA handle; large ones
    // are resized from decoded pixels, never decoded a second time.
    let (handle, width, height) = if width > COVER_THUMB_PX || height > COVER_THUMB_PX {
        let CoverHandle::Rgba { pixels, .. } = decoded.handle else {
            return None;
        };
        let rgba = image::RgbaImage::from_raw(width, height, pixels.to_vec())?;
        let rgba = image::DynamicImage::ImageRgba8(rgba)
            .thumbnail(COVER_THUMB_PX, COVER_THUMB_PX)
            .to_rgba8();
        let (width, height) = rgba.dimensions();
        (
            CoverHandle::from_rgba(width, height, rgba.into_raw()),
            width,
            height,
        )
    } else {
        (decoded.handle, width, height)
    };

    let palette = match &handle {
        CoverHandle::Rgba { pixels, .. } => palette_of(pixels),
        _ => [[96, 96, 96]; 3],
    };
    Some(CoverArt {
        pixels: Pixels::Resident(handle),
        source,
        width,
        height,
        palette,
    })
}

/// The leading colours of RGBA `pixels`: a coarse colour histogram (16 levels
/// a channel) weighted toward saturated colours and away from near-black and
/// near-white, then the strongest buckets that differ clearly from each other.
fn palette_of(pixels: &[u8]) -> [[u8; 3]; 3] {
    const LEVELS: usize = 16;
    let mut buckets = vec![(0u32, [0u64; 3]); LEVELS * LEVELS * LEVELS];
    // Every fourth pixel is plenty for a thumbnail.
    for px in pixels.as_chunks::<4>().0.iter().step_by(4) {
        if px[3] < 128 {
            continue;
        }
        let index = (usize::from(px[0]) >> 4) * LEVELS * LEVELS
            + (usize::from(px[1]) >> 4) * LEVELS
            + (usize::from(px[2]) >> 4);
        let bucket = &mut buckets[index];
        bucket.0 += 1;
        for (sum, value) in bucket.1.iter_mut().zip(px) {
            *sum += u64::from(*value);
        }
    }
    let mut scored: Vec<([u8; 3], f32)> = buckets
        .iter()
        .filter(|(count, _)| *count > 0)
        .map(|(count, sums)| {
            let color = sums.map(|sum| (sum / u64::from(*count)) as u8);
            let max = f32::from(*color.iter().max().unwrap_or(&0)) / 255.0;
            let min = f32::from(*color.iter().min().unwrap_or(&0)) / 255.0;
            let saturation = max - min;
            let lightness = (max + min) / 2.0;
            let mid = 1.0 - (lightness - 0.5).abs() * 1.6;
            (color, *count as f32 * (0.25 + saturation) * mid.max(0.1))
        })
        .collect();
    scored.sort_by(|a, b| b.1.total_cmp(&a.1));
    let mut picked: Vec<[u8; 3]> = Vec::with_capacity(3);
    for (color, _) in &scored {
        let distinct = picked.iter().all(|other| {
            other
                .iter()
                .zip(color)
                .map(|(a, b)| (i32::from(*a) - i32::from(*b)).abs())
                .sum::<i32>()
                > 90
        });
        if distinct {
            picked.push(*color);
            if picked.len() == 3 {
                break;
            }
        }
    }
    let first = picked.first().copied().unwrap_or([96, 96, 96]);
    [
        first,
        picked.get(1).copied().unwrap_or(first),
        picked
            .get(2)
            .copied()
            .or_else(|| picked.get(1).copied())
            .unwrap_or(first),
    ]
}

#[derive(Debug)]
struct BoundedRead {
    bytes: Option<Vec<u8>>,
    #[cfg(test)]
    retained_bytes: usize,
}

fn read_bounded(reader: impl Read, cap: u64) -> io::Result<BoundedRead> {
    let mut bytes = Vec::new();
    reader.take(cap.saturating_add(1)).read_to_end(&mut bytes)?;
    let retained_bytes = bytes.len();
    let bytes = (u64::try_from(retained_bytes).unwrap_or(u64::MAX) <= cap).then_some(bytes);
    Ok(BoundedRead {
        bytes,
        #[cfg(test)]
        retained_bytes,
    })
}

fn open_sidecar(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);

    let file = options.open(path)?;
    if !file.metadata()?.file_type().is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "cover sidecar is not a regular file",
        ));
    }
    Ok(file)
}

fn read_sidecar(path: &Path) -> io::Result<BoundedRead> {
    read_bounded(open_sidecar(path)?, COVER_DECODE_LIMITS.max_encoded_bytes)
}

/// Content identity of a library cover: a hash and byte length of its encoded
/// image, and whether it was embedded or a sidecar.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CoverKey(u64, u64, CoverSource);

impl CoverKey {
    fn of_bytes(bytes: &[u8], source: CoverSource) -> Self {
        Self(hash_bytes(bytes), bytes.len() as u64, source)
    }

    fn file_name(self) -> String {
        let source = match self.2 {
            CoverSource::Embedded => 'e',
            CoverSource::Sidecar => 's',
        };
        format!("{:016x}-{:x}-{source}.jpg", self.0, self.1)
    }
}

/// What a cached library cover needs to be drawn again without its source file.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct CoverMeta {
    key: CoverKey,
    source: CoverSource,
    width: u32,
    height: u32,
    palette: [[u8; 3]; 3],
}

impl CoverMeta {
    /// The content identity of the cached thumbnail.
    pub fn key(&self) -> CoverKey {
        self.key
    }
}

impl CoverArt {
    /// The persistent identity of a library cover held by the cover store.
    pub fn meta(&self) -> Option<CoverMeta> {
        match &self.pixels {
            Pixels::Stored(stored) => Some(stored.meta),
            Pixels::Resident(_) => None,
        }
    }

    fn stored(stored: Arc<StoredCover>) -> Self {
        let meta = stored.meta;
        Self {
            pixels: Pixels::Stored(stored),
            source: meta.source,
            width: meta.width,
            height: meta.height,
            palette: meta.palette,
        }
    }
}

/// Decoded library cover pixels kept in memory at once; the rest reload from
/// the on-disk thumbnail cache when drawn.
pub const COVER_RESIDENT_BYTES: usize = 64 * 1024 * 1024;

#[derive(Clone, Debug)]
enum Pixels {
    Resident(CoverHandle),
    Stored(Arc<StoredCover>),
}

struct StoredCover {
    meta: CoverMeta,
    slot: Mutex<Option<CoverHandle>>,
    last_use: AtomicU64,
    requested: AtomicBool,
}

impl std::fmt::Debug for StoredCover {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StoredCover")
            .field("key", &self.meta.key)
            .finish_non_exhaustive()
    }
}

static TRANSPARENT: LazyLock<CoverHandle> =
    LazyLock::new(|| CoverHandle::from_rgba(1, 1, vec![0; 4]));

impl StoredCover {
    fn new(meta: CoverMeta, handle: Option<CoverHandle>) -> Arc<Self> {
        Arc::new(Self {
            meta,
            slot: Mutex::new(handle),
            last_use: AtomicU64::new(STORE.tick()),
            requested: AtomicBool::new(false),
        })
    }

    fn bytes(&self) -> usize {
        self.meta.width as usize * self.meta.height as usize * 4
    }

    fn handle(self: &Arc<Self>) -> CoverHandle {
        self.last_use.store(STORE.tick(), Ordering::Relaxed);
        if let Some(handle) = lock(&self.slot).as_ref() {
            return handle.clone();
        }
        STORE.request(self);
        TRANSPARENT.clone()
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[derive(Default)]
struct StoreState {
    known: HashMap<CoverKey, Weak<StoredCover>>,
    resident: Vec<Weak<StoredCover>>,
    queue: VecDeque<Arc<StoredCover>>,
    loading: bool,
}

/// Process-wide library covers keyed by content, so every scan shares one
/// decode per image and memory stays within [`COVER_RESIDENT_BYTES`].
///
/// Without a configured directory nothing can reload, so covers stay resident.
struct CoverStore {
    dir: OnceLock<PathBuf>,
    state: Mutex<StoreState>,
    clock: AtomicU64,
    wake: Mutex<Option<iced::futures::channel::mpsc::UnboundedSender<()>>>,
}

static STORE: LazyLock<CoverStore> = LazyLock::new(|| CoverStore {
    dir: OnceLock::new(),
    state: Mutex::new(StoreState::default()),
    clock: AtomicU64::new(1),
    wake: Mutex::new(None),
});

/// Keep library cover thumbnails in `dir` so evicted covers can reload.
pub fn configure_cover_store(dir: PathBuf) {
    if std::fs::create_dir_all(&dir).is_ok() {
        let _ = STORE.dir.set(dir);
    }
}

/// Emits each time evicted covers finish reloading, so the UI redraws them.
pub fn cover_reload_subscription() -> iced::Subscription<()> {
    iced::Subscription::run(|| {
        let (sender, receiver) = iced::futures::channel::mpsc::unbounded();
        *lock(&STORE.wake) = Some(sender);
        receiver
    })
}

/// The cached cover described by `meta`, or `None` when its thumbnail is gone.
pub fn revive_cover(meta: &CoverMeta) -> Option<CoverArt> {
    if let Some(stored) = STORE.live(meta.key) {
        return Some(CoverArt::stored(stored));
    }
    let dir = STORE.dir.get()?;
    let file = dir.join(meta.key.file_name());
    if !file.is_file() {
        return None;
    }
    mark_used(&file);
    let stored = StoredCover::new(*meta, None);
    lock(&STORE.state)
        .known
        .insert(meta.key, Arc::downgrade(&stored));
    Some(CoverArt::stored(stored))
}

impl CoverStore {
    fn tick(&self) -> u64 {
        self.clock.fetch_add(1, Ordering::Relaxed)
    }

    fn live(&self, key: CoverKey) -> Option<Arc<StoredCover>> {
        lock(&self.state).known.get(&key)?.upgrade()
    }

    fn loaded_file_names(&self) -> HashSet<String> {
        let keys: Vec<CoverKey> = lock(&self.state)
            .known
            .iter()
            .filter(|(_, weak)| weak.strong_count() > 0)
            .map(|(key, _)| *key)
            .collect();
        keys.into_iter().map(CoverKey::file_name).collect()
    }

    fn shared(&self, key: CoverKey) -> Option<CoverArt> {
        self.live(key).map(CoverArt::stored)
    }

    // Writes the thumbnail once so the decoded pixels can later be evicted.
    fn admit(&self, key: CoverKey, art: CoverArt) -> CoverArt {
        let Some(dir) = self.dir.get() else {
            return art;
        };
        let file = dir.join(key.file_name());
        if file.is_file() {
            mark_used(&file);
        } else {
            let staged = file.with_extension("part");
            if !art.save_jpeg(&staged) || std::fs::rename(&staged, &file).is_err() {
                let _ = std::fs::remove_file(&staged);
                return art;
            }
        }
        let Pixels::Resident(handle) = &art.pixels else {
            return art;
        };
        let meta = CoverMeta {
            key,
            source: art.source,
            width: art.width,
            height: art.height,
            palette: art.palette,
        };
        let stored = StoredCover::new(meta, Some(handle.clone()));
        let mut state = lock(&self.state);
        if state.known.len() >= 4096 && state.known.len().is_power_of_two() {
            state.known.retain(|_, weak| weak.strong_count() > 0);
        }
        state.known.insert(key, Arc::downgrade(&stored));
        state.resident.push(Arc::downgrade(&stored));
        Self::evict(&mut state);
        drop(state);
        CoverArt::stored(stored)
    }

    fn evict(state: &mut StoreState) {
        state.resident.retain(|weak| weak.strong_count() > 0);
        let mut live: Vec<Arc<StoredCover>> =
            state.resident.iter().filter_map(Weak::upgrade).collect();
        let mut bytes: usize = live.iter().map(|stored| stored.bytes()).sum();
        if bytes <= COVER_RESIDENT_BYTES {
            return;
        }
        live.sort_by_key(|stored| stored.last_use.load(Ordering::Relaxed));
        for stored in live {
            if bytes <= COVER_RESIDENT_BYTES {
                break;
            }
            bytes -= stored.bytes();
            *lock(&stored.slot) = None;
            stored.requested.store(false, Ordering::Relaxed);
            state
                .resident
                .retain(|weak| !std::ptr::eq(weak.as_ptr(), Arc::as_ptr(&stored)));
        }
    }

    fn request(&self, stored: &Arc<StoredCover>) {
        if stored.requested.swap(true, Ordering::AcqRel) {
            return;
        }
        let mut state = lock(&self.state);
        state.queue.push_back(Arc::clone(stored));
        if !state.loading {
            state.loading = true;
            if std::thread::Builder::new()
                .name("music-cover-reload".into())
                .spawn(|| STORE.reload_queued())
                .is_err()
            {
                state.loading = false;
            }
        }
    }

    fn reload_queued(&self) {
        loop {
            let stored = {
                let mut state = lock(&self.state);
                match state.queue.pop_front() {
                    Some(stored) => stored,
                    None => {
                        state.loading = false;
                        break;
                    }
                }
            };
            // A cover no track holds any more is not worth decoding.
            if Arc::strong_count(&stored) == 1 {
                continue;
            }
            let decoded = self
                .dir
                .get()
                .map(|dir| dir.join(stored.meta.key.file_name()))
                .inspect(|file| mark_used(file))
                .and_then(|file| read_sidecar(&file).ok())
                .and_then(|bounded| bounded.bytes)
                .and_then(|bytes| decode_cover(&bytes, stored.meta.source));
            // A failed reload keeps `requested` set, so the cover stays a placeholder.
            let Some(Pixels::Resident(handle)) = decoded.map(|art| art.pixels) else {
                continue;
            };
            *lock(&stored.slot) = Some(handle);
            let mut state = lock(&self.state);
            state.resident.push(Arc::downgrade(&stored));
            Self::evict(&mut state);
            drop(state);
            if let Some(wake) = lock(&self.wake).as_ref() {
                let _ = wake.unbounded_send(());
            }
        }
    }
}

/// On-disk budget for library cover thumbnails.
///
/// A 256 px thumbnail is roughly 10 to 30 KB, so the budget holds several
/// thousand albums. Covers the loaded library still holds are kept even past
/// it, because evicted pixels reload from these files.
pub const COVER_DISK_BYTES: u64 = 128 * 1024 * 1024;

static PRUNING: AtomicBool = AtomicBool::new(false);

// Thumbnail modification times order disk eviction; access times are often
// not updated on `relatime` or `noatime` mounts.
fn mark_used(file: &Path) {
    if let Ok(handle) = OpenOptions::new().write(true).open(file) {
        let _ = handle.set_modified(SystemTime::now());
    }
}

/// Remove cached thumbnails that neither `referenced` nor a loaded cover
/// names, then trim the least recently used ones to [`COVER_DISK_BYTES`].
///
/// Runs on its own thread and returns at once; a call while a prune is still
/// running is skipped. Does nothing when no cover directory is configured.
pub fn prune_cover_store(referenced: HashSet<CoverKey>) {
    let Some(dir) = STORE.dir.get().cloned() else {
        return;
    };
    if PRUNING.swap(true, Ordering::AcqRel) {
        return;
    }
    let cutoff = SystemTime::now();
    let spawned = std::thread::Builder::new()
        .name("music-cover-prune".into())
        .spawn(move || {
            let referenced: HashSet<String> =
                referenced.into_iter().map(CoverKey::file_name).collect();
            prune_thumbnails(
                &dir,
                &referenced,
                &|| STORE.loaded_file_names(),
                cutoff,
                COVER_DISK_BYTES,
            );
            PRUNING.store(false, Ordering::Release);
        });
    if spawned.is_err() {
        PRUNING.store(false, Ordering::Release);
    }
}

struct Thumbnail {
    path: PathBuf,
    name: String,
    len: u64,
    modified: SystemTime,
}

// Files changed at or after `cutoff` belong to work that started after the
// prune was requested, so they are never removed.
fn prune_thumbnails(
    dir: &Path,
    referenced: &HashSet<String>,
    loaded: &dyn Fn() -> HashSet<String>,
    cutoff: SystemTime,
    budget: u64,
) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut total = 0u64;
    let mut candidates: Vec<Thumbnail> = Vec::new();
    for entry in entries.flatten() {
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        if !meta.is_file() || !(name.ends_with(".jpg") || name.ends_with(".part")) {
            continue;
        }
        total = total.saturating_add(meta.len());
        let modified = meta.modified().unwrap_or(cutoff);
        if modified < cutoff {
            candidates.push(Thumbnail {
                path: entry.path(),
                name,
                len: meta.len(),
                modified,
            });
        }
    }
    let loaded = loaded();
    candidates.retain(|thumbnail| !loaded.contains(&thumbnail.name));
    candidates.sort_by_key(|thumbnail| thumbnail.modified);
    let (orphans, in_use): (Vec<_>, Vec<_>) = candidates
        .into_iter()
        .partition(|thumbnail| !referenced.contains(&thumbnail.name));
    for thumbnail in orphans {
        if std::fs::remove_file(&thumbnail.path).is_ok() {
            total = total.saturating_sub(thumbnail.len);
        }
    }
    for thumbnail in in_use {
        if total <= budget {
            break;
        }
        if std::fs::remove_file(&thumbnail.path).is_ok() {
            total = total.saturating_sub(thumbnail.len);
        }
    }
}

/// Non-cryptographic hash used to recognise an already decoded cover. The
/// cache also keys on byte length (see [`CoverCache::embedded`]).
fn hash_bytes(bytes: &[u8]) -> u64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    bytes.hash(&mut hasher);
    hasher.finish()
}

/// Decode-once cache for one scan.
///
/// Lives for one folder walk; the [`CoverArt`] values it hands out outlive it.
/// Failures are cached too, so a corrupt picture is tried once per album.
#[derive(Default)]
pub struct CoverCache {
    /// keyed by (hash, byte length) so a hash collision between two different
    /// pictures of different sizes cannot serve the wrong art.
    by_bytes: HashMap<(u64, usize), Option<CoverArt>>,
    by_path: HashMap<PathBuf, Option<CoverArt>>,
    decodes: usize,
    #[cfg(test)]
    last_sidecar_read_bytes: usize,
}

impl CoverCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// Decode an embedded picture, reusing an earlier decode of the same bytes.
    pub fn embedded(&mut self, bytes: &[u8]) -> Option<CoverArt> {
        let key = (hash_bytes(bytes), bytes.len());
        if let Some(hit) = self.by_bytes.get(&key) {
            return hit.clone();
        }
        let store_key = CoverKey::of_bytes(bytes, CoverSource::Embedded);
        let art = STORE.shared(store_key).or_else(|| {
            self.decodes += 1;
            decode_cover(bytes, CoverSource::Embedded).map(|art| STORE.admit(store_key, art))
        });
        self.by_bytes.insert(key, art.clone());
        art
    }

    /// Decode a sidecar cover file, reusing an earlier decode of the same path.
    ///
    /// Keyed by path, so a folder's tracks read the shared `cover.jpg` once.
    pub fn sidecar(&mut self, path: &Path) -> Option<CoverArt> {
        if let Some(hit) = self.by_path.get(path) {
            return hit.clone();
        }
        self.decodes += 1;
        let read = read_sidecar(path).ok();
        #[cfg(test)]
        {
            self.last_sidecar_read_bytes =
                read.as_ref().map_or(0, |bounded| bounded.retained_bytes);
        }
        let art = read.and_then(|bounded| bounded.bytes).and_then(|bytes| {
            let store_key = CoverKey::of_bytes(&bytes, CoverSource::Sidecar);
            STORE.shared(store_key).or_else(|| {
                decode_cover(&bytes, CoverSource::Sidecar).map(|art| STORE.admit(store_key, art))
            })
        });
        self.by_path.insert(path.to_path_buf(), art.clone());
        art
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn thumbnail(dir: &Path, name: &str, len: usize, age_secs: u64) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, vec![0u8; len]).unwrap();
        File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(SystemTime::now() - Duration::from_secs(age_secs))
            .unwrap();
        path
    }

    fn names(list: &[&str]) -> HashSet<String> {
        list.iter().map(|name| (*name).to_string()).collect()
    }

    #[test]
    fn prune_removes_unreferenced_thumbnails_but_keeps_loaded_and_new_ones() {
        let dir = tempfile::tempdir().unwrap();
        let cutoff = SystemTime::now() - Duration::from_secs(10);
        let referenced = thumbnail(dir.path(), "a-1-e.jpg", 100, 60);
        let orphan = thumbnail(dir.path(), "b-1-e.jpg", 100, 60);
        let loaded = thumbnail(dir.path(), "c-1-s.jpg", 100, 60);
        let stale_part = thumbnail(dir.path(), "d-1-e.part", 100, 60);
        let written_after_cutoff = thumbnail(dir.path(), "e-1-e.jpg", 100, 0);
        let unrelated = thumbnail(dir.path(), "notes.txt", 100, 60);

        prune_thumbnails(
            dir.path(),
            &names(&["a-1-e.jpg"]),
            &|| names(&["c-1-s.jpg"]),
            cutoff,
            u64::MAX,
        );

        assert!(referenced.exists());
        assert!(!orphan.exists());
        assert!(loaded.exists());
        assert!(!stale_part.exists());
        assert!(written_after_cutoff.exists());
        assert!(unrelated.exists());
    }

    #[test]
    fn prune_evicts_least_recently_used_thumbnails_down_to_the_budget() {
        let dir = tempfile::tempdir().unwrap();
        let cutoff = SystemTime::now();
        let oldest = thumbnail(dir.path(), "a-1-e.jpg", 400, 400);
        let older = thumbnail(dir.path(), "b-1-e.jpg", 400, 300);
        let recent = thumbnail(dir.path(), "c-1-e.jpg", 400, 200);
        let loaded_oldest = thumbnail(dir.path(), "d-1-e.jpg", 400, 900);
        let referenced = names(&["a-1-e.jpg", "b-1-e.jpg", "c-1-e.jpg", "d-1-e.jpg"]);

        prune_thumbnails(
            dir.path(),
            &referenced,
            &|| names(&["d-1-e.jpg"]),
            cutoff,
            1_000,
        );

        assert!(!oldest.exists());
        assert!(!older.exists());
        assert!(recent.exists());
        assert!(loaded_oldest.exists());
    }

    #[test]
    fn using_a_thumbnail_makes_it_the_most_recent_for_eviction() {
        let dir = tempfile::tempdir().unwrap();
        let cutoff = SystemTime::now() + Duration::from_secs(60);
        let reused = thumbnail(dir.path(), "a-1-e.jpg", 400, 400);
        let idle = thumbnail(dir.path(), "b-1-e.jpg", 400, 100);
        mark_used(&reused);

        prune_thumbnails(
            dir.path(),
            &names(&["a-1-e.jpg", "b-1-e.jpg"]),
            &HashSet::new,
            cutoff,
            500,
        );

        assert!(reused.exists());
        assert!(!idle.exists());
    }

    #[test]
    fn a_3000_pixel_podcast_cover_decodes_to_a_thumbnail() {
        let image = image::RgbImage::from_pixel(3000, 3000, image::Rgb([200, 40, 90]));
        let mut bytes = Vec::new();
        image::DynamicImage::ImageRgb8(image)
            .write_to(
                &mut std::io::Cursor::new(&mut bytes),
                image::ImageFormat::Jpeg,
            )
            .unwrap();
        assert!(decode_cover(&bytes, CoverSource::Embedded).is_none());
        let cover = decode_cover_within(&bytes, CoverSource::Embedded, &REMOTE_COVER_DECODE_LIMITS)
            .unwrap();
        assert!(cover.width <= COVER_THUMB_PX && cover.height <= COVER_THUMB_PX);
    }
}
