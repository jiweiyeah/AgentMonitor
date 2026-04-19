//! Filesystem watcher (P4). Notify-backed, with a 10s fallback rescan.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use notify::{recommended_watcher, Event, EventKind, RecursiveMode, Watcher};
use parking_lot::RwLock;
use tokio::sync::mpsc::unbounded_channel;
use tokio::sync::Notify;
use tokio::time::{interval, sleep};

use crate::adapter::DynAdapter;
use crate::app::AppState;

/// Background task: owns a notify watcher + fallback ticker.
pub async fn run(
    adapters: Vec<DynAdapter>,
    state: Arc<RwLock<AppState>>,
    dirty: Arc<Notify>,
) {
    let (ev_tx, mut ev_rx) = unbounded_channel::<Event>();

    // Notify uses a blocking callback. Bridge it into a tokio channel.
    let watcher_result = recommended_watcher(move |res: notify::Result<Event>| {
        if let Ok(ev) = res {
            let _ = ev_tx.send(ev);
        }
    });
    let mut watcher = match watcher_result {
        Ok(w) => w,
        Err(err) => {
            tracing::warn!(?err, "notify watcher unavailable; falling back to polling only");
            return fallback_only(adapters, state, dirty).await;
        }
    };

    for adapter in &adapters {
        for root in adapter.session_roots() {
            if !root.exists() {
                continue;
            }
            if let Err(err) = watcher.watch(&root, RecursiveMode::Recursive) {
                tracing::warn!(root = %root.display(), ?err, "watch failed");
            }
        }
    }

    let mut reconcile = interval(Duration::from_secs(10));
    reconcile.tick().await; // discard first immediate tick

    loop {
        tokio::select! {
            Some(event) = ev_rx.recv() => {
                handle_event(event, &adapters, &state).await;
                dirty.notify_one();
            }
            _ = reconcile.tick() => {
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
                drop(s);
                dirty.notify_one();
            }
            else => break,
        }
    }
}

async fn handle_event(ev: Event, adapters: &[DynAdapter], state: &Arc<RwLock<AppState>>) {
    match ev.kind {
        EventKind::Create(_) | EventKind::Modify(_) => {
            for path in ev.paths {
                update_for_path(path, adapters, state).await;
            }
        }
        EventKind::Remove(_) => {
            for path in ev.paths {
                let mut s = state.write();
                if let Some(idx) = s.sessions.iter().position(|m| m.path == path) {
                    s.sessions.remove(idx);
                    if s.selected_session >= s.sessions.len() && !s.sessions.is_empty() {
                        s.selected_session = s.sessions.len() - 1;
                    }
                    s.dirty = true;
                }
            }
        }
        _ => {}
    }
}

async fn update_for_path(
    path: PathBuf,
    adapters: &[DynAdapter],
    state: &Arc<RwLock<AppState>>,
) {
    let Some(adapter) = adapters.iter().find(|a| a.owns_path(&path)) else {
        return;
    };
    // Brief debounce: editors often fire multiple Modify events in quick
    // succession. Sleep 50ms to coalesce the tail.
    sleep(Duration::from_millis(50)).await;
    let meta = match adapter.parse_meta_fast(&path).await {
        Ok(m) => m,
        Err(err) => {
            tracing::debug!(path = %path.display(), ?err, "fast parse failed");
            return;
        }
    };

    let mut s = state.write();
    if let Some(existing) = s.sessions.iter_mut().find(|m| m.path == path) {
        *existing = meta;
    } else {
        s.sessions.push(meta);
    }
    s.sessions.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    s.dirty = true;
}

/// Poll-only fallback when notify isn't available (e.g. in restricted sandboxes).
async fn fallback_only(
    adapters: Vec<DynAdapter>,
    state: Arc<RwLock<AppState>>,
    dirty: Arc<Notify>,
) {
    let mut ticker = interval(Duration::from_secs(10));
    ticker.tick().await;
    loop {
        ticker.tick().await;
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
        drop(s);
        dirty.notify_one();
    }
}
