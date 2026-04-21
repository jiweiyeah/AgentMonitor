use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};
use ratatui::Frame;

use crate::app::App;
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

    let items: Vec<ListItem> = visible
        .iter()
        .map(|&i| {
            let s = &state.sessions[i];
            let when = s
                .updated_at
                .map(|t| t.with_timezone(&chrono::Local).format("%m-%d %H:%M").to_string())
                .unwrap_or_else(|| "-----".into());
            let line = Line::from(vec![
                Span::styled(
                    format!("{:<10} ", s.agent_label()),
                    Style::default()
                        .fg(theme::ACCENT)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(format!("{} ", when), theme::muted()),
                Span::styled(
                    format!("{:<8} ", s.short_id()),
                    Style::default().fg(Color::White),
                ),
                Span::raw(shorten(&s.cwd_display(), 30)),
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

    let title = format!(" Sessions ({}/{}) ", visible.len(), state.sessions.len());
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
            "No sessions found yet. Run `claude` or `codex` to see them here."
        } else {
            "No sessions match the current filter."
        };
        let empty = Paragraph::new(msg).style(theme::muted()).block(
            Block::default()
                .borders(Borders::ALL)
                .title(Span::styled(" Detail ", theme::title())),
        );
        frame.render_widget(empty, body[1]);
    }
}

fn render_filter_bar(frame: &mut Frame, area: Rect, app: &App) {
    let mut spans = vec![Span::styled("Filter ", theme::muted())];
    if app.session_filter_input {
        spans.push(Span::styled(
            format!("{}█", app.session_filter),
            Style::default()
                .fg(Color::Black)
                .bg(theme::ACCENT)
                .add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::styled(
            "  Esc cancel · Enter apply · Backspace delete",
            theme::muted(),
        ));
    } else if app.session_filter.is_empty() {
        spans.push(Span::styled(
            "(press / to filter)",
            theme::muted(),
        ));
    } else {
        spans.push(Span::styled(
            format!("`{}`", app.session_filter),
            Style::default()
                .fg(theme::ACCENT)
                .add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::styled(" (c to clear)", theme::muted()));
    }
    spans.push(Span::styled("    Sort ", theme::muted()));
    spans.push(Span::styled(
        app.session_sort.label(),
        Style::default()
            .fg(theme::ACCENT)
            .add_modifier(Modifier::BOLD),
    ));
    spans.push(Span::styled(" (s to cycle)", theme::muted()));
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
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
