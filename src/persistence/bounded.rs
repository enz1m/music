//! Pre-retention bounded deserialization for the persisted collections.
//!
//! `#[derive(Deserialize)]` skips unknown fields without building a value tree,
//! but it grows a collection to whatever length the input names and decodes
//! each string into an owned `String` before any post-parse check runs. Two
//! authorities are applied here, during deserialization, before the next value
//! is retained:
//!
//! * cardinality: every collection refuses at its domain count via
//!   [`bounded_vec`] (records) or the path/string sequence readers, dropping the
//!   over-cap element rather than retaining it;
//! * per-value and aggregate bytes: every path, playlist name, feed string and
//!   uuid is read through a length-checked visitor that refuses over its
//!   per-field cap and charges [`crate::domain::MAX_AGGREGATE_STRING_BYTES`]
//!   before the decoded string is retained, so neither a single long value nor
//!   many short ones can force unbounded retention under the raw byte ceiling.
//!
//! The post-parse `over_domain_limit` audit re-checks the same authorities as
//! defense-in-depth and as the save-side gate; it is not the load authority.

use std::cell::Cell;
use std::marker::PhantomData;
use std::path::PathBuf;

use serde::Deserialize;
use serde::de::{self, DeserializeSeed, Deserializer, SeqAccess, Visitor};

use super::{EpisodeResumeRec, PlaylistRec, SubscriptionRec, TrackAddedRec, TrackStatsRec};
use crate::domain::{
    MAX_AGGREGATE_STRING_BYTES, MAX_EPISODE_GUID_BYTES, MAX_EPISODE_RESUME,
    MAX_FEED_DESCRIPTION_BYTES, MAX_FEED_TITLE_BYTES, MAX_FEED_URL_BYTES, MAX_LIBRARY_FOLDERS,
    MAX_PERSISTED_PATH_BYTES, MAX_PERSISTED_PLAYLISTS, MAX_PERSISTED_TRACKS,
    MAX_PLAYLIST_NAME_BYTES, MAX_QUEUE_PATHS, MAX_RADIO_FAVORITES, MAX_RADIO_UUID_BYTES,
    MAX_SUBSCRIPTIONS, MAX_TRACKS_PER_PLAYLIST,
};

/// Prefix on a bounded-deserialization error so `super::load_from` can map it
/// to a typed [`super::LoadError::OverLimit`] rather than a generic corrupt.
pub(super) const BOUND_MARKER: &str = "music-state-over-limit";

thread_local! {
    /// Aggregate variable-length bytes retained by the parse in progress on this
    /// thread. Reset by [`reset_retained`] at each parse entry.
    static RETAINED_BYTES: Cell<u64> = const { Cell::new(0) };
}

/// Reset the aggregate retained-byte meter at the start of one parse.
///
/// `super::load_from` calls this before `serde_json::from_str`, so each parse
/// begins at zero and a prior parse's total never leaks across loads.
pub(super) fn reset_retained() {
    RETAINED_BYTES.with(|c| c.set(0));
}

/// Charge `n` retained bytes against the aggregate budget, refusing before the
/// value is retained once the running total exceeds the domain authority.
fn charge<E: de::Error>(n: usize) -> Result<(), E> {
    let total = RETAINED_BYTES.with(|c| {
        let next = c.get().saturating_add(n as u64);
        c.set(next);
        next
    });
    if total > MAX_AGGREGATE_STRING_BYTES as u64 {
        return Err(de::Error::custom(format!(
            "{BOUND_MARKER}: aggregate persisted bytes exceed {MAX_AGGREGATE_STRING_BYTES}"
        )));
    }
    Ok(())
}

fn too_long<E: de::Error>(what: &str, n: usize, cap: usize) -> E {
    de::Error::custom(format!(
        "{BOUND_MARKER}: {what} too long: {n} exceeds {cap}"
    ))
}

/// Read one string, refusing over `cap` bytes and charging the aggregate before
/// the decoded value is retained.
///
/// A borrowed string is length-checked before it is owned; an escaped string is
/// checked before it enters the returned value, so a hostile field cannot be
/// retained past its per-field or the aggregate authority.
fn bounded_string<'de, D>(d: D, cap: usize, what: &'static str) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    struct V {
        cap: usize,
        what: &'static str,
    }

    impl<'de> Visitor<'de> for V {
        type Value = String;

        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            write!(f, "a {} of at most {} bytes", self.what, self.cap)
        }

        fn visit_str<E: de::Error>(self, s: &str) -> Result<String, E> {
            if s.len() > self.cap {
                return Err(too_long(self.what, s.len(), self.cap));
            }
            charge::<E>(s.len())?;
            Ok(s.to_owned())
        }

        fn visit_string<E: de::Error>(self, s: String) -> Result<String, E> {
            if s.len() > self.cap {
                return Err(too_long(self.what, s.len(), self.cap));
            }
            charge::<E>(s.len())?;
            Ok(s)
        }
    }

    d.deserialize_str(V { cap, what })
}

/// Read one path with the same per-value and aggregate authority as a string.
fn bounded_path<'de, D>(d: D, cap: usize, what: &'static str) -> Result<PathBuf, D::Error>
where
    D: Deserializer<'de>,
{
    bounded_string(d, cap, what).map(PathBuf::from)
}

struct PathSeed {
    cap: usize,
    what: &'static str,
}

impl<'de> DeserializeSeed<'de> for PathSeed {
    type Value = PathBuf;

    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<PathBuf, D::Error> {
        bounded_path(d, self.cap, self.what)
    }
}

struct StringSeed {
    cap: usize,
    what: &'static str,
}

impl<'de> DeserializeSeed<'de> for StringSeed {
    type Value = String;

    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<String, D::Error> {
        bounded_string(d, self.cap, self.what)
    }
}

/// Read a homogeneous array of records, refusing once `cap` elements are
/// retained. The record's own string/path fields carry the per-value and
/// aggregate authority through their field readers below.
fn bounded_vec<'de, D, T>(d: D, cap: usize, what: &'static str) -> Result<Vec<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    struct V<T> {
        cap: usize,
        what: &'static str,
        _elem: PhantomData<T>,
    }

    impl<'de, T> Visitor<'de> for V<T>
    where
        T: Deserialize<'de>,
    {
        type Value = Vec<T>;

        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            write!(f, "at most {} {} elements", self.cap, self.what)
        }

        fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
        where
            A: SeqAccess<'de>,
        {
            let mut out: Vec<T> = Vec::new();
            while let Some(item) = seq.next_element::<T>()? {
                if out.len() >= self.cap {
                    return Err(de::Error::custom(format!(
                        "{BOUND_MARKER}: too many {} (limit {})",
                        self.what, self.cap
                    )));
                }
                out.push(item);
            }
            Ok(out)
        }
    }

    d.deserialize_seq(V {
        cap,
        what,
        _elem: PhantomData,
    })
}

/// Read an array of paths, refusing over the cardinality `cap`, over the
/// per-path byte cap, or over the aggregate budget, all before retention.
fn bounded_path_vec<'de, D>(d: D, cap: usize, what: &'static str) -> Result<Vec<PathBuf>, D::Error>
where
    D: Deserializer<'de>,
{
    struct V {
        cap: usize,
        what: &'static str,
    }

    impl<'de> Visitor<'de> for V {
        type Value = Vec<PathBuf>;

        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            write!(f, "at most {} {} paths", self.cap, self.what)
        }

        fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
        where
            A: SeqAccess<'de>,
        {
            let mut out: Vec<PathBuf> = Vec::new();
            while let Some(path) = seq.next_element_seed(PathSeed {
                cap: MAX_PERSISTED_PATH_BYTES,
                what: self.what,
            })? {
                if out.len() >= self.cap {
                    return Err(de::Error::custom(format!(
                        "{BOUND_MARKER}: too many {} (limit {})",
                        self.what, self.cap
                    )));
                }
                out.push(path);
            }
            Ok(out)
        }
    }

    d.deserialize_seq(V { cap, what })
}

/// Read an array of capped strings with cardinality, per-value and aggregate
/// authority applied before each element is retained.
fn bounded_string_vec<'de, D>(
    d: D,
    cap: usize,
    per_value: usize,
    what: &'static str,
) -> Result<Vec<String>, D::Error>
where
    D: Deserializer<'de>,
{
    struct V {
        cap: usize,
        per_value: usize,
        what: &'static str,
    }

    impl<'de> Visitor<'de> for V {
        type Value = Vec<String>;

        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            write!(f, "at most {} {} strings", self.cap, self.what)
        }

        fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
        where
            A: SeqAccess<'de>,
        {
            let mut out: Vec<String> = Vec::new();
            while let Some(item) = seq.next_element_seed(StringSeed {
                cap: self.per_value,
                what: self.what,
            })? {
                if out.len() >= self.cap {
                    return Err(de::Error::custom(format!(
                        "{BOUND_MARKER}: too many {} (limit {})",
                        self.what, self.cap
                    )));
                }
                out.push(item);
            }
            Ok(out)
        }
    }

    d.deserialize_seq(V {
        cap,
        per_value,
        what,
    })
}

pub(super) fn folders<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<PathBuf>, D::Error> {
    bounded_path_vec(d, MAX_LIBRARY_FOLDERS, "library folder")
}

pub(super) fn playlists<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<PlaylistRec>, D::Error> {
    bounded_vec(d, MAX_PERSISTED_PLAYLISTS, "playlists")
}

pub(super) fn playlist_track_paths<'de, D: Deserializer<'de>>(
    d: D,
) -> Result<Vec<PathBuf>, D::Error> {
    bounded_path_vec(d, MAX_TRACKS_PER_PLAYLIST, "playlist track")
}

pub(super) fn subscriptions<'de, D: Deserializer<'de>>(
    d: D,
) -> Result<Vec<SubscriptionRec>, D::Error> {
    bounded_vec(d, MAX_SUBSCRIPTIONS, "subscriptions")
}

pub(super) fn radio_favorites<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u64>, D::Error> {
    bounded_vec(d, MAX_RADIO_FAVORITES, "radio favourites")
}

pub(super) fn radio_favorite_uuids<'de, D: Deserializer<'de>>(
    d: D,
) -> Result<Vec<String>, D::Error> {
    bounded_string_vec(
        d,
        MAX_RADIO_FAVORITES,
        MAX_RADIO_UUID_BYTES,
        "radio favourite uuid",
    )
}

pub(super) fn track_added<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<TrackAddedRec>, D::Error> {
    bounded_vec(d, MAX_PERSISTED_TRACKS, "first-seen records")
}

pub(super) fn track_stats<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<TrackStatsRec>, D::Error> {
    bounded_vec(d, MAX_PERSISTED_TRACKS, "play-stats records")
}

/// The per-episode resume list, capped at [`MAX_EPISODE_RESUME`]
/// records. Each record's `feed_url` / `guid` strings are further bounded by
/// their own field deserializers during retention.
pub(super) fn episode_resume<'de, D: Deserializer<'de>>(
    d: D,
) -> Result<Vec<EpisodeResumeRec>, D::Error> {
    bounded_vec(d, MAX_EPISODE_RESUME, "episode resume records")
}

/// A persisted episode GUID, bounded like a feed URL because a GUID is
/// frequently a full item URL.
pub(super) fn episode_guid<'de, D: Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    bounded_string(d, MAX_EPISODE_GUID_BYTES, "episode guid")
}

pub(super) fn queue_order<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<usize>, D::Error> {
    bounded_vec(d, MAX_QUEUE_PATHS, "unshuffled queue order")
}

pub(super) fn queue_paths<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<PathBuf>, D::Error> {
    bounded_path_vec(d, MAX_QUEUE_PATHS, "queued track")
}

pub(super) fn path_field<'de, D: Deserializer<'de>>(d: D) -> Result<PathBuf, D::Error> {
    bounded_path(d, MAX_PERSISTED_PATH_BYTES, "track")
}

pub(super) fn optional_path_field<'de, D: Deserializer<'de>>(
    d: D,
) -> Result<Option<PathBuf>, D::Error> {
    struct OptV;

    impl<'de> Visitor<'de> for OptV {
        type Value = Option<PathBuf>;

        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            write!(f, "an optional bounded path")
        }

        fn visit_none<E: de::Error>(self) -> Result<Self::Value, E> {
            Ok(None)
        }

        fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
            Ok(None)
        }

        fn visit_some<D: Deserializer<'de>>(self, d: D) -> Result<Self::Value, D::Error> {
            bounded_path(d, MAX_PERSISTED_PATH_BYTES, "current track").map(Some)
        }
    }

    d.deserialize_option(OptV)
}

pub(super) fn playlist_name<'de, D: Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    bounded_string(d, MAX_PLAYLIST_NAME_BYTES, "playlist name")
}

pub(super) fn feed_url<'de, D: Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    bounded_string(d, MAX_FEED_URL_BYTES, "feed url")
}

pub(super) fn feed_title<'de, D: Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    bounded_string(d, MAX_FEED_TITLE_BYTES, "feed title")
}

pub(super) fn feed_description<'de, D: Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    bounded_string(d, MAX_FEED_DESCRIPTION_BYTES, "feed description")
}

pub(super) fn rating<'de, D: Deserializer<'de>>(deserializer: D) -> Result<u8, D::Error> {
    let value = u8::deserialize(deserializer)?;
    if value > 5 {
        return Err(de::Error::custom("rating exceeds five"));
    }
    Ok(value)
}

pub(super) fn rate<'de, D: Deserializer<'de>>(deserializer: D) -> Result<f64, D::Error> {
    let value = f64::deserialize(deserializer)?;
    enzui_services::media::sanitize_rate(value)
        .ok_or_else(|| de::Error::custom("invalid playback rate"))
}
