use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};
use ratatui::Frame;

use crate::app::App;
use crate::tui::{detail, theme, widgets::status_span};

pub fn render(frame: &mut Frame, area: Rect, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(area);

    let state = app.state.read();
    let items: Vec<ListItem> = state
        .sessions
        .iter()
        .map(|s| {
            let when = s
                .updated_at
                .map(|t| t.format("%m-%d %H:%M").to_string())
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

    let mut list_state = ListState::default();
    if !state.sessions.is_empty() {
        list_state.select(Some(state.selected_session.min(state.sessions.len() - 1)));
    }

    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title(Span::styled(
            format!(" Sessions ({}) ", state.sessions.len()),
            theme::title(),
        )))
        .highlight_style(theme::selected())
        .highlight_symbol("▶ ");

    frame.render_stateful_widget(list, chunks[0], &mut list_state);

    // Detail on the right.
    let selected = state.sessions.get(state.selected_session).cloned();
    let preview = state.preview.clone();
    drop(state);
    if let Some(s) = selected {
        let preview_ref = preview.as_ref().filter(|p| p.path == s.path);
        detail::render(frame, chunks[1], &s, preview_ref);
    } else {
        let empty =
            Paragraph::new("No sessions found yet. Run `claude` or `codex` to see them here.")
                .style(theme::muted())
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .title(Span::styled(" Detail ", theme::title())),
                );
        frame.render_widget(empty, chunks[1]);
    }

    let _ = status_span; // silence unused in this module while P5 wires full details
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
