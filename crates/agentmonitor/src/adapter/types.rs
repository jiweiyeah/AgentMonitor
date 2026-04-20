use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum SessionStatus {
    Active,
    Idle,
    Completed,
    #[default]
    Unknown,
}

impl SessionStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Idle => "idle",
            Self::Completed => "done",
            Self::Unknown => "?",
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TokenStats {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_creation: u64,
}

impl TokenStats {
    pub fn total(&self) -> u64 {
        self.input + self.output + self.cache_read + self.cache_creation
    }
}

#[derive(Debug, Clone)]
pub struct SessionMeta {
    pub agent: &'static str,
    pub id: String,
    pub path: PathBuf,
    pub cwd: Option<PathBuf>,
    pub model: Option<String>,
    pub version: Option<String>,
    pub git_branch: Option<String>,
    pub started_at: Option<DateTime<Utc>>,
    pub updated_at: Option<DateTime<Utc>>,
    pub message_count: usize,
    pub tokens: TokenStats,
    pub status: SessionStatus,
    pub byte_offset: u64,
    pub size_bytes: u64,
}

impl SessionMeta {
    pub fn short_id(&self) -> String {
        let n = 8.min(self.id.len());
        self.id[..n].to_string()
    }

    pub fn cwd_display(&self) -> String {
        self.cwd
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "?".into())
    }

    pub fn agent_label(&self) -> &'static str {
        agent_display_name(self.agent)
    }
}

pub fn agent_display_name(id: &str) -> &'static str {
    match id {
        "claude" => "ClaudeCode",
        "codex" => "Codex",
        _ => "unknown",
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MessageRole {
    User,
    Assistant,
    System,
    Tool,
    Other(String),
}

impl MessageRole {
    pub fn label(&self) -> &str {
        match self {
            Self::User => "user",
            Self::Assistant => "assistant",
            Self::System => "system",
            Self::Tool => "tool",
            Self::Other(s) => s.as_str(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct MessagePreview {
    pub ts: Option<DateTime<Utc>>,
    pub role: MessageRole,
    pub text: String,
}
