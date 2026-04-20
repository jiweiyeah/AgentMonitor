use std::path::PathBuf;
use std::sync::Arc;
use std::time::SystemTime;

use anyhow::Result;
use parking_lot::RwLock;

use crate::adapter::conversation::ConversationEvent;
use crate::adapter::types::{MessagePreview, SessionMeta};
use crate::adapter::{ClaudeAdapter, CodexAdapter, DynAdapter};
use crate::collector::metrics::MetricsStore;
use crate::config::Config;

/// Active tab in the TUI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Dashboard,
    Sessions,
    Process,
}

impl Tab {
    pub fn all() -> [Tab; 3] {
        [Tab::Dashboard, Tab::Sessions, Tab::Process]
    }
    pub fn title(self) -> &'static str {
        match self {
            Tab::Dashboard => "Dashboard",
            Tab::Sessions => "Sessions",
            Tab::Process => "Process",
        }
    }
    pub fn next(self) -> Tab {
        match self {
            Tab::Dashboard => Tab::Sessions,
            Tab::Sessions => Tab::Process,
            Tab::Process => Tab::Dashboard,
        }
    }
    pub fn prev(self) -> Tab {
        match self {
            Tab::Dashboard => Tab::Process,
            Tab::Sessions => Tab::Dashboard,
            Tab::Process => Tab::Sessions,
        }
    }
    pub fn index(self) -> usize {
        match self {
            Tab::Dashboard => 0,
            Tab::Sessions => 1,
            Tab::Process => 2,
        }
    }
}

/// UI mode. The default `Normal` mode renders the tab bar + body. `Viewer`
/// takes over the whole screen with a full conversation transcript.
#[derive(Debug, Clone, Default)]
pub enum Mode {
    #[default]
    Normal,
    Viewer {
        path: PathBuf,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ExpandMode {
    #[default]
    Collapsed,
    Expanded,
}

/// Fully-parsed transcript kept alive while the viewer is (or was last) open.
/// Lives on `AppState` so background loaders and the renderer share one source
/// of truth without extra plumbing.
#[derive(Debug)]
pub struct ConversationCache {
    pub path: PathBuf,
    pub mtime: Option<SystemTime>,
    pub events: Vec<ConversationEvent>,
    pub scroll: u16,
    pub expand: ExpandMode,
    pub loading: bool,
    pub error: Option<String>,
    /// Viewport height of the body area, written by the renderer, read by the
    /// event handler so Ctrl+D / PgDn can scroll by a real half-/full-page.
    pub viewport_height: u16,
    /// Total flattened line count from the last render. Used by the event
    /// handler to clamp `scroll` so `G` and oversized deltas land on a real
    /// row instead of leaving `scroll` out-of-bounds forever.
    pub last_rendered_total: u32,
}

impl ConversationCache {
    pub fn loading(path: PathBuf) -> Self {
        Self {
            path,
            mtime: None,
            events: Vec::new(),
            scroll: 0,
            expand: ExpandMode::Collapsed,
            loading: true,
            error: None,
            viewport_height: 0,
            last_rendered_total: 0,
        }
    }

    /// Maximum valid scroll given the last known total and viewport height.
    pub fn max_scroll(&self) -> u16 {
        let total = self.last_rendered_total;
        let h = self.viewport_height.max(1) as u32;
        total.saturating_sub(h).min(u16::MAX as u32) as u16
    }
}

/// Shared state snapshot rendered every frame.
#[derive(Debug, Default)]
pub struct AppState {
    pub sessions: Vec<SessionMeta>,
    pub selected_session: usize,
    pub dirty: bool,
    /// Cached preview + full stats for the currently selected session.
    /// Keyed by path so we don't re-render stale data after the list shifts.
    pub preview: Option<PreviewCache>,
    pub mode: Mode,
    /// Single-slot cache for the full-screen viewer. We don't LRU: the viewer
    /// is a modal, only one is visible at a time, and re-opening the same
    /// session hits this slot directly.
    pub conversation: Option<ConversationCache>,
}

#[derive(Debug, Clone)]
pub struct PreviewCache {
    pub path: PathBuf,
    pub messages: Vec<MessagePreview>,
    pub message_count: usize,
    pub loading: bool,
}

/// App bundles shared state, config, adapters, and metric store.
pub struct App {
    pub config: Config,
    pub state: Arc<RwLock<AppState>>,
    pub metrics: Arc<MetricsStore>,
    pub adapters: Vec<DynAdapter>,
    pub tab: Tab,
    pub should_quit: bool,
}

impl App {
    pub async fn new(config: Config) -> Result<Self> {
        let adapters: Vec<DynAdapter> = vec![
            Arc::new(ClaudeAdapter::new(config.claude_root.clone())),
            Arc::new(CodexAdapter::new(config.codex_root.clone())),
        ];
        let state = Arc::new(RwLock::new(AppState::default()));
        let metrics = Arc::new(MetricsStore::new(config.metrics_capacity));
        let app = Self {
            config,
            state,
            metrics,
            adapters,
            tab: Tab::Dashboard,
            should_quit: false,
        };
        app.initial_scan().await?;
        Ok(app)
    }

    /// Blocking full scan across all adapters.
    pub async fn initial_scan(&self) -> Result<()> {
        let mut all = Vec::new();
        for adapter in &self.adapters {
            match adapter.scan_all().await {
                Ok(mut metas) => all.append(&mut metas),
                Err(err) => tracing::warn!(agent = adapter.id(), ?err, "scan failed"),
            }
        }
        all.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        let mut s = self.state.write();
        s.sessions = all;
        s.selected_session = 0;
        s.dirty = true;
        Ok(())
    }

    /// `--once-and-exit` path. Prints compact session list for benchmarking.
    pub fn print_snapshot(&self) {
        let state = self.state.read();
        println!(
            "agent-monitor snapshot — {} session(s)",
            state.sessions.len()
        );
        for s in state.sessions.iter().take(20) {
            println!(
                "  [{:<10}] {} · {} · {}",
                s.agent_label(),
                shorten(&s.id, 12),
                s.cwd_display(),
                s.model.clone().unwrap_or_else(|| "-".into()),
            );
        }
    }

    pub fn cycle_tab_next(&mut self) {
        self.tab = self.tab.next();
    }
    pub fn cycle_tab_prev(&mut self) {
        self.tab = self.tab.prev();
    }
    pub fn set_tab(&mut self, tab: Tab) {
        self.tab = tab;
    }
}

fn shorten(s: &str, n: usize) -> String {
    if s.len() <= n {
        s.to_string()
    } else {
        format!("{}…", &s[..n])
    }
}
