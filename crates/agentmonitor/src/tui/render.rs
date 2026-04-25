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
use crate::i18n::t;
use crate::tui::{dashboard, sessions, settings as settings_tab, theme, viewer};

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
    let title = Line::from(vec![
        Span::styled(" agent-monitor ", theme::title()),
        Span::styled(format!("v{} ", env!("CARGO_PKG_VERSION")), theme::muted()),
    ]);
    let tabs = Tabs::new(titles)
        .select(app.tab.index())
        .block(Block::default().borders(Borders::ALL).title(title))
        .highlight_style(theme::active_tab())
        .divider("|");
    frame.render_widget(tabs, chunks[0]);

    // ── body ────────────────────────────────────────────
    match app.tab {
        Tab::Dashboard => dashboard::render(frame, chunks[1], app),
        Tab::Sessions => sessions::render(frame, chunks[1], app),
        Tab::Settings => settings_tab::render(frame, chunks[1], app),
    }

    // ── footer hint ─────────────────────────────────────
    let filter_active = app.session_filter_input && app.tab == Tab::Sessions;
    let mut spans: Vec<Span> = Vec::new();
    if filter_active {
        spans.push(Span::styled(" Esc ", Style::default()));
        spans.push(Span::styled(
            format!("{} ", t("footer.cancel")),
            theme::muted(),
        ));
        spans.push(Span::styled(" Enter ", Style::default()));
        spans.push(Span::styled(
            format!("{} ", t("footer.apply")),
            theme::muted(),
        ));
        spans.push(Span::styled(" ⌫ ", Style::default()));
        spans.push(Span::styled(
            format!("{} ", t("footer.delete")),
            theme::muted(),
        ));
    } else {
        spans.push(Span::styled(" q ", Style::default()));
        spans.push(Span::styled(
            format!("{} ", t("footer.quit")),
            theme::muted(),
        ));
        spans.push(Span::styled(" Tab ", Style::default()));
        spans.push(Span::styled(
            format!("{} ", t("footer.switch")),
            theme::muted(),
        ));
        spans.push(Span::styled(" j/k ", Style::default()));
        spans.push(Span::styled(
            format!("{} ", t("footer.move")),
            theme::muted(),
        ));
        match app.tab {
            Tab::Sessions => {
                spans.push(Span::styled(" f ", Style::default()));
                spans.push(Span::styled(
                    format!("{} ", t("footer.refresh")),
                    theme::muted(),
                ));
                spans.push(Span::styled(" / ", Style::default()));
                spans.push(Span::styled(
                    format!("{} ", t("footer.filter")),
                    theme::muted(),
                ));
                spans.push(Span::styled(" a ", Style::default()));
                spans.push(Span::styled(
                    format!("{} ", t("footer.active_only")),
                    theme::muted(),
                ));
                spans.push(Span::styled(" s ", Style::default()));
                spans.push(Span::styled(
                    format!("{} ", t("footer.sort")),
                    theme::muted(),
                ));
                spans.push(Span::styled(" c ", Style::default()));
                spans.push(Span::styled(
                    format!("{} ", t("footer.clear")),
                    theme::muted(),
                ));
                spans.push(Span::styled(" Enter ", Style::default()));
                spans.push(Span::styled(
                    format!("{} ", t("footer.open_viewer")),
                    theme::muted(),
                ));
                spans.push(Span::styled(" r ", Style::default()));
                spans.push(Span::styled(
                    format!("{} ", t("footer.resume")),
                    theme::muted(),
                ));
            }
            Tab::Dashboard => {
                spans.push(Span::styled(" f ", Style::default()));
                spans.push(Span::styled(
                    format!("{} ", t("footer.refresh")),
                    theme::muted(),
                ));
                spans.push(Span::styled(" Enter ", Style::default()));
                spans.push(Span::styled(
                    format!("{} ", t("footer.jump_session")),
                    theme::muted(),
                ));
            }
            Tab::Settings => {
                spans.push(Span::styled(" ←/→ ", Style::default()));
                spans.push(Span::styled(
                    format!("{} ", t("footer.change")),
                    theme::muted(),
                ));
                spans.push(Span::styled(" r ", Style::default()));
                spans.push(Span::styled("reset ", theme::muted()));
                spans.push(Span::styled(" f ", Style::default()));
                spans.push(Span::styled(
                    format!("{} ", t("footer.refresh")),
                    theme::muted(),
                ));
            }
        }
    }
    let footer = Paragraph::new(Line::from(spans));
    frame.render_widget(footer, chunks[2]);
}
