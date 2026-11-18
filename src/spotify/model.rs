//! What Music shows from Spotify, and reading it out of Web API replies.

use serde_json::Value;

use super::{API, Failure};

/// A name with the Spotify URI it links to (an artist, an album).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Link {
    pub uri: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Track {
    pub uri: String,
    pub name: String,
    pub artists: Vec<Link>,
    pub album: Option<Link>,
    pub duration_ms: u64,
    /// A small cover for the row.
    pub thumb: Option<String>,
    pub explicit: bool,
    /// The track's number on its disc, inside an album.
    pub number: u32,
}

impl Track {
    #[must_use]
    pub fn artist_names(&self) -> String {
        join_names(&self.artists)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Album {
    pub uri: String,
    pub name: String,
    pub artists: Vec<Link>,
    /// A card-sized cover.
    pub image: Option<String>,
    pub thumb: Option<String>,
    /// The release year.
    pub year: String,
    /// `album`, `single` or `compilation`.
    pub album_type: String,
    /// How the album relates to an artist on that artist's page.
    pub group: String,
    pub total_tracks: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Artist {
    pub uri: String,
    pub name: String,
    pub image: Option<String>,
    pub thumb: Option<String>,
    pub genres: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Playlist {
    pub uri: String,
    pub name: String,
    pub owner_id: String,
    pub owner: String,
    pub description: String,
    pub image: Option<String>,
    pub thumb: Option<String>,
    pub total: u32,
    pub collaborative: bool,
}

/// One page of a list, with where the next one is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Page<T> {
    pub items: Vec<T>,
    pub next: Option<String>,
    pub total: u32,
}

impl<T> Default for Page<T> {
    fn default() -> Self {
        Self {
            items: Vec::new(),
            next: None,
            total: 0,
        }
    }
}

/// What Spotify is playing, and where.
#[derive(Debug, Clone, PartialEq)]
pub struct Playing {
    pub track: Option<Track>,
    /// The cover at the size the now-playing panel draws it.
    pub cover: Option<String>,
    pub is_playing: bool,
    pub progress_ms: u64,
    pub shuffle: bool,
    /// `off`, `context` or `track`.
    pub repeat: String,
    pub device: Option<Device>,
    pub context_uri: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Device {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub active: bool,
    pub volume: Option<u8>,
    pub supports_volume: bool,
}

/// The current user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Me {
    pub id: String,
    pub name: String,
}

fn text(value: &Value, max: usize) -> String {
    value
        .as_str()
        .unwrap_or_default()
        .chars()
        .take(max)
        .collect()
}

fn join_names(links: &[Link]) -> String {
    links
        .iter()
        .map(|l| l.name.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

fn links(list: &Value) -> Vec<Link> {
    list.as_array()
        .map(|items| {
            items
                .iter()
                .take(12)
                .filter_map(|a| {
                    Some(Link {
                        uri: a["uri"].as_str()?.to_owned(),
                        name: text(&a["name"], 256),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The image nearest `edge` pixels wide (Spotify lists them largest first;
/// some have no sizes).
pub fn image_near(images: &Value, edge: u64) -> Option<String> {
    let images = images.as_array()?;
    images
        .iter()
        .filter(|i| i["url"].as_str().is_some_and(|u| u.starts_with("https://")))
        .min_by_key(|i| {
            i["width"].as_u64().map_or(u64::MAX / 2, |w| {
                if w >= edge { w - edge } else { (edge - w) * 3 }
            })
        })
        .and_then(|i| i["url"].as_str())
        .map(str::to_owned)
}

/// Row thumbnails, sharp on a scaled display.
const THUMB: u64 = 128;
/// Cards and headers.
const CARD: u64 = 300;
/// The now-playing cover.
const COVER: u64 = 640;

pub fn track(value: &Value, album_fallback: Option<&Album>) -> Option<Track> {
    let uri = value["uri"].as_str()?;
    if !uri.starts_with("spotify:track:") {
        return None;
    }
    let album = value["album"]["uri"].as_str().map(|uri| Link {
        uri: uri.to_owned(),
        name: text(&value["album"]["name"], 256),
    });
    Some(Track {
        uri: uri.to_owned(),
        name: text(&value["name"], 512),
        artists: links(&value["artists"]),
        album: album.or_else(|| {
            album_fallback.map(|a| Link {
                uri: a.uri.clone(),
                name: a.name.clone(),
            })
        }),
        duration_ms: value["duration_ms"].as_u64().unwrap_or(0),
        thumb: image_near(&value["album"]["images"], THUMB)
            .or_else(|| album_fallback.and_then(|a| a.thumb.clone())),
        explicit: value["explicit"] == true,
        number: value["track_number"].as_u64().unwrap_or(0) as u32,
    })
}

pub fn album(value: &Value) -> Option<Album> {
    let uri = value["uri"].as_str()?;
    if !uri.starts_with("spotify:album:") {
        return None;
    }
    Some(Album {
        uri: uri.to_owned(),
        name: text(&value["name"], 512),
        artists: links(&value["artists"]),
        image: image_near(&value["images"], CARD),
        thumb: image_near(&value["images"], THUMB),
        year: text(&value["release_date"], 4),
        album_type: text(&value["album_type"], 32),
        group: text(&value["album_group"], 32),
        total_tracks: value["total_tracks"].as_u64().unwrap_or(0) as u32,
    })
}

pub fn artist(value: &Value) -> Option<Artist> {
    let uri = value["uri"].as_str()?;
    if !uri.starts_with("spotify:artist:") {
        return None;
    }
    Some(Artist {
        uri: uri.to_owned(),
        name: text(&value["name"], 256),
        image: image_near(&value["images"], CARD),
        thumb: image_near(&value["images"], THUMB),
        genres: value["genres"]
            .as_array()
            .map(|g| {
                g.iter()
                    .take(3)
                    .filter_map(|g| g.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default(),
    })
}

pub fn playlist(value: &Value) -> Option<Playlist> {
    let uri = value["uri"].as_str()?;
    if !uri.starts_with("spotify:playlist:") {
        return None;
    }
    // `items` since February 2026, `tracks` before.
    let total = value["items"]["total"]
        .as_u64()
        .or_else(|| value["tracks"]["total"].as_u64())
        .unwrap_or(0) as u32;
    Some(Playlist {
        uri: uri.to_owned(),
        name: text(&value["name"], 512),
        owner_id: text(&value["owner"]["id"], 128),
        owner: value["owner"]["display_name"]
            .as_str()
            .or_else(|| value["owner"]["id"].as_str())
            .unwrap_or("Spotify")
            .chars()
            .take(128)
            .collect(),
        description: strip_tags(&text(&value["description"], 1024)),
        image: image_near(&value["images"], CARD),
        thumb: image_near(&value["images"], THUMB),
        total,
        collaborative: value["collaborative"] == true,
    })
}

/// Playlist descriptions arrive with HTML links and entities.
fn strip_tags(text: &str) -> String {
    let mut out = String::new();
    let mut inside = false;
    for c in text.chars() {
        match c {
            '<' => inside = true,
            '>' => inside = false,
            c if !inside => out.push(c),
            _ => {}
        }
    }
    out.replace("&amp;", "&")
        .replace("&quot;", "\"")
        .replace("&#x27;", "'")
        .replace("&#39;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
}

/// The `next` link as a path under the API root, refusing anything else.
pub fn next_path(page: &Value) -> Option<String> {
    page["next"]
        .as_str()
        .and_then(|url| url.strip_prefix(API))
        .filter(|path| {
            path.starts_with('/') && !path.starts_with("//") && !path.contains(['\r', '\n', '#'])
        })
        .map(str::to_owned)
}

/// A page object's items, each through `parse`. Saved and playlist entries
/// wrap the object (`track`, `item`, `album`); those are unwrapped first.
pub fn page<T>(page: &Value, parse: impl Fn(&Value) -> Option<T>) -> Result<Page<T>, Failure> {
    let items = page["items"].as_array().ok_or(Failure::InvalidReply)?;
    Ok(Page {
        items: items
            .iter()
            .take(100)
            .filter_map(|entry| {
                // A wrapper has no URI of its own; a track has an `album`
                // that isn't its wrapper.
                let inner = if entry.get("uri").is_some() {
                    entry
                } else {
                    ["track", "item", "album"]
                        .iter()
                        .find_map(|k| entry.get(*k).filter(|v| v.is_object()))
                        .unwrap_or(entry)
                };
                parse(inner)
            })
            .collect(),
        next: next_path(page),
        total: page["total"].as_u64().unwrap_or(0) as u32,
    })
}

pub fn playing(value: &Value) -> Playing {
    let item = &value["item"];
    Playing {
        track: track(item, None),
        cover: image_near(&item["album"]["images"], COVER),
        is_playing: value["is_playing"] == true,
        progress_ms: value["progress_ms"].as_u64().unwrap_or(0),
        shuffle: value["shuffle_state"] == true,
        repeat: value["repeat_state"].as_str().unwrap_or("off").to_owned(),
        device: device(&value["device"]),
        context_uri: value["context"]["uri"].as_str().map(str::to_owned),
    }
}

pub fn device(value: &Value) -> Option<Device> {
    Some(Device {
        id: value["id"].as_str()?.to_owned(),
        name: text(&value["name"], 128),
        kind: text(&value["type"], 32),
        active: value["is_active"] == true,
        volume: value["volume_percent"].as_u64().map(|v| v.min(100) as u8),
        supports_volume: value["supports_volume"] != false,
    })
}

pub fn devices(value: &Value) -> Result<Vec<Device>, Failure> {
    Ok(value["devices"]
        .as_array()
        .ok_or(Failure::InvalidReply)?
        .iter()
        .take(50)
        .filter(|d| d["is_restricted"] != true)
        .filter_map(device)
        .collect())
}

pub fn me(value: &Value) -> Option<Me> {
    Some(Me {
        id: value["id"].as_str()?.to_owned(),
        name: value["display_name"]
            .as_str()
            .or_else(|| value["id"].as_str())?
            .chars()
            .take(128)
            .collect(),
    })
}

/// A duration as `m:ss` (or `h:mm:ss`).
#[must_use]
pub fn clock(ms: u64) -> String {
    let secs = ms / 1000;
    if secs >= 3600 {
        format!("{}:{:02}:{:02}", secs / 3600, secs / 60 % 60, secs % 60)
    } else {
        format!("{}:{:02}", secs / 60, secs % 60)
    }
}

/// A total length in words: "1 hr 12 min", "38 min".
#[must_use]
pub fn length_words(ms: u64) -> String {
    let mins = ms / 60_000;
    if mins >= 60 {
        format!("{} hr {} min", mins / 60, mins % 60)
    } else {
        format!("{} min", mins.max(1))
    }
}
