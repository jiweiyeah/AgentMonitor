use std::io::Stdout;

use anyhow::Result;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Tabs};
use ratatui::Frame;
use ratatui::Terminal;

use crate::app::{App, Mode, Tab};
use crate::tui::{dashboard, process, sessions, theme, viewer};

/// Full-frame draw. Called on every input event and every `dirty` notify.
pub async fn draw(terminal: &mut Terminal<CrosstermBackend<Stdout>>, app: &App) -> Result<()> {
    let mode = app.state.read().mode.clone();
    terminal.draw(|frame| {
        let area = frame.area();
        match mode {
            Mode::Normal => draw_tabs(frame, area, app),
            Mode::Viewer { .. } => viewer::render(frame, area, app),
        }
    })?;
    Ok(())
}

fn draw_tabs(frame: &mut Frame, area: Rect, app: &App) {
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
        .map(|(i, t)| Line::from(Span::raw(format!(" {} {} ", i + 1, t.title()))))
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
    let mut spans = vec![
        Span::styled(" q ", Style::default()),
        Span::styled("quit ", theme::muted()),
        Span::styled(" Tab ", Style::default()),
        Span::styled("switch ", theme::muted()),
        Span::styled(" j/k ", Style::default()),
        Span::styled("move ", theme::muted()),
        Span::styled(" r ", Style::default()),
        Span::styled("refresh ", theme::muted()),
    ];
    if app.tab == Tab::Sessions {
        spans.push(Span::styled(" Enter ", Style::default()));
        spans.push(Span::styled("open viewer ", theme::muted()));
    }
    let footer = Paragraph::new(Line::from(spans));
    frame.render_widget(footer, chunks[2]);
}
