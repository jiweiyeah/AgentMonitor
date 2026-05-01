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
use crate::app::{App, ConversationCache, ExpandMode, Mode, ViewerSearch};
use crate::i18n::t;
use crate::settings::{self, KeyAction};
use crate::tui::theme;
use crate::tui::widgets::pack_chips;

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

    // Build footer chips first so we can size the footer pane to fit them.
    let (cache_scroll, cache_expand, cache_total) = {
        let s = app.state.read();
        match s.conversation.as_ref() {
            Some(c) => (c.scroll, c.expand, c.events.len()),
            None => (0, ExpandMode::Collapsed, 0),
        }
    };
    let footer_lines = build_footer_lines(area.width, cache_scroll, cache_expand, cache_total);
    // Clamp 1..3 — extreme narrow widths shouldn't let footer eat the body.
    let footer_h = (footer_lines.len() as u16).clamp(1, 3);

    // Search bar takes one row above the body when there's an active search
    // (either the user is typing, or a query is committed).
    let has_search = {
        let s = app.state.read();
        s.conversation
            .as_ref()
            .and_then(|c| c.search.as_ref())
            .is_some_and(|sr| sr.editing || !sr.query.is_empty())
    };
    let search_h = if has_search { 1 } else { 0 };

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(5),
            Constraint::Length(search_h),
            Constraint::Min(3),
            Constraint::Length(footer_h),
        ])
        .split(area);
    let body_h = chunks[2].height.saturating_sub(2) as usize;

    // Read the cache once, build lines if needed, and compute clipped scroll
    // + total lines while we still hold the read lock. Everything after this
    // point only needs the widget-level snapshots.
    let state = app.state.read();
    let cache = state.conversation.as_ref();
    let (visible, clipped_scroll, total_lines) = build_visible(cache, &path, body_h);

    render_header(frame, chunks[0], meta.as_ref(), cache);
    if has_search {
        render_search_bar(frame, chunks[1], cache);
    }
    render_body(frame, chunks[2], cache, visible);
    render_footer(frame, chunks[3], &footer_lines);
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

    // Highlight matched lines + mark the "current" match more prominently
    // (used by `n`/`N` navigation). We rebuild visible lines from the cached
    // ones so highlight changes don't invalidate the structural cache.
    let search = c.search.as_ref();
    let vis: Vec<Line<'static>> = cell.lines[start..end]
        .iter()
        .enumerate()
        .map(|(offset, line)| {
            let line_idx = start + offset;
            highlight_for_search(line, line_idx, search)
        })
        .collect();
    (Some(vis), clipped, total as u32)
}

/// Apply a search-match highlight to a line if it lives in the match list.
/// The "current" match (the one `n`/`N` is on) gets a brighter style than
/// other matches so navigation feels responsive even when many lines hit.
fn highlight_for_search(
    line: &Line<'static>,
    line_idx: usize,
    search: Option<&ViewerSearch>,
) -> Line<'static> {
    let Some(search) = search else {
        return line.clone();
    };
    if search.matches.is_empty() {
        return line.clone();
    }
    let pos = search.matches.iter().position(|&i| i == line_idx);
    let Some(pos) = pos else {
        return line.clone();
    };
    let is_current = pos == search.current;
    let bg = if is_current { Color::Yellow } else { Color::DarkGray };
    let fg = Color::Black;
    let highlight = Style::default().bg(bg).fg(fg).add_modifier(Modifier::BOLD);

    // Re-style each span with the highlight. Keep span structure so existing
    // role-coloured prefixes still appear (just with the bg overlay).
    let spans: Vec<Span<'static>> = line
        .spans
        .iter()
        .map(|s| Span::styled(s.content.clone(), highlight))
        .collect();
    Line::from(spans)
}

/// Case-insensitive scan over the flattened render-cache lines. Returns line
/// indices that contain `query` anywhere in their concatenated text. Empty
/// query → empty vec (no match concept).
pub(crate) fn find_matches(lines: &[Line<'static>], query: &str) -> Vec<usize> {
    if query.is_empty() {
        return Vec::new();
    }
    let needle = query.to_lowercase();
    lines
        .iter()
        .enumerate()
        .filter_map(|(idx, line)| {
            let text: String = line
                .spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect::<String>()
                .to_lowercase();
            text.contains(&needle).then_some(idx)
        })
        .collect()
}

/// Recompute matches for the conversation cache and align `current` to the
/// nearest match >= the current scroll, so `n`/`N` start from where the user
/// is rather than from match #0. Public so the event handler can call it on
/// commit and on scroll.
pub(crate) fn refresh_search_matches(cache: &mut ConversationCache) {
    let Some(search) = cache.search.as_mut() else {
        return;
    };
    let lines = build_lines(&cache.events, cache.expand);
    let matches = find_matches(&lines, &search.query);
    let cursor_line = cache.scroll as usize;
    let current = matches
        .iter()
        .position(|&i| i >= cursor_line)
        .unwrap_or(0);
    search.matches = matches;
    search.current = current;
}

fn render_search_bar(frame: &mut Frame, area: Rect, cache: Option<&ConversationCache>) {
    let Some(cache) = cache else {
        return;
    };
    let Some(search) = cache.search.as_ref() else {
        return;
    };
    let mut spans: Vec<Span<'static>> = Vec::with_capacity(8);
    spans.push(Span::styled(
        format!(" {} ", t("viewer.search.prompt")),
        Style::default()
            .fg(theme::accent())
            .add_modifier(Modifier::BOLD),
    ));
    if search.editing {
        let body = if search.query.is_empty() {
            t("viewer.search.placeholder").to_string()
        } else {
            search.query.clone()
        };
        spans.push(Span::styled(
            format!("{}█", body),
            Style::default()
                .fg(Color::Black)
                .bg(theme::accent())
                .add_modifier(Modifier::BOLD),
        ));
    } else {
        spans.push(Span::styled(
            format!("`{}` ", search.query),
            Style::default()
                .fg(theme::accent())
                .add_modifier(Modifier::BOLD),
        ));
    }
    if search.matches.is_empty() && !search.query.is_empty() {
        spans.push(Span::styled(
            format!("  ({})", t("viewer.search.no_match")),
            theme::muted(),
        ));
    } else if !search.matches.is_empty() {
        spans.push(Span::styled(
            format!(
                "  {}/{} {}",
                search.current + 1,
                search.matches.len(),
                t("viewer.search.matches")
            ),
            theme::muted(),
        ));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
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

/// Build footer chips and pack them into one or more lines fitting `width`.
/// Each chip is a `[binding-key span, label span]` pair; the trailing
/// status `[expanded · 123 events · row 7]` chunk rides along as the last
/// chip so it wraps consistently with the rest.
fn build_footer_lines(
    width: u16,
    scroll: u16,
    expand: ExpandMode,
    total: usize,
) -> Vec<Line<'static>> {
    let expand_label = match expand {
        ExpandMode::Collapsed => t("viewer.collapsed"),
        ExpandMode::Expanded => t("viewer.expanded"),
    };
    let kb = settings::get().keybindings;

    let chip = |action: KeyAction, label_key: &str| -> Vec<Span<'static>> {
        vec![
            Span::raw(format!(" {} ", kb.binding_display(action))),
            Span::styled(t(label_key).to_string(), theme::muted()),
        ]
    };

    let chips: Vec<Vec<Span<'static>>> = vec![
        chip(KeyAction::ViewerBack, "footer.back"),
        chip(KeyAction::ViewerScrollDown, "footer.scroll"),
        chip(KeyAction::ViewerHalfPageDown, "footer.half_page"),
        chip(KeyAction::ViewerTop, "footer.top_bottom"),
        chip(KeyAction::ViewerExpand, "footer.expand_collapse"),
        chip(KeyAction::ViewerSearchStart, "footer.search"),
        chip(KeyAction::ViewerSearchNext, "footer.search_next_prev"),
        chip(KeyAction::ViewerResume, "footer.resume"),
        chip(KeyAction::ViewerDelete, "footer.delete"),
        // Status chip — last so it rides whichever line still has room.
        vec![Span::styled(
            format!(
                " [{expand_label} · {total} {} · row {scroll}] ",
                t("viewer.events")
            ),
            theme::muted(),
        )],
    ];

    let mut lines = pack_chips(&chips, width);
    if lines.is_empty() {
        lines.push(Line::from(""));
    }
    lines
}

fn render_footer(frame: &mut Frame, area: Rect, lines: &[Line<'static>]) {
    let visible_h = area.height as usize;
    let take = visible_h.min(lines.len()).max(1);
    let render_lines: Vec<Line<'static>> = lines.iter().take(take).cloned().collect();
    frame.render_widget(Paragraph::new(render_lines), area);
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
        .map(|t| {
            t.with_timezone(&chrono::Local)
                .format("%Y-%m-%d %H:%M:%S")
                .to_string()
        })
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn footer_wraps_to_multiple_lines_on_narrow_width() {
        let _guard = crate::settings::test_lock();
        let original = settings::get().keybindings;
        crate::settings::settings().write().keybindings = crate::settings::KeyBindings::default();

        // 80 cols: way too narrow for 7 chips + status. Expect ≥ 2 lines.
        let lines = build_footer_lines(80, 0, ExpandMode::Collapsed, 0);
        assert!(
            lines.len() >= 2,
            "expected wrap on width 80, got {} lines",
            lines.len()
        );

        // 200 cols: plenty of space, single line should still suffice.
        let lines_wide = build_footer_lines(200, 0, ExpandMode::Collapsed, 0);
        assert_eq!(lines_wide.len(), 1);

        crate::settings::settings().write().keybindings = original;
    }

    #[test]
    fn footer_status_chip_present_in_packed_lines() {
        let _guard = crate::settings::test_lock();
        let original = settings::get().keybindings;
        crate::settings::settings().write().keybindings = crate::settings::KeyBindings::default();

        let lines = build_footer_lines(120, 42, ExpandMode::Expanded, 7);
        let all_text: String = lines
            .iter()
            .flat_map(|l| l.spans.iter())
            .map(|s| s.content.as_ref())
            .collect();
        assert!(all_text.contains("row 42"), "row marker missing: {all_text}");
        assert!(all_text.contains('7'), "events count missing: {all_text}");

        crate::settings::settings().write().keybindings = original;
    }

    fn line(s: &str) -> Line<'static> {
        Line::from(Span::raw(s.to_string()))
    }

    #[test]
    fn find_matches_is_case_insensitive_and_returns_indices() {
        let lines = vec![
            line("Hello World"),
            line("foo bar"),
            line("THE WORLD"),
            line(""),
        ];
        assert_eq!(find_matches(&lines, "world"), vec![0, 2]);
        assert_eq!(find_matches(&lines, "WoRlD"), vec![0, 2]);
        assert_eq!(find_matches(&lines, "bar"), vec![1]);
        assert_eq!(find_matches(&lines, "missing"), Vec::<usize>::new());
    }

    #[test]
    fn find_matches_empty_query_returns_empty() {
        let lines = vec![line("anything"), line("else")];
        assert_eq!(find_matches(&lines, ""), Vec::<usize>::new());
    }

    #[test]
    fn find_matches_handles_multi_span_lines() {
        // A line with multiple spans (e.g. role-coloured prefix + body) should
        // match across span boundaries — the search treats the whole line as
        // concatenated text. Reproducing this in tests is critical because
        // build_lines uses styled spans for thinking/tool blocks.
        let multi = Line::from(vec![
            Span::raw("│ "),
            Span::styled("error: ".to_string(), Style::default().fg(Color::Red)),
            Span::raw("could not parse"),
        ]);
        let matches = find_matches(&[multi], "error: could");
        assert_eq!(matches, vec![0]);
    }

    #[test]
    fn highlight_for_search_returns_clone_when_no_search_or_no_match() {
        let line = line("hello");
        // No search → identity.
        let out = highlight_for_search(&line, 0, None);
        assert_eq!(out.spans.len(), 1);
        // Search but line index not in matches → identity.
        let search = ViewerSearch {
            query: "bar".into(),
            matches: vec![5, 7],
            current: 0,
            editing: false,
        };
        let out = highlight_for_search(&line, 3, Some(&search));
        assert_eq!(out.spans.len(), 1);
        assert_eq!(out.spans[0].content.as_ref(), "hello");
    }

    #[test]
    fn highlight_for_search_marks_current_match_distinctly() {
        let line = line("matched");
        let search = ViewerSearch {
            query: "match".into(),
            matches: vec![0, 5],
            current: 0,
            editing: false,
        };
        let out = highlight_for_search(&line, 0, Some(&search));
        // Style overlaid: bg should be Yellow (current), fg Black, BOLD.
        assert_eq!(out.spans[0].style.bg, Some(Color::Yellow));
        assert_eq!(out.spans[0].style.fg, Some(Color::Black));

        // A non-current match (line index 5) gets DarkGray bg instead.
        let search2 = ViewerSearch {
            query: "match".into(),
            matches: vec![0, 5],
            current: 0, // current is index 0, so line 5 is "other match"
            editing: false,
        };
        let out2 = highlight_for_search(&line, 5, Some(&search2));
        assert_eq!(out2.spans[0].style.bg, Some(Color::DarkGray));
    }
}
