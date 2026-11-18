// Playback state

/// What the now-playing surface shows when it is not a library track.
///
/// A stream selection and [`PlaybackState::current_track_id`] are mutually
/// exclusive: [`PlaybackState::select_stream`] clears the track, and every
/// track load clears the stream. [`PlaybackState::stream_selection`] lets a
/// loaded track win, so a missed clear still shows one answer.
///
/// A selection never invents progress: transport fields come from the audio
/// pipeline, and a live stream keeps `duration_secs == 0` (no seek bar).
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StreamSelection {
    /// A tuned radio station, by [`crate::radio::RadioStation::id`].
    Station(u64),
    /// A selected podcast episode, keyed by feed URL and item `guid` so a
    /// retitled or reordered feed keeps the same selection.
    Episode { feed_url: String, guid: String },
}

/// The three lines the now-playing surfaces print for a [`StreamSelection`],
/// resolved by [`resolve_stream`] at render time so a renamed episode is shown
/// current.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StreamNowPlaying {
    pub title: String,
    pub subtitle: String,
    pub detail: String,
}

/// The third line under a selected podcast episode.
pub const PODCAST_EPISODE_DETAIL: &str = "Podcast episode";

/// Resolve a [`StreamSelection`] into the now-playing lines, or `None` when it
/// no longer names a catalog entry, so the panel shows its empty state.
pub fn resolve_stream(
    selection: &StreamSelection,
    radio: &crate::radio::RadioState,
    subscriptions: &[crate::podcasts::PodcastFeed],
) -> Option<StreamNowPlaying> {
    match selection {
        StreamSelection::Station(id) => {
            let station = radio.station(*id)?;
            Some(StreamNowPlaying {
                title: station.name.clone(),
                subtitle: station.description.clone(),
                detail: station.tags.clone(),
            })
        }
        StreamSelection::Episode { feed_url, guid } => {
            let feed = subscriptions.iter().find(|f| f.feed_url == *feed_url)?;
            let episode = feed.episodes.iter().find(|e| e.guid == *guid)?;
            Some(StreamNowPlaying {
                title: episode.title.clone(),
                subtitle: if feed.title.is_empty() {
                    feed.feed_url.clone()
                } else {
                    feed.title.clone()
                },
                detail: PODCAST_EPISODE_DETAIL.to_string(),
            })
        }
    }
}

/// Audio-route consequence of [`PlaybackState::drop_tracks`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlaybackDropAction {
    /// The current source was unaffected.
    None,
    /// The current source vanished and playback stopped.
    Stopped,
    /// The current source vanished and this surviving track took its place.
    Loaded(u64),
}

/// Checked, nonzero identity for one occurrence of a track in the play queue.
///
/// The same track may be queued more than once, and a row index aliases
/// whatever slides into it after a remove or reorder. An occurrence keeps its
/// id through reorder and current-track movement, and ids are never reused
/// within one process. Views, focus ids and context targets key on it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct QueueEntryId(std::num::NonZeroU64);

impl QueueEntryId {
    /// Wrap a raw value, rejecting zero.
    ///
    /// Ids are normally minted by [`PlaybackState`]; this is for callers that
    /// already hold a checked value.
    #[must_use]
    pub fn new(value: u64) -> Option<Self> {
        std::num::NonZeroU64::new(value).map(Self)
    }

    /// The underlying nonzero value, used to derive a stable focus-id string.
    #[must_use]
    pub fn get(self) -> u64 {
        self.0.get()
    }
}

/// Ceiling on live queue occurrences, equal to the persisted bound
/// [`MAX_QUEUE_PATHS`] so a queue that fits on disk also fits in memory.
pub const MAX_QUEUE_ENTRIES: usize = MAX_QUEUE_PATHS;

/// Where a selected stream (station or episode) is between being chosen and
/// playing: still connecting, or refused with a short reason. A playing or
/// paused stream is `Idle` and `playing` says which.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum StreamPhase {
    #[default]
    Idle,
    Loading,
    Failed(String),
}

/// Current playback state: a library track
/// ([`PlaybackState::current_track_id`]) or a [`StreamSelection`], never both.
#[derive(Clone, Debug)]
pub struct PlaybackState {
    pub playing: bool,
    pub current_track_id: Option<u64>,
    /// The tuned station or selected episode when the current source is not a
    /// library track.
    pub current_stream: Option<StreamSelection>,
    /// The selected stream's connection state.
    pub stream_phase: StreamPhase,
    pub position_secs: f64,
    pub duration_secs: f64,
    pub volume: f32,
    pub muted: bool,
    pub repeat: RepeatMode,
    pub shuffle: bool,
    pub(crate) unshuffled_order: Vec<QueueEntryId>,
    pub queue: Vec<u64>,
    pub queue_index: usize,
    /// The [`QueueEntryId`] of each occurrence in `queue`, same length and order.
    ///
    /// Mutation methods move ids with their entries. Code that assigns `queue`
    /// directly must follow with [`Self::reset_queue_from_ids`] or
    /// `Self::resync_queue_entry_ids`.
    pub queue_entry_ids: Vec<QueueEntryId>,
    /// Count of occurrence ids minted so far; the next id is `queue_seq + 1`.
    ///
    /// Never reset or wrapped within a process, so a stale menu event cannot
    /// alias a later id. Not persisted.
    pub queue_seq: u64,
    /// Per-source generation stamped on `Message::PlaybackCompleted`.
    ///
    /// Bumped on every source replacement, so completions for an older source
    /// are dropped. Not persisted; it restarts at `0` each launch.
    pub completion_generation: u64,
    /// Whether the current generation already credited a play, so a repeated
    /// end-of-stream cannot double-count. Reset when the generation bumps.
    pub completion_credited: bool,
}

impl Default for PlaybackState {
    fn default() -> Self {
        Self {
            playing: false,
            current_track_id: None,
            current_stream: None,
            stream_phase: StreamPhase::Idle,
            position_secs: 0.0,
            duration_secs: 0.0,
            volume: 0.8,
            muted: false,
            repeat: RepeatMode::Off,
            shuffle: false,
            unshuffled_order: Vec::new(),
            queue: Vec::new(),
            queue_index: 0,
            queue_entry_ids: Vec::new(),
            queue_seq: 0,
            completion_generation: 0,
            completion_credited: false,
        }
    }
}

impl PlaybackState {
    pub fn current_track<'a>(&self, lib: &'a Library) -> Option<&'a Track> {
        self.current_track_id.and_then(|id| lib.tracks.get(&id))
    }

    // Queue-occurrence identity

    /// Mint the next [`QueueEntryId`], or `None` once the counter is exhausted;
    /// it never wraps.
    fn mint_queue_entry_id(&mut self) -> Option<QueueEntryId> {
        let next = self.queue_seq.checked_add(1)?;
        let id = QueueEntryId(std::num::NonZeroU64::new(next)?);
        self.queue_seq = next;
        Some(id)
    }

    /// Whether `additional` more occurrences fit under [`MAX_QUEUE_ENTRIES`] and
    /// the id counter. Enqueue methods check this before mutating, so a refusal
    /// never leaves the queue half-changed.
    #[must_use]
    pub fn can_enqueue(&self, additional: usize) -> bool {
        self.queue.len().saturating_add(additional) <= MAX_QUEUE_ENTRIES
            && self.queue_seq.checked_add(additional as u64).is_some()
    }

    /// Replace the queue with `ids`, minting a fresh occurrence id for each.
    ///
    /// Capped at [`MAX_QUEUE_ENTRIES`] and stops if the counter is exhausted.
    /// The counter is not reset, so new ids never alias older ones.
    pub fn reset_queue_from_ids(&mut self, ids: &[u64]) {
        self.queue.clear();
        self.queue_entry_ids.clear();
        self.unshuffled_order.clear();
        for &tid in ids.iter().take(MAX_QUEUE_ENTRIES) {
            let Some(id) = self.mint_queue_entry_id() else {
                break;
            };
            self.queue.push(tid);
            self.queue_entry_ids.push(id);
        }
    }

    /// Re-mint ids only when the id vector length has drifted from the queue,
    /// as after direct test assignment; never re-mints a live id otherwise.
    fn heal_queue_entry_ids_len(&mut self) {
        if self.queue_entry_ids.len() != self.queue.len() {
            self.remint_all_entry_ids();
        }
    }

    /// Re-mint the id vector to match the queue, dropping any unlabelled tail
    /// if the counter exhausts, so the lockstep invariant holds.
    fn remint_all_entry_ids(&mut self) {
        self.queue_entry_ids.clear();
        for _ in 0..self.queue.len() {
            match self.mint_queue_entry_id() {
                Some(id) => self.queue_entry_ids.push(id),
                None => break,
            }
        }
        self.queue.truncate(self.queue_entry_ids.len());
        if self.queue_index >= self.queue.len() {
            self.queue_index = self.queue.len().saturating_sub(1);
        }
    }

    /// The occurrence id at `pos`, or `None` past the end.
    #[must_use]
    pub fn entry_id_at(&self, pos: usize) -> Option<QueueEntryId> {
        self.queue_entry_ids.get(pos).copied()
    }

    /// The live position of `entry`, or `None` once that occurrence has left
    /// the queue, which makes a stale context-menu event inert.
    #[must_use]
    pub fn index_of_entry(&self, entry: QueueEntryId) -> Option<usize> {
        self.queue_entry_ids.iter().position(|&e| e == entry)
    }

    /// Is this exact occurrence still in the queue?
    #[must_use]
    pub fn contains_entry(&self, entry: QueueEntryId) -> bool {
        self.queue_entry_ids.contains(&entry)
    }

    /// The track id of `entry`, resolved through its live position, or `None`
    /// once the occurrence has departed.
    #[must_use]
    pub fn track_of_entry(&self, entry: QueueEntryId) -> Option<u64> {
        self.index_of_entry(entry)
            .and_then(|pos| self.queue.get(pos).copied())
    }

    /// The occurrence id of the current entry, or `None` when nothing is loaded.
    #[must_use]
    pub fn current_entry_id(&self) -> Option<QueueEntryId> {
        self.current_track_id?;
        self.entry_id_at(self.queue_index)
    }

    /// Point the now-playing surfaces at a station or an episode.
    ///
    /// Clears the track slot, zeroes the transport fields and sets `playing`.
    pub fn select_stream(&mut self, selection: StreamSelection) {
        self.current_stream = Some(selection);
        self.stream_phase = StreamPhase::Loading;
        self.current_track_id = None;
        self.position_secs = 0.0;
        self.duration_secs = 0.0;
        self.playing = true;
    }

    /// Tune a radio station: [`select_stream`](Self::select_stream) with
    /// [`StreamSelection::Station`].
    pub fn tune_station(&mut self, station_id: u64) {
        self.select_stream(StreamSelection::Station(station_id));
    }

    /// Select a podcast episode: [`select_stream`](Self::select_stream) with
    /// [`StreamSelection::Episode`].
    pub fn select_episode(&mut self, feed_url: String, guid: String) {
        self.select_stream(StreamSelection::Episode { feed_url, guid });
    }

    /// The stream selection to render; a loaded library track takes precedence.
    pub fn stream_selection(&self) -> Option<&StreamSelection> {
        if self.current_track_id.is_some() {
            return None;
        }
        self.current_stream.as_ref()
    }

    /// Whether this episode is the current stream selection.
    pub fn is_current_episode(&self, feed_url: &str, guid: &str) -> bool {
        matches!(
            self.stream_selection(),
            Some(StreamSelection::Episode { feed_url: f, guid: g }) if f == feed_url && g == guid
        )
    }

    /// Load a track into the now-playing slot without changing `playing` or the
    /// queue.
    ///
    /// Every current-track swap goes through here so `duration_secs` always
    /// matches the track; an unknown id clears the duration to 0. Also clears
    /// any stream selection and bumps the completion generation.
    pub fn load_current_track(&mut self, track_id: u64, lib: &Library) {
        self.current_track_id = Some(track_id);
        // A track and a stream are mutually exclusive.
        self.current_stream = None;
        self.position_secs = 0.0;
        self.duration_secs = lib
            .tracks
            .get(&track_id)
            .map(|t| t.duration_secs)
            .unwrap_or(0.0);
        // Each source replacement starts a fresh play-qualification session.
        self.bump_completion_generation();
    }

    /// Bump the completion generation and reset the credited flag together.
    ///
    /// `clear_queue` and current-row removal call this directly; other
    /// callers should go through [`Self::load_current_track`].
    pub(crate) fn bump_completion_generation(&mut self) {
        self.completion_generation = self.completion_generation.saturating_add(1);
        self.completion_credited = false;
    }

    /// Play a specific track at the queue's current position and mark playing.
    ///
    /// Returns `false` only when a new occurrence was needed but the queue or id
    /// space was full; nothing is mutated then. Jumping to a queued track
    /// always succeeds.
    pub fn play_track(&mut self, track_id: u64, lib: &Library) -> bool {
        self.heal_queue_entry_ids_len();
        if let Some(pos) = self.queue.iter().position(|&t| t == track_id) {
            self.queue_index = pos;
        } else {
            if !self.can_enqueue(1) {
                return false;
            }
            let Some(id) = self.mint_queue_entry_id() else {
                return false;
            };
            let idx = self.queue_index.min(self.queue.len());
            self.queue.insert(idx, track_id);
            self.queue_entry_ids.insert(idx, id);
        }
        self.load_current_track(track_id, lib);
        self.playing = true;
        true
    }

    /// Mix upcoming occurrences without interrupting the current source.
    pub fn set_shuffle(&mut self, enabled: bool) {
        if self.shuffle == enabled { return; }
        let start = self.queue_index.saturating_add(1).min(self.queue.len());
        if enabled {
            self.shuffle_queue_from(start);
        } else {
            let ranks: std::collections::HashMap<_, _> = self.unshuffled_order.iter()
                .enumerate().map(|(index, entry)| (*entry, index)).collect();
            let mut entries: Vec<_> = self.queue[start..].iter().copied()
                .zip(self.queue_entry_ids[start..].iter().copied()).collect();
            entries.sort_by_key(|(_, entry)| ranks.get(entry).copied().unwrap_or(usize::MAX));
            for (index, (track, entry)) in entries.into_iter().enumerate() {
                self.queue[start + index] = track;
                self.queue_entry_ids[start + index] = entry;
            }
            self.unshuffled_order.clear();
        }
        self.shuffle = enabled;
    }

    pub(crate) fn shuffle_queue_from(&mut self, start: usize) {
        self.heal_queue_entry_ids_len();
        let start = start.min(self.queue.len());
        self.unshuffled_order = self.queue_entry_ids.clone();
        let mut entries: Vec<_> = self.queue[start..].iter().copied()
            .zip(self.queue_entry_ids[start..].iter().copied()).collect();
        fastrand::shuffle(&mut entries);
        if entries.len() > 1 && entries.iter().map(|(_, entry)| entry)
            .eq(self.queue_entry_ids[start..].iter()) {
            entries.rotate_left(1);
        }
        for (index, (track, entry)) in entries.into_iter().enumerate() {
            self.queue[start + index] = track;
            self.queue_entry_ids[start + index] = entry;
        }
    }

    /// Insert a track just after the current one.
    ///
    /// Returns `false` when a new occurrence could not be enqueued; moving an
    /// already-queued track keeps its id and always succeeds.
    pub fn play_next(&mut self, track_id: u64) -> bool {
        self.heal_queue_entry_ids_len();
        // Move an already-queued occurrence with its id instead of minting one.
        if let Some(pos) = self.queue.iter().position(|&t| t == track_id) {
            if pos != self.queue_index {
                self.queue.remove(pos);
                let entry = self.queue_entry_ids.remove(pos);
                let insert_at = if pos < self.queue_index {
                    // The removal shifted queue_index down.
                    self.queue_index.saturating_sub(1) + 1
                } else {
                    self.queue_index + 1
                };
                let clamped = insert_at.min(self.queue.len());
                self.queue.insert(clamped, track_id);
                self.queue_entry_ids.insert(clamped, entry);
                // Re-align queue_index if the removed item was before it.
                if pos < self.queue_index {
                    self.queue_index = self.queue_index.saturating_sub(1);
                }
            }
            return true;
        }
        if !self.can_enqueue(1) {
            return false;
        }
        let Some(id) = self.mint_queue_entry_id() else {
            return false;
        };
        let insert_at = if self.queue.is_empty() {
            0
        } else {
            self.queue_index + 1
        };
        let insert_at = insert_at.min(self.queue.len());
        self.queue.insert(insert_at, track_id);
        self.queue_entry_ids.insert(insert_at, id);
        true
    }

    /// Append a track to the end of the queue.
    ///
    /// Returns `false` when a new occurrence could not be enqueued; a track
    /// already in the queue is a successful no-op.
    pub fn add_to_queue(&mut self, track_id: u64) -> bool {
        self.heal_queue_entry_ids_len();
        if self.queue.contains(&track_id) {
            return true;
        }
        if !self.can_enqueue(1) {
            return false;
        }
        let Some(id) = self.mint_queue_entry_id() else {
            return false;
        };
        self.queue.push(track_id);
        self.queue_entry_ids.push(id);
        true
    }

    /// Seed the queue with `ids` and play the first one.
    ///
    /// Backs the `Play All` and `Shuffle Library` cards; empty `ids` is a no-op.
    // Every method that replaces `current_track_id` must bump
    // `completion_generation` so a pending completion cannot credit the next source.
    pub fn seed_queue_with(&mut self, ids: &[u64], lib: &Library) {
        if ids.is_empty() {
            return;
        }
        // `reset_queue_from_ids` keeps the counter, so seeded ids never alias.
        self.reset_queue_from_ids(ids);
        if self.queue.is_empty() {
            // Counter exhausted before any entry; leave the transport untouched.
            return;
        }
        self.queue_index = 0;
        let first = self.queue[0];
        self.load_current_track(first, lib);
        self.playing = true;
    }

    /// Remove the entry at `queue_pos`; when it was current, the next entry
    /// becomes current.
    pub fn remove_from_queue(&mut self, queue_pos: usize, lib: &Library) {
        self.heal_queue_entry_ids_len();
        if queue_pos >= self.queue.len() {
            return;
        }
        let was_current = self.current_track_id.is_some() && queue_pos == self.queue_index;
        self.queue.remove(queue_pos);
        self.queue_entry_ids.remove(queue_pos);
        if self.stream_selection().is_some() {
            if queue_pos < self.queue_index { self.queue_index -= 1; }
            self.queue_index = self.queue_index.min(self.queue.len().saturating_sub(1));
            return;
        }
        if self.queue.is_empty() {
            self.queue_index = 0;
            self.current_track_id = None;
            self.current_stream = None;
            self.position_secs = 0.0;
            self.duration_secs = 0.0;
            self.playing = false;
            // Invalidate any pending completion for the removed track.
            self.bump_completion_generation();
            return;
        }
        if queue_pos < self.queue_index {
            self.queue_index -= 1;
        }
        if was_current {
            if self.queue_index >= self.queue.len() {
                self.queue_index = self.queue.len() - 1;
            }
            let next_id = self.queue[self.queue_index];
            self.load_current_track(next_id, lib);
        }
    }

    /// Drop every id in `removed` from the queue; when the current track
    /// vanished, advance to its nearest surviving successor or stop.
    ///
    /// The returned action tells the caller which audio-route change follows.
    pub fn drop_tracks(
        &mut self,
        removed: &std::collections::HashSet<u64>,
        lib: &Library,
    ) -> PlaybackDropAction {
        if removed.is_empty() {
            return PlaybackDropAction::None;
        }
        self.heal_queue_entry_ids_len();
        let current = self.current_track_id;
        let current_removed = current.is_some_and(|id| removed.contains(&id));
        let was_playing = self.playing;
        let old_index = self.queue_index;
        // The track that should become current when the playing one vanished:
        // the first survivor at or after the old index, else the last survivor.
        let successor: Option<u64> = self
            .queue
            .iter()
            .enumerate()
            .find(|(i, id)| *i >= old_index && !removed.contains(id))
            .map(|(_, &id)| id)
            .or_else(|| {
                self.queue
                    .iter()
                    .rev()
                    .find(|id| !removed.contains(id))
                    .copied()
            });
        let mut kept_tracks: Vec<u64> = Vec::with_capacity(self.queue.len());
        let mut kept_ids: Vec<QueueEntryId> = Vec::with_capacity(self.queue_entry_ids.len());
        for (i, &tid) in self.queue.iter().enumerate() {
            if !removed.contains(&tid) {
                kept_tracks.push(tid);
                if let Some(&eid) = self.queue_entry_ids.get(i) {
                    kept_ids.push(eid);
                }
            }
        }
        self.queue = kept_tracks;
        self.queue_entry_ids = kept_ids;

        if self.queue.is_empty() {
            self.queue_index = 0;
            if current.is_none() && !current_removed {
                return PlaybackDropAction::None;
            }
            self.current_track_id = None;
            self.current_stream = None;
            self.position_secs = 0.0;
            self.duration_secs = 0.0;
            self.playing = false;
            self.bump_completion_generation();
            return PlaybackDropAction::Stopped;
        }

        if current_removed {
            if let Some(next) = successor {
                let pos = self.queue.iter().position(|&id| id == next).unwrap_or(0);
                self.queue_index = pos;
                self.load_current_track(next, lib);
                self.playing = was_playing;
                return PlaybackDropAction::Loaded(next);
            }
            self.queue_index = 0;
            self.current_track_id = None;
            self.current_stream = None;
            self.position_secs = 0.0;
            self.duration_secs = 0.0;
            self.playing = false;
            self.bump_completion_generation();
            return PlaybackDropAction::Stopped;
        }

        if let Some(cid) = current {
            self.queue_index = self
                .queue
                .iter()
                .position(|&id| id == cid)
                .unwrap_or_else(|| self.queue_index.min(self.queue.len() - 1));
        } else if self.queue_index >= self.queue.len() {
            self.queue_index = self.queue.len() - 1;
        }
        PlaybackDropAction::None
    }

    /// Move the entry at `queue_pos` to just after the current one.
    pub fn move_to_top(&mut self, queue_pos: usize) {
        self.heal_queue_entry_ids_len();
        if queue_pos >= self.queue.len() || queue_pos == self.queue_index {
            return;
        }
        let id = self.queue.remove(queue_pos);
        let entry = self.queue_entry_ids.remove(queue_pos);
        let insert_at = if queue_pos < self.queue_index {
            // Removal shifted current down by 1.
            self.queue_index -= 1;
            self.queue_index + 1
        } else {
            self.queue_index + 1
        };
        let insert_at = insert_at.min(self.queue.len());
        self.queue.insert(insert_at, id);
        self.queue_entry_ids.insert(insert_at, entry);
    }

    /// Swap the entry at `queue_pos` with its predecessor; a no-op at 0.
    ///
    /// `queue_index` follows the current track, so the same track stays current.
    pub fn move_up(&mut self, queue_pos: usize) {
        self.heal_queue_entry_ids_len();
        if queue_pos == 0 || queue_pos >= self.queue.len() {
            return;
        }
        self.queue.swap(queue_pos - 1, queue_pos);
        self.queue_entry_ids.swap(queue_pos - 1, queue_pos);
        if self.queue_index == queue_pos {
            self.queue_index -= 1;
        } else if self.queue_index == queue_pos - 1 {
            self.queue_index += 1;
        }
    }

    /// Swap the entry at `queue_pos` with its successor; a no-op at the end.
    pub fn move_down(&mut self, queue_pos: usize) {
        self.heal_queue_entry_ids_len();
        if queue_pos + 1 >= self.queue.len() {
            return;
        }
        self.queue.swap(queue_pos, queue_pos + 1);
        self.queue_entry_ids.swap(queue_pos, queue_pos + 1);
        if self.queue_index == queue_pos {
            self.queue_index += 1;
        } else if self.queue_index == queue_pos + 1 {
            self.queue_index -= 1;
        }
    }

    /// Move `entry` to insertion `slot` (counted before the move, `0..=len`),
    /// as a drag-and-drop reorder drops it. The playing entry stays current
    /// wherever it ends up.
    pub fn move_entry(&mut self, entry: QueueEntryId, slot: usize) {
        self.heal_queue_entry_ids_len();
        let Some(from) = self.index_of_entry(entry) else {
            return;
        };
        let len = self.queue.len();
        let to = if slot > from { slot - 1 } else { slot }.min(len.saturating_sub(1));
        if to == from {
            return;
        }
        let current = self.current_entry_id();
        let track = self.queue.remove(from);
        let id = self.queue_entry_ids.remove(from);
        self.queue.insert(to, track);
        self.queue_entry_ids.insert(to, id);
        if let Some(current) = current
            && let Some(index) = self.index_of_entry(current)
        {
            self.queue_index = index;
        }
    }

    /// Jump to the entry at `queue_pos` and start playing it.
    pub fn jump_to(&mut self, queue_pos: usize, lib: &Library) {
        if queue_pos >= self.queue.len() {
            return;
        }
        self.queue_index = queue_pos;
        let target = self.queue[queue_pos];
        self.load_current_track(target, lib);
        self.playing = true;
    }

    /// The index of the nearest [`TrackAvailability::Present`] entry after the
    /// current one, optionally wrapping, or `None`. Non-present entries are
    /// skipped so an automatic advance never stalls on them.
    #[must_use]
    pub fn present_index_forward(&self, lib: &Library, wrap: bool) -> Option<usize> {
        let len = self.queue.len();
        if len == 0 {
            return None;
        }
        for step in 1..=len {
            let raw = self.queue_index + step;
            let idx = if wrap { raw % len } else { raw };
            if idx >= len {
                break;
            }
            if lib.is_playable_now(self.queue[idx]) {
                return Some(idx);
            }
        }
        None
    }

    /// The index of the nearest [`TrackAvailability::Present`] entry before the
    /// current one, optionally wrapping, or `None`.
    #[must_use]
    pub fn present_index_backward(&self, lib: &Library, wrap: bool) -> Option<usize> {
        let len = self.queue.len();
        if len == 0 {
            return None;
        }
        for step in 1..=len {
            let idx = if step <= self.queue_index {
                self.queue_index - step
            } else if wrap {
                len - (step - self.queue_index)
            } else {
                break;
            };
            if lib.is_playable_now(self.queue[idx]) {
                return Some(idx);
            }
        }
        None
    }

    /// Empty the queue and stop playback.
    pub fn clear_queue(&mut self) {
        self.unshuffled_order.clear();
        self.queue.clear();
        self.queue_entry_ids.clear();
        self.queue_index = 0;
        self.current_track_id = None;
        self.current_stream = None;
        self.position_secs = 0.0;
        self.duration_secs = 0.0;
        self.playing = false;
        // Invalidate any pending completion for the last track.
        self.bump_completion_generation();
    }
}

#[cfg(test)]
impl PlaybackState {
    pub fn resync_queue_entry_ids(&mut self) {
        self.remint_all_entry_ids();
    }
}
