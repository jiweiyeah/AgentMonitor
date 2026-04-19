use ratatui::style::{Color, Modifier, Style};

pub const ACCENT: Color = Color::Cyan;
pub const SUCCESS: Color = Color::Green;
pub const WARN: Color = Color::Yellow;
pub const MUTED: Color = Color::DarkGray;

pub fn title() -> Style {
    Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)
}
pub fn muted() -> Style {
    Style::default().fg(MUTED)
}
pub fn selected() -> Style {
    Style::default().bg(Color::DarkGray).add_modifier(Modifier::BOLD)
}
pub fn active_tab() -> Style {
    Style::default().fg(Color::Black).bg(ACCENT).add_modifier(Modifier::BOLD)
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
