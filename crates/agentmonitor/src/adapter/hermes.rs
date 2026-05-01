use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json::Value;
use tokio::task;

use super::conversation::{Block, ConversationEvent};
use super::types::{MessagePreview, MessageRole, SessionMeta, SessionStatus, TokenStats};
use super::AgentAdapter;

/// Hermes Agent (Nous Research) adapter — SQLite-backed like OpenCode.
///
/// Hermes stores all session metadata and message history in a single
/// `~/.hermes/state.db` (WAL mode). There are no per-session files on disk,
/// so we synthesize virtual paths of the form `~/.hermes/sessions/<id>.json`
/// to plug into the rest of the pipeline (fs_watch / token_refresh / viewer).
///
/// Schema (relevant columns only — see Hermes session-storage docs for full):
///   sessions(id TEXT PK, source, model, started_at REAL, ended_at REAL,
///            message_count INT, input_tokens INT, output_tokens INT,
///            cache_read_tokens INT, cache_write_tokens INT,
///            reasoning_tokens INT, title)
///   messages(id INT PK, session_id TEXT, role, content, tool_calls,
///            tool_name, timestamp REAL, reasoning, ...)
///
/// Token totals on the sessions row are **cumulative** (overwritten on each
/// turn), matching the same model as Codex — `token_refresh` writes them in
/// directly without summing per-message rows.
pub struct HermesAdapter {
    db_path: Option<PathBuf>,
}

impl HermesAdapter {
    pub fn new(db_path: Option<PathBuf>) -> Self {
        Self { db_path }
    }

    fn base_meta(&self, session_id: &str, virtual_path: PathBuf) -> SessionMeta {
        SessionMeta {
            agent: self.id(),
            id: session_id.to_string(),
            path: virtual_path,
            cwd: None,
            model: None,
            version: None,
            git_branch: None,
            source: None,
            started_at: None,
            updated_at: None,
            message_count: 0,
            tokens: TokenStats::default(),
            status: SessionStatus::Unknown,
            byte_offset: 0,
            size_bytes: 0,
        }
    }

    fn virtual_path_for(&self, session_id: &str) -> Option<PathBuf> {
        self.db_path.as_ref().map(|db| {
            db.parent()
                .unwrap_or(Path::new(""))
                .join("sessions")
                .join(format!("{}.json", session_id))
        })
    }

    fn session_id_from_path(&self, path: &Path) -> Option<String> {
        if !self.owns_path(path) {
            return None;
        }
        path.file_stem()
            .and_then(|s| s.to_str())
            .map(|s| s.to_string())
    }
}

#[async_trait]
impl AgentAdapter for HermesAdapter {
    fn id(&self) -> &'static str {
        "hermes"
    }

    fn display_name(&self) -> &'static str {
        "Hermes"
    }

    fn session_roots(&self) -> Vec<PathBuf> {
        Vec::new()
    }

    fn owns_path(&self, path: &Path) -> bool {
        path.parent()
            .and_then(|p| p.file_name())
            .and_then(|s| s.to_str())
            == Some("sessions")
            && path.extension().and_then(|s| s.to_str()) == Some("json")
            && self
                .db_path
                .as_ref()
                .and_then(|db| db.parent())
                .zip(path.parent().and_then(|p| p.parent()))
                .is_some_and(|(db_parent, virt_parent)| db_parent == virt_parent)
    }

    fn matches_process(&self, cmd: &[String], _exe: Option<&Path>) -> bool {
        cmd.iter().any(|s| {
            s == "hermes"
                || s.ends_with("/hermes")
                || s.contains("hermes_cli.main")
                || s.contains("hermes_cli/main.py")
        })
    }

    fn needs_fs_stat(&self) -> bool {
        false
    }

    async fn parse_meta_fast(&self, path: &Path) -> Result<SessionMeta> {
        let session_id = self
            .session_id_from_path(path)
            .context("invalid hermes virtual path")?;
        let db_path = self
            .db_path
            .clone()
            .context("hermes db path not configured")?;
        let virtual_path = path.to_path_buf();

        let id = session_id.clone();
        let row = task::spawn_blocking(move || {
            let conn = open_hermes_db(&db_path)?;

            let mut stmt = conn.prepare(
                "SELECT s.source, s.model, s.started_at, s.ended_at, s.message_count, \
                 (SELECT MAX(m.timestamp) FROM messages m WHERE m.session_id = s.id) \
                 FROM sessions s WHERE s.id = ?1",
            )?;

            let row = stmt.query_row([&id], |row| {
                Ok((
                    row.get::<_, Option<String>>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<f64>>(2)?,
                    row.get::<_, Option<f64>>(3)?,
                    row.get::<_, Option<i64>>(4)?,
                    row.get::<_, Option<f64>>(5)?,
                ))
            });

            match row {
                Ok(r) => Ok::<_, anyhow::Error>(Some(r)),
                Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
                Err(e) => Err(e.into()),
            }
        })
        .await
        .context("spawn_blocking failed")?;

        let row = match row? {
            Some(r) => r,
            None => return Ok(self.base_meta(&session_id, virtual_path)),
        };

        let (source, model, started_at, ended_at, message_count, last_msg_ts) = row;
        let mut meta = self.base_meta(&session_id, virtual_path);
        meta.model = model;
        meta.source = source;
        meta.started_at = started_at.and_then(unix_secs_to_dt);
        meta.updated_at = pick_updated_at(ended_at, last_msg_ts, started_at);
        meta.message_count = message_count.unwrap_or(0).max(0) as usize;
        meta.status = infer_status(ended_at.is_some(), meta.updated_at);

        Ok(meta)
    }

    async fn parse_meta_full(&self, path: &Path) -> Result<SessionMeta> {
        let mut meta = self.parse_meta_fast(path).await?;

        let session_id = self
            .session_id_from_path(path)
            .context("invalid hermes virtual path")?;
        let db_path = self
            .db_path
            .clone()
            .context("hermes db path not configured")?;

        let id = session_id;
        let (count, tokens) = task::spawn_blocking(move || {
            let conn = open_hermes_db(&db_path)?;

            // Sessions row carries cumulative counters maintained by
            // hermes_state on every turn; trust those rather than re-summing
            // per-message token_count (which is per-row delta and rounds).
            let row = conn.query_row(
                "SELECT message_count, input_tokens, output_tokens, \
                 cache_read_tokens, cache_write_tokens \
                 FROM sessions WHERE id = ?1",
                [&id],
                |row| {
                    Ok((
                        row.get::<_, Option<i64>>(0)?,
                        row.get::<_, Option<i64>>(1)?,
                        row.get::<_, Option<i64>>(2)?,
                        row.get::<_, Option<i64>>(3)?,
                        row.get::<_, Option<i64>>(4)?,
                    ))
                },
            );
            let (msg_count, input, output, cache_read, cache_write) = match row {
                Ok(r) => r,
                Err(rusqlite::Error::QueryReturnedNoRows) => {
                    return Ok::<_, anyhow::Error>((0usize, TokenStats::default()));
                }
                Err(e) => return Err(e.into()),
            };

            let tokens = TokenStats {
                input: input.unwrap_or(0).max(0) as u64,
                output: output.unwrap_or(0).max(0) as u64,
                cache_read: cache_read.unwrap_or(0).max(0) as u64,
                cache_creation: cache_write.unwrap_or(0).max(0) as u64,
            };
            // Prefer the live messages count (the trigger-maintained
            // counter on the sessions row drifts during multi-process
            // contention) but fall back to it if the table is gone.
            let live_count: Option<i64> = conn
                .query_row(
                    "SELECT COUNT(*) FROM messages WHERE session_id = ?1",
                    [&id],
                    |row| row.get(0),
                )
                .ok();
            let count = live_count
                .or(msg_count)
                .unwrap_or(0)
                .max(0) as usize;

            Ok::<_, anyhow::Error>((count, tokens))
        })
        .await
        .context("spawn_blocking failed")??;

        meta.message_count = count;
        meta.tokens = tokens;

        Ok(meta)
    }

    async fn scan_all(&self) -> Result<Vec<SessionMeta>> {
        let db_path = match self.db_path.clone() {
            Some(p) => p,
            None => return Ok(Vec::new()),
        };
        if !db_path.exists() {
            // Hermes not installed / never run yet — silently skip rather
            // than logging a warn on every reconcile tick.
            return Ok(Vec::new());
        }

        let rows = task::spawn_blocking(move || {
            let conn = open_hermes_db(&db_path)?;

            let mut stmt = conn.prepare(
                "SELECT s.id, s.source, s.model, s.started_at, s.ended_at, s.message_count, \
                 (SELECT MAX(m.timestamp) FROM messages m WHERE m.session_id = s.id) \
                 FROM sessions s \
                 ORDER BY s.started_at DESC",
            )?;

            let rows = stmt.query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<f64>>(3)?,
                    row.get::<_, Option<f64>>(4)?,
                    row.get::<_, Option<i64>>(5)?,
                    row.get::<_, Option<f64>>(6)?,
                ))
            })?;

            let mut out = Vec::new();
            for r in rows {
                out.push(r?);
            }
            Ok::<_, anyhow::Error>(out)
        })
        .await
        .context("spawn_blocking failed")??;

        let mut sessions = Vec::with_capacity(rows.len());
        for (sid, source, model, started_at, ended_at, message_count, last_msg_ts) in rows {
            let Some(virtual_path) = self.virtual_path_for(&sid) else {
                continue;
            };
            let mut meta = self.base_meta(&sid, virtual_path);
            meta.model = model;
            meta.source = source;
            meta.started_at = started_at.and_then(unix_secs_to_dt);
            meta.updated_at = pick_updated_at(ended_at, last_msg_ts, started_at);
            meta.message_count = message_count.unwrap_or(0).max(0) as usize;
            meta.status = infer_status(ended_at.is_some(), meta.updated_at);
            sessions.push(meta);
        }
        Ok(sessions)
    }

    async fn tail_messages(&self, path: &Path, n: usize) -> Result<Vec<MessagePreview>> {
        let session_id = self
            .session_id_from_path(path)
            .context("invalid hermes virtual path")?;
        let db_path = self
            .db_path
            .clone()
            .context("hermes db path not configured")?;

        let id = session_id;
        let limit = n;
        let rows = task::spawn_blocking(move || {
            let conn = open_hermes_db(&db_path)?;

            let mut stmt = conn.prepare(
                "SELECT role, content, tool_calls, tool_name, timestamp \
                 FROM messages WHERE session_id = ?1 \
                 ORDER BY timestamp DESC LIMIT ?2",
            )?;

            let rows = stmt.query_map(
                rusqlite::params![&id, limit as i64],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, Option<String>>(3)?,
                        row.get::<_, f64>(4)?,
                    ))
                },
            )?;

            let mut out = Vec::new();
            for r in rows {
                out.push(r?);
            }
            Ok::<_, anyhow::Error>(out)
        })
        .await
        .context("spawn_blocking failed")??;

        let mut previews = Vec::with_capacity(rows.len());
        for (role_raw, content, tool_calls, tool_name, ts) in rows {
            let role = parse_role(&role_raw);
            let text = match (content.as_deref(), tool_name.as_deref()) {
                (Some(c), _) if !c.trim().is_empty() => c.to_string(),
                (_, Some(name)) if !name.is_empty() => format!("→ {}", name),
                _ => match tool_calls.as_deref() {
                    Some(tc) if !tc.is_empty() => summarize_tool_calls(tc),
                    _ => continue,
                },
            };
            previews.push(MessagePreview {
                ts: unix_secs_to_dt(ts),
                role,
                text: truncate(text, 600),
            });
        }
        previews.reverse();
        Ok(previews)
    }

    async fn load_conversation(&self, path: &Path) -> Result<Vec<ConversationEvent>> {
        let session_id = self
            .session_id_from_path(path)
            .context("invalid hermes virtual path")?;
        let db_path = self
            .db_path
            .clone()
            .context("hermes db path not configured")?;

        let id = session_id;
        let rows = task::spawn_blocking(move || {
            let conn = open_hermes_db(&db_path)?;

            let mut stmt = conn.prepare(
                "SELECT role, content, tool_calls, tool_name, tool_call_id, \
                 reasoning, reasoning_content, timestamp \
                 FROM messages WHERE session_id = ?1 \
                 ORDER BY timestamp",
            )?;

            let rows = stmt.query_map([&id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, Option<String>>(6)?,
                    row.get::<_, f64>(7)?,
                ))
            })?;

            let mut out = Vec::new();
            for r in rows {
                out.push(r?);
            }
            Ok::<_, anyhow::Error>(out)
        })
        .await
        .context("spawn_blocking failed")??;

        let mut events = Vec::new();
        for (role_raw, content, tool_calls, tool_name, _tool_call_id, reasoning, reasoning_content, ts) in
            rows
        {
            let role = parse_role(&role_raw);
            let ts = unix_secs_to_dt(ts);
            let mut blocks = Vec::new();

            if let Some(r) = reasoning.or(reasoning_content) {
                if !r.trim().is_empty() {
                    blocks.push(Block::Thinking(r));
                }
            }

            if let Some(c) = content {
                if !c.trim().is_empty() {
                    blocks.push(Block::Text(c));
                }
            }

            // Assistant turns can carry tool_calls (a JSON array of OpenAI-shaped
            // call objects). Tool result rows have role=tool with content holding
            // the result and tool_name naming which tool replied.
            if let Some(tc) = tool_calls.as_deref() {
                if !tc.is_empty() && tc != "null" {
                    if let Ok(arr) = serde_json::from_str::<Value>(tc) {
                        if let Some(calls) = arr.as_array() {
                            for call in calls {
                                if let Some(b) = tool_call_to_block(call) {
                                    blocks.push(b);
                                }
                            }
                        }
                    }
                }
            }

            if matches!(role, MessageRole::Tool) {
                if let Some(name) = tool_name {
                    // Replace the placeholder Text block we may have just
                    // pushed with a structured ToolResult so the viewer
                    // renders it correctly.
                    let result_text = match blocks.pop() {
                        Some(Block::Text(t)) => t,
                        Some(other) => {
                            blocks.push(other);
                            String::new()
                        }
                        None => String::new(),
                    };
                    blocks.push(Block::ToolResult {
                        is_error: false,
                        content: if result_text.is_empty() {
                            format!("[{}]", name)
                        } else {
                            result_text
                        },
                    });
                }
            }

            if !blocks.is_empty() {
                events.push(ConversationEvent { ts, role, blocks });
            }
        }
        Ok(events)
    }
}

/// Open the Hermes state.db read-only with a friendlier busy timeout. Hermes
/// is a heavy concurrent writer (gateway + CLI sessions + cron) and the
/// default 5s wait is excessive when our caller only wants header data.
fn open_hermes_db(db_path: &Path) -> Result<rusqlite::Connection> {
    let flags = rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY
        | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX
        | rusqlite::OpenFlags::SQLITE_OPEN_URI;
    let conn = rusqlite::Connection::open_with_flags(db_path, flags)
        .with_context(|| format!("open hermes db {}", db_path.display()))?;
    conn.busy_timeout(std::time::Duration::from_millis(750))?;
    Ok(conn)
}

fn parse_role(raw: &str) -> MessageRole {
    match raw {
        "user" => MessageRole::User,
        "assistant" => MessageRole::Assistant,
        "system" => MessageRole::System,
        "tool" | "function" => MessageRole::Tool,
        other => MessageRole::Other(other.to_string()),
    }
}

/// Render a single OpenAI-shaped tool_call entry into a viewer Block.
fn tool_call_to_block(call: &Value) -> Option<Block> {
    let function = call.get("function").or(Some(call))?;
    let name = function.get("name").and_then(|v| v.as_str()).unwrap_or("?");
    let raw_args = function.get("arguments");
    let arg_str = match raw_args {
        Some(Value::String(s)) => s.clone(),
        Some(other) => serde_json::to_string_pretty(other).unwrap_or_default(),
        None => String::new(),
    };
    let preview_args = first_line_truncated(&arg_str, 100);
    let preview = if preview_args.is_empty() {
        name.to_string()
    } else {
        format!("{}  {}", name, preview_args)
    };
    Some(Block::ToolUse {
        name: name.to_string(),
        preview,
        input: arg_str,
    })
}

fn summarize_tool_calls(raw: &str) -> String {
    let v: Value = match serde_json::from_str(raw) {
        Ok(v) => v,
        Err(_) => return String::new(),
    };
    let arr = match v.as_array() {
        Some(a) => a,
        None => return String::new(),
    };
    let names: Vec<&str> = arr
        .iter()
        .filter_map(|c| {
            c.get("function")
                .and_then(|f| f.get("name"))
                .or_else(|| c.get("name"))
                .and_then(|n| n.as_str())
        })
        .collect();
    if names.is_empty() {
        String::new()
    } else {
        format!("→ {}", names.join(", "))
    }
}

fn unix_secs_to_dt(secs: f64) -> Option<DateTime<Utc>> {
    if !secs.is_finite() {
        return None;
    }
    let millis = (secs * 1000.0) as i64;
    DateTime::from_timestamp_millis(millis)
}

/// Decide which timestamp best represents "last activity" for a session.
/// Order of preference: max(messages.timestamp), ended_at, started_at.
/// We don't blindly take ended_at first because it's recorded on graceful
/// exit only — a still-running session would otherwise show its start time
/// as its updated_at and look stale on the dashboard.
fn pick_updated_at(
    ended_at: Option<f64>,
    last_msg_ts: Option<f64>,
    started_at: Option<f64>,
) -> Option<DateTime<Utc>> {
    let mut best: Option<f64> = None;
    for cand in [last_msg_ts, ended_at, started_at].into_iter().flatten() {
        best = Some(match best {
            Some(prev) if prev >= cand => prev,
            _ => cand,
        });
    }
    best.and_then(unix_secs_to_dt)
}

fn infer_status(ended: bool, updated_at: Option<DateTime<Utc>>) -> SessionStatus {
    let Some(t) = updated_at else {
        return SessionStatus::Unknown;
    };
    let age = Utc::now() - t;
    // ended_at flips to Completed only when the session is also stale.
    // Hermes records ended_at on /reset and on graceful exit, so an active
    // chat that just sent /reset and immediately resumed would otherwise
    // flicker to "done" for one tick.
    if ended && age.num_seconds() >= 60 {
        return SessionStatus::Completed;
    }
    if age.num_seconds() < 60 {
        SessionStatus::Active
    } else if age.num_hours() < 24 {
        SessionStatus::Idle
    } else {
        SessionStatus::Completed
    }
}

fn truncate(s: String, max: usize) -> String {
    if s.chars().count() <= max {
        s
    } else {
        let mut out: String = s.chars().take(max).collect();
        out.push('…');
        out
    }
}

fn first_line_truncated(s: &str, max: usize) -> String {
    let line = s.lines().next().unwrap_or("").trim();
    let count = line.chars().count();
    if count <= max {
        line.to_string()
    } else {
        let mut out: String = line.chars().take(max).collect();
        out.push('…');
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    /// Drop-guard for a temp dir so test cleanup is automatic. We don't pull
    /// in `tempfile` because the rest of the crate already uses
    /// `std::env::temp_dir()` + a unique suffix for its tests.
    struct TempDir(PathBuf);
    impl TempDir {
        fn new(label: &str) -> Self {
            let unique = format!(
                "agentmonitor-hermes-{}-{}-{}",
                label,
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map(|d| d.as_nanos())
                    .unwrap_or(0),
            );
            let path = std::env::temp_dir().join(unique);
            std::fs::create_dir_all(&path).expect("create temp dir");
            TempDir(path)
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn make_adapter_with_temp_db(label: &str) -> (HermesAdapter, TempDir, PathBuf) {
        let dir = TempDir::new(label);
        let db_path = dir.path().join("state.db");
        let conn = rusqlite::Connection::open(&db_path).expect("open db");
        conn.execute_batch(
            "CREATE TABLE sessions (
                id TEXT PRIMARY KEY,
                source TEXT,
                model TEXT,
                started_at REAL,
                ended_at REAL,
                message_count INTEGER DEFAULT 0,
                input_tokens INTEGER DEFAULT 0,
                output_tokens INTEGER DEFAULT 0,
                cache_read_tokens INTEGER DEFAULT 0,
                cache_write_tokens INTEGER DEFAULT 0
             );
             CREATE TABLE messages (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id TEXT,
                role TEXT,
                content TEXT,
                tool_calls TEXT,
                tool_name TEXT,
                tool_call_id TEXT,
                reasoning TEXT,
                reasoning_content TEXT,
                timestamp REAL
             );",
        )
        .expect("create schema");
        let adapter = HermesAdapter::new(Some(db_path.clone()));
        (adapter, dir, db_path)
    }

    #[test]
    fn parses_session_id_from_virtual_path() {
        let adapter = HermesAdapter::new(Some(PathBuf::from("/home/u/.hermes/state.db")));
        let path = Path::new("/home/u/.hermes/sessions/sess_xyz.json");
        assert_eq!(
            adapter.session_id_from_path(path),
            Some("sess_xyz".to_string())
        );
    }

    #[test]
    fn rejects_non_virtual_path() {
        let adapter = HermesAdapter::new(Some(PathBuf::from("/home/u/.hermes/state.db")));
        // Wrong parent directory name.
        assert!(!adapter.owns_path(Path::new("/home/u/.hermes/state.db")));
        // Wrong extension.
        assert!(!adapter.owns_path(Path::new("/home/u/.hermes/sessions/sess.jsonl")));
        // Path under a different agent's tree (e.g. opencode also synthesizes
        // sessions/<id>.json virtual paths) must NOT be claimed.
        assert!(!adapter.owns_path(Path::new(
            "/home/u/.local/share/opencode/sessions/ses_abc.json"
        )));
    }

    #[test]
    fn matches_hermes_process() {
        let adapter = HermesAdapter::new(None);
        assert!(adapter.matches_process(&["hermes".to_string()], None));
        assert!(adapter.matches_process(&["/usr/local/bin/hermes".to_string()], None));
        assert!(adapter.matches_process(
            &[
                "/Users/u/.hermes/hermes-agent/venv/bin/python".to_string(),
                "-m".to_string(),
                "hermes_cli.main".to_string(),
                "chat".to_string(),
            ],
            None,
        ));
        // hermes-agent in path component (e.g. cwd argument) must NOT
        // count — only the hermes binary or hermes_cli.main module should.
        assert!(!adapter.matches_process(
            &["bash".to_string(), "-c".to_string(), "ls /home/u/.hermes/hermes-agent".to_string()],
            None,
        ));
        assert!(!adapter.matches_process(&["claude".to_string()], None));
    }

    #[test]
    fn pick_updated_at_prefers_last_message_then_ended() {
        // last_msg > ended > started — last_msg wins.
        assert_eq!(
            pick_updated_at(Some(10.0), Some(20.0), Some(5.0)),
            unix_secs_to_dt(20.0)
        );
        // No messages, ended_at known.
        assert_eq!(
            pick_updated_at(Some(15.0), None, Some(10.0)),
            unix_secs_to_dt(15.0)
        );
        // Active session with messages but no ended_at — message ts wins.
        assert_eq!(
            pick_updated_at(None, Some(30.0), Some(5.0)),
            unix_secs_to_dt(30.0)
        );
        // Bare new session.
        assert_eq!(
            pick_updated_at(None, None, Some(7.0)),
            unix_secs_to_dt(7.0)
        );
    }

    #[test]
    fn infer_status_recent_overrides_ended() {
        // Just sent /reset 10s ago: ended_at set, age < 60s -> still Active.
        let recent = Some(Utc::now() - chrono::Duration::seconds(10));
        assert_eq!(infer_status(true, recent), SessionStatus::Active);
        // Same age, no ended_at -> Active.
        assert_eq!(infer_status(false, recent), SessionStatus::Active);
    }

    #[test]
    fn infer_status_old_ended_completes() {
        let stale = Some(Utc::now() - chrono::Duration::hours(2));
        assert_eq!(infer_status(true, stale), SessionStatus::Completed);
        // Without ended_at, the same age is just Idle.
        assert_eq!(infer_status(false, stale), SessionStatus::Idle);
    }

    #[test]
    fn token_call_to_block_handles_string_args() {
        let v: Value = serde_json::json!({
            "id": "call_1",
            "type": "function",
            "function": {"name": "shell", "arguments": "{\"cmd\": \"ls\"}"}
        });
        match tool_call_to_block(&v) {
            Some(Block::ToolUse { name, preview, input }) => {
                assert_eq!(name, "shell");
                assert!(preview.contains("shell"));
                assert!(input.contains("\"cmd\""));
            }
            _ => panic!("expected ToolUse"),
        }
    }

    #[test]
    fn cwd_display_falls_back_to_source_for_hermes() {
        let (adapter, _dir, _db_path) = make_adapter_with_temp_db("cwd-display");
        let mut meta = adapter.base_meta("s1", PathBuf::from("/tmp/s1.json"));
        meta.source = Some("cli".into());
        assert_eq!(meta.cwd_display(), "〈cli〉");

        meta.source = Some("telegram".into());
        assert_eq!(meta.cwd_display(), "〈telegram〉");

        meta.source = None;
        assert_eq!(meta.cwd_display(), "?");

        // A real cwd, when present, still wins over source.
        meta.cwd = Some(PathBuf::from("/work/repo"));
        meta.source = Some("cli".into());
        assert_eq!(meta.cwd_display(), "/work/repo");
    }

    #[tokio::test]
    async fn parse_meta_full_reads_cumulative_tokens() {
        let (adapter, _dir, db_path) = make_adapter_with_temp_db("full-tokens");
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute(
            "INSERT INTO sessions (id, source, model, started_at, ended_at,
             message_count, input_tokens, output_tokens, cache_read_tokens, cache_write_tokens)
             VALUES (?1, 'cli', 'anthropic/claude-sonnet-4.6', 1714000000.0, NULL,
                     4, 1500, 800, 12000, 200)",
            ["sess_a"],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO messages (session_id, role, content, timestamp)
             VALUES ('sess_a', 'user', 'hi', 1714000010.0),
                    ('sess_a', 'assistant', 'hello', 1714000020.0)",
            [],
        )
        .unwrap();

        let path = adapter.virtual_path_for("sess_a").unwrap();
        let meta = adapter.parse_meta_full(&path).await.expect("parse_meta_full");
        assert_eq!(meta.id, "sess_a");
        assert_eq!(meta.agent, "hermes");
        assert_eq!(meta.tokens.input, 1500);
        assert_eq!(meta.tokens.output, 800);
        assert_eq!(meta.tokens.cache_read, 12000);
        assert_eq!(meta.tokens.cache_creation, 200);
        // The trigger-maintained sessions.message_count says 4 but the live
        // table has 2 — we trust the live count.
        assert_eq!(meta.message_count, 2);
        assert_eq!(meta.model.as_deref(), Some("anthropic/claude-sonnet-4.6"));
        assert_eq!(meta.source.as_deref(), Some("cli"));
        assert!(meta.git_branch.is_none());
    }

    #[tokio::test]
    async fn scan_all_returns_empty_when_db_missing() {
        let dir = TempDir::new("missing-db");
        let adapter = HermesAdapter::new(Some(dir.path().join("does-not-exist.db")));
        let sessions = adapter.scan_all().await.expect("scan_all");
        assert!(sessions.is_empty());
    }

    #[tokio::test]
    async fn scan_all_orders_by_started_desc() {
        let (adapter, _dir, db_path) = make_adapter_with_temp_db("scan-order");
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute(
            "INSERT INTO sessions (id, source, started_at) VALUES
              ('old',   'cli', 1714000000.0),
              ('newer', 'cli', 1714010000.0),
              ('newest','cli', 1714020000.0)",
            [],
        )
        .unwrap();
        let sessions = adapter.scan_all().await.expect("scan_all");
        let ids: Vec<&str> = sessions.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, vec!["newest", "newer", "old"]);
    }

    #[tokio::test]
    async fn tail_messages_returns_chronological() {
        let (adapter, _dir, db_path) = make_adapter_with_temp_db("tail");
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute(
            "INSERT INTO sessions (id, source, started_at) VALUES ('s1', 'cli', 1.0)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO messages (session_id, role, content, timestamp) VALUES
              ('s1', 'user', 'first', 10.0),
              ('s1', 'assistant', 'second', 20.0),
              ('s1', 'user', 'third', 30.0)",
            [],
        )
        .unwrap();
        let path = adapter.virtual_path_for("s1").unwrap();
        let previews = adapter.tail_messages(&path, 2).await.expect("tail");
        assert_eq!(previews.len(), 2);
        assert_eq!(previews[0].text, "second");
        assert_eq!(previews[1].text, "third");
    }

    #[tokio::test]
    async fn parse_meta_fast_handles_missing_session() {
        let (adapter, _dir, _db_path) = make_adapter_with_temp_db("missing-row");
        let path = adapter.virtual_path_for("nope").unwrap();
        // Session row absent: should return base meta without erroring.
        let meta = adapter.parse_meta_fast(&path).await.expect("parse_meta_fast");
        assert_eq!(meta.id, "nope");
        assert_eq!(meta.tokens.total(), 0);
        assert_eq!(meta.message_count, 0);
    }
}
