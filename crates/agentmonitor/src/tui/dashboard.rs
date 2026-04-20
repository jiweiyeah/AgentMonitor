use std::collections::HashMap;

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;

use crate::app::App;
use crate::tui::theme;
use crate::tui::widgets::human_bytes;

pub fn render(frame: &mut Frame, area: Rect, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(8), Constraint::Min(3)])
        .split(area);

    let state = app.state.read();
    let total_sessions = state.sessions.len();
    let by_agent: HashMap<&str, usize> =
        state.sessions.iter().fold(HashMap::new(), |mut acc, s| {
            *acc.entry(s.agent).or_insert(0) += 1;
            acc
        });

    let now = chrono::Utc::now();
    let today_count = state
        .sessions
        .iter()
        .filter(|s| {
            s.updated_at
                .map(|t| (now - t).num_hours() < 24)
                .unwrap_or(false)
        })
        .count();

    let procs = app.metrics.snapshot();
    let total_rss = app.metrics.total_rss_kb();

    let lines = vec![
        Line::from(vec![
            Span::styled("Total sessions  ", theme::muted()),
            Span::styled(
                format!("{total_sessions}"),
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled("   last 24h  ", theme::muted()),
            Span::styled(
                format!("{today_count}"),
                Style::default()
                    .fg(theme::SUCCESS)
                    .add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::from(vec![
            Span::styled("By agent        ", theme::muted()),
            agent_chips(&by_agent, &app.adapters),
        ]),
        Line::from(vec![
            Span::styled("Live processes  ", theme::muted()),
            Span::styled(
                format!("{}", procs.len()),
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled("   aggregate RSS  ", theme::muted()),
            Span::styled(
                human_bytes(total_rss),
                Style::default()
                    .fg(theme::ACCENT)
                    .add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::from(vec![
            Span::styled("Sample interval ", theme::muted()),
            Span::raw(format!("{}s", app.config.sample_interval.as_secs())),
            Span::styled("   retention   ", theme::muted()),
            Span::raw(format!("{} samples", app.config.metrics_capacity)),
        ]),
    ];

    let summary = Paragraph::new(lines).block(
        Block::default()
            .borders(Borders::ALL)
            .title(Span::styled(" Overview ", theme::title())),
    );
    frame.render_widget(summary, chunks[0]);

    // Recent sessions list (compact).
    let recent: Vec<Line> = state
        .sessions
        .iter()
        .take(chunks[1].height.saturating_sub(2) as usize)
        .map(|s| {
            Line::from(vec![
                Span::styled(
                    format!(" {:<10}  ", s.agent_label()),
                    Style::default().fg(theme::ACCENT),
                ),
                Span::raw(format!("{}  ", s.short_id())),
                Span::styled(
                    s.updated_at
                        .map(|t| t.format("%m-%d %H:%M").to_string())
                        .unwrap_or_else(|| "-----".into()),
                    theme::muted(),
                ),
                Span::raw("  "),
                Span::raw(shorten(&s.cwd_display(), 40)),
            ])
        })
        .collect();

    let list = Paragraph::new(recent).block(
        Block::default()
            .borders(Borders::ALL)
            .title(Span::styled(" Recent ", theme::title())),
    );
    frame.render_widget(list, chunks[1]);
}

fn agent_chips(
    by_agent: &HashMap<&str, usize>,
    adapters: &[crate::adapter::DynAdapter],
) -> Span<'static> {
    let mut parts = Vec::new();
    for a in adapters {
        let count = by_agent.get(a.id()).copied().unwrap_or(0);
        parts.push(format!("{}={}", a.display_name(), count));
    }
    Span::styled(parts.join("  "), Style::default().fg(Color::White))
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
