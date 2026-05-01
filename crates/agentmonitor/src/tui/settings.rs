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
use crate::settings::{self, KeyAction, Settings};
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
    StarStatus,
    Keybindings,
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
            SettingsItem::Keybindings,
            SettingsItem::StarStatus,
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
            SettingsItem::StarStatus => "settings.star_status",
            SettingsItem::Keybindings => "settings.keybindings",
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
            SettingsItem::StarStatus => {
                if crate::settings::which_exists("gh") {
                    t(s.star_status.label()).to_string()
                } else {
                    t("settings.star_browser_only").to_string()
                }
            }
            SettingsItem::Keybindings => {
                format!("{} actions", crate::settings::KeyAction::all().len())
            }
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
            SettingsItem::StarStatus => {}
            SettingsItem::Keybindings => {}
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
            SettingsItem::StarStatus => {}
            SettingsItem::Keybindings => {}
        });
    }
}

/// Reset every field to its `Default`. Invoked by `r` on the Settings tab.
pub fn reset_to_defaults() {
    settings::update(|s| {
        *s = Settings::default();
    });
}

pub fn render(frame: &mut Frame, area: Rect, app: &App) {
    if app.settings_keybindings_open {
        render_keybindings(frame, area, app);
        return;
    }

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(4),
            Constraint::Length(7),
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
    list_state.select(Some(
        app.selected_setting.min(SettingsItem::all().len()),
    ));

    let mut items = items;
    // GitHub link row. We render plain text — embedding OSC 8 hyperlink escapes
    // (`\x1b]8;;<URL>\x1b\\…\x1b]8;;\x1b\\`) inside a `Span::styled` does NOT
    // work: ratatui's `Buffer::set_stringn` filters out every grapheme that
    // contains a control character (see ratatui/ratatui#902), which strips the
    // `\x1b` bytes but leaves `]8;;https://…\` and the closing `]8;;\` as
    // visible text. The result is a value column wildly wider than 24 cells
    // that overflows neighboring spans and renders as different garbage on
    // every redraw depending on terminal width and clipping.
    //
    // Pressing Enter on this row already opens the browser via the keymap, so
    // the loss of click-to-open in OSC-8-aware terminals is acceptable; the
    // alternative (per-cell `set_symbol` hack from ratatui's hyperlink example)
    // would push raw escapes through a side channel and is overkill here.
    items.push(ListItem::new(Line::from(vec![
        Span::styled(
            pad_display_width("GitHub", 28),
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            pad_display_width("github.com/jiweiyeah/AgentMonitor", 36),
            Style::default()
                .fg(theme::accent())
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("  Enter"),
    ])));

    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(Span::styled(t("settings.title"), theme::title())),
        )
        .highlight_style(theme::selected())
        .highlight_symbol("▶ ");
    frame.render_stateful_widget(list, chunks[1], &mut list_state);

    // Diagnostics panel — read-only snapshot of collector counters. Helps
    // when triaging "tokens look frozen" reports without needing --debug
    // logs. The raw atomics are loaded once into a snapshot struct so the
    // five values that share lock cycles look consistent on the screen.
    let diag = app.diagnostics.snapshot();
    let avg_ms = diag.token_refresh_avg_ms();
    let cache_pct = diag.token_cache_hit_rate() * 100.0;
    let debounce_pct = diag.fs_watch_debounce_ratio() * 100.0;
    let diagnostics_lines = vec![
        Line::from(vec![
            Span::styled(t("settings.diagnostics.title"), theme::title()),
        ]),
        Line::from(vec![
            Span::styled(
                pad_display_width(t("settings.diagnostics.token_refresh"), 28),
                theme::muted(),
            ),
            Span::raw(format!(
                "{} passes · avg {:.1}ms · {} writes",
                diag.token_refresh_passes, avg_ms, diag.token_refresh_writes,
            )),
        ]),
        Line::from(vec![
            Span::styled(
                pad_display_width(t("settings.diagnostics.token_cache"), 28),
                theme::muted(),
            ),
            Span::raw(format!(
                "{:.0}% hits ({}/{})",
                cache_pct,
                diag.token_cache_hits,
                diag.token_cache_hits + diag.token_cache_misses,
            )),
        ]),
        Line::from(vec![
            Span::styled(
                pad_display_width(t("settings.diagnostics.fs_watch"), 28),
                theme::muted(),
            ),
            Span::raw(format!(
                "{} events → {} processed ({:.0}% past debounce) · {} reconciles",
                diag.fs_watch_events,
                diag.fs_watch_paths_processed,
                debounce_pct,
                diag.fs_watch_reconciles,
            )),
        ]),
    ];
    frame.render_widget(
        Paragraph::new(diagnostics_lines).block(Block::default().borders(Borders::TOP)),
        chunks[2],
    );

    // Footer note — tiny reminder that one field behaves differently (sample
    // interval takes effect after restart), plus a generic save confirmation.
    let notes = Paragraph::new(vec![
        Line::from(Span::styled(t("settings.saved"), theme::muted())),
        Line::from(Span::styled(t("settings.note_theme"), theme::muted())),
        Line::from(Span::styled(t("settings.note_interval"), theme::muted())),
    ])
    .block(Block::default().borders(Borders::TOP));
    frame.render_widget(notes, chunks[3]);
}

fn render_keybindings(frame: &mut Frame, area: Rect, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(4),
            Constraint::Length(3),
        ])
        .split(area);

    let snapshot = settings::get();
    let hint = if let Some(action) = app.capturing_keybinding {
        format!(
            "{}: {}",
            t("settings.keybindings_capture"),
            t(action.label_key())
        )
    } else {
        t("settings.keybindings_hint").to_string()
    };
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(hint, theme::muted()))),
        chunks[0],
    );

    let items: Vec<ListItem> = KeyAction::all()
        .iter()
        .map(|action| {
            let line = Line::from(vec![
                Span::styled(
                    pad_display_width(t(action.context().label_key()), 16),
                    theme::muted(),
                ),
                Span::styled(
                    pad_display_width(t(action.label_key()), 34),
                    Style::default()
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    snapshot.keybindings.binding_display(*action),
                    Style::default()
                        .fg(theme::accent())
                        .add_modifier(Modifier::BOLD),
                ),
            ]);
            ListItem::new(line)
        })
        .collect();

    let mut list_state = ListState::default();
    list_state.select(Some(
        app.selected_keybinding.min(KeyAction::all().len() - 1),
    ));
    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title(Span::styled(
            t("settings.keybindings_title"),
            theme::title(),
        )))
        .highlight_style(theme::selected())
        .highlight_symbol("▶ ");
    frame.render_stateful_widget(list, chunks[1], &mut list_state);

    let mut lines = vec![Line::from(Span::styled(
        t("settings.keybindings_saved"),
        theme::muted(),
    ))];
    if let Some(conflict) = app.keybinding_conflict {
        lines.push(Line::from(Span::styled(
            format!(
                "{} {}",
                t("settings.keybindings_replaced"),
                t(conflict.label_key())
            ),
            Style::default().fg(Color::Yellow),
        )));
    } else {
        lines.push(Line::from(Span::styled(
            t("settings.keybindings_note"),
            theme::muted(),
        )));
    }
    frame.render_widget(
        Paragraph::new(lines).block(Block::default().borders(Borders::TOP)),
        chunks[2],
    );
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
    fn keybindings_item_is_renderable() {
        let _guard = test_lock();
        let s = settings::get();
        assert!(SettingsItem::all().contains(&SettingsItem::Keybindings));
        assert!(!SettingsItem::Keybindings.value_string(&s).is_empty());
        assert_ne!(SettingsItem::Keybindings.label_key(), "??");
    }

    #[test]
    fn reset_restores_default_keybindings() {
        let _guard = test_lock();
        settings()
            .write()
            .keybindings
            .clear_binding(crate::settings::KeyAction::Quit);
        reset_to_defaults();
        assert!(settings::get()
            .keybindings
            .bindings_for(crate::settings::KeyAction::Quit)
            .iter()
            .any(|binding| binding.display() == "q"));
    }
}
