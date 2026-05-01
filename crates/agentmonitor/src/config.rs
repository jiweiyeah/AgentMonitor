use std::path::PathBuf;
use std::time::Duration;

/// Runtime configuration derived from CLI flags and platform defaults.
#[derive(Debug, Clone)]
pub struct Config {
    pub sample_interval: Duration,
    pub metrics_capacity: usize,
    pub claude_root: Option<PathBuf>,
    pub claude_desktop_root: Option<PathBuf>,
    pub codex_root: Option<PathBuf>,
    pub gemini_root: Option<PathBuf>,
    pub hermes_root: Option<PathBuf>,
    pub opencode_root: Option<PathBuf>,
}

impl Default for Config {
    fn default() -> Self {
        let home = directories::BaseDirs::new().map(|b| b.home_dir().to_path_buf());
        Self {
            sample_interval: Duration::from_secs(2),
            metrics_capacity: 300,
            claude_root: home.as_ref().map(|h| h.join(".claude").join("projects")),
            claude_desktop_root: home.as_ref().map(|h| {
                h.join("Library")
                    .join("Application Support")
                    .join("Claude-3p")
                    .join("local-agent-mode-sessions")
            }),
            codex_root: home.as_ref().map(|h| h.join(".codex").join("sessions")),
            gemini_root: home.as_ref().map(|h| h.join(".gemini").join("tmp")),
            hermes_root: home.as_ref().map(|h| h.join(".hermes")),
            opencode_root: home.as_ref().map(|h| {
                h.join(".local")
                    .join("share")
                    .join("opencode")
            }),
        }
    }
}
