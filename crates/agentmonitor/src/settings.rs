//! User-facing preferences: language, theme color, display formats.
//!
//! Persisted as JSON under `$XDG_CONFIG_HOME/agent-monitor/settings.json`
//! (macOS: `~/Library/Application Support/dev.agentmonitor.agent-monitor/`).
//! Read through a global `OnceLock<RwLock<_>>` so theme/i18n helpers stay
//! call-site-friendly without threading an extra handle everywhere.
//!
//! Load failures fall back silently to defaults — a malformed settings file
//! should never prevent the TUI from starting.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use parking_lot::RwLock;
use ratatui::style::Color;
use serde::{Deserialize, Serialize};

pub use crate::keybinding::{KeyAction, KeyBinding, KeyBindings, KeyCodeSpec, KeyContext};

/// UI language. Extend this enum when adding a new locale; both `i18n::t_*`
/// dispatches and the Settings tab cycle list read from `all()`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Language {
    #[default]
    En,
    Zh,
}

impl Language {
    pub fn all() -> &'static [Language] {
        &[Language::En, Language::Zh]
    }
    pub fn label(self) -> &'static str {
        match self {
            Language::En => "English",
            Language::Zh => "中文",
        }
    }
    pub fn cycle(self) -> Self {
        let all = Self::all();
        let idx = all.iter().position(|&l| l == self).unwrap_or(0);
        all[(idx + 1) % all.len()]
    }
    pub fn cycle_back(self) -> Self {
        let all = Self::all();
        let idx = all.iter().position(|&l| l == self).unwrap_or(0);
        all[(idx + all.len() - 1) % all.len()]
    }
}

/// Accent palette used for titles, active tab, highlighted tokens. The full
/// color wheel is available so users can dodge clashes with their terminal's
/// background or their agent-specific muscle memory.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThemeColor {
    #[default]
    Cyan,
    Green,
    Blue,
    Magenta,
    Yellow,
    Red,
    White,
}

impl ThemeColor {
    pub fn all() -> &'static [ThemeColor] {
        &[
            ThemeColor::Cyan,
            ThemeColor::Green,
            ThemeColor::Blue,
            ThemeColor::Magenta,
            ThemeColor::Yellow,
            ThemeColor::Red,
            ThemeColor::White,
        ]
    }
    pub fn label(self) -> &'static str {
        match self {
            ThemeColor::Cyan => "cyan",
            ThemeColor::Green => "green",
            ThemeColor::Blue => "blue",
            ThemeColor::Magenta => "magenta",
            ThemeColor::Yellow => "yellow",
            ThemeColor::Red => "red",
            ThemeColor::White => "white",
        }
    }
    pub fn to_color(self) -> Color {
        match self {
            ThemeColor::Cyan => Color::Cyan,
            ThemeColor::Green => Color::Green,
            ThemeColor::Blue => Color::Blue,
            ThemeColor::Magenta => Color::Magenta,
            ThemeColor::Yellow => Color::Yellow,
            ThemeColor::Red => Color::Red,
            ThemeColor::White => Color::White,
        }
    }
    pub fn cycle(self) -> Self {
        let all = Self::all();
        let idx = all.iter().position(|&c| c == self).unwrap_or(0);
        all[(idx + 1) % all.len()]
    }
    pub fn cycle_back(self) -> Self {
        let all = Self::all();
        let idx = all.iter().position(|&c| c == self).unwrap_or(0);
        all[(idx + all.len() - 1) % all.len()]
    }
}

/// Clock format used throughout the UI. The `chrono::format!` strings that
/// translate this enum live in `time_format_pattern`; callers never branch on
/// the enum directly.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TimeFormat {
    #[default]
    Hr24,
    Hr12,
}

impl TimeFormat {
    pub fn all() -> &'static [TimeFormat] {
        &[TimeFormat::Hr24, TimeFormat::Hr12]
    }
    pub fn label(self) -> &'static str {
        match self {
            TimeFormat::Hr24 => "24h",
            TimeFormat::Hr12 => "12h",
        }
    }
    /// Pattern for `chrono::format` — short (Sessions list / detail).
    pub fn pattern_short(self) -> &'static str {
        match self {
            TimeFormat::Hr24 => "%m-%d %H:%M",
            TimeFormat::Hr12 => "%m-%d %I:%M%p",
        }
    }
    /// Pattern for `chrono::format` — long (Viewer / Detail started/updated).
    pub fn pattern_long(self) -> &'static str {
        match self {
            TimeFormat::Hr24 => "%Y-%m-%d %H:%M:%S",
            TimeFormat::Hr12 => "%Y-%m-%d %I:%M:%S%p",
        }
    }
    pub fn cycle(self) -> Self {
        let all = Self::all();
        let idx = all.iter().position(|&t| t == self).unwrap_or(0);
        all[(idx + 1) % all.len()]
    }
    pub fn cycle_back(self) -> Self {
        self.cycle()
    }
}

/// Rendering mode for token counts in aggregate panels.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TokenUnit {
    #[default]
    Compact,
    Raw,
}

impl TokenUnit {
    pub fn all() -> &'static [TokenUnit] {
        &[TokenUnit::Compact, TokenUnit::Raw]
    }
    pub fn label(self) -> &'static str {
        match self {
            TokenUnit::Compact => "compact (K/M/B)",
            TokenUnit::Raw => "raw",
        }
    }
    pub fn cycle(self) -> Self {
        let all = Self::all();
        let idx = all.iter().position(|&t| t == self).unwrap_or(0);
        all[(idx + 1) % all.len()]
    }
    pub fn cycle_back(self) -> Self {
        self.cycle()
    }
}

/// Terminal application used to resume sessions. Only terminals that are
/// detected on the current system appear in the Settings UI cycle list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TerminalApp {
    Terminal,
    Ghostty,
    ITerm2,
    Alacritty,
    Kitty,
    WezTerm,
    Warp,
}

impl Default for TerminalApp {
    fn default() -> Self {
        // Pick the first available terminal, falling back to Terminal on macOS
        // or the first CLI-based option on Linux.
        Self::detect()
            .first()
            .copied()
            .unwrap_or(if cfg!(target_os = "macos") {
                TerminalApp::Terminal
            } else {
                TerminalApp::Alacritty
            })
    }
}

impl TerminalApp {
    pub fn all() -> &'static [TerminalApp] {
        &[
            TerminalApp::Terminal,
            TerminalApp::Ghostty,
            TerminalApp::ITerm2,
            TerminalApp::Alacritty,
            TerminalApp::Kitty,
            TerminalApp::WezTerm,
            TerminalApp::Warp,
        ]
    }

    pub fn label(self) -> &'static str {
        match self {
            TerminalApp::Terminal => "Terminal",
            TerminalApp::Ghostty => "Ghostty",
            TerminalApp::ITerm2 => "iTerm2",
            TerminalApp::Alacritty => "Alacritty",
            TerminalApp::Kitty => "Kitty",
            TerminalApp::WezTerm => "WezTerm",
            TerminalApp::Warp => "Warp",
        }
    }

    /// Detect which terminals are available on the current system.
    /// Result is cached for the process lifetime — the set of installed
    /// terminals does not change during a TUI session.
    pub fn detect() -> Vec<TerminalApp> {
        static CACHE: std::sync::OnceLock<Vec<TerminalApp>> = std::sync::OnceLock::new();
        CACHE
            .get_or_init(|| {
                Self::all()
                    .iter()
                    .filter(|t| t.is_available())
                    .copied()
                    .collect()
            })
            .clone()
    }

    /// Check whether this terminal is installed / accessible.
    fn is_available(&self) -> bool {
        match self {
            TerminalApp::Terminal => cfg!(target_os = "macos"),
            TerminalApp::Ghostty => which_exists("ghostty"),
            TerminalApp::ITerm2 => {
                Path::new("/Applications/iTerm.app").exists()
                    || home_applications().join("iTerm.app").exists()
            }
            TerminalApp::Alacritty => which_exists("alacritty"),
            TerminalApp::Kitty => which_exists("kitty"),
            TerminalApp::WezTerm => which_exists("wezterm"),
            TerminalApp::Warp => {
                Path::new("/Applications/Warp.app").exists()
                    || home_applications().join("Warp.app").exists()
            }
        }
    }

    /// Cycle forward through detected terminals only.
    pub fn cycle(self) -> Self {
        let available = Self::detect();
        if available.len() <= 1 {
            return self;
        }
        let idx = available.iter().position(|&t| t == self).unwrap_or(0);
        available[(idx + 1) % available.len()]
    }

    /// Cycle backward through detected terminals only.
    pub fn cycle_back(self) -> Self {
        let available = Self::detect();
        if available.len() <= 1 {
            return self;
        }
        let idx = available.iter().position(|&t| t == self).unwrap_or(0);
        available[(idx + available.len() - 1) % available.len()]
    }
}

/// Resolve `~/Applications/` for macOS .app detection.
fn home_applications() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string())).join("Applications")
}

/// Check whether an executable exists on `$PATH` via in-process lookup.
/// Avoids spawning a subprocess and works on systems without the `which`
/// binary.
pub(crate) fn which_exists(name: &str) -> bool {
    std::env::var("PATH")
        .map(|paths| std::env::split_paths(&paths).any(|dir| dir.join(name).exists()))
        .unwrap_or(false)
}

/// Star status tracked locally so the Settings tab can show whether the user
/// has starred the project. We do not query GitHub on startup; the status is
/// updated when the user presses `*` and `gh star` succeeds (or reports already
/// starred).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StarStatus {
    #[default]
    Unknown,
    Starred,
    NotStarred,
}

impl StarStatus {
    pub fn label(self) -> &'static str {
        match self {
            StarStatus::Unknown => "unknown",
            StarStatus::Starred => "starred",
            StarStatus::NotStarred => "not starred",
        }
    }
}

/// Discrete choices for the process-sampler cadence. Offered as a fixed menu
/// rather than a free-form integer so Settings tab navigation stays keyboard-
/// driven — there's no numeric input widget.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SampleIntervalSecs(pub u64);

impl Default for SampleIntervalSecs {
    fn default() -> Self {
        SampleIntervalSecs(2)
    }
}

impl SampleIntervalSecs {
    pub fn all() -> &'static [SampleIntervalSecs] {
        &[
            SampleIntervalSecs(1),
            SampleIntervalSecs(2),
            SampleIntervalSecs(5),
            SampleIntervalSecs(10),
        ]
    }
    pub fn label(self) -> String {
        format!("{}s", self.0)
    }
    pub fn cycle(self) -> Self {
        let all = Self::all();
        let idx = all.iter().position(|c| c.0 == self.0).unwrap_or(1);
        all[(idx + 1) % all.len()]
    }
    pub fn cycle_back(self) -> Self {
        let all = Self::all();
        let idx = all.iter().position(|c| c.0 == self.0).unwrap_or(1);
        all[(idx + all.len() - 1) % all.len()]
    }
}

/// Aggregate of all user preferences. Any field added here must:
/// 1. implement `Default` so old config files stay loadable,
/// 2. be wired into `tui::settings::render` so users can change it,
/// 3. be honored by the subsystem that consumes it (theme, i18n, renderers).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub language: Language,
    pub theme: ThemeColor,
    pub time_format: TimeFormat,
    pub token_unit: TokenUnit,
    pub sample_interval: SampleIntervalSecs,
    /// When true, the Dashboard's Σ tokens adds cache_read + cache_creation.
    /// When false, cache buckets are excluded so the headline matches what
    /// users intuitively think of as "real" token spend.
    pub include_cache_in_total: bool,
    /// Terminal emulator used to resume sessions. Only terminals detected on
    /// the system are offered in the Settings UI.
    pub terminal: TerminalApp,
    /// Local cache of whether the user has starred the project on GitHub.
    /// Updated automatically when `*` is pressed and `gh star` runs.
    pub star_status: StarStatus,
    /// How many times the app has been launched. Used to decide when to show
    /// the gentle star prompt toast.
    pub launch_count: u32,
    /// How many times the star prompt toast has already been shown.
    /// Capped at 3 so the prompt never becomes spam.
    pub star_prompt_count: u32,
    /// User-editable keyboard shortcuts, grouped by context.
    pub keybindings: KeyBindings,
    /// Paths the user has bookmarked. Persisted across sessions; the Sessions
    /// list floats these to the top by default and renders a star icon next
    /// to each. Filter token `starred:true` / `starred:false` matches against
    /// this set. We store paths as strings (rather than `PathBuf`) so JSON
    /// (de)serialization stays trivial and platform-portable.
    #[serde(default)]
    pub starred_paths: Vec<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            language: Language::default(),
            theme: ThemeColor::default(),
            time_format: TimeFormat::default(),
            token_unit: TokenUnit::default(),
            sample_interval: SampleIntervalSecs::default(),
            include_cache_in_total: true,
            terminal: TerminalApp::default(),
            star_status: StarStatus::default(),
            launch_count: 0,
            star_prompt_count: 0,
            keybindings: KeyBindings::default(),
            starred_paths: Vec::new(),
        }
    }
}

impl Settings {
    /// Resolves to `$XDG_CONFIG_HOME/agent-monitor/settings.json` or the macOS
    /// equivalent. `None` on platforms where `ProjectDirs` cannot determine a
    /// config root — in that case we quietly run with defaults and skip saves.
    pub fn config_path() -> Option<PathBuf> {
        let dirs = directories::ProjectDirs::from("dev", "agentmonitor", "agent-monitor")?;
        std::fs::create_dir_all(dirs.config_dir()).ok();
        Some(dirs.config_dir().join("settings.json"))
    }

    /// Best-effort load. A missing or malformed file returns defaults — the
    /// TUI must never block on disk I/O or surface JSON errors to the user.
    pub fn load_or_default() -> Self {
        let Some(path) = Self::config_path() else {
            return Self::default();
        };
        match std::fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str(&text).unwrap_or_else(|err| {
                tracing::warn!(?err, path = %path.display(), "settings parse failed; using defaults");
                Self::default()
            }),
            Err(_) => Self::default(),
        }
    }

    pub fn save(&self) -> anyhow::Result<()> {
        let Some(path) = Self::config_path() else {
            return Ok(());
        };
        self.save_to_path(&path)
    }

    fn save_to_path(&self, path: &Path) -> anyhow::Result<()> {
        let json = serde_json::to_string_pretty(self)?;
        let tmp_path = path.with_extension("json.tmp");
        std::fs::write(&tmp_path, json)?;
        std::fs::rename(&tmp_path, path)?;
        Ok(())
    }
}

static SETTINGS: OnceLock<RwLock<Settings>> = OnceLock::new();

/// Shared access handle. First caller initializes from disk; later callers see
/// the in-memory copy. Writers go through [`update`] so saves stay central.
pub fn settings() -> &'static RwLock<Settings> {
    SETTINGS.get_or_init(|| RwLock::new(initial_settings()))
}

#[cfg(test)]
fn initial_settings() -> Settings {
    Settings::default()
}

#[cfg(not(test))]
fn initial_settings() -> Settings {
    Settings::load_or_default()
}

/// Snapshot the current settings. Cheap — a `Settings` is seven small fields.
pub fn get() -> Settings {
    settings().read().clone()
}

/// Mutate settings and persist atomically. Save failures are logged but not
/// propagated: a read-only config dir shouldn't break live editing.
pub fn update<F: FnOnce(&mut Settings)>(f: F) {
    let mut guard = settings().write();
    f(&mut guard);
    save_after_update(&guard);
}

/// Returns whether `path` is in the starred set. Cheap (linear over a small
/// list — most users star fewer than a dozen sessions). String comparison;
/// callers should pass the canonical path form `SessionMeta::path` carries.
pub fn is_starred(path: &Path) -> bool {
    let s = settings().read();
    let key = path.to_string_lossy();
    s.starred_paths.iter().any(|p| p == key.as_ref())
}

/// Toggle whether `path` is starred. Persists to disk via `update()`.
pub fn toggle_starred(path: &Path) {
    let key = path.to_string_lossy().to_string();
    update(|s| {
        if let Some(idx) = s.starred_paths.iter().position(|p| p == &key) {
            s.starred_paths.swap_remove(idx);
        } else {
            s.starred_paths.push(key);
        }
    });
}

#[cfg(test)]
fn save_after_update(_: &Settings) {}

#[cfg(not(test))]
fn save_after_update(settings: &Settings) {
    if let Err(err) = settings.save() {
        tracing::warn!(?err, "settings save failed");
    }
}

/// Shared mutex used by tests that touch the global settings RwLock. Cargo
/// runs library tests in parallel by default, and several suites mutate
/// settings — acquiring this lock serializes them so no test observes a
/// half-mutated state from a sibling test.
///
/// Exposed at module-scope (not inside `mod tests`) so sibling test modules
/// in other files can reach it via `crate::settings::test_lock()`.
#[cfg(test)]
pub fn test_lock() -> std::sync::MutexGuard<'static, ()> {
    use std::sync::{Mutex, OnceLock};
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|poisoned| {
            // Clear poison so one panicking test doesn't cascade into every
            // other test failing with PoisonError.
            poisoned.into_inner()
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_round_trip_through_json() {
        let s = Settings::default();
        let text = serde_json::to_string(&s).unwrap();
        let back: Settings = serde_json::from_str(&text).unwrap();
        assert_eq!(back.language, s.language);
        assert_eq!(back.theme, s.theme);
        assert_eq!(back.sample_interval.0, s.sample_interval.0);
    }

    #[test]
    fn cycle_covers_every_variant_exactly_once() {
        let mut seen = vec![Language::En];
        let mut cur = Language::En;
        for _ in 0..Language::all().len() - 1 {
            cur = cur.cycle();
            assert!(!seen.contains(&cur), "duplicate variant: {cur:?}");
            seen.push(cur);
        }
        assert_eq!(cur.cycle(), Language::En, "cycle must wrap");
    }

    #[test]
    fn missing_fields_fall_back_to_default() {
        // Older settings files won't have every field. `#[serde(default)]`
        // should paper over that instead of throwing.
        let partial = r#"{"language":"zh"}"#;
        let s: Settings = serde_json::from_str(partial).unwrap();
        assert_eq!(s.language, Language::Zh);
        assert_eq!(s.theme, ThemeColor::default());
        assert_eq!(s.sample_interval, SampleIntervalSecs::default());
    }

    #[test]
    fn keybindings_default_round_trip_and_old_json_backfills() {
        let s = Settings::default();
        assert!(s
            .keybindings
            .bindings_for(KeyAction::Quit)
            .iter()
            .any(|binding| binding.display() == "q"));

        let text = serde_json::to_string(&s).unwrap();
        let back: Settings = serde_json::from_str(&text).unwrap();
        assert_eq!(
            back.keybindings.bindings_for(KeyAction::Quit),
            s.keybindings.bindings_for(KeyAction::Quit)
        );

        let partial = r#"{"language":"zh"}"#;
        let backfilled: Settings = serde_json::from_str(partial).unwrap();
        assert!(backfilled
            .keybindings
            .bindings_for(KeyAction::SettingsActivate)
            .iter()
            .any(|binding| binding.display() == "Enter"));
    }

    #[test]
    fn save_to_path_writes_readable_json() {
        let unique = format!(
            "agentmonitor-settings-save-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("time went backwards")
                .as_nanos()
        );
        let dir = std::env::temp_dir().join(unique);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings.json");
        let mut s = Settings::default();
        s.keybindings.set_binding(
            KeyAction::Quit,
            KeyBinding::plain(KeyCodeSpec::Char('x')),
        );

        s.save_to_path(&path).unwrap();
        let back: Settings = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();

        assert!(back.keybindings.matches_action(
            KeyAction::Quit,
            crossterm::event::KeyCode::Char('x'),
            crossterm::event::KeyModifiers::empty()
        ));
        assert!(!path.with_extension("json.tmp").exists());

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn custom_keybinding_round_trips_through_json() {
        let mut s = Settings::default();
        let custom = KeyBinding::plain(KeyCodeSpec::Char('x'));
        s.keybindings.set_binding(KeyAction::Quit, custom);

        let text = serde_json::to_string(&s).unwrap();
        let back: Settings = serde_json::from_str(&text).unwrap();

        assert!(back
            .keybindings
            .matches_action(KeyAction::Quit, crossterm::event::KeyCode::Char('x'), crossterm::event::KeyModifiers::empty()));
        assert!(!back
            .keybindings
            .matches_action(KeyAction::Quit, crossterm::event::KeyCode::Char('q'), crossterm::event::KeyModifiers::empty()));
    }

    #[test]
    fn keybinding_display_and_conflict_resolution_are_context_aware() {
        let ctrl_d = KeyBinding::ctrl(KeyCodeSpec::Char('d'));
        assert_eq!(ctrl_d.display(), "Ctrl+D");
        assert_eq!(KeyBinding::plain(KeyCodeSpec::Delete).display(), "Delete");
        assert_eq!(KeyBinding::plain(KeyCodeSpec::Left).display(), "←");

        let mut keybindings = KeyBindings::default();
        let resume = KeyBinding::plain(KeyCodeSpec::Char('r'));
        let replaced = keybindings.set_binding(KeyAction::SessionsOpenViewer, resume);
        assert_eq!(replaced, Some(KeyAction::SessionsResume));
        assert!(keybindings
            .bindings_for(KeyAction::SessionsResume)
            .is_empty());
        assert_eq!(
            keybindings.bindings_for(KeyAction::SessionsOpenViewer),
            &[resume]
        );

        let viewer_q = KeyBinding::plain(KeyCodeSpec::Char('q'));
        let replaced = keybindings.set_binding(KeyAction::ViewerBack, viewer_q);
        assert_eq!(replaced, None);
        assert_eq!(keybindings.bindings_for(KeyAction::ViewerBack), &[viewer_q]);
    }

    #[test]
    fn reset_binding_removes_default_conflicts_in_same_context() {
        let mut keybindings = KeyBindings::default();
        let default_open = KeyBinding::plain(KeyCodeSpec::Enter);
        keybindings.set_binding(KeyAction::SessionsResume, default_open);
        assert!(keybindings
            .bindings_for(KeyAction::SessionsOpenViewer)
            .is_empty());

        keybindings.reset_binding(KeyAction::SessionsOpenViewer);

        assert_eq!(
            keybindings.bindings_for(KeyAction::SessionsOpenViewer),
            &[default_open]
        );
        assert!(!keybindings
            .bindings_for(KeyAction::SessionsResume)
            .contains(&default_open));
    }

    #[test]
    fn missing_default_action_backfill_removes_conflicts() {
        let text = r#"{
            "entries": [
                {
                    "action": "sessions_resume",
                    "bindings": [{ "code": { "type": "enter" } }]
                }
            ]
        }"#;

        let keybindings: KeyBindings = serde_json::from_str(text).unwrap();
        let default_open = KeyBinding::plain(KeyCodeSpec::Enter);

        assert_eq!(
            keybindings.bindings_for(KeyAction::SessionsOpenViewer),
            &[default_open]
        );
        assert!(!keybindings
            .bindings_for(KeyAction::SessionsResume)
            .contains(&default_open));
    }

    #[test]
    fn deserialization_removes_existing_same_context_conflicts() {
        let text = r#"{
            "entries": [
                {
                    "action": "sessions_open_viewer",
                    "bindings": [{ "code": { "type": "enter" } }]
                },
                {
                    "action": "sessions_resume",
                    "bindings": [{ "code": { "type": "enter" } }]
                }
            ]
        }"#;

        let keybindings: KeyBindings = serde_json::from_str(text).unwrap();
        let enter = KeyBinding::plain(KeyCodeSpec::Enter);
        let owners = [KeyAction::SessionsOpenViewer, KeyAction::SessionsResume]
            .into_iter()
            .filter(|action| keybindings.bindings_for(*action).contains(&enter))
            .count();

        assert_eq!(owners, 1);
    }

    #[test]
    fn reset_restores_default_keybindings() {
        let mut keybindings = KeyBindings::default();
        keybindings.clear_binding(KeyAction::Quit);
        assert!(keybindings.bindings_for(KeyAction::Quit).is_empty());
        keybindings.reset_binding(KeyAction::Quit);
        assert!(keybindings
            .bindings_for(KeyAction::Quit)
            .iter()
            .any(|binding| binding.display() == "q"));
    }

    #[test]
    fn sample_interval_cycle_preserves_membership() {
        let seen: Vec<u64> = (0..SampleIntervalSecs::all().len())
            .scan(SampleIntervalSecs::default(), |cur, _| {
                let out = cur.0;
                *cur = cur.cycle();
                Some(out)
            })
            .collect();
        for s in SampleIntervalSecs::all() {
            assert!(seen.contains(&s.0), "missing {}", s.0);
        }
    }
}
