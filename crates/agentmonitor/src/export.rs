//! Markdown export + clipboard copy for viewed conversations.
//!
//! Both entry points work on the already-parsed `Vec<ConversationEvent>`
//! that the viewer keeps in `ConversationCache`, so neither touches the
//! filesystem source again. The output is intentionally close to what
//! Claude / Codex render in the web UI: role-headed h2 blocks with code
//! fences for tool calls and quoted blocks for tool results.
//!
//! Clipboard support probes the platform's CLI tool (pbcopy / wl-copy /
//! xclip / clip.exe) at call time and falls back to a clear error if none
//! are available — the user gets a toast saying so instead of silent
//! failure.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::adapter::conversation::{Block, ConversationEvent};
use crate::adapter::types::MessageRole;

/// Render `events` as a single Markdown document. Roles become h2 headers,
/// tool calls and results become fenced code blocks, thinking and
/// attachments become collapsible-feeling quote blocks. Empty blocks are
/// skipped so the output mirrors what the viewer actually shows.
pub fn to_markdown(events: &[ConversationEvent]) -> String {
    let mut out = String::with_capacity(events.len() * 256);
    for ev in events {
        if !ev.has_visible_content() {
            continue;
        }
        let when = ev
            .ts
            .map(|t| {
                t.with_timezone(&chrono::Local)
                    .format("%Y-%m-%d %H:%M:%S")
                    .to_string()
            })
            .unwrap_or_default();
        // Header line — `## <role>  _<timestamp>_`
        if when.is_empty() {
            out.push_str(&format!("## {}\n\n", role_label(&ev.role)));
        } else {
            out.push_str(&format!("## {}  _{}_\n\n", role_label(&ev.role), when));
        }
        for block in &ev.blocks {
            if !block.has_content() {
                continue;
            }
            append_block(&mut out, block);
        }
        out.push_str("\n---\n\n");
    }
    // Trim trailing separator + whitespace so the file doesn't end with
    // a dangling `---`.
    let trimmed = out.trim_end_matches(|c: char| c == '\n' || c == '-' || c.is_whitespace());
    let mut result = trimmed.to_string();
    result.push('\n');
    result
}

fn role_label(role: &MessageRole) -> &'static str {
    match role {
        MessageRole::User => "User",
        MessageRole::Assistant => "Assistant",
        MessageRole::System => "System",
        MessageRole::Tool => "Tool",
        MessageRole::Other(_) => "Other",
    }
}

fn append_block(out: &mut String, block: &Block) {
    match block {
        Block::Text(text) | Block::Summary(text) => {
            out.push_str(text.trim_end());
            out.push_str("\n\n");
        }
        Block::Thinking(text) => {
            out.push_str("<details><summary>thinking</summary>\n\n");
            for line in text.lines() {
                out.push_str("> ");
                out.push_str(line);
                out.push('\n');
            }
            out.push_str("\n</details>\n\n");
        }
        Block::ToolUse {
            name,
            preview,
            input,
        } => {
            let header = if preview.is_empty() {
                name.clone()
            } else {
                format!("{name} — {preview}")
            };
            out.push_str(&format!("**tool_use:** `{header}`\n\n"));
            if !input.trim().is_empty() {
                out.push_str("```json\n");
                out.push_str(input.trim_end());
                out.push_str("\n```\n\n");
            }
        }
        Block::ToolResult { is_error, content } => {
            let label = if *is_error {
                "tool_result (ERROR)"
            } else {
                "tool_result"
            };
            out.push_str(&format!("**{label}:**\n\n"));
            out.push_str("```\n");
            out.push_str(content.trim_end());
            out.push_str("\n```\n\n");
        }
        Block::System(text) => {
            out.push_str("> _system:_ ");
            out.push_str(text.trim());
            out.push_str("\n\n");
        }
        Block::Attachment { label, body } => {
            out.push_str(&format!("**attachment ({label}):**\n\n"));
            if !body.trim().is_empty() {
                out.push_str("```\n");
                out.push_str(body.trim_end());
                out.push_str("\n```\n\n");
            }
        }
    }
}

/// Build the export path for a session under `~/Downloads/agent-monitor/`.
/// Filename: `<agent>-<short_id>.md`. Creates parent dirs as needed.
pub fn default_export_path(agent: &str, short_id: &str) -> Result<PathBuf> {
    let home = std::env::var("HOME").context("HOME not set")?;
    let dir = Path::new(&home).join("Downloads").join("agent-monitor");
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("create_dir_all {}", dir.display()))?;
    Ok(dir.join(format!("{agent}-{short_id}.md")))
}

/// Atomically write `content` to `path` via a tmp + rename. Avoids partial
/// files if the process is interrupted mid-write. Returns the final path.
pub fn write_atomic(path: &Path, content: &str) -> Result<()> {
    let tmp = path.with_extension("md.tmp");
    std::fs::write(&tmp, content)
        .with_context(|| format!("write tmp {}", tmp.display()))?;
    std::fs::rename(&tmp, path)
        .with_context(|| format!("rename {} → {}", tmp.display(), path.display()))?;
    Ok(())
}

/// Pipe `content` to the platform's clipboard tool. Returns Err with a
/// human-friendly message if no tool is available — caller surfaces that
/// in a toast.
pub fn copy_to_clipboard(content: &str) -> Result<()> {
    let candidates = clipboard_candidates();
    for (program, args) in &candidates {
        if !crate::settings::which_exists(program) {
            continue;
        }
        return run_clipboard(program, args, content);
    }
    anyhow::bail!(
        "no clipboard tool found (tried: {})",
        candidates
            .iter()
            .map(|(p, _)| *p)
            .collect::<Vec<_>>()
            .join(", ")
    );
}

/// Per-platform candidate list of `(program, args)` pairs to try in order.
/// Each program is expected to read content from stdin.
fn clipboard_candidates() -> Vec<(&'static str, Vec<&'static str>)> {
    if cfg!(target_os = "macos") {
        vec![("pbcopy", vec![])]
    } else if cfg!(target_os = "windows") {
        // clip.exe accepts piped stdin; encoding caveats apply but UTF-8
        // works on Win10+ when the console code page is set right. Good
        // enough for a "best effort" export.
        vec![("clip.exe", vec![]), ("clip", vec![])]
    } else {
        // Linux: prefer Wayland's wl-copy, fall back to xclip / xsel.
        vec![
            ("wl-copy", vec![]),
            ("xclip", vec!["-selection", "clipboard"]),
            ("xsel", vec!["--clipboard", "--input"]),
        ]
    }
}

fn run_clipboard(program: &str, args: &[&str], content: &str) -> Result<()> {
    use std::io::Write;
    use std::process::{Command, Stdio};
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .with_context(|| format!("spawn {program}"))?;
    if let Some(stdin) = child.stdin.as_mut() {
        stdin
            .write_all(content.as_bytes())
            .with_context(|| format!("write to {program} stdin"))?;
    }
    let status = child
        .wait()
        .with_context(|| format!("wait for {program}"))?;
    if !status.success() {
        anyhow::bail!("{program} exited with {status}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(role: MessageRole, blocks: Vec<Block>) -> ConversationEvent {
        ConversationEvent {
            ts: None,
            role,
            blocks,
        }
    }

    #[test]
    fn markdown_includes_role_headers_and_text() {
        let events = vec![
            ev(MessageRole::User, vec![Block::Text("hello".into())]),
            ev(
                MessageRole::Assistant,
                vec![Block::Text("hi there".into())],
            ),
        ];
        let md = to_markdown(&events);
        assert!(md.contains("## User"));
        assert!(md.contains("## Assistant"));
        assert!(md.contains("hello"));
        assert!(md.contains("hi there"));
    }

    #[test]
    fn markdown_skips_empty_events() {
        let events = vec![
            ev(MessageRole::User, vec![Block::Text("   ".into())]),
            ev(MessageRole::Assistant, vec![Block::Text("ok".into())]),
        ];
        let md = to_markdown(&events);
        assert!(!md.contains("User"), "blank user event should be skipped");
        assert!(md.contains("Assistant"));
    }

    #[test]
    fn markdown_renders_tool_use_with_input() {
        let events = vec![ev(
            MessageRole::Assistant,
            vec![Block::ToolUse {
                name: "Read".into(),
                preview: "/tmp/x.rs".into(),
                input: r#"{"path":"/tmp/x.rs"}"#.into(),
            }],
        )];
        let md = to_markdown(&events);
        assert!(md.contains("**tool_use:** `Read — /tmp/x.rs`"));
        assert!(md.contains("```json"));
        assert!(md.contains(r#"{"path":"/tmp/x.rs"}"#));
    }

    #[test]
    fn markdown_renders_tool_result_with_error_flag() {
        let events = vec![ev(
            MessageRole::Tool,
            vec![Block::ToolResult {
                is_error: true,
                content: "permission denied".into(),
            }],
        )];
        let md = to_markdown(&events);
        assert!(md.contains("tool_result (ERROR)"));
        assert!(md.contains("permission denied"));
    }

    #[test]
    fn markdown_does_not_end_with_separator() {
        let events = vec![ev(MessageRole::User, vec![Block::Text("hi".into())])];
        let md = to_markdown(&events);
        assert!(md.ends_with('\n'));
        assert!(!md.trim_end().ends_with("---"));
    }

    #[test]
    fn write_atomic_creates_file_with_content() {
        let dir = std::env::temp_dir().join(format!("agent-monitor-test-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("export.md");
        write_atomic(&path, "hello\n").unwrap();
        let read = std::fs::read_to_string(&path).unwrap();
        assert_eq!(read, "hello\n");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
