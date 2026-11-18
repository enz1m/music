//! The XDG Desktop Portal directory chooser behind the Library's Add Folder
//! button.
//!
//! Thin app-side state around `enzui_widgets::file_chooser`: the request
//! in flight, its target and its terminal outcome. The host's portal draws the
//! dialog, so desktop bookmarks, sandbox permissions and keyboard conventions
//! apply; `rfd` is avoided because its zenity fallback is not feature-gated.
//!
//! Requests use [`PortalOperation::OpenDirectory`] and are unparented
//! ([`PortalParent::None`]) because a Wayland parent needs an asynchronous
//! xdg-foreign export token the raw window handle does not carry.
//!
//! The D-Bus backend starts a tokio runtime, so it is built on the first pick
//! rather than at boot. It is not `Send`, so `update` calls
//! [`file_chooser::pump`](enzui_widgets::file_chooser::pump) when its
//! completion signal wakes the app; nothing polls on a clock.

use std::path::PathBuf;

use enzui_widgets::file_chooser::{
    ChooserOptions, FileRequest, FileResult, PendingRequest, PortalBackend, PortalOperation,
    PortalOutcome, PortalParent, PortalWake,
};

/// Which affordance opened the chooser, so the terminal outcome is routed to
/// the right place.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PickerTarget {
    /// The Library "Add Folder" button: choose a folder to scan and add.
    AddFolder,
}

impl PickerTarget {
    /// The portal operation this target asks for.
    pub fn operation(self) -> PortalOperation {
        match self {
            // A directory: a music folder to walk for audio files.
            Self::AddFolder => PortalOperation::OpenDirectory,
        }
    }

    /// The chooser's window title.
    pub fn title(self) -> &'static str {
        match self {
            Self::AddFolder => "Add a music folder",
        }
    }
}

/// A user-facing sentence for a non-success outcome.
///
/// Every arm is named so a new `PortalOutcome` variant cannot silently render
/// as a generic failure. [`PortalOutcome::Completed`] and
/// [`PortalOutcome::Cancelled`] return `None`.
pub fn failure_text(outcome: &PortalOutcome<FileResult>) -> Option<String> {
    let s = match outcome {
        PortalOutcome::Completed(_) | PortalOutcome::Cancelled(_) => return None,
        PortalOutcome::Unavailable => {
            "No file chooser on this system — this desktop has no XDG desktop \
             portal running, so a music folder cannot be chosen here."
        }
        PortalOutcome::Unsupported => {
            "This desktop's file portal does not support choosing folders."
        }
        PortalOutcome::NotDeclared => "This build is not permitted to open a file chooser.",
        PortalOutcome::Denied => "The file chooser was refused permission to open.",
        PortalOutcome::ParentUnavailable => {
            "The file chooser could not be attached to this window."
        }
        PortalOutcome::Failed => "The file chooser failed — no folder was added.",
    };
    Some(s.to_string())
}

/// The app-side portal chooser state.
pub struct Picker {
    /// Built on the first pick so constructing the app spawns no runtime.
    backend: Option<Box<dyn PortalBackend>>,
    /// The live request and what it was opened for.
    pending: Option<(PickerTarget, PendingRequest)>,
    /// Identity of the live request, bumped per request so iced replaces the
    /// completion subscription instead of reusing the finished stream.
    generation: u64,
}

impl std::fmt::Debug for Picker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Picker")
            .field("backend", &self.backend.as_ref().map(|b| b.describe()))
            .field("pending", &self.pending.as_ref().map(|(t, _)| *t))
            .finish()
    }
}

impl Default for Picker {
    fn default() -> Self {
        Self::new()
    }
}

impl Picker {
    /// A picker with no backend built and nothing in flight.
    pub fn new() -> Self {
        Self {
            backend: None,
            pending: None,
            generation: 0,
        }
    }

    /// Whether a request is in flight. The completion subscription is gated on
    /// this, so an app with no chooser open holds no portal wake source.
    pub fn is_pending(&self) -> bool {
        self.pending.as_ref().is_some_and(|(_, p)| p.is_pending())
    }

    /// Opens the host's chooser for `target`.
    ///
    /// # Errors
    ///
    /// Returns the terminal outcome when the request never started (no portal, or
    /// refused before any dialog); callers route it like a late outcome.
    pub fn start(&mut self, target: PickerTarget) -> Result<(), PortalOutcome<FileResult>> {
        if self.is_pending() {
            // One dialog at a time — a real portal enforces the same per parent.
            return Ok(());
        }

        let options = ChooserOptions::new().title(target.title());
        let Some(backend) = self.backend_mut() else {
            return Err(PortalOutcome::Unavailable);
        };
        let backend: &mut dyn PortalBackend = &mut **backend;
        match FileRequest::new(target.operation(), PortalParent::None)
            .with_options(options)
            // Only consulted for `PortalParent::Window`, so a constant is correct here.
            .start(backend, |_| true)
        {
            Ok(p) => {
                self.pending = Some((target, p));
                self.generation = self.generation.wrapping_add(1);
                Ok(())
            }
            Err(outcome) => Err(outcome),
        }
    }

    /// The event-driven wake source for the live request; `Subscription::none()`
    /// unless a request is pending with an asynchronous backend.
    pub fn completion_subscription(&self) -> iced::Subscription<PortalWake> {
        enzui_widgets::file_chooser::completion_subscription(
            self.generation,
            self.pending.as_ref().map(|(_, p)| p),
            self.backend.as_deref(),
        )
    }

    /// One completion-driven pass of the framework pump.
    ///
    /// Returns `Some((target, outcome))` on the wake a terminal outcome appears and
    /// `None` while the user is still deciding; the slot is then cleared.
    pub fn pump(&mut self) -> Option<(PickerTarget, PortalOutcome<FileResult>)> {
        let (target, pending) = self.pending.as_mut()?;
        let target = *target;

        // The closure is the identity because outcomes are routed through `update`;
        // the Task is read only for whether it carries an outcome.
        let mut captured: Option<PortalOutcome<FileResult>> = None;
        let backend = self
            .backend
            .as_deref_mut()
            .expect("a pending request implies a backend was built");
        let _task: iced::Task<()> = enzui_widgets::file_chooser::pump(pending, backend, |o| {
            captured = Some(o);
        });

        if captured.is_some() {
            self.pending = None;
        }
        captured.map(|o| (target, o))
    }

    // The backend, built on first use. Runtime construction can fail under FD or
    // memory pressure and is reported as `PortalOutcome::Unavailable`.
    fn backend_mut(&mut self) -> Option<&mut Box<dyn PortalBackend>> {
        if self.backend.is_none() {
            #[cfg(test)]
            if tests::backend_construction_is_forced_to_fail() {
                return None;
            }
            match enzui_widgets::file_chooser::DbusBackend::new() {
                Ok(b) => self.backend = Some(Box::new(b)),
                Err(_) => return None,
            }
        }
        self.backend.as_mut()
    }
}

/// The first local directory from a completed result, if any; a network
/// location or empty result yields `None`.
pub fn first_local_dir(result: &FileResult) -> Option<PathBuf> {
    result.local_paths().into_iter().next()
}

#[cfg(test)]
pub mod tests {
    //! An in-process [`PortalBackend`] for reducer tests: it records the
    //! operation, title and parent handle each request carried and scripts one
    //! reply, with no D-Bus and no dialog.

    use std::sync::atomic::{AtomicBool, Ordering};

    /// Force `backend_mut` to take its construction-failure path, which the
    /// real runtime only hits under FD or memory pressure.
    static FORCE_BACKEND_FAILURE: AtomicBool = AtomicBool::new(false);

    pub(super) fn backend_construction_is_forced_to_fail() -> bool {
        FORCE_BACKEND_FAILURE.load(Ordering::Relaxed)
    }
}
