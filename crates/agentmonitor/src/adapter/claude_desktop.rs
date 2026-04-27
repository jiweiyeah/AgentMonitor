use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use async_trait::async_trait;
use futures::stream::StreamExt;
use tokio::fs;
use tokio::io::{AsyncBufReadExt, BufReader};

use super::conversation::ConversationEvent;
use super::types::{MessagePreview, SessionMeta, SessionStatus, TokenStats};
use super::{
    claude::{
        claude_event_from_line, decode_cwd_from_parent, fold_claude_line, infer_status,
        is_native_first_type, meta_of, truncate, HEADER_LINE_SCAN,
    },
    AgentAdapter,
};

pub struct ClaudeDesktopAdapter {
    root: Option<PathBuf>,
}

impl ClaudeDesktopAdapter {
    pub fn new(root: Option<PathBuf>) -> Self {
        Self { root }
    }

    fn base_meta(&self, path: &Path, size: u64, mtime: Option<chrono::DateTime<chrono::Utc>>) -> SessionMeta {
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

/// Match the Claude Desktop main process only — not the Electron helper/
/// renderer/crashpad subprocesses.
fn is_claude_desktop_main_process(path: &Path) -> bool {
    let s = path.to_string_lossy();
    s.ends_with("/Claude.app/Contents/MacOS/Claude")
}

#[async_trait]
impl AgentAdapter for ClaudeDesktopAdapter {
    fn id(&self) -> &'static str {
        "claude-desktop"
    }

    fn display_name(&self) -> &'static str {
        "ClaudeDesktop"
    }

    fn session_roots(&self) -> Vec<PathBuf> {
        self.root.iter().cloned().collect()
    }

    fn owns_path(&self, path: &Path) -> bool {
        // Reject audit.jsonl files — they use a different schema (_audit_timestamp,
        // client_platform, session_id) and are not real conversation sessions.
        if path
            .file_name()
            .and_then(|s| s.to_str())
            .is_some_and(|n| n == "audit.jsonl")
        {
            return false;
        }
        path.extension().and_then(|s| s.to_str()) == Some("jsonl")
    }

    fn matches_process(&self, _cmd: &[String], exe: Option<&Path>) -> bool {
        exe.is_some_and(is_claude_desktop_main_process)
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
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) {
                if first_type.is_none() {
                    if let Some(t) = v.get("type").and_then(|t| t.as_str()) {
                        // Skip queue-operation noise from claude-mem when
                        // determining the first real record type. Desktop
                        // sessions often start with these rows, but the file
                        // is still a valid session.
                        if t != "queue-operation" {
                            first_type = Some(t.to_string());
                        }
                    }
                }
                fold_claude_line(&v, &mut meta);
            }
            read += 1;
            if read >= HEADER_LINE_SCAN && meta.cwd.is_some() && !meta.id.is_empty() {
                break;
            }
        }
        if !is_native_first_type(first_type.as_deref()) {
            anyhow::bail!(
                "not a Claude Desktop conversation: {}",
                path.display()
            );
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
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) {
                if first_type.is_none() {
                    if let Some(t) = v.get("type").and_then(|t| t.as_str()) {
                        if t != "queue-operation" {
                            first_type = Some(t.to_string());
                        }
                    }
                }
                fold_claude_line(&v, &mut meta);
            }
        }
        if !is_native_first_type(first_type.as_deref()) {
            anyhow::bail!(
                "not a Claude Desktop conversation: {}",
                path.display()
            );
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
        // local-agent-mode-sessions/<org>/<workspace>/local_<id>/.claude/projects/<dir>/<id>.jsonl
        // Can be up to 7+ levels deep.
        let files: Vec<PathBuf> = walkdir::WalkDir::new(root)
            .max_depth(8)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_file())
            .filter(|e| e.path().extension().and_then(|s| s.to_str()) == Some("jsonl"))
            .filter(|e| {
                e.file_name()
                    .to_string_lossy()
                    != "audit.jsonl"
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
                Err(err) => {
                    tracing::debug!(path = %path.display(), ?err, "claude-desktop parse skipped")
                }
            }
        }
        Ok(out)
    }

    async fn tail_messages(&self, path: &Path, n: usize) -> Result<Vec<MessagePreview>> {
        use super::claude::HEADER_LINE_SCAN;
        let _ = HEADER_LINE_SCAN; // suppress unused import warning
        let data = super::tail_bytes(path, 256 * 1024).await?;
        let mut previews = Vec::new();
        for line in data.lines().rev() {
            if line.trim().is_empty() {
                continue;
            }
            let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
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
                "user" => super::types::MessageRole::User,
                "assistant" => super::types::MessageRole::Assistant,
                "system" => super::types::MessageRole::System,
                _ => continue,
            };
            let text = extract_text(&v).unwrap_or_default();
            if text.is_empty() {
                continue;
            }
            let ts = v
                .get("timestamp")
                .and_then(|t| t.as_str())
                .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                .map(|d| d.with_timezone(&chrono::Utc));
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
            let Ok(v) = serde_json::from_str::<serde_json::Value>(l) else {
                continue;
            };
            if let Some(ev) = claude_event_from_line(&v) {
                events.push(ev);
            }
        }
        Ok(events)
    }
}

fn extract_text(v: &serde_json::Value) -> Option<String> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_claude_desktop_main_process() {
        let adapter = ClaudeDesktopAdapter::new(None);
        let exe = Path::new("/Applications/Claude.app/Contents/MacOS/Claude");
        assert!(adapter.matches_process(&[], Some(exe)));
    }

    #[test]
    fn does_not_match_claude_helper_processes() {
        let adapter = ClaudeDesktopAdapter::new(None);
        let helper =
            Path::new("/Applications/Claude.app/Contents/Frameworks/Claude Helper.app/Contents/MacOS/Claude Helper");
        assert!(!adapter.matches_process(&[], Some(helper)));

        let renderer = Path::new(
            "/Applications/Claude.app/Contents/Frameworks/Claude Helper (Renderer).app/Contents/MacOS/Claude Helper (Renderer)",
        );
        assert!(!adapter.matches_process(&[], Some(renderer)));
    }

    #[test]
    fn does_not_match_claude_cli() {
        let adapter = ClaudeDesktopAdapter::new(None);
        let cmd = vec!["/Users/test/.claude/local/bin/claude".to_string()];
        assert!(!adapter.matches_process(&cmd, None));
    }

    #[test]
    fn does_not_match_crashpad_handler() {
        let adapter = ClaudeDesktopAdapter::new(None);
        let exe = Path::new(
            "/Applications/Claude.app/Contents/Frameworks/Electron Framework.framework/Helpers/chrome_crashpad_handler",
        );
        assert!(!adapter.matches_process(&[], Some(exe)));
    }
}
