//! Library core: tracks, stats, and play credits.

use super::playlists::PlaylistIdAllocator;
use super::tracklist::{AvailabilityAuthority, TrackAvailability};
use super::*;

#[derive(Clone, Debug)]
pub(crate) struct TrackSortKeys {
    pub(crate) title: String,
    pub(crate) artist: String,
    pub(crate) album: String,
}

/// The final, total tie-break for track orderings: case-folded title, exact
/// title, path, then id. Borrowed comparisons only, so sorts stay
/// allocation-free; the result never depends on insertion or hash order.
pub(crate) fn track_tie_break(
    keys: &HashMap<u64, TrackSortKeys>,
    (a_id, a): (u64, &Track),
    (b_id, b): (u64, &Track),
) -> std::cmp::Ordering {
    keys.get(&a_id)
        .map(|key| key.title.as_str())
        .cmp(&keys.get(&b_id).map(|key| key.title.as_str()))
        .then_with(|| a.title.cmp(&b.title))
        .then_with(|| a.path.cmp(&b.path))
        .then_with(|| a_id.cmp(&b_id))
}

impl TrackSortKeys {
    pub(crate) fn from_track(track: &Track) -> Self {
        Self {
            title: track.title.to_lowercase(),
            artist: track.artist.to_lowercase(),
            album: track.album.to_lowercase(),
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Library {
    pub(crate) next_id: u64,
    pub(crate) playlist_ids: PlaylistIdAllocator,
    /// Monotonic ordering counter backing `Track::last_played_seq`. Increment
    /// then assign, so `0` stays the never-played sentinel.
    pub(crate) next_play_seq: u64,
    /// Changes whenever derived library indexes are rebuilt, so view
    /// projections refresh exactly once after a scan.
    pub(crate) revision: u64,
    /// Changes whenever a track enters the library, so the set of
    /// `(path, added_at)` pairs may differ.
    ///
    /// Distinct from [`Self::revision`]: `add_track` changes the first-seen
    /// table without reindexing, and a bare reindex leaves it unchanged.
    /// Persistence keys its shared `track_added_at` on this counter, so every
    /// insertion into `tracks` must bump it.
    pub(crate) track_identity_revision: u64,
    // Persistence caches the played-track and user-rating union under this revision.
    pub(crate) track_stats_revision: u64,
    pub tracks: HashMap<u64, Track>,
    pub(crate) user_ratings: std::collections::BTreeMap<PathBuf, u8>,
    pub(crate) user_rating_bytes: usize,
    /// Filesystem path to stable track id, so a rescan resolves an existing
    /// row in one probe. Rebuilt wholesale by [`Self::reindex`].
    pub(crate) path_index: HashMap<PathBuf, u64>,
    /// Filesystem path to the ids of playlists whose saved track list names
    /// that path, so a new track visits only those playlists.
    pub(crate) playlist_index: HashMap<PathBuf, Vec<u64>>,
    /// Playlist id to its position in [`Self::playlists`]. Maintained wherever
    /// the playlist set or order changes: restore, create, and delete.
    pub(crate) playlist_positions: HashMap<u64, usize>,
    /// Case-folded sort keys computed when track metadata enters the model,
    /// avoiding per-comparison allocation in view sorts.
    pub(crate) sort_keys: HashMap<u64, TrackSortKeys>,
    pub track_order: Vec<u64>,
    pub albums: Vec<Album>,
    pub(crate) album_positions: HashMap<u64, usize>,
    pub artists: Vec<Artist>,
    pub(crate) artist_positions: HashMap<u64, usize>,
    pub genres: Vec<Genre>,
    /// Tag groupings derived from `Track::genre` via [`split_tags`].
    pub tags: Vec<Tag>,
    pub folders: Vec<PathBuf>,
    pub playlists: Vec<Playlist>,
    /// Per-path playback availability, the single runtime authority for
    /// whether a row's source is `Present`, `Missing`, `Undecodable`,
    /// `Unknown` or `Unverified`. Never persisted, so a restored row is
    /// `Unknown` until a bounded scan verifies it.
    pub(crate) availability: AvailabilityAuthority,
}

impl Library {
    pub fn new() -> Self {
        Self::default()
    }

    /// The availability of the track named by `path`, defaulting to
    /// [`TrackAvailability::Unknown`] when no scan has decided it.
    #[must_use]
    pub fn availability_of_path(&self, path: &std::path::Path) -> TrackAvailability {
        self.availability.state(path)
    }

    /// The availability of the track with `id`, or
    /// [`TrackAvailability::Unknown`] when the id is not live or undecided.
    #[must_use]
    pub fn availability_of(&self, id: u64) -> TrackAvailability {
        self.tracks
            .get(&id)
            .map(|t| self.availability.state(&t.path))
            .unwrap_or_default()
    }

    /// Whether the track with `id` has been proven [`TrackAvailability::Present`]
    /// by an authoritative scan.
    #[must_use]
    pub fn is_present(&self, id: u64) -> bool {
        self.availability_of(id).is_present()
    }

    /// Whether the track with `id` may be played or advanced to: true only
    /// when a completed authoritative scan proved it `Present`.
    ///
    /// Every playback skip, refusal and bulk-seed decision uses this, so a
    /// removed or unmounted file is never presented as healthy.
    #[must_use]
    pub fn is_playable_now(&self, id: u64) -> bool {
        self.availability_of(id).is_present()
    }

    /// Record an availability decision for `path` under a scan `generation`,
    /// rejecting a stale (older-generation) decision. Returns whether the
    /// record changed.
    pub fn decide_availability(
        &mut self,
        path: PathBuf,
        state: TrackAvailability,
        generation: u64,
    ) -> bool {
        self.availability.decide(path, state, generation)
    }

    /// Add a scanned track, assigning it a fresh id. Returns the id.
    pub fn add_track(&mut self, mut track: Track) -> u64 {
        // The only production entry point for a new `(path, added_at)` pair;
        // `merge_scanned_track` delegates here for unseen paths.
        self.track_identity_revision = self.track_identity_revision.wrapping_add(1);
        // Scanned tracks carry zero play stats, so a scan leaves the stats
        // revision and its shared-Arc cache untouched; fixtures with preset
        // counts bump it.
        let bump_stats = track.play_count > 0 || track.last_played_unix > 0;
        let id = self.next_id;
        self.next_id += 1;
        track.id = id;
        let path = track.path.clone();
        let keys = TrackSortKeys::from_track(&track);
        self.track_order.push(id);
        self.tracks.insert(id, track);
        self.sort_keys.insert(id, keys);
        self.path_index.insert(path.clone(), id);
        if bump_stats {
            self.track_stats_revision = self.track_stats_revision.saturating_add(1);
        }
        self.resolve_playlist_membership(&path, id);
        id
    }

    /// Append `id` to every playlist whose saved paths name `path`, skipping
    /// playlists that already list it.
    pub(crate) fn resolve_playlist_membership(&mut self, path: &std::path::Path, id: u64) {
        let Some(playlist_ids) = self.playlist_index.get(path) else {
            return;
        };
        let targets = playlist_ids.clone();
        for pid in targets {
            if let Some(pos) = self.playlist_slot(pid)
                && let Some(playlist) = self.playlists.get_mut(pos)
                && !playlist.track_ids.contains(&id)
            {
                playlist.track_ids.push(id);
            }
        }
    }

    /// Position of the playlist named by `pid`, or `None` when no playlist
    /// carries it. One probe of `playlist_positions`.
    pub(crate) fn playlist_slot(&self, pid: u64) -> Option<usize> {
        self.playlist_positions.get(&pid).copied()
    }

    /// Rebuild `playlist_positions` from the current playlist order.
    pub(crate) fn rebuild_playlist_positions(&mut self) {
        self.playlist_positions = self
            .playlists
            .iter()
            .enumerate()
            .map(|(position, playlist)| (playlist.id, position))
            .collect();
    }

    /// Merge metadata produced by a later scan without changing the stable
    /// in-session id or listener-owned counters of an existing path.
    ///
    /// The path is a track's durable identity: a rescan refreshes tags without
    /// duplicating the row or resetting `added_at`, play count, or recent-play
    /// ordering. New paths go through [`Self::add_track`].
    pub fn merge_scanned_track(&mut self, mut track: Track) -> u64 {
        let Some(id) = self.track_id_for_path(&track.path) else {
            return self.add_track(track);
        };

        if let Some(existing) = self.tracks.get(&id) {
            track.id = id;
            track.added_at = existing.added_at;
            track.play_count = existing.play_count;
            track.last_played_seq = existing.last_played_seq;
            track.last_played_unix = existing.last_played_unix;
        }
        let keys = TrackSortKeys::from_track(&track);
        self.tracks.insert(id, track);
        self.sort_keys.insert(id, keys);
        id
    }

    /// Resolve `path` to the id of the track that currently holds it, or
    /// `None` when no track does. One probe of `path_index`.
    pub fn track_id_for_path(&self, path: &std::path::Path) -> Option<u64> {
        self.path_index.get(path).copied()
    }

    /// Recompute the path index in insertion (`track_order`) order so that when
    /// two rows share a path the later-added row wins, as in [`Self::add_track`].
    pub(crate) fn rebuilt_path_index(&self) -> HashMap<PathBuf, u64> {
        let mut index: HashMap<PathBuf, u64> = HashMap::with_capacity(self.tracks.len());
        let mut ordered: std::collections::HashSet<u64> =
            std::collections::HashSet::with_capacity(self.track_order.len());
        for &id in &self.track_order {
            if let Some(track) = self.tracks.get(&id) {
                index.insert(track.path.clone(), id);
                ordered.insert(id);
            }
        }
        // Fold in tracks absent from `track_order` by ascending id so the
        // rebuild never depends on map iteration order.
        let mut stray: Vec<u64> = self
            .tracks
            .keys()
            .copied()
            .filter(|id| !ordered.contains(id))
            .collect();
        stray.sort_unstable();
        for id in stray {
            if let Some(track) = self.tracks.get(&id) {
                index.insert(track.path.clone(), id);
            }
        }
        index
    }

    /// The current `track_stats_revision`. Persistence's
    /// `Persist::track_stats_for` keys its shared-Arc cache on this counter;
    /// callers outside persistence should treat it as an opaque token.
    #[must_use]
    pub fn track_stats_revision(&self) -> u64 {
        self.track_stats_revision
    }

    /// Add `path` to the library folder list, returning `true` if it was new.
    /// Adding a folder already in the list is a no-op.
    pub fn add_folder(&mut self, path: std::path::PathBuf) -> bool {
        if self.folders.iter().any(|f| f == &path) {
            return false;
        }
        self.folders.push(path);
        true
    }

    /// Remove `path` from the library folder list, returning `true` if it was
    /// present. Tracks already scanned from it stay in the library.
    pub fn remove_folder(&mut self, path: &std::path::Path) -> bool {
        let before = self.folders.len();
        self.folders.retain(|f| f != path);
        self.folders.len() != before
    }

    /// Remove the track with `id`, returning `true` when it existed.
    ///
    /// The per-scan id is dropped from every playlist's `track_ids`, but each
    /// playlist's durable `track_paths` is left intact so a later scan that
    /// re-discovers the file resolves it back into place; until then the entry
    /// is a path with no live track. `reindex` must run after a batch of
    /// removals to rebuild the derived groupings.
    pub fn remove_track(&mut self, id: u64) -> bool {
        let Some(track) = self.tracks.remove(&id) else {
            return false;
        };
        self.track_order.retain(|&t| t != id);
        self.sort_keys.remove(&id);
        if self.path_index.get(&track.path) == Some(&id) {
            self.path_index.remove(&track.path);
        }
        // Forget the availability decision so a re-add starts from `Unknown`.
        self.availability.forget(&track.path);
        for playlist in &mut self.playlists {
            playlist.track_ids.retain(|&t| t != id);
        }
        self.track_identity_revision = self.track_identity_revision.wrapping_add(1);
        if track.play_count > 0 || track.last_played_unix > 0 {
            self.track_stats_revision = self.track_stats_revision.saturating_add(1);
        }
        true
    }

    /// Ids of the tracks whose path lies under `root`.
    #[must_use]
    pub fn track_ids_under(&self, root: &std::path::Path) -> Vec<u64> {
        self.tracks
            .values()
            .filter(|t| t.path.starts_with(root))
            .map(|t| t.id)
            .collect()
    }

    /// Rebuild all derived groupings (albums, artists, genres, tags) from
    /// scratch.
    pub fn reindex(&mut self) {
        self.revision = self.revision.wrapping_add(1);
        // Tests and imports may fill `tracks` directly; reindex makes every
        // derived index current.
        self.sort_keys = self
            .tracks
            .iter()
            .map(|(&id, track)| (id, TrackSortKeys::from_track(track)))
            .collect();
        self.path_index = self.rebuilt_path_index();
        self.rebuild_albums();
        self.album_positions = self
            .albums
            .iter()
            .enumerate()
            .map(|(position, album)| (album.id, position))
            .collect();
        self.rebuild_artists();
        self.artist_positions = self
            .artists
            .iter()
            .enumerate()
            .map(|(position, artist)| (artist.id, position))
            .collect();
        self.rebuild_genres();
        self.rebuild_tags();
    }

    /// Rebuild the tag index: every distinct token in a track's genre string
    /// (see [`split_tags`]) becomes a `Tag` listing its tracks. Case-insensitive,
    /// keeping the first-seen spelling.
    pub(crate) fn rebuild_tags(&mut self) {
        use std::collections::HashMap as StdMap;
        let mut by_key: StdMap<String, Tag> = StdMap::new();
        let mut order: Vec<String> = Vec::new();
        for &tid in &self.track_order {
            if let Some(t) = self.tracks.get(&tid) {
                for name in split_tags(&t.genre) {
                    let key = name.to_lowercase();
                    let entry = by_key.entry(key.clone()).or_insert_with(|| {
                        order.push(key.clone());
                        Tag {
                            id: tag_id(&key),
                            name: name.clone(),
                            track_ids: Vec::new(),
                        }
                    });
                    // Dedup: a track with `"Rock, Rock"` must be listed once.
                    if !entry.track_ids.contains(&tid) {
                        entry.track_ids.push(tid);
                    }
                }
            }
        }
        self.tags = order
            .into_iter()
            .filter_map(|k| by_key.remove(&k))
            .collect();
    }

    pub(crate) fn rebuild_albums(&mut self) {
        let mut album_map: HashMap<(String, String), Vec<u64>> = HashMap::new();
        for (&id, track) in &self.tracks {
            let key = (
                track.album.to_lowercase(),
                track
                    .album_artist
                    .to_lowercase()
                    .chars()
                    .take(50)
                    .collect::<String>(),
            );
            album_map.entry(key).or_default().push(id);
        }

        // Order the groups by their unique (album, album artist) key before
        // numbering them, so album order and ids never depend on hash order.
        let mut groups: Vec<((String, String), Vec<u64>)> = album_map.into_iter().collect();
        groups.sort_by(|(a, _), (b, _)| a.cmp(b));
        let albums: Vec<Album> = groups
            .into_iter()
            .enumerate()
            .map(|(idx, ((_, _), mut track_ids))| {
                // Disc and track number first; untagged or duplicate numbers
                // fall back to title and path, so neither the album view nor
                // its play queue inherits hash iteration order.
                track_ids.sort_by(|a, b| {
                    let ta = self.tracks.get(a);
                    let tb = self.tracks.get(b);
                    match (ta, tb) {
                        (Some(ta), Some(tb)) => ta
                            .disc_no
                            .cmp(&tb.disc_no)
                            .then(ta.track_no.cmp(&tb.track_no))
                            .then_with(|| track_tie_break(&self.sort_keys, (*a, ta), (*b, tb))),
                        _ => a.cmp(b),
                    }
                });
                let first = self.tracks.get(&track_ids[0]);
                // The first track with art wins, so an untagged opening file
                // does not hide the album's cover. Cloning shares the Arc.
                let cover = track_ids
                    .iter()
                    .find_map(|id| self.tracks.get(id).and_then(|t| t.cover.clone()));
                Album {
                    id: idx as u64,
                    title: first.map(|t| t.album.clone()).unwrap_or_default(),
                    artist: first
                        .map(|t| {
                            if t.album_artist.is_empty() {
                                t.artist.clone()
                            } else {
                                t.album_artist.clone()
                            }
                        })
                        .unwrap_or_default(),
                    year: first.map(|t| t.year).unwrap_or(0),
                    track_ids,
                    cover_path: first.and_then(|t| t.cover_path.clone()),
                    cover,
                }
            })
            .collect();

        self.albums = albums;
    }

    pub(crate) fn rebuild_artists(&mut self) {
        // Map from lowercase name → (original-case display name, track ids).
        let mut artist_map: HashMap<String, (String, Vec<u64>)> = HashMap::new();
        for (&id, track) in &self.tracks {
            let names = split_artist_names(&track.artist);
            for name in &names {
                let key = name.to_lowercase();
                let entry = artist_map
                    .entry(key)
                    .or_insert_with(|| (name.clone(), Vec::new()));
                entry.1.push(id);
            }
            // Also index album_artist.
            if !track.album_artist.is_empty() {
                let aa_names = split_artist_names(&track.album_artist);
                for name in &aa_names {
                    let key = name.to_lowercase();
                    let entry = artist_map
                        .entry(key)
                        .or_insert_with(|| (name.clone(), Vec::new()));
                    if !entry.1.contains(&id) {
                        entry.1.push(id);
                    }
                }
            }
        }

        let mut artists: Vec<Artist> = artist_map
            .into_iter()
            .enumerate()
            .map(|(idx, (_, (display_name, track_ids)))| {
                let mut album_ids: Vec<u64> = Vec::new();
                for &tid in &track_ids {
                    for album in &self.albums {
                        if album.track_ids.contains(&tid) && !album_ids.contains(&album.id) {
                            album_ids.push(album.id);
                        }
                    }
                }
                Artist {
                    id: idx as u64,
                    name: display_name,
                    album_ids,
                    track_ids,
                }
            })
            .collect();

        artists.sort_by_key(|a| a.name.to_lowercase());
        self.artists = artists;
    }

    pub(crate) fn rebuild_genres(&mut self) {
        let mut genre_map: HashMap<String, Vec<u64>> = HashMap::new();
        for (&id, track) in &self.tracks {
            if !track.genre.is_empty() {
                genre_map
                    .entry(track.genre.to_lowercase())
                    .or_default()
                    .push(id);
            }
        }

        let mut genres: Vec<Genre> = genre_map
            .into_iter()
            .enumerate()
            .map(|(idx, (_, track_ids))| {
                let mut album_ids: Vec<u64> = Vec::new();
                for &tid in &track_ids {
                    for album in &self.albums {
                        if album.track_ids.contains(&tid) && !album_ids.contains(&album.id) {
                            album_ids.push(album.id);
                        }
                    }
                }
                let display_name = self
                    .tracks
                    .get(&track_ids[0])
                    .map(|t| t.genre.clone())
                    .unwrap_or_default();
                Genre {
                    id: idx as u64,
                    name: display_name,
                    track_ids,
                    album_ids,
                }
            })
            .collect();

        genres.sort_by_key(|a| a.name.to_lowercase());
        self.genres = genres;
    }

    /// Credit an authoritative playback completion to `id`, stamping the
    /// persisted [`Track::last_played_unix`] and the in-session
    /// [`Track::last_played_seq`] tiebreak.
    ///
    /// The only path that increments `play_count`. Callers credit a play only
    /// on a backend completion crossing EOS or the scrobble threshold, never on
    /// a button press or queue jump. `unix_now` is the wall-clock second.
    pub fn credit_play(&mut self, id: u64, unix_now: u64) {
        // Tiebreak for two completions within one wall-clock second.
        self.next_play_seq = self.next_play_seq.saturating_add(1);
        let seq = self.next_play_seq;
        if let Some(t) = self.tracks.get_mut(&id) {
            t.play_count = t.play_count.saturating_add(1);
            t.last_played_seq = seq;
            t.last_played_unix = unix_now;
            // Invalidate `Persist::track_stats_for`'s shared-Arc cache.
            self.track_stats_revision = self.track_stats_revision.saturating_add(1);
        }
    }

    /// Set play stats for `id` from a persisted record without incrementing
    /// anything, as boot restore does in
    /// [`crate::MusicApp::finalize_pending_track_stats`]. Counting a play goes
    /// through [`Self::credit_play`].
    pub fn set_play_stats(&mut self, id: u64, play_count: u32, last_played_unix: u64) {
        if let Some(t) = self.tracks.get_mut(&id) {
            // Matching values leave the revision alone, so a settled resume
            // keeps the shared-Arc cache warm and causes no rewrite.
            if t.play_count != play_count || t.last_played_unix != last_played_unix {
                t.play_count = play_count;
                t.last_played_unix = last_played_unix;
                self.track_stats_revision = self.track_stats_revision.saturating_add(1);
            }
        }
    }
}

/// A tag's id from its lowercased name: stable across rebuilds and runs.
pub(crate) fn tag_id(key: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    key.hash(&mut hasher);
    hasher.finish()
}
