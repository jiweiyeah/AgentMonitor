use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use futures::stream::StreamExt;
use serde_json::Value;
use tokio::fs;

use super::conversation::{Block, ConversationEvent};
use super::types::{MessagePreview, MessageRole, SessionMeta, SessionStatus, TokenStats};
use super::AgentAdapter;

pub struct GeminiAdapter {
    root: Option<PathBuf>,
}

impl GeminiAdapter {
    pub fn new(root: Option<PathBuf>) -> Self {
        Self { root }
    }

    fn base_meta(&self, path: &Path, size: u64, mtime: Option<DateTime<Utc>>) -> SessionMeta {
        // Gemini session files don't carry a cwd field; use the parent directory
        // (the <project_hash> folder under ~/.gemini/tmp) as a fallback so the
        // sessions list and detail pane show something useful instead of "?".
        let cwd = path
            .parent()
            .and_then(|p| p.parent())
            .map(|p| p.to_path_buf());
        SessionMeta {
            agent: self.id(),
            id: String::new(),
            path: path.to_path_buf(),
            cwd,
            model: None,
            version: None,
            git_branch: None,
            source: None,
            started_at: None,
            updated_at: mtime,
            message_count: 0,
            tokens: TokenStats::default(),
            status: SessionStatus::Unknown,
            byte_offset: size,
            size_bytes: size,
        }
    }
}

#[async_trait]
impl AgentAdapter for GeminiAdapter {
    fn id(&self) -> &'static str {
        "gemini"
    }

    fn display_name(&self) -> &'static str {
        "Gemini"
    }

    fn session_roots(&self) -> Vec<PathBuf> {
        self.root.iter().cloned().collect()
    }

    fn owns_path(&self, path: &Path) -> bool {
        path.file_name()
            .and_then(|s| s.to_str())
            .map(|n| n.starts_with("session-") && n.ends_with(".json"))
            .unwrap_or(false)
    }

    fn matches_process(&self, cmd: &[String], _exe: Option<&Path>) -> bool {
        cmd.iter().any(|s| {
            s.ends_with("/gemini")
                || s == "gemini"
                || s.contains("/@google/gemini-cli")
        })
    }

    async fn parse_meta_fast(&self, path: &Path) -> Result<SessionMeta> {
        let file = fs::File::open(path)
            .await
            .with_context(|| format!("open {}", path.display()))?;
        let (size, mtime) = meta_of(&file).await;
        let mut meta = self.base_meta(path, size, mtime);

        let content = fs::read_to_string(path)
            .await
            .with_context(|| format!("read {}", path.display()))?;
        let v: Value = serde_json::from_str(&content)
            .with_context(|| format!("parse JSON {}", path.display()))?;

        fold_gemini_session(&v, &mut meta, false);
        meta.status = infer_status(meta.updated_at);
        Ok(meta)
    }

    async fn parse_meta_full(&self, path: &Path) -> Result<SessionMeta> {
        let file = fs::File::open(path)
            .await
            .with_context(|| format!("open {}", path.display()))?;
        let (size, mtime) = meta_of(&file).await;
        let mut meta = self.base_meta(path, size, mtime);

        let content = fs::read_to_string(path)
            .await
            .with_context(|| format!("read {}", path.display()))?;
        let v: Value = serde_json::from_str(&content)
            .with_context(|| format!("parse JSON {}", path.display()))?;

        fold_gemini_session(&v, &mut meta, true);
        meta.status = infer_status(meta.updated_at);
        Ok(meta)
    }

    async fn scan_all(&self) -> Result<Vec<SessionMeta>> {
        let Some(root) = &self.root else {
            return Ok(Vec::new());
        };
        if !root.exists() {
            return Ok(Vec::new());
        }

        let files: Vec<PathBuf> = walkdir::WalkDir::new(root)
            .max_depth(5)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_file())
            .filter(|e| {
                let name = e.file_name().to_string_lossy();
                name.starts_with("session-") && name.ends_with(".json")
            })
            .map(|e| e.path().to_path_buf())
            .collect();

        let this = self;
        let results: Vec<(PathBuf, Result<SessionMeta>)> = futures::stream::iter(files)
            .map(|p| async move { (p.clone(), this.parse_meta_fast(&p).await) })
            .buffer_unordered(16)
            .collect()
            .await;

        let mut out = Vec::with_capacity(results.len());
        for (path, res) in results {
            match res {
                Ok(meta) => out.push(meta),
                Err(err) => tracing::debug!(path = %path.display(), ?err, "gemini parse skipped"),
            }
        }
        Ok(out)
    }

    async fn tail_messages(&self, path: &Path, n: usize) -> Result<Vec<MessagePreview>> {
        let content = fs::read_to_string(path)
            .await
            .with_context(|| format!("read {}", path.display()))?;
        let v: Value = serde_json::from_str(&content)
            .with_context(|| format!("parse JSON {}", path.display()))?;

        let messages = v.get("messages").and_then(|m| m.as_array());
        let Some(messages) = messages else {
            return Ok(Vec::new());
        };

        let mut previews = Vec::new();
        for msg in messages.iter().rev() {
            let msg_type = msg.get("type").and_then(|t| t.as_str()).unwrap_or("");
            let (role, text) = match msg_type {
                "user" => {
                    let text = msg.get("content").and_then(|c| c.as_str()).unwrap_or("");
                    (MessageRole::User, text)
                }
                "gemini" => {
                    let text = msg.get("content").and_then(|c| c.as_str()).unwrap_or("");
                    (MessageRole::Assistant, text)
                }
                _ => continue,
            };

            if text.trim().is_empty() {
                continue;
            }

            let ts = msg
                .get("timestamp")
                .and_then(|t| t.as_str())
                .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
                .map(|d| d.with_timezone(&Utc));

            previews.push(MessagePreview {
                ts,
                role,
                text: truncate(text.to_string(), 600),
            });

            if previews.len() >= n {
                break;
            }
        }

        previews.reverse();
        Ok(previews)
    }

    async fn load_conversation(&self, path: &Path) -> Result<Vec<ConversationEvent>> {
        let content = fs::read_to_string(path)
            .await
            .with_context(|| format!("read {}", path.display()))?;
        let v: Value = serde_json::from_str(&content)
            .with_context(|| format!("parse JSON {}", path.display()))?;

        let messages = v.get("messages").and_then(|m| m.as_array());
        let Some(messages) = messages else {
            return Ok(Vec::new());
        };

        let mut events = Vec::new();
        for msg in messages {
            let msg_type = msg.get("type").and_then(|t| t.as_str()).unwrap_or("");
            let ts = msg
                .get("timestamp")
                .and_then(|t| t.as_str())
                .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
                .map(|d| d.with_timezone(&Utc));

            let (role, blocks) = match msg_type {
                "user" => {
                    let text = msg.get("content").and_then(|c| c.as_str()).unwrap_or("");
                    (MessageRole::User, vec![Block::Text(text.to_string())])
                }
                "gemini" => {
                    let mut blocks = Vec::new();

                    // Main response text
                    if let Some(text) = msg.get("content").and_then(|c| c.as_str()) {
                        if !text.trim().is_empty() {
                            blocks.push(Block::Text(text.to_string()));
                        }
                    }

                    // Thinking blocks
                    if let Some(thoughts) = msg.get("thoughts").and_then(|t| t.as_array()) {
                        for thought in thoughts {
                            let subject = thought.get("subject").and_then(|s| s.as_str()).unwrap_or("");
                            let desc = thought.get("description").and_then(|s| s.as_str()).unwrap_or("");
                            let mut text = String::new();
                            if !subject.is_empty() {
                                text.push_str(subject);
                            }
                            if !desc.is_empty() {
                                if !text.is_empty() {
                                    text.push_str(": ");
                                }
                                text.push_str(desc);
                            }
                            if !text.trim().is_empty() {
                                blocks.push(Block::Thinking(text));
                            }
                        }
                    }

                    // Tool calls
                    if let Some(tool_calls) = msg.get("toolCalls").and_then(|t| t.as_array()) {
                        for tc in tool_calls {
                            let name = tc.get("name").and_then(|n| n.as_str()).unwrap_or("?").to_string();
                            let args = tc.get("args").cloned().unwrap_or(Value::Null);
                            let preview = tc
                                .get("displayName")
                                .and_then(|d| d.as_str())
                                .map(|s| s.to_string())
                                .unwrap_or_else(|| name.clone());
                            let input_str = serde_json::to_string_pretty(&args).unwrap_or_default();
                            blocks.push(Block::ToolUse {
                                name,
                                preview,
                                input: input_str,
                            });

                            let status = tc.get("status").and_then(|s| s.as_str()).unwrap_or("");
                            let is_error = status == "error";
                            let result_content = tc
                                .get("resultDisplay")
                                .and_then(|r| r.as_str())
                                .unwrap_or("")
                                .to_string();
                            blocks.push(Block::ToolResult {
                                is_error,
                                content: result_content,
                            });
                        }
                    }

                    (MessageRole::Assistant, blocks)
                }
                "info" => {
                    let text = msg.get("content").and_then(|c| c.as_str()).unwrap_or("");
                    (MessageRole::System, vec![Block::System(text.to_string())])
                }
                "error" => {
                    let text = msg.get("content").and_then(|c| c.as_str()).unwrap_or("");
                    (MessageRole::Other("error".to_string()), vec![Block::Text(text.to_string())])
                }
                _ => continue,
            };

            if blocks.iter().any(|b| b.has_content()) {
                events.push(ConversationEvent { ts, role, blocks });
            }
        }

        Ok(events)
    }
}

fn fold_gemini_session(v: &Value, meta: &mut SessionMeta, full_tokens: bool) {
    if let Some(id) = v.get("sessionId").and_then(|s| s.as_str()) {
        meta.id = id.to_string();
    }

    if let Some(ts) = v.get("startTime").and_then(|s| s.as_str()) {
        if let Ok(dt) = DateTime::parse_from_rfc3339(ts) {
            meta.started_at = Some(dt.with_timezone(&Utc));
        }
    }

    if let Some(ts) = v.get("lastUpdated").and_then(|s| s.as_str()) {
        if let Ok(dt) = DateTime::parse_from_rfc3339(ts) {
            let new = dt.with_timezone(&Utc);
            meta.updated_at = Some(match meta.updated_at {
                Some(prev) if prev >= new => prev,
                _ => new,
            });
        }
    }

    if let Some(messages) = v.get("messages").and_then(|m| m.as_array()) {
        meta.message_count = messages.len();

        if full_tokens {
            for msg in messages {
                if msg.get("type").and_then(|t| t.as_str()) == Some("gemini") {
                    if let Some(tokens) = msg.get("tokens") {
                        apply_gemini_tokens(tokens, meta);
                    }
                    if let Some(model) = msg.get("model").and_then(|m| m.as_str()) {
                        meta.model = Some(model.to_string());
                    }
                }
            }
        }
    }

    // Fallback: extract id from filename if sessionId is missing
    if meta.id.is_empty() {
        if let Some(stem) = meta.path.file_stem().and_then(|s| s.to_str()) {
            // session-YYYY-MM-DDTHH-MM-<session_id>
            if let Some(id) = stem.rsplit('-').next() {
                meta.id = id.to_string();
            }
        }
    }
}

fn apply_gemini_tokens(tokens: &Value, meta: &mut SessionMeta) {
    let input = tokens.get("input").and_then(|v| v.as_u64()).unwrap_or(0);
    let output = tokens.get("output").and_then(|v| v.as_u64()).unwrap_or(0);
    let cached = tokens.get("cached").and_then(|v| v.as_u64()).unwrap_or(0);
    let thoughts = tokens.get("thoughts").and_then(|v| v.as_u64()).unwrap_or(0);
    let tool = tokens.get("tool").and_then(|v| v.as_u64()).unwrap_or(0);

    meta.tokens.input += input;
    meta.tokens.output += output + thoughts + tool;
    meta.tokens.cache_read += cached;
    // Gemini doesn't expose cache_creation; bucket stays 0
}

async fn meta_of(file: &fs::File) -> (u64, Option<DateTime<Utc>>) {
    let m = file.metadata().await.ok();
    let size = m.as_ref().map(|m| m.len()).unwrap_or(0);
    let mtime = m.and_then(|m| m.modified().ok()).map(DateTime::<Utc>::from);
    (size, mtime)
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn test_path() -> PathBuf {
        PathBuf::from("/tmp/session-2026-01-01T00-00-testid.json")
    }

    fn base_meta_for_test() -> SessionMeta {
        SessionMeta {
            agent: "gemini",
            id: String::new(),
            path: test_path(),
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

    #[test]
    fn test_fold_gemini_session_extracts_basic_fields() {
        let v = json!({
            "sessionId": "abc-123",
            "startTime": "2026-01-01T00:00:00Z",
            "lastUpdated": "2026-01-01T01:00:00Z",
            "messages": [
                {"id": "1", "timestamp": "2026-01-01T00:00:00Z", "type": "user", "content": "hi"},
                {"id": "2", "timestamp": "2026-01-01T00:01:00Z", "type": "gemini", "content": "hello", "model": "gemini-3-pro", "tokens": {"input": 10, "output": 5, "cached": 2, "thoughts": 1, "tool": 0}}
            ]
        });

        let mut meta = base_meta_for_test();
        fold_gemini_session(&v, &mut meta, true);

        assert_eq!(meta.id, "abc-123");
        assert_eq!(meta.message_count, 2);
        assert_eq!(meta.model, Some("gemini-3-pro".to_string()));
        assert_eq!(meta.tokens.input, 10);
        assert_eq!(meta.tokens.output, 6); // 5 + 1
        assert_eq!(meta.tokens.cache_read, 2);
    }

    #[test]
    fn test_fold_gemini_session_fast_skips_tokens() {
        let v = json!({
            "sessionId": "xyz-789",
            "startTime": "2026-01-01T00:00:00Z",
            "lastUpdated": "2026-01-01T01:00:00Z",
            "messages": [
                {"id": "1", "type": "gemini", "tokens": {"input": 100, "output": 50, "cached": 10, "thoughts": 5, "tool": 2}}
            ]
        });

        let mut meta = base_meta_for_test();
        fold_gemini_session(&v, &mut meta, false);

        assert_eq!(meta.id, "xyz-789");
        assert_eq!(meta.message_count, 1);
        assert_eq!(meta.tokens.input, 0);
        assert_eq!(meta.tokens.output, 0);
    }

    #[test]
    fn test_fold_gemini_session_accumulates_tokens() {
        let v = json!({
            "sessionId": "multi",
            "messages": [
                {"type": "gemini", "tokens": {"input": 10, "output": 5, "cached": 1, "thoughts": 0, "tool": 0}},
                {"type": "gemini", "tokens": {"input": 20, "output": 8, "cached": 2, "thoughts": 1, "tool": 1}}
            ]
        });

        let mut meta = base_meta_for_test();
        fold_gemini_session(&v, &mut meta, true);

        assert_eq!(meta.tokens.input, 30);
        assert_eq!(meta.tokens.output, 15); // 5 + 8 + 1 + 1
        assert_eq!(meta.tokens.cache_read, 3);
    }

    #[test]
    fn test_fold_gemini_session_fallback_id_from_filename() {
        let v = json!({
            "messages": []
        });

        let mut meta = base_meta_for_test();
        fold_gemini_session(&v, &mut meta, false);

        assert_eq!(meta.id, "testid");
    }

    #[test]
    fn test_owns_path() {
        let adapter = GeminiAdapter::new(None);
        assert!(adapter.owns_path(Path::new("session-2026-01-01T00-00-abc.json")));
        assert!(!adapter.owns_path(Path::new("session-2026-01-01T00-00-abc.jsonl")));
        assert!(!adapter.owns_path(Path::new("other.json")));
        assert!(!adapter.owns_path(Path::new("session-abc.txt")));
    }

    #[test]
    fn test_matches_process() {
        let adapter = GeminiAdapter::new(None);
        assert!(adapter.matches_process(
            &["/usr/local/bin/gemini".to_string()],
            None
        ));
        assert!(adapter.matches_process(
            &["gemini".to_string()],
            None
        ));
        assert!(adapter.matches_process(
            &["/node_modules/@google/gemini-cli/dist/index.js".to_string()],
            None
        ));
        assert!(!adapter.matches_process(
            &["node".to_string(), "server.js".to_string()],
            None
        ));
    }

    #[test]
    fn test_infer_status_active() {
        let t = Utc::now() - chrono::Duration::seconds(30);
        assert_eq!(infer_status(Some(t)), SessionStatus::Active);
    }

    #[test]
    fn test_infer_status_idle() {
        let t = Utc::now() - chrono::Duration::minutes(30);
        assert_eq!(infer_status(Some(t)), SessionStatus::Idle);
    }

    #[test]
    fn test_infer_status_completed() {
        let t = Utc::now() - chrono::Duration::days(2);
        assert_eq!(infer_status(Some(t)), SessionStatus::Completed);
    }

    #[test]
    fn test_gemini_message_to_preview() {
        let v = json!({
            "messages": [
                {"id": "1", "timestamp": "2026-01-01T00:00:00Z", "type": "user", "content": "Hello"},
                {"id": "2", "timestamp": "2026-01-01T00:01:00Z", "type": "gemini", "content": "Hi there", "model": "gemini-3-pro"},
                {"id": "3", "timestamp": "2026-01-01T00:02:00Z", "type": "info", "content": "System message"},
                {"id": "4", "timestamp": "2026-01-01T00:03:00Z", "type": "gemini", "content": "Final response", "model": "gemini-3-pro"}
            ]
        });

        let mut meta = base_meta_for_test();
        fold_gemini_session(&v, &mut meta, false);

        // Simulate tail_messages logic manually
        let messages = v.get("messages").and_then(|m| m.as_array()).unwrap();
        let mut previews = Vec::new();
        for msg in messages.iter().rev() {
            let msg_type = msg.get("type").and_then(|t| t.as_str()).unwrap_or("");
            let (role, text) = match msg_type {
                "user" => {
                    let text = msg.get("content").and_then(|c| c.as_str()).unwrap_or("");
                    (MessageRole::User, text)
                }
                "gemini" => {
                    let text = msg.get("content").and_then(|c| c.as_str()).unwrap_or("");
                    (MessageRole::Assistant, text)
                }
                _ => continue,
            };
            if text.trim().is_empty() {
                continue;
            }
            let ts = msg
                .get("timestamp")
                .and_then(|t| t.as_str())
                .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
                .map(|d| d.with_timezone(&Utc));
            previews.push(MessagePreview {
                ts,
                role,
                text: text.to_string(),
            });
            if previews.len() >= 2 {
                break;
            }
        }
        previews.reverse();

        assert_eq!(previews.len(), 2);
        assert_eq!(previews[0].role, MessageRole::Assistant);
        assert_eq!(previews[0].text, "Hi there");
        assert_eq!(previews[1].role, MessageRole::Assistant);
        assert_eq!(previews[1].text, "Final response");
    }

    #[test]
    fn test_gemini_load_conversation() {
        let v = json!({
            "messages": [
                {"id": "1", "timestamp": "2026-01-01T00:00:00Z", "type": "user", "content": "Hello"},
                {"id": "2", "timestamp": "2026-01-01T00:01:00Z", "type": "gemini", "content": "Hi there", "model": "gemini-3-pro"},
                {"id": "3", "timestamp": "2026-01-01T00:02:00Z", "type": "info", "content": "System update"},
                {"id": "4", "timestamp": "2026-01-01T00:03:00Z", "type": "error", "content": "Something failed"}
            ]
        });

        let messages = v.get("messages").and_then(|m| m.as_array()).unwrap();
        let mut events = Vec::new();

        for msg in messages {
            let msg_type = msg.get("type").and_then(|t| t.as_str()).unwrap_or("");
            let ts = msg
                .get("timestamp")
                .and_then(|t| t.as_str())
                .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
                .map(|d| d.with_timezone(&Utc));

            let (role, blocks) = match msg_type {
                "user" => {
                    let text = msg.get("content").and_then(|c| c.as_str()).unwrap_or("");
                    (MessageRole::User, vec![Block::Text(text.to_string())])
                }
                "gemini" => {
                    let mut blocks = Vec::new();
                    if let Some(text) = msg.get("content").and_then(|c| c.as_str()) {
                        if !text.trim().is_empty() {
                            blocks.push(Block::Text(text.to_string()));
                        }
                    }
                    (MessageRole::Assistant, blocks)
                }
                "info" => {
                    let text = msg.get("content").and_then(|c| c.as_str()).unwrap_or("");
                    (MessageRole::System, vec![Block::System(text.to_string())])
                }
                "error" => {
                    let text = msg.get("content").and_then(|c| c.as_str()).unwrap_or("");
                    (MessageRole::Other("error".to_string()), vec![Block::Text(text.to_string())])
                }
                _ => continue,
            };

            if blocks.iter().any(|b| b.has_content()) {
                events.push(ConversationEvent { ts, role, blocks });
            }
        }

        assert_eq!(events.len(), 4);
        assert_eq!(events[0].role, MessageRole::User);
        assert_eq!(events[1].role, MessageRole::Assistant);
        assert_eq!(events[2].role, MessageRole::System);
        assert_eq!(events[3].role, MessageRole::Other("error".to_string()));
    }
}
