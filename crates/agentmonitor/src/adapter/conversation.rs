//! Shared conversation model rendered by the full-screen viewer.
//!
//! Each adapter parses its native JSONL into a flat `Vec<ConversationEvent>`.
//! Events are append-only chronological entries; every event owns a list of
//! `Block`s that can be independently collapsed in the UI.

use chrono::{DateTime, Utc};

use super::types::MessageRole;

/// One chronological entry in the rendered transcript.
#[derive(Debug, Clone)]
pub struct ConversationEvent {
    pub ts: Option<DateTime<Utc>>,
    pub role: MessageRole,
    pub blocks: Vec<Block>,
}

impl ConversationEvent {
    pub fn has_visible_content(&self) -> bool {
        self.blocks.iter().any(|b| b.has_content())
    }
}

#[derive(Debug, Clone)]
pub enum Block {
    /// Plain user/assistant text.
    Text(String),
    /// Assistant reasoning ("thinking") block.
    Thinking(String),
    /// Tool call request issued by the assistant.
    ToolUse {
        name: String,
        /// Compact one-liner for the collapsed view (e.g. `Read /path/to/file`).
        preview: String,
        /// Pretty-printed JSON of the full input, shown when expanded.
        input: String,
    },
    /// Result coming back from a tool call.
    ToolResult { is_error: bool, content: String },
    /// System message / slash-command expansion.
    System(String),
    /// Hook output, file snapshots, or other non-conversational attachments.
    Attachment { label: String, body: String },
    /// Session summary row (first line of most Claude transcripts).
    Summary(String),
}

impl Block {
    pub fn has_content(&self) -> bool {
        match self {
            Block::Text(s) | Block::Thinking(s) | Block::System(s) | Block::Summary(s) => {
                !s.trim().is_empty()
            }
            Block::ToolUse { preview, input, .. } => !preview.is_empty() || !input.is_empty(),
            Block::ToolResult { content, .. } => !content.trim().is_empty(),
            Block::Attachment { label, body } => !label.is_empty() || !body.trim().is_empty(),
        }
    }

    /// True if the block is noisy enough to default-hide behind a one-line
    /// summary. Plain text is always shown.
    pub fn is_collapsible(&self) -> bool {
        matches!(
            self,
            Block::Thinking(_)
                | Block::ToolUse { .. }
                | Block::ToolResult { .. }
                | Block::Attachment { .. }
                | Block::System(_)
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_with_whitespace_is_empty() {
        assert!(!Block::Text("   \n ".into()).has_content());
        assert!(Block::Text("hi".into()).has_content());
    }

    #[test]
    fn event_skips_blank_blocks() {
        let ev = ConversationEvent {
            ts: None,
            role: MessageRole::User,
            blocks: vec![Block::Text("  ".into())],
        };
        assert!(!ev.has_visible_content());
        let ev = ConversationEvent {
            ts: None,
            role: MessageRole::User,
            blocks: vec![Block::Text("".into()), Block::Text("ok".into())],
        };
        assert!(ev.has_visible_content());
    }

    #[test]
    fn tool_use_without_content_is_empty() {
        let b = Block::ToolUse {
            name: String::new(),
            preview: String::new(),
            input: String::new(),
        };
        assert!(!b.has_content());
    }
}
