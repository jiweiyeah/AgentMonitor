use std::time::{SystemTime, UNIX_EPOCH};

use chrono::{DateTime, Timelike, Utc};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;

use crate::adapter::types::agent_display_name;
use crate::app::App;
use crate::tui::stats::{
    activity_buckets, aggregate_rss_buckets, tokens_by_agent, top_projects, AgentTokenRow,
    ProjectRow,
};
use crate::tui::theme;
use crate::tui::widgets::{braille_spark, human_bytes, trend_arrow};

pub fn render(frame: &mut Frame, area: Rect, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(5),                             // Overview
            Constraint::Min(6),                                // Activity + Top projects row
            Constraint::Min(4),                                // Live Processes table
            Constraint::Length(4 + app.adapters.len() as u16), // Tokens-by-agent strip
        ])
        .split(area);

    let state = app.state.read();
    let sessions = state.sessions.clone();
    drop(state);

    let now = chrono::Utc::now();
    let now_unix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let last24h = sessions
        .iter()
        .filter(|s| {
            s.updated_at
                .map(|t| (now - t).num_hours() < 24)
                .unwrap_or(false)
        })
        .count();

    let agent_ids: Vec<&'static str> = app.adapters.iter().map(|a| a.id()).collect();
    let agent_rows = tokens_by_agent(&sessions, &agent_ids);
    let total_tokens: u64 = agent_rows.iter().map(|r| r.tokens.total()).sum();

    let procs = app.metrics.snapshot();
    let total_rss = app.metrics.total_rss_kb();

    // 20 buckets of sample_interval size gives ~40s of trend at the default 2s
    // cadence — short enough to reflect a fresh tab switch, long enough to show
    // real ramp-up when a `claude` session starts.
    let rss_trend = aggregate_rss_buckets(
        &app.metrics,
        now_unix,
        app.config.sample_interval.as_secs().max(1),
        20,
    );

    render_overview(
        frame,
        chunks[0],
        OverviewData {
            total_sessions: sessions.len(),
            last24h,
            agent_rows: &agent_rows,
            total_tokens,
            live_pids: procs.len(),
            total_rss_kb: total_rss,
            sample_interval_secs: app.config.sample_interval.as_secs(),
            rss_trend: &rss_trend,
        },
    );

    let middle = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(45), Constraint::Percentage(55)])
        .split(chunks[1]);

    let hist = activity_buckets(&sessions, now, 24);
    render_activity(frame, middle[0], &hist, now);

    // Cap the list at what fits in the pane so the block never overflows.
    let top_n = middle[1].height.saturating_sub(3) as usize;
    let projects = top_projects(&sessions, top_n.max(1));
    render_top_projects(frame, middle[1], &projects, now);

    crate::tui::process::render(frame, chunks[2], app);

    render_tokens_by_agent(frame, chunks[3], &agent_rows);
}

struct OverviewData<'a> {
    total_sessions: usize,
    last24h: usize,
    agent_rows: &'a [AgentTokenRow],
    total_tokens: u64,
    live_pids: usize,
    total_rss_kb: u64,
    sample_interval_secs: u64,
    rss_trend: &'a [u64],
}

fn render_overview(frame: &mut Frame, area: Rect, d: OverviewData<'_>) {
    let agent_summary = if d.agent_rows.is_empty() {
        "-".to_string()
    } else {
        d.agent_rows
            .iter()
            .map(|r| format!("{}={}", agent_display_name(r.agent), r.sessions))
            .collect::<Vec<_>>()
            .join(" ")
    };

    let lines = vec![
        Line::from(vec![
            Span::styled("Sessions  ", theme::muted()),
            bold(format!("{}", d.total_sessions)),
            Span::styled("   last24h  ", theme::muted()),
            Span::styled(
                format!("{}", d.last24h),
                Style::default()
                    .fg(theme::SUCCESS)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled("   Σ tokens  ", theme::muted()),
            Span::styled(
                format_token_count(d.total_tokens),
                Style::default()
                    .fg(theme::ACCENT)
                    .add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::from(vec![
            Span::styled("Agents    ", theme::muted()),
            Span::raw(agent_summary),
        ]),
        Line::from(process_row_spans(&d)),
    ];

    let widget = Paragraph::new(lines).block(
        Block::default()
            .borders(Borders::ALL)
            .title(Span::styled(" Overview ", theme::title())),
    );
    frame.render_widget(widget, area);
}

/// Third Overview row — process count, current RSS, direction + spark, then
/// the trend window's range and sample cadence. Split out so the list of spans
/// stays readable; there are otherwise 10+ pieces on one line.
fn process_row_spans(d: &OverviewData<'_>) -> Vec<Span<'static>> {
    let accent_bold = Style::default()
        .fg(theme::ACCENT)
        .add_modifier(Modifier::BOLD);
    vec![
        Span::styled("Process   ", theme::muted()),
        bold(format!("{} live", d.live_pids)),
        Span::styled(" · ", theme::muted()),
        Span::styled(human_bytes(d.total_rss_kb), accent_bold),
        Span::raw(" "),
        Span::styled(trend_arrow(d.rss_trend), theme::muted()),
        Span::raw("  "),
        Span::styled(
            braille_spark(d.rss_trend, 20),
            Style::default().fg(theme::ACCENT),
        ),
        Span::raw("  "),
        Span::styled(
            trend_footnote(d.rss_trend, d.sample_interval_secs),
            theme::muted(),
        ),
    ]
}

/// Small meta label shown at the end of the Process row: `(429-468 MB, 2s)`
/// when the trend window has meaningful spread, otherwise just `(2s)` so we
/// don't render `(468-468 MB, 2s)` when everything is flat.
fn trend_footnote(trend: &[u64], sample_secs: u64) -> String {
    let non_zero: Vec<u64> = trend.iter().copied().filter(|&v| v > 0).collect();
    match (non_zero.iter().min(), non_zero.iter().max()) {
        (Some(&min), Some(&max)) if max.saturating_sub(min).saturating_mul(20) >= max => {
            format!("({}, {}s)", format_range(min, max), sample_secs)
        }
        _ => format!("({}s)", sample_secs),
    }
}

/// Format two byte counts as a single collapsed range: when both fall in the
/// same unit (both MB, both GB, …) we show `429-468 MB`; otherwise fall back
/// to `min-max` with each side carrying its own unit.
fn format_range(min_kb: u64, max_kb: u64) -> String {
    let min_s = human_bytes(min_kb);
    let max_s = human_bytes(max_kb);
    let min_unit = min_s.rsplit(' ').next().unwrap_or("");
    let max_unit = max_s.rsplit(' ').next().unwrap_or("");
    if min_unit == max_unit && !min_unit.is_empty() {
        let min_num = min_s.split_whitespace().next().unwrap_or("0");
        format!("{min_num}-{max_s}")
    } else {
        format!("{min_s}-{max_s}")
    }
}

fn render_activity(frame: &mut Frame, area: Rect, hist: &[u64], now: DateTime<Utc>) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title(Span::styled(" 24h Activity ", theme::title()));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if inner.height == 0 || inner.width == 0 || hist.is_empty() {
        return;
    }

    // Row budget: bars | ticks | caption. Collapse from the bottom up when
    // the pane is short.
    let caption_h: u16 = if inner.height >= 2 { 1 } else { 0 };
    let tick_h: u16 = if inner.height >= 4 { 1 } else { 0 };
    let bars_h: u16 = inner.height.saturating_sub(caption_h + tick_h).max(1);

    let n = hist.len();
    let col_w = ((inner.width as usize) / n.max(1)).max(1);
    // Reserve the last column of each slot as a gap so adjacent bars stay
    // distinguishable and tick labels have breathing room.
    let bar_w: usize = if col_w >= 2 { col_w - 1 } else { 1 };

    let max_val = hist.iter().copied().max().unwrap_or(0);

    // Fractional-height block glyphs (U+2581..U+2588) for eighths resolution.
    const BLOCKS: [&str; 9] = [" ", "▁", "▂", "▃", "▄", "▅", "▆", "▇", "█"];

    {
        let buf = frame.buffer_mut();
        for (i, &v) in hist.iter().enumerate() {
            let x0 = inner.x + (i * col_w) as u16;
            if x0 >= inner.x + inner.width {
                break;
            }
            let total_eighths = bars_h as u64 * 8;
            let h_eighths = if max_val == 0 {
                0
            } else {
                v.saturating_mul(total_eighths) / max_val
            };
            let is_empty = v == 0;

            for r in 0..bars_h {
                let row_from_bottom = (bars_h - 1 - r) as u64;
                let lower_edge = row_from_bottom * 8;
                let fill = h_eighths.saturating_sub(lower_edge).min(8) as usize;
                let y = inner.y + r;
                let is_baseline = r == bars_h - 1;
                for dx in 0..bar_w as u16 {
                    let x = x0 + dx;
                    if x >= inner.x + inner.width {
                        break;
                    }
                    let Some(cell) = buf.cell_mut((x, y)) else {
                        continue;
                    };
                    if fill > 0 {
                        cell.set_symbol(BLOCKS[fill])
                            .set_style(Style::default().fg(theme::ACCENT));
                    } else if is_empty && is_baseline && dx == (bar_w / 2) as u16 {
                        // Dim baseline marker keeps empty hours anchored under
                        // their tick so sparse days remain readable.
                        cell.set_symbol("·")
                            .set_style(Style::default().fg(theme::MUTED));
                    }
                }
            }
        }
    }

    if tick_h > 0 {
        let tick_y = inner.y + bars_h;
        let now_hour = now.with_timezone(&chrono::Local).hour();
        let mut spans: Vec<Span<'static>> = Vec::new();
        let mut cursor: usize = 0;
        for i in 0..n {
            let is_edge_right = i == n - 1;
            let is_major = i % 6 == 0 || is_edge_right;
            if !is_major {
                continue;
            }
            let hours_ago = (n - 1 - i) as u32;
            let label = if is_edge_right {
                "now".to_string()
            } else {
                let hr = (now_hour + 24 - hours_ago % 24) % 24;
                format!("{hr:02}h")
            };
            let target_x = i * col_w;
            // Right-anchor "now" so its last char sits in the final bucket slot.
            let start = if is_edge_right {
                target_x + bar_w.saturating_sub(label.chars().count())
            } else {
                target_x
            };
            if start > cursor {
                spans.push(Span::raw(" ".repeat(start - cursor)));
                cursor = start;
            }
            cursor += label.chars().count();
            spans.push(Span::styled(label, theme::muted()));
        }
        let tick_area = Rect {
            x: inner.x,
            y: tick_y,
            width: inner.width,
            height: 1,
        };
        frame.render_widget(Paragraph::new(Line::from(spans)), tick_area);
    }

    if caption_h > 0 {
        let caption_area = Rect {
            x: inner.x,
            y: inner.y + bars_h + tick_h,
            width: inner.width,
            height: 1,
        };
        let total: u64 = hist.iter().sum();
        let caption = Line::from(vec![Span::styled(
            format!("Σ {total} sessions"),
            theme::muted(),
        )]);
        frame.render_widget(Paragraph::new(caption), caption_area);
    }
}

fn render_top_projects(
    frame: &mut Frame,
    area: Rect,
    rows: &[ProjectRow],
    now: chrono::DateTime<chrono::Utc>,
) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title(Span::styled(" Top Projects ", theme::title()));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if inner.height == 0 {
        return;
    }

    if rows.is_empty() {
        let hint = Paragraph::new(Line::from(Span::styled(
            "No sessions yet — start `claude` or `codex` in a project.",
            theme::muted(),
        )));
        frame.render_widget(hint, inner);
        return;
    }

    // Name col = inner width - count(4) - age(6) - two spaces(2).
    let name_width = (inner.width as usize).saturating_sub(12).max(10);
    let lines: Vec<Line> = rows
        .iter()
        .map(|r| {
            Line::from(vec![
                Span::styled(
                    shorten_tail(&r.cwd, name_width),
                    Style::default().fg(Color::White),
                ),
                Span::raw(" "),
                Span::styled(format!("{:>3}", r.count), theme::muted()),
                Span::raw(" "),
                Span::styled(
                    r.latest
                        .map(|t| humanize_age(now - t))
                        .unwrap_or_else(|| "-".into()),
                    Style::default().fg(theme::SUCCESS),
                ),
            ])
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), inner);
}

fn render_tokens_by_agent(frame: &mut Frame, area: Rect, rows: &[AgentTokenRow]) {
    let mut lines: Vec<Line> = Vec::with_capacity(rows.len() + 1);
    lines.push(Line::from(vec![
        Span::styled(
            format!("{:<12} {:>4}  ", "Agent", "Ses"),
            theme::muted(),
        ),
        Span::styled(
            format!("{:>9}  {:>9}  {:>9}  {:>9}  {:>9}", "input", "output", "cache_r", "cache_w", "Σ"),
            theme::muted(),
        ),
    ]));
    for r in rows {
        let t = &r.tokens;
        lines.push(Line::from(vec![
            Span::styled(
                format!("{:<12} ", agent_display_name(r.agent)),
                Style::default()
                    .fg(theme::ACCENT)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(format!("{:>4}  ", r.sessions), Style::default().fg(Color::White)),
            Span::raw(format!(
                "{:>9}  {:>9}  {:>9}  {:>9}  ",
                format_token_count(t.input),
                format_token_count(t.output),
                format_token_count(t.cache_read),
                format_token_count(t.cache_creation),
            )),
            Span::styled(
                format!("{:>9}", format_token_count(t.total())),
                Style::default()
                    .fg(theme::ACCENT)
                    .add_modifier(Modifier::BOLD),
            ),
        ]));
    }

    let widget = Paragraph::new(lines).block(
        Block::default()
            .borders(Borders::ALL)
            .title(Span::styled(" Tokens by agent ", theme::title())),
    );
    frame.render_widget(widget, area);
}

fn bold(text: String) -> Span<'static> {
    Span::styled(
        text,
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD),
    )
}

fn format_token_count(n: u64) -> String {
    if n < 1000 {
        format!("{n}")
    } else if n < 1_000_000 {
        format!("{:.1}K", n as f64 / 1_000.0)
    } else if n < 1_000_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else {
        format!("{:.2}B", n as f64 / 1_000_000_000.0)
    }
}

fn humanize_age(d: chrono::Duration) -> String {
    let s = d.num_seconds();
    if s < 60 {
        format!("{s}s")
    } else if s < 3600 {
        format!("{}m", s / 60)
    } else if s < 86400 {
        format!("{}h", s / 3600)
    } else {
        format!("{}d", s / 86400)
    }
}

/// Truncate from the left so the trailing path segment (usually the repo dir)
/// stays visible — the prefix is lower-signal.
fn shorten_tail(s: &str, max: usize) -> String {
    if s.chars().count() <= max || max == 0 {
        return format!("{:<width$}", s, width = max);
    }
    let take = max.saturating_sub(1);
    let tail: String = s.chars().skip(s.chars().count() - take).collect();
    format!("…{tail}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_range_same_unit_collapses() {
        // Both in MB → show `429-468 MB`.
        assert_eq!(format_range(429 * 1024, 468 * 1024), "429.0-468.0 MB");
    }

    #[test]
    fn format_range_mixed_units_keeps_both() {
        // min in MB, max in GB → carry units on both sides.
        let min_kb = 900 * 1024; // 900 MB
        let max_kb = 3 * 1024 * 1024; // 3 GB
        let s = format_range(min_kb, max_kb);
        assert!(s.contains("MB-"), "expected MB prefix, got {s}");
        assert!(s.ends_with(" GB"), "expected GB suffix, got {s}");
    }

    #[test]
    fn trend_footnote_flat_drops_range() {
        // All flat → only the sampling cadence is shown.
        let trend = vec![468u64 * 1024; 10];
        assert_eq!(trend_footnote(&trend, 2), "(2s)");
    }

    #[test]
    fn trend_footnote_spread_shows_range_and_sampling() {
        // 429..468 MB spread > 5 % → both pieces shown.
        let mut trend = vec![429u64 * 1024; 5];
        trend.extend(vec![468u64 * 1024; 5]);
        let s = trend_footnote(&trend, 2);
        assert!(s.contains("429"), "missing min in {s}");
        assert!(s.contains("468"), "missing max in {s}");
        assert!(s.ends_with(", 2s)"), "missing sampling in {s}");
    }

    #[test]
    fn trend_footnote_all_zero_still_shows_sampling() {
        // No process data yet → keep user oriented on the sampling cadence.
        assert_eq!(trend_footnote(&[0, 0, 0], 5), "(5s)");
    }
}
