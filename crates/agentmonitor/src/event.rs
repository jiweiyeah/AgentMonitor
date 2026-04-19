use std::io::Stdout;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use crossterm::event::{Event, EventStream, KeyCode, KeyEventKind, KeyModifiers};
use futures::StreamExt;
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use tokio::sync::Notify;

use crate::adapter::DynAdapter;
use crate::app::{App, PreviewCache, Tab};
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
    s.sessions
        .get(s.selected_session)
        .map(|m| m.path.clone())
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

/// Returns `true` if the loop should exit.
fn handle_event(ev: Event, app: &mut App) -> bool {
    let Event::Key(key) = ev else { return false };
    if key.kind != KeyEventKind::Press {
        return false;
    }
    match (key.code, key.modifiers) {
        (KeyCode::Char('q'), _) | (KeyCode::Esc, _) => {
            app.should_quit = true;
            return true;
        }
        (KeyCode::Char('c'), KeyModifiers::CONTROL) => {
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
