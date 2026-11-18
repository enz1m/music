#[test]
fn queue_insertion_playback_and_removal_keep_the_current_song() {
    let mut library = Library::new();
    let a = library.add_track(t("/a.mp3", "A", "Artist", "Album", 0));
    let b = library.add_track(t("/b.mp3", "B", "Artist", "Album", 0));
    let c = library.add_track(t("/c.mp3", "C", "Artist", "Album", 0));
    library.reindex();
    let mut playback = PlaybackState::default();
    playback.play_track(a, &library);
    playback.add_to_queue(b);
    playback.add_to_queue(b);
    playback.play_next(c);
    assert_eq!(playback.queue, [a, c, b]);
    playback.jump_to(1, &library);
    assert_eq!(playback.current_track_id, Some(c));
    assert!(playback.playing);
    playback.remove_from_queue(1, &library);
    assert_eq!(playback.queue, [a, b]);
    assert_eq!(playback.current_track_id, Some(b));
    playback.clear_queue();
    assert!(playback.queue.is_empty());
    assert!(!playback.playing);
    assert_eq!(playback.duration_secs, 0.0);
}

#[test]
fn reordering_and_boundary_actions_preserve_queue_identity() {
    let mut playback = PlaybackState {
        queue: vec![10, 20, 30, 40],
        queue_index: 1,
        current_track_id: Some(20),
        ..Default::default()
    };
    playback.move_to_top(3);
    assert_eq!(playback.queue, [10, 20, 40, 30]);
    playback.move_up(2);
    assert_eq!(playback.queue, [10, 40, 20, 30]);
    assert_eq!(playback.queue_index, 2);
    playback.move_down(2);
    assert_eq!(playback.queue, [10, 40, 30, 20]);
    assert_eq!(playback.queue_index, 3);
    playback.move_up(0);
    playback.move_down(3);
    playback.jump_to(99, &Library::new());
    assert_eq!(playback.queue, [10, 40, 30, 20]);
    assert_eq!(playback.current_track_id, Some(20));
    assert_eq!(playback.queue_index, 3);
}

#[test]
fn removing_neighbors_and_the_last_song_keeps_playback_consistent() {
    let mut library = Library::new();
    let a = library.add_track(t("/a.mp3", "A", "Artist", "Album", 0));
    let b = library.add_track(t("/b.mp3", "B", "Artist", "Album", 0));
    let c = library.add_track(t("/c.mp3", "C", "Artist", "Album", 0));
    library.reindex();
    let mut playback = PlaybackState {
        queue: vec![a, b, c],
        queue_index: 1,
        current_track_id: Some(b),
        playing: true,
        ..Default::default()
    };
    playback.remove_from_queue(0, &library);
    assert_eq!(playback.queue, [b, c]);
    assert_eq!(playback.queue_index, 0);
    assert_eq!(playback.current_track_id, Some(b));
    playback.remove_from_queue(1, &library);
    assert_eq!(playback.current_track_id, Some(b));
    playback.remove_from_queue(0, &library);
    assert!(playback.queue.is_empty());
    assert!(!playback.playing);
}
