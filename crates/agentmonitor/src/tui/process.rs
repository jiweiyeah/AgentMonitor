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
        Cell::from("RSS"),
        Cell::from("CPU %"),
        Cell::from("Uptime"),
        Cell::from("History"),
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
            let hist = ascii_spark(&p.rss_history());
            Row::new(vec![
                Cell::from(p.agent),
                Cell::from(p.pid.to_string()),
                Cell::from(human_bytes(p.latest_rss_kb())),
                Cell::from(format!("{:.1}", p.latest_cpu())),
                Cell::from(format_uptime(uptime)),
                Cell::from(hist),
            ])
        })
        .collect();

    let widths = [
        Constraint::Length(8),
        Constraint::Length(8),
        Constraint::Length(10),
        Constraint::Length(8),
        Constraint::Length(10),
        Constraint::Min(10),
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

/// Unicode block spark — good enough without pulling in the Sparkline widget
/// for the table cell (which only takes a Cell<'_>).
fn ascii_spark(values: &[u64]) -> String {
    if values.is_empty() {
        return String::new();
    }
    const BARS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    let max = *values.iter().max().unwrap_or(&1).max(&1);
    let min = *values.iter().min().unwrap_or(&0);
    let range = (max - min).max(1) as f64;
    values
        .iter()
        .map(|v| {
            let norm = (*v - min) as f64 / range;
            let idx = (norm * (BARS.len() - 1) as f64).round() as usize;
            BARS[idx.min(BARS.len() - 1)]
        })
        .collect()
}
