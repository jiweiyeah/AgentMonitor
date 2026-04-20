use std::time::{SystemTime, UNIX_EPOCH};

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Paragraph, Row, Table, TableState};
use ratatui::Frame;

use crate::app::App;
use crate::tui::stats::aggregate_rss_buckets;
use crate::tui::theme;
use crate::tui::widgets::{ascii_spark, braille_spark, human_bytes, trend_arrow};

pub fn render(frame: &mut Frame, area: Rect, app: &App) {
    let procs = app.metrics.snapshot();

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(4), Constraint::Min(4)])
        .split(area);

    render_summary(frame, chunks[0], app, procs.len());

    if procs.is_empty() {
        let empty = Paragraph::new(Line::from(Span::styled(
            "No agent processes detected. Start claude / codex in another terminal.",
            theme::muted(),
        )))
        .block(Block::default().borders(Borders::ALL));
        frame.render_widget(empty, chunks[1]);
        return;
    }

    render_table(frame, chunks[1], app, &procs);
}

fn render_summary(frame: &mut Frame, area: Rect, app: &App, live_count: usize) {
    let total_rss = app.metrics.total_rss_kb();
    let now_unix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let bucket_secs = app.config.sample_interval.as_secs().max(1);

    // Width of the inline sparkline — fill whatever space the block has left
    // after the count/RSS spans. Anything from ~20 to ~60 columns works well.
    let spark_width = area
        .width
        .saturating_sub(44)
        .clamp(16, 80) as usize;
    let rss_trend = aggregate_rss_buckets(&app.metrics, now_unix, bucket_secs, spark_width);
    let window_secs = bucket_secs * spark_width as u64;

    let lines = vec![
        Line::from(vec![
            Span::styled(" Live ", theme::muted()),
            Span::styled(
                format!("{live_count}"),
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled("   Σ RSS ", theme::muted()),
            Span::styled(
                human_bytes(total_rss),
                Style::default()
                    .fg(theme::ACCENT)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled("   Sampling ", theme::muted()),
            Span::raw(format!("{}s", bucket_secs)),
        ]),
        Line::from(vec![
            Span::styled(" RSS trend ", theme::muted()),
            Span::styled(trend_arrow(&rss_trend), theme::muted()),
            Span::raw(" "),
            Span::styled(
                braille_spark(&rss_trend, spark_width),
                Style::default().fg(theme::ACCENT),
            ),
            Span::styled(
                format!("  (last {})", format_window(window_secs)),
                theme::muted(),
            ),
        ]),
    ];

    let summary = Paragraph::new(lines).block(
        Block::default()
            .borders(Borders::ALL)
            .title(Span::styled(" Processes ", theme::title())),
    );
    frame.render_widget(summary, area);
}

fn render_table(
    frame: &mut Frame,
    area: Rect,
    app: &App,
    procs: &[crate::collector::metrics::ProcessEntry],
) {
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

    let now_unix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
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

    // Clamp to the current process count so shrinking doesn't leave the
    // highlight dangling past the last row.
    let selected = app.selected_process.min(procs.len().saturating_sub(1));
    let mut table_state = TableState::default();
    table_state.select(Some(selected));

    let table = Table::new(rows, widths)
        .header(header)
        .block(Block::default().borders(Borders::ALL))
        .row_highlight_style(theme::selected())
        .highlight_symbol("▶ ");
    frame.render_stateful_widget(table, area, &mut table_state);
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

fn format_window(secs: u64) -> String {
    if secs < 120 {
        format!("{secs}s")
    } else if secs < 7200 {
        format!("{}m", secs / 60)
    } else {
        format!("{}h", secs / 3600)
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
