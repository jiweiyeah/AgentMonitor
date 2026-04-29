//! Background refresh for per-session token stats.
//!
//! The fast-parse path in each adapter only walks the JSONL header — enough
//! to resolve id/cwd/model/branch/timestamps, but token usage accumulates
//! across *every* assistant turn (Claude) or lives in the *last* token_count
//! event (Codex). The values `SessionMeta.tokens` carries right after
//! `scan_all` are therefore wrong: missing most of the usage.
//!
//! This task runs `parse_meta_full` for each known session, caches the
//! result by `(path, mtime)` so unchanged files are skipped on subsequent
//! passes, and writes the authoritative tokens / message_count back into
//! `AppState.sessions`. fs_watch signals us over `token_dirty` whenever a
//! file changes so updates propagate within ~1s; a 5s safety-net ticker
//! runs as a backstop.
//!
//! **Ownership**: this task is the *sole writer* for `SessionMeta.tokens`
//! and `SessionMeta.message_count`. fs_watch explicitly preserves those
//! fields when replacing metadata, so fast-parse noise never reaches the
//! Dashboard.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use futures::stream::{self, StreamExt};
use parking_lot::RwLock;
use tokio::sync::Notify;
use tokio::time::interval;

use crate::adapter::types::TokenStats;
use crate::adapter::{adapter_for_path, DynAdapter};
use crate::app::AppState;

const CONCURRENCY: usize = 16;
/// Safety-net interval. Event-driven refreshes fire whenever fs_watch pings
/// `token_dirty`, but this ticker still runs periodically so we don't drift
/// if a signal is ever dropped and so newly-opened sessions that predate the
/// last signal still get picked up.
const RESCAN_INTERVAL: Duration = Duration::from_secs(5);

#[derive(Debug, Default)]
pub struct TokenCache {
    inner: RwLock<HashMap<PathBuf, CacheEntry>>,
}

#[derive(Debug, Clone)]
struct CacheEntry {
    mtime: SystemTime,
    tokens: TokenStats,
    message_count: usize,
}

impl TokenCache {
    pub fn new() -> Self {
        Self::default()
    }

    fn get_if_fresh(&self, path: &Path, mtime: SystemTime) -> Option<(TokenStats, usize)> {
        let map = self.inner.read();
        let entry = map.get(path)?;
        if entry.mtime == mtime {
            Some((entry.tokens.clone(), entry.message_count))
        } else {
            None
        }
    }

    fn insert(&self, path: PathBuf, mtime: SystemTime, tokens: TokenStats, message_count: usize) {
        self.inner.write().insert(
            path,
            CacheEntry {
                mtime,
                tokens,
                message_count,
            },
        );
    }

    pub fn remove(&self, path: &Path) {
        self.inner.write().remove(path);
    }
}

pub async fn run(
    adapters: Vec<DynAdapter>,
    state: Arc<RwLock<AppState>>,
    cache: Arc<TokenCache>,
    token_dirty: Arc<Notify>,
    dirty: Arc<Notify>,
) {
    tracing::info!("token_refresh: starting, first-pass sweep begins");
    let t0 = std::time::Instant::now();
    let updated = refresh_all(&adapters, &state, &cache).await;
    tracing::info!(
        updated,
        elapsed_ms = t0.elapsed().as_millis() as u64,
        "token_refresh: first pass done"
    );
    if updated > 0 {
        dirty.notify_one();
    }

    let mut ticker = interval(RESCAN_INTERVAL);
    ticker.tick().await; // discard immediate tick

    loop {
        // Either the ticker fires (safety net) or fs_watch reports a file
        // change. Notify collapses multiple signals into a single permit, so
        // a burst of file events triggers just one refresh pass.
        let reason = tokio::select! {
            _ = ticker.tick() => "ticker",
            _ = token_dirty.notified() => "signal",
        };
        let t0 = std::time::Instant::now();
        let updated = refresh_all(&adapters, &state, &cache).await;
        if updated > 0 {
            tracing::info!(
                reason,
                updated,
                elapsed_ms = t0.elapsed().as_millis() as u64,
                "token_refresh: pass done"
            );
            dirty.notify_one();
        } else {
            tracing::debug!(reason, "token_refresh: pass done, no changes");
        }
    }
}

async fn refresh_all(
    adapters: &[DynAdapter],
    state: &Arc<RwLock<AppState>>,
    cache: &TokenCache,
) -> usize {
    let paths: Vec<PathBuf> = state
        .read()
        .sessions
        .iter()
        .map(|m| m.path.clone())
        .collect();

    stream::iter(paths)
        .map(|path| async move { usize::from(refresh_one(adapters, state, cache, &path).await) })
        .buffer_unordered(CONCURRENCY)
        .fold(0usize, |acc, n| async move { acc + n })
        .await
}

async fn refresh_one(
    adapters: &[DynAdapter],
    state: &Arc<RwLock<AppState>>,
    cache: &TokenCache,
    path: &Path,
) -> bool {
    // Pre-parse stat: lets us short-circuit on cache hits without touching
    // the (potentially multi-MB) file. Post-parse stat below is what we key
    // the cache on — writing the *pre-parse* mtime would be unsound when the
    // file was modified during our read, causing subsequent lookups to miss
    // and tokens to flicker.
    let mtime_before = match tokio::fs::metadata(path).await {
        Ok(md) => match md.modified() {
            Ok(t) => Some(t),
            Err(_) => return false,
        },
        Err(_) => None,
    };

    if let Some(mtime) = mtime_before {
        if let Some((tokens, count)) = cache.get_if_fresh(path, mtime) {
            return write_back(state, path, tokens, count);
        }
    }

    let Some(adapter) = adapter_for_path(adapters, path).cloned() else {
        return false;
    };

    // Virtual paths (e.g. OpenCode's .json stubs) have no real file on disk.
    // Skip the metadata check for adapters that don't need it.
    if mtime_before.is_none() && adapter.needs_fs_stat() {
        return false;
    }

    let parsed = match adapter.parse_meta_full(path).await {
        Ok(m) => m,
        Err(err) => {
            tracing::debug!(path = %path.display(), ?err, "token refresh skipped");
            return false;
        }
    };

    // Re-stat after parsing. If the file was appended to during our read, we
    // now either see mtime_before (file stable during parse) or a later
    // value. Either way the token counts we hold are at least as fresh as
    // this post-parse mtime — keying the cache by it guarantees the next
    // fs_watch-triggered lookup matches.
    if let Some(mtime) = mtime_before {
        let mtime_after = tokio::fs::metadata(path)
            .await
            .ok()
            .and_then(|m| m.modified().ok())
            .unwrap_or(mtime);
        cache.insert(
            path.to_path_buf(),
            mtime_after,
            parsed.tokens.clone(),
            parsed.message_count,
        );
    }
    write_back(state, path, parsed.tokens, parsed.message_count)
}

/// Update `sessions[i]` in place. Returns `true` if anything actually
/// changed — lets the caller avoid spurious redraws. Token totals are
/// monotone-nondecreasing per session within a process lifetime, so we also
/// refuse to overwrite with a *smaller* number: a transient parse that
/// failed to see the full file shouldn't be allowed to roll tokens back.
fn write_back(
    state: &Arc<RwLock<AppState>>,
    path: &Path,
    tokens: TokenStats,
    message_count: usize,
) -> bool {
    let mut s = state.write();
    let Some(meta) = s.sessions.iter_mut().find(|m| m.path == path) else {
        tracing::debug!(path = %path.display(), "write_back: path not in sessions, skipping");
        return false;
    };
    let old_total = meta.tokens.total();
    let new_total = tokens.total();
    // Guard against regressions: Claude/Codex sessions only grow (append-only
    // JSONL), so any new total should be >= the old total. If we compute a
    // smaller number, the file was likely truncated / rotated or we raced —
    // either way, keep the old value and let the next pass reconcile.
    if new_total < old_total && old_total > 0 {
        tracing::debug!(
            path = %path.display(),
            new = new_total,
            old = old_total,
            "write_back: rejecting regression"
        );
        return false;
    }
    let tokens_changed = meta.tokens.total() != tokens.total()
        || meta.tokens.input != tokens.input
        || meta.tokens.output != tokens.output
        || meta.tokens.cache_read != tokens.cache_read
        || meta.tokens.cache_creation != tokens.cache_creation;
    let count_changed = meta.message_count != message_count;
    if !tokens_changed && !count_changed {
        return false;
    }
    meta.tokens = tokens;
    meta.message_count = message_count;
    s.dirty = true;
    tracing::info!(
        path = %path.display(),
        old = old_total,
        new = new_total,
        delta = new_total as i128 - old_total as i128,
        "write_back: accepted"
    );
    true
}
