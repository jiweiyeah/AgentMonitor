//! Settings tab.
//!
//! Each row corresponds to one `SettingsItem`. The UI is a vertical list with:
//! - a label column (left-aligned, localized via i18n)
//! - a value column (left-aligned, shows the current choice + hints at the cycle)
//!
//! Changes persist to disk immediately — there's no save/cancel flow. If a
//! user edits the wrong value by accident they press the same arrow key again
//! and it cycles through; there's nothing to "lose" by ESCing.

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};
use ratatui::Frame;

use crate::app::App;
use crate::i18n::t;
use crate::settings::{
    self, Language, SampleIntervalSecs, Settings, TerminalApp, ThemeColor, TimeFormat, TokenUnit,
};
use crate::tui::theme;
use crate::tui::widgets::pad_display_width;

/// Every setting the user can change via the Settings tab. The `cycle_*`
/// methods mutate the shared global and persist. Keeping this enum (vs. a
/// `Vec<Box<dyn Trait>>`) gives us exhaustive-match safety when a new field
/// is added to `Settings` — the compiler nudges every call site.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsItem {
    Language,
    Theme,
    TimeFormat,
    TokenUnit,
    SampleInterval,
    IncludeCacheInTotal,
    Terminal,
}

impl SettingsItem {
    pub fn all() -> &'static [SettingsItem] {
        &[
            SettingsItem::Language,
            SettingsItem::Theme,
            SettingsItem::TimeFormat,
            SettingsItem::TokenUnit,
            SettingsItem::SampleInterval,
            SettingsItem::IncludeCacheInTotal,
            SettingsItem::Terminal,
        ]
    }

    pub fn label_key(self) -> &'static str {
        match self {
            SettingsItem::Language => "settings.language",
            SettingsItem::Theme => "settings.theme",
            SettingsItem::TimeFormat => "settings.time_format",
            SettingsItem::TokenUnit => "settings.token_unit",
            SettingsItem::SampleInterval => "settings.sample_interval",
            SettingsItem::IncludeCacheInTotal => "settings.include_cache",
            SettingsItem::Terminal => "settings.terminal",
        }
    }

    /// Rendered value for the current state. Strings must live long enough to
    /// be rendered — built fresh per call instead of referencing globals.
    pub fn value_string(self, s: &Settings) -> String {
        match self {
            SettingsItem::Language => s.language.label().to_string(),
            SettingsItem::Theme => s.theme.label().to_string(),
            SettingsItem::TimeFormat => s.time_format.label().to_string(),
            SettingsItem::TokenUnit => s.token_unit.label().to_string(),
            SettingsItem::SampleInterval => s.sample_interval.label(),
            SettingsItem::IncludeCacheInTotal => if s.include_cache_in_total {
                t("settings.on")
            } else {
                t("settings.off")
            }
            .to_string(),
            SettingsItem::Terminal => s.terminal.label().to_string(),
        }
    }

    pub fn cycle_forward(self) {
        settings::update(|s| match self {
            SettingsItem::Language => s.language = s.language.cycle(),
            SettingsItem::Theme => s.theme = s.theme.cycle(),
            SettingsItem::TimeFormat => s.time_format = s.time_format.cycle(),
            SettingsItem::TokenUnit => s.token_unit = s.token_unit.cycle(),
            SettingsItem::SampleInterval => s.sample_interval = s.sample_interval.cycle(),
            SettingsItem::IncludeCacheInTotal => {
                s.include_cache_in_total = !s.include_cache_in_total
            }
            SettingsItem::Terminal => s.terminal = s.terminal.cycle(),
        });
    }

    pub fn cycle_back(self) {
        settings::update(|s| match self {
            SettingsItem::Language => s.language = s.language.cycle_back(),
            SettingsItem::Theme => s.theme = s.theme.cycle_back(),
            SettingsItem::TimeFormat => s.time_format = s.time_format.cycle_back(),
            SettingsItem::TokenUnit => s.token_unit = s.token_unit.cycle_back(),
            SettingsItem::SampleInterval => s.sample_interval = s.sample_interval.cycle_back(),
            SettingsItem::IncludeCacheInTotal => {
                s.include_cache_in_total = !s.include_cache_in_total
            }
            SettingsItem::Terminal => s.terminal = s.terminal.cycle_back(),
        });
    }
}

/// Reset every field to its `Default`. Invoked by `r` on the Settings tab.
pub fn reset_to_defaults() {
    settings::update(|s| {
        *s = Settings {
            language: Language::default(),
            theme: ThemeColor::default(),
            time_format: TimeFormat::default(),
            token_unit: TokenUnit::default(),
            sample_interval: SampleIntervalSecs::default(),
            include_cache_in_total: true,
            terminal: TerminalApp::default(),
        };
    });
}

pub fn render(frame: &mut Frame, area: Rect, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(4),
            Constraint::Length(3),
        ])
        .split(area);

    // Top hint row — explains keyboard shortcuts in whatever language is active.
    let hint = Paragraph::new(Line::from(vec![Span::styled(
        t("settings.hint"),
        theme::muted(),
    )]));
    frame.render_widget(hint, chunks[0]);

    // Main list. ListState tracks selection; the app-level `selected_setting`
    // is authoritative so navigation persists when the tab is toggled away.
    let snapshot = settings::get();
    let items: Vec<ListItem> = SettingsItem::all()
        .iter()
        .map(|item| {
            let label = t(item.label_key());
            let value = item.value_string(&snapshot);
            // Pad by terminal column count (not char count) so CJK labels
            // like "语言" align with "Language" — a Chinese char is 2 cols
            // but 1 `char`, which `{:<N}` gets wrong.
            let line = Line::from(vec![
                Span::styled(
                    pad_display_width(label, 28),
                    Style::default()
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    pad_display_width(&value, 24),
                    Style::default()
                        .fg(theme::accent())
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled("  ← →", theme::muted()),
            ]);
            ListItem::new(line)
        })
        .collect();

    let mut list_state = ListState::default();
    list_state.select(Some(app.selected_setting.min(SettingsItem::all().len() - 1)));

    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(Span::styled(t("settings.title"), theme::title())),
        )
        .highlight_style(theme::selected())
        .highlight_symbol("▶ ");
    frame.render_stateful_widget(list, chunks[1], &mut list_state);

    // Footer note — tiny reminder that one field behaves differently (sample
    // interval takes effect after restart), plus a generic save confirmation.
    let notes = Paragraph::new(vec![
        Line::from(Span::styled(t("settings.saved"), theme::muted())),
        Line::from(Span::styled(t("settings.note_theme"), theme::muted())),
        Line::from(Span::styled(t("settings.note_interval"), theme::muted())),
    ])
    .block(Block::default().borders(Borders::TOP));
    frame.render_widget(notes, chunks[2]);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::{settings, test_lock, Language, ThemeColor};

    #[test]
    fn cycle_forward_changes_language() {
        let _guard = test_lock();
        let original = settings::get().language;
        SettingsItem::Language.cycle_forward();
        let after = settings::get().language;
        assert_ne!(original, after);
        // Restore so other tests relying on default aren't surprised.
        settings().write().language = original;
    }

    #[test]
    fn cycle_forward_and_back_is_identity() {
        let _guard = test_lock();
        let original = settings::get().theme;
        SettingsItem::Theme.cycle_forward();
        SettingsItem::Theme.cycle_back();
        assert_eq!(settings::get().theme, original);
    }

    #[test]
    fn reset_restores_defaults() {
        let _guard = test_lock();
        settings().write().language = Language::Zh;
        settings().write().theme = ThemeColor::Red;
        reset_to_defaults();
        assert_eq!(settings::get().language, Language::default());
        assert_eq!(settings::get().theme, ThemeColor::default());
    }

    #[test]
    fn every_item_exposes_a_non_empty_value() {
        let _guard = test_lock();
        let s = settings::get();
        for item in SettingsItem::all() {
            let v = item.value_string(&s);
            assert!(!v.is_empty(), "{:?} produced empty value", item);
        }
    }
}
