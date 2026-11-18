//! Theme-derived colour helpers.
//!
//! Every colour comes from the framework `Palette` on [`EnzTheme`]; this module
//! holds no colour literals, so palette changes reach every call site.

use enzui_theme::EnzTheme;
use iced::Color;

// Shared theme source selection and OS routing live in the framework
// `ThemeIntegration` owned by `MusicApp`.

/// Primary text colour.
pub fn text_color(theme: &EnzTheme) -> Color {
    theme.palette.text
}

/// The danger / error foreground, e.g. the failed podcast-feed caption.
pub fn danger_color(theme: &EnzTheme) -> Color {
    theme.palette.danger
}

/// Secondary (muted) text colour.
pub fn secondary_text_color(theme: &EnzTheme) -> Color {
    theme.palette.text_muted
}

/// Alternate surface — one step off the base (sidebar, panels).
pub fn surface_alt_color(theme: &EnzTheme) -> Color {
    theme.palette.surface_alt
}

/// Accent colour (active nav, playing track, primary affordances).
pub fn accent_color(theme: &EnzTheme) -> Color {
    theme.palette.accent
}

/// Neutral hover overlay, exactly `Palette::overlay_hover()`.
pub fn overlay_hover_color(theme: &EnzTheme) -> Color {
    theme.palette.overlay_hover()
}
