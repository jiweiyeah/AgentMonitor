use std::io::Stdout;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use crossterm::event::{Event, EventStream, KeyCode, KeyEventKind, KeyModifiers};
use futures::StreamExt;
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use tokio::sync::Notify;

use crate::adapter::{adapter_for_path, DynAdapter};
use crate::app::{
    toggle_filter_token, App, ConversationCache, ExpandMode, Mode, PreviewCache, Tab,
};
use crate::collector::{fs_watch, proc_sampler, token_refresh};
use crate::settings::TerminalApp;
use crate::tui::render;
use crate::tui::settings::SettingsItem;

#[derive(Debug, Clone, Copy)]
pub struct EventLoopOptions {
    pub frame_budget: Duration,
}

impl Default for EventLoopOptions {
    fn default() -> Self {
        Self {
            frame_budget: Duration::from_millis(16),
        }
    }
}

pub async fn run_event_loop(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    mut app: App,
    _opts: EventLoopOptions,
) -> Result<()> {
    let dirty = Arc::new(Notify::new());
    // Dedicated signal from fs_watch → token_refresh. Separate from `dirty`
    // so a render-only notification doesn't spin the (heavier) token refresh
    // loop, and a file change doesn't miss redraws.
    let token_dirty = Arc::new(Notify::new());

    // Spawn background collectors. P1 uses the placeholder fs_watch; P3/P4
    // swap in notify-backed implementations without touching this function.
    let adapters = app.adapters.clone();
    let state = app.state.clone();
    let metrics = app.metrics.clone();
    let interval = app.config.sample_interval;

    let dirty_clone = dirty.clone();
    tokio::spawn(async move {
        proc_sampler::run(adapters.clone(), metrics, interval, dirty_clone).await;
    });

    let adapters_fs = app.adapters.clone();
    let state_fs = state.clone();
    let dirty_fs = dirty.clone();
    let token_dirty_fs = token_dirty.clone();
    tokio::spawn(async move {
        fs_watch::run(adapters_fs, state_fs, token_dirty_fs, dirty_fs).await;
    });

    let adapters_tok = app.adapters.clone();
    let state_tok = state.clone();
    let cache_tok = app.token_cache.clone();
    let dirty_tok = dirty.clone();
    let token_dirty_tok = token_dirty.clone();
    tokio::spawn(async move {
        token_refresh::run(
            adapters_tok,
            state_tok,
            cache_tok,
            token_dirty_tok,
            dirty_tok,
        )
        .await;
    });

    let mut events = EventStream::new();
    // Always draw once so the user sees something immediately.
    render::draw(terminal, &app).await?;
    // Kick off the initial preview load if a session is already selected.
    maybe_load_preview(&app);

    loop {
        tokio::select! {
            maybe = events.next() => {
                let Some(Ok(ev)) = maybe else { break };
                let prev_selected_path = current_selected_path(&app);
                if handle_event(ev, &mut app) {
                    break;
                }
                let new_selected_path = current_selected_path(&app);
                if new_selected_path != prev_selected_path {
                    maybe_load_preview(&app);
                }
                render::draw(terminal, &app).await?;
            }
            _ = dirty.notified() => {
                render::draw(terminal, &app).await?;
            }
        }
        if app.should_quit {
            break;
        }
    }
    Ok(())
}

fn current_selected_path(app: &App) -> Option<PathBuf> {
    let s = app.state.read();
    let visible = s.visible_session_indices(&app.session_filter, app.session_sort);
    let row = visible.get(s.selected_session.min(visible.len().saturating_sub(1)))?;
    s.sessions.get(*row).map(|m| m.path.clone())
}

/// Spawn a task to load the preview (last messages + full message count)
/// for the currently selected session. Does nothing if already cached.
fn maybe_load_preview(app: &App) {
    let Some(path) = current_selected_path(app) else {
        return;
    };
    {
        let s = app.state.read();
        if let Some(cache) = &s.preview {
            if cache.path == path {
                return; // already cached
            }
        }
    }
    // Reserve the cache entry in "loading" state.
    {
        let mut s = app.state.write();
        s.preview = Some(PreviewCache {
            path: path.clone(),
            messages: Vec::new(),
            message_count: 0,
            loading: true,
        });
    }
    let state = app.state.clone();
    let adapters: Vec<DynAdapter> = app.adapters.clone();
    tokio::spawn(async move {
        let Some(adapter) = adapter_for_path(&adapters, &path).cloned() else {
            return;
        };
        let (messages, count) = tokio::join!(
            adapter.tail_messages(&path, 5),
            adapter.parse_meta_full(&path),
        );
        let messages = messages.unwrap_or_default();
        let message_count = count.map(|m| m.message_count).unwrap_or(0);
        let mut s = state.write();
        if let Some(cache) = &mut s.preview {
            if cache.path == path {
                cache.messages = messages;
                cache.message_count = message_count;
                cache.loading = false;
                s.dirty = true;
            }
        }
    });
}

/// Ensure the conversation cache is populated for `path`, reusing any fresh
/// entry. If `force` is true or the on-disk mtime is newer than the cached
/// snapshot, a background reload is kicked off.
fn ensure_conversation(app: &App, path: &Path, force: bool) {
    let path_buf = path.to_path_buf();
    // Fast path: already cached and not loading and not forced → nothing to do.
    {
        let s = app.state.read();
        if let Some(cache) = &s.conversation {
            if cache.path == path_buf && !cache.loading && !force && cache.error.is_none() {
                return;
            }
        }
    }
    {
        let mut s = app.state.write();
        s.conversation = Some(ConversationCache::loading(path_buf.clone()));
    }
    let state = app.state.clone();
    let adapters: Vec<DynAdapter> = app.adapters.clone();
    tokio::spawn(async move {
        let Some(adapter) = adapter_for_path(&adapters, &path_buf).cloned() else {
            let mut s = state.write();
            if let Some(cache) = &mut s.conversation {
                if cache.path == path_buf {
                    cache.loading = false;
                    cache.error = Some("no adapter owns this path".into());
                    s.dirty = true;
                }
            }
            return;
        };
        let mtime = tokio::fs::metadata(&path_buf)
            .await
            .ok()
            .and_then(|m| m.modified().ok());
        let result = adapter.load_conversation(&path_buf).await;
        let mut s = state.write();
        let Some(cache) = s.conversation.as_mut() else {
            return;
        };
        if cache.path != path_buf {
            return; // user moved on to another session
        }
        match result {
            Ok(events) => {
                cache.events = events;
                cache.mtime = mtime;
            }
            Err(err) => {
                cache.error = Some(err.to_string());
            }
        }
        cache.loading = false;
        s.dirty = true;
    });
}

/// Returns `true` if the loop should exit.
fn handle_event(ev: Event, app: &mut App) -> bool {
    let Event::Key(key) = ev else { return false };
    if key.kind != KeyEventKind::Press {
        return false;
    }
    // Ctrl+C always quits, regardless of mode.
    if matches!(key.code, KeyCode::Char('c')) && key.modifiers.contains(KeyModifiers::CONTROL) {
        app.should_quit = true;
        return true;
    }
    let mode = app.state.read().mode.clone();
    match mode {
        Mode::Normal => handle_normal(key.code, key.modifiers, app),
        Mode::Viewer { path } => handle_viewer(key.code, key.modifiers, app, &path),
    }
}

fn handle_normal(code: KeyCode, modifiers: KeyModifiers, app: &mut App) -> bool {
    // Sessions-tab filter input swallows printable keys, Backspace, Esc and
    // Enter so they don't trigger normal-mode actions while the user is typing.
    if app.session_filter_input && app.tab == Tab::Sessions {
        handle_session_filter_input(code, app);
        return false;
    }
    match (code, modifiers) {
        (KeyCode::Char('q'), _) => {
            app.should_quit = true;
            return true;
        }
        (KeyCode::Tab, _) => app.cycle_tab_next(),
        (KeyCode::BackTab, _) => app.cycle_tab_prev(),
        (KeyCode::Char('1'), _) => app.set_tab(Tab::Dashboard),
        (KeyCode::Char('2'), _) => app.set_tab(Tab::Sessions),
        (KeyCode::Char('3'), _) => app.set_tab(Tab::Settings),
        (KeyCode::Char('j') | KeyCode::Down, _) => move_selection(app, 1),
        (KeyCode::Char('k') | KeyCode::Up, _) => move_selection(app, -1),
        (KeyCode::Enter, _) if app.tab == Tab::Dashboard => {
            jump_to_selected_process_session(app);
        }
        (KeyCode::Right, _) if app.tab == Tab::Settings => {
            cycle_selected_setting(app, true);
        }
        (KeyCode::Left, _) if app.tab == Tab::Settings => {
            cycle_selected_setting(app, false);
        }
        (KeyCode::Enter, _) if app.tab == Tab::Settings => {
            cycle_selected_setting(app, true);
        }
        (KeyCode::Right, _) => app.cycle_tab_next(),
        (KeyCode::Left, _) => app.cycle_tab_prev(),
        (KeyCode::Enter, _) if app.tab == Tab::Sessions => {
            let path = current_selected_path(app);
            if let Some(p) = path {
                {
                    let mut s = app.state.write();
                    s.mode = Mode::Viewer { path: p.clone() };
                    s.dirty = true;
                }
                ensure_conversation(app, &p, false);
            }
        }
        (KeyCode::Char('/'), _) if app.tab == Tab::Sessions => {
            app.session_filter_input = true;
            app.state.write().dirty = true;
        }
        (KeyCode::Char('s'), _) if app.tab == Tab::Sessions => {
            app.session_sort = app.session_sort.cycle();
            let mut s = app.state.write();
            s.selected_session = 0;
            s.dirty = true;
        }
        (KeyCode::Char('a'), _) if app.tab == Tab::Sessions => {
            toggle_filter_token(&mut app.session_filter, "status:active");
            let mut s = app.state.write();
            s.selected_session = 0;
            s.dirty = true;
        }
        (KeyCode::Char('c'), _) if app.tab == Tab::Sessions => {
            app.session_filter.clear();
            let mut s = app.state.write();
            s.selected_session = 0;
            s.dirty = true;
        }
        (KeyCode::Char('f'), _) if app.tab == Tab::Settings => {
            // Settings tab: 'f' is refresh (rescan), not the 'r' reset shortcut.
            rescan_sessions(app);
        }
        (KeyCode::Char('f'), _) => {
            // Manual refresh — one-shot scan. Must not clobber tokens: the
            // fast-parse path can't reproduce them, so preserve whatever
            // token_refresh already wrote into state for each path.
            rescan_sessions(app);
        }
        (KeyCode::Char('r'), _) if app.tab == Tab::Sessions => {
            // Resume selected session in a new Terminal window.
            resume_session(app);
        }
        (KeyCode::Char('r'), _) if app.tab == Tab::Settings => {
            crate::tui::settings::reset_to_defaults();
            app.state.write().dirty = true;
        }
        _ => {}
    }
    false
}

fn cycle_selected_setting(app: &mut App, forward: bool) {
    let items = SettingsItem::all();
    if items.is_empty() {
        return;
    }
    let idx = app.selected_setting.min(items.len() - 1);
    if forward {
        items[idx].cycle_forward();
    } else {
        items[idx].cycle_back();
    }
    app.state.write().dirty = true;
}

/// Handle key events while the Sessions filter input is active. Esc cancels
/// the filter, Enter commits it (leaving input mode), Backspace deletes, and
/// any printable character is appended.
fn handle_session_filter_input(code: KeyCode, app: &mut App) {
    match code {
        KeyCode::Esc => {
            app.session_filter.clear();
            app.session_filter_input = false;
            let mut s = app.state.write();
            s.selected_session = 0;
            s.dirty = true;
        }
        KeyCode::Enter => {
            app.session_filter_input = false;
            app.state.write().dirty = true;
        }
        KeyCode::Backspace => {
            app.session_filter.pop();
            let mut s = app.state.write();
            s.selected_session = 0;
            s.dirty = true;
        }
        KeyCode::Char(c) if !c.is_control() => {
            app.session_filter.push(c);
            let mut s = app.state.write();
            s.selected_session = 0;
            s.dirty = true;
        }
        _ => {}
    }
}

fn move_selection(app: &mut App, delta: i32) {
    match app.tab {
        Tab::Sessions => {
            let mut s = app.state.write();
            let visible_len = s
                .visible_session_indices(&app.session_filter, app.session_sort)
                .len();
            if visible_len == 0 {
                return;
            }
            s.selected_session = clamp_step(s.selected_session, delta, visible_len);
            s.dirty = true;
        }
        Tab::Dashboard => {
            // Dashboard embeds the processes table; j/k walks its rows so
            // Enter's "jump to session" action still targets a real row.
            let len = app.metrics.snapshot().len();
            if len == 0 {
                return;
            }
            app.selected_process = clamp_step(app.selected_process, delta, len);
            app.state.write().dirty = true;
        }
        Tab::Settings => {
            let len = SettingsItem::all().len();
            if len == 0 {
                return;
            }
            app.selected_setting = clamp_step(app.selected_setting, delta, len);
            app.state.write().dirty = true;
        }
    }
}

fn jump_to_selected_process_session(app: &mut App) {
    let processes = app.metrics.snapshot();
    let Some(process) = processes.get(app.selected_process).cloned() else {
        return;
    };

    let path = {
        let state = app.state.read();
        let Some(session_idx) = state.best_session_index_for_process(&process) else {
            return;
        };
        state.sessions[session_idx].path.clone()
    };

    app.tab = Tab::Sessions;
    app.session_filter.clear();
    app.session_filter_input = false;

    {
        let mut state = app.state.write();
        if let Some(row) = state.visible_row_for_path("", app.session_sort, &path) {
            state.selected_session = row;
            state.dirty = true;
        }
    }
    maybe_load_preview(app);
}

fn clamp_step(cur: usize, delta: i32, len: usize) -> usize {
    if len == 0 {
        return 0;
    }
    let max = len - 1;
    if delta >= 0 {
        (cur.saturating_add(delta as usize)).min(max)
    } else {
        cur.saturating_sub((-delta) as usize)
    }
}

fn handle_viewer(code: KeyCode, _modifiers: KeyModifiers, app: &mut App, _path: &Path) -> bool {
    match (code, _modifiers) {
        (KeyCode::Esc, _) | (KeyCode::Char('q'), _) => {
            let mut s = app.state.write();
            s.mode = Mode::Normal;
            s.dirty = true;
        }
        (KeyCode::Char('j') | KeyCode::Down, _) => scroll_by(app, 1),
        (KeyCode::Char('k') | KeyCode::Up, _) => scroll_by(app, -1),
        (KeyCode::Char('d'), m) if m.contains(KeyModifiers::CONTROL) => scroll_half(app, 1),
        (KeyCode::Char('u'), m) if m.contains(KeyModifiers::CONTROL) => scroll_half(app, -1),
        (KeyCode::PageDown, _) => scroll_page(app, 1),
        (KeyCode::PageUp, _) => scroll_page(app, -1),
        (KeyCode::Char('g'), _) => set_scroll(app, 0),
        (KeyCode::Char('G'), _) => set_scroll(app, u16::MAX),
        (KeyCode::Char('e'), _) => set_expand(app, ExpandMode::Expanded),
        (KeyCode::Char('c'), _) => set_expand(app, ExpandMode::Collapsed),
        (KeyCode::Char('r'), _) => {
            // Resume works from viewer too — the viewed session is the one
            // being resumed (not the list-selected one).
            resume_session_from_viewer(app, _path);
        }
        _ => {}
    }
    false
}

fn scroll_by(app: &App, delta: i32) {
    let mut s = app.state.write();
    let Some(c) = s.conversation.as_mut() else {
        return;
    };
    let max = c.max_scroll();
    c.scroll = apply_delta(c.scroll, delta).min(max);
    s.dirty = true;
}

fn scroll_half(app: &App, dir: i32) {
    let h = viewport_height(app);
    let step = (h as i32 / 2).max(1) * dir;
    scroll_by(app, step);
}

fn scroll_page(app: &App, dir: i32) {
    let h = viewport_height(app);
    let step = (h as i32).max(1) * dir;
    scroll_by(app, step);
}

fn viewport_height(app: &App) -> u16 {
    app.state
        .read()
        .conversation
        .as_ref()
        .map(|c| c.viewport_height)
        .unwrap_or(20)
        .max(1)
}

fn set_scroll(app: &App, v: u16) {
    let mut s = app.state.write();
    let Some(c) = s.conversation.as_mut() else {
        return;
    };
    let max = c.max_scroll();
    c.scroll = v.min(max);
    s.dirty = true;
}

fn set_expand(app: &App, m: ExpandMode) {
    let mut s = app.state.write();
    let Some(c) = s.conversation.as_mut() else {
        return;
    };
    if c.expand != m {
        c.expand = m;
        // Line positions shift when we change expand mode; reset scroll so the
        // viewport lands on a predictable spot instead of mid-block.
        c.scroll = 0;
        s.dirty = true;
    }
}

fn apply_delta(scroll: u16, delta: i32) -> u16 {
    if delta < 0 {
        scroll.saturating_sub((-delta) as u16)
    } else {
        scroll.saturating_add(delta as u16)
    }
}

/// One-shot rescan across all adapters. Must not clobber tokens: the
/// fast-parse path can't reproduce them, so preserve whatever token_refresh
/// already wrote into state for each path.
fn rescan_sessions(app: &App) {
    let state = app.state.clone();
    let adapters = app.adapters.clone();
    tokio::spawn(async move {
        let mut fresh = Vec::new();
        for adapter in &adapters {
            if let Ok(mut batch) = adapter.scan_all().await {
                fresh.append(&mut batch);
            }
        }
        fresh.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        fs_watch::replace_preserving_tokens(&state, fresh);
    });
}

/// Resume the currently list-selected session (Normal mode).
fn resume_session(app: &App) {
    let s = app.state.read();
    let visible = s.visible_session_indices(&app.session_filter, app.session_sort);
    let row = visible.get(s.selected_session);
    let meta = row.and_then(|&i| s.sessions.get(i)).cloned();
    drop(s);

    let Some(meta) = meta else {
        return;
    };

    let cmd = match meta.agent {
        "claude" => format!("claude --resume {}", meta.id),
        "codex" => format!("codex resume {}", meta.id),
        _ => return,
    };

    let terminal = crate::settings::get().terminal;
    if let Err(err) = open_terminal_with_command(terminal, meta.cwd.as_deref(), &cmd) {
        tracing::warn!(?err, "failed to open terminal for resume");
    }
}

/// Resume the session currently being viewed (Viewer mode). Falls back to
/// list-selected if the viewer path doesn't match any known session.
fn resume_session_from_viewer(app: &App, viewer_path: &Path) {
    let s = app.state.read();
    let meta = s
        .sessions
        .iter()
        .find(|m| m.path == viewer_path)
        .cloned()
        .or_else(|| {
            // Fallback: use list-selected session.
            let visible = s.visible_session_indices(&app.session_filter, app.session_sort);
            let row = visible.get(s.selected_session)?;
            s.sessions.get(*row).cloned()
        });
    drop(s);

    let Some(meta) = meta else {
        return;
    };

    let cmd = match meta.agent {
        "claude" => format!("claude --resume {}", meta.id),
        "codex" => format!("codex resume {}", meta.id),
        _ => return,
    };

    let terminal = crate::settings::get().terminal;
    if let Err(err) = open_terminal_with_command(terminal, meta.cwd.as_deref(), &cmd) {
        tracing::warn!(?err, "failed to open terminal for resume");
    }
}

/// Build a shell command string that changes to `cwd` (if given) then runs
/// `cmd`. Uses single-quote escaping to prevent shell injection.
fn build_cd_command(cwd: Option<&Path>, cmd: &str) -> String {
    match cwd {
        Some(dir) => {
            let escaped = dir.display().to_string().replace('\'', "'\\''");
            format!("cd '{}' && {}", escaped, cmd)
        }
        None => cmd.to_string(),
    }
}

/// Open a new terminal window, `cd` to `cwd` (if given), then run `cmd`.
/// Uses the user's configured terminal from Settings; falls back to
/// macOS Terminal.app or a generic CLI launch on Linux.
fn open_terminal_with_command(
    terminal: TerminalApp,
    cwd: Option<&Path>,
    cmd: &str,
) -> anyhow::Result<()> {
    match terminal {
        TerminalApp::Terminal => open_terminal_applescript(cwd, cmd),
        TerminalApp::ITerm2 => open_iterm_applescript(cwd, cmd),
        TerminalApp::Warp => open_warp(cwd, cmd),
        _ => open_cli_terminal(terminal, cwd, cmd),
    }
}

#[cfg(target_os = "macos")]
fn open_terminal_applescript(cwd: Option<&Path>, cmd: &str) -> anyhow::Result<()> {
    let full_cmd = build_cd_command(cwd, cmd);
    let escaped = full_cmd.replace('\\', "\\\\").replace('"', "\\\"");
    std::process::Command::new("osascript")
        .arg("-e")
        .arg(format!(
            "tell application \"Terminal\" to do script \"{escaped}\""
        ))
        .spawn()?;
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn open_terminal_applescript(_cwd: Option<&Path>, _cmd: &str) -> anyhow::Result<()> {
    anyhow::bail!("Terminal.app is only available on macOS")
}

#[cfg(target_os = "macos")]
fn open_iterm_applescript(cwd: Option<&Path>, cmd: &str) -> anyhow::Result<()> {
    let full_cmd = build_cd_command(cwd, cmd);
    let escaped = full_cmd.replace('\\', "\\\\").replace('"', "\\\"");
    std::process::Command::new("osascript")
        .arg("-e")
        .arg(format!(
            "tell application \"iTerm\"\n\
             activate\n\
             create window with default profile\n\
             tell current session of current window\n\
             write text \"{escaped}\"\n\
             end tell\n\
             end tell"
        ))
        .spawn()?;
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn open_iterm_applescript(_cwd: Option<&Path>, _cmd: &str) -> anyhow::Result<()> {
    anyhow::bail!("iTerm2 is only available on macOS")
}

#[cfg(target_os = "macos")]
fn open_warp(cwd: Option<&Path>, cmd: &str) -> anyhow::Result<()> {
    // Prefer CLI launch if the warp binary is on PATH — avoids System Events
    // keystroke injection entirely (which needs accessibility permissions and
    // breaks on special characters).
    if crate::settings::which_exists("warp") {
        return open_cli_terminal(TerminalApp::Warp, cwd, cmd);
    }
    // Fallback: open the app and paste the command via clipboard.
    let full_cmd = build_cd_command(cwd, cmd);
    // Copy command to clipboard.
    let mut pbcopy = std::process::Command::new("pbcopy")
        .stdin(std::process::Stdio::piped())
        .spawn()?;
    if let Some(mut stdin) = pbcopy.stdin.take() {
        use std::io::Write;
        let _ = stdin.write_all(full_cmd.as_bytes());
    }
    pbcopy.wait()?;
    // Activate Warp.
    std::process::Command::new("osascript")
        .arg("-e")
        .arg("tell application \"Warp\" to activate")
        .spawn()?;
    // Small delay for Warp to come to foreground.
    std::thread::sleep(std::time::Duration::from_millis(300));
    // Paste via Cmd+V.
    std::process::Command::new("osascript")
        .arg("-e")
        .arg("tell application \"System Events\" to keystroke \"v\" using command down")
        .spawn()?;
    // Press Enter.
    std::process::Command::new("osascript")
        .arg("-e")
        .arg("tell application \"System Events\" to key code 36")
        .spawn()?;
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn open_warp(_cwd: Option<&Path>, _cmd: &str) -> anyhow::Result<()> {
    anyhow::bail!("Warp is only available on macOS")
}

/// Resolve the user's login shell, falling back to `/bin/sh`.
fn user_shell() -> String {
    std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string())
}

/// Launch a CLI-based terminal (Ghostty, Alacritty, Kitty, WezTerm, Warp)
/// that accepts `-e` or equivalent to run a command.
fn open_cli_terminal(terminal: TerminalApp, cwd: Option<&Path>, cmd: &str) -> anyhow::Result<()> {
    let full_cmd = build_cd_command(cwd, cmd);
    let shell = user_shell();
    let (program, args) = match terminal {
        TerminalApp::Ghostty => ("ghostty", vec!["-e", &shell, "-c", &full_cmd] as Vec<_>),
        TerminalApp::Alacritty => ("alacritty", vec!["-e", &shell, "-c", &full_cmd]),
        TerminalApp::Kitty => ("kitty", vec![&shell, "-c", &full_cmd]),
        TerminalApp::WezTerm => ("wezterm", vec!["start", "--", &shell, "-c", &full_cmd]),
        TerminalApp::Warp => ("warp", vec![&shell, "-c", &full_cmd]),
        _ => anyhow::bail!("unsupported terminal: {:?}", terminal),
    };
    std::process::Command::new(program).args(&args).spawn()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::types::{MessageRole, SessionMeta, SessionStatus, TokenStats};
    use crate::adapter::{ClaudeAdapter, CodexAdapter};
    use crate::app::{AppState, SessionSort};
    use crate::collector::metrics::MetricsStore;
    use crate::collector::token_refresh::TokenCache;
    use crate::config::Config;
    use parking_lot::RwLock;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    fn test_app() -> App {
        App {
            config: Config {
                claude_root: None,
                codex_root: None,
                ..Config::default()
            },
            state: Arc::new(RwLock::new(AppState::default())),
            metrics: Arc::new(MetricsStore::new(8)),
            adapters: Vec::new(),
            tab: Tab::Dashboard,
            should_quit: false,
            session_filter: String::new(),
            session_filter_input: false,
            session_sort: SessionSort::default(),
            selected_process: 0,
            selected_setting: 0,
            token_cache: Arc::new(TokenCache::new()),
        }
    }

    #[test]
    fn esc_does_not_quit_in_normal_mode() {
        let mut app = test_app();

        let should_exit = handle_normal(KeyCode::Esc, KeyModifiers::empty(), &mut app);

        assert!(!should_exit);
        assert!(!app.should_quit);
    }

    #[test]
    fn q_quits_in_normal_mode() {
        let mut app = test_app();

        let should_exit = handle_normal(KeyCode::Char('q'), KeyModifiers::empty(), &mut app);

        assert!(should_exit);
        assert!(app.should_quit);
    }

    #[tokio::test]
    async fn preview_loader_uses_codex_adapter_for_codex_sessions() {
        let unique = format!(
            "agentmonitor-preview-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("time went backwards")
                .as_nanos()
        );
        let base = std::env::temp_dir().join(unique);
        let claude_root = base.join(".claude").join("projects");
        let codex_root = base.join(".codex").join("sessions");
        let session_path = codex_root
            .join("2026")
            .join("04")
            .join("25")
            .join("rollout-2026-04-25T16-21-21-019dc3ba-a222-7631-96e6-2e1ebb238e53.jsonl");
        std::fs::create_dir_all(&claude_root).expect("create claude root");
        std::fs::create_dir_all(session_path.parent().expect("session dir"))
            .expect("create codex session dir");
        std::fs::write(
            &session_path,
            concat!(
                r#"{"timestamp":"2026-04-25T08:21:38.973Z","type":"session_meta","payload":{"id":"019dc3ba-a222-7631-96e6-2e1ebb238e53","timestamp":"2026-04-25T08:21:21.574Z","cwd":"/tmp/repo","originator":"Codex Desktop","cli_version":"0.125.0-alpha.3","model_provider":"openai"}}"#,
                "\n",
                r#"{"timestamp":"2026-04-25T08:21:56.528Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"hello from user"}]}}"#,
                "\n",
                r#"{"timestamp":"2026-04-25T08:22:07.734Z","type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"hello from assistant"}]}}"#,
                "\n"
            ),
        )
        .expect("write codex session");

        let app = App {
            config: Config {
                claude_root: Some(claude_root.clone()),
                codex_root: Some(codex_root.clone()),
                ..Config::default()
            },
            state: Arc::new(RwLock::new(AppState {
                sessions: vec![SessionMeta {
                    agent: "codex",
                    id: "019dc3ba-a222-7631-96e6-2e1ebb238e53".into(),
                    path: session_path.clone(),
                    cwd: Some(PathBuf::from("/tmp/repo")),
                    model: Some("openai".into()),
                    version: Some("0.125.0-alpha.3".into()),
                    git_branch: None,
                    started_at: None,
                    updated_at: None,
                    message_count: 0,
                    tokens: TokenStats::default(),
                    status: SessionStatus::Idle,
                    byte_offset: 0,
                    size_bytes: 0,
                }],
                selected_session: 0,
                dirty: false,
                preview: None,
                mode: Mode::Normal,
                conversation: None,
            })),
            metrics: Arc::new(MetricsStore::new(8)),
            adapters: vec![
                Arc::new(ClaudeAdapter::new(Some(claude_root))),
                Arc::new(CodexAdapter::new(Some(codex_root))),
            ],
            tab: Tab::Sessions,
            should_quit: false,
            session_filter: String::new(),
            session_filter_input: false,
            session_sort: SessionSort::default(),
            selected_process: 0,
            selected_setting: 0,
            token_cache: Arc::new(TokenCache::new()),
        };

        maybe_load_preview(&app);

        let mut loaded = None;
        for _ in 0..50 {
            {
                let s = app.state.read();
                if let Some(cache) = &s.preview {
                    if !cache.loading {
                        loaded = Some(cache.clone());
                        break;
                    }
                }
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        let preview = loaded.expect("preview should finish loading");
        assert_eq!(
            preview.messages.len(),
            2,
            "preview should show codex messages"
        );
        assert!(matches!(preview.messages[0].role, MessageRole::User));
        assert_eq!(preview.messages[0].text, "hello from user");
        assert!(matches!(preview.messages[1].role, MessageRole::Assistant));
        assert_eq!(preview.messages[1].text, "hello from assistant");

        let _ = std::fs::remove_dir_all(base);
    }
}
