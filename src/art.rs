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

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{self, Read};
use std::path::{Path, PathBuf};

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
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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
    handle: CoverHandle,
    source: CoverSource,
    width: u32,
    height: u32,
    /// The cover's leading colours, most prominent first, for the Now
    /// Playing backdrop.
    palette: [[u8; 3]; 3],
}

impl CoverArt {
    /// The drawable handle. Clone it into a widget; the clone shares pixels.
    pub fn handle(&self) -> &CoverHandle {
        &self.handle
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
        let CoverHandle::Rgba {
            width,
            height,
            pixels,
            ..
        } = &self.handle
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
        handle,
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
        self.decodes += 1;
        let art = decode_cover(bytes, CoverSource::Embedded);
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
        let art = read
            .and_then(|bounded| bounded.bytes)
            .and_then(|bytes| decode_cover(&bytes, CoverSource::Sidecar));
        self.by_path.insert(path.to_path_buf(), art.clone());
        art
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
