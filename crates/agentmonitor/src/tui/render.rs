use std::io::Stdout;

use anyhow::Result;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Tabs};
use ratatui::Terminal;

use crate::app::{App, Tab};
use crate::tui::{dashboard, process, sessions, theme};

/// Full-frame draw. Called on every input event and every `dirty` notify.
pub async fn draw(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    app: &App,
) -> Result<()> {
    terminal.draw(|frame| {
        let area = frame.area();
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3), // top bar
                Constraint::Min(5),    // body
                Constraint::Length(1), // footer
            ])
            .split(area);

        // ── top tabs ────────────────────────────────────────
        let titles: Vec<Line> = Tab::all()
            .iter()
            .enumerate()
            .map(|(i, t)| {
                Line::from(Span::raw(format!(" {} {} ", i + 1, t.title())))
            })
            .collect();
        let tabs = Tabs::new(titles)
            .select(app.tab.index())
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(Span::styled(" agent-monitor ", theme::title())),
            )
            .highlight_style(theme::active_tab())
            .divider("|");
        frame.render_widget(tabs, chunks[0]);

        // ── body ────────────────────────────────────────────
        match app.tab {
            Tab::Dashboard => dashboard::render(frame, chunks[1], app),
            Tab::Sessions => sessions::render(frame, chunks[1], app),
            Tab::Process => process::render(frame, chunks[1], app),
        }

        // ── footer hint ─────────────────────────────────────
        let footer = Paragraph::new(Line::from(vec![
            Span::styled(" q ", Style::default()),
            Span::styled("quit ", theme::muted()),
            Span::styled(" Tab ", Style::default()),
            Span::styled("switch ", theme::muted()),
            Span::styled(" j/k ", Style::default()),
            Span::styled("move ", theme::muted()),
            Span::styled(" r ", Style::default()),
            Span::styled("refresh ", theme::muted()),
        ]));
        frame.render_widget(footer, chunks[2]);
    })?;
    Ok(())
}
