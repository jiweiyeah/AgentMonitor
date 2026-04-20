# CLAUDE.md

Notes for future agents / contributors working on agent-monitor. Focused on
non-obvious invariants and the bugs we've already paid for once.

## Architecture at a glance

```
crates/agentmonitor/src/
  adapter/         per-agent session parsing
    claude.rs      Claude Code JSONL schema
    codex.rs       Codex rollout-*.jsonl schema
  collector/       background data sources
    fs_watch.rs    notify-backed file watcher + 10s reconcile fallback
    proc_sampler.rs ps-style process sampling
    token_refresh.rs full-parse token computation + (path, mtime) cache
  tui/             ratatui renderers (dashboard/sessions/process/viewer)
  app.rs           AppState (RwLock-guarded), App, SessionSort
  event.rs         event loop + key dispatch + Notify plumbing
```

Three background tasks feed `AppState` through `Arc<Notify>` signals:

- `proc_sampler` writes `MetricsStore`, notifies `dirty` → render.
- `fs_watch` writes `AppState.sessions` **metadata only** (id / cwd / mtime /
  size / status / model), notifies `dirty` and `token_dirty`.
- `token_refresh` writes `AppState.sessions` **tokens + message_count**,
  notifies `dirty`. Triggered by `token_dirty` (event-driven) plus a 5s
  safety-net ticker.

## Hard-won invariants

### 1. fast-parse results MUST NOT touch `tokens` or `message_count`

`parse_meta_fast` reads at most ~8 header lines. For Claude that's usually
`permission-mode` + `attachment` rows, *before* any assistant message with
a `usage` field — so fast-parse always returns ~0 tokens. For Codex it's
the single `session_meta` row, also 0 tokens. These values **are not the
truth**; they're a header subtotal.

`collector::token_refresh` is the **sole writer** for `tokens` /
`message_count`. `fs_watch::update_for_path` and
`fs_watch::replace_preserving_tokens` explicitly preserve whatever the
previous state held for a given path; `App::initial_scan` zeroes tokens on
fresh scan. If you ever feel tempted to merge fast-parse tokens into state
"just in case", remember the symptom is the Dashboard flashing back to a
header-sized fraction of the real total every 10s.

### 2. Tokens are monotone-nondecreasing per session

Claude/Codex JSONL is append-only. `token_refresh::write_back` rejects any
new total smaller than the existing one (when existing > 0). This protects
against transient partial reads when `parse_meta_full` races an active
writer and reaches EOF prematurely. Next pass catches up.

The one case this silently drops real data is `/compact`, which can
rewrite a session with a summary. Accept that trade; the alternative is
visible Dashboard flicker on every active-session write.

### 3. Cache `(path, mtime)` keys use **post-parse** mtime

`parse_meta_full` on a multi-MB file takes hundreds of ms. If the file is
being appended during the read, the pre-parse mtime is already stale by
the time parsing finishes. Keying the cache on pre-parse mtime means the
next fs_watch lookup uses a newer mtime and misses — causing needless
re-parses and, combined with the firmlink bug below, user-visible
oscillation. Stat again after `parse_meta_full` returns and use that.

### 4. macOS firmlinks break path equality

`/Users/yjw/.claude/...` and `/System/Volumes/Data/Users/yjw/.claude/...`
refer to the same inode on APFS but compare as different `PathBuf`s.
`std::fs::canonicalize` does **not** collapse them. WalkDir (used by
`scan_all`) emits the short form; `notify` on macOS sometimes emits the
long form. Without normalization, `sessions.iter().find(|m| m.path ==
event.path)` silently fails → fs_watch pushes a duplicate entry on every
modify → reconcile drops one form or the other → tokens oscillate.

Fix lives in `fs_watch::normalize_fs_path`: strip `/System/Volumes/Data`
prefix. Apply at every notify entry point (Create/Modify/Remove) and
defensively in `replace_preserving_tokens`. Not needed on Linux/Windows.

### 5. First-line type filter is a **blocklist**, not an allowlist

`adapter/claude.rs::is_native_first_type` used to allowlist
`summary | user | assistant | system | file-history-snapshot`. Claude Code
has since added `permission-mode`, `attachment`, `progress`,
`worktree-state` — ~20% of a real user's sessions were being silently
rejected by `parse_meta_fast`, including the one they were actively
chatting in. The Dashboard appeared frozen at a historical token total
because the active session literally wasn't in the list.

Invert: reject only known-non-session types (`queue-operation` from
claude-mem). Any other first type is treated as a real session. If
claude-mem adds new junk later, extend the blocklist.

**Pattern**: when the upstream format evolves faster than we can track
(CLI version bumps, new event records), allowlists fail open as silent
data loss. Prefer blocklists for known-bad when the universe of "good"
isn't enumerable.

### 6. Token accounting is agent-specific

- **Claude** `message.usage` is a **per-turn delta** — sum across turns.
  Three-level precedence per line: `message.usage` > `toolUseResult.usage`
  > `toolUseResult.totalTokens` (legacy, routes to input for non-assistant
  lines, output otherwise). Only one source wins per line to avoid double
  counting.
- **Codex** `event_msg.payload.info.total_token_usage` is **cumulative** —
  overwrite, don't sum. Mapping: `input_tokens - cached_input_tokens` →
  `input` (fresh input only), `cached_input_tokens` → `cache_read`,
  `output_tokens` → `output` (already includes `reasoning_output_tokens`,
  don't add). Codex doesn't expose cache creation — that bucket stays 0.
- **Dashboard Σ tokens** = `input + output + cache_read + cache_creation`.
  Cache reads typically dominate by 10-100× because Claude Code resends
  context every turn and most of it hits the prompt cache. The number is
  big but technically correct; unit is `K`/`M`/`B`.

## Debugging loop

`cargo run -p agentmonitor --release -- --debug` writes
`$XDG_CACHE_HOME/agent-monitor.log` (macOS:
`~/Library/Caches/dev.agentmonitor.agent-monitor/agent-monitor.log`). Key
info-level lines:

- `token_refresh: starting` / `token_refresh: first pass done updated=N` —
  confirms the background sweep ran.
- `token_refresh: pass done reason={ticker,signal} updated=N` — per-pass.
- `fs_watch: new session tracked path=...` — should fire **once per
  session**. If it fires repeatedly for the same path, either normalize is
  broken (§4) or the path is escaping state for some other reason.
- `fs_watch: reconcile replaced sessions preserved=N new_paths=M` — bulk
  sync after 10s. `new_paths > 0` long after startup means a new session
  file appeared that fs_watch missed via notify.
- `write_back: accepted path=... old=X new=Y delta=Z` — the authoritative
  "tokens changed for this path" record. If the user reports stuck
  totals, grep for the active session's path and look at deltas.

When token totals misbehave, the failure is almost always at one of:

1. `parse_meta_fast` rejecting the file → session missing from state (§5).
2. `parse_meta_full` returning 0 or too few tokens → adapter logic bug.
3. fs_watch clobbering `tokens` → broken preserve logic (§1).
4. notify path ≠ stored path → firmlink / case / Unicode normalization (§4).

Add structured info-level logs at the suspicious boundary and re-run —
two lines of evidence beats two hours of speculation.

## Test conventions

- Adapter parsing changes: add table-driven tests under
  `#[cfg(test)] mod tests` in the adapter file. Use `serde_json::json!` to
  construct fixtures; don't hand-roll JSON strings.
- fs_watch / token_refresh changes: test via `replace_preserving_tokens`
  and `write_back` helpers directly. They're `pub(crate)` or private with
  module-scope tests.
- Before shipping any change touching data flow: run `cargo test -p
  agentmonitor --lib && cargo clippy -p agentmonitor --all-targets --
  -D warnings`.

## CLI

```
agent-monitor [--once-and-exit] [--sample-interval SECS] [--debug]
```

`--once-and-exit` prints the session snapshot and exits — fastest way to
verify a parsing change hasn't regressed the visible-session count.
