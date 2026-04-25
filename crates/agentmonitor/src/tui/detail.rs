use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use ratatui::Frame;

use crate::adapter::types::{MessagePreview, MessageRole, SessionMeta};
use crate::app::PreviewCache;
use crate::i18n::t;
use crate::settings;
use crate::tui::theme;
use crate::tui::widgets::{status_span, token_bar_line};

pub fn render(frame: &mut Frame, area: Rect, s: &SessionMeta, preview: Option<&PreviewCache>) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(13), Constraint::Min(3)])
        .split(area);

    render_meta(frame, chunks[0], s, preview);
    render_preview(frame, chunks[1], preview);
}

fn render_meta(frame: &mut Frame, area: Rect, s: &SessionMeta, preview: Option<&PreviewCache>) {
    let long_pat = settings::get().time_format.pattern_long();
    let started = s
        .started_at
        .map(|t| t.with_timezone(&chrono::Local).format(long_pat).to_string())
        .unwrap_or_else(|| "-".into());
    let updated = s
        .updated_at
        .map(|t| t.with_timezone(&chrono::Local).format(long_pat).to_string())
        .unwrap_or_else(|| "-".into());

    // If the full parse has finished, prefer its message_count; otherwise
    // show an ellipsis so users understand it's loading.
    let (count_label, count_style) = match preview {
        Some(p) if p.path == s.path && !p.loading => (
            format!("{}", p.message_count),
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
        Some(p) if p.path == s.path && p.loading => ("…".to_string(), theme::muted()),
        _ => ("-".to_string(), theme::muted()),
    };

    let lines = vec![
        Line::from(vec![
            Span::styled(t("detail.agent"), theme::muted()),
            Span::raw(s.agent_label()),
            Span::raw("    "),
            status_span(s.status),
        ]),
        Line::from(vec![
            Span::styled(t("detail.session"), theme::muted()),
            Span::raw(s.id.clone()),
        ]),
        Line::from(vec![
            Span::styled(t("detail.cwd"), theme::muted()),
            Span::raw(s.cwd_display()),
        ]),
        Line::from(vec![
            Span::styled(t("detail.model"), theme::muted()),
            Span::raw(s.model.clone().unwrap_or_else(|| "-".into())),
            Span::styled(t("detail.version"), theme::muted()),
            Span::raw(s.version.clone().unwrap_or_else(|| "-".into())),
        ]),
        Line::from(vec![
            Span::styled(t("detail.branch"), theme::muted()),
            Span::raw(s.git_branch.clone().unwrap_or_else(|| "-".into())),
        ]),
        Line::from(vec![
            Span::styled(t("detail.started"), theme::muted()),
            Span::raw(started),
        ]),
        Line::from(vec![
            Span::styled(t("detail.updated"), theme::muted()),
            Span::raw(updated),
        ]),
        Line::from(vec![
            Span::styled(t("detail.messages"), theme::muted()),
            Span::styled(count_label, count_style),
            Span::styled(t("detail.file_size"), theme::muted()),
            Span::raw(format_bytes(s.size_bytes)),
        ]),
        Line::from(""),
        Line::from(Span::styled(t("detail.tokens"), theme::title())),
        token_bar_line(
            s.tokens.input,
            s.tokens.cache_read,
            s.tokens.cache_creation,
            s.tokens.output,
        ),
        Line::from(Span::styled(
            t("detail.resume_hint"),
            Style::default().fg(theme::accent()),
        )),
    ];

    let para = Paragraph::new(lines).block(
        Block::default()
            .borders(Borders::ALL)
            .title(Span::styled(t("sessions.detail"), theme::title())),
    );
    frame.render_widget(para, area);
}

fn render_preview(frame: &mut Frame, area: Rect, preview: Option<&PreviewCache>) {
    let (lines, title) = match preview {
        None => (
            vec![Line::from(Span::styled(
                t("sessions.preview_select"),
                theme::muted(),
            ))],
            t("sessions.recent_messages").to_string(),
        ),
        Some(p) if p.loading => (
            vec![Line::from(Span::styled(
                t("sessions.preview_loading"),
                theme::muted(),
            ))],
            t("sessions.recent_messages_loading").to_string(),
        ),
        Some(p) if p.messages.is_empty() => (
            vec![Line::from(Span::styled(
                t("sessions.preview_no_messages"),
                theme::muted(),
            ))],
            t("sessions.recent_messages").to_string(),
        ),
        Some(p) => {
            let mut lines = Vec::new();
            for m in &p.messages {
                lines.extend(message_to_lines(m));
                lines.push(Line::from(""));
            }
            (
                lines,
                format!("{}({}) ", t("sessions.recent_messages"), p.messages.len()),
            )
        }
    };
    let para = Paragraph::new(lines).wrap(Wrap { trim: false }).block(
        Block::default()
            .borders(Borders::ALL)
            .title(Span::styled(title, theme::title())),
    );
    frame.render_widget(para, area);
}

fn message_to_lines(m: &MessagePreview) -> Vec<Line<'static>> {
    let role_style = match m.role {
        MessageRole::User => Style::default()
            .fg(theme::SUCCESS)
            .add_modifier(Modifier::BOLD),
        MessageRole::Assistant => Style::default()
            .fg(theme::accent())
            .add_modifier(Modifier::BOLD),
        MessageRole::System => Style::default().fg(theme::WARN),
        _ => theme::muted(),
    };
    let short_pat = settings::get().time_format.pattern_short();
    let when =
        m.ts.map(|t| {
            t.with_timezone(&chrono::Local)
                .format(short_pat)
                .to_string()
        })
        .unwrap_or_else(|| "".into());
    let header = Line::from(vec![
        Span::styled(format!("[{}] ", m.role.label()), role_style),
        Span::styled(when, theme::muted()),
    ]);
    let body = m.text.clone();
    let mut out = vec![header];
    for body_line in body.split('\n').take(6) {
        out.push(Line::from(Span::raw(body_line.to_string())));
    }
    out
}

fn format_bytes(n: u64) -> String {
    if n < 1024 {
        format!("{n} B")
    } else if n < 1024 * 1024 {
        format!("{:.1} KB", n as f64 / 1024.0)
    } else {
        format!("{:.2} MB", n as f64 / (1024.0 * 1024.0))
    }
}
