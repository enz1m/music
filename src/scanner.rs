//! Library scanner — recursive folder scan with tag extraction.
//!
//! The direct folder-walk primitive; production traversal is
//! [`crate::library_watch::scan_root_bounded`].

use std::path::{Path, PathBuf};

use crate::art::CoverCache;
use crate::domain::Track;

/// Events streamed from the scanner background thread.
#[derive(Debug)]
/// Never constructed: scans return a whole `Vec<Track>`. This is the
/// streaming protocol a progressive walk would publish.
#[allow(dead_code)]
pub enum ScanEvent {
    Started(PathBuf),
    Track(Box<Track>),
    Finished { root: PathBuf, count: usize },
    Failed { root: PathBuf, reason: String },
}

/// Supported audio file extensions.
const AUDIO_EXTENSIONS: &[&str] = &[
    "mp3", "flac", "m4a", "ogg", "opus", "wav", "aac", "wma", "oga",
];

pub(crate) fn is_audio_file(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| AUDIO_EXTENSIONS.contains(&e.to_lowercase().as_str()))
        .unwrap_or(false)
}

/// Scan one file's tags and cover art, reusing a caller-owned [`CoverCache`].
pub fn scan_track_with_covers(
    path: &Path,
    cache: Option<&Path>,
    covers: &mut CoverCache,
) -> Result<Track, String> {
    use lofty::config::ParseOptions;
    use lofty::file::AudioFile;
    use lofty::prelude::*;
    use lofty::probe::Probe;

    let tagged = Probe::open(path)
        .map_err(|e| format!("probe: {e}"))?
        .options(ParseOptions::new().read_properties(true))
        .guess_file_type()
        .map_err(|e| format!("guess: {e}"))?
        .read()
        .map_err(|e| format!("read: {e}"))?;

    let properties = tagged.properties();
    let duration_secs = properties.duration().as_secs_f64();

    let tag = tagged.primary_tag().or_else(|| tagged.first_tag()).cloned();

    let (title, artist, album, album_artist, track_no, year, disc_no, genre) =
        if let Some(ref tag) = tag {
            (
                tag.title().unwrap_or_default().to_string(),
                tag.artist().unwrap_or_default().to_string(),
                tag.album().unwrap_or_default().to_string(),
                tag.get_string(&ItemKey::AlbumArtist)
                    .unwrap_or_default()
                    .to_string(),
                tag.track().unwrap_or(0),
                tag.year().unwrap_or(0),
                tag.disk().unwrap_or(0),
                tag.get_string(&ItemKey::Genre)
                    .unwrap_or_default()
                    .to_string(),
            )
        } else {
            (
                String::new(),
                String::new(),
                String::new(),
                String::new(),
                0,
                0,
                0,
                String::new(),
            )
        };

    let format = format!("{:?}", tagged.file_type());
    let bitrate_kbps = properties.overall_bitrate().unwrap_or(0);
    let sample_rate = properties.sample_rate().unwrap_or(0);
    let channels = properties.channels().unwrap_or(0);
    let bit_depth = properties.bit_depth().unwrap_or(0);

    let mut cover_path: Option<PathBuf> = None;
    // Decoded and downscaled on the scan worker so the UI thread never decodes
    // a cover and the model holds no full-size bitmap.
    let mut cover: Option<crate::art::CoverArt> = None;

    if let Some(ref tag) = tag
        && let Some(pic) = tag.pictures().first()
    {
        let bytes = pic.data();
        cover = covers.embedded(bytes);
        if let Some(cache) = cache {
            let hash = hash_bytes(bytes);
            let cover_file = cache.join(format!("{hash}.jpg"));
            if !cover_file.exists()
                && let Some(ref art) = cover
            {
                let _ = art.save_jpeg(&cover_file);
            }
            cover_path = Some(cover_file);
        }
    }

    // Sidecar fallback (e.g. `cover.jpg`) when no embedded picture decodes.
    if (cover_path.is_none() || cover.is_none())
        && let Some(parent) = path.parent()
    {
        for name in ["cover.jpg", "cover.png", "folder.jpg", "folder.png"] {
            let candidate = parent.join(name);
            if candidate.exists() {
                if cover.is_none() {
                    cover = covers.sidecar(&candidate);
                }
                if cover_path.is_none() {
                    cover_path = Some(candidate);
                }
                break;
            }
        }
    }

    let title = if title.is_empty() {
        path.file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("Unknown")
            .to_string()
    } else {
        title
    };

    // ReplayGain tags come from the same `tag` block as title/artist/album; each
    // key is independently optional.
    let replay_gain = if let Some(ref tag) = tag {
        let get = |key: &str| tag.get_string(&ItemKey::Unknown(key.to_string()));
        crate::domain::ReplayGain {
            track_gain_db: get("REPLAYGAIN_TRACK_GAIN").and_then(parse_gain_db),
            track_peak: get("REPLAYGAIN_TRACK_PEAK").and_then(parse_peak),
            album_gain_db: get("REPLAYGAIN_ALBUM_GAIN").and_then(parse_gain_db),
            album_peak: get("REPLAYGAIN_ALBUM_PEAK").and_then(parse_peak),
        }
    } else {
        crate::domain::ReplayGain::default()
    };

    Ok(Track {
        id: 0, // assigned by Library::add_track
        path: path.to_path_buf(),
        title,
        artist,
        album: if album.is_empty() {
            "Unknown Album".into()
        } else {
            album
        },
        album_artist,
        track_no,
        disc_no,
        year,
        genre,
        format,
        duration_secs,
        bitrate_kbps,
        sample_rate,
        channels,
        bit_depth,
        cover_path,
        cover,
        // File mtime orders "Recently added", so a rescan keeps the order; now() is
        // the fallback when the stat fails.
        added_at: std::fs::metadata(path)
            .and_then(|m| m.modified())
            .unwrap_or_else(|_| std::time::SystemTime::now())
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0),
        play_count: 0,
        last_played_seq: 0,
        last_played_unix: 0,
        replay_gain,
    })
}

/// Parse a `REPLAYGAIN_*_GAIN` tag value into a dB float.
///
/// Accepts `-6.24 dB`, `+1.5`, comma decimals (`-6,24 dB`) and any unit
/// case. Rejects non-finite values and magnitudes beyond ±[`GAIN_SANITY_DB`],
/// which would drive the sink into destructive gain.
pub fn parse_gain_db(raw: &str) -> Option<f32> {
    let t = raw.trim();
    let t = t
        .strip_suffix("dB")
        .or_else(|| t.strip_suffix("DB"))
        .or_else(|| t.strip_suffix("Db"))
        .or_else(|| t.strip_suffix("db"))
        .unwrap_or(t)
        .trim();
    if t.is_empty() {
        return None;
    }
    let v: f32 = t.replace(',', ".").parse().ok()?;
    if !v.is_finite() || v.abs() > GAIN_SANITY_DB {
        return None;
    }
    Some(v)
}

/// The largest plausible ReplayGain magnitude, in dB. Anything beyond this is
/// treated as a corrupt tag.
pub const GAIN_SANITY_DB: f32 = 24.0;

/// Parse a `REPLAYGAIN_*_PEAK` tag value into a linear sample peak.
///
/// Peaks above 1.0 are legal (the file already clips). Comma decimals are
/// accepted. Zero, negative and non-finite peaks are rejected, since clip
/// protection divides by the peak.
pub fn parse_peak(raw: &str) -> Option<f32> {
    let v: f32 = raw.trim().replace(',', ".").parse().ok()?;
    if !v.is_finite() || v <= 0.0 || v > 4.0 {
        return None;
    }
    Some(v)
}

fn hash_bytes(bytes: &[u8]) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    bytes.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- ReplayGain tag parsing ----

    #[test]
    fn gain_parsing_survives_every_real_world_spelling() {
        assert_eq!(parse_gain_db("-6.24 dB"), Some(-6.24));
        assert_eq!(parse_gain_db("+1.5 dB"), Some(1.5));
        assert_eq!(parse_gain_db("-6.24"), Some(-6.24));
        assert_eq!(parse_gain_db("-6,24 dB"), Some(-6.24), "comma decimal");
        assert_eq!(parse_gain_db("-6.24 DB"), Some(-6.24), "upper case unit");
        assert_eq!(parse_gain_db("-6.24 Db"), Some(-6.24), "mixed case unit");
        assert_eq!(parse_gain_db("   -6.24  dB   "), Some(-6.24), "whitespace");
        // 0 dB is a real value (already at reference loudness), not absent.
        assert_eq!(parse_gain_db("0.00 dB"), Some(0.0));
    }

    #[test]
    fn gain_parsing_rejects_nonsense_and_destructive_magnitudes() {
        for bad in ["", "   ", "loud", "dB", "nan dB", "inf dB", "-inf"] {
            assert_eq!(parse_gain_db(bad), None, "{bad:?} must be rejected");
        }
        assert_eq!(parse_gain_db("100 dB"), None, "absurdly loud");
        assert_eq!(parse_gain_db("-100 dB"), None, "absurdly quiet");
        // The boundary itself is accepted; just past it is not.
        assert_eq!(parse_gain_db("24 dB"), Some(GAIN_SANITY_DB));
        assert_eq!(parse_gain_db("24.5 dB"), None);
    }

    #[test]
    fn peak_parsing_keeps_valid_peaks_and_rejects_degenerate_ones() {
        assert_eq!(parse_peak("0.98481"), Some(0.98481));
        assert_eq!(parse_peak("0,984812"), Some(0.984812), "comma decimal");
        assert_eq!(parse_peak(" 1.0 "), Some(1.0));
        assert_eq!(parse_peak("1.25"), Some(1.25), "a peak > 1 is legal");

        for bad in ["0", "0.0", "-0.5", "nan", "inf", "", "loud"] {
            assert_eq!(
                parse_peak(bad),
                None,
                "{bad:?} must be rejected — a zero peak would make clip \
                 protection divide by zero"
            );
        }
    }

    #[test]
    fn parsed_tags_feed_the_clip_protection() {
        use crate::domain::{
            EqState, PlaybackState, ReplayGain, ReplayGainMode, ReplayGainState,
            effective_sample_scale_with_rg,
        };

        let rg = ReplayGain {
            track_gain_db: parse_gain_db("+6.00 dB"),
            track_peak: parse_peak("0.98"),
            album_gain_db: None,
            album_peak: None,
        };
        assert!(rg.has_any(), "the parsers must have produced a gain");
        let peak = rg
            .track_peak
            .expect("the parsers must have produced a peak");

        let pb = PlaybackState {
            volume: 1.0,
            ..PlaybackState::default()
        };
        let eq = EqState::default();
        let on = ReplayGainState {
            mode: ReplayGainMode::Track,
            prevent_clip: true,
            ..ReplayGainState::default()
        };

        let protected = effective_sample_scale_with_rg(&pb, &eq, &on, &rg);
        assert!(
            peak * protected <= 1.0 + 1e-6,
            "the PARSED peak must be what caps the scale; got {} ",
            peak * protected
        );

        let mut off = on;
        off.prevent_clip = false;
        assert!(
            peak * effective_sample_scale_with_rg(&pb, &eq, &off, &rg) > 1.0,
            "and without protection this parsed track must clip, or the tags \
             are not reaching the audio path at all"
        );
    }
}
