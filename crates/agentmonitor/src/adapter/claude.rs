use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::{Context, Result};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json::Value;
use tokio::fs;
use tokio::io::{AsyncBufReadExt, BufReader};

use super::conversation::{Block, ConversationEvent};
use super::types::{MessagePreview, MessageRole, SessionMeta, SessionStatus, TokenStats};
use super::AgentAdapter;

const HEADER_LINE_SCAN: usize = 8;

pub struct ClaudeAdapter {
    root: Option<PathBuf>,
}

impl ClaudeAdapter {
    pub fn new(root: Option<PathBuf>) -> Self {
        Self { root }
    }

    fn base_meta(&self, path: &Path, size: u64, mtime: Option<DateTime<Utc>>) -> SessionMeta {
        SessionMeta {
            agent: self.id(),
            id: path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_string(),
            path: path.to_path_buf(),
            cwd: decode_cwd_from_parent(path),
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
impl AgentAdapter for ClaudeAdapter {
    fn id(&self) -> &'static str {
        "claude"
    }
    fn display_name(&self) -> &'static str {
        "ClaudeCode"
    }
    fn session_roots(&self) -> Vec<PathBuf> {
        self.root.iter().cloned().collect()
    }
    fn matches_process(&self, cmd: &[String], _exe: Option<&Path>) -> bool {
        cmd.iter()
            .any(|s| s.ends_with("/bin/claude") || s == "claude" || s.contains("/.claude/local/"))
    }

    async fn parse_meta_fast(&self, path: &Path) -> Result<SessionMeta> {
        let file = fs::File::open(path)
            .await
            .with_context(|| format!("open {}", path.display()))?;
        let (size, mtime) = meta_of(&file).await;
        let mut meta = self.base_meta(path, size, mtime);
        let mut reader = BufReader::new(file).lines();
        let mut read = 0usize;
        let mut first_type: Option<String> = None;
        while let Some(line) = reader.next_line().await? {
            if line.trim().is_empty() {
                continue;
            }
            if let Ok(v) = serde_json::from_str::<Value>(&line) {
                if first_type.is_none() {
                    first_type = v
                        .get("type")
                        .and_then(|t| t.as_str())
                        .map(|s| s.to_string());
                }
                fold_claude_line(&v, &mut meta);
            }
            read += 1;
            if read >= HEADER_LINE_SCAN && meta.cwd.is_some() && !meta.id.is_empty() {
                break;
            }
        }
        if !is_native_first_type(first_type.as_deref()) {
            anyhow::bail!("not a Claude Code conversation: {}", path.display());
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
        let mut first_type: Option<String> = None;
        while let Some(line) = reader.next_line().await? {
            if line.trim().is_empty() {
                continue;
            }
            meta.message_count += 1;
            if let Ok(v) = serde_json::from_str::<Value>(&line) {
                if first_type.is_none() {
                    first_type = v
                        .get("type")
                        .and_then(|t| t.as_str())
                        .map(|s| s.to_string());
                }
                fold_claude_line(&v, &mut meta);
            }
        }
        if !is_native_first_type(first_type.as_deref()) {
            anyhow::bail!("not a Claude Code conversation: {}", path.display());
        }
        if meta.message_count > 0 && meta.started_at.is_none() {
            meta.started_at = meta.updated_at;
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
            .min_depth(2)
            .max_depth(2)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_file())
            .filter(|e| e.path().extension().and_then(|s| s.to_str()) == Some("jsonl"))
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
                Err(err) => tracing::debug!(path = %path.display(), ?err, "claude parse skipped"),
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
            let Some(role_str) = v.get("type").and_then(|t| t.as_str()).or_else(|| {
                v.get("message")
                    .and_then(|m| m.get("role"))
                    .and_then(|r| r.as_str())
            }) else {
                continue;
            };
            let role = match role_str {
                "user" => MessageRole::User,
                "assistant" => MessageRole::Assistant,
                "system" => MessageRole::System,
                _ => continue,
            };
            let text = extract_text(&v).unwrap_or_default();
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
            if let Some(ev) = claude_event_from_line(&v) {
                events.push(ev);
            }
        }
        Ok(events)
    }
}

fn fold_claude_line(v: &Value, meta: &mut SessionMeta) {
    if let Some(id) = v.get("sessionId").and_then(|s| s.as_str()) {
        if meta.id.is_empty() {
            meta.id = id.to_string();
        }
    }
    if meta.cwd.is_none() {
        if let Some(cwd) = v.get("cwd").and_then(|s| s.as_str()) {
            meta.cwd = Some(PathBuf::from(cwd));
        }
    }
    if meta.version.is_none() {
        if let Some(ver) = v.get("version").and_then(|s| s.as_str()) {
            meta.version = Some(ver.to_string());
        }
    }
    if meta.git_branch.is_none() {
        if let Some(br) = v.get("gitBranch").and_then(|s| s.as_str()) {
            if br != "HEAD" && !br.is_empty() {
                meta.git_branch = Some(br.to_string());
            }
        }
    }
    if meta.started_at.is_none() {
        if let Some(ts) = v.get("timestamp").and_then(|s| s.as_str()) {
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
    if let Some(msg) = v.get("message") {
        if let Some(model) = msg.get("model").and_then(|m| m.as_str()) {
            meta.model = Some(model.to_string());
        }
        if let Some(usage) = msg.get("usage") {
            if let Some(n) = usage.get("input_tokens").and_then(|u| u.as_u64()) {
                meta.tokens.input += n;
            }
            if let Some(n) = usage.get("output_tokens").and_then(|u| u.as_u64()) {
                meta.tokens.output += n;
            }
            if let Some(n) = usage
                .get("cache_read_input_tokens")
                .and_then(|u| u.as_u64())
            {
                meta.tokens.cache_read += n;
            }
            if let Some(n) = usage
                .get("cache_creation_input_tokens")
                .and_then(|u| u.as_u64())
            {
                meta.tokens.cache_creation += n;
            }
        }
    }
}

fn extract_text(v: &Value) -> Option<String> {
    if let Some(s) = v
        .get("message")
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_str())
    {
        return Some(s.to_string());
    }
    if let Some(arr) = v
        .get("message")
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_array())
    {
        let mut buf = String::new();
        for block in arr {
            if let Some(t) = block.get("text").and_then(|t| t.as_str()) {
                buf.push_str(t);
                buf.push('\n');
            }
        }
        if !buf.is_empty() {
            return Some(buf.trim().to_string());
        }
    }
    None
}

fn decode_cwd_from_parent(path: &Path) -> Option<PathBuf> {
    let parent = path.parent()?.file_name()?.to_str()?;
    if !parent.starts_with('-') {
        return None;
    }
    // Claude Code encodes both `/` and `.` as `-` in the project directory name,
    // so `/Users/yjw/.claude/mem` becomes `-Users-yjw--claude-mem`. Heuristic:
    // consecutive `--` restore to `/.` (hidden directories), remaining `-` to `/`.
    let decoded = parent.replace("--", "/.").replace('-', "/");
    Some(PathBuf::from(decoded))
}

fn is_native_first_type(t: Option<&str>) -> bool {
    // Real Claude Code sessions begin with one of these record types. Plugin
    // data files that happen to live under `~/.claude/projects/` (e.g. the
    // `queue-operation` rows claude-mem writes for its observer queue) open
    // with a non-conversation type and should be skipped.
    matches!(
        t.unwrap_or(""),
        "summary" | "user" | "assistant" | "system" | "file-history-snapshot"
    )
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

fn claude_event_from_line(v: &Value) -> Option<ConversationEvent> {
    let t = v.get("type").and_then(|s| s.as_str())?;
    let ts = v
        .get("timestamp")
        .and_then(|t| t.as_str())
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|d| d.with_timezone(&Utc));
    match t {
        // Internal bookkeeping; not part of the readable transcript.
        "file-history-snapshot" | "last-prompt" => None,
        "summary" => {
            let s = v
                .get("summary")
                .and_then(|s| s.as_str())
                .unwrap_or("")
                .trim()
                .to_string();
            if s.is_empty() {
                return None;
            }
            Some(ConversationEvent {
                ts,
                role: MessageRole::System,
                blocks: vec![Block::Summary(s)],
            })
        }
        "attachment" => {
            let att = v.get("attachment")?;
            let kind = att
                .get("type")
                .and_then(|s| s.as_str())
                .unwrap_or("hook")
                .to_string();
            let event = att.get("hookEvent").and_then(|s| s.as_str()).unwrap_or("");
            let hook = att.get("hookName").and_then(|s| s.as_str()).unwrap_or("");
            let content = att.get("content").and_then(|s| s.as_str()).unwrap_or("");
            let stdout = att.get("stdout").and_then(|s| s.as_str()).unwrap_or("");
            let stderr = att.get("stderr").and_then(|s| s.as_str()).unwrap_or("");
            let mut body = String::new();
            if !content.is_empty() {
                body.push_str(content);
            }
            if !stdout.is_empty() {
                if !body.is_empty() {
                    body.push('\n');
                }
                body.push_str(stdout);
            }
            if !stderr.is_empty() {
                if !body.is_empty() {
                    body.push_str("\n--- stderr ---\n");
                }
                body.push_str(stderr);
            }
            let label = match (event.is_empty(), hook.is_empty()) {
                (true, true) => kind,
                (true, false) => format!("{kind} · {hook}"),
                (false, true) => format!("{kind} · {event}"),
                (false, false) => format!("{kind} · {event}:{hook}"),
            };
            Some(ConversationEvent {
                ts,
                role: MessageRole::System,
                blocks: vec![Block::Attachment { label, body }],
            })
        }
        "system" => {
            let content = v
                .get("content")
                .and_then(|s| s.as_str())
                .unwrap_or("")
                .trim()
                .to_string();
            if content.is_empty() {
                return None;
            }
            let subtype = v
                .get("subtype")
                .and_then(|s| s.as_str())
                .unwrap_or("system");
            Some(ConversationEvent {
                ts,
                role: MessageRole::System,
                blocks: vec![Block::Attachment {
                    label: format!("system · {subtype}"),
                    body: content,
                }],
            })
        }
        "user" | "assistant" => {
            let role = if t == "user" {
                MessageRole::User
            } else {
                MessageRole::Assistant
            };
            let message = v.get("message")?;
            let blocks = parse_claude_blocks(message);
            if blocks.is_empty() {
                return None;
            }
            Some(ConversationEvent { ts, role, blocks })
        }
        _ => None,
    }
}

fn parse_claude_blocks(message: &Value) -> Vec<Block> {
    let Some(content) = message.get("content") else {
        return vec![];
    };
    if let Some(s) = content.as_str() {
        if s.trim().is_empty() {
            return vec![];
        }
        return vec![Block::Text(s.to_string())];
    }
    let Some(arr) = content.as_array() else {
        return vec![];
    };
    let mut blocks = Vec::with_capacity(arr.len());
    for b in arr {
        let Some(bt) = b.get("type").and_then(|t| t.as_str()) else {
            continue;
        };
        match bt {
            "text" => {
                let s = b
                    .get("text")
                    .and_then(|t| t.as_str())
                    .unwrap_or("")
                    .to_string();
                if !s.trim().is_empty() {
                    blocks.push(Block::Text(s));
                }
            }
            "thinking" => {
                let s = b
                    .get("thinking")
                    .and_then(|t| t.as_str())
                    .unwrap_or("")
                    .to_string();
                if !s.trim().is_empty() {
                    blocks.push(Block::Thinking(s));
                }
            }
            "tool_use" => {
                let name = b
                    .get("name")
                    .and_then(|t| t.as_str())
                    .unwrap_or("?")
                    .to_string();
                let input = b.get("input").cloned().unwrap_or(Value::Null);
                let preview = tool_use_preview(&name, &input);
                let input_str = serde_json::to_string_pretty(&input).unwrap_or_default();
                blocks.push(Block::ToolUse {
                    name,
                    preview,
                    input: input_str,
                });
            }
            "tool_result" => {
                let is_error = b.get("is_error").and_then(|e| e.as_bool()).unwrap_or(false);
                let content = extract_tool_result_content(b.get("content"));
                blocks.push(Block::ToolResult { is_error, content });
            }
            "image" => blocks.push(Block::Text("[image]".to_string())),
            _ => {}
        }
    }
    blocks
}

fn tool_use_preview(name: &str, input: &Value) -> String {
    const KEYS: &[&str] = &[
        "file_path",
        "path",
        "command",
        "pattern",
        "url",
        "query",
        "description",
        "notebook_path",
        "prompt",
    ];
    for k in KEYS {
        if let Some(s) = input.get(*k).and_then(|v| v.as_str()) {
            let s = first_line_truncated(s, 100);
            if !s.is_empty() {
                return format!("{name}  {s}");
            }
        }
    }
    name.to_string()
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

fn extract_tool_result_content(v: Option<&Value>) -> String {
    let Some(v) = v else {
        return String::new();
    };
    if let Some(s) = v.as_str() {
        return s.to_string();
    }
    if let Some(arr) = v.as_array() {
        let mut buf = String::new();
        for b in arr {
            if let Some(s) = b.get("text").and_then(|t| t.as_str()) {
                if !buf.is_empty() {
                    buf.push('\n');
                }
                buf.push_str(s);
            }
        }
        return buf;
    }
    serde_json::to_string(v).unwrap_or_default()
}

// SystemTime helper no longer needed; conversion inlined in meta_of.
#[allow(dead_code)]
fn system_time_to_utc(t: SystemTime) -> DateTime<Utc> {
    DateTime::<Utc>::from(t)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn skips_snapshot_and_last_prompt() {
        for ty in ["file-history-snapshot", "last-prompt"] {
            let v = json!({"type": ty});
            assert!(
                claude_event_from_line(&v).is_none(),
                "{ty} should be skipped"
            );
        }
    }

    #[test]
    fn parses_summary() {
        let v = json!({"type": "summary", "summary": "Recap of prior conversation"});
        let ev = claude_event_from_line(&v).expect("event");
        assert!(matches!(ev.role, MessageRole::System));
        assert_eq!(ev.blocks.len(), 1);
        match &ev.blocks[0] {
            Block::Summary(s) => assert_eq!(s, "Recap of prior conversation"),
            other => panic!("expected Summary, got {other:?}"),
        }
    }

    #[test]
    fn parses_attachment_hook() {
        let v = json!({
            "type": "attachment",
            "attachment": {
                "type": "hook_success",
                "hookName": "SessionStart:clear",
                "hookEvent": "SessionStart",
                "content": "",
                "stdout": "ok",
                "stderr": "",
            }
        });
        let ev = claude_event_from_line(&v).expect("event");
        match &ev.blocks[0] {
            Block::Attachment { label, body } => {
                assert!(label.contains("SessionStart"), "label: {label}");
                assert!(label.contains("hook_success"), "label: {label}");
                assert_eq!(body, "ok");
            }
            other => panic!("expected Attachment, got {other:?}"),
        }
    }

    #[test]
    fn parses_system_subtype() {
        let v = json!({
            "type": "system",
            "subtype": "local_command",
            "content": "<local-command-stdout></local-command-stdout>"
        });
        let ev = claude_event_from_line(&v).expect("event");
        match &ev.blocks[0] {
            Block::Attachment { label, .. } => {
                assert!(label.contains("local_command"), "label: {label}");
            }
            other => panic!("expected Attachment, got {other:?}"),
        }
    }

    #[test]
    fn parses_user_string_content() {
        let v = json!({
            "type":"user",
            "message": {"role":"user","content":"hello"}
        });
        let ev = claude_event_from_line(&v).expect("event");
        assert!(matches!(ev.role, MessageRole::User));
        match &ev.blocks[0] {
            Block::Text(s) => assert_eq!(s, "hello"),
            other => panic!("expected Text, got {other:?}"),
        }
    }

    #[test]
    fn parses_assistant_blocks() {
        let v = json!({
            "type":"assistant",
            "message": {
                "role":"assistant",
                "content":[
                    {"type":"thinking","thinking":"reasoning"},
                    {"type":"text","text":"answer"},
                    {"type":"tool_use","id":"t1","name":"Read","input":{"file_path":"/a/b.rs"}}
                ]
            }
        });
        let ev = claude_event_from_line(&v).expect("event");
        assert!(matches!(ev.role, MessageRole::Assistant));
        assert_eq!(ev.blocks.len(), 3);
        assert!(matches!(&ev.blocks[0], Block::Thinking(s) if s == "reasoning"));
        assert!(matches!(&ev.blocks[1], Block::Text(s) if s == "answer"));
        match &ev.blocks[2] {
            Block::ToolUse {
                name,
                preview,
                input,
            } => {
                assert_eq!(name, "Read");
                assert!(preview.contains("/a/b.rs"), "preview: {preview}");
                assert!(input.contains("file_path"));
            }
            other => panic!("expected ToolUse, got {other:?}"),
        }
    }

    #[test]
    fn parses_tool_result_string_and_blocks() {
        let v = json!({
            "type":"user",
            "message": {
                "role":"user",
                "content":[
                    {"type":"tool_result","tool_use_id":"t1","is_error":false,"content":"stdout"}
                ]
            }
        });
        let ev = claude_event_from_line(&v).expect("event");
        match &ev.blocks[0] {
            Block::ToolResult { is_error, content } => {
                assert!(!*is_error);
                assert_eq!(content, "stdout");
            }
            other => panic!("expected ToolResult, got {other:?}"),
        }

        let v2 = json!({
            "type":"user",
            "message": {
                "role":"user",
                "content":[
                    {"type":"tool_result","tool_use_id":"t2","is_error":true,
                     "content":[{"type":"text","text":"line1"},{"type":"text","text":"line2"}]}
                ]
            }
        });
        let ev2 = claude_event_from_line(&v2).expect("event");
        match &ev2.blocks[0] {
            Block::ToolResult { is_error, content } => {
                assert!(*is_error);
                assert_eq!(content, "line1\nline2");
            }
            other => panic!("expected ToolResult, got {other:?}"),
        }
    }

    #[test]
    fn drops_empty_text_blocks() {
        let v = json!({
            "type":"assistant",
            "message":{
                "role":"assistant",
                "content":[{"type":"text","text":"   "},{"type":"text","text":"ok"}]
            }
        });
        let ev = claude_event_from_line(&v).expect("event");
        assert_eq!(ev.blocks.len(), 1);
        assert!(matches!(&ev.blocks[0], Block::Text(s) if s == "ok"));
    }
}
