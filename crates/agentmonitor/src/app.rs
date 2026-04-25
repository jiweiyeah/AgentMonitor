use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

use anyhow::Result;
use parking_lot::RwLock;

use crate::adapter::conversation::ConversationEvent;
use crate::adapter::types::{MessagePreview, SessionMeta, SessionStatus};
use crate::adapter::{ClaudeAdapter, CodexAdapter, DynAdapter};
use crate::collector::metrics::{MetricsStore, ProcessEntry};
use crate::collector::token_refresh::TokenCache;
use crate::config::Config;

/// Active tab in the TUI. Process details used to live in their own tab but
/// are now embedded in the Dashboard — the old `Tab::Process` was redundant
/// with the Overview summary, which already carried the same live-count/RSS
/// snapshot the Process tab was showing in its header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Dashboard,
    Sessions,
    Settings,
}

impl Tab {
    pub fn all() -> [Tab; 3] {
        [Tab::Dashboard, Tab::Sessions, Tab::Settings]
    }
    pub fn title(self) -> &'static str {
        match self {
            Tab::Dashboard => crate::i18n::t("tab.dashboard"),
            Tab::Sessions => crate::i18n::t("tab.sessions"),
            Tab::Settings => crate::i18n::t("tab.settings"),
        }
    }
    pub fn next(self) -> Tab {
        match self {
            Tab::Dashboard => Tab::Sessions,
            Tab::Sessions => Tab::Settings,
            Tab::Settings => Tab::Dashboard,
        }
    }
    pub fn prev(self) -> Tab {
        match self {
            Tab::Dashboard => Tab::Settings,
            Tab::Sessions => Tab::Dashboard,
            Tab::Settings => Tab::Sessions,
        }
    }
    pub fn index(self) -> usize {
        match self {
            Tab::Dashboard => 0,
            Tab::Sessions => 1,
            Tab::Settings => 2,
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
    TokensDesc,
    SizeDesc,
    MessagesDesc,
    StatusDesc,
}

impl SessionSort {
    pub fn label(self) -> &'static str {
        match self {
            Self::UpdatedDesc => "updated↓",
            Self::TokensDesc => "tokens↓",
            Self::SizeDesc => "size↓",
            Self::MessagesDesc => "msgs↓",
            Self::StatusDesc => "status↓",
        }
    }
    pub fn cycle(self) -> Self {
        match self {
            Self::UpdatedDesc => Self::TokensDesc,
            Self::TokensDesc => Self::SizeDesc,
            Self::SizeDesc => Self::MessagesDesc,
            Self::MessagesDesc => Self::StatusDesc,
            Self::StatusDesc => Self::UpdatedDesc,
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
        let tokens = filter
            .split_whitespace()
            .map(|token| token.to_lowercase())
            .collect::<Vec<_>>();
        let mut idxs: Vec<usize> = (0..self.sessions.len())
            .filter(|&i| tokens.is_empty() || session_matches_filter(&self.sessions[i], &tokens))
            .collect();
        match sort {
            SessionSort::UpdatedDesc => {
                // Storage is already sorted by updated_at desc, but a filter
                // may have skipped rows — keep the relative order stable.
            }
            SessionSort::TokensDesc => {
                idxs.sort_by(|&a, &b| {
                    self.sessions[b]
                        .tokens
                        .total()
                        .cmp(&self.sessions[a].tokens.total())
                });
            }
            SessionSort::SizeDesc => {
                idxs.sort_by(|&a, &b| {
                    self.sessions[b]
                        .size_bytes
                        .cmp(&self.sessions[a].size_bytes)
                });
            }
            SessionSort::MessagesDesc => {
                idxs.sort_by(|&a, &b| {
                    self.sessions[b]
                        .message_count
                        .cmp(&self.sessions[a].message_count)
                });
            }
            SessionSort::StatusDesc => {
                idxs.sort_by(|&a, &b| {
                    status_rank(self.sessions[b].status).cmp(&status_rank(self.sessions[a].status))
                });
            }
        }
        idxs
    }

    /// Pick the best candidate session for a live process. We only match
    /// within the same agent, then prefer exact CWD matches, then fresher /
    /// more obviously live sessions.
    pub fn best_session_index_for_process(&self, process: &ProcessEntry) -> Option<usize> {
        let process_cwd = process.cwd.as_deref();
        self.sessions
            .iter()
            .enumerate()
            .filter(|(_, session)| session.agent == process.agent)
            .max_by_key(|(_, session)| {
                (
                    same_cwd(process_cwd, session.cwd.as_deref()),
                    status_rank(session.status),
                    session.updated_at,
                )
            })
            .map(|(idx, _)| idx)
    }

    pub fn visible_row_for_path(
        &self,
        filter: &str,
        sort: SessionSort,
        path: &Path,
    ) -> Option<usize> {
        self.visible_session_indices(filter, sort)
            .iter()
            .position(|&idx| self.sessions[idx].path == path)
    }
}

fn session_matches_filter(s: &SessionMeta, tokens: &[String]) -> bool {
    tokens.iter().all(|token| session_matches_token(s, token))
}

pub(crate) fn filter_has_token(filter: &str, token: &str) -> bool {
    filter
        .split_whitespace()
        .any(|part| part.eq_ignore_ascii_case(token))
}

pub(crate) fn toggle_filter_token(filter: &mut String, token: &str) {
    let had_token = filter_has_token(filter, token);
    let mut parts = filter
        .split_whitespace()
        .filter(|part| !part.eq_ignore_ascii_case(token))
        .map(str::to_string)
        .collect::<Vec<_>>();
    if !had_token {
        parts.push(token.to_string());
    }
    *filter = parts.join(" ");
}

fn session_matches_token(s: &SessionMeta, token: &str) -> bool {
    if let Some((field, value)) = token.split_once(':') {
        if !value.is_empty() {
            return match field {
                "agent" => {
                    s.agent.eq_ignore_ascii_case(value)
                        || s.agent_label().to_lowercase().contains(value)
                }
                "status" => session_status_matches(s.status, value),
                "cwd" => contains_path(&s.cwd, value),
                "branch" => contains_opt(&s.git_branch, value),
                "id" => s.id.to_lowercase().contains(value),
                "model" => contains_opt(&s.model, value),
                "version" => contains_opt(&s.version, value),
                _ => session_matches_fuzzy(s, token),
            };
        }
    }
    session_matches_fuzzy(s, token)
}

fn session_matches_fuzzy(s: &SessionMeta, needle: &str) -> bool {
    s.id.to_lowercase().contains(needle)
        || s.agent.eq_ignore_ascii_case(needle)
        || s.agent_label().to_lowercase().contains(needle)
        || contains_path(&s.cwd, needle)
        || contains_opt(&s.git_branch, needle)
        || contains_opt(&s.model, needle)
        || contains_opt(&s.version, needle)
        || s.status.label().contains(needle)
}

fn session_status_matches(status: SessionStatus, value: &str) -> bool {
    match value {
        "active" => status == SessionStatus::Active,
        "idle" => status == SessionStatus::Idle,
        "done" | "complete" | "completed" => status == SessionStatus::Completed,
        "unknown" | "?" => status == SessionStatus::Unknown,
        _ => status.label().contains(value),
    }
}

fn contains_path(path: &Option<PathBuf>, needle: &str) -> bool {
    path.as_ref()
        .map(|p| p.to_string_lossy().to_lowercase().contains(needle))
        .unwrap_or(false)
}

fn contains_opt(value: &Option<String>, needle: &str) -> bool {
    value
        .as_ref()
        .map(|s| s.to_lowercase().contains(needle))
        .unwrap_or(false)
}

fn same_cwd(a: Option<&str>, b: Option<&Path>) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => a == b.to_string_lossy(),
        _ => false,
    }
}

fn status_rank(status: SessionStatus) -> u8 {
    match status {
        SessionStatus::Active => 3,
        SessionStatus::Idle => 2,
        SessionStatus::Completed => 1,
        SessionStatus::Unknown => 0,
    }
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
    /// Settings-tab row selection. Indexes into `SettingsItem::all()`.
    pub selected_setting: usize,
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
            selected_setting: 0,
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
    use std::collections::VecDeque;
    use std::path::PathBuf;

    struct SessionFixture {
        agent: &'static str,
        id: &'static str,
        cwd: Option<&'static str>,
        size: u64,
        msgs: usize,
        updated_minutes_ago: i64,
        token_total: u64,
        status: SessionStatus,
    }

    fn mk(f: SessionFixture) -> SessionMeta {
        let now = Utc.with_ymd_and_hms(2026, 4, 20, 12, 0, 0).unwrap();
        SessionMeta {
            agent: f.agent,
            id: f.id.into(),
            path: PathBuf::from(format!("/tmp/{}.jsonl", f.id)),
            cwd: f.cwd.map(PathBuf::from),
            model: None,
            version: None,
            git_branch: None,
            started_at: None,
            updated_at: Some(now - chrono::Duration::minutes(f.updated_minutes_ago)),
            message_count: f.msgs,
            tokens: TokenStats {
                input: f.token_total,
                output: 0,
                cache_read: 0,
                cache_creation: 0,
            },
            status: f.status,
            byte_offset: 0,
            size_bytes: f.size,
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
            mk(SessionFixture {
                agent: "claude",
                id: "abc123",
                cwd: Some("/repos/AgentMonitor"),
                size: 10,
                msgs: 1,
                updated_minutes_ago: 0,
                token_total: 100,
                status: SessionStatus::Active,
            }),
            mk(SessionFixture {
                agent: "codex",
                id: "xyz789",
                cwd: Some("/repos/other"),
                size: 20,
                msgs: 5,
                updated_minutes_ago: 10,
                token_total: 400,
                status: SessionStatus::Idle,
            }),
            mk(SessionFixture {
                agent: "claude",
                id: "def456",
                cwd: None,
                size: 5,
                msgs: 2,
                updated_minutes_ago: 30,
                token_total: 50,
                status: SessionStatus::Completed,
            }),
        ]);
        let idxs = state.visible_session_indices("agent", SessionSort::UpdatedDesc);
        assert_eq!(
            idxs,
            vec![0],
            "filter matches cwd substring case-insensitive"
        );

        let idxs = state.visible_session_indices("XYZ", SessionSort::UpdatedDesc);
        assert_eq!(idxs, vec![1], "filter matches id case-insensitive");

        let idxs = state.visible_session_indices("", SessionSort::UpdatedDesc);
        assert_eq!(
            idxs,
            vec![0, 1, 2],
            "empty filter keeps all, preserves order"
        );
    }

    #[test]
    fn visible_indices_sort_by_size_and_messages() {
        let state = state_with(vec![
            mk(SessionFixture {
                agent: "claude",
                id: "a",
                cwd: None,
                size: 10,
                msgs: 1,
                updated_minutes_ago: 0,
                token_total: 100,
                status: SessionStatus::Active,
            }),
            mk(SessionFixture {
                agent: "claude",
                id: "b",
                cwd: None,
                size: 30,
                msgs: 5,
                updated_minutes_ago: 10,
                token_total: 500,
                status: SessionStatus::Idle,
            }),
            mk(SessionFixture {
                agent: "claude",
                id: "c",
                cwd: None,
                size: 20,
                msgs: 3,
                updated_minutes_ago: 20,
                token_total: 200,
                status: SessionStatus::Completed,
            }),
        ]);
        let by_size = state.visible_session_indices("", SessionSort::SizeDesc);
        assert_eq!(by_size, vec![1, 2, 0]);
        let by_msgs = state.visible_session_indices("", SessionSort::MessagesDesc);
        assert_eq!(by_msgs, vec![1, 2, 0]);
        let by_tokens = state.visible_session_indices("", SessionSort::TokensDesc);
        assert_eq!(by_tokens, vec![1, 2, 0]);
        let by_status = state.visible_session_indices("", SessionSort::StatusDesc);
        assert_eq!(by_status, vec![0, 1, 2]);
    }

    #[test]
    fn session_sort_cycle_round_trips() {
        let s = SessionSort::default();
        assert_eq!(s, SessionSort::UpdatedDesc);
        assert_eq!(s.cycle(), SessionSort::TokensDesc);
        assert_eq!(s.cycle().cycle(), SessionSort::SizeDesc);
        assert_eq!(s.cycle().cycle().cycle(), SessionSort::MessagesDesc);
        assert_eq!(s.cycle().cycle().cycle().cycle(), SessionSort::StatusDesc);
        assert_eq!(
            s.cycle().cycle().cycle().cycle().cycle(),
            SessionSort::UpdatedDesc
        );
    }

    #[test]
    fn visible_indices_support_structured_query_tokens() {
        let state = state_with(vec![
            mk(SessionFixture {
                agent: "claude",
                id: "abc123",
                cwd: Some("/repos/AgentMonitor"),
                size: 10,
                msgs: 1,
                updated_minutes_ago: 0,
                token_total: 100,
                status: SessionStatus::Active,
            }),
            mk(SessionFixture {
                agent: "codex",
                id: "xyz789",
                cwd: Some("/repos/AgentMonitor"),
                size: 20,
                msgs: 5,
                updated_minutes_ago: 10,
                token_total: 400,
                status: SessionStatus::Active,
            }),
            mk(SessionFixture {
                agent: "codex",
                id: "zzz111",
                cwd: Some("/repos/other"),
                size: 25,
                msgs: 4,
                updated_minutes_ago: 20,
                token_total: 200,
                status: SessionStatus::Completed,
            }),
        ]);

        let idxs = state.visible_session_indices(
            "agent:codex status:active agentmonitor",
            SessionSort::UpdatedDesc,
        );
        assert_eq!(idxs, vec![1]);

        let idxs = state.visible_session_indices(
            "status:completed cwd:/repos/other",
            SessionSort::UpdatedDesc,
        );
        assert_eq!(idxs, vec![2]);
    }

    #[test]
    fn best_session_index_for_process_prefers_same_cwd_and_live_session() {
        let state = state_with(vec![
            mk(SessionFixture {
                agent: "codex",
                id: "old-same-cwd",
                cwd: Some("/repos/AgentMonitor"),
                size: 10,
                msgs: 1,
                updated_minutes_ago: 30,
                token_total: 100,
                status: SessionStatus::Completed,
            }),
            mk(SessionFixture {
                agent: "codex",
                id: "active-same-cwd",
                cwd: Some("/repos/AgentMonitor"),
                size: 20,
                msgs: 5,
                updated_minutes_ago: 1,
                token_total: 500,
                status: SessionStatus::Active,
            }),
            mk(SessionFixture {
                agent: "codex",
                id: "wrong-cwd",
                cwd: Some("/repos/other"),
                size: 30,
                msgs: 10,
                updated_minutes_ago: 0,
                token_total: 800,
                status: SessionStatus::Active,
            }),
            mk(SessionFixture {
                agent: "claude",
                id: "other-agent",
                cwd: Some("/repos/AgentMonitor"),
                size: 30,
                msgs: 10,
                updated_minutes_ago: 0,
                token_total: 800,
                status: SessionStatus::Active,
            }),
        ]);
        let process = ProcessEntry {
            agent: "codex",
            pid: 42,
            name: "codex".into(),
            cmd: "codex".into(),
            cwd: Some("/repos/AgentMonitor".into()),
            started_unix: 0,
            samples: VecDeque::new(),
        };

        let idx = state.best_session_index_for_process(&process);
        assert_eq!(idx, Some(1));
    }

    #[test]
    fn toggle_filter_token_adds_and_removes_exact_term() {
        let mut filter = "agent:codex branch:main".to_string();
        toggle_filter_token(&mut filter, "status:active");
        assert!(filter_has_token(&filter, "status:active"));
        assert!(filter_has_token(&filter, "agent:codex"));

        toggle_filter_token(&mut filter, "status:active");
        assert!(!filter_has_token(&filter, "status:active"));
        assert_eq!(filter, "agent:codex branch:main");
    }
}
