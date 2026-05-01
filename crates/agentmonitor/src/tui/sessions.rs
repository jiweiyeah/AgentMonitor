use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};
use ratatui::Frame;

use crate::adapter::types::{SessionMeta, SessionStatus};
use crate::app::App;
use crate::i18n::t;
use crate::settings::{self, TokenUnit};
use crate::tui::{detail, theme, SESSIONS_TWO_PANE_MIN_WIDTH};

pub fn render(frame: &mut Frame, area: Rect, app: &App) {
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(3)])
        .split(area);

    render_filter_bar(frame, outer[0], app);

    // Two-pane on wide terminals; single-column list on narrow ones. The
    // detail pane needs roughly 40 cols to be useful and the list needs
    // ~70 once cwd is included; below SESSIONS_TWO_PANE_MIN_WIDTH the
    // 50/50 split kills both. Single-column users still get full detail
    // via the existing Viewer (`o` / Enter).
    let two_pane = outer[1].width >= SESSIONS_TWO_PANE_MIN_WIDTH;
    let body = if two_pane {
        Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Min(60), Constraint::Min(40)])
            .split(outer[1])
    } else {
        Layout::default()
            .constraints([Constraint::Min(0)])
            .split(outer[1])
    };

    let state = app.state.read();
    let visible = state.visible_session_indices(&app.session_filter, app.session_sort);
    let settings = settings::get();
    let time_pattern = settings.time_format.pattern_short();
    let include_cache = settings.include_cache_in_total;
    let token_unit = settings.token_unit;

    // List inner area = block inner = body[0] minus 2 cols of borders.
    let list_inner_w = body[0].width.saturating_sub(2);

    let items: Vec<ListItem> = visible
        .iter()
        .map(|&i| {
            let s = &state.sessions[i];
            ListItem::new(build_session_line(
                s,
                list_inner_w,
                time_pattern,
                token_unit,
                include_cache,
            ))
        })
        .collect();

    let selected_row = if visible.is_empty() {
        None
    } else {
        Some(state.selected_session.min(visible.len() - 1))
    };

    let mut list_state = ListState::default();
    list_state.select(selected_row);

    let title = format!(
        "{}({}/{}) ",
        t("sessions.title"),
        visible.len(),
        state.sessions.len()
    );
    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(Span::styled(title, theme::title())),
        )
        .highlight_style(theme::selected())
        .highlight_symbol("▶ ");

    frame.render_stateful_widget(list, body[0], &mut list_state);

    // Detail on the right — only on wide terminals. Single-pane users see
    // detail via the full-screen Viewer (`o` / Enter).
    if !two_pane {
        return;
    }

    let selected_meta = selected_row
        .and_then(|row| visible.get(row))
        .and_then(|&i| state.sessions.get(i))
        .cloned();
    let preview = state.preview.clone();
    drop(state);

    if let Some(s) = selected_meta {
        let preview_ref = preview.as_ref().filter(|p| p.path == s.path);
        detail::render(frame, body[1], &s, preview_ref);
    } else {
        let msg = if app.session_filter.is_empty() {
            t("sessions.empty")
        } else {
            t("sessions.empty_filtered")
        };
        let empty = Paragraph::new(msg).style(theme::muted()).block(
            Block::default()
                .borders(Borders::ALL)
                .title(Span::styled(t("sessions.detail"), theme::title())),
        );
        frame.render_widget(empty, body[1]);
    }
}

/// Build a single list row whose total display width fits inside `inner_w`.
/// Columns are dropped from least-essential to most-essential as space
/// shrinks: cwd → short_id → tokens (kept). status + agent + when always
/// stay. The selection indicator (`▶ ` = 2 cols) is rendered by the List
/// widget itself, so it's not part of inner_w here.
pub(crate) fn build_session_line(
    s: &SessionMeta,
    inner_w: u16,
    time_pattern: &str,
    token_unit: TokenUnit,
    include_cache: bool,
) -> Line<'static> {
    let inner = inner_w as usize;
    let when = s
        .updated_at
        .map(|t| {
            t.with_timezone(&chrono::Local)
                .format(time_pattern)
                .to_string()
        })
        .unwrap_or_else(|| "-----".into());
    let token_total = s.tokens.total_with_preference(include_cache);
    let token_label = format_token_count(token_total, token_unit);

    // Per-column display widths. status=1 (the ●), agent=13 (max
    // "ClaudeDesktop"), when=11 ("MM-DD HH:MM"), id=12, tokens=8.
    // Each item's column width includes a trailing space.
    const W_STATUS: usize = 2; // glyph + space
    const W_WHEN: usize = 12; // "MM-DD HH:MM" + space
    let agent_w_full = 14usize; // 13 + space
    let agent_w_short = 8usize; // truncated agent + space
    const W_ID: usize = 13; // 12 + space
    const W_TOKENS: usize = 9; // 8 + space

    // Allocate columns greedily based on what fits.
    let mut spans: Vec<Span<'static>> = Vec::with_capacity(8);
    let mut used = 0usize;

    // Status: always.
    spans.push(status_marker(s.status));
    spans.push(Span::raw(" "));
    used += W_STATUS;

    // Star marker (always after status, takes 2 cols including trailing
    // space). When the user hasn't bookmarked any session, every row gets
    // an invisible blank in this slot so the rest of the row stays aligned.
    let starred = crate::settings::is_starred(&s.path);
    spans.push(if starred {
        Span::styled(
            "★",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )
    } else {
        Span::raw(" ")
    });
    spans.push(Span::raw(" "));
    used += 2;

    // Agent: full width if there's room, otherwise truncate to 7 chars.
    let agent_label = s.agent_label();
    let agent_w = if inner.saturating_sub(used) >= agent_w_full + W_WHEN + W_TOKENS {
        agent_w_full
    } else {
        agent_w_short
    };
    let agent_text = if agent_label.len() < agent_w {
        format!("{:<width$} ", agent_label, width = agent_w - 1)
    } else {
        let mut s = agent_label.chars().take(agent_w - 2).collect::<String>();
        s.push('…');
        s.push(' ');
        s
    };
    spans.push(Span::styled(
        agent_text,
        Style::default()
            .fg(theme::accent())
            .add_modifier(Modifier::BOLD),
    ));
    used += agent_w;

    // When: always.
    spans.push(Span::styled(format!("{} ", when), theme::muted()));
    used += W_WHEN;

    // Short id: only if there's room *after* still leaving space for tokens.
    let want_id = inner.saturating_sub(used) >= W_ID + W_TOKENS;
    if want_id {
        spans.push(Span::styled(
            format!("{:<12} ", s.short_id()),
            Style::default().fg(Color::White),
        ));
        used += W_ID;
    }

    // Tokens: always last numeric column (keep when space lets us).
    if inner.saturating_sub(used) >= W_TOKENS {
        spans.push(Span::styled(
            format!("{:>8} ", token_label),
            Style::default().fg(theme::SUCCESS),
        ));
        used += W_TOKENS;
    }

    // CWD fills the remainder if any.
    let remaining = inner.saturating_sub(used);
    if remaining >= 6 {
        spans.push(Span::raw(shorten(
            &s.cwd_display(),
            remaining.saturating_sub(1),
        )));
    }

    Line::from(spans)
}

fn render_filter_bar(frame: &mut Frame, area: Rect, app: &App) {
    let mut spans = vec![Span::styled(t("sessions.filter_label"), theme::muted())];
    if app.session_filter_input {
        spans.push(Span::styled(
            format!("{}█", app.session_filter),
            Style::default()
                .fg(Color::Black)
                .bg(theme::accent())
                .add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::styled(t("sessions.filter_edit_hint"), theme::muted()));
    } else if app.session_filter.is_empty() {
        spans.push(Span::styled(t("sessions.filter_hint"), theme::muted()));
    } else {
        spans.push(Span::styled(
            format!("`{}`", app.session_filter),
            Style::default()
                .fg(theme::accent())
                .add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::styled(
            t("sessions.filter_clear_hint"),
            theme::muted(),
        ));
    }
    spans.push(Span::styled(t("sessions.sort_label"), theme::muted()));
    spans.push(Span::styled(
        app.session_sort.label(),
        Style::default()
            .fg(theme::accent())
            .add_modifier(Modifier::BOLD),
    ));
    spans.push(Span::styled(t("sessions.sort_hint"), theme::muted()));
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn status_marker(status: SessionStatus) -> Span<'static> {
    match status {
        SessionStatus::Active => Span::styled("●", theme::status_active()),
        SessionStatus::Idle => Span::styled("◐", theme::status_idle()),
        SessionStatus::Completed => Span::styled("○", theme::status_done()),
        SessionStatus::Unknown => Span::styled("?", theme::status_done()),
    }
}

fn format_token_count(n: u64, unit: TokenUnit) -> String {
    match unit {
        TokenUnit::Raw => n.to_string(),
        TokenUnit::Compact => {
            if n >= 1_000_000_000 {
                format!("{:.1}B", n as f64 / 1_000_000_000.0)
            } else if n >= 1_000_000 {
                format!("{:.1}M", n as f64 / 1_000_000.0)
            } else if n >= 1_000 {
                format!("{:.1}K", n as f64 / 1_000.0)
            } else {
                n.to_string()
            }
        }
    }
}

fn shorten(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max).collect();
        out.push('…');
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::types::TokenStats;
    use std::path::PathBuf;

    fn meta() -> SessionMeta {
        SessionMeta {
            agent: "claude",
            id: "abcdef1234567890".into(),
            path: PathBuf::from("/tmp/x.jsonl"),
            cwd: Some(PathBuf::from("/Users/me/code/projects/AgentMonitor")),
            model: None,
            version: None,
            git_branch: None,
            source: None,
            started_at: None,
            updated_at: None,
            message_count: 0,
            tokens: TokenStats::default(),
            status: Default::default(),
            byte_offset: 0,
            size_bytes: 0,
        }
    }

    fn line_text(line: &Line<'_>) -> String {
        line.spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect::<String>()
    }

    #[test]
    fn build_session_line_drops_cwd_at_narrow_width() {
        // 50 cols inner: status(2) + agent(14) + when(12) + id(13) + tokens(9) = 50,
        // no room for cwd.
        let s = meta();
        let line = build_session_line(&s, 50, "%m-%d %H:%M", TokenUnit::Compact, true);
        let text = line_text(&line);
        assert!(
            !text.contains("AgentMonitor"),
            "cwd unexpectedly visible at width 50: {text:?}"
        );
    }

    #[test]
    fn build_session_line_keeps_cwd_when_wide() {
        // 100 cols: plenty of room for cwd to show.
        let s = meta();
        let line = build_session_line(&s, 100, "%m-%d %H:%M", TokenUnit::Compact, true);
        let text = line_text(&line);
        assert!(
            text.contains("AgentMonitor"),
            "cwd missing at width 100: {text:?}"
        );
    }

    #[test]
    fn build_session_line_drops_short_id_at_minimal_width() {
        // 35 cols: room for status + agent + when + tokens, no id.
        let s = meta();
        let line = build_session_line(&s, 35, "%m-%d %H:%M", TokenUnit::Compact, true);
        let text = line_text(&line);
        assert!(
            !text.contains("abcdef123456"),
            "short_id unexpectedly visible at width 35: {text:?}"
        );
    }
}
