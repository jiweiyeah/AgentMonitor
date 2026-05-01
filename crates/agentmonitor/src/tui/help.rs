//! Help overlay (`?`).
//!
//! Renders a centered modal listing every `KeyAction`'s current binding,
//! grouped by `KeyContext`. Painted on top of whatever tab/mode the user is
//! in — including the viewer — by `tui::render::draw` after the body. Any
//! keypress while the overlay is up dismisses it and is *not* dispatched to
//! the underlying tab (handled in `event.rs::handle_event`).
//!
//! Design notes:
//!
//! - We never look up bindings on the hot path from external callers; this
//!   render runs only when `state.show_help == true`, so a bit of formatting
//!   work per frame is fine.
//! - Group order mirrors the order of `KeyContext` matches in
//!   `KeyAction::context()` so the overlay always reads top-to-bottom from
//!   most-global → most-specific.
//! - When the modal would not fit (small terminal), we shrink the inner
//!   layout to body-only and let lines truncate gracefully via `Paragraph`'s
//!   default behavior. We avoid scrolling the modal because users dismiss
//!   it on next keypress anyway.

use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::Frame;

use crate::i18n::t;
use crate::keybinding::{KeyAction, KeyContext};
use crate::settings;
use crate::tui::theme;

/// Paint the overlay. Call only when `state.show_help` is true.
pub fn render(frame: &mut Frame, area: Rect) {
    let modal = centered_rect(area, 80, 80);
    let bindings = settings::get().keybindings;

    // Clear the underlying body so the modal looks like a real popover.
    frame.render_widget(Clear, modal);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(3), Constraint::Length(1)])
        .split(modal);

    let mut lines: Vec<Line<'static>> = Vec::new();
    for context in CONTEXT_ORDER {
        lines.push(Line::from(Span::styled(
            format!(" {} ", t(context.label_key())),
            Style::default()
                .fg(theme::accent())
                .add_modifier(Modifier::BOLD),
        )));
        for action in KeyAction::all() {
            if action.context() != *context {
                continue;
            }
            let key_display = bindings.binding_display(*action);
            let display = if key_display == "unbound" {
                t("help.unbound").to_string()
            } else {
                key_display
            };
            let row = Line::from(vec![
                Span::raw("  "),
                Span::styled(
                    format!("{:<14}", display),
                    Style::default()
                        .fg(theme::accent())
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(" "),
                Span::raw(t(action.label_key()).to_string()),
            ]);
            lines.push(row);
        }
        lines.push(Line::from(""));
    }

    let body = Paragraph::new(lines).block(
        Block::default()
            .borders(Borders::ALL)
            .title(Span::styled(t("help.title"), theme::title())),
    );
    frame.render_widget(body, chunks[0]);

    let hint = Paragraph::new(Line::from(Span::styled(
        t("help.dismiss_hint"),
        theme::muted(),
    )))
    .alignment(Alignment::Center);
    frame.render_widget(hint, chunks[1]);
}

/// Order in which contexts appear in the overlay. Global first, then user-
/// visible tabs, then modal contexts that the user invokes from elsewhere.
const CONTEXT_ORDER: &[KeyContext] = &[
    KeyContext::Global,
    KeyContext::Dashboard,
    KeyContext::Sessions,
    KeyContext::Viewer,
    KeyContext::Settings,
    KeyContext::FilterInput,
    KeyContext::DeleteConfirm,
];

/// Center-clipped rect: returns a rectangle of `pct_x`% × `pct_y`% of `area`,
/// anchored at the center. Used by the modal layout.
fn centered_rect(area: Rect, pct_x: u16, pct_y: u16) -> Rect {
    let pop_w = area.width.saturating_mul(pct_x) / 100;
    let pop_h = area.height.saturating_mul(pct_y) / 100;
    let x = area.x + (area.width.saturating_sub(pop_w)) / 2;
    let y = area.y + (area.height.saturating_sub(pop_h)) / 2;
    Rect::new(x, y, pop_w, pop_h)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn centered_rect_returns_centered_subarea() {
        let area = Rect::new(0, 0, 100, 40);
        let inner = centered_rect(area, 80, 80);
        assert_eq!(inner.width, 80);
        assert_eq!(inner.height, 32);
        assert_eq!(inner.x, 10);
        assert_eq!(inner.y, 4);
    }

    #[test]
    fn centered_rect_handles_zero_dimensions() {
        let area = Rect::new(0, 0, 0, 0);
        let inner = centered_rect(area, 80, 80);
        assert_eq!(inner.width, 0);
        assert_eq!(inner.height, 0);
    }

    #[test]
    fn context_order_covers_every_known_context() {
        // Every context referenced by an action's `context()` must appear in
        // the overlay's order list, otherwise that group of actions would be
        // silently hidden in the help modal.
        let in_order: std::collections::HashSet<KeyContext> = CONTEXT_ORDER.iter().copied().collect();
        for action in KeyAction::all() {
            let ctx = action.context();
            assert!(
                in_order.contains(&ctx),
                "KeyContext {:?} (used by action {:?}) is missing from CONTEXT_ORDER",
                ctx,
                action
            );
        }
    }
}
