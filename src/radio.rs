//! Radio station model, view modes and catalog state.
//!
//! The catalog is fetched off-thread from
//! [Radio Browser](https://www.radio-browser.info/) through the shared
//! [`enzui_resources::Transport`] seam
//! ([`crate::podcasts::default_transport`]) under
//! [`enzui_core::url_policy::UrlPolicy`]; `crate::radio_catalog` parses
//! and validates it. Boot dispatches [`crate::Message::RadioCatalogLoad`] once
//! audio has spawned; tests seed a catalog with `seed_test_catalog`.
//!
//! `Message::RadioTune` plays through
//! [`crate::audio::Playback::load_remote_and_autoplay`], so the framework URL
//! and remote-gate policies apply. Only confirmed backend state claims
//! playback; a failure keeps the station selected but not playing and shows
//! one retryable Warning.
//!
//! [`RadioStation::remote_uuid`] is the durable identity and the key for
//! persisted favourites. The u64 `id` used by messages and widgets is derived
//! from it by [`derive_local_id_from_uuid`], so it is stable across restarts.

use std::collections::BTreeSet;

// Data model

/// One internet-radio station.
///
/// `id` is the in-process key derived from `remote_uuid` (Radio Browser
/// `stationuuid`), which is the durable identity. Stations without a
/// `stream_url` are dropped by the parser. `homepage` and `favicon_url` may be
/// empty, `bitrate` is 0 when unknown, and zero `last_check_*` values mean
/// the catalog carried no liveness data; `last_check_ok == false` shows a
/// "recently unreachable" hint.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RadioStation {
    pub id: u64,
    pub remote_uuid: String,
    pub name: String,
    pub description: String,
    pub tags: String,
    pub stream_url: String,
    pub homepage: String,
    pub favicon_url: String,
    pub country: String,
    pub codec: String,
    pub bitrate: u32,
    pub votes: i64,
    pub last_check_ok: bool,
    pub last_check_time_unix: u64,
}

impl RadioStation {
    /// Case-insensitive substring match over name, description, tags, country
    /// and codec.
    #[must_use]
    pub fn matches(&self, query: &str) -> bool {
        let q = query.trim().to_lowercase();
        if q.is_empty() {
            return false;
        }
        self.name.to_lowercase().contains(&q)
            || self.description.to_lowercase().contains(&q)
            || self.tags.to_lowercase().contains(&q)
            || self.country.to_lowercase().contains(&q)
            || self.codec.to_lowercase().contains(&q)
    }
}

/// Derive the in-process u64 `id` from the durable `remote_uuid` string.
///
/// Stable across restarts, so persisted favourites can be matched before the
/// catalog reloads. The hash is not collision-resistant; that is harmless
/// because ids are UI keys and every consumer resolves them against the
/// catalog.
#[must_use]
pub fn derive_local_id_from_uuid(remote_uuid: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    remote_uuid.hash(&mut hasher);
    // 0 is reserved as a sentinel; a hash of 0 becomes 1.
    let raw = hasher.finish();
    if raw == 0 { 1 } else { raw }
}

// Browsing

/// The genres the browse chips offer, as `(Radio Browser tag, label)`.
pub const GENRES: [(&str, &str); 12] = [
    ("pop", "Pop"),
    ("rock", "Rock"),
    ("news", "News"),
    ("talk", "Talk"),
    ("jazz", "Jazz"),
    ("classical", "Classical"),
    ("electronic", "Electronic"),
    ("dance", "Dance"),
    ("hip hop", "Hip-hop"),
    ("oldies", "Oldies"),
    ("80s", "80s"),
    ("chillout", "Chill"),
];

/// A browse list's key: the country and the genre tag (`None`: every genre).
pub type BrowseKey = (String, Option<&'static str>);

/// The prefix of a station the user added by its stream URL, which Radio
/// Browser doesn't know.
pub const CUSTOM_PREFIX: &str = "custom:";

/// A station the user added by its stream URL.
#[must_use]
pub fn custom_station(name: &str, stream_url: &str) -> RadioStation {
    let uuid = format!("{CUSTOM_PREFIX}{stream_url}");
    RadioStation {
        id: derive_local_id_from_uuid(&uuid),
        remote_uuid: uuid,
        name: if name.trim().is_empty() {
            stream_url
                .split("://")
                .nth(1)
                .and_then(|rest| rest.split(['/', ':']).next())
                .unwrap_or(stream_url)
                .to_owned()
        } else {
            name.trim().chars().take(128).collect()
        },
        description: String::new(),
        tags: "Added by you".to_owned(),
        stream_url: stream_url.to_owned(),
        homepage: String::new(),
        favicon_url: String::new(),
        country: String::new(),
        codec: String::new(),
        bitrate: 0,
        votes: 0,
        last_check_ok: true,
        last_check_time_unix: 0,
    }
}

/// The remote catalog fetch cycle, the single authority for the catalog
/// status line.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum RadioCatalogState {
    /// Not yet requested; boot switches to `Loading` before the first paint.
    #[default]
    Idle,
    /// A fetch is in flight; only its generation is accepted.
    Loading {
        /// Generation the completion must match; stale replies are dropped.
        generation: u64,
    },
    /// The last fetch succeeded.
    Loaded {
        /// Wall-clock unix seconds when the catalog arrived.
        at_unix: u64,
    },
    /// The last fetch failed.
    ///
    /// `retryable` is `false` for policy refusals and `true` for transport
    /// failures. Previously loaded stations stay visible.
    Failed { message: String, retryable: bool },
}

impl RadioCatalogState {}

// Test-only seed

#[cfg(test)]
#[must_use]
pub fn seed_test_catalog() -> Vec<RadioStation> {
    let mk = |name: &str, description: &str, tags: &str, stream_url: &str| {
        let uuid = format!("test-uuid-{name}").to_lowercase().replace(' ', "-");
        RadioStation {
            id: derive_local_id_from_uuid(&uuid),
            remote_uuid: uuid,
            name: name.to_string(),
            description: description.to_string(),
            tags: tags.to_string(),
            stream_url: stream_url.to_string(),
            homepage: String::new(),
            favicon_url: String::new(),
            country: "Fixture".to_string(),
            codec: "MP3".to_string(),
            bitrate: 128,
            votes: 0,
            last_check_ok: true,
            last_check_time_unix: 0,
        }
    };
    vec![
        mk(
            "SomaFM Groove Salad",
            "A nicely chilled plate of ambient/downtempo beats and grooves.",
            "ambient · downtempo",
            "https://fixture.invalid/groove.mp3",
        ),
        mk(
            "SomaFM Drone Zone",
            "Atmospheric, ambient space-music. Served best chilled.",
            "ambient · drone",
            "https://fixture.invalid/drone.mp3",
        ),
        mk(
            "SomaFM Secret Agent",
            "The soundtrack for your stylish, mysterious, dangerous life.",
            "lounge · jazz · spy",
            "https://fixture.invalid/agent.mp3",
        ),
        mk(
            "SomaFM Deep Space One",
            "Deep ambient electronic, experimental and space music.",
            "ambient · electronic",
            "https://fixture.invalid/deep.mp3",
        ),
        mk(
            "SomaFM Left Coast 70s",
            "Mellow gold from the California singer-songwriter era.",
            "70s · mellow",
            "https://fixture.invalid/left.mp3",
        ),
        mk(
            "SomaFM Indie Pop Rocks!",
            "New and classic indie/alternative rock — upbeat and eclectic.",
            "indie · alternative",
            "https://fixture.invalid/indie.mp3",
        ),
        mk(
            "Radio Paradise",
            "Eclectic curated mix — rock, world, electronica, blues.",
            "eclectic · curated",
            "https://fixture.invalid/paradise.mp3",
        ),
        mk(
            "NTS Radio 1",
            "Global music discovery from London.",
            "eclectic · discovery",
            "https://fixture.invalid/nts.mp3",
        ),
    ]
}

// State

/// Everything the Radio surface owns.
#[derive(Clone, Debug, Default)]
pub struct RadioState {
    /// Popular stations by country and genre, each loaded once per run.
    pub lists: std::collections::HashMap<BrowseKey, Vec<RadioStation>>,
    /// The genre chip selected (`None`: every genre).
    pub genre: Option<&'static str>,
    /// The browse list being fetched.
    pub loading: Option<BrowseKey>,
    /// The last browse fetch's outcome, for the page and the corner notice.
    pub catalog_status: RadioCatalogState,
    /// The live contents of the search field.
    pub query: String,
    /// The submitted query; `None` while not searching.
    pub submitted: Option<String>,
    /// Bumped on submit; only the matching completion lands.
    pub search_generation: u64,
    /// Search results; `None` while the search is in flight.
    pub results: Option<Vec<RadioStation>>,
    /// Starred stations by `remote_uuid`, persisted.
    pub favorites: BTreeSet<String>,
    /// The starred stations' records, fetched by UUID at start.
    pub favorite_stations: Vec<RadioStation>,
    /// Stations added by their stream URL, persisted.
    pub custom: Vec<RadioStation>,
    /// The tuned station's record: Now Playing keeps it whatever the page
    /// shows next.
    pub tuned: Option<RadioStation>,
    /// The tuned station, by UUID.
    pub current_tuned_uuid: Option<String>,
    /// The add-a-station form.
    pub add_name: String,
    pub add_url: String,
}

impl RadioState {
    /// The popular stations for `country` in the selected genre, once loaded.
    #[must_use]
    pub fn browse(&self, country: &str) -> Option<&Vec<RadioStation>> {
        self.lists.get(&(country.to_owned(), self.genre))
    }

    /// Every station record known, the tuned one and the user's first.
    fn known(&self) -> impl Iterator<Item = &RadioStation> {
        self.tuned
            .iter()
            .chain(self.custom.iter())
            .chain(self.favorite_stations.iter())
            .chain(self.results.iter().flatten())
            .chain(self.lists.values().flatten())
    }

    /// Whether this station is one of the user's (starred or added).
    #[must_use]
    pub fn is_favorite(&self, id: u64) -> bool {
        self.station(id).is_some_and(|s| {
            self.favorites.contains(&s.remote_uuid)
                || self.custom.iter().any(|c| c.remote_uuid == s.remote_uuid)
        })
    }

    /// The station with this durable UUID, from anything loaded (the same
    /// station may sit in several lists; they're one station).
    #[must_use]
    pub fn by_uuid(&self, uuid: &str) -> Option<&RadioStation> {
        self.known().find(|s| s.remote_uuid == uuid)
    }

    /// The station for `id`, from anything loaded.
    #[must_use]
    pub fn station(&self, id: u64) -> Option<&RadioStation> {
        self.known().find(|s| s.id == id)
    }

    /// Tune a station, returning it when the id resolves.
    pub fn tune(&mut self, id: u64) -> Option<&RadioStation> {
        let station = self.station(id)?.clone();
        self.current_tuned_uuid = Some(station.remote_uuid.clone());
        self.tuned = Some(station);
        self.tuned.as_ref()
    }

    /// Star or unstar a station (an added station is removed), returning
    /// whether it is now the user's.
    pub fn toggle_favorite(&mut self, id: u64) -> bool {
        let Some(station) = self.station(id).cloned() else {
            return false;
        };
        if station.remote_uuid.starts_with(CUSTOM_PREFIX) {
            self.custom.retain(|c| c.remote_uuid != station.remote_uuid);
            return false;
        }
        if self.favorites.remove(&station.remote_uuid) {
            self.favorite_stations
                .retain(|s| s.remote_uuid != station.remote_uuid);
            false
        } else {
            self.favorites.insert(station.remote_uuid.clone());
            self.favorite_stations.push(station);
            true
        }
    }

    /// The user's stations: added ones, then starred ones in starring order.
    #[must_use]
    pub fn yours(&self) -> Vec<&RadioStation> {
        self.custom
            .iter()
            .chain(
                self.favorite_stations
                    .iter()
                    .filter(|s| self.favorites.contains(&s.remote_uuid)),
            )
            .collect()
    }

    /// Starred stations whose records aren't loaded yet.
    #[must_use]
    pub fn unresolved_favorites(&self) -> Vec<String> {
        self.favorites
            .iter()
            .filter(|uuid| {
                !self
                    .favorite_stations
                    .iter()
                    .any(|s| &s.remote_uuid == *uuid)
            })
            .cloned()
            .collect()
    }

    /// Stations on screen, for artwork: the tuned one, the user's, the
    /// results and the list on show.
    #[must_use]
    pub fn visible(&self, country: &str) -> Vec<&RadioStation> {
        let mut out: Vec<&RadioStation> = self.tuned.iter().collect();
        out.extend(self.yours());
        out.extend(self.results.iter().flatten());
        out.extend(self.browse(country).into_iter().flatten());
        out
    }

    /// Apply a submitted search and return its generation for the request.
    pub fn submit_search(&mut self) -> u64 {
        let q = self.query.trim();
        if q.is_empty() {
            self.submitted = None;
            self.results = None;
        } else {
            self.submitted = Some(q.to_string());
            self.results = None;
        }
        self.search_generation = self.search_generation.wrapping_add(1);
        self.search_generation
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_starred_station_stays_whatever_list_loads_next() {
        let seed = seed_test_catalog();
        let mut st = RadioState::default();
        st.lists.insert(("hu".into(), None), seed.clone());
        let id = seed[2].id;
        assert!(st.toggle_favorite(id));
        // Another list replaces the one it came from.
        st.lists.clear();
        assert!(st.is_favorite(id), "kept with its own record");
        assert_eq!(st.yours().len(), 1);
        assert!(!st.toggle_favorite(id));
        assert!(st.yours().is_empty());
    }

    #[test]
    fn the_tuned_station_stays_known_for_the_player() {
        let seed = seed_test_catalog();
        let mut st = RadioState {
            results: Some(seed.clone()),
            ..RadioState::default()
        };
        let id = seed[1].id;
        assert!(st.tune(id).is_some());
        st.results = None;
        assert_eq!(
            st.station(id).map(|s| s.name.as_str()),
            Some(seed[1].name.as_str())
        );
    }

    #[test]
    fn an_added_station_is_the_users_and_removing_it_deletes_it() {
        let mut st = RadioState::default();
        st.custom.push(custom_station(
            "",
            "https://stream.example.com:8443/live.mp3",
        ));
        let id = st.custom[0].id;
        assert_eq!(st.custom[0].name, "stream.example.com");
        assert!(st.is_favorite(id));
        assert!(!st.toggle_favorite(id));
        assert!(st.custom.is_empty());
    }

    #[test]
    fn matching_covers_every_visible_and_searchable_field() {
        let seed = seed_test_catalog();
        let s = &seed[0];
        assert!(s.matches("GROOVE"), "name case-insensitive");
        assert!(s.matches("downtempo"), "tags");
        assert!(s.matches("chilled plate"), "description");
        assert!(s.matches("fixture"), "country");
        assert!(s.matches("mp3"), "codec");
        assert!(!s.matches("   "), "blank query matches nothing");
        assert!(!s.matches("polka"));
    }
}
