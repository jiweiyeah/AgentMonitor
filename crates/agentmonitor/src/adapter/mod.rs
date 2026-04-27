pub mod claude;
pub mod claude_desktop;
pub mod codex;
pub mod conversation;
pub mod types;

use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use async_trait::async_trait;
use tokio::fs;
use tokio::io::{AsyncReadExt, AsyncSeekExt, SeekFrom};

pub use claude::ClaudeAdapter;
pub use claude_desktop::ClaudeDesktopAdapter;
pub use codex::CodexAdapter;
pub use conversation::{Block, ConversationEvent};
pub use types::*;

/// Shared adapter handle stored in [`App`].
pub type DynAdapter = Arc<dyn AgentAdapter>;

/// Pick the adapter that owns the given session file. Uses `session_roots`
/// first (path prefix match) so that `.jsonl` files under `~/.codex` are never
/// mistakenly claimed by the Claude adapter, regardless of vec ordering.
pub fn adapter_for_path<'a>(adapters: &'a [DynAdapter], path: &Path) -> Option<&'a DynAdapter> {
    for a in adapters {
        for root in a.session_roots() {
            if path.starts_with(&root) && a.owns_path(path) {
                return Some(a);
            }
        }
    }
    // Fallback: any adapter that explicitly claims the path.
    adapters.iter().find(|a| a.owns_path(path))
}

/// Read the last `max` bytes of `path` as UTF-8 text (lossy), trimming the
/// leading partial line so callers only see complete JSONL records.
pub(crate) async fn tail_bytes(path: &Path, max: u64) -> Result<String> {
    let mut file = fs::File::open(path)
        .await
        .with_context(|| format!("tail open {}", path.display()))?;
    let size = file.metadata().await?.len();
    let start = size.saturating_sub(max);
    file.seek(SeekFrom::Start(start)).await?;
    let mut buf = Vec::with_capacity(max.min(size) as usize);
    file.read_to_end(&mut buf).await?;
    let text = String::from_utf8_lossy(&buf).to_string();
    if start == 0 {
        Ok(text)
    } else {
        // Drop the potentially truncated first line.
        Ok(match text.find('\n') {
            Some(idx) => text[idx + 1..].to_string(),
            None => String::new(),
        })
    }
}

#[async_trait]
pub trait AgentAdapter: Send + Sync + 'static {
    /// Stable identifier (`"claude"`, `"codex"`, ...).
    fn id(&self) -> &'static str;
    /// Human-readable display name.
    fn display_name(&self) -> &'static str;
    /// Directories notify should watch. Empty if adapter is disabled.
    fn session_roots(&self) -> Vec<std::path::PathBuf>;
    /// Return `true` if a session file under one of this adapter's roots
    /// should be handed to `parse_meta_fast`. Default: filename ends in `.jsonl`.
    fn owns_path(&self, path: &Path) -> bool {
        path.extension().and_then(|s| s.to_str()) == Some("jsonl")
    }
    /// True if this `sysinfo` process looks like the agent's main process.
    fn matches_process(&self, cmd: &[String], exe: Option<&Path>) -> bool;
    /// Fast parse: header + mtime/size, used on startup and on fs events.
    /// Must stay well under 1 ms per call even for multi-MB files.
    async fn parse_meta_fast(&self, path: &Path) -> Result<SessionMeta>;
    /// Full parse: walks every line to count messages and sum tokens. Used on
    /// demand when a session is selected.
    async fn parse_meta_full(&self, path: &Path) -> Result<SessionMeta>;
    /// Scan every session file under `session_roots()` in parallel.
    async fn scan_all(&self) -> Result<Vec<SessionMeta>>;
    /// Read the last `n` messages for preview. Default impl returns empty.
    async fn tail_messages(&self, _path: &Path, _n: usize) -> Result<Vec<MessagePreview>> {
        Ok(Vec::new())
    }
    /// Parse the whole session file into a chronological list of events.
    /// Default impl returns empty so adapters can opt in.
    async fn load_conversation(&self, _path: &Path) -> Result<Vec<ConversationEvent>> {
        Ok(Vec::new())
    }
}
