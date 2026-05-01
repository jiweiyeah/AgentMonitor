use std::io::Stdout;

use anyhow::Result;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Tabs, Wrap};
use ratatui::Frame;
use ratatui::Terminal;

use crate::app::{App, Mode, Tab};
use crate::i18n::t;
use crate::settings::{self, KeyAction};
use crate::tui::{dashboard, help, sessions, settings as settings_tab, theme, viewer};

/// Full-frame draw. Called on every input event and every `dirty` notify.
pub async fn draw(terminal: &mut Terminal<CrosstermBackend<Stdout>>, app: &App) -> Result<()> {
    let mode = app.state.read().mode.clone();
    let show_help = app.state.read().show_help;
    terminal.draw(|frame| {
        let area = frame.area();
        match mode {
            Mode::Normal => draw_tabs(frame, area, app),
            Mode::Viewer { .. } => viewer::render(frame, area, app),
        }
        draw_delete_confirm(frame, area, app);
        draw_delete_footer(frame, area, app);
        if show_help {
            help::render(frame, area);
        }
    })?;
    Ok(())
}

fn draw_tabs(frame: &mut Frame, area: Rect, app: &App) {
    let has_toast = app.state.read().toast.is_some();
    let chunks = if has_toast {
        Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3),
                Constraint::Length(1),
                Constraint::Min(4),
                Constraint::Length(1),
            ])
            .split(area)
    } else {
        Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3),
                Constraint::Min(5),
                Constraint::Length(1),
            ])
            .split(area)
    };

    let titles: Vec<Line> = Tab::all()
        .iter()
        .enumerate()
        .map(|(i, t)| Line::from(Span::raw(format!(" {} {} ", i + 1, t.title()))))
        .collect();
    let title = Line::from(vec![
        Span::styled(" Agent Monitor ", theme::title()),
        Span::styled(format!("v{} ", env!("CARGO_PKG_VERSION")), theme::muted()),
    ]);
    let tabs = Tabs::new(titles)
        .select(app.tab.index())
        .block(Block::default().borders(Borders::ALL).title(title))
        .highlight_style(theme::active_tab())
        .divider("|");
    frame.render_widget(tabs, chunks[0]);

    let body_idx = if has_toast { 2 } else { 1 };
    let footer_idx = if has_toast { 3 } else { 2 };

    if has_toast {
        if let Some(ref toast) = app.state.read().toast {
            let toast_line = Line::from(vec![
                Span::styled(" ⭐ ", Style::default().fg(Color::Yellow)),
                Span::styled(toast.clone(), Style::default().fg(Color::White)),
            ]);
            frame.render_widget(
                Paragraph::new(toast_line).style(Style::default().bg(Color::DarkGray)),
                chunks[1],
            );
        }
    }

    match app.tab {
        Tab::Dashboard => dashboard::render(frame, chunks[body_idx], app),
        Tab::Sessions => sessions::render(frame, chunks[body_idx], app),
        Tab::Settings => settings_tab::render(frame, chunks[body_idx], app),
    }

    frame.render_widget(Clear, chunks[footer_idx]);
    frame.render_widget(Paragraph::new(footer_line(app)), chunks[footer_idx]);
}

pub(crate) fn footer_line(app: &App) -> Line<'static> {
    let filter_active = app.session_filter_input && app.tab == Tab::Sessions;
    let keybindings = settings::get().keybindings;
    let mut spans: Vec<Span> = Vec::new();
    if app.delete_confirm.is_some() {
        push_action(
            &mut spans,
            &keybindings,
            KeyAction::DeleteCancel,
            "footer.cancel",
        );
        push_action(
            &mut spans,
            &keybindings,
            KeyAction::DeleteConfirm,
            "footer.delete",
        );
    } else if filter_active {
        push_action(
            &mut spans,
            &keybindings,
            KeyAction::FilterCancel,
            "footer.cancel",
        );
        push_action(
            &mut spans,
            &keybindings,
            KeyAction::FilterApply,
            "footer.apply",
        );
        push_action(
            &mut spans,
            &keybindings,
            KeyAction::FilterDeleteChar,
            "footer.delete",
        );
    } else {
        push_action(&mut spans, &keybindings, KeyAction::Star, "footer.star");
        push_action(&mut spans, &keybindings, KeyAction::Quit, "footer.quit");
        push_action(
            &mut spans,
            &keybindings,
            KeyAction::TabNext,
            "footer.switch",
        );
        push_action(&mut spans, &keybindings, KeyAction::MoveDown, "footer.move");
        match app.tab {
            Tab::Sessions => {
                push_action(
                    &mut spans,
                    &keybindings,
                    KeyAction::Refresh,
                    "footer.refresh",
                );
                push_action(
                    &mut spans,
                    &keybindings,
                    KeyAction::SessionsStartFilter,
                    "footer.filter",
                );
                push_action(
                    &mut spans,
                    &keybindings,
                    KeyAction::SessionsToggleActiveOnly,
                    "footer.active_only",
                );
                push_action(
                    &mut spans,
                    &keybindings,
                    KeyAction::SessionsCycleSort,
                    "footer.sort",
                );
                push_action(
                    &mut spans,
                    &keybindings,
                    KeyAction::SessionsClearFilter,
                    "footer.clear",
                );
                push_action(
                    &mut spans,
                    &keybindings,
                    KeyAction::SessionsOpenViewer,
                    "footer.open_viewer",
                );
                push_action(
                    &mut spans,
                    &keybindings,
                    KeyAction::SessionsResume,
                    "footer.resume",
                );
                push_action(
                    &mut spans,
                    &keybindings,
                    KeyAction::SessionsDelete,
                    "footer.delete",
                );
            }
            Tab::Dashboard => {
                push_action(
                    &mut spans,
                    &keybindings,
                    KeyAction::Refresh,
                    "footer.refresh",
                );
                push_action(
                    &mut spans,
                    &keybindings,
                    KeyAction::DashboardJumpSession,
                    "footer.jump_session",
                );
            }
            Tab::Settings => {
                if app.settings_keybindings_open {
                    push_action(
                        &mut spans,
                        &keybindings,
                        KeyAction::SettingsActivate,
                        "footer.change",
                    );
                    push_action(
                        &mut spans,
                        &keybindings,
                        KeyAction::SettingsReset,
                        "footer.reset",
                    );
                    push_action(&mut spans, &keybindings, KeyAction::MoveDown, "footer.move");
                } else {
                    push_action(
                        &mut spans,
                        &keybindings,
                        KeyAction::SettingsChangeNext,
                        "footer.change",
                    );
                    push_action(
                        &mut spans,
                        &keybindings,
                        KeyAction::SettingsReset,
                        "footer.reset",
                    );
                    push_action(
                        &mut spans,
                        &keybindings,
                        KeyAction::Refresh,
                        "footer.refresh",
                    );
                }
            }
        }
    }
    Line::from(spans)
}

fn push_action(
    spans: &mut Vec<Span<'static>>,
    keybindings: &crate::settings::KeyBindings,
    action: KeyAction,
    label_key: &str,
) {
    spans.push(Span::styled(
        format!(" {} ", keybindings.binding_display(action)),
        Style::default(),
    ));
    spans.push(Span::styled(format!("{} ", t(label_key)), theme::muted()));
}

fn delete_confirm_hint(keybindings: &crate::settings::KeyBindings) -> String {
    format!(
        "{} {} · {} {}",
        keybindings.binding_display(KeyAction::DeleteConfirm),
        t("footer.delete"),
        keybindings.binding_display(KeyAction::DeleteCancel),
        t("footer.cancel")
    )
}

fn draw_delete_footer(frame: &mut Frame, area: Rect, app: &App) {
    if app.delete_confirm.is_none() {
        return;
    }
    let footer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(area)[1];
    frame.render_widget(Clear, footer);
    frame.render_widget(Paragraph::new(footer_line(app)), footer);
}
fn draw_delete_confirm(frame: &mut Frame, area: Rect, app: &App) {
    let Some(confirm) = app.delete_confirm.as_ref() else {
        return;
    };

    let (session_label, cwd_label) = {
        let state = app.state.read();
        let meta = state
            .sessions
            .iter()
            .find(|session| session.path == confirm.path);
        let session_label = meta
            .map(|session| session.id.clone())
            .or_else(|| {
                confirm
                    .path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .map(str::to_owned)
            })
            .unwrap_or_else(|| confirm.path.display().to_string());
        let cwd_label = meta.map(|session| session.cwd_display());
        (session_label, cwd_label)
    };

    let popup = centered_rect(76, 8, area);
    let mut lines = vec![
        Line::from(t("sessions.delete_body")),
        Line::from(vec![
            Span::styled(t("sessions.delete_target"), theme::muted()),
            Span::raw(session_label),
        ]),
    ];
    if let Some(cwd) = cwd_label {
        lines.push(Line::from(vec![
            Span::styled(t("sessions.delete_cwd"), theme::muted()),
            Span::raw(cwd),
        ]));
    }
    lines.push(Line::from(vec![
        Span::styled(t("sessions.delete_path"), theme::muted()),
        Span::raw(confirm.path.display().to_string()),
    ]));
    if let Some(err) = &confirm.error {
        lines.push(Line::from(Span::styled(
            format!("{}: {err}", t("sessions.delete_error")),
            Style::default().fg(Color::Red),
        )));
    }
    let keybindings = settings::get().keybindings;
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        delete_confirm_hint(&keybindings),
        Style::default().fg(theme::accent()),
    )));

    frame.render_widget(Clear, popup);
    frame.render_widget(
        Paragraph::new(lines).wrap(Wrap { trim: true }).block(
            Block::default()
                .borders(Borders::ALL)
                .title(Span::styled(t("sessions.delete_title"), theme::title())),
        ),
        popup,
    );
}

fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
    let available_width = area.width.saturating_sub(2).max(1);
    let available_height = area.height.saturating_sub(2).max(1);
    let popup_width = width.min(available_width).max(available_width.min(24));
    let popup_height = height.min(available_height).max(available_height.min(5));
    let x = area.x + area.width.saturating_sub(popup_width) / 2;
    let y = area.y + area.height.saturating_sub(popup_height) / 2;
    Rect::new(x, y, popup_width, popup_height)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::DeleteConfirm;
    use crate::collector::metrics::MetricsStore;
    use crate::collector::token_refresh::TokenCache;
    use crate::config::Config;
    use parking_lot::RwLock;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use std::path::PathBuf;
    use std::sync::Arc;

    fn test_app() -> App {
        App {
            config: Config::default(),
            state: Arc::new(RwLock::new(crate::app::AppState::default())),
            metrics: Arc::new(MetricsStore::new(8)),
            adapters: Vec::new(),
            tab: Tab::Dashboard,
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
            dirty: Arc::new(tokio::sync::Notify::new()),
            token_dirty: Arc::new(tokio::sync::Notify::new()),
        }
    }

    fn buffer_row(buffer: &ratatui::buffer::Buffer, y: u16) -> String {
        (0..buffer.area.width)
            .map(|x| buffer[(x, y)].symbol())
            .collect()
    }

    #[test]
    fn delete_confirm_overlays_footer_with_confirm_actions() {
        let _guard = crate::settings::test_lock();
        let original = crate::settings::get().keybindings;
        crate::settings::settings().write().keybindings = crate::settings::KeyBindings::default();

        let mut app = test_app();
        app.state.write().mode = Mode::Viewer {
            path: PathBuf::from("/tmp/session.jsonl"),
        };
        app.delete_confirm = Some(DeleteConfirm::new(PathBuf::from("/tmp/session.jsonl")));

        let backend = TestBackend::new(100, 20);
        let mut terminal = Terminal::new(backend).unwrap();
        let frame = terminal
            .draw(|frame| {
                viewer::render(frame, frame.area(), &app);
                draw_delete_confirm(frame, frame.area(), &app);
                draw_delete_footer(frame, frame.area(), &app);
            })
            .unwrap();
        let footer_text = buffer_row(frame.buffer, frame.area.height - 1);

        assert!(footer_text.contains("Enter/y delete"));
        assert!(footer_text.contains("Esc/q cancel"));
        assert!(!footer_text.contains("expand/collapse"));

        crate::settings::settings().write().keybindings = original;
    }
}
