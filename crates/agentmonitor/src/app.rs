use std::path::PathBuf;
use std::sync::Arc;
use std::time::SystemTime;

use anyhow::Result;
use parking_lot::RwLock;

use crate::adapter::conversation::ConversationEvent;
use crate::adapter::types::{MessagePreview, SessionMeta};
use crate::adapter::{ClaudeAdapter, CodexAdapter, DynAdapter};
use crate::collector::metrics::MetricsStore;
use crate::collector::token_refresh::TokenCache;
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

/// Sort order applied to the Sessions list. The default (`UpdatedDesc`) is
/// what the raw storage already holds, so it's a no-op in that case.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SessionSort {
    #[default]
    UpdatedDesc,
    SizeDesc,
    MessagesDesc,
}

impl SessionSort {
    pub fn label(self) -> &'static str {
        match self {
            Self::UpdatedDesc => "updated↓",
            Self::SizeDesc => "size↓",
            Self::MessagesDesc => "msgs↓",
        }
    }
    pub fn cycle(self) -> Self {
        match self {
            Self::UpdatedDesc => Self::SizeDesc,
            Self::SizeDesc => Self::MessagesDesc,
            Self::MessagesDesc => Self::UpdatedDesc,
        }
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

impl AppState {
    /// Compute the indices of `sessions` that pass `filter` and are sorted
    /// according to `sort`. Returned indices refer to the raw `self.sessions`
    /// vector, so callers can use them to index back into it without cloning.
    pub fn visible_session_indices(&self, filter: &str, sort: SessionSort) -> Vec<usize> {
        let needle = filter.trim().to_lowercase();
        let mut idxs: Vec<usize> = (0..self.sessions.len())
            .filter(|&i| needle.is_empty() || session_matches_filter(&self.sessions[i], &needle))
            .collect();
        match sort {
            SessionSort::UpdatedDesc => {
                // Storage is already sorted by updated_at desc, but a filter
                // may have skipped rows — keep the relative order stable.
            }
            SessionSort::SizeDesc => {
                idxs.sort_by(|&a, &b| {
                    self.sessions[b].size_bytes.cmp(&self.sessions[a].size_bytes)
                });
            }
            SessionSort::MessagesDesc => {
                idxs.sort_by(|&a, &b| {
                    self.sessions[b]
                        .message_count
                        .cmp(&self.sessions[a].message_count)
                });
            }
        }
        idxs
    }
}

fn session_matches_filter(s: &SessionMeta, needle: &str) -> bool {
    // Case-insensitive substring match over the high-signal fields a user
    // would narrow on: project path, id, branch, agent label.
    if s.id.to_lowercase().contains(needle) {
        return true;
    }
    if s.agent_label().to_lowercase().contains(needle) {
        return true;
    }
    if let Some(cwd) = &s.cwd {
        if cwd
            .to_string_lossy()
            .to_lowercase()
            .contains(needle)
        {
            return true;
        }
    }
    if let Some(branch) = &s.git_branch {
        if branch.to_lowercase().contains(needle) {
            return true;
        }
    }
    false
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
    /// Sessions-tab filter text. Applied case-insensitively over cwd/id/branch.
    pub session_filter: String,
    /// True while the user is typing in the filter input — swallows keys like
    /// `q` and `Esc` so they edit the filter instead of quitting.
    pub session_filter_input: bool,
    pub session_sort: SessionSort,
    /// Process-tab row selection. Indexes into `metrics.snapshot()`.
    pub selected_process: usize,
    /// Per-session token cache (`(path, mtime) → tokens + message_count`).
    /// Fed by `collector::token_refresh` in the background so the Dashboard
    /// aggregates and the Sessions list see accurate numbers without waiting
    /// on a full-parse per frame.
    pub token_cache: Arc<TokenCache>,
}

impl App {
    pub async fn new(config: Config) -> Result<Self> {
        let adapters: Vec<DynAdapter> = vec![
            Arc::new(ClaudeAdapter::new(config.claude_root.clone())),
            Arc::new(CodexAdapter::new(config.codex_root.clone())),
        ];
        let state = Arc::new(RwLock::new(AppState::default()));
        let metrics = Arc::new(MetricsStore::new(config.metrics_capacity));
        let token_cache = Arc::new(TokenCache::new());
        let app = Self {
            config,
            state,
            metrics,
            adapters,
            tab: Tab::Dashboard,
            should_quit: false,
            session_filter: String::new(),
            session_filter_input: false,
            session_sort: SessionSort::default(),
            selected_process: 0,
            token_cache,
        };
        app.initial_scan().await?;
        Ok(app)
    }

    /// Blocking full scan across all adapters. Fast-parse tokens are zeroed
    /// out because they only reflect the JSONL header and would otherwise
    /// ship to the Dashboard as-is; `collector::token_refresh` is the sole
    /// writer for `tokens` and `message_count`, and it fills in real totals
    /// on its first pass.
    pub async fn initial_scan(&self) -> Result<()> {
        let mut all = Vec::new();
        for adapter in &self.adapters {
            match adapter.scan_all().await {
                Ok(mut metas) => all.append(&mut metas),
                Err(err) => tracing::warn!(agent = adapter.id(), ?err, "scan failed"),
            }
        }
        for m in &mut all {
            m.tokens = crate::adapter::types::TokenStats::default();
            m.message_count = 0;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::types::TokenStats;
    use chrono::{TimeZone, Utc};
    use std::path::PathBuf;

    fn mk(
        agent: &'static str,
        id: &str,
        cwd: Option<&str>,
        size: u64,
        msgs: usize,
        updated_minutes_ago: i64,
    ) -> SessionMeta {
        let now = Utc.with_ymd_and_hms(2026, 4, 20, 12, 0, 0).unwrap();
        SessionMeta {
            agent,
            id: id.into(),
            path: PathBuf::from(format!("/tmp/{id}.jsonl")),
            cwd: cwd.map(PathBuf::from),
            model: None,
            version: None,
            git_branch: None,
            started_at: None,
            updated_at: Some(now - chrono::Duration::minutes(updated_minutes_ago)),
            message_count: msgs,
            tokens: TokenStats::default(),
            status: Default::default(),
            byte_offset: 0,
            size_bytes: size,
        }
    }

    fn state_with(sessions: Vec<SessionMeta>) -> AppState {
        AppState {
            sessions,
            ..Default::default()
        }
    }

    #[test]
    fn visible_indices_filter_matches_cwd_id_and_is_case_insensitive() {
        let state = state_with(vec![
            mk("claude", "abc123", Some("/repos/AgentMonitor"), 10, 1, 0),
            mk("codex", "xyz789", Some("/repos/other"), 20, 5, 10),
            mk("claude", "def456", None, 5, 2, 30),
        ]);
        let idxs = state.visible_session_indices("agent", SessionSort::UpdatedDesc);
        assert_eq!(idxs, vec![0], "filter matches cwd substring case-insensitive");

        let idxs = state.visible_session_indices("XYZ", SessionSort::UpdatedDesc);
        assert_eq!(idxs, vec![1], "filter matches id case-insensitive");

        let idxs = state.visible_session_indices("", SessionSort::UpdatedDesc);
        assert_eq!(idxs, vec![0, 1, 2], "empty filter keeps all, preserves order");
    }

    #[test]
    fn visible_indices_sort_by_size_and_messages() {
        let state = state_with(vec![
            mk("claude", "a", None, 10, 1, 0),
            mk("claude", "b", None, 30, 5, 10),
            mk("claude", "c", None, 20, 3, 20),
        ]);
        let by_size = state.visible_session_indices("", SessionSort::SizeDesc);
        assert_eq!(by_size, vec![1, 2, 0]);
        let by_msgs = state.visible_session_indices("", SessionSort::MessagesDesc);
        assert_eq!(by_msgs, vec![1, 2, 0]);
    }

    #[test]
    fn session_sort_cycle_round_trips() {
        let s = SessionSort::default();
        assert_eq!(s, SessionSort::UpdatedDesc);
        assert_eq!(s.cycle(), SessionSort::SizeDesc);
        assert_eq!(s.cycle().cycle(), SessionSort::MessagesDesc);
        assert_eq!(s.cycle().cycle().cycle(), SessionSort::UpdatedDesc);
    }
}
