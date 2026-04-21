//! Terminal styles. The accent is read live from `settings::get()` so a
//! theme change on the Settings tab propagates on the next redraw without
//! restarting the app. Non-accent colors (success/warn/muted) stay static —
//! their semantic meaning is terminal-universal and the extra knob would only
//! hurt readability.
//!
//! Backwards compat: the legacy `theme::ACCENT` constant was inlined at dozens
//! of call sites. The dynamic `accent()` function mirrors that usage so the
//! call sites only need a `()` — not a settings handle — and the theme tab can
//! swap the color wheel beneath them.

use ratatui::style::{Color, Modifier, Style};

use crate::settings::get;

/// Fixed palette. Centralized so ad-hoc `Color::Green` literals elsewhere can
/// be migrated here over time without chasing the file list.
pub const SUCCESS: Color = Color::Green;
pub const WARN: Color = Color::Yellow;
pub const MUTED: Color = Color::DarkGray;

/// Live accent color. Reads from the global settings each call — a read lock
/// is acquired per call but settings reads are contention-free under typical
/// TUI use (writer count ≈ user's edit rate; reader count ≈ spans per frame).
pub fn accent() -> Color {
    get().theme.to_color()
}

pub fn title() -> Style {
    Style::default().fg(accent()).add_modifier(Modifier::BOLD)
}
pub fn muted() -> Style {
    Style::default().fg(MUTED)
}
pub fn selected() -> Style {
    Style::default()
        .bg(Color::DarkGray)
        .add_modifier(Modifier::BOLD)
}
pub fn active_tab() -> Style {
    Style::default()
        .fg(Color::Black)
        .bg(accent())
        .add_modifier(Modifier::BOLD)
}
pub fn status_active() -> Style {
    Style::default().fg(SUCCESS).add_modifier(Modifier::BOLD)
}
pub fn status_idle() -> Style {
    Style::default().fg(WARN)
}
pub fn status_done() -> Style {
    Style::default().fg(MUTED)
}
