//! Apple Podcasts top charts: what to browse before searching.
//!
//! Apple's directory is open: the per-country chart feed and the lookup
//! endpoint need no account, key or cookie. A chart lists Apple ids; a
//! lookup turns them into each show's own RSS feed, so subscribing goes to
//! the podcaster's feed, not to Apple.

use std::time::Duration;

use serde_json::Value;

use super::search::PodcastSearchResult;

/// How many shows a chart lists.
const CHART_SIZE: usize = 50;
const MAX_REPLY: usize = 2 * 1024 * 1024;

/// Apple's podcast genres the Discover chips offer, as `(genre id, label)`.
pub const GENRES: [(u32, &str); 14] = [
    (1489, "News"),
    (1303, "Comedy"),
    (1324, "Society & Culture"),
    (1488, "True Crime"),
    (1310, "Music"),
    (1318, "Technology"),
    (1321, "Business"),
    (1545, "Sports"),
    (1512, "Health & Fitness"),
    (1304, "Education"),
    (1487, "History"),
    (1533, "Science"),
    (1309, "TV & Film"),
    (1305, "Kids & Family"),
];

/// Countries with an Apple Podcasts store, as `(code, name)`, by name.
pub const COUNTRIES: [(&str, &str); 30] = [
    ("au", "Australia"),
    ("at", "Austria"),
    ("be", "Belgium"),
    ("br", "Brazil"),
    ("ca", "Canada"),
    ("cz", "Czechia"),
    ("dk", "Denmark"),
    ("fi", "Finland"),
    ("fr", "France"),
    ("de", "Germany"),
    ("hu", "Hungary"),
    ("in", "India"),
    ("ie", "Ireland"),
    ("it", "Italy"),
    ("jp", "Japan"),
    ("mx", "Mexico"),
    ("nl", "the Netherlands"),
    ("nz", "New Zealand"),
    ("no", "Norway"),
    ("pl", "Poland"),
    ("pt", "Portugal"),
    ("ro", "Romania"),
    ("sk", "Slovakia"),
    ("za", "South Africa"),
    ("es", "Spain"),
    ("se", "Sweden"),
    ("ch", "Switzerland"),
    ("tr", "Turkey"),
    ("gb", "the United Kingdom"),
    ("us", "the United States"),
];

/// The store country to start with: where the desktop's time zone is
/// (a better guess than the language, often left as US English), then the
/// locale's region, then `us`.
#[must_use]
pub fn country() -> String {
    timezone_country()
        .or_else(|| {
            ["LC_ALL", "LC_MESSAGES", "LANG"]
                .iter()
                .filter_map(|name| std::env::var(name).ok())
                .find_map(|value| country_in(&value))
        })
        .unwrap_or_else(|| "us".to_owned())
}

/// The country of the desktop's time zone, from the system zone table.
fn timezone_country() -> Option<String> {
    let zone = std::env::var("TZ")
        .ok()
        .map(|tz| tz.trim_start_matches(':').to_owned())
        .filter(|tz| tz.contains('/'))
        .or_else(|| {
            let target = std::fs::read_link("/etc/localtime").ok()?;
            let target = target.to_string_lossy();
            Some(target.split("zoneinfo/").nth(1)?.to_owned())
        })?;
    let table = std::fs::read_to_string("/usr/share/zoneinfo/zone.tab").ok()?;
    zone_country(&table, &zone)
}

fn zone_country(table: &str, zone: &str) -> Option<String> {
    table
        .lines()
        .filter(|line| !line.starts_with('#'))
        .find_map(|line| {
            let mut fields = line.split('\t');
            let code = fields.next()?;
            let _coordinates = fields.next()?;
            (fields.next()? == zone && code.len() == 2).then(|| code.to_ascii_lowercase())
        })
}

fn country_in(locale: &str) -> Option<String> {
    let region = locale.split(['.', '@']).next()?.split('_').nth(1)?;
    (region.len() == 2 && region.bytes().all(|b| b.is_ascii_alphabetic()))
        .then(|| region.to_ascii_lowercase())
}

/// Whether `code` looks like a store country code.
#[must_use]
pub fn valid_country(code: &str) -> bool {
    code.len() == 2 && code.bytes().all(|b| b.is_ascii_lowercase())
}

/// The country's name for the section heading ("Top podcasts in Hungary").
#[must_use]
pub fn country_name(code: &str) -> Option<&'static str> {
    COUNTRIES
        .iter()
        .find(|(c, _)| *c == code)
        .map(|(_, name)| *name)
}

/// The country's chart, overall or for one genre, in chart order.
///
/// # Errors
/// A sentence for the user when Apple can't be reached.
pub fn fetch(country: &str, genre: Option<u32>) -> Result<Vec<PodcastSearchResult>, String> {
    let genre_part = genre.map(|g| format!("/genre={g}")).unwrap_or_default();
    let chart = get(&format!(
        "https://itunes.apple.com/{country}/rss/toppodcasts/limit={CHART_SIZE}{genre_part}/json"
    ))
    .ok_or("Apple's podcast charts couldn't be reached. Check your connection.")?;
    let ids = chart_ids(&chart);
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let lookup = get(&format!(
        "https://itunes.apple.com/lookup?entity=podcast&country={country}&id={}",
        ids.join(",")
    ))
    .ok_or("Apple's podcast directory couldn't be reached. Check your connection.")?;
    Ok(ordered(&ids, &lookup))
}

/// The Apple ids in a chart feed, in order. A one-entry chart is an object,
/// not a list.
fn chart_ids(chart: &Value) -> Vec<String> {
    let entries = match &chart["feed"]["entry"] {
        Value::Array(list) => list.iter().collect(),
        entry @ Value::Object(_) => vec![entry],
        _ => Vec::new(),
    };
    entries
        .into_iter()
        .filter_map(|e| e["id"]["attributes"]["im:id"].as_str())
        .filter(|id| !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit()))
        .map(str::to_owned)
        .collect()
}

/// Lookup results with an https RSS feed, in the chart's order.
fn ordered(ids: &[String], lookup: &Value) -> Vec<PodcastSearchResult> {
    let results = lookup["results"].as_array().cloned().unwrap_or_default();
    ids.iter()
        .filter_map(|id| {
            let show = results.iter().find(|r| {
                r["collectionId"].as_u64().map(|c| c.to_string()).as_deref() == Some(id)
            })?;
            let feed_url = show["feedUrl"]
                .as_str()
                .filter(|u| u.starts_with("https://") || u.starts_with("http://"))?;
            let text = |key: &str, max: usize| -> String {
                show[key]
                    .as_str()
                    .unwrap_or_default()
                    .chars()
                    .take(max)
                    .collect()
            };
            Some(PodcastSearchResult {
                title: text("collectionName", 256),
                author: text("artistName", 256),
                feed_url: feed_url.to_owned(),
                artwork_url: show["artworkUrl600"]
                    .as_str()
                    .or_else(|| show["artworkUrl100"].as_str())
                    .filter(|u| u.starts_with("https://"))
                    .map(str::to_owned),
                description: text("primaryGenreName", 64),
            })
        })
        .collect()
}

fn get(url: &str) -> Option<Value> {
    let mut easy = curl::easy::Easy::new();
    let mut bytes = Vec::new();
    easy.url(url).ok()?;
    easy.useragent("Music (enzUI desktop player)").ok()?;
    easy.connect_timeout(Duration::from_secs(5)).ok()?;
    easy.timeout(Duration::from_secs(15)).ok()?;
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
    (easy.response_code().ok()? == 200)
        .then(|| serde_json::from_slice(&bytes).ok())
        .flatten()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_country_comes_from_the_locale() {
        assert_eq!(country_in("hu_HU.UTF-8").as_deref(), Some("hu"));
        assert_eq!(country_in("en_GB@euro").as_deref(), Some("gb"));
        assert_eq!(country_in("C"), None);
        assert_eq!(country_in("POSIX"), None);
    }

    #[test]
    fn a_chart_keeps_its_order_and_drops_shows_without_a_feed() {
        let chart = json!({"feed": {"entry": [
            {"id": {"attributes": {"im:id": "2"}}},
            {"id": {"attributes": {"im:id": "1"}}},
            {"id": {"attributes": {"im:id": "3"}}},
        ]}});
        let ids = chart_ids(&chart);
        assert_eq!(ids, ["2", "1", "3"]);
        let lookup = json!({"results": [
            {"collectionId": 1, "collectionName": "One", "artistName": "A", "feedUrl": "https://one.example/rss", "artworkUrl600": "https://img/1"},
            {"collectionId": 2, "collectionName": "Two", "artistName": "B", "feedUrl": "https://two.example/rss"},
            {"collectionId": 3, "collectionName": "No feed"},
        ]});
        let shows = ordered(&ids, &lookup);
        let titles: Vec<&str> = shows.iter().map(|s| s.title.as_str()).collect();
        assert_eq!(titles, ["Two", "One"]);
        assert_eq!(shows[1].artwork_url.as_deref(), Some("https://img/1"));
    }

    #[test]
    fn the_time_zone_names_the_country() {
        let table = "# comment\nHU\t+4730+01905\tEurope/Budapest\nIE\t+5320-00615\tEurope/Dublin\n";
        assert_eq!(zone_country(table, "Europe/Dublin").as_deref(), Some("ie"));
        assert_eq!(
            zone_country(table, "Europe/Budapest").as_deref(),
            Some("hu")
        );
        assert_eq!(zone_country(table, "Mars/Base"), None);
    }

    #[test]
    fn a_one_show_chart_is_an_object() {
        let chart = json!({"feed": {"entry": {"id": {"attributes": {"im:id": "9"}}}}});
        assert_eq!(chart_ids(&chart), ["9"]);
    }
}
