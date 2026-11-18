use std::path::PathBuf;

use enzui_core::activation::ApplicationIntent;
use iced::Task;

use crate::{Message, MusicApp, domain, scanner};

pub(crate) fn scan_opened_paths(paths: &[PathBuf]) -> Result<Vec<domain::Track>, String> {
    let mut covers = crate::art::CoverCache::new();
    paths
        .iter()
        .map(|path| {
            if !scanner::is_audio_file(path)
                || !std::fs::metadata(path).is_ok_and(|metadata| {
                    metadata.is_file() && metadata.len() <= 1024 * 1024 * 1024
                })
            {
                return Err("Open supported local audio files of at most 1 GiB each.".into());
            }
            scanner::scan_track_with_covers(path, None, &mut covers)
        })
        .collect()
}

impl MusicApp {
    pub(crate) fn desktop_intents(
        &mut self,
        intents: impl IntoIterator<Item = ApplicationIntent>,
    ) -> Task<Message> {
        for intent in intents {
            if let ApplicationIntent::Open(uris) = intent {
                for uri in uris {
                    if self.desktop_paths.len() >= 128 {
                        self.post_folder_status(
                            "Open at most 128 audio files at once.",
                            crate::ToastTone::Warning,
                        );
                        break;
                    }
                    match url::Url::parse(&uri)
                        .ok()
                        .and_then(|uri| uri.to_file_path().ok())
                    {
                        Some(path) => self.desktop_paths.push(path),
                        None => self.post_folder_status(
                            "Open a local audio file.",
                            crate::ToastTone::Warning,
                        ),
                    }
                }
            }
        }
        self.pump_desktop_paths()
    }

    pub(crate) fn pump_desktop_paths(&mut self) -> Task<Message> {
        if self.boot_load_worker_spawned
            || self.desktop_open_pending
            || self.desktop_paths.is_empty()
            || self.close_after_save
        {
            return Task::none();
        }
        let paths = std::mem::take(&mut self.desktop_paths);
        self.desktop_open_pending = true;
        let work = self.boot_load_owner.submit(0, move |context| {
            if context.is_cancelled() {
                return Err("Audio opening cancelled.".into());
            }
            scan_opened_paths(&paths)
        });
        Task::perform(work, |settled| {
            Message::DesktopFilesOpened(
                settled
                    .into_result()
                    .map_err(|error| error.to_string())
                    .and_then(|result| result),
            )
        })
    }
}
