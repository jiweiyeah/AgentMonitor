use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Paragraph, Row, Table};
use ratatui::Frame;

use crate::app::App;
use crate::tui::theme;
use crate::tui::widgets::human_bytes;

pub fn render(frame: &mut Frame, area: Rect, app: &App) {
    let procs = app.metrics.snapshot();

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(4)])
        .split(area);

    // Summary strip
    let total_rss = app.metrics.total_rss_kb();
    let summary = Paragraph::new(Line::from(vec![
        Span::styled(" Live processes  ", theme::muted()),
        Span::styled(
            format!("{}", procs.len()),
            Style::default().fg(Color::White).add_modifier(Modifier::BOLD),
        ),
        Span::styled("   aggregate RSS  ", theme::muted()),
        Span::styled(
            human_bytes(total_rss),
            Style::default().fg(theme::ACCENT).add_modifier(Modifier::BOLD),
        ),
        Span::styled("   sampling @ ", theme::muted()),
        Span::raw(format!("{}s", app.config.sample_interval.as_secs())),
        Span::styled("   trend ", theme::muted()),
        Span::raw("▁▂▃▄▅▆▇█"),
        Span::styled(" (low→high RSS, newest on right)", theme::muted()),
    ]))
    .block(
        Block::default()
            .borders(Borders::ALL)
            .title(Span::styled(" Processes ", theme::title())),
    );
    frame.render_widget(summary, chunks[0]);

    if procs.is_empty() {
        let empty = Paragraph::new(Line::from(Span::styled(
            "No agent processes detected. Start claude / codex in another terminal.",
            theme::muted(),
        )))
        .block(Block::default().borders(Borders::ALL));
        frame.render_widget(empty, chunks[1]);
        return;
    }

    let header = Row::new(vec![
        Cell::from("Agent"),
        Cell::from("PID"),
        Cell::from("CWD"),
        Cell::from("RSS"),
        Cell::from("CPU %"),
        Cell::from("Uptime"),
        Cell::from("RSS trend"),
    ])
    .style(theme::title());

    let now_unix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let rows: Vec<Row> = procs
        .iter()
        .map(|p| {
            let uptime = now_unix.saturating_sub(p.started_unix);
            let hist = ascii_spark(&p.rss_history(), 14);
            let agent_label = app
                .adapters
                .iter()
                .find(|a| a.id() == p.agent)
                .map(|a| a.display_name())
                .unwrap_or(p.agent);
            let cwd = p.cwd.as_deref().unwrap_or("-");
            Row::new(vec![
                Cell::from(agent_label),
                Cell::from(p.pid.to_string()),
                Cell::from(shorten_path(cwd, 40)),
                Cell::from(human_bytes(p.latest_rss_kb())),
                Cell::from(format!("{:.1}", p.latest_cpu())),
                Cell::from(format_uptime(uptime)),
                Cell::from(hist),
            ])
        })
        .collect();

    let widths = [
        Constraint::Length(12),
        Constraint::Length(8),
        Constraint::Min(20),
        Constraint::Length(10),
        Constraint::Length(8),
        Constraint::Length(10),
        Constraint::Length(16),
    ];

    let table = Table::new(rows, widths)
        .header(header)
        .block(Block::default().borders(Borders::ALL));
    frame.render_widget(table, chunks[1]);
}

fn format_uptime(secs: u64) -> String {
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m{}s", secs / 60, secs % 60)
    } else if secs < 86400 {
        format!("{}h{}m", secs / 3600, (secs % 3600) / 60)
    } else {
        format!("{}d{}h", secs / 86400, (secs % 86400) / 3600)
    }
}

/// Truncate a path from the left so the trailing segments (project name) stay
/// visible — that's what users scan for when locating which workspace a PID
/// belongs to.
fn shorten_path(s: &str, max: usize) -> String {
    let count = s.chars().count();
    if count <= max || max == 0 {
        return s.to_string();
    }
    let take = max.saturating_sub(1);
    let tail: String = s.chars().skip(count - take).collect();
    format!("…{tail}")
}

/// Unicode block sparkline over the most recent `max_width` RSS samples.
/// Flattens to a low bar when variance is below ~5% of peak so minor sampling
/// jitter isn't rendered as a dramatic ridge.
fn ascii_spark(values: &[u64], max_width: usize) -> String {
    if values.is_empty() || max_width == 0 {
        return String::new();
    }
    const BARS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    let start = values.len().saturating_sub(max_width);
    let slice = &values[start..];
    let max = *slice.iter().max().unwrap_or(&0);
    let min = *slice.iter().min().unwrap_or(&0);
    let spread = max.saturating_sub(min);
    if max == 0 || spread.saturating_mul(20) < max {
        return BARS[1].to_string().repeat(slice.len());
    }
    let range = spread as f64;
    slice
        .iter()
        .map(|v| {
            let norm = (*v - min) as f64 / range;
            let idx = (norm * (BARS.len() - 1) as f64).round() as usize;
            BARS[idx.min(BARS.len() - 1)]
        })
        .collect()
}
