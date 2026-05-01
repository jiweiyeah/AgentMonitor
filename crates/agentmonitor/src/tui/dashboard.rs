use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use chrono::{DateTime, Timelike, Utc};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;
use unicode_width::UnicodeWidthStr;

use crate::adapter::types::agent_display_name;
use crate::adapter::types::SessionMeta;
use crate::app::App;
use crate::i18n::t;
use crate::settings::{self, TokenUnit};
use crate::tui::stats::{
    activity_buckets, aggregate_rss_buckets, tokens_by_agent, top_projects, AgentTokenRow,
    ProjectRow,
};
use crate::tui::theme;
use crate::tui::widgets::{braille_spark, human_bytes, pad_display_width, trend_arrow};
use crate::tui::{DASHBOARD_HSTACK_MIN_WIDTH, DASHBOARD_TOKENS_STRIP_MIN_HEIGHT};

/// Pure functions of `sessions` cached so the renderer can skip recomputing
/// them between non-mutation `dirty` notifies. Process-sampler ticks fire 1-2
/// times per second; without this cache, every tick walks all sessions five
/// times (once each for last24h, tokens_by_agent, aggregate_cost, top_projects,
/// activity_buckets).
///
/// Invalidation: keyed by `AppState.session_generation` plus the bool that
/// flips with the user's "include cache tokens in Σ" preference. The
/// generation counter is bumped on every `mutate_sessions` call (and on direct
/// `s.sessions = ...` assignments in collectors). We deliberately don't use
/// `Arc::as_ptr(&sessions)` because `Arc::make_mut` only allocates a new Arc
/// when there are *other* holders; an in-place mutation by the sole owner
/// silently reuses the same pointer and would slip past pointer-equality.
#[derive(Debug, Clone)]
struct DashboardAggregates {
    generation: u64,
    include_cache: bool,
    last24h: usize,
    agent_rows: Vec<AgentTokenRow>,
    total_tokens: u64,
    total_cost: f64,
    activity_24h: Vec<u64>,
    /// Capped at 100 in the cache so we don't invalidate just because the
    /// terminal grew taller; the renderer takes only as many rows as it has
    /// space for.
    top_projects: Vec<ProjectRow>,
    /// Time the cache was built — used to invalidate after 60 s elapsed so
    /// the time-relative `last24h` count rolls forward even when sessions
    /// don't mutate (e.g. an idle dashboard).
    computed_at: SystemTime,
}

fn aggregate_cache() -> &'static Mutex<Option<DashboardAggregates>> {
    static CELL: OnceLock<Mutex<Option<DashboardAggregates>>> = OnceLock::new();
    CELL.get_or_init(|| Mutex::new(None))
}

/// Return cached aggregates if `session_generation` and the include_cache
/// preference haven't changed and the cache isn't more than 60 s stale.
/// Otherwise compute fresh, store, return.
fn aggregates_for(
    sessions: &std::sync::Arc<Vec<SessionMeta>>,
    generation: u64,
    include_cache: bool,
    agent_ids: &[&'static str],
    now: DateTime<Utc>,
) -> DashboardAggregates {
    {
        let cache = aggregate_cache().lock().expect("aggregate cache poisoned");
        if let Some(cached) = cache.as_ref() {
            let stale = cached
                .computed_at
                .elapsed()
                .map(|d| d.as_secs() >= 60)
                .unwrap_or(true);
            if cached.generation == generation
                && cached.include_cache == include_cache
                && !stale
            {
                return cached.clone();
            }
        }
    }

    let last24h = sessions
        .iter()
        .filter(|s| {
            s.updated_at
                .map(|t| (now - t).num_hours() < 24)
                .unwrap_or(false)
        })
        .count();
    let agent_rows = tokens_by_agent(sessions, agent_ids);
    let total_tokens: u64 = agent_rows
        .iter()
        .map(|r| r.tokens.total_with_preference(include_cache))
        .sum();
    let total_cost = crate::pricing::aggregate_cost(sessions);
    let activity_24h = activity_buckets(sessions, now, 24);
    let top_projects = top_projects(sessions, 100);

    let fresh = DashboardAggregates {
        generation,
        include_cache,
        last24h,
        agent_rows,
        total_tokens,
        total_cost,
        activity_24h,
        top_projects,
        computed_at: SystemTime::now(),
    };
    *aggregate_cache().lock().expect("aggregate cache poisoned") = Some(fresh.clone());
    fresh
}

pub fn render(frame: &mut Frame, area: Rect, app: &App) {
    let (sessions, generation) = {
        let state = app.state.read();
        (state.sessions.clone(), state.session_generation)
    };

    let now = chrono::Utc::now();
    let now_unix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let agent_ids: Vec<&'static str> = app.adapters.iter().map(|a| a.id()).collect();
    let include_cache = settings::get().include_cache_in_total;
    let agg = aggregates_for(&sessions, generation, include_cache, &agent_ids, now);
    let last24h = agg.last24h;
    let agent_rows = agg.agent_rows;
    let total_tokens = agg.total_tokens;
    let total_cost = agg.total_cost;

    // Token trend buckets: 30 buckets of 1 minute = ~30 minutes of history,
    // matching the rate caption window. The TokenTrend lives in App so it
    // accumulates across renders without being re-derived from scratch.
    let now_systime = SystemTime::now();
    let trend_window = std::time::Duration::from_secs(60 * 30);
    let token_buckets = app.token_trend.buckets(
        now_systime,
        std::time::Duration::from_secs(60),
        30,
    );
    let token_window_total = app.token_trend.rate_in_window(now_systime, trend_window);
    let has_trend = token_buckets.iter().any(|&v| v > 0);

    // Overview height grows with the number of agent lines actually needed
    // for this terminal width — one row per pack-line plus header/process/borders.
    let overview_height = compute_overview_height(area.width, &agent_rows, has_trend);

    // Responsive: short terminals drop the Tokens-by-agent strip from the
    // bottom. Its detail (input/output/cache_r/cache_w split) is "advanced"
    // info; per-agent counts live in the Overview row, and Σ tokens lives
    // there too. Restoring it costs the user one keystroke (bigger window).
    let show_tokens_strip = area.height >= DASHBOARD_TOKENS_STRIP_MIN_HEIGHT;
    let mut constraints: Vec<Constraint> = vec![
        Constraint::Length(overview_height), // Overview (responsive)
        Constraint::Min(6),                  // Activity + Top projects row
        Constraint::Min(4),                  // Live Processes table
    ];
    if show_tokens_strip {
        constraints.push(Constraint::Length(4 + app.adapters.len() as u16));
    }
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(area);

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
            total_cost,
            live_pids: procs.len(),
            total_rss_kb: total_rss,
            sample_interval_secs: app.config.sample_interval.as_secs(),
            rss_trend: &rss_trend,
            token_trend: &token_buckets,
            token_window_total,
            token_window_label: "30m",
            token_window_minutes: 30,
        },
    );

    let middle = if chunks[1].width >= DASHBOARD_HSTACK_MIN_WIDTH {
        Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(45), Constraint::Percentage(55)])
            .split(chunks[1])
    } else {
        // Narrow terminal: side-by-side compresses both panels (cwd column
        // unreadable on Top Projects, bars too thin on Activity). Stack them
        // vertically — each gets the full inner width.
        Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
            .split(chunks[1])
    };

    // Activity buckets and top projects come from the cached aggregates.
    // Top projects was capped at 100 in the cache so we trim to what the pane
    // can actually show.
    let hist = &agg.activity_24h;
    render_activity(frame, middle[0], hist, now);

    // Cap the list at what fits in the pane so the block never overflows.
    let top_n = middle[1].height.saturating_sub(3) as usize;
    let projects: Vec<ProjectRow> = agg
        .top_projects
        .iter()
        .take(top_n.max(1))
        .cloned()
        .collect();
    let project_focused =
        app.dashboard_cursor == crate::app::DashboardCursor::Project;
    let selected_project = if project_focused {
        Some(app.selected_project.min(projects.len().saturating_sub(1)))
    } else {
        None
    };
    render_top_projects(frame, middle[1], &projects, now, selected_project);

    crate::tui::process::render(frame, chunks[2], app);

    if show_tokens_strip {
        render_tokens_by_agent(frame, chunks[3], &agent_rows);
    }
}

struct OverviewData<'a> {
    total_sessions: usize,
    last24h: usize,
    agent_rows: &'a [AgentTokenRow],
    total_tokens: u64,
    /// Aggregate USD cost across all sessions whose model is in the pricing
    /// table. Sessions with unknown models contribute 0.
    total_cost: f64,
    live_pids: usize,
    total_rss_kb: u64,
    sample_interval_secs: u64,
    rss_trend: &'a [u64],
    /// Per-minute deltas from `TokenTrend::buckets`. Empty when no trend data
    /// has been collected yet (first ~10s after launch). When non-empty,
    /// rendered as a small braille sparkline next to Σ tokens.
    token_trend: &'a [u64],
    /// Tokens added in the last `token_window_label` window. Drives the
    /// per-minute rate display.
    token_window_total: u64,
    /// Window label like "30m" / "1h" used in the rate caption.
    token_window_label: &'a str,
    /// Length of the rate window in minutes. Used to compute "X K/min".
    token_window_minutes: u64,
}

fn render_overview(frame: &mut Frame, area: Rect, d: OverviewData<'_>) {
    let label_width = 10usize;
    // Block borders eat 2 cols. Anything below that means we can't render
    // anything meaningful — bail out gracefully via the same zero-budget path.
    let inner_width = (area.width as usize).saturating_sub(2);
    let agent_budget = inner_width.saturating_sub(label_width);

    let agent_packed = build_agent_lines(d.agent_rows, agent_budget);

    let mut lines: Vec<Line> = Vec::new();

    lines.push(Line::from(vec![
        Span::styled(
            pad_display_width(t("dashboard.sessions"), label_width),
            theme::muted(),
        ),
        bold(format!("{}", d.total_sessions)),
        Span::styled(
            format!("   {}", pad_display_width(t("dashboard.last24h"), label_width)),
            theme::muted(),
        ),
        Span::styled(
            format!("{}", d.last24h),
            Style::default()
                .fg(theme::SUCCESS)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("   {}  ", t("dashboard.total_tokens")),
            theme::muted(),
        ),
        Span::styled(
            format_token_count(d.total_tokens),
            Style::default()
                .fg(theme::accent())
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("   {}  ", t("dashboard.total_cost")),
            theme::muted(),
        ),
        Span::styled(
            format_cost(d.total_cost),
            Style::default()
                .fg(theme::SUCCESS)
                .add_modifier(Modifier::BOLD),
        ),
    ]));

    // Token trend row: sparkline + "Σ rate ≈ XX K/min · 30m" caption. Only
    // shown once the trend has at least one non-zero bucket; before that the
    // line is blank to avoid implying "0 tokens/min" when the truth is "we
    // haven't measured yet".
    let any_trend = d.token_trend.iter().any(|&v| v > 0);
    if any_trend {
        // Sparkline width: pack each pair of buckets into one braille char.
        // 30 buckets → 15 chars, fits comfortably alongside the rate caption.
        let spark = braille_spark(d.token_trend, 15);
        let per_min = if d.token_window_minutes == 0 {
            0
        } else {
            d.token_window_total / d.token_window_minutes
        };
        lines.push(Line::from(vec![
            Span::styled(
                pad_display_width(t("dashboard.rate"), label_width),
                theme::muted(),
            ),
            Span::styled(
                spark,
                Style::default()
                    .fg(theme::accent())
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("  "),
            Span::styled(
                format!(
                    "≈ {}/min  ({} {})",
                    format_token_count(per_min),
                    format_token_count(d.token_window_total),
                    d.token_window_label,
                ),
                theme::muted(),
            ),
        ]));
    }

    for (i, spans) in agent_packed.into_iter().enumerate() {
        let mut line_spans: Vec<Span<'static>> = Vec::with_capacity(spans.len() + 1);
        if i == 0 {
            line_spans.push(Span::styled(
                pad_display_width(t("dashboard.agents"), label_width),
                theme::muted(),
            ));
        } else {
            // Continuation rows: blank gutter the width of the label so the
            // agent items stay vertically aligned with the first row.
            line_spans.push(Span::raw(" ".repeat(label_width)));
        }
        line_spans.extend(spans);
        lines.push(Line::from(line_spans));
    }

    lines.push(Line::from(process_row_spans(&d)));

    let widget = Paragraph::new(lines).block(
        Block::default()
            .borders(Borders::ALL)
            .title(Span::styled(t("dashboard.overview"), theme::title())),
    );
    frame.render_widget(widget, area);
}

/// Pack agent rows into one or more visual lines fitting `width_budget` cols.
/// Items use a two-space separator. When `width_budget` is too small to hold
/// even a single item, we still emit one item per line — overflow is preferred
/// to dropping data.
fn build_agent_lines(rows: &[AgentTokenRow], width_budget: usize) -> Vec<Vec<Span<'static>>> {
    if rows.is_empty() {
        return vec![vec![Span::styled("-", theme::muted())]];
    }

    const SEP: &str = "  ";
    let mut lines: Vec<Vec<Span<'static>>> = Vec::new();
    let mut current: Vec<Span<'static>> = Vec::new();
    let mut current_w: usize = 0;

    for r in rows {
        let name = agent_display_name(r.agent);
        let count = r.sessions.to_string();
        let item_w = UnicodeWidthStr::width(name) + 1 + UnicodeWidthStr::width(count.as_str());
        let need = if current.is_empty() {
            item_w
        } else {
            current_w + SEP.len() + item_w
        };
        if need > width_budget && !current.is_empty() {
            lines.push(std::mem::take(&mut current));
            current_w = 0;
        }
        if !current.is_empty() {
            current.push(Span::raw(SEP));
            current_w += SEP.len();
        }
        current.push(Span::styled(
            name.to_string(),
            Style::default().fg(Color::White),
        ));
        current.push(Span::styled("=", theme::muted()));
        current.push(Span::styled(
            count,
            Style::default()
                .fg(theme::accent())
                .add_modifier(Modifier::BOLD),
        ));
        current_w += item_w;
    }
    if !current.is_empty() {
        lines.push(current);
    }
    lines
}

/// Total height the Overview block needs at this terminal width: 2 borders +
/// 1 sessions row + (optional 1 rate row) + N agent rows + 1 process row.
/// Min 5 keeps behaviour identical to the legacy fixed layout when nothing
/// wraps and there's no trend data yet.
fn compute_overview_height(
    area_width: u16,
    rows: &[AgentTokenRow],
    has_trend: bool,
) -> u16 {
    let label_width = 10usize;
    let inner_width = (area_width as usize).saturating_sub(2);
    let budget = inner_width.saturating_sub(label_width);
    let n_agent_lines = build_agent_lines(rows, budget).len().max(1);
    let trend_row = if has_trend { 1 } else { 0 };
    (2 + 1 + trend_row + n_agent_lines + 1) as u16
}

/// Third Overview row — process count, current RSS, direction + spark, then
/// the trend window's range and sample cadence. Split out so the list of spans
/// stays readable; there are otherwise 10+ pieces on one line.
fn process_row_spans(d: &OverviewData<'_>) -> Vec<Span<'static>> {
    let accent_bold = Style::default()
        .fg(theme::accent())
        .add_modifier(Modifier::BOLD);
    vec![
        Span::styled(
            pad_display_width(t("dashboard.process"), 10),
            theme::muted(),
        ),
        bold(format!("{} {}", d.live_pids, t("dashboard.live"))),
        Span::styled(" · ", theme::muted()),
        Span::styled(human_bytes(d.total_rss_kb), accent_bold),
        Span::raw(" "),
        Span::styled(trend_arrow(d.rss_trend), theme::muted()),
        Span::raw("  "),
        Span::styled(
            braille_spark(d.rss_trend, 20),
            Style::default().fg(theme::accent()),
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
        .title(Span::styled(t("dashboard.activity"), theme::title()));
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
                            .set_style(Style::default().fg(theme::accent()));
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
            format!("Σ {total} {}", t("dashboard.sessions_sum")),
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
    selected: Option<usize>,
) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title(Span::styled(t("dashboard.top_projects"), theme::title()));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if inner.height == 0 {
        return;
    }

    if rows.is_empty() {
        let hint = Paragraph::new(Line::from(Span::styled(
            t("dashboard.no_sessions"),
            theme::muted(),
        )));
        frame.render_widget(hint, inner);
        return;
    }

    // Reserve 2 cols for the selection indicator (`▶ ` or `  `) so the
    // name column lines up regardless of which row is focused. Without the
    // reservation, the entire list would shift one column right whenever
    // the user toggles cursor onto Projects, which looks like a bug.
    let indicator_w: usize = 2;
    // Name col = inner width - indicator(2) - count(4) - age(6) - two spaces(2).
    let name_width = (inner.width as usize)
        .saturating_sub(12 + indicator_w)
        .max(10);
    let lines: Vec<Line> = rows
        .iter()
        .enumerate()
        .map(|(idx, r)| {
            let is_selected = selected == Some(idx);
            let prefix_span = if is_selected {
                Span::styled(
                    "▶ ",
                    Style::default()
                        .fg(theme::accent())
                        .add_modifier(Modifier::BOLD),
                )
            } else {
                Span::raw("  ")
            };
            let name_style = if is_selected {
                Style::default()
                    .fg(theme::accent())
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::White)
            };
            Line::from(vec![
                prefix_span,
                Span::styled(shorten_tail(&r.cwd, name_width), name_style),
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

/// Max display width across all agent names: "ClaudeDesktop" = 13 chars.
const AGENT_COL_WIDTH: usize = 14;

fn render_tokens_by_agent(frame: &mut Frame, area: Rect, rows: &[AgentTokenRow]) {
    let mut lines: Vec<Line> = Vec::with_capacity(rows.len() + 1);
    lines.push(Line::from(vec![
        Span::styled(
            format!(
                "{:<width$} {:>4}  ",
                "Agent",
                "Ses",
                width = AGENT_COL_WIDTH
            ),
            theme::muted(),
        ),
        Span::styled(
            format!(
                "{:>9}  {:>9}  {:>9}  {:>9}  {:>9}",
                "input", "output", "cache_r", "cache_w", "Σ"
            ),
            theme::muted(),
        ),
    ]));
    for r in rows {
        let tok = &r.tokens;
        lines.push(Line::from(vec![
            Span::styled(
                format!(
                    "{:<width$} ",
                    agent_display_name(r.agent),
                    width = AGENT_COL_WIDTH
                ),
                Style::default()
                    .fg(theme::accent())
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("{:>4}  ", r.sessions),
                Style::default().fg(Color::White),
            ),
            Span::raw(format!(
                "{:>9}  {:>9}  {:>9}  {:>9}  ",
                format_token_count(tok.input),
                format_token_count(tok.output),
                format_token_count(tok.cache_read),
                format_token_count(tok.cache_creation),
            )),
            Span::styled(
                format!(
                    "{:>9}",
                    format_token_count(
                        tok.total_with_preference(settings::get().include_cache_in_total)
                    )
                ),
                Style::default()
                    .fg(theme::accent())
                    .add_modifier(Modifier::BOLD),
            ),
        ]));
    }

    let widget = Paragraph::new(lines).block(
        Block::default()
            .borders(Borders::ALL)
            .title(Span::styled(t("dashboard.tokens_by_agent"), theme::title())),
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
    match settings::get().token_unit {
        TokenUnit::Raw => {
            // Thousands separators so eight-digit totals stay scannable.
            let s = n.to_string();
            let bytes = s.as_bytes();
            let mut out = String::with_capacity(s.len() + s.len() / 3);
            for (i, b) in bytes.iter().enumerate() {
                if i > 0 && (bytes.len() - i) % 3 == 0 {
                    out.push(',');
                }
                out.push(*b as char);
            }
            out
        }
        TokenUnit::Compact => {
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

/// Render a USD amount with two decimals and a leading `$`. Always shows two
/// decimals even for whole-dollar amounts so eyeball alignment in the
/// Overview row stays clean. Sub-cent amounts (typically only on brand-new
/// sessions) collapse to "$0.00" rather than "<$0.01" — the user understands.
fn format_cost(n: f64) -> String {
    if n.is_nan() || n.is_infinite() {
        return "—".into();
    }
    if n >= 10_000.0 {
        // Drop cents at four-figure-and-above totals; "$12,345" reads
        // better than "$12,345.67" in the same horizontal space.
        format!("${:.0}", n)
    } else {
        format!("${:.2}", n)
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

    fn row(agent: &'static str, sessions: usize) -> AgentTokenRow {
        AgentTokenRow {
            agent,
            sessions,
            tokens: crate::adapter::types::TokenStats::default(),
        }
    }

    #[test]
    fn agent_lines_pack_into_single_line_when_fitting() {
        let rows = vec![row("claude", 10), row("codex", 5)];
        let lines = build_agent_lines(&rows, 80);
        assert_eq!(lines.len(), 1, "two short items should share one line");
    }

    #[test]
    fn agent_lines_wrap_when_budget_too_small() {
        // Six rows roughly mirroring the screenshot: ClaudeCode=355
        // ClaudeDesktop=2 Codex=258 Gemini=4 Hermes=5 OpenCode=429.
        let rows = vec![
            row("claude", 355),
            row("claude-desktop", 2),
            row("codex", 258),
            row("gemini", 4),
            row("hermes", 5),
            row("opencode", 429),
        ];
        // Tight budget that can hold ~3 of these per row.
        let lines = build_agent_lines(&rows, 50);
        assert!(
            lines.len() >= 2,
            "expected wrap, got {} lines on 50-col budget",
            lines.len()
        );
    }

    #[test]
    fn agent_lines_overflow_one_per_row_when_no_item_fits() {
        // Pathological narrow budget: every item gets its own line, never dropped.
        let rows = vec![row("claude", 1), row("codex", 2), row("gemini", 3)];
        let lines = build_agent_lines(&rows, 1);
        assert_eq!(lines.len(), 3);
    }

    #[test]
    fn overview_height_grows_with_agent_count() {
        let many = (0..10)
            .map(|i| {
                let s = Box::leak(format!("agent{i}").into_boxed_str());
                row(s, i)
            })
            .collect::<Vec<_>>();
        // Wide terminal — single line.
        let h_wide = compute_overview_height(200, &many, false);
        // Narrow terminal — must wrap.
        let h_narrow = compute_overview_height(40, &many, false);
        assert!(
            h_narrow > h_wide,
            "narrow ({h_narrow}) should exceed wide ({h_wide})"
        );
        // Empty rows still get one agent row → height 5 (matches legacy).
        assert_eq!(compute_overview_height(200, &[], false), 5);
        // Trend row adds 1 to whatever the no-trend height was.
        assert_eq!(
            compute_overview_height(200, &[], true),
            compute_overview_height(200, &[], false) + 1
        );
    }

    // -- Responsive layout tests ---------------------------------------------
    // Render the full Dashboard onto a TestBackend at different geometries
    // and assert the panels we expect to drop / restack actually do.

    use crate::app::App;
    use crate::collector::metrics::MetricsStore;
    use crate::collector::token_refresh::TokenCache;
    use crate::config::Config;
    use parking_lot::RwLock;
    use ratatui::backend::TestBackend;
    use std::sync::Arc;

    fn dashboard_app() -> App {
        App {
            config: Config::default(),
            state: Arc::new(RwLock::new(crate::app::AppState::default())),
            metrics: Arc::new(MetricsStore::new(8)),
            adapters: Vec::new(),
            tab: crate::app::Tab::Dashboard,
            should_quit: false,
            session_filter: String::new(),
            session_filter_input: false,
            session_sort: crate::app::SessionSort::default(),
            delete_confirm: None,
            selected_process: 0,
            selected_project: 0,
            dashboard_cursor: crate::app::DashboardCursor::default(),
            selected_setting: 0,
            settings_keybindings_open: false,
            selected_keybinding: 0,
            capturing_keybinding: None,
            keybinding_conflict: None,
            token_cache: Arc::new(TokenCache::new()),
            token_trend: Arc::new(crate::collector::token_trend::TokenTrend::default()),
            diagnostics: Arc::new(crate::collector::diagnostics::DiagnosticsStore::new()),
            dirty: Arc::new(tokio::sync::Notify::new()),
            token_dirty: Arc::new(tokio::sync::Notify::new()),
        }
    }

    fn dump_buffer(buffer: &ratatui::buffer::Buffer) -> String {
        let mut out = String::with_capacity((buffer.area.width * buffer.area.height) as usize);
        for y in 0..buffer.area.height {
            for x in 0..buffer.area.width {
                out.push_str(buffer[(x, y)].symbol());
            }
            out.push('\n');
        }
        out
    }

    #[test]
    fn dashboard_short_terminal_drops_tokens_strip() {
        let _guard = crate::settings::test_lock();
        let app = dashboard_app();
        // 24 rows is below DASHBOARD_TOKENS_STRIP_MIN_HEIGHT(28).
        let backend = TestBackend::new(120, 24);
        let mut term = ratatui::Terminal::new(backend).unwrap();
        let _ = term
            .draw(|frame| render(frame, frame.area(), &app))
            .unwrap();
        let dump = dump_buffer(term.backend().buffer());
        assert!(
            !dump.contains("Tokens by agent"),
            "tokens strip leaked at h=24:\n{dump}"
        );
    }

    #[test]
    fn dashboard_tall_terminal_keeps_tokens_strip() {
        let _guard = crate::settings::test_lock();
        let app = dashboard_app();
        // 40 rows is well above the threshold.
        let backend = TestBackend::new(120, 40);
        let mut term = ratatui::Terminal::new(backend).unwrap();
        let _ = term
            .draw(|frame| render(frame, frame.area(), &app))
            .unwrap();
        let dump = dump_buffer(term.backend().buffer());
        assert!(
            dump.contains("Tokens by agent"),
            "tokens strip missing at h=40:\n{dump}"
        );
    }

    #[test]
    fn dashboard_narrow_terminal_stacks_activity_above_top_projects() {
        let _guard = crate::settings::test_lock();
        let app = dashboard_app();
        // 60 cols: middle row drops below DASHBOARD_HSTACK_MIN_WIDTH(70).
        let backend = TestBackend::new(60, 40);
        let mut term = ratatui::Terminal::new(backend).unwrap();
        let _ = term
            .draw(|frame| render(frame, frame.area(), &app))
            .unwrap();
        let buffer = term.backend().buffer();
        let mut activity_y: Option<u16> = None;
        let mut projects_y: Option<u16> = None;
        for y in 0..buffer.area.height {
            let row: String = (0..buffer.area.width)
                .map(|x| buffer[(x, y)].symbol())
                .collect();
            if activity_y.is_none() && row.contains("Activity") {
                activity_y = Some(y);
            }
            if projects_y.is_none() && row.contains("Top Projects") {
                projects_y = Some(y);
            }
        }
        let (a, p) = (activity_y.expect("activity"), projects_y.expect("projects"));
        assert!(a < p, "activity (y={a}) should sit above projects (y={p})");
    }

    #[test]
    fn aggregates_cache_hits_on_same_generation() {
        // Same generation + same Arc → second call returns the cached snapshot
        // unchanged (verified via `computed_at` equality, which would shift on
        // a fresh recompute).
        use std::sync::Arc;
        let _guard = crate::settings::test_lock();

        let now = chrono::Utc::now();
        let sessions = Arc::new(vec![SessionMeta {
            agent: "claude",
            id: "x".into(),
            path: std::path::PathBuf::from("/tmp/x.jsonl"),
            cwd: None,
            model: None,
            version: None,
            git_branch: None,
            source: None,
            started_at: None,
            updated_at: Some(now),
            message_count: 0,
            tokens: crate::adapter::types::TokenStats::default(),
            status: crate::adapter::types::SessionStatus::Active,
            byte_offset: 0,
            size_bytes: 0,
        }]);

        let agg1 = aggregates_for(&sessions, 42, true, &["claude"], now);
        assert_eq!(agg1.last24h, 1);
        assert_eq!(agg1.generation, 42);

        // Second call with the same generation must hit the cache — same
        // `computed_at` proves we returned the snapshot unmodified.
        let agg2 = aggregates_for(&sessions, 42, true, &["claude"], now);
        assert_eq!(agg2.computed_at, agg1.computed_at);
    }

    #[test]
    fn aggregates_cache_misses_on_new_generation() {
        // The whole point of the generation counter: an in-place mutation
        // (like Arc::make_mut on the sole owner, which keeps the Arc pointer
        // identical) still bumps the cache. Simulate that by passing a new
        // generation number.
        use std::sync::Arc;
        let _guard = crate::settings::test_lock();

        let now = chrono::Utc::now();
        let mut sessions_vec = vec![SessionMeta {
            agent: "claude",
            id: "first".into(),
            path: std::path::PathBuf::from("/tmp/first.jsonl"),
            cwd: None,
            model: None,
            version: None,
            git_branch: None,
            source: None,
            started_at: None,
            updated_at: Some(now),
            message_count: 0,
            tokens: crate::adapter::types::TokenStats::default(),
            status: crate::adapter::types::SessionStatus::Active,
            byte_offset: 0,
            size_bytes: 0,
        }];
        let sessions_v1 = Arc::new(sessions_vec.clone());
        let agg1 = aggregates_for(&sessions_v1, 1, true, &["claude"], now);
        assert_eq!(agg1.last24h, 1);

        // Mutate, bump generation, ask again.
        sessions_vec.push(SessionMeta {
            agent: "claude",
            id: "second".into(),
            path: std::path::PathBuf::from("/tmp/second.jsonl"),
            cwd: None,
            model: None,
            version: None,
            git_branch: None,
            source: None,
            started_at: None,
            updated_at: Some(now),
            message_count: 0,
            tokens: crate::adapter::types::TokenStats::default(),
            status: crate::adapter::types::SessionStatus::Active,
            byte_offset: 0,
            size_bytes: 0,
        });
        let sessions_v2 = Arc::new(sessions_vec);
        let agg2 = aggregates_for(&sessions_v2, 2, true, &["claude"], now);
        assert_eq!(agg2.last24h, 2);
        assert_ne!(agg1.generation, agg2.generation);
    }
}
