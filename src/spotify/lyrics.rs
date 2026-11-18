//! Lyrics for Spotify songs, from LRCLIB.
//!
//! Spotify shows lyrics in its own apps but offers them to no other app, so
//! Music asks LRCLIB (lrclib.net), a free, open lyrics database with lines
//! timed to the song for most of what Spotify plays. Only the song's title,
//! artist, album and length are sent.

use std::time::Duration;

use serde_json::Value;

use super::{Lyrics, Track, encode};
use crate::track_details::parse_lyrics;

const API: &str = "https://lrclib.net/api";
/// LRCLIB asks clients to name themselves.
const USER_AGENT: &str = "Music (enzUI desktop player)";
const MAX_REPLY: usize = 512 * 1024;

/// The song's lyrics, timed when LRCLIB has them timed.
pub fn fetch(track: &Track) -> Lyrics {
    let artist = track.artists.first().map_or("", |a| a.name.as_str());
    let album = track.album.as_ref().map_or("", |a| a.name.as_str());
    let secs = track.duration_ms / 1000;
    // The exact match first: title, artist, album and length.
    let exact = format!(
        "{API}/get?track_name={}&artist_name={}&album_name={}&duration={secs}",
        encode(&track.name),
        encode(artist),
        encode(album)
    );
    if let Some(found) = get(&exact).as_ref().and_then(read) {
        return found;
    }
    // Then a search, preferring a match of about the same length.
    let search = format!(
        "{API}/search?track_name={}&artist_name={}",
        encode(&track.name),
        encode(artist)
    );
    let Some(Value::Array(results)) = get(&search) else {
        return Lyrics::Missing;
    };
    let close = |r: &&Value| {
        r["duration"]
            .as_f64()
            .is_some_and(|d| (d - secs as f64).abs() <= 3.0)
    };
    results
        .iter()
        .filter(close)
        .chain(results.iter())
        .find_map(read)
        .unwrap_or(Lyrics::Missing)
}

/// Lyrics out of one LRCLIB record: timed if it has them, plain otherwise.
fn read(record: &Value) -> Option<Lyrics> {
    if record["instrumental"] == true {
        return None;
    }
    if let Some(text) = record["syncedLyrics"].as_str()
        && let Ok(lines) = parse_lyrics(text, true)
    {
        // Timed blank lines mark instrumental breaks; keep them as rests.
        return Some(Lyrics::Found {
            lines,
            synced: true,
        });
    }
    let text = record["plainLyrics"].as_str()?;
    let lines = parse_lyrics(text, false).ok()?;
    Some(Lyrics::Found {
        lines,
        synced: false,
    })
}

fn get(url: &str) -> Option<Value> {
    let mut easy = curl::easy::Easy::new();
    let mut bytes = Vec::new();
    easy.url(url).ok()?;
    easy.useragent(USER_AGENT).ok()?;
    easy.connect_timeout(Duration::from_secs(5)).ok()?;
    easy.timeout(Duration::from_secs(10)).ok()?;
    {
        let mut transfer = easy.transfer();
        transfer
            .write_function(|data| {
                if bytes.len() + data.len() > MAX_REPLY {
                    return Ok(0);
                }
                bytes.extend_from_slice(data);
                Ok(data.len())
            })
            .ok()?;
        transfer.perform().ok()?;
    }
    if easy.response_code().ok()? != 200 {
        return None;
    }
    serde_json::from_slice(&bytes).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn timed_lyrics_win_over_plain_ones() {
        let record = json!({
            "syncedLyrics": "[00:01.00] First\n[00:04.50] Second",
            "plainLyrics": "First\nSecond",
        });
        let Some(Lyrics::Found { lines, synced }) = read(&record) else {
            panic!("lyrics expected");
        };
        assert!(synced);
        assert_eq!(lines[1].millis, Some(4500));
        assert_eq!(lines[1].text.trim(), "Second");
    }

    #[test]
    fn plain_lyrics_and_instrumentals() {
        let plain = json!({ "syncedLyrics": null, "plainLyrics": "Only words" });
        assert!(matches!(
            read(&plain),
            Some(Lyrics::Found { synced: false, .. })
        ));
        assert_eq!(read(&json!({ "instrumental": true })), None);
    }
}
