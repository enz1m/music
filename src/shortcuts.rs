//! Keyboard shortcuts, decoded and documented from one table.
//!
//! [`SHORTCUTS`] feeds both [`match_event`], the subscription's decoder, and
//! [`overlay_entries`], the shortcuts overlay's [`ShortcutEntry`] rows, so a
//! listed row always has a binding. Matching goes through
//! [`Shortcut::matches_event`], which ignores key-repeat events so a held key
//! does not re-fire its action.

use enzui_core::shortcut::{Modifier, Shortcut};
use enzui_widgets::shortcut_list::{ShortcutCatalogRow, ShortcutEntry, shortcut_entries};
use iced::keyboard::{self, key::Named};
// `Key` / `Modifiers` are named only by the `#[cfg(test)]`
// `match_key` / `matches_key` signatures.
use std::sync::LazyLock;

/// A discrete action a shortcut performs; every variant has a binding.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ShortcutAction {
    /// Space — play / pause.
    PlayPause,
    /// Right arrow / N — next track.
    NextTrack,
    /// Left arrow / P — previous track.
    PreviousTrack,
    /// S — toggle shuffle.
    ToggleShuffle,
    /// R — cycle repeat mode.
    CycleRepeat,
    VolumeUp,
    VolumeDown,
    ToggleMute,
    /// `/` — open search.
    OpenSearch,
    /// Q — toggle the Up Next queue.
    ToggleQueue,
    /// E — open the equalizer dialog.
    ToggleEqualizer,
    /// T — open the sleep-timer dialog.
    ToggleSleepTimer,
    /// Ctrl+/ — toggle this shortcuts overlay.
    ToggleShortcuts,
    /// Escape — close a dialog / menu / search.
    Dismiss,
}

/// One binding: a primary chord, hidden alternate chords, an overlay section
/// and description, and the action.
///
/// The overlay badge is the primary [`Shortcut`]'s canonical
/// [`Display`](std::fmt::Display), so the visible chord cannot drift from the
/// matcher.
pub struct Binding {
    pub section: &'static str,
    /// The primary shortcut, matched and drawn in the overlay via
    /// [`Binding::badge`].
    pub primary: Shortcut,
    /// Alternate chords for the same action, not shown in the overlay.
    pub alternates: Vec<Shortcut>,
    pub description: &'static str,
    pub action: ShortcutAction,
}

impl Binding {
    /// The canonical badge text drawn in the overlay for this row.
    pub fn badge(&self) -> String {
        self.primary.to_string()
    }

    /// Whether any of this binding's chords matches a raw keyboard event.
    ///
    /// Key-repeat events never match.
    fn matches_event(&self, event: &keyboard::Event) -> bool {
        self.primary.matches_event(event) || self.alternates.iter().any(|s| s.matches_event(event))
    }
}

/// The shortcut table read by both the key decoder and the overlay.
///
/// A [`LazyLock`] because [`Shortcut::new`] allocates for character keys and
/// cannot run in a `const` context.
pub static SHORTCUTS: LazyLock<Vec<Binding>> = LazyLock::new(|| {
    vec![
        Binding {
            section: "Playback",
            primary: Shortcut::new(Modifier::NONE, Named::Space),
            alternates: vec![],
            description: "Play / Pause",
            action: ShortcutAction::PlayPause,
        },
        Binding {
            section: "Playback",
            primary: Shortcut::new(Modifier::NONE, Named::ArrowRight),
            alternates: vec![Shortcut::new(Modifier::NONE, 'n')],
            description: "Next track",
            action: ShortcutAction::NextTrack,
            // Badge reads "Right", not an arrow glyph some screen readers drop.
        },
        Binding {
            section: "Playback",
            primary: Shortcut::new(Modifier::NONE, Named::ArrowLeft),
            alternates: vec![Shortcut::new(Modifier::NONE, 'p')],
            description: "Previous track",
            action: ShortcutAction::PreviousTrack,
        },
        Binding {
            section: "Playback",
            primary: Shortcut::new(Modifier::NONE, 's'),
            alternates: vec![],
            description: "Toggle shuffle",
            action: ShortcutAction::ToggleShuffle,
        },
        Binding {
            section: "Playback",
            primary: Shortcut::new(Modifier::NONE, 'r'),
            alternates: vec![],
            description: "Cycle repeat mode",
            action: ShortcutAction::CycleRepeat,
        },
        Binding {
            section: "Playback",
            primary: Shortcut::new(Modifier::NONE, '+'),
            alternates: vec![
                Shortcut::new(Modifier::SHIFT, '+'),
                Shortcut::new(Modifier::NONE, '='),
            ],
            description: "Volume up",
            action: ShortcutAction::VolumeUp,
        },
        Binding {
            section: "Playback",
            primary: Shortcut::new(Modifier::NONE, '-'),
            alternates: vec![],
            description: "Volume down",
            action: ShortcutAction::VolumeDown,
        },
        Binding {
            section: "Playback",
            primary: Shortcut::new(Modifier::NONE, 'm'),
            alternates: vec![],
            description: "Mute / unmute",
            action: ShortcutAction::ToggleMute,
        },
        // Sections must stay contiguous: `enz_shortcut_list` prints a header
        // whenever the section changes from the previous entry.
        Binding {
            section: "Playback",
            primary: Shortcut::new(Modifier::NONE, 'e'),
            alternates: vec![],
            description: "Equalizer",
            action: ShortcutAction::ToggleEqualizer,
        },
        Binding {
            section: "Playback",
            primary: Shortcut::new(Modifier::NONE, 't'),
            alternates: vec![],
            description: "Sleep timer",
            action: ShortcutAction::ToggleSleepTimer,
        },
        Binding {
            section: "Search & Queue",
            primary: Shortcut::new(Modifier::NONE, '/'),
            alternates: vec![Shortcut::new(Modifier::CTRL, 'f')],
            description: "Open search",
            action: ShortcutAction::OpenSearch,
        },
        Binding {
            section: "Search & Queue",
            primary: Shortcut::new(Modifier::NONE, 'q'),
            alternates: vec![],
            description: "Toggle Up Next queue",
            action: ShortcutAction::ToggleQueue,
        },
        Binding {
            section: "Navigation",
            primary: Shortcut::new(Modifier::CTRL, '/'),
            alternates: vec![],
            description: "Keyboard shortcuts",
            action: ShortcutAction::ToggleShortcuts,
        },
        Binding {
            section: "Navigation",
            primary: Shortcut::new(Modifier::NONE, Named::Escape),
            alternates: vec![],
            description: "Close dialog / menu / search",
            action: ShortcutAction::Dismiss,
        },
    ]
});

/// Decode a raw key press into the [`ShortcutAction`] the table binds it to,
/// or `None` if no shortcut claims it.
///
/// Takes the whole [`keyboard::Event`] so key-repeat events can be refused.
pub fn match_event(event: &keyboard::Event) -> Option<ShortcutAction> {
    SHORTCUTS
        .iter()
        .find(|s| s.matches_event(event))
        .map(|s| s.action)
}

/// Adapt a [`Binding`] to the framework's shortcut-catalog row.
impl ShortcutCatalogRow for Binding {
    fn section(&self) -> &str {
        self.section
    }
    fn badge(&self) -> String {
        Binding::badge(self)
    }
    fn description(&self) -> &str {
        self.description
    }
}

/// The overlay's rows, derived from [`SHORTCUTS`].
pub fn overlay_entries() -> Vec<ShortcutEntry> {
    shortcut_entries(&SHORTCUTS[..])
}

#[cfg(test)]
mod badge_derivation_tests {}
