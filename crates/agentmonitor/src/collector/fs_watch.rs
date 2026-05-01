//! Filesystem watcher (P4). Notify-backed, with a 10s fallback rescan.
//!
//! **Token ownership**: this module only handles session metadata
//! (id/cwd/updated_at/size/status/model). Tokens and message_count are
//! written exclusively by `collector::token_refresh`; fs_watch preserves
//! whatever values are already in `AppState.sessions` for a given path
//! instead of letting fast-parse clobber them. Fast-parse can't compute
//! real totals anyway (it reads at most a handful of header lines), so
//! overwriting would cause the Dashboard to flash back to near-zero every
//! time the active session is written to.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use notify::{recommended_watcher, Event, EventKind, RecursiveMode, Watcher};
use parking_lot::RwLock;
use tokio::sync::mpsc::unbounded_channel;
use tokio::sync::Notify;
use tokio::time::{interval, MissedTickBehavior};

use crate::adapter::types::{SessionMeta, TokenStats};
use crate::adapter::DynAdapter;
use crate::app::AppState;
use crate::collector::diagnostics::DiagnosticsStore;
use crate::collector::token_refresh::TokenCache;

/// How long a path must stay quiet (no new Modify events) before we re-parse
/// it. Tuned to absorb a typical editor's "burst write" without making the
/// dashboard feel laggy on real changes.
const DEBOUNCE_QUIET: Duration = Duration::from_millis(50);
/// How often we sweep the pending map looking for paths that have hit their
/// quiet window. Should be small enough that a single `Modify` doesn't sit
/// in the queue much longer than `DEBOUNCE_QUIET`, but large enough that
/// idle minutes don't burn CPU on no-op sweeps.
const DEBOUNCE_SWEEP: Duration = Duration::from_millis(30);

/// Background task: owns a notify watcher + fallback ticker.
pub async fn run(
    adapters: Vec<DynAdapter>,
    state: Arc<RwLock<AppState>>,
    token_dirty: Arc<Notify>,
    dirty: Arc<Notify>,
) {
    run_with_cache(adapters, state, None, None, token_dirty, dirty).await
}

/// Variant that also has a `TokenCache` handle so reconcile can prune cache
/// entries for sessions that have disappeared without an explicit Remove
/// event (CLAUDE.md §12 / unbounded growth concern), and an optional
/// `DiagnosticsStore` for the Settings tab's runtime instrumentation panel.
/// Kept as a separate entry point so existing callers in tests don't need
/// the extra arguments.
pub async fn run_with_cache(
    adapters: Vec<DynAdapter>,
    state: Arc<RwLock<AppState>>,
    cache: Option<Arc<TokenCache>>,
    diagnostics: Option<Arc<DiagnosticsStore>>,
    token_dirty: Arc<Notify>,
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
            tracing::warn!(
                ?err,
                "notify watcher unavailable; falling back to polling only"
            );
            return fallback_only(adapters, state, token_dirty, dirty).await;
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

    // Path-aware debounce: a Modify event only inserts/refreshes a `last_seen`
    // entry. The sweep ticker drains entries that have been quiet for
    // `DEBOUNCE_QUIET`, processing each path exactly once regardless of how
    // many notify events landed in that window. Cheaper than the old
    // `sleep(50ms)` per event when an editor fires hundreds of bursts.
    let mut pending: HashMap<PathBuf, Instant> = HashMap::new();
    let mut sweep = interval(DEBOUNCE_SWEEP);
    sweep.set_missed_tick_behavior(MissedTickBehavior::Skip);

    loop {
        tokio::select! {
            Some(event) = ev_rx.recv() => {
                if let Some(d) = diagnostics.as_ref() {
                    d.record_fs_event();
                }
                // Remove fires immediately — it's idempotent and we want the
                // session gone from the UI right now. Create/Modify go into
                // the debounce queue.
                let mut any_remove = false;
                for path in event.paths {
                    let path = normalize_fs_path(path);
                    match event.kind {
                        EventKind::Create(_) | EventKind::Modify(_) => {
                            pending.insert(path, Instant::now());
                        }
                        EventKind::Remove(_) => {
                            let mut s = state.write();
                            if s.remove_session_path(&path) {
                                any_remove = true;
                            }
                        }
                        _ => {}
                    }
                }
                if any_remove {
                    dirty.notify_one();
                    token_dirty.notify_one();
                }
            }
            _ = sweep.tick() => {
                let ready = drain_ready(&mut pending, DEBOUNCE_QUIET);
                if ready.is_empty() {
                    continue;
                }
                let mut any = false;
                for path in ready {
                    if let Some(d) = diagnostics.as_ref() {
                        d.record_fs_path_processed();
                    }
                    if update_for_path(path, &adapters, &state).await {
                        any = true;
                    }
                }
                if any {
                    dirty.notify_one();
                    token_dirty.notify_one();
                }
            }
            _ = reconcile.tick() => {
                if let Some(d) = diagnostics.as_ref() {
                    d.record_fs_reconcile();
                }
                let mut fresh = Vec::new();
                for adapter in &adapters {
                    if let Ok(mut batch) = adapter.scan_all().await {
                        fresh.append(&mut batch);
                    }
                }
                fresh.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
                replace_preserving_tokens(&state, fresh);
                if let Some(cache) = cache.as_ref() {
                    let live: std::collections::HashSet<PathBuf> = state
                        .read()
                        .sessions
                        .iter()
                        .map(|m| m.path.clone())
                        .collect();
                    cache.retain_paths(&live);
                }
                dirty.notify_one();
                token_dirty.notify_one();
            }
            else => break,
        }
    }
}

/// Pull every path whose `last_seen` is at least `quiet` ago out of the
/// pending map. Returned paths are removed from the map so they don't get
/// re-processed. Pure function over the map so we can unit-test the timing
/// math without standing up the whole watcher.
pub(crate) fn drain_ready(
    pending: &mut HashMap<PathBuf, Instant>,
    quiet: Duration,
) -> Vec<PathBuf> {
    let now = Instant::now();
    let mut ready = Vec::new();
    pending.retain(|path, last_seen| {
        if now.duration_since(*last_seen) >= quiet {
            ready.push(path.clone());
            false
        } else {
            true
        }
    });
    ready
}

/// Process one path that has been quiet long enough. The debounce / quiet
/// window is now enforced at the call site via `drain_ready`; this function
/// itself does no waiting. Returns `true` if it actually mutated state, so
/// the caller can decide whether the dirty / token_dirty notifies are worth
/// firing.
async fn update_for_path(
    path: PathBuf,
    adapters: &[DynAdapter],
    state: &Arc<RwLock<AppState>>,
) -> bool {
    let Some(adapter) = adapters.iter().find(|a| a.owns_path(&path)) else {
        return false;
    };
    let meta = match adapter.parse_meta_fast(&path).await {
        Ok(m) => m,
        Err(err) => {
            tracing::debug!(path = %path.display(), ?err, "fast parse failed");
            return false;
        }
    };

    let mut s = state.write();
    s.mutate_sessions(|sessions| {
        if let Some(existing) = sessions.iter_mut().find(|m| m.path == path) {
            // KEEP tokens + message_count. They are owned by token_refresh; if we
            // let fast-parse noise through here the Dashboard would flash back to
            // a header-only subtotal every time the active session is appended.
            let kept_tokens = std::mem::take(&mut existing.tokens);
            let kept_count = existing.message_count;
            *existing = meta;
            existing.tokens = kept_tokens;
            existing.message_count = kept_count;
            tracing::debug!(path = %path.display(), "fs_watch: updated existing session");
        } else {
            // Diagnostic: dump any sessions with the same filename so we can see
            // exactly how the event path differs from stored paths. If this fires
            // it means some upstream representation (symlink form, Unicode
            // normalization, trailing slash, etc.) still isn't being normalized.
            let filename = path.file_name().map(|n| n.to_os_string());
            let similar: Vec<String> = sessions
                .iter()
                .filter(|m| m.path.file_name().map(|n| n.to_os_string()) == filename)
                .map(|m| m.path.display().to_string())
                .take(3)
                .collect();
            tracing::warn!(
                event_path = %path.display(),
                similar_in_state = ?similar,
                sessions_count = sessions.len(),
                "fs_watch: event path did not match any stored session — tracking as new"
            );
            // Brand new session — zero the fast-parse values explicitly. They'd
            // either be 0 already (typical Claude/Codex sessions whose headers
            // hold no usage) or a tiny sliver that doesn't reflect reality; either
            // way token_refresh's first pass on this path will fill in truth.
            let mut fresh = meta;
            fresh.tokens = TokenStats::default();
            fresh.message_count = 0;
            tracing::info!(path = %path.display(), "fs_watch: new session tracked");
            sessions.push(fresh);
        }
        sessions.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    });
    s.dirty = true;
    true
}

/// Merge `fresh` into `sessions`, preserving tokens/message_count by path
/// and — crucially — carrying over any session that exists in `state` but is
/// missing from `fresh`. Fast-parse is not a deletion oracle: one failed
/// `parse_meta_fast` (EMFILE race, momentary EOF mid-`/compact`, a reader
/// hitting a partial line during active writes) would drop a session from
/// `fresh`, and a replace-based reconcile would then evict it until the
/// next Modify event pushed it back in. That flap was visible as Top
/// Projects `latest` jumping to the *second-newest* session's timestamp
/// every ~10s. File removal is handled authoritatively via fs_watch's
/// `EventKind::Remove` branch; reconcile only adds/updates, never deletes.
pub(crate) fn replace_preserving_tokens(
    state: &Arc<RwLock<AppState>>,
    mut fresh: Vec<SessionMeta>,
) {
    // Normalize on the way in so every stored SessionMeta uses the
    // `/Users/...` form even if something (e.g. a symlinked config dir) fed
    // us a `/System/Volumes/Data/...` path.
    for meta in &mut fresh {
        meta.path = normalize_fs_path(std::mem::take(&mut meta.path));
    }
    let mut s = state.write();
    let new_arc = {
        // Drain prev into a by-path map so we can (a) look up tokens to
        // preserve, and (b) harvest the sessions that fresh didn't report.
        // The `make_mut` call gives us an in-place mutable Vec when we own
        // the only Arc, or copies-on-write when other holders exist (e.g.
        // a dashboard render in flight).
        let inner = std::mem::take(&mut s.sessions);
        let prev_vec: Vec<SessionMeta> = match Arc::try_unwrap(inner) {
            Ok(v) => v,
            Err(arc) => (*arc).clone(),
        };
        let mut prev: HashMap<PathBuf, SessionMeta> =
            prev_vec.into_iter().map(|m| (m.path.clone(), m)).collect();

        let mut merged: Vec<SessionMeta> = Vec::with_capacity(prev.len().max(fresh.len()));
        let mut preserved = 0usize;
        let mut new_paths = 0usize;
        for mut meta in fresh {
            if let Some(existing) = prev.remove(&meta.path) {
                meta.tokens = existing.tokens;
                meta.message_count = existing.message_count;
                preserved += 1;
            } else {
                meta.tokens = TokenStats::default();
                meta.message_count = 0;
                new_paths += 1;
            }
            merged.push(meta);
        }
        // Anything still in `prev` was in state but absent from fresh. Keep it
        // — see the doc comment above for why this is the right call.
        let carried = prev.len();
        for (_, meta) in prev {
            merged.push(meta);
        }
        tracing::info!(
            preserved,
            new_paths,
            carried,
            total = merged.len(),
            "fs_watch: reconcile merged sessions"
        );
        Arc::new(merged)
    };
    s.sessions = new_arc;
    s.session_generation = s.session_generation.wrapping_add(1);
    s.dirty = true;
}

/// macOS firmlinks map `/System/Volumes/Data/Users/...` ↔ `/Users/...` to the
/// same inode, but `std::fs::canonicalize` keeps them distinct. FSEvents
/// (what `notify` uses on macOS) sometimes emits the long form while
/// `WalkDir` always returns the short form — which would make
/// `m.path == event.path` *always false* for the active session, causing
/// every fs_watch event to `push` a duplicate entry and the Dashboard to
/// flicker between correct and doubled-then-preserved-with-zeros values.
///
/// Strip the firmlink prefix here so all stored paths use the canonical
/// `/Users/...` form that `scan_all` and `App::initial_scan` produce. The
/// function is a no-op on non-macOS and on paths that don't carry the
/// prefix.
fn normalize_fs_path(path: PathBuf) -> PathBuf {
    const FIRMLINK_PREFIX: &str = "/System/Volumes/Data";
    let Some(s) = path.to_str() else {
        return path;
    };
    if let Some(rest) = s.strip_prefix(FIRMLINK_PREFIX) {
        if rest.starts_with('/') {
            return PathBuf::from(rest);
        }
    }
    path
}

/// Poll-only fallback when notify isn't available (e.g. in restricted sandboxes).
async fn fallback_only(
    adapters: Vec<DynAdapter>,
    state: Arc<RwLock<AppState>>,
    token_dirty: Arc<Notify>,
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
        replace_preserving_tokens(&state, fresh);
        dirty.notify_one();
        token_dirty.notify_one();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::types::SessionStatus;
    use std::path::{Path, PathBuf};

    fn sess(path: &str, input: u64, msg_count: usize) -> SessionMeta {
        SessionMeta {
            agent: "claude",
            id: "abc".into(),
            path: PathBuf::from(path),
            cwd: None,
            model: None,
            version: None,
            git_branch: None,

            source: None,
            started_at: None,
            updated_at: None,
            message_count: msg_count,
            tokens: TokenStats {
                input,
                output: 0,
                cache_read: 0,
                cache_creation: 0,
            },
            status: SessionStatus::Unknown,
            byte_offset: 0,
            size_bytes: 0,
        }
    }

    #[test]
    fn replace_preserves_tokens_when_fresh_is_zero() {
        // token_refresh previously wrote 1M tokens for this session. A new
        // reconcile tick comes in with fast-parse (tokens = 0). The merge
        // must not roll the Dashboard back to zero — this is the regression
        // the user reported ("jumps up then falls back to a very old value").
        let state = Arc::new(RwLock::new(AppState::default()));
        state.write().sessions = Arc::new(vec![sess("/a", 1_000_000, 42)]);

        let fresh = vec![sess("/a", 0, 0)];
        replace_preserving_tokens(&state, fresh);

        let s = state.read();
        assert_eq!(s.sessions[0].tokens.input, 1_000_000);
        assert_eq!(s.sessions[0].message_count, 42);
    }

    #[test]
    fn replace_adopts_new_session_tokens_from_scratch() {
        // A new session path that wasn't in state before. We have no prior
        // tokens to preserve, so whatever fast-parse produced (usually 0)
        // should be kept — token_refresh fills it in on the next pass.
        let state = Arc::new(RwLock::new(AppState::default()));
        let fresh = vec![sess("/new", 0, 0)];
        replace_preserving_tokens(&state, fresh);

        let s = state.read();
        assert_eq!(s.sessions.len(), 1);
        assert_eq!(s.sessions[0].tokens.input, 0);
    }

    #[test]
    fn replace_zeros_fast_parse_tokens_for_new_paths() {
        // Fast-parse tokens from scan_all are only a header subtotal. When
        // they reach replace_preserving_tokens as "new" paths, they must be
        // zeroed so the Dashboard never shows partial truth. token_refresh
        // will fill in real values on its next pass.
        let state = Arc::new(RwLock::new(AppState::default()));
        let fresh = vec![sess("/fresh", 500, 1)];
        replace_preserving_tokens(&state, fresh);

        let s = state.read();
        assert_eq!(
            s.sessions[0].tokens.input, 0,
            "fast-parse tokens must be dropped"
        );
        assert_eq!(s.sessions[0].message_count, 0);
    }

    #[test]
    fn replace_prefers_prior_tokens_even_when_fresh_claims_more() {
        // If fast-parse somehow reports a larger number than token_refresh
        // did, we still prefer token_refresh's value — fast-parse can only
        // see the header, so anything beyond that is noise. Regressions are
        // better than false inflation here.
        let state = Arc::new(RwLock::new(AppState::default()));
        state.write().sessions = Arc::new(vec![sess("/a", 100, 5)]);

        let fresh = vec![sess("/a", 9_999_999, 999)];
        replace_preserving_tokens(&state, fresh);

        let s = state.read();
        assert_eq!(s.sessions[0].tokens.input, 100);
        assert_eq!(s.sessions[0].message_count, 5);
    }

    #[test]
    fn reconcile_carries_forward_sessions_missing_from_fresh() {
        // Regression: a transient parse_meta_fast failure (FD pressure,
        // partial read mid-write, /compact in progress) can leave `fresh`
        // short one or more sessions that really do exist. Before, reconcile
        // did a full replace and the missing session disappeared from state
        // until the next notify Modify event pushed it back, causing Top
        // Projects ages to flip to the *second-newest* session every ~10s.
        // Now reconcile is a merge: scan_all adds/updates only, deletion is
        // the sole job of EventKind::Remove.
        let state = Arc::new(RwLock::new(AppState::default()));
        state.write().sessions = Arc::new(vec![
            sess("/a", 1_000, 10),
            sess("/b", 2_000, 20), // flaky: scan_all failed to parse this pass
            sess("/c", 3_000, 30),
        ]);

        let fresh = vec![sess("/a", 0, 0), sess("/c", 0, 0)]; // /b missing
        replace_preserving_tokens(&state, fresh);

        let s = state.read();
        let paths: Vec<_> = s
            .sessions
            .iter()
            .map(|m| m.path.display().to_string())
            .collect();
        assert!(paths.contains(&"/a".to_string()));
        assert!(
            paths.contains(&"/b".to_string()),
            "missing-from-fresh session must be carried forward"
        );
        assert!(paths.contains(&"/c".to_string()));
        let b = s
            .sessions
            .iter()
            .find(|m| m.path == Path::new("/b"))
            .unwrap();
        assert_eq!(
            b.tokens.input, 2_000,
            "carried-forward session keeps its prior tokens"
        );
        assert_eq!(b.message_count, 20);
    }

    #[test]
    fn normalize_fs_path_strips_macos_firmlink_prefix() {
        let long = PathBuf::from("/System/Volumes/Data/Users/yjw/.claude/projects/x.jsonl");
        let short = PathBuf::from("/Users/yjw/.claude/projects/x.jsonl");
        assert_eq!(normalize_fs_path(long), short);
    }

    #[test]
    fn normalize_fs_path_leaves_non_firmlink_unchanged() {
        let p = PathBuf::from("/Users/yjw/.claude/projects/x.jsonl");
        assert_eq!(normalize_fs_path(p.clone()), p);
        let p = PathBuf::from("/tmp/foo");
        assert_eq!(normalize_fs_path(p.clone()), p);
        // Defensive: a path that merely starts with the same characters but
        // isn't actually under the firmlink boundary must not be rewritten.
        let p = PathBuf::from("/System/Volumes/Datastore/x");
        assert_eq!(normalize_fs_path(p.clone()), p);
    }

    #[test]
    fn drain_ready_returns_only_paths_past_quiet_window() {
        use std::time::Duration;
        let quiet = Duration::from_millis(20);
        let mut pending: HashMap<PathBuf, Instant> = HashMap::new();
        let now = Instant::now();
        // /a is old (50ms ago), /b is fresh (just now). Only /a should drain.
        pending.insert(PathBuf::from("/a"), now - Duration::from_millis(50));
        pending.insert(PathBuf::from("/b"), now);

        let drained = drain_ready(&mut pending, quiet);
        assert_eq!(drained, vec![PathBuf::from("/a")]);
        assert_eq!(pending.len(), 1);
        assert!(pending.contains_key(&PathBuf::from("/b")));
    }

    #[test]
    fn drain_ready_dedups_bursts_into_one_path() {
        // The whole point of the path-aware debounce: 100 modifies on the
        // same path inside the quiet window must result in *one* drained
        // entry, not 100. The map key uniqueness handles dedup; this test
        // pins down that contract.
        use std::time::Duration;
        let mut pending: HashMap<PathBuf, Instant> = HashMap::new();
        let stale = Instant::now() - Duration::from_millis(50);
        for _ in 0..100 {
            pending.insert(PathBuf::from("/burst"), stale);
        }
        let drained = drain_ready(&mut pending, Duration::from_millis(20));
        assert_eq!(drained.len(), 1);
        assert_eq!(drained[0], PathBuf::from("/burst"));
    }

    #[test]
    fn drain_ready_empty_map_is_noop() {
        let mut pending: HashMap<PathBuf, Instant> = HashMap::new();
        let drained = drain_ready(&mut pending, std::time::Duration::from_millis(10));
        assert!(drained.is_empty());
    }
}
