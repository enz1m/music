use std::fs::{self, OpenOptions};
use std::io::{Read, Seek, SeekFrom};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use enzui_core::user_file::{SavePolicy, save_with};
use enzui_widgets::file_drop::FileDrop;
use iced::Task;

use crate::{Message, MusicApp, domain, scanner};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Target {
    Library,
    Playlist(u64),
    Queue,
}

const FILE_LIMIT: u64 = 1024 * 1024 * 1024;

impl MusicApp {
    pub(crate) fn native_import_directory(&self) -> PathBuf {
        self.persist
            .path()
            .parent()
            .unwrap_or(Path::new("."))
            .join("Imported Music")
    }

    pub(crate) fn native_files_dropped(&mut self, drop: FileDrop, target: Target) -> Task<Message> {
        if !self.persist.is_enabled()
            || self.native_import_pending
            || self.close_after_save
            || drop
                .action()
                .is_some_and(|action| action != enzui_widgets::file_drop::DragAction::Copy)
            || drop.paths().is_empty()
            || drop.paths().len() > 128
        {
            return Task::none();
        }
        let directory = self.native_import_directory();
        if !directory.is_absolute() {
            self.post_folder_status("Music storage is unavailable.", crate::ToastTone::Warning);
            return Task::none();
        }
        if !self.native_target_exists(target) {
            return Task::none();
        }
        let (known, paths): (Vec<_>, Vec<_>) = drop
            .paths()
            .iter()
            .map(|path| (self.library.track_id_for_path(path), path.clone()))
            .partition(|(id, _)| id.is_some());
        let known: Vec<_> = known.into_iter().filter_map(|(id, _)| id).collect();
        let paths: Vec<_> = paths.into_iter().map(|(_, path)| path).collect();
        if paths.is_empty() {
            return self.native_imported(drop, target, known, Ok(Vec::new()));
        }
        self.native_import_pending = true;
        let work = self.boot_load_owner.submit(0, move |context| {
            import_files(&paths, &directory, || context.is_cancelled())
        });
        Task::perform(work, move |settled| {
            Message::FilesImported(
                target,
                known.clone(),
                drop.clone(),
                settled
                    .into_result()
                    .map_err(|error| error.to_string())
                    .and_then(|result| result),
            )
        })
    }

    fn native_target_exists(&self, target: Target) -> bool {
        match target {
            Target::Playlist(id) => self
                .library
                .playlists
                .iter()
                .any(|playlist| playlist.id == id),
            _ => true,
        }
    }

    pub(crate) fn native_imported(
        &mut self,
        drop: FileDrop,
        target: Target,
        mut ids: Vec<u64>,
        result: Result<Vec<domain::Track>, String>,
    ) -> Task<Message> {
        self.native_import_pending = false;
        if !self.persist.is_enabled() {
            return Task::none();
        }
        let target_exists = self.native_target_exists(target);
        let mut accepted = false;
        match result {
            Ok(tracks) => {
                let paths: Vec<_> = tracks.iter().map(|track| track.path.clone()).collect();
                if !tracks.is_empty() {
                    self.library.add_folder(self.native_import_directory());
                }
                self.absorb_scanned_tracks(tracks);
                ids.extend(
                    paths
                        .iter()
                        .filter_map(|path| self.library.track_id_for_path(path)),
                );
                accepted = match target {
                    _ if !target_exists => {
                        self.post_folder_status("The destination playlist was removed. Imported files remain in your library.", crate::ToastTone::Warning);
                        false
                    }
                    Target::Library => true,
                    Target::Playlist(playlist) => {
                        for id in ids {
                            self.library.add_track_to_playlist(playlist, id);
                        }
                        true
                    }
                    Target::Queue => {
                        let mut queue = self.playback.clone();
                        if ids.into_iter().all(|id| queue.add_to_queue(id)) {
                            self.playback = queue;
                            true
                        } else {
                            self.post_folder_status(
                                domain::QUEUE_FULL_DISCLOSURE,
                                crate::ToastTone::Warning,
                            );
                            false
                        }
                    }
                };
                self.refresh_view_projections();
            }
            Err(error) => {
                // Published copies remain discoverable even when a later file fails.
                self.library.add_folder(self.native_import_directory());
                self.post_folder_status(error, crate::ToastTone::Warning);
            }
        }
        self.persistence_dirty = true;
        let task = self.schedule_persistence();
        if accepted {
            if let Some(pending) = &self.pending_save {
                self.native_import_receipts
                    .push((pending.ticket().generation(), drop));
            } else if self.persist.is_enabled() {
                drop.finish();
            }
        }
        task.unwrap_or_else(|| self.maybe_finish_close().unwrap_or_else(Task::none))
    }
}

fn import_files(
    paths: &[PathBuf],
    directory: &Path,
    cancelled: impl Fn() -> bool,
) -> Result<Vec<domain::Track>, String> {
    fs::create_dir_all(directory)
        .map_err(|error| format!("Cannot create Music imports: {error}"))?;
    let mut tracks = Vec::new();
    let mut covers = crate::art::CoverCache::new();
    let mut total = 0;
    let mut seen = std::collections::HashSet::new();
    for path in paths {
        if !seen.insert(path) {
            continue;
        }
        if cancelled() {
            return Err("Music import cancelled.".into());
        }
        if !scanner::is_audio_file(path) {
            return Err("Drop supported audio files into Music.".into());
        }
        let mut input = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW)
            .open(path)
            .map_err(|error| format!("Cannot open dropped audio: {error}"))?;
        let metadata = input.metadata().map_err(|error| error.to_string())?;
        if !metadata.is_file() || metadata.len() > FILE_LIMIT {
            return Err("Drop regular audio files of at most 1 GiB each.".into());
        }
        total += metadata.len();
        if total > FILE_LIMIT {
            return Err("Import at most 1 GiB of music at a time.".into());
        }
        let name = path.file_name().ok_or("The dropped file has no name.")?;
        let mut published = None;
        for collision in 0..1000 {
            let target = if collision == 0 {
                directory.join(name)
            } else {
                let mut name = path.file_stem().unwrap_or_default().to_os_string();
                name.push(format!(" ({collision})"));
                let mut target = directory.join(name);
                if let Some(extension) = path.extension() {
                    target.set_extension(extension);
                }
                target
            };
            input
                .seek(SeekFrom::Start(0))
                .map_err(|error| error.to_string())?;
            let result = save_with(&target, SavePolicy::CreateNew, |output| {
                let mut input = (&mut input).take(metadata.len() + 1);
                let mut buffer = [0; 64 * 1024];
                let mut written = 0;
                loop {
                    if cancelled() {
                        return Err(std::io::Error::from(std::io::ErrorKind::Interrupted));
                    }
                    let read = input.read(&mut buffer)?;
                    if read == 0 {
                        break;
                    }
                    written += read as u64;
                    if written > metadata.len() {
                        return Err(std::io::Error::from(std::io::ErrorKind::InvalidData));
                    }
                    output.write_all(&buffer[..read])?;
                }
                if written != metadata.len() {
                    return Err(std::io::Error::from(std::io::ErrorKind::UnexpectedEof));
                }
                Ok(())
            });
            match result {
                Ok(()) => {
                    published = Some(target);
                    break;
                }
                Err(error) if error.io_error().kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(format!("Cannot import dropped audio: {error}")),
            }
        }
        let target = published.ok_or("Too many imports with the same name.")?;
        tracks.push(scanner::scan_track_with_covers(&target, None, &mut covers)?);
    }
    Ok(tracks)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn imported_audio_preserves_originals_and_collisions_and_cancels_without_publication() {
        let root = tempfile::tempdir().expect("isolated music imports");
        let source = root.path().join("source.mp3");
        let bytes = include_bytes!("../tests/fixtures/covered/01 - alpha.mp3");
        fs::write(&source, bytes).expect("isolated native file workflow");
        let opened = crate::desktop_open::scan_opened_paths(std::slice::from_ref(&source))
            .expect("open local audio");
        assert_eq!(
            opened[0].path, source,
            "opening audio retains its original local path"
        );
        let destination = root.path().join("imports");
        fs::create_dir(&destination).expect("isolated native file workflow");
        let existing = destination.join("source.mp3");
        fs::write(&existing, b"keep existing file").expect("isolated native file workflow");
        let tracks = import_files(&[source.clone(), source.clone()], &destination, || false)
            .expect("isolated native file workflow");
        assert_eq!(tracks.len(), 1, "one imported track per source");
        assert_ne!(tracks[0].path, existing);
        assert!(
            tracks[0].duration_secs > 0.0,
            "copy remains decodable audio"
        );
        assert_eq!(
            fs::read(&tracks[0].path).expect("isolated native file workflow"),
            bytes
        );
        assert_eq!(
            fs::read(&source).expect("isolated native file workflow"),
            bytes
        );
        assert_eq!(
            fs::read(&existing).expect("isolated native file workflow"),
            b"keep existing file"
        );
        let before = fs::read_dir(&destination)
            .expect("isolated native file workflow")
            .count();
        assert!(import_files(std::slice::from_ref(&source), &destination, || true).is_err());
        let alias = root.path().join("alias.mp3");
        std::os::unix::fs::symlink(&source, &alias).expect("isolated native file workflow");
        assert!(import_files(&[alias], &destination, || false).is_err());
        assert_eq!(
            fs::read_dir(&destination)
                .expect("isolated native file workflow")
                .count(),
            before
        );
        assert_eq!(
            fs::read(source).expect("isolated native file workflow"),
            bytes
        );
    }
}
