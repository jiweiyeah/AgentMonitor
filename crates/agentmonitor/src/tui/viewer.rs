//! Full-screen transcript viewer.
//!
//! Design goals:
//! - Render a complete parsed conversation in chronological order.
//! - Keep non-conversational noise (tool use/result, thinking, attachments)
//!   visible but collapsed by default — users flip with `e` / `c`.
//! - Constant-time per frame regardless of transcript size. We build the full
//!   `Vec<Line>` once per (events_len, expand) change and slice out only the
//!   visible window. Scrolling never rebuilds.

use std::sync::{Mutex, OnceLock};

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;

use crate::adapter::conversation::{Block as CBlock, ConversationEvent};
use crate::adapter::types::{MessageRole, SessionMeta};
use crate::app::{App, ConversationCache, ExpandMode, Mode};
use crate::i18n::t;
use crate::settings;
use crate::tui::theme;

/// Cached flattened transcript — kept outside of `AppState` so `draw()` can
/// touch it without acquiring the state write lock. Invalidated by comparing
/// a small fingerprint (path + events len + expand mode).
#[derive(Default)]
struct RenderCache {
    fingerprint: Option<Fingerprint>,
    lines: Vec<Line<'static>>,
}

#[derive(PartialEq, Eq)]
struct Fingerprint {
    path: std::path::PathBuf,
    events_len: usize,
    expand: ExpandMode,
}

fn render_cache() -> &'static Mutex<RenderCache> {
    static CACHE: OnceLock<Mutex<RenderCache>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(RenderCache::default()))
}

pub fn render(frame: &mut Frame, area: Rect, app: &App) {
    let (path, meta) = {
        let s = app.state.read();
        let Mode::Viewer { path } = s.mode.clone() else {
            return;
        };
        let meta = s.sessions.iter().find(|m| m.path == path).cloned();
        (path, meta)
    };

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(5),
            Constraint::Min(3),
            Constraint::Length(1),
        ])
        .split(area);
    let body_h = chunks[1].height.saturating_sub(2) as usize;

    // Read the cache once, build lines if needed, and compute clipped scroll
    // + total lines while we still hold the read lock. Everything after this
    // point only needs the widget-level snapshots.
    let state = app.state.read();
    let cache = state.conversation.as_ref();
    let (visible, clipped_scroll, total_lines) = build_visible(cache, &path, body_h);

    render_header(frame, chunks[0], meta.as_ref(), cache);
    render_body(frame, chunks[1], cache, visible);
    render_footer(frame, chunks[2], cache);
    drop(state);

    // Sync geometry + clipped scroll back so the event handler's bounds math
    // matches what the user actually sees.
    {
        let mut s = app.state.write();
        if let Some(c) = s.conversation.as_mut() {
            if c.path == path {
                c.viewport_height = body_h as u16;
                c.last_rendered_total = total_lines;
                c.scroll = clipped_scroll;
            }
        }
    }
}

fn build_visible(
    cache: Option<&ConversationCache>,
    mode_path: &std::path::Path,
    body_h: usize,
) -> (Option<Vec<Line<'static>>>, u16, u32) {
    let Some(c) = cache else {
        return (None, 0, 0);
    };
    if c.path != mode_path {
        return (None, c.scroll, 0);
    }
    if c.error.is_some() || c.events.is_empty() {
        return (None, 0, 0);
    }
    let fp = Fingerprint {
        path: c.path.clone(),
        events_len: c.events.len(),
        expand: c.expand,
    };
    let mut cell = render_cache().lock().expect("viewer cache poisoned");
    if cell.fingerprint.as_ref() != Some(&fp) {
        cell.lines = build_lines(&c.events, c.expand);
        cell.fingerprint = Some(fp);
    }
    let total = cell.lines.len();
    let max_scroll = total.saturating_sub(body_h.max(1)) as u16;
    let clipped = c.scroll.min(max_scroll);
    let start = clipped as usize;
    let end = (start + body_h).min(total);
    let vis: Vec<Line<'static>> = cell.lines[start..end].to_vec();
    (Some(vis), clipped, total as u32)
}

fn render_header(
    frame: &mut Frame,
    area: Rect,
    meta: Option<&SessionMeta>,
    cache: Option<&ConversationCache>,
) {
    let long_pat = settings::get().time_format.pattern_long();
    let (agent, id, cwd, model, updated) = match meta {
        Some(m) => (
            m.agent_label().to_string(),
            m.id.clone(),
            m.cwd_display(),
            m.model.clone().unwrap_or_else(|| "-".into()),
            m.updated_at
                .map(|t| t.with_timezone(&chrono::Local).format(long_pat).to_string())
                .unwrap_or_else(|| "-".into()),
        ),
        None => ("-".into(), "-".into(), "-".into(), "-".into(), "-".into()),
    };
    let status_chip = match cache {
        Some(c) if c.loading => Span::styled(t("viewer.loading_chip"), theme::muted()),
        Some(c) if c.error.is_some() => Span::styled(
            format!(" error: {} ", c.error.as_deref().unwrap_or("?")),
            Style::default().fg(Color::Red),
        ),
        Some(c) => Span::styled(
            format!(" {} {} ", c.events.len(), t("viewer.events")),
            Style::default()
                .fg(theme::accent())
                .add_modifier(Modifier::BOLD),
        ),
        None => Span::styled(t("viewer.no_data"), theme::muted()),
    };
    let lines = vec![
        Line::from(vec![
            Span::styled(
                " Viewer · ",
                Style::default()
                    .fg(theme::accent())
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(agent),
            Span::raw("  "),
            Span::raw(id),
            Span::raw("  "),
            status_chip,
        ]),
        Line::from(vec![
            Span::styled(t("viewer.cwd"), theme::muted()),
            Span::raw(cwd),
        ]),
        Line::from(vec![
            Span::styled(t("viewer.model"), theme::muted()),
            Span::raw(model),
            Span::styled(t("viewer.updated"), theme::muted()),
            Span::raw(updated),
        ]),
    ];
    let para = Paragraph::new(lines).block(
        Block::default()
            .borders(Borders::ALL)
            .title(Span::styled(t("viewer.title"), theme::title())),
    );
    frame.render_widget(para, area);
}

fn render_body(
    frame: &mut Frame,
    area: Rect,
    cache: Option<&ConversationCache>,
    visible: Option<Vec<Line<'static>>>,
) {
    let block = Block::default().borders(Borders::ALL);
    let Some(cache) = cache else {
        let para = Paragraph::new(Line::from(Span::styled(
            t("viewer.no_conversation"),
            theme::muted(),
        )))
        .block(block);
        frame.render_widget(para, area);
        return;
    };
    if let Some(err) = &cache.error {
        let para = Paragraph::new(vec![Line::from(Span::styled(
            format!("Failed to load: {err}"),
            Style::default().fg(Color::Red),
        ))])
        .block(block);
        frame.render_widget(para, area);
        return;
    }
    if cache.loading && cache.events.is_empty() {
        let para = Paragraph::new(Line::from(Span::styled(
            t("viewer.loading"),
            theme::muted(),
        )))
        .block(block);
        frame.render_widget(para, area);
        return;
    }
    if cache.events.is_empty() {
        let para = Paragraph::new(Line::from(Span::styled(
            t("viewer.no_messages"),
            theme::muted(),
        )))
        .block(block);
        frame.render_widget(para, area);
        return;
    }
    let visible = visible.unwrap_or_default();
    let para = Paragraph::new(visible).block(block);
    frame.render_widget(para, area);
}

fn render_footer(frame: &mut Frame, area: Rect, cache: Option<&ConversationCache>) {
    let (scroll, expand, total) = match cache {
        Some(c) => (c.scroll, c.expand, c.events.len()),
        None => (0, ExpandMode::Collapsed, 0),
    };
    let expand_label = match expand {
        ExpandMode::Collapsed => t("viewer.collapsed"),
        ExpandMode::Expanded => t("viewer.expanded"),
    };
    let footer = Paragraph::new(Line::from(vec![
        Span::styled(" Esc ", Style::default()),
        Span::styled(format!("{} ", t("footer.back")), theme::muted()),
        Span::styled(" j/k ", Style::default()),
        Span::styled(format!("{} ", t("footer.scroll")), theme::muted()),
        Span::styled(" Ctrl+D/U ", Style::default()),
        Span::styled(format!("{} ", t("footer.half_page")), theme::muted()),
        Span::styled(" g/G ", Style::default()),
        Span::styled(format!("{} ", t("footer.top_bottom")), theme::muted()),
        Span::styled(" e/c ", Style::default()),
        Span::styled(format!("{} ", t("footer.expand_collapse")), theme::muted()),
        Span::styled(" r ", Style::default()),
        Span::styled(format!("{} ", t("footer.resume")), theme::muted()),
        Span::styled(
            format!(" [{expand_label} · {total} {} · row {scroll}] ", t("viewer.events")),
            theme::muted(),
        ),
    ]));
    frame.render_widget(footer, area);
}

fn build_lines(events: &[ConversationEvent], expand: ExpandMode) -> Vec<Line<'static>> {
    let mut out: Vec<Line<'static>> = Vec::with_capacity(events.len() * 8);
    for ev in events {
        if !ev.has_visible_content() {
            continue;
        }
        out.push(event_header(ev));
        for block in &ev.blocks {
            if !block.has_content() {
                continue;
            }
            append_block(&mut out, block, expand);
        }
        out.push(Line::from(""));
    }
    out
}

fn event_header(ev: &ConversationEvent) -> Line<'static> {
    let role_style = match ev.role {
        MessageRole::User => Style::default()
            .fg(theme::SUCCESS)
            .add_modifier(Modifier::BOLD),
        MessageRole::Assistant => Style::default()
            .fg(theme::accent())
            .add_modifier(Modifier::BOLD),
        MessageRole::System => Style::default().fg(theme::WARN),
        MessageRole::Tool => Style::default().fg(Color::Magenta),
        MessageRole::Other(_) => theme::muted(),
    };
    let when = ev
        .ts
        .map(|t| t.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M:%S").to_string())
        .unwrap_or_default();
    Line::from(vec![
        Span::styled(format!("┌─[{}] ", ev.role.label()), role_style),
        Span::styled(when, theme::muted()),
    ])
}

fn append_block(out: &mut Vec<Line<'static>>, block: &CBlock, expand: ExpandMode) {
    match block {
        CBlock::Text(text) => push_body(out, text, Style::default()),
        CBlock::Summary(text) => {
            out.push(indent_line(
                "📎 summary",
                Style::default()
                    .fg(theme::WARN)
                    .add_modifier(Modifier::BOLD),
            ));
            push_body(out, text, Style::default());
        }
        CBlock::Thinking(text) => {
            let lines = count_lines(text);
            let head = format!("▸ thinking ({lines} line{})", plural(lines));
            out.push(indent_line(&head, Style::default().fg(Color::Magenta)));
            if expand == ExpandMode::Expanded {
                push_body(out, text, theme::muted());
            }
        }
        CBlock::ToolUse {
            name,
            preview,
            input,
        } => {
            let head = format!("▸ tool_use {}", preview_or_name(name, preview));
            out.push(indent_line(&head, Style::default().fg(theme::accent())));
            if expand == ExpandMode::Expanded && !input.trim().is_empty() {
                push_body(out, input, theme::muted());
            }
        }
        CBlock::ToolResult { is_error, content } => {
            let chars = content.chars().count();
            let flag = if *is_error { " ERROR" } else { "" };
            let head = format!("▸ tool_result{flag} ({chars} chars)");
            let style = if *is_error {
                Style::default().fg(Color::Red)
            } else {
                Style::default().fg(Color::Magenta)
            };
            out.push(indent_line(&head, style));
            if expand == ExpandMode::Expanded {
                push_body(out, content, theme::muted());
            }
        }
        CBlock::System(text) => {
            out.push(indent_line("▸ system", Style::default().fg(theme::WARN)));
            if expand == ExpandMode::Expanded {
                push_body(out, text, theme::muted());
            }
        }
        CBlock::Attachment { label, body } => {
            let chars = body.chars().count();
            let head = format!("▸ {label} ({chars} chars)");
            out.push(indent_line(&head, theme::muted()));
            if expand == ExpandMode::Expanded && !body.is_empty() {
                push_body(out, body, theme::muted());
            }
        }
    }
}

fn preview_or_name(name: &str, preview: &str) -> String {
    if preview.is_empty() {
        name.to_string()
    } else {
        preview.to_string()
    }
}

fn push_body(out: &mut Vec<Line<'static>>, text: &str, style: Style) {
    const BODY_PREFIX: &str = "│ ";
    for raw in text.lines() {
        let line = format!("{BODY_PREFIX}{raw}");
        if style == Style::default() {
            out.push(Line::from(Span::raw(line)));
        } else {
            out.push(Line::from(Span::styled(line, style)));
        }
    }
}

fn indent_line(text: &str, style: Style) -> Line<'static> {
    Line::from(Span::styled(format!("│ {text}"), style))
}

fn count_lines(text: &str) -> usize {
    if text.is_empty() {
        0
    } else {
        text.lines().count()
    }
}

fn plural(n: usize) -> &'static str {
    if n == 1 {
        ""
    } else {
        "s"
    }
}
