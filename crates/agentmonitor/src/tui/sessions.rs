use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};
use ratatui::Frame;

use crate::adapter::types::SessionStatus;
use crate::app::App;
use crate::i18n::t;
use crate::settings::{self, TokenUnit};
use crate::tui::{detail, theme};

pub fn render(frame: &mut Frame, area: Rect, app: &App) {
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(3)])
        .split(area);

    render_filter_bar(frame, outer[0], app);

    let body = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(outer[1]);

    let state = app.state.read();
    let visible = state.visible_session_indices(&app.session_filter, app.session_sort);
    let settings = settings::get();
    let time_pattern = settings.time_format.pattern_short();
    let include_cache = settings.include_cache_in_total;
    let token_unit = settings.token_unit;

    let items: Vec<ListItem> = visible
        .iter()
        .map(|&i| {
            let s = &state.sessions[i];
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
            let line = Line::from(vec![
                status_marker(s.status),
                Span::raw(" "),
                Span::styled(
                    format!("{:<13} ", s.agent_label()),
                    Style::default()
                        .fg(theme::accent())
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(format!("{} ", when), theme::muted()),
                Span::styled(
                    format!("{:<8} ", s.short_id()),
                    Style::default().fg(Color::White),
                ),
                Span::styled(
                    format!("{:>8} ", token_label),
                    Style::default().fg(theme::SUCCESS),
                ),
                Span::raw(shorten(&s.cwd_display(), 22)),
            ]);
            ListItem::new(line)
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

    // Detail on the right.
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
