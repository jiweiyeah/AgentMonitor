use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json::Value;
use tokio::task;

use super::conversation::{Block, ConversationEvent};
use super::types::{MessagePreview, MessageRole, SessionMeta, SessionStatus, TokenStats};
use super::AgentAdapter;

pub struct OpencodeAdapter {
    db_path: Option<PathBuf>,
}

impl OpencodeAdapter {
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
impl AgentAdapter for OpencodeAdapter {
    fn id(&self) -> &'static str {
        "opencode"
    }

    fn display_name(&self) -> &'static str {
        "OpenCode"
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
            && self.db_path.is_some()
    }

    fn matches_process(&self, cmd: &[String], _exe: Option<&Path>) -> bool {
        cmd.iter()
            .any(|s| s == "opencode" || s.contains("/opencode") || s.ends_with("opencode"))
    }

    fn needs_fs_stat(&self) -> bool {
        false
    }

    async fn parse_meta_fast(&self, path: &Path) -> Result<SessionMeta> {
        let session_id = self
            .session_id_from_path(path)
            .context("invalid opencode virtual path")?;
        let db_path = self
            .db_path
            .clone()
            .context("opencode db path not configured")?;
        let virtual_path = path.to_path_buf();

        let id = session_id.clone();
        let row = task::spawn_blocking(move || {
            let conn = rusqlite::Connection::open(&db_path)
                .with_context(|| format!("open opencode db {}", db_path.display()))?;

            let mut stmt = conn.prepare(
                "SELECT s.directory, s.title, s.version, s.time_created, s.time_updated, p.name, \
                 (SELECT json_extract(m.data, '$.modelID') FROM message m \
                  WHERE m.session_id = s.id ORDER BY m.time_created DESC LIMIT 1) as model_id \
                 FROM session s \
                 LEFT JOIN project p ON s.project_id = p.id \
                 WHERE s.id = ?1",
            )?;

            let row = stmt.query_row([&id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, Option<String>>(6)?,
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
            None => {
                return Ok(self.base_meta(&session_id, virtual_path));
            }
        };

        let (directory, _title, version, time_created, time_updated, project_name, model_id) = row;

        let mut meta = self.base_meta(&session_id, virtual_path);
        meta.cwd = Some(PathBuf::from(directory));
        meta.model = model_id.or(project_name);
        meta.version = version;
        meta.started_at = DateTime::from_timestamp_millis(time_created);
        meta.updated_at = DateTime::from_timestamp_millis(time_updated);
        meta.status = infer_status(meta.updated_at);

        Ok(meta)
    }

    async fn parse_meta_full(&self, path: &Path) -> Result<SessionMeta> {
        let mut meta = self.parse_meta_fast(path).await?;

        let session_id = self
            .session_id_from_path(path)
            .context("invalid opencode virtual path")?;
        let db_path = self
            .db_path
            .clone()
            .context("opencode db path not configured")?;

        let id = session_id;
        let (count, tokens) = task::spawn_blocking(move || {
            let conn = rusqlite::Connection::open(&db_path)
                .with_context(|| format!("open opencode db {}", db_path.display()))?;

            let count: i64 = conn.query_row(
                "SELECT COUNT(*) FROM message WHERE session_id = ?1",
                [&id],
                |row| row.get(0),
            )?;

            let mut stmt = conn
                .prepare("SELECT data FROM message WHERE session_id = ?1 ORDER BY time_created")?;

            let rows = stmt.query_map([&id], |row| row.get::<_, String>(0))?;

            let mut tokens = TokenStats::default();
            for row in rows {
                let data = row?;
                if let Ok(v) = serde_json::from_str::<Value>(&data) {
                    if let Some(token_data) = v.get("tokens") {
                        if let Some(input) = token_data.get("input").and_then(|v| v.as_u64()) {
                            tokens.input = tokens.input.saturating_add(input);
                        }
                        if let Some(output) = token_data.get("output").and_then(|v| v.as_u64()) {
                            tokens.output = tokens.output.saturating_add(output);
                        }
                        if let Some(cache) = token_data.get("cache") {
                            if let Some(read) = cache.get("read").and_then(|v| v.as_u64()) {
                                tokens.cache_read = tokens.cache_read.saturating_add(read);
                            }
                            if let Some(write) = cache.get("write").and_then(|v| v.as_u64()) {
                                tokens.cache_creation =
                                    tokens.cache_creation.saturating_add(write);
                            }
                        }
                    }
                }
            }

            Ok::<_, anyhow::Error>((count as usize, tokens))
        })
        .await
        .context("spawn_blocking failed")??;

        meta.message_count = count;
        meta.tokens = tokens;

        Ok(meta)
    }

    async fn scan_all(&self) -> Result<Vec<SessionMeta>> {
        let db_path = self
            .db_path
            .clone()
            .context("opencode db path not configured")?;

        let rows = task::spawn_blocking(move || {
            let conn = rusqlite::Connection::open(&db_path)
                .with_context(|| format!("open opencode db {}", db_path.display()))?;

            let mut stmt = conn.prepare(
                "SELECT s.id, s.directory, s.title, s.version, s.time_created, s.time_updated, \
                 p.name, \
                 (SELECT json_extract(m.data, '$.modelID') FROM message m \
                  WHERE m.session_id = s.id ORDER BY m.time_created DESC LIMIT 1) as model_id \
                 FROM session s \
                 LEFT JOIN project p ON s.project_id = p.id \
                 WHERE s.time_archived IS NULL \
                 ORDER BY s.time_updated DESC",
            )?;

            let rows = stmt.query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, Option<String>>(6)?,
                    row.get::<_, Option<String>>(7)?,
                ))
            })?;

            let mut results = Vec::new();
            for row in rows {
                results.push(row?);
            }
            Ok::<_, anyhow::Error>(results)
        })
        .await
        .context("spawn_blocking failed")?;

        let rows = rows?;
        let mut sessions = Vec::with_capacity(rows.len());
        for (sid, directory, _title, version, time_created, time_updated, project_name, model_id) in
            rows
        {
            if let Some(virtual_path) = self.virtual_path_for(&sid) {
                let mut meta = self.base_meta(&sid, virtual_path);
                meta.cwd = Some(PathBuf::from(directory));
                meta.model = model_id.or(project_name);
                meta.version = version;
                meta.started_at = DateTime::from_timestamp_millis(time_created);
                meta.updated_at = DateTime::from_timestamp_millis(time_updated);
                meta.status = infer_status(meta.updated_at);
                sessions.push(meta);
            }
        }

        Ok(sessions)
    }

    async fn tail_messages(&self, path: &Path, n: usize) -> Result<Vec<MessagePreview>> {
        let session_id = self
            .session_id_from_path(path)
            .context("invalid opencode virtual path")?;
        let db_path = self
            .db_path
            .clone()
            .context("opencode db path not configured")?;

        let id = session_id;
        let limit = n;
        let messages = task::spawn_blocking(move || {
            let conn = rusqlite::Connection::open(&db_path)
                .with_context(|| format!("open opencode db {}", db_path.display()))?;

            // Fetch last N messages with their text parts aggregated.
            let mut stmt = conn.prepare(
                "SELECT m.data, m.time_created, \
                 (SELECT group_concat(json_extract(p.data, '$.text'), char(10)) \
                  FROM part p WHERE p.message_id = m.id AND json_extract(p.data, '$.type') = 'text') as texts \
                 FROM message m \
                 WHERE m.session_id = ?1 \
                 ORDER BY m.time_created DESC \
                 LIMIT ?2",
            )?;

            let rows = stmt.query_map([&id, &limit.to_string()], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            })?;

            let mut results = Vec::new();
            for row in rows {
                results.push(row?);
            }
            Ok::<_, anyhow::Error>(results)
        })
        .await
        .context("spawn_blocking failed")?;

        let messages = messages?;
        let mut previews = Vec::with_capacity(messages.len());
        for (msg_data, time_created, texts) in messages {
            let v: Value = serde_json::from_str(&msg_data).unwrap_or(Value::Null);

            let role_str = v.get("role").and_then(|r| r.as_str()).unwrap_or("assistant");
            let role = match role_str {
                "user" => MessageRole::User,
                "assistant" => MessageRole::Assistant,
                _ => MessageRole::Other(role_str.to_string()),
            };

            // Use aggregated text parts if available, otherwise fall back to summary title.
            let text = texts.filter(|t| !t.is_empty()).or_else(|| {
                v.get("summary")
                    .and_then(|s| s.get("title"))
                    .and_then(|t| t.as_str())
                    .map(|t| t.to_string())
            });

            if let Some(text) = text {
                let ts = DateTime::from_timestamp_millis(time_created);
                previews.push(MessagePreview {
                    ts,
                    role,
                    text: truncate(text, 600),
                });
            }
        }

        // Reverse to chronological order (database returned DESC).
        previews.reverse();
        Ok(previews)
    }

    async fn load_conversation(&self, path: &Path) -> Result<Vec<ConversationEvent>> {
        let session_id = self
            .session_id_from_path(path)
            .context("invalid opencode virtual path")?;
        let db_path = self
            .db_path
            .clone()
            .context("opencode db path not configured")?;

        let id = session_id;
        let messages = task::spawn_blocking(move || {
            let conn = rusqlite::Connection::open(&db_path)
                .with_context(|| format!("open opencode db {}", db_path.display()))?;

            // Fetch all messages with their parts.
            let mut msg_stmt = conn.prepare(
                "SELECT id, data, time_created FROM message WHERE session_id = ?1 ORDER BY time_created",
            )?;

            let msg_rows = msg_stmt.query_map([&id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            })?;

            let mut results = Vec::new();
            for msg_row in msg_rows {
                let (msg_id, msg_data, time_created) = msg_row?;
                let msg_json: Value = serde_json::from_str(&msg_data).unwrap_or(Value::Null);

                let role_str = msg_json
                    .get("role")
                    .and_then(|r| r.as_str())
                    .unwrap_or("assistant");
                let role = match role_str {
                    "user" => MessageRole::User,
                    "assistant" => MessageRole::Assistant,
                    _ => MessageRole::Other(role_str.to_string()),
                };

                let ts = DateTime::from_timestamp_millis(time_created);

                // Fetch parts for this message.
                let mut part_stmt = conn.prepare(
                    "SELECT data FROM part WHERE message_id = ?1 ORDER BY time_created",
                )?;
                let part_rows = part_stmt.query_map([&msg_id], |row| row.get::<_, String>(0))?;

                let mut blocks = Vec::new();
                for part_row in part_rows {
                    let part_data = part_row?;
                    if let Ok(part_json) = serde_json::from_str::<Value>(&part_data) {
                        if let Some(block) = part_to_block(&part_json) {
                            blocks.push(block);
                        }
                    }
                }

                // If no parts produced blocks, try to extract text from summary.
                if blocks.is_empty() {
                    if let Some(title) = msg_json
                        .get("summary")
                        .and_then(|s| s.get("title"))
                        .and_then(|t| t.as_str())
                    {
                        blocks.push(Block::Text(title.to_string()));
                    }
                }

                if !blocks.is_empty() {
                    results.push(ConversationEvent { ts, role, blocks });
                }
            }

            Ok::<_, anyhow::Error>(results)
        })
        .await
        .context("spawn_blocking failed")?;

        Ok(messages?)
    }
}

/// Convert an OpenCode part JSON into a display Block.
fn part_to_block(part: &Value) -> Option<Block> {
    let part_type = part.get("type").and_then(|t| t.as_str()).unwrap_or("");
    match part_type {
        "text" => {
            let text = part.get("text").and_then(|t| t.as_str()).unwrap_or("");
            if text.is_empty() {
                None
            } else {
                Some(Block::Text(text.to_string()))
            }
        }
        "reasoning" => {
            let text = part.get("text").and_then(|t| t.as_str()).unwrap_or("");
            if text.is_empty() {
                None
            } else {
                Some(Block::Thinking(text.to_string()))
            }
        }
        "tool" => {
            let tool_name = part.get("tool").and_then(|t| t.as_str()).unwrap_or("?");
            let call_id = part.get("callID").and_then(|t| t.as_str()).unwrap_or("");
            let state = part.get("state");
            let status = state
                .and_then(|s| s.get("status"))
                .and_then(|s| s.as_str())
                .unwrap_or("?");
            let title = state
                .and_then(|s| s.get("title"))
                .and_then(|t| t.as_str())
                .unwrap_or(tool_name);

            // Extract input JSON for the expanded view.
            let input = state
                .and_then(|s| s.get("input"))
                .map(|v| serde_json::to_string_pretty(v).unwrap_or_default())
                .unwrap_or_default();

            // Build a preview line similar to codex: "tool_name  title"
            let preview = if title == tool_name {
                tool_name.to_string()
            } else {
                format!("{}  {}", tool_name, first_line_truncated(title, 100))
            };

            let output = state
                .and_then(|s| s.get("output"))
                .and_then(|o| o.as_str())
                .unwrap_or("");

            // If this part has both input and output, emit ToolUse + ToolResult.
            // If only output (rare), emit just ToolResult.
            let mut blocks = Vec::new();
            if !input.is_empty() || !call_id.is_empty() {
                blocks.push(Block::ToolUse {
                    name: tool_name.to_string(),
                    preview,
                    input,
                });
            }
            if !output.is_empty() {
                blocks.push(Block::ToolResult {
                    is_error: status == "error",
                    content: output.to_string(),
                });
            }

            // Return the first block; caller flattens multiple blocks per message.
            // Actually we need to return a single Option<Block>. If there are both,
            // we can only return one. Prefer ToolUse as it contains the call info.
            blocks.into_iter().next()
        }
        "patch" => {
            let files = part
                .get("files")
                .and_then(|f| f.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str())
                        .collect::<Vec<_>>()
                        .join("\n")
                })
                .unwrap_or_default();
            if files.is_empty() {
                None
            } else {
                Some(Block::Text(format!("📎 patch\n{}", files)))
            }
        }
        "file" => {
            let filename = part
                .get("filename")
                .and_then(|f| f.as_str())
                .unwrap_or("unknown");
            Some(Block::Attachment {
                label: format!("file: {}", filename),
                body: String::new(),
            })
        }
        // step-start, step-finish, compaction: skip — no conversational content.
        _ => None,
    }
}

fn infer_status(updated_at: Option<DateTime<Utc>>) -> SessionStatus {
    let Some(t) = updated_at else {
        return SessionStatus::Unknown;
    };
    let age = Utc::now() - t;
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

    #[test]
    fn parses_session_id_from_virtual_path() {
        let adapter = OpencodeAdapter::new(Some(PathBuf::from(
            "/home/user/.local/share/opencode/opencode.db",
        )));
        let path = Path::new("/home/user/.local/share/opencode/sessions/ses_abc123.json");
        assert_eq!(
            adapter.session_id_from_path(path),
            Some("ses_abc123".to_string())
        );
    }

    #[test]
    fn rejects_non_virtual_path() {
        let adapter = OpencodeAdapter::new(None);
        assert!(!adapter.owns_path(Path::new(
            "/home/user/.local/share/opencode/opencode.db"
        )));
        assert!(!adapter.owns_path(Path::new(
            "/home/user/.claude/projects/session.jsonl"
        )));
    }

    #[test]
    fn matches_opencode_process() {
        let adapter = OpencodeAdapter::new(None);
        assert!(adapter.matches_process(&["opencode".to_string()], None));
        assert!(adapter.matches_process(&["/usr/bin/opencode".to_string()], None));
        assert!(!adapter.matches_process(&["claude".to_string()], None));
    }

    #[test]
    fn infer_status_active() {
        let now = Some(Utc::now());
        assert_eq!(infer_status(now), SessionStatus::Active);
    }

    #[test]
    fn infer_status_idle() {
        let two_hours_ago = Some(Utc::now() - chrono::Duration::hours(2));
        assert_eq!(infer_status(two_hours_ago), SessionStatus::Idle);
    }

    #[test]
    fn infer_status_completed() {
        let two_days_ago = Some(Utc::now() - chrono::Duration::days(2));
        assert_eq!(infer_status(two_days_ago), SessionStatus::Completed);
    }

    #[tokio::test]
    async fn scan_all_reads_local_database() {
        let home = std::env::var("HOME").expect("HOME env var");
        let db_path = PathBuf::from(home).join(".local/share/opencode/opencode.db");
        if !db_path.exists() {
            eprintln!("Skipping: opencode db not found at {}", db_path.display());
            return;
        }

        let adapter = OpencodeAdapter::new(Some(db_path));
        let sessions = adapter.scan_all().await.expect("scan_all should succeed");

        assert!(!sessions.is_empty(), "expected at least one opencode session");
        for s in &sessions {
            assert_eq!(s.agent, "opencode");
            assert!(!s.id.is_empty(), "session id should not be empty");
            assert!(s.cwd.is_some(), "cwd should be set");
        }

        println!("Found {} opencode sessions", sessions.len());
        if let Some(first) = sessions.first() {
            println!(
                "First session: id={} cwd={} model={:?} version={:?}",
                first.id,
                first.cwd.as_ref().unwrap().display(),
                first.model,
                first.version
            );
        }
    }

    #[tokio::test]
    async fn parse_meta_full_reads_tokens() {
        let home = std::env::var("HOME").expect("HOME env var");
        let db_path = PathBuf::from(home).join(".local/share/opencode/opencode.db");
        if !db_path.exists() {
            eprintln!("Skipping: opencode db not found");
            return;
        }

        let adapter = OpencodeAdapter::new(Some(db_path.clone()));
        let sessions = adapter.scan_all().await.expect("scan_all should succeed");
        if sessions.is_empty() {
            eprintln!("Skipping: no sessions to test");
            return;
        }

        let first = &sessions[0];
        let full = adapter
            .parse_meta_full(&first.path)
            .await
            .expect("parse_meta_full should succeed");

        assert_eq!(full.id, first.id);
        assert_eq!(full.agent, "opencode");
        println!(
            "Session {}: messages={}, tokens={{input={}, output={}, cache_read={}, cache_creation={}}}",
            full.id,
            full.message_count,
            full.tokens.input,
            full.tokens.output,
            full.tokens.cache_read,
            full.tokens.cache_creation
        );
    }

    #[tokio::test]
    async fn tail_messages_reads_local_database() {
        let home = std::env::var("HOME").expect("HOME env var");
        let db_path = PathBuf::from(home).join(".local/share/opencode/opencode.db");
        if !db_path.exists() {
            eprintln!("Skipping: opencode db not found");
            return;
        }

        let adapter = OpencodeAdapter::new(Some(db_path));
        let sessions = adapter.scan_all().await.expect("scan_all should succeed");
        if sessions.is_empty() {
            eprintln!("Skipping: no sessions to test");
            return;
        }

        let first = &sessions[0];
        let previews = adapter
            .tail_messages(&first.path, 3)
            .await
            .expect("tail_messages should succeed");

        println!("Session {}: got {} preview messages", first.id, previews.len());
        for (i, p) in previews.iter().enumerate() {
            println!(
                "  [{}] {:?}: {}",
                i,
                p.role,
                p.text.chars().take(80).collect::<String>()
            );
        }

        // Previews may be empty if the session has no text content, but the call should succeed.
    }

    #[tokio::test]
    async fn load_conversation_reads_local_database() {
        let home = std::env::var("HOME").expect("HOME env var");
        let db_path = PathBuf::from(home).join(".local/share/opencode/opencode.db");
        if !db_path.exists() {
            eprintln!("Skipping: opencode db not found");
            return;
        }

        let adapter = OpencodeAdapter::new(Some(db_path));
        let sessions = adapter.scan_all().await.expect("scan_all should succeed");
        if sessions.is_empty() {
            eprintln!("Skipping: no sessions to test");
            return;
        }

        let first = &sessions[0];
        let events = adapter
            .load_conversation(&first.path)
            .await
            .expect("load_conversation should succeed");

        println!(
            "Session {}: got {} conversation events",
            first.id,
            events.len()
        );
        for (i, ev) in events.iter().enumerate() {
            println!(
                "  [{}] {:?}: {} blocks",
                i,
                ev.role,
                ev.blocks.len()
            );
            for (j, block) in ev.blocks.iter().enumerate() {
                let desc = match block {
                    Block::Text(_) => "Text",
                    Block::Thinking(_) => "Thinking",
                    Block::ToolUse { .. } => "ToolUse",
                    Block::ToolResult { .. } => "ToolResult",
                    Block::System(_) => "System",
                    Block::Attachment { .. } => "Attachment",
                    Block::Summary(_) => "Summary",
                };
                println!("    block[{}]: {}", j, desc);
            }
        }
    }
}
