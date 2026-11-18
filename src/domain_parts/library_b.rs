//! Library queries: recent plays and search.

use super::*;

impl Library {
    /// The most recently played track ids under a filter, newest first, capped
    /// at `limit`. Never-played tracks are excluded.
    pub fn recently_played_ids(&self, filter: LibraryFilter, limit: usize) -> Vec<u64> {
        let mut ids: Vec<u64> = self
            .ids_for_filter(filter)
            .into_iter()
            .filter(|id| {
                // A play counts once either the persisted `last_played_unix` or
                // the in-session `last_played_seq` is set.
                self.tracks
                    .get(id)
                    .map(|t| t.last_played_unix > 0 || t.last_played_seq > 0)
                    .unwrap_or(false)
            })
            .collect();
        ids.sort_by(|a, b| {
            let track_a = self.tracks.get(a);
            let track_b = self.tracks.get(b);
            let ua = track_a.map(|t| t.last_played_unix).unwrap_or(0);
            let ub = track_b.map(|t| t.last_played_unix).unwrap_or(0);
            let sa = track_a.map(|t| t.last_played_seq).unwrap_or(0);
            let sb = track_b.map(|t| t.last_played_seq).unwrap_or(0);
            // Wall-clock second first so order survives a restart; the session
            // seq breaks same-second ties, then title.
            ub.cmp(&ua).then_with(|| sb.cmp(&sa)).then_with(|| {
                let ta = self.sort_keys.get(a).map(|keys| keys.title.as_str());
                let tb = self.sort_keys.get(b).map(|keys| keys.title.as_str());
                ta.cmp(&tb)
            })
        });
        ids.truncate(limit);
        ids
    }

    pub fn rating(&self, id: u64) -> u8 {
        self.tracks
            .get(&id)
            .map(|track| self.rating_for_path(&track.path))
            .unwrap_or(0)
    }

    pub fn rating_for_path(&self, path: &std::path::Path) -> u8 {
        self.user_ratings.get(path).copied().unwrap_or(0)
    }

    pub fn ratings(&self) -> impl Iterator<Item = (&PathBuf, &u8)> {
        self.user_ratings.iter()
    }

    pub fn restore_ratings(&mut self, records: impl Iterator<Item = (PathBuf, u8)>) {
        let ratings: std::collections::BTreeMap<PathBuf, u8> = records
            .filter(|(path, rating)| {
                (1..=5).contains(rating) && path.as_os_str().len() <= MAX_PERSISTED_PATH_BYTES
            })
            .take(MAX_PERSISTED_TRACKS)
            .collect();
        if self.user_ratings != ratings {
            self.user_rating_bytes = ratings
                .keys()
                .map(|path: &PathBuf| path.as_os_str().len())
                .sum();
            self.user_ratings = ratings;
            self.track_stats_revision = self.track_stats_revision.saturating_add(1);
        }
    }

    pub fn set_rating(&mut self, path: PathBuf, rating: u8) -> bool {
        if rating > 5
            || path.as_os_str().len() > MAX_PERSISTED_PATH_BYTES
            || self.rating_for_path(&path) == rating
            || (rating > 0
                && !self.user_ratings.contains_key(&path)
                && (self.user_ratings.len() >= MAX_PERSISTED_TRACKS
                    || self
                        .user_rating_bytes
                        .saturating_add(path.as_os_str().len())
                        > MAX_AGGREGATE_STRING_BYTES))
        {
            return false;
        }
        if rating == 0 {
            self.user_rating_bytes -= path.as_os_str().len();
            self.user_ratings.remove(&path);
        } else {
            if !self.user_ratings.contains_key(&path) {
                self.user_rating_bytes += path.as_os_str().len();
            }
            self.user_ratings.insert(path, rating);
        }
        self.track_stats_revision = self.track_stats_revision.saturating_add(1);
        true
    }

    pub fn track_count(&self) -> usize {
        self.tracks.len()
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// The counter persistence compares instead of walking every track; see
    /// the `track_identity_revision` field.
    pub fn track_identity_revision(&self) -> u64 {
        self.track_identity_revision
    }

    pub fn album(&self, id: u64) -> Option<&Album> {
        self.album_positions
            .get(&id)
            .and_then(|&position| self.albums.get(position))
    }

    pub fn artist(&self, id: u64) -> Option<&Artist> {
        self.artist_positions
            .get(&id)
            .and_then(|&position| self.artists.get(position))
    }

    pub fn total_duration(&self) -> f64 {
        self.tracks.values().map(|t| t.duration_secs).sum()
    }

    fn ids_for_filter(&self, filter: LibraryFilter) -> Vec<u64> {
        match filter {
            LibraryFilter::Rated => self
                .track_order
                .iter()
                .copied()
                .filter(|id| self.rating(*id) > 0)
                .collect(),
            LibraryFilter::None => self.track_order.clone(),
            LibraryFilter::Artist(aid) => self
                .artist(aid)
                .map(|a| a.track_ids.clone())
                .unwrap_or_default(),
            LibraryFilter::Album(aid) => self
                .album(aid)
                .map(|a| a.track_ids.clone())
                .unwrap_or_default(),
            LibraryFilter::Genre(gid) => self
                .genres
                .iter()
                .find(|g| g.id == gid)
                .map(|g| g.track_ids.clone())
                .unwrap_or_default(),
            LibraryFilter::Tag(tid) => self
                .tags
                .iter()
                .find(|t| t.id == tid)
                .map(|t| t.track_ids.clone())
                .unwrap_or_default(),
            LibraryFilter::Playlist(pid) => self
                .playlists
                .iter()
                .find(|p| p.id == pid)
                .map(|p| p.track_ids.clone())
                .unwrap_or_default(),
        }
    }

    /// Track ids in `sort` order, restricted by `filter`; the single source of
    /// the master track list ordering.
    pub fn ordered_tracks(&self, sort: SortMode, filter: LibraryFilter) -> Vec<u64> {
        let mut ids = self.ids_for_filter(filter);
        let tracks = &self.tracks;
        let keys = &self.sort_keys;
        // Every mode ends in the same title/path tie-break, so equal primary
        // keys never fall back to insertion or hash iteration order.
        let tie =
            |a: &u64, b: &u64, ta: &Track, tb: &Track| track_tie_break(keys, (*a, ta), (*b, tb));
        match sort {
            SortMode::Rating => ids.sort_by(|a, b| {
                self.rating(*b).cmp(&self.rating(*a)).then_with(|| {
                    match (tracks.get(a), tracks.get(b)) {
                        (Some(ta), Some(tb)) => tie(a, b, ta, tb),
                        _ => a.cmp(b),
                    }
                })
            }),
            SortMode::Album => ids.sort_by(|a, b| match (tracks.get(a), tracks.get(b)) {
                (Some(ta), Some(tb)) => keys
                    .get(a)
                    .map(|key| key.album.as_str())
                    .cmp(&keys.get(b).map(|key| key.album.as_str()))
                    .then(ta.disc_no.cmp(&tb.disc_no))
                    .then(ta.track_no.cmp(&tb.track_no))
                    .then_with(|| tie(a, b, ta, tb)),
                _ => a.cmp(b),
            }),
            SortMode::Artist => ids.sort_by(|a, b| match (tracks.get(a), tracks.get(b)) {
                (Some(ta), Some(tb)) => keys
                    .get(a)
                    .map(|key| (key.artist.as_str(), key.album.as_str()))
                    .cmp(
                        &keys
                            .get(b)
                            .map(|key| (key.artist.as_str(), key.album.as_str())),
                    )
                    .then(ta.disc_no.cmp(&tb.disc_no))
                    .then(ta.track_no.cmp(&tb.track_no))
                    .then_with(|| tie(a, b, ta, tb)),
                _ => a.cmp(b),
            }),
            SortMode::Title => ids.sort_by(|a, b| match (tracks.get(a), tracks.get(b)) {
                (Some(ta), Some(tb)) => tie(a, b, ta, tb),
                _ => a.cmp(b),
            }),
            SortMode::Recent => ids.sort_by(|a, b| match (tracks.get(a), tracks.get(b)) {
                (Some(ta), Some(tb)) => tb
                    .added_at
                    .cmp(&ta.added_at)
                    .then_with(|| tie(a, b, ta, tb)),
                _ => a.cmp(b),
            }),
            SortMode::MostPlayed => ids.sort_by(|a, b| match (tracks.get(a), tracks.get(b)) {
                (Some(ta), Some(tb)) => tb
                    .play_count
                    .cmp(&ta.play_count)
                    .then_with(|| tie(a, b, ta, tb)),
                _ => a.cmp(b),
            }),
            // Shuffle is a deterministic hash permutation, so the render order is
            // stable across keystrokes; ties resolve by title and path.
            SortMode::Shuffle => ids.sort_by(|a, b| {
                let ka = shuffle_key(*a);
                let kb = shuffle_key(*b);
                match (tracks.get(a), tracks.get(b)) {
                    (Some(ta), Some(tb)) => ka.cmp(&kb).then_with(|| tie(a, b, ta, tb)),
                    _ => ka.cmp(&kb).then_with(|| a.cmp(b)),
                }
            }),
        }
        ids
    }

    // Playlists

    /// Install playlists restored from storage and reserve ids past the highest.
    ///
    /// Track ids start empty and [`Self::add_track`] resolves each stored path
    /// as scan results arrive. The persistence loader already refuses duplicate
    /// ids, so this installs the set as-is.
    pub(crate) fn restore_playlists(&mut self, playlists: Vec<Playlist>) {
        self.playlist_ids
            .reserve_past(playlists.iter().map(|p| p.id).max());
        self.playlists = playlists;
        self.rebuild_playlist_index();
        self.rebuild_playlist_positions();
    }

    /// Rebuild `playlist_index` from every playlist's saved paths, in playlist
    /// order so membership resolution is deterministic.
    fn rebuild_playlist_index(&mut self) {
        let mut index: HashMap<PathBuf, Vec<u64>> = HashMap::new();
        for playlist in &self.playlists {
            for path in &playlist.track_paths {
                let entry = index.entry(path.clone()).or_default();
                if !entry.contains(&playlist.id) {
                    entry.push(playlist.id);
                }
            }
        }
        self.playlist_index = index;
    }

    /// Create a new empty playlist, returning its id, or `None` when the
    /// playlist id space is exhausted (no playlist is created).
    pub fn create_playlist(&mut self, name: String) -> Option<u64> {
        let id = self.playlist_ids.allocate()?;
        self.playlist_positions.insert(id, self.playlists.len());
        self.playlists.push(Playlist {
            id,
            name,
            track_paths: Vec::new(),
            track_ids: Vec::new(),
        });
        Some(id)
    }

    pub fn add_track_to_playlist(&mut self, playlist_id: u64, track_id: u64) {
        let track_path = match self.tracks.get(&track_id) {
            Some(t) => t.path.clone(),
            None => return,
        };
        if let Some(p) = self.playlists.iter_mut().find(|p| p.id == playlist_id)
            && !p.track_ids.contains(&track_id)
        {
            p.track_ids.push(track_id);
            p.track_paths.push(track_path.clone());
            let entry = self.playlist_index.entry(track_path).or_default();
            if !entry.contains(&playlist_id) {
                entry.push(playlist_id);
            }
        }
    }

    /// Create one named playlist from a fully parsed report and add every valid
    /// row, returning the new id and bounded counts.
    ///
    /// All admitted paths are kept in `track_paths`, so a path with no current
    /// library track resolves later through
    /// [`Self::resolve_playlist_membership`]. Returns `None` only when the
    /// playlist id space is exhausted.
    pub fn import_paths_as_playlist(
        &mut self,
        name: String,
        report: &crate::playlist_io::ParseReport,
    ) -> Option<(u64, ImportCounts)> {
        let id = self.create_playlist(name)?;
        let slot = self
            .playlist_slot(id)
            .expect("a just-created playlist has a position");
        let mut imported = 0usize;
        let mut missing = 0usize;
        for path in &report.paths {
            // The parser already deduplicates paths; this keeps the index consistent.
            if self.playlists[slot].track_paths.contains(path) {
                continue;
            }
            self.playlists[slot].track_paths.push(path.clone());
            let entry = self.playlist_index.entry(path.clone()).or_default();
            if !entry.contains(&id) {
                entry.push(id);
            }
            if let Some(tid) = self.track_id_for_path(path) {
                if !self.playlists[slot].track_ids.contains(&tid) {
                    self.playlists[slot].track_ids.push(tid);
                }
                imported += 1;
            } else {
                missing += 1;
            }
        }
        Some((
            id,
            ImportCounts {
                imported,
                missing,
                duplicate: report.duplicate,
                refused: report.refused,
            },
        ))
    }

    /// The ordered durable track paths of a playlist, for export, or `None` for
    /// an unknown id. Tracks missing from the library still export.
    #[must_use]
    pub fn playlist_export_paths(&self, playlist_id: u64) -> Option<Vec<PathBuf>> {
        let slot = self.playlist_slot(playlist_id)?;
        Some(self.playlists[slot].track_paths.clone())
    }

    /// Remove a track from a playlist.
    ///
    /// Not yet wired to any UI action; adding it needs a `ContextAction`
    /// variant, target gating in `context_menu`, and a reducer arm.
    #[allow(dead_code)]
    pub fn remove_track_from_playlist(&mut self, playlist_id: u64, track_id: u64) {
        if let Some(p) = self.playlists.iter_mut().find(|p| p.id == playlist_id)
            && let Some(idx) = p.track_ids.iter().position(|&t| t == track_id)
        {
            p.track_ids.remove(idx);
            let removed_path = p.track_paths.remove(idx);
            // Drop the playlist from the path's index entry only when no other
            // copy of that path remains in the same playlist.
            if !p.track_paths.contains(&removed_path)
                && let Some(entry) = self.playlist_index.get_mut(&removed_path)
            {
                entry.retain(|&pid| pid != playlist_id);
                if entry.is_empty() {
                    self.playlist_index.remove(&removed_path);
                }
            }
        }
    }

    pub fn delete_playlist(&mut self, playlist_id: u64) {
        if let Some(p) = self.playlists.iter().find(|p| p.id == playlist_id) {
            for path in &p.track_paths {
                if let Some(entry) = self.playlist_index.get_mut(path) {
                    entry.retain(|&pid| pid != playlist_id);
                }
            }
            self.playlist_index.retain(|_, entry| !entry.is_empty());
        }
        self.playlists.retain(|p| p.id != playlist_id);
        self.rebuild_playlist_positions();
    }

    /// Rename a playlist, preserving its tracks; `false` when the id is unknown.
    ///
    /// Ids are never reused. The caller validates with
    /// [`Library::validate_playlist_name`].
    pub fn rename_playlist(&mut self, playlist_id: u64, new_name: String) -> bool {
        if let Some(p) = self.playlists.iter_mut().find(|p| p.id == playlist_id) {
            p.name = new_name;
            true
        } else {
            false
        }
    }

    /// Validate a proposed playlist name, returning the trimmed name to commit
    /// or the reason it is rejected.
    ///
    /// `target_id` is excluded from the duplicate check, so re-submitting a
    /// playlist's own current name is accepted. Length is counted in Unicode
    /// scalar values.
    pub fn validate_playlist_name(
        &self,
        target_id: u64,
        raw: &str,
    ) -> Result<String, PlaylistNameError> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Err(PlaylistNameError::Empty);
        }
        if trimmed.chars().count() > MAX_PLAYLIST_NAME_LEN {
            return Err(PlaylistNameError::TooLong);
        }
        if self
            .playlists
            .iter()
            .any(|p| p.id != target_id && p.name == trimmed)
        {
            return Err(PlaylistNameError::Duplicate);
        }
        Ok(trimmed.to_string())
    }

    /// Create a playlist from an ordered list of track ids, returning its id,
    /// or `None` when the playlist id space is exhausted. Order is preserved;
    /// unknown and duplicate ids are dropped.
    pub fn save_queue_as_playlist(&mut self, name: String, queue: &[u64]) -> Option<u64> {
        let id = self.create_playlist(name)?;
        for &tid in queue {
            self.add_track_to_playlist(id, tid);
        }
        Some(id)
    }

    /// Resolve the tracks a [`PickerTarget`] contributes, in a stable order.
    ///
    /// - [`PickerTarget::Track`] → that one id (empty if unknown).
    /// - [`PickerTarget::Album`] → the album's tracks in album order.
    /// - [`PickerTarget::CurrentTrack`] → `current` if `Some` and known, else
    ///   empty; the library does not know what is playing.
    pub fn picker_track_ids(&self, target: PickerTarget, current: Option<u64>) -> Vec<u64> {
        match target {
            PickerTarget::Track(id) => {
                if self.tracks.contains_key(&id) {
                    vec![id]
                } else {
                    Vec::new()
                }
            }
            PickerTarget::Album(album_id) => self
                .albums
                .iter()
                .find(|a| a.id == album_id)
                .map(|a| a.track_ids.clone())
                .unwrap_or_default(),
            PickerTarget::CurrentTrack => match current {
                Some(id) if self.tracks.contains_key(&id) => vec![id],
                _ => Vec::new(),
            },
        }
    }

    /// Add every track a [`PickerTarget`] resolves to into an existing
    /// playlist, skipping duplicates and unknown ids.
    ///
    /// Returns the number of tracks the target resolved to, not the number
    /// newly added.
    pub fn add_target_to_playlist(
        &mut self,
        playlist_id: u64,
        target: PickerTarget,
        current: Option<u64>,
    ) -> usize {
        let ids = self.picker_track_ids(target, current);
        for &tid in &ids {
            self.add_track_to_playlist(playlist_id, tid);
        }
        ids.len()
    }

    /// Create a playlist named `name` and add a picker target's tracks to it.
    /// Returns the new playlist id, or `None` when the playlist id space is
    /// exhausted.
    pub fn create_playlist_with_target(
        &mut self,
        name: String,
        target: PickerTarget,
        current: Option<u64>,
    ) -> Option<u64> {
        let id = self.create_playlist(name)?;
        self.add_target_to_playlist(id, target, current);
        Some(id)
    }

    /// Search the library by case-insensitive substring on title, album and
    /// artist.
    ///
    /// Returns every match; display truncation happens only in
    /// [`track_list_visible`], which discloses the total.
    pub fn search(&self, query: &str) -> SearchResults {
        let q = query.trim().to_lowercase();
        if q.is_empty() {
            return SearchResults::default();
        }
        let mut tracks: Vec<u64> = Vec::new();
        for &id in &self.track_order {
            if let Some(t) = self.tracks.get(&id)
                && (t.title.to_lowercase().contains(&q)
                    || t.artist.to_lowercase().contains(&q)
                    || t.album.to_lowercase().contains(&q))
            {
                tracks.push(id);
            }
        }
        let albums: Vec<u64> = self
            .albums
            .iter()
            .filter(|a| a.title.to_lowercase().contains(&q) || a.artist.to_lowercase().contains(&q))
            .map(|a| a.id)
            .collect();
        let artists: Vec<u64> = self
            .artists
            .iter()
            .filter(|a| a.name.to_lowercase().contains(&q))
            .map(|a| a.id)
            .collect();
        SearchResults {
            tracks,
            albums,
            artists,
        }
    }
}
