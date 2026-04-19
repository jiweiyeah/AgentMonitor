use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::{Context, Result};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json::Value;
use tokio::fs;
use tokio::io::{AsyncBufReadExt, BufReader};

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
        cmd.iter().any(|s| {
            s.ends_with("/bin/claude") || s == "claude" || s.contains("/.claude/local/")
        })
    }

    async fn parse_meta_fast(&self, path: &Path) -> Result<SessionMeta> {
        let file = fs::File::open(path)
            .await
            .with_context(|| format!("open {}", path.display()))?;
        let (size, mtime) = meta_of(&file).await;
        let mut meta = self.base_meta(path, size, mtime);
        let mut reader = BufReader::new(file).lines();
        let mut read = 0usize;
        while let Some(line) = reader.next_line().await? {
            if line.trim().is_empty() {
                continue;
            }
            if let Ok(v) = serde_json::from_str::<Value>(&line) {
                fold_claude_line(&v, &mut meta);
            }
            read += 1;
            if read >= HEADER_LINE_SCAN && meta.cwd.is_some() && !meta.id.is_empty() {
                break;
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
                fold_claude_line(&v, &mut meta);
            }
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
        let futures = files.into_iter().map(|p| async move {
            (p.clone(), this.parse_meta_fast(&p).await)
        });
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
            let Some(role_str) = v
                .get("type")
                .and_then(|t| t.as_str())
                .or_else(|| v.get("message").and_then(|m| m.get("role")).and_then(|r| r.as_str()))
            else {
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
            if let Some(n) = usage.get("cache_read_input_tokens").and_then(|u| u.as_u64()) {
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
    Some(PathBuf::from(parent.replace('-', "/")))
}

async fn meta_of(file: &fs::File) -> (u64, Option<DateTime<Utc>>) {
    let m = file.metadata().await.ok();
    let size = m.as_ref().map(|m| m.len()).unwrap_or(0);
    let mtime = m
        .and_then(|m| m.modified().ok())
        .map(DateTime::<Utc>::from);
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

// SystemTime helper no longer needed; conversion inlined in meta_of.
#[allow(dead_code)]
fn system_time_to_utc(t: SystemTime) -> DateTime<Utc> {
    DateTime::<Utc>::from(t)
}
