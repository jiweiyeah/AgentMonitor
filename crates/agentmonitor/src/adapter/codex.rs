use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json::Value;
use tokio::fs;
use tokio::io::{AsyncBufReadExt, BufReader};

use super::conversation::{Block, ConversationEvent};
use super::types::{MessagePreview, MessageRole, SessionMeta, SessionStatus, TokenStats};
use super::AgentAdapter;

pub struct CodexAdapter {
    root: Option<PathBuf>,
}

impl CodexAdapter {
    pub fn new(root: Option<PathBuf>) -> Self {
        Self { root }
    }

    fn base_meta(&self, path: &Path, size: u64, mtime: Option<DateTime<Utc>>) -> SessionMeta {
        SessionMeta {
            agent: self.id(),
            id: String::new(),
            path: path.to_path_buf(),
            cwd: None,
            model: None,
            version: None,
            git_branch: None,
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
impl AgentAdapter for CodexAdapter {
    fn id(&self) -> &'static str {
        "codex"
    }
    fn display_name(&self) -> &'static str {
        "Codex"
    }
    fn session_roots(&self) -> Vec<PathBuf> {
        self.root.iter().cloned().collect()
    }
    fn owns_path(&self, path: &Path) -> bool {
        path.file_name()
            .and_then(|s| s.to_str())
            .map(|n| n.starts_with("rollout-") && n.ends_with(".jsonl"))
            .unwrap_or(false)
    }
    fn matches_process(&self, cmd: &[String], _exe: Option<&Path>) -> bool {
        cmd.iter().any(|s| {
            s.ends_with("/bin/codex")
                || s == "codex"
                || s.contains("/@openai/codex-")
                || s.contains("/codex-cli/")
        })
    }

    async fn parse_meta_fast(&self, path: &Path) -> Result<SessionMeta> {
        let file = fs::File::open(path)
            .await
            .with_context(|| format!("open {}", path.display()))?;
        let (size, mtime) = meta_of(&file).await;
        let mut meta = self.base_meta(path, size, mtime);
        let mut reader = BufReader::new(file).lines();
        // First line is session_meta for codex.
        if let Some(line) = reader.next_line().await? {
            if let Ok(v) = serde_json::from_str::<Value>(&line) {
                fold_codex_line(&v, &mut meta);
            }
        }
        if meta.id.is_empty() {
            if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                if let Some(uuid) = stem.rsplit('-').next() {
                    meta.id = uuid.to_string();
                }
            }
        }
        meta.status = infer_status(meta.updated_at);
        Ok(meta)
    }

    async fn parse_meta_full(&self, path: &Path) -> Result<SessionMeta> {
        let file = fs::File::open(path)
            .await
            .with_context(|| format!("open {}", path.display()))?;
        let (size, mtime) = meta_of(&file).await;
        let mut meta = self.base_meta(path, size, mtime);
        let mut reader = BufReader::new(file).lines();
        while let Some(line) = reader.next_line().await? {
            if line.trim().is_empty() {
                continue;
            }
            meta.message_count += 1;
            if let Ok(v) = serde_json::from_str::<Value>(&line) {
                fold_codex_line(&v, &mut meta);
            }
        }
        if meta.id.is_empty() {
            if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                if let Some(uuid) = stem.rsplit('-').next() {
                    meta.id = uuid.to_string();
                }
            }
        }
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
                name.starts_with("rollout-") && name.ends_with(".jsonl")
            })
            .map(|e| e.path().to_path_buf())
            .collect();

        let this = self;
        let futures = files
            .into_iter()
            .map(|p| async move { (p.clone(), this.parse_meta_fast(&p).await) });
        let results = futures::future::join_all(futures).await;
        let mut out = Vec::with_capacity(results.len());
        for (path, res) in results {
            match res {
                Ok(meta) => out.push(meta),
                Err(err) => tracing::debug!(path = %path.display(), ?err, "codex parse skipped"),
            }
        }
        Ok(out)
    }

    async fn tail_messages(&self, path: &Path, n: usize) -> Result<Vec<MessagePreview>> {
        let data = super::tail_bytes(path, 256 * 1024).await?;
        let mut previews = Vec::new();
        for line in data.lines().rev() {
            if line.trim().is_empty() {
                continue;
            }
            let Ok(v) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            let top_type = v.get("type").and_then(|t| t.as_str()).unwrap_or("");
            let (role, text) = match top_type {
                "response_item" => {
                    let payload = v.get("payload");
                    let role_str = payload
                        .and_then(|p| p.get("role"))
                        .and_then(|r| r.as_str())
                        .unwrap_or("assistant");
                    let role = match role_str {
                        "user" => MessageRole::User,
                        "assistant" => MessageRole::Assistant,
                        _ => MessageRole::Other(role_str.to_string()),
                    };
                    let text = payload
                        .and_then(|p| p.get("content"))
                        .and_then(|c| c.as_array())
                        .map(|arr| {
                            arr.iter()
                                .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
                                .collect::<Vec<_>>()
                                .join("\n")
                        })
                        .unwrap_or_default();
                    (role, text)
                }
                _ => continue,
            };
            if text.is_empty() {
                continue;
            }
            let ts = v
                .get("timestamp")
                .and_then(|t| t.as_str())
                .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
                .map(|d| d.with_timezone(&Utc));
            previews.push(MessagePreview {
                ts,
                role,
                text: truncate(text, 600),
            });
            if previews.len() >= n {
                break;
            }
        }
        previews.reverse();
        Ok(previews)
    }

    async fn load_conversation(&self, path: &Path) -> Result<Vec<ConversationEvent>> {
        let file = fs::File::open(path)
            .await
            .with_context(|| format!("open {}", path.display()))?;
        let mut reader = BufReader::new(file).lines();
        let mut events: Vec<ConversationEvent> = Vec::new();
        while let Some(line) = reader.next_line().await? {
            let l = line.trim();
            if l.is_empty() {
                continue;
            }
            let Ok(v) = serde_json::from_str::<Value>(l) else {
                continue;
            };
            if let Some(ev) = codex_event_from_line(&v) {
                events.push(ev);
            }
        }
        Ok(events)
    }
}

fn fold_codex_line(v: &Value, meta: &mut SessionMeta) {
    let top_type = v.get("type").and_then(|t| t.as_str()).unwrap_or("");
    if top_type == "session_meta" {
        let payload = v.get("payload");
        if let Some(id) = payload.and_then(|p| p.get("id")).and_then(|s| s.as_str()) {
            meta.id = id.to_string();
        }
        if let Some(cwd) = payload.and_then(|p| p.get("cwd")).and_then(|s| s.as_str()) {
            meta.cwd = Some(PathBuf::from(cwd));
        }
        if let Some(ver) = payload
            .and_then(|p| p.get("cli_version"))
            .and_then(|s| s.as_str())
        {
            meta.version = Some(ver.to_string());
        }
        if let Some(branch) = payload
            .and_then(|p| p.get("git"))
            .and_then(|g| g.get("branch"))
            .and_then(|s| s.as_str())
        {
            if !branch.is_empty() {
                meta.git_branch = Some(branch.to_string());
            }
        }
        if let Some(provider) = payload
            .and_then(|p| p.get("model_provider"))
            .and_then(|s| s.as_str())
        {
            meta.model = Some(provider.to_string());
        }
        if let Some(ts) = payload
            .and_then(|p| p.get("timestamp"))
            .and_then(|s| s.as_str())
        {
            meta.started_at = DateTime::parse_from_rfc3339(ts)
                .ok()
                .map(|d| d.with_timezone(&Utc));
        }
    }
    if let Some(ts) = v.get("timestamp").and_then(|s| s.as_str()) {
        if let Ok(dt) = DateTime::parse_from_rfc3339(ts) {
            meta.updated_at = Some(dt.with_timezone(&Utc));
        }
    }
    // Codex emits an `event_msg` with `payload.type == "token_count"` after
    // every model turn. `payload.info.total_token_usage` is *cumulative* for
    // the whole session, so we overwrite (not add) — the final line in file
    // order therefore produces the canonical total. `last_token_usage` is a
    // fallback for older log variants that only carry the per-turn delta.
    if top_type == "event_msg" {
        if let Some(payload) = v.get("payload") {
            if payload.get("type").and_then(|t| t.as_str()) == Some("token_count") {
                apply_codex_token_count(payload, meta);
            }
        }
    }
}

/// Overwrite `meta.tokens` from one `token_count` payload. See notes in
/// `fold_codex_line` on cumulative vs delta semantics. Mapping rules:
///
/// - `input_tokens - cached_input_tokens` → `tokens.input` (fresh input only)
/// - `cached_input_tokens`               → `tokens.cache_read`
/// - `output_tokens`                     → `tokens.output` (already includes
///   reasoning_output_tokens in OpenAI accounting, so we don't add it)
/// - Codex doesn't report cache creation, so that bucket stays 0.
fn apply_codex_token_count(payload: &Value, meta: &mut SessionMeta) {
    let info = match payload.get("info") {
        Some(v) if !v.is_null() => v,
        _ => return,
    };
    let usage = info
        .get("total_token_usage")
        .or_else(|| info.get("last_token_usage"));
    let Some(usage) = usage else { return };

    let input_total = usage
        .get("input_tokens")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let cached_input = usage
        .get("cached_input_tokens")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let output_total = usage
        .get("output_tokens")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);

    meta.tokens.input = input_total.saturating_sub(cached_input);
    meta.tokens.cache_read = cached_input;
    meta.tokens.output = output_total;
    meta.tokens.cache_creation = 0;
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

fn codex_event_from_line(v: &Value) -> Option<ConversationEvent> {
    let top_type = v.get("type").and_then(|t| t.as_str())?;
    if top_type != "response_item" {
        return None;
    }
    let ts = v
        .get("timestamp")
        .and_then(|t| t.as_str())
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|d| d.with_timezone(&Utc));
    let payload = v.get("payload")?;
    let ptype = payload.get("type").and_then(|t| t.as_str()).unwrap_or("");
    match ptype {
        "message" => {
            let role_str = payload
                .get("role")
                .and_then(|r| r.as_str())
                .unwrap_or("assistant");
            let role = match role_str {
                "user" => MessageRole::User,
                "assistant" => MessageRole::Assistant,
                "system" | "developer" => MessageRole::System,
                other => MessageRole::Other(other.to_string()),
            };
            let text = payload
                .get("content")
                .and_then(|c| c.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
                        .collect::<Vec<_>>()
                        .join("\n")
                })
                .unwrap_or_default();
            if text.trim().is_empty() {
                return None;
            }
            Some(ConversationEvent {
                ts,
                role,
                blocks: vec![Block::Text(text)],
            })
        }
        "function_call" => {
            let name = payload
                .get("name")
                .and_then(|t| t.as_str())
                .unwrap_or("?")
                .to_string();
            let args_str = payload
                .get("arguments")
                .and_then(|t| t.as_str())
                .unwrap_or("");
            let args_val: Value = serde_json::from_str(args_str).unwrap_or(Value::Null);
            let preview = codex_tool_preview(&name, &args_val, args_str);
            let input = if args_val.is_null() {
                args_str.to_string()
            } else {
                serde_json::to_string_pretty(&args_val).unwrap_or_else(|_| args_str.to_string())
            };
            Some(ConversationEvent {
                ts,
                role: MessageRole::Assistant,
                blocks: vec![Block::ToolUse {
                    name,
                    preview,
                    input,
                }],
            })
        }
        "function_call_output" => {
            let output = payload
                .get("output")
                .and_then(|t| t.as_str())
                .unwrap_or("")
                .to_string();
            if output.trim().is_empty() {
                return None;
            }
            Some(ConversationEvent {
                ts,
                role: MessageRole::Tool,
                blocks: vec![Block::ToolResult {
                    is_error: false,
                    content: output,
                }],
            })
        }
        "reasoning" => {
            let text = payload
                .get("summary")
                .and_then(|s| s.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
                        .collect::<Vec<_>>()
                        .join("\n")
                })
                .unwrap_or_default();
            if text.trim().is_empty() {
                return None;
            }
            Some(ConversationEvent {
                ts,
                role: MessageRole::Assistant,
                blocks: vec![Block::Thinking(text)],
            })
        }
        _ => None,
    }
}

fn codex_tool_preview(name: &str, args: &Value, raw: &str) -> String {
    const KEYS: &[&str] = &["cmd", "command", "path", "file_path", "query", "url"];
    for k in KEYS {
        if let Some(s) = args.get(*k).and_then(|v| v.as_str()) {
            return format!("{name}  {}", first_line_truncated(s, 100));
        }
    }
    let snippet = first_line_truncated(raw, 100);
    if snippet.is_empty() {
        name.to_string()
    } else {
        format!("{name}  {snippet}")
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
    use serde_json::json;

    #[test]
    fn skips_non_response_items() {
        for ty in ["session_meta", "event_msg", "turn_context"] {
            let v = json!({"type": ty});
            assert!(
                codex_event_from_line(&v).is_none(),
                "{ty} should be skipped"
            );
        }
    }

    #[test]
    fn parses_message_roles() {
        let v = json!({
            "type":"response_item",
            "payload":{
                "type":"message",
                "role":"user",
                "content":[{"type":"input_text","text":"hello"}]
            }
        });
        let ev = codex_event_from_line(&v).expect("event");
        assert!(matches!(ev.role, MessageRole::User));
        assert!(matches!(&ev.blocks[0], Block::Text(s) if s == "hello"));

        let dev = json!({
            "type":"response_item",
            "payload":{
                "type":"message",
                "role":"developer",
                "content":[{"type":"input_text","text":"instructions"}]
            }
        });
        let ev = codex_event_from_line(&dev).expect("event");
        assert!(matches!(ev.role, MessageRole::System));
    }

    #[test]
    fn parses_function_call() {
        let v = json!({
            "type":"response_item",
            "payload":{
                "type":"function_call",
                "name":"exec_command",
                "arguments": "{\"cmd\":\"ls\"}",
                "call_id":"c1"
            }
        });
        let ev = codex_event_from_line(&v).expect("event");
        match &ev.blocks[0] {
            Block::ToolUse {
                name,
                preview,
                input,
            } => {
                assert_eq!(name, "exec_command");
                assert!(preview.contains("ls"), "preview: {preview}");
                assert!(input.contains("cmd"));
            }
            other => panic!("expected ToolUse, got {other:?}"),
        }
    }

    #[test]
    fn parses_function_call_output() {
        let v = json!({
            "type":"response_item",
            "payload":{
                "type":"function_call_output",
                "call_id":"c1",
                "output":"stdout\nline2"
            }
        });
        let ev = codex_event_from_line(&v).expect("event");
        assert!(matches!(ev.role, MessageRole::Tool));
        match &ev.blocks[0] {
            Block::ToolResult { is_error, content } => {
                assert!(!*is_error);
                assert_eq!(content, "stdout\nline2");
            }
            other => panic!("expected ToolResult, got {other:?}"),
        }
    }

    #[test]
    fn parses_reasoning_summary() {
        let v = json!({
            "type":"response_item",
            "payload":{
                "type":"reasoning",
                "summary":[{"type":"summary_text","text":"step 1"}]
            }
        });
        let ev = codex_event_from_line(&v).expect("event");
        match &ev.blocks[0] {
            Block::Thinking(s) => assert_eq!(s, "step 1"),
            other => panic!("expected Thinking, got {other:?}"),
        }
    }

    fn empty_meta() -> SessionMeta {
        SessionMeta {
            agent: "codex",
            id: String::new(),
            path: std::path::PathBuf::from("/tmp/x.jsonl"),
            cwd: None,
            model: None,
            version: None,
            git_branch: None,
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
    fn codex_token_count_is_cumulative_last_wins() {
        // Three token_count events; only the last cumulative total should be
        // reflected. This proves we overwrite (not add) across the file.
        let mut meta = empty_meta();
        for (input, output, cached) in [(100u64, 10u64, 0u64), (200, 20, 5), (300, 30, 10)] {
            let v = serde_json::json!({
                "type": "event_msg",
                "payload": {
                    "type": "token_count",
                    "info": {
                        "total_token_usage": {
                            "input_tokens": input,
                            "cached_input_tokens": cached,
                            "output_tokens": output,
                            "reasoning_output_tokens": 0,
                            "total_tokens": input + output
                        }
                    }
                }
            });
            fold_codex_line(&v, &mut meta);
        }
        assert_eq!(meta.tokens.input, 290, "input = input_tokens - cached");
        assert_eq!(meta.tokens.cache_read, 10);
        assert_eq!(meta.tokens.output, 30);
        assert_eq!(meta.tokens.cache_creation, 0);
        assert_eq!(
            meta.tokens.total(),
            330,
            "Σ should equal input_tokens + output_tokens from last cumulative"
        );
    }

    #[test]
    fn codex_token_count_falls_back_to_last_token_usage() {
        let mut meta = empty_meta();
        let v = serde_json::json!({
            "type": "event_msg",
            "payload": {
                "type": "token_count",
                "info": {
                    "last_token_usage": {
                        "input_tokens": 50,
                        "cached_input_tokens": 5,
                        "output_tokens": 7
                    }
                }
            }
        });
        fold_codex_line(&v, &mut meta);
        assert_eq!(meta.tokens.input, 45);
        assert_eq!(meta.tokens.cache_read, 5);
        assert_eq!(meta.tokens.output, 7);
    }

    #[test]
    fn codex_non_token_count_event_leaves_tokens_untouched() {
        let mut meta = empty_meta();
        meta.tokens.input = 123;
        let v = serde_json::json!({
            "type": "event_msg",
            "payload": { "type": "agent_message", "payload": {} }
        });
        fold_codex_line(&v, &mut meta);
        assert_eq!(meta.tokens.input, 123, "unrelated event must not clobber");
    }

    #[test]
    fn codex_token_count_with_null_info_is_safe() {
        // Early in a session Codex emits `info: null`. Must not panic or
        // silently zero the prior value.
        let mut meta = empty_meta();
        meta.tokens.input = 50;
        let v = serde_json::json!({
            "type": "event_msg",
            "payload": { "type": "token_count", "info": null }
        });
        fold_codex_line(&v, &mut meta);
        assert_eq!(meta.tokens.input, 50);
    }
}
