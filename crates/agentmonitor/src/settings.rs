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
        let json = serde_json::to_string_pretty(self)?;
        std::fs::write(path, json)?;
        Ok(())
    }
}

static SETTINGS: OnceLock<RwLock<Settings>> = OnceLock::new();

/// Shared access handle. First caller initializes from disk; later callers see
/// the in-memory copy. Writers go through [`update`] so saves stay central.
pub fn settings() -> &'static RwLock<Settings> {
    SETTINGS.get_or_init(|| RwLock::new(Settings::load_or_default()))
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
    if let Err(err) = guard.save() {
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
    fn theme_color_maps_to_ratatui_color() {
        assert_eq!(ThemeColor::Cyan.to_color(), Color::Cyan);
        assert_eq!(ThemeColor::Red.to_color(), Color::Red);
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
