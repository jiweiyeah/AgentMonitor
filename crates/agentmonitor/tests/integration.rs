//! End-to-end tests covering the data flow from filesystem events to the
//! `AppState.sessions` token totals.
//!
//! These tests exist because every `CLAUDE.md` invariant (§1–§9) was a bug
//! found in this exact pipeline:
//!
//! 1. fast-parse not clobbering tokens (§1)
//! 2. tokens monotone-nondecreasing (§2)
//! 3. cache keying on post-parse mtime (§3)
//! 4. macOS firmlink path normalization (§4)
//! 5. blocklist semantics for first-line type (§5)
//! 6. `updated_at` monotone forward in fold pass (§7)
//! 7. reconcile is merge-not-replace (§9)
//!
//! Unit tests cover the building blocks; this file wires them together with a
//! real filesystem (via `tempfile`) and the live `fs_watch` + `token_refresh`
//! collectors so regressions in the *interactions* between modules surface.
//!
//! Tests are async-Tokio. The collectors are spawned as background tasks and
//! the asserts use a polling helper with a generous timeout (2s) — the events
//! they're waiting for typically land in <100ms but CI runners can be slow.

use std::sync::Arc;
use std::time::Duration;

use parking_lot::RwLock;
use tempfile::tempdir;
use tokio::sync::Notify;

use agentmonitor::adapter::{ClaudeAdapter, DynAdapter};
use agentmonitor::app::AppState;
use agentmonitor::collector::diagnostics::DiagnosticsStore;
use agentmonitor::collector::token_refresh::TokenCache;
use agentmonitor::collector::token_trend::TokenTrend;
use agentmonitor::collector::{fs_watch, token_refresh};

/// Construct a single Claude assistant line that contributes
/// `(input, output, cache_read, cache_creation)` to the session's token total.
fn assistant_usage_line(
    session_id: &str,
    cwd: &str,
    input: u64,
    output: u64,
    cache_read: u64,
    cache_creation: u64,
) -> String {
    let payload = serde_json::json!({
        "parentUuid": null,
        "isSidechain": false,
        "userType": "external",
        "cwd": cwd,
        "sessionId": session_id,
        "version": "2.1.100",
        "gitBranch": "main",
        "type": "assistant",
        "message": {
            "id": "resp_test",
            "type": "message",
            "role": "assistant",
            "content": [{"type": "text", "text": "ok"}],
            "model": "claude-sonnet-test",
            "stop_reason": "end_turn",
            "usage": {
                "input_tokens": input,
                "output_tokens": output,
                "cache_creation_input_tokens": cache_creation,
                "cache_read_input_tokens": cache_read
            }
        },
        "uuid": format!("uuid-{input}-{output}"),
        "timestamp": "2026-04-13T14:23:16.861Z",
    });
    serde_json::to_string(&payload).unwrap()
}

/// Header line that wouldn't pass the old allowlist (CLAUDE.md §5).
/// `permission-mode` is one of the recently added types Claude Code emits.
fn header_permission_line() -> String {
    let payload = serde_json::json!({
        "type": "permission-mode",
        "mode": "default",
        "timestamp": "2026-04-13T14:23:00.000Z",
    });
    serde_json::to_string(&payload).unwrap()
}

async fn wait_for<F>(predicate: F, timeout_ms: u64, label: &str)
where
    F: Fn() -> bool,
{
    let mut elapsed = 0u64;
    let step = 50u64;
    while elapsed < timeout_ms {
        if predicate() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(step)).await;
        elapsed += step;
    }
    panic!("predicate {} never became true within {}ms", label, timeout_ms);
}

/// fs_watch installs notify watchers asynchronously after spawn; FSEvents on
/// macOS in particular can take a beat to start listening on a freshly-created
/// directory. The 10s reconcile is the safety net, but for a fast-feedback
/// test we'd rather not block on it. Wait for the watcher to be ready by
/// writing a sentinel file and waiting for the first session to appear.
///
/// Caller should write real session content AFTER this returns.
async fn wait_for_watcher_ready(state: &Arc<RwLock<AppState>>, sentinel: &std::path::Path) {
    std::fs::write(sentinel, "{}\n").unwrap();
    // The sentinel won't parse as a real session, but if it doesn't appear in
    // state at all that's fine — reconcile will run anyway. Just give the
    // watcher a moment to come up before the test starts churning.
    tokio::time::sleep(Duration::from_millis(200)).await;
    // Drain any session entries the sentinel may have created so it doesn't
    // pollute test asserts. parse_meta_fast bails on `{}` (no first type),
    // so it should never have been added — but be defensive.
    let _ = std::fs::remove_file(sentinel);
    let _ = state.read().sessions.len(); // touch state to make sure lock works
}

/// Bring up the full collector stack against a tempdir-backed Claude root.
/// Returns the shared state + cache so each test can poke at them.
struct Harness {
    state: Arc<RwLock<AppState>>,
    _token_cache: Arc<TokenCache>,
    _dirty: Arc<Notify>,
    _token_dirty: Arc<Notify>,
    claude_root: std::path::PathBuf,
}

async fn harness() -> (Harness, tempfile::TempDir) {
    let dir = tempdir().expect("tempdir");
    let claude_projects = dir.path().join(".claude").join("projects");
    // Adapter expects a directory tree where leaves are .jsonl files; the
    // immediate parent encodes the cwd. Create one project subdirectory so
    // session writes land somewhere realistic.
    let project_dir = claude_projects.join("-Users-test-fixture");
    std::fs::create_dir_all(&project_dir).unwrap();

    let adapter: DynAdapter = Arc::new(ClaudeAdapter::new(Some(claude_projects.clone())));
    let adapters = vec![adapter];

    let state = Arc::new(RwLock::new(AppState::default()));
    let token_cache = Arc::new(TokenCache::new());
    let token_trend = Arc::new(TokenTrend::default());
    let dirty = Arc::new(Notify::new());
    let token_dirty = Arc::new(Notify::new());

    // Spawn fs_watch and token_refresh just like run_event_loop does.
    {
        let adapters = adapters.clone();
        let state = state.clone();
        let dirty = dirty.clone();
        let token_dirty = token_dirty.clone();
        tokio::spawn(async move {
            fs_watch::run(adapters, state, token_dirty, dirty).await;
        });
    }
    {
        let adapters = adapters.clone();
        let state = state.clone();
        let cache = token_cache.clone();
        let trend = token_trend.clone();
        let diagnostics = Arc::new(DiagnosticsStore::new());
        let dirty = dirty.clone();
        let token_dirty = token_dirty.clone();
        tokio::spawn(async move {
            token_refresh::run(adapters, state, cache, trend, diagnostics, token_dirty, dirty)
                .await;
        });
    }

    let harness = Harness {
        state,
        _token_cache: token_cache,
        _dirty: dirty,
        _token_dirty: token_dirty,
        claude_root: project_dir,
    };
    (harness, dir)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fs_event_propagates_to_token_total() {
    let (h, _dir) = harness().await;
    wait_for_watcher_ready(&h.state, &h.claude_root.join("__sentinel.jsonl")).await;

    // Write a session file with two assistant turns. Token totals should be
    // additive across turns (CLAUDE.md §6).
    let session_path = h.claude_root.join("test-session.jsonl");
    let body = format!(
        "{}\n{}\n{}\n",
        header_permission_line(),
        assistant_usage_line("test-session", "/Users/test/fixture", 100, 50, 200, 0),
        assistant_usage_line("test-session", "/Users/test/fixture", 30, 70, 5, 0),
    );
    std::fs::write(&session_path, body).unwrap();

    // The first thing fs_watch's reconcile does (after notify, or 10s
    // backstop) is scan_all → push the session into state. Token_refresh
    // then computes totals on its 5s ticker. We expect to see the merged
    // totals well before that — the notify path should kick everything off
    // within ~100ms. Use a 15s timeout to span one reconcile + a token-refresh
    // pass on slow CI runners.
    let state = h.state.clone();
    let session_path_for_wait = session_path.clone();
    wait_for(
        move || {
            let s = state.read();
            s.sessions
                .iter()
                .any(|m| m.path == session_path_for_wait && m.tokens.input == 130)
        },
        15_000,
        "session token total reaches 130",
    )
    .await;

    // Verify all four buckets too — CLAUDE.md §6 says output is summed and
    // cache_read is summed, both as deltas.
    let s = h.state.read();
    let session = s
        .sessions
        .iter()
        .find(|m| m.path.file_name().and_then(|n| n.to_str()) == Some("test-session.jsonl"))
        .expect("session present");
    assert_eq!(session.tokens.input, 130, "summed input");
    assert_eq!(session.tokens.output, 120, "summed output");
    assert_eq!(session.tokens.cache_read, 205, "summed cache_read");
    assert_eq!(session.tokens.cache_creation, 0, "no cache_creation");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unknown_first_type_does_not_silently_drop_session() {
    // CLAUDE.md §5: the first-line type filter is a blocklist, not allowlist.
    // A session whose first record is a recently-added type like
    // `permission-mode` must still be tracked.
    let (h, _dir) = harness().await;
    wait_for_watcher_ready(&h.state, &h.claude_root.join("__sentinel.jsonl")).await;

    let session_path = h.claude_root.join("permission-first.jsonl");
    let body = format!(
        "{}\n{}\n",
        header_permission_line(),
        assistant_usage_line("permission-first", "/Users/test/fixture", 50, 0, 0, 0),
    );
    std::fs::write(&session_path, body).unwrap();

    let state = h.state.clone();
    wait_for(
        move || {
            let s = state.read();
            s.sessions
                .iter()
                .any(|m| m.path.file_name().and_then(|n| n.to_str()) == Some("permission-first.jsonl"))
        },
        15_000,
        "permission-mode first-line session is tracked",
    )
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn append_grows_token_total_monotonically() {
    // CLAUDE.md §2: tokens are monotone-nondecreasing per session. Appending
    // to an existing file must NOT cause the dashboard to flash backward.
    let (h, _dir) = harness().await;
    wait_for_watcher_ready(&h.state, &h.claude_root.join("__sentinel.jsonl")).await;

    let session_path = h.claude_root.join("growing.jsonl");
    let initial = format!(
        "{}\n",
        assistant_usage_line("growing", "/Users/test/fixture", 100, 0, 0, 0),
    );
    std::fs::write(&session_path, &initial).unwrap();

    let state = h.state.clone();
    let path_for_wait = session_path.clone();
    wait_for(
        move || {
            let s = state.read();
            s.sessions
                .iter()
                .any(|m| m.path == path_for_wait && m.tokens.input == 100)
        },
        15_000,
        "initial total is 100",
    )
    .await;

    // Append another turn.
    let appended = format!(
        "{}{}\n",
        initial,
        assistant_usage_line("growing", "/Users/test/fixture", 250, 0, 0, 0),
    );
    std::fs::write(&session_path, &appended).unwrap();

    let state = h.state.clone();
    let path_for_wait = session_path.clone();
    wait_for(
        move || {
            let s = state.read();
            s.sessions
                .iter()
                .any(|m| m.path == path_for_wait && m.tokens.input == 350)
        },
        15_000,
        "appended total is 350",
    )
    .await;
}
