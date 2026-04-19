use std::path::PathBuf;
use std::time::Duration;

/// Runtime configuration derived from CLI flags and platform defaults.
#[derive(Debug, Clone)]
pub struct Config {
    pub sample_interval: Duration,
    pub metrics_capacity: usize,
    pub claude_root: Option<PathBuf>,
    pub codex_root: Option<PathBuf>,
}

impl Default for Config {
    fn default() -> Self {
        let home = directories::BaseDirs::new().map(|b| b.home_dir().to_path_buf());
        Self {
            sample_interval: Duration::from_secs(2),
            metrics_capacity: 300,
            claude_root: home.as_ref().map(|h| h.join(".claude").join("projects")),
            codex_root: home.as_ref().map(|h| h.join(".codex").join("sessions")),
        }
    }
}
