//! Podcast subscriptions to and from OPML files, the format every podcast
//! app exports (AntennaPod, Pocket Casts, Overcast, gPodder, Apple Podcasts).
//!
//! Import opens the host `OpenFile` chooser filtered to OPML; the chosen file
//! is read with the playlist importer's bounded, no-follow read off the UI
//! thread. Export opens the host `SaveFile` chooser, whose grant authorizes
//! exactly one location, and publishes through the grant's reservation (the
//! identity-safe, no-clobber shape). Generations are monotonic, so a late
//! outcome from a superseded dialog is inert.

use std::path::{Path, PathBuf};

use enzui_core::download::DownloadId;
use enzui_core::download_destination::{
    DestinationBroker, DestinationOutcome, DownloadDestination,
};
use enzui_widgets::file_chooser::{
    ChooserOptions, FileFilter, FileRequest, FileResult, PendingRequest, PortalBackend,
    PortalOperation, PortalOutcome, PortalParent, PortalWake,
};

use crate::playlist_transfer::ReadRefusal;

/// The largest OPML file read: thousands of feeds fit well inside.
const MAX_OPML_BYTES: u64 = 4 * 1024 * 1024;

/// The owner of the two OPML portal dialogs.
#[derive(Default)]
pub struct OpmlTransfer {
    backend: Option<Box<dyn PortalBackend>>,
    import: Option<(u64, PendingRequest)>,
    export: DestinationBroker,
    generation: u64,
    /// The document each export dialog will save, by generation.
    exports: std::collections::HashMap<u64, String>,
}

impl std::fmt::Debug for OpmlTransfer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpmlTransfer")
            .field("import_pending", &self.import.is_some())
            .field("exports", &self.exports.len())
            .finish_non_exhaustive()
    }
}

impl OpmlTransfer {
    fn next_generation(&mut self) -> Option<u64> {
        self.generation = self.generation.checked_add(1)?;
        Some(self.generation)
    }

    fn ensure_backend(&mut self) -> bool {
        if self.backend.is_none()
            && let Ok(backend) = enzui_widgets::file_chooser::DbusBackend::new()
        {
            self.backend = Some(Box::new(backend));
        }
        self.backend.is_some()
    }

    /// Open the chooser for an OPML file to import.
    ///
    /// # Errors
    /// A sentence for the user when no chooser opens.
    pub fn start_import(&mut self) -> Result<(), String> {
        if self.import.is_some() {
            return Ok(());
        }
        let generation = self.next_generation().ok_or_else(unavailable)?;
        if !self.ensure_backend() {
            return Err(unavailable());
        }
        let options = ChooserOptions::new()
            .title("Import podcast subscriptions")
            .current_filter(FileFilter::extensions(
                "OPML subscription lists",
                ["opml", "xml"],
            ));
        let backend = self.backend.as_deref_mut().ok_or_else(unavailable)?;
        match FileRequest::new(PortalOperation::OpenFile, PortalParent::None)
            .with_options(options)
            .start(backend, |_| true)
        {
            Ok(request) => {
                self.import = Some((generation, request));
                Ok(())
            }
            Err(outcome) => Err(import_failure(&outcome).unwrap_or_else(unavailable)),
        }
    }

    /// Open the chooser for where to save `document`.
    ///
    /// # Errors
    /// A sentence for the user when no chooser opens.
    pub fn start_export(&mut self, document: String) -> Result<(), String> {
        let generation = self.next_generation().ok_or_else(unavailable)?;
        if !self.ensure_backend() {
            return Err(unavailable());
        }
        let options = ChooserOptions::new()
            .title("Export podcast subscriptions")
            .current_name("Podcasts.opml".to_owned())
            .filter(FileFilter::extensions("OPML subscription lists", ["opml"]));
        let backend = self.backend.as_deref_mut().ok_or_else(unavailable)?;
        match self.export.request_with_options(
            DownloadId(generation),
            backend,
            PortalParent::None,
            options,
            |_| true,
        ) {
            None => {
                self.exports.insert(generation, document);
                Ok(())
            }
            Some(outcome) => Err(export_failure(&outcome).unwrap_or_else(unavailable)),
        }
    }

    /// The event-driven wake for both dialogs.
    pub fn completion_subscription(&self) -> iced::Subscription<PortalWake> {
        let import = enzui_widgets::file_chooser::completion_subscription(
            self.import.as_ref().map_or(0, |(g, _)| *g),
            self.import.as_ref().map(|(_, r)| r),
            self.backend.as_deref(),
        );
        let export = self.export.completion_subscription(self.backend.as_deref());
        iced::Subscription::batch([import, export])
    }

    /// The chosen import file, once its dialog finished.
    ///
    /// `Some(Ok(path))` for a chosen local file, `Some(Err(text))` for a
    /// failure worth saying, `None` while open or after a cancel.
    pub fn pump_import(&mut self) -> Option<Result<PathBuf, String>> {
        let (_, request) = self.import.as_mut()?;
        let backend = self.backend.as_deref_mut()?;
        let mut captured = None;
        let _task: iced::Task<()> =
            enzui_widgets::file_chooser::pump(request, backend, |outcome| captured = Some(outcome));
        let outcome = captured?;
        self.import = None;
        match outcome {
            PortalOutcome::Completed(result) => Some(
                first_local_file(&result)
                    .ok_or_else(|| "That location is not a local file.".to_owned()),
            ),
            PortalOutcome::Cancelled(_) => None,
            other => import_failure(&other).map(Err),
        }
    }

    /// Finished export dialogs: each granted location with its document, or a
    /// failure worth saying.
    pub fn pump_export(&mut self) -> Vec<Result<(DownloadDestination, String), String>> {
        let Some(backend) = self.backend.as_deref_mut() else {
            return Vec::new();
        };
        let mut done = Vec::new();
        for (id, outcome) in self.export.pump(backend) {
            let document = self.exports.remove(&id.0);
            match outcome {
                DestinationOutcome::Completed(grant) => {
                    if let Some(document) = document {
                        done.push(Ok((grant, document)));
                    }
                }
                DestinationOutcome::Cancelled(_) => {}
                other => {
                    if let Some(text) = export_failure(&other) {
                        done.push(Err(text));
                    }
                }
            }
        }
        done
    }
}

fn first_local_file(result: &FileResult) -> Option<PathBuf> {
    result.local_paths().into_iter().next()
}

fn unavailable() -> String {
    "No file chooser is available on this system.".to_owned()
}

fn import_failure(outcome: &PortalOutcome<FileResult>) -> Option<String> {
    crate::playlist_transfer::import_failure_text(outcome)
        .map(|text| text.replace("a playlist cannot be imported", "nothing can be imported"))
        .map(|text| text.replace("no playlist was imported", "nothing was imported"))
}

fn export_failure(outcome: &DestinationOutcome) -> Option<String> {
    crate::playlist_transfer::export_failure_text(outcome)
        .map(|text| text.replace("a playlist cannot be exported", "nothing can be exported"))
        .map(|text| text.replace("no playlist was exported", "nothing was saved"))
}

/// Read a chosen OPML file. Runs off the UI thread.
///
/// # Errors
/// A sentence for the user when the file can't be read.
pub fn read(path: &Path) -> Result<String, String> {
    let bytes =
        crate::playlist_transfer::read_file_bounded(path, MAX_OPML_BYTES).map_err(|refusal| {
            match refusal {
                ReadRefusal::NotRegularFile => "That is not a regular file.".to_owned(),
                ReadRefusal::TooLarge => {
                    "That file is too large to be a subscription list.".to_owned()
                }
                ReadRefusal::IdentityChanged => {
                    "The file changed while it was being read. Try again.".to_owned()
                }
                _ => "The file couldn't be read.".to_owned(),
            }
        })?;
    String::from_utf8(bytes).map_err(|_| "That file isn't a text OPML document.".to_owned())
}

/// Save `document` to the granted location. Runs off the UI thread.
///
/// # Errors
/// A sentence for the user when saving fails.
pub fn publish(grant: &DownloadDestination, document: &str) -> Result<(), String> {
    let reserved = grant
        .reserve_file()
        .map_err(|_| "The file couldn't be created there.".to_owned())?;
    reserved
        .publish_with(|w| w.write_all(document.as_bytes()))
        .map_err(|e| {
            if e.was_published() {
                "The file was saved, but the disk may not have finished writing it.".to_owned()
            } else {
                "The file couldn't be saved there.".to_owned()
            }
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_exported_opml_file_reads_back_into_its_feeds() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("Podcasts.opml");
        let document = r#"<?xml version="1.0"?><opml version="2.0"><body>
            <outline type="rss" text="Telex After" xmlUrl="https://example.com/after.rss"/>
            <outline type="rss" text="Other" xmlUrl="https://example.com/other.rss"/>
        </body></opml>"#;
        std::fs::write(&path, document).unwrap();
        let text = read(&path).unwrap();
        let feeds = crate::podcasts::import_opml(&text);
        assert_eq!(feeds.len(), 2);
        assert_eq!(feeds[0].1, "https://example.com/after.rss");
    }

    #[test]
    fn a_link_or_folder_is_not_read() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("real.opml");
        std::fs::write(&target, "<opml/>").unwrap();
        let link = dir.path().join("link.opml");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        assert!(read(&link).is_err());
        assert!(read(dir.path()).is_err());
    }
}
