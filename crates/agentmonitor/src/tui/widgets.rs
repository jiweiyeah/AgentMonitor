//! Reusable widgets.

use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Sparkline;

use crate::adapter::types::SessionStatus;
use crate::tui::theme;

pub fn status_span(status: SessionStatus) -> Span<'static> {
    let (label, style) = match status {
        SessionStatus::Active => ("● active", theme::status_active()),
        SessionStatus::Idle => ("◐ idle", theme::status_idle()),
        SessionStatus::Completed => ("○ done", theme::status_done()),
        SessionStatus::Unknown => ("○ ?", theme::status_done()),
    };
    Span::styled(label, style)
}

pub fn rss_spark(history: &[u64], style: Style) -> Sparkline<'_> {
    Sparkline::default().data(history).style(style)
}

pub fn human_bytes(bytes_kb: u64) -> String {
    if bytes_kb < 1024 {
        format!("{bytes_kb} KB")
    } else if bytes_kb < 1024 * 1024 {
        format!("{:.1} MB", bytes_kb as f64 / 1024.0)
    } else {
        format!("{:.2} GB", bytes_kb as f64 / (1024.0 * 1024.0))
    }
}

pub fn token_bar_line(
    input: u64,
    cache_read: u64,
    cache_create: u64,
    output: u64,
) -> Line<'static> {
    let total = input + cache_read + cache_create + output;
    let label = format!(
        "in {input}  cache_r {cache_read}  cache_w {cache_create}  out {output}  Σ {total}"
    );
    Line::from(Span::styled(label, theme::muted()))
}
