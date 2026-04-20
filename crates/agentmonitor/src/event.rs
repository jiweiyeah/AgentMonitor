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
use crate::collector::{fs_watch, proc_sampler};
use crate::tui::render;

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
    tokio::spawn(async move {
        fs_watch::run(adapters_fs, state_fs, dirty_fs).await;
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
    s.sessions.get(s.selected_session).map(|m| m.path.clone())
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
    match (code, modifiers) {
        (KeyCode::Char('q'), _) | (KeyCode::Esc, _) => {
            app.should_quit = true;
            return true;
        }
        (KeyCode::Tab, _) | (KeyCode::Right, _) => app.cycle_tab_next(),
        (KeyCode::BackTab, _) | (KeyCode::Left, _) => app.cycle_tab_prev(),
        (KeyCode::Char('1'), _) => app.set_tab(Tab::Dashboard),
        (KeyCode::Char('2'), _) => app.set_tab(Tab::Sessions),
        (KeyCode::Char('3'), _) => app.set_tab(Tab::Process),
        (KeyCode::Char('j') | KeyCode::Down, _) => {
            let mut s = app.state.write();
            if !s.sessions.is_empty() && s.selected_session + 1 < s.sessions.len() {
                s.selected_session += 1;
            }
        }
        (KeyCode::Char('k') | KeyCode::Up, _) => {
            let mut s = app.state.write();
            if s.selected_session > 0 {
                s.selected_session -= 1;
            }
        }
        (KeyCode::Enter, _) if app.tab == Tab::Sessions => {
            let path = {
                let s = app.state.read();
                s.sessions.get(s.selected_session).map(|m| m.path.clone())
            };
            if let Some(p) = path {
                {
                    let mut s = app.state.write();
                    s.mode = Mode::Viewer { path: p.clone() };
                    s.dirty = true;
                }
                ensure_conversation(app, &p, false);
            }
        }
        (KeyCode::Char('r'), _) => {
            // Manual refresh — trigger a one-shot scan.
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
                let mut s = state.write();
                s.sessions = fresh;
                s.dirty = true;
            });
        }
        _ => {}
    }
    false
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
