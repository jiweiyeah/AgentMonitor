use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json::Value;
use tokio::fs;
use tokio::io::{AsyncBufReadExt, BufReader};

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
        let futures = files.into_iter().map(|p| async move {
            (p.clone(), this.parse_meta_fast(&p).await)
        });
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
