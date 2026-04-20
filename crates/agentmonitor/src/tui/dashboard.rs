use std::time::{SystemTime, UNIX_EPOCH};

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Sparkline};
use ratatui::Frame;

use crate::adapter::types::agent_display_name;
use crate::app::App;
use crate::tui::stats::{
    activity_buckets, aggregate_rss_buckets, tokens_by_agent, top_projects, AgentTokenRow,
    ProjectRow,
};
use crate::tui::theme;
use crate::tui::widgets::{ascii_spark, human_bytes};

pub fn render(frame: &mut Frame, area: Rect, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(5),  // Overview
            Constraint::Min(6),     // Activity + Top projects row
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
    render_activity(frame, middle[0], &hist);

    // Cap the list at what fits in the pane so the block never overflows.
    let top_n = middle[1].height.saturating_sub(3) as usize;
    let projects = top_projects(&sessions, top_n.max(1));
    render_top_projects(frame, middle[1], &projects, now);

    render_tokens_by_agent(frame, chunks[2], &agent_rows);
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
            Span::styled("By agent  ", theme::muted()),
            Span::raw(agent_summary),
            Span::styled("   Live  ", theme::muted()),
            bold(format!("{}", d.live_pids)),
            Span::styled(" / ", theme::muted()),
            Span::styled(
                human_bytes(d.total_rss_kb),
                Style::default()
                    .fg(theme::ACCENT)
                    .add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::from(vec![
            Span::styled("Sampling  ", theme::muted()),
            Span::raw(format!("{}s", d.sample_interval_secs)),
            Span::styled("   RSS trend  ", theme::muted()),
            Span::styled(
                ascii_spark(d.rss_trend, 20),
                Style::default().fg(theme::ACCENT),
            ),
        ]),
    ];

    let widget = Paragraph::new(lines).block(
        Block::default()
            .borders(Borders::ALL)
            .title(Span::styled(" Overview ", theme::title())),
    );
    frame.render_widget(widget, area);
}

fn render_activity(frame: &mut Frame, area: Rect, hist: &[u64]) {
    // Render order: block + sparkline inside + axis caption below (inside
    // block). We can't compose directly, so allocate the inner area manually.
    let block = Block::default()
        .borders(Borders::ALL)
        .title(Span::styled(" 24h Activity ", theme::title()));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if inner.height == 0 || inner.width == 0 {
        return;
    }
    let spark_area = Rect {
        x: inner.x,
        y: inner.y,
        width: inner.width,
        height: inner.height.saturating_sub(1).max(1),
    };
    let sparkline = Sparkline::default()
        .data(hist)
        .style(Style::default().fg(theme::ACCENT));
    frame.render_widget(sparkline, spark_area);

    if inner.height >= 2 {
        let caption_area = Rect {
            x: inner.x,
            y: inner.y + inner.height - 1,
            width: inner.width,
            height: 1,
        };
        let total: u64 = hist.iter().sum();
        let caption = Line::from(vec![
            Span::styled("-24h", theme::muted()),
            Span::raw(" "),
            Span::styled(
                format!("Σ {total} sessions"),
                theme::muted(),
            ),
            Span::styled(format!("{:>width$}", "now", width = 8), theme::muted()),
        ]);
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
