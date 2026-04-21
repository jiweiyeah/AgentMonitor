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
use crate::app::{App, ConversationCache, ExpandMode, Mode, PreviewCache, Tab};
use crate::collector::{fs_watch, proc_sampler, token_refresh};
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
        token_refresh::run(adapters_tok, state_tok, cache_tok, token_dirty_tok, dirty_tok).await;
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
        let adapter = adapters.iter().find(|a| a.owns_path(&path));
        let Some(adapter) = adapter else { return };
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
    // Enter so they don't trigger normal-mode actions like quit / open viewer.
    if app.session_filter_input && app.tab == Tab::Sessions {
        handle_session_filter_input(code, app);
        return false;
    }
    match (code, modifiers) {
        (KeyCode::Char('q'), _) | (KeyCode::Esc, _) => {
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
        (KeyCode::Enter, _) if app.tab == Tab::Dashboard => {
            // Dashboard now embeds the processes table, so Enter jumps to
            // the session whose cwd matches the highlighted process row.
            jump_to_process_session(app);
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
        (KeyCode::Char('c'), _) if app.tab == Tab::Sessions => {
            app.session_filter.clear();
            let mut s = app.state.write();
            s.selected_session = 0;
            s.dirty = true;
        }
        (KeyCode::Char('r'), _) if app.tab == Tab::Settings => {
            // Settings tab reuses `r` as reset-to-defaults rather than the
            // global rescan shortcut — rescanning from here doesn't help the
            // user tweak preferences.
            crate::tui::settings::reset_to_defaults();
            app.state.write().dirty = true;
        }
        (KeyCode::Char('r'), _) => {
            // Manual refresh — one-shot scan. Must not clobber tokens: the
            // fast-parse path can't reproduce them, so preserve whatever
            // token_refresh already wrote into state for each path.
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

/// On Enter from the Dashboard tab, find a session whose `cwd` matches the
/// currently highlighted process row and jump to it in the Sessions tab.
/// Best-effort: if no match, stay put. This is the cross-tab correlation
/// that makes PIDs actionable from the dashboard's embedded processes table.
fn jump_to_process_session(app: &mut App) {
    let procs = app.metrics.snapshot();
    let Some(proc) = procs.get(app.selected_process) else {
        return;
    };
    let Some(target_cwd) = proc.cwd.clone() else {
        return;
    };
    let agent = proc.agent;
    let target = {
        let s = app.state.read();
        // Among sessions with matching cwd+agent, prefer the most recently
        // updated one — that's the active conversation the user just saw.
        s.sessions
            .iter()
            .enumerate()
            .filter(|(_, m)| {
                m.agent == agent
                    && m.cwd
                        .as_ref()
                        .map(|p| p.to_string_lossy() == target_cwd)
                        .unwrap_or(false)
            })
            .max_by_key(|(_, m)| m.updated_at)
            .map(|(i, _)| i)
    };
    let Some(raw_idx) = target else { return };

    app.tab = Tab::Sessions;
    // Resolve raw_idx → visible row so selected_session indexes into what the
    // user actually sees.
    let mut s = app.state.write();
    let visible = s.visible_session_indices(&app.session_filter, app.session_sort);
    if let Some(row) = visible.iter().position(|&i| i == raw_idx) {
        s.selected_session = row;
    } else {
        // Filter hides it: clear filter so the jump is visible.
        drop(s);
        app.session_filter.clear();
        let mut s = app.state.write();
        let visible = s.visible_session_indices(&app.session_filter, app.session_sort);
        if let Some(row) = visible.iter().position(|&i| i == raw_idx) {
            s.selected_session = row;
        }
    }
    app.state.write().dirty = true;
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
