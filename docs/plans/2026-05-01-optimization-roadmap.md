# Optimization Roadmap — 2026-05-01

This file is the canonical execution plan for the 18 optimization items
identified in the May 2026 review. Each entry has:

- **Why** — what user-visible / engineering pain it solves
- **Plan** — concrete implementation strategy
- **Files** — exact files touched
- **Tests** — what proves it works
- **Risk** — invariants we can violate if we're not careful (cross-ref CLAUDE.md §1–§9)

## Status Legend

- ✅ **Done** — landed in this session, tests + clippy clean
- ⏸ **Deferred** — design ready, implementation scoped for a follow-up commit
- ⬜ **Pending** — not yet started

## Summary

| # | Item | Status |
|---|---|---|
| #1 | Viewer 内搜索 (`/`) | ✅ |
| #2 | Token 趋势 sparkline | ✅ |
| #3 | 成本估算 | ✅ |
| #4 | Top Projects → Sessions filter | ✅ |
| #5 | Session 收藏 / 置顶 | ✅ |
| #6 | Help 弹窗 (`?`) | ✅ |
| #7 | 导出 / 复制 session | ⏸ |
| #8 | Sessions Arc 化 (perf) | ✅ |
| #9 | 缓存 dashboard aggregates | ⏸ |
| #10 | gh api 改 async 不阻塞 | ✅ |
| #11 | fs_watch path-aware debounce | ⏸ |
| #12 | TokenCache LRU + 上限 | ✅ |
| #13 | event.rs 拆分 (1952 行) | ⏸ |
| #14 | 端到端集成测试 | ✅ |
| #15 | 自身运行时指标 / Diagnostics | ⏸ |
| #16 | Panic hook | ✅ |
| #17 | Windows 终端启动支持 | ⏸ |
| #18 | 可配置 session roots (CLI flag) | ✅ |

**Tests**: 192 lib + 3 integration = 195 passing (baseline 164 → +31).
**Clippy**: clean across all targets.

Execution order is P0 → P1 → P2 → P3, but within a tier items are independent
and can be parallelized. Each item ships as its own commit so we can revert
surgically.

---

## P0 — Highest leverage, smallest blast radius

### #1 Viewer 内搜索 / 跳转

**Why**: 500K+ token sessions are unsearchable today; users have to manually
`Ctrl+D` through hundreds of pages to find an error message.

**Plan**:
1. `app::ConversationCache` gets a new `search: Option<SearchState>` field.
   ```rust
   pub struct SearchState {
       pub query: String,
       pub matches: Vec<usize>,    // indices into RenderCache::lines
       pub current: usize,         // which match we're on
       pub editing: bool,          // true while user is typing
   }
   ```
2. Add `KeyAction::ViewerSearchStart` (`/`), `ViewerSearchNext` (`n`),
   `ViewerSearchPrev` (`N`), `ViewerSearchCancel` (`Esc` already wired through
   `ViewerBack`, but in search-edit mode `Esc` should cancel search not exit).
3. `tui/viewer.rs::build_visible` already returns the `Vec<Line>`. Add a sibling
   `find_matches(lines, &query) -> Vec<usize>` that scans `Span.content`
   case-insensitively.
4. Render: highlight matched lines with a `Style::default().bg(Yellow).fg(Black)`
   on the matching span only (need to split the span — start with line-level
   highlight as v1, span-level as polish).
5. When user types `/`, push search-edit mode flag. Backspace edits, Enter
   commits and jumps to first match, Esc clears.
6. `n`/`N` in non-edit mode: scroll to `lines[matches[current]]`.

**Files**:
- `crates/agentmonitor/src/app.rs` — `SearchState`, hook into `ConversationCache`
- `crates/agentmonitor/src/keybinding.rs` — 4 new `KeyAction`s + defaults
- `crates/agentmonitor/src/event.rs` — handlers for new actions in viewer mode
- `crates/agentmonitor/src/tui/viewer.rs` — match scanning + highlight + footer
- `crates/agentmonitor/src/i18n.rs` — labels (`viewer.search.placeholder`, etc.)

**Tests**:
- Unit: `find_matches("hello WORLD", "world")` returns line index, case-insensitive
- Unit: empty query returns empty vec, no panic
- Manual: open a session, `/`, type `error`, `n` jumps line-by-line

**Risk**: None — pure UI, no data flow changes.

---

### #6 Help 弹窗 (`?`)

**Why**: New users have no in-app discovery for keybindings; everything goes
through README.

**Plan**:
1. `AppState` gets `show_help: bool`. Toggled by `?` (new
   `KeyAction::ShowHelp`, context = Global so works in every tab/mode).
2. New module `tui/help.rs::render_help_overlay(frame, area, app)` draws a
   centered `Clear`-backed `Block` listing every `KeyAction::all()` grouped by
   `KeyContext`.
3. `tui/render.rs::draw` checks `show_help` after the body, paints the overlay.
4. Any keypress while overlay is up → close overlay (don't dispatch the key
   to the underlying tab).

**Files**:
- `crates/agentmonitor/src/app.rs` — `show_help` field
- `crates/agentmonitor/src/keybinding.rs` — `KeyAction::ShowHelp`, default `?`
- `crates/agentmonitor/src/event.rs` — early return in `handle_event` if overlay open
- `crates/agentmonitor/src/tui/help.rs` — new file
- `crates/agentmonitor/src/tui/mod.rs` — `pub mod help;`
- `crates/agentmonitor/src/tui/render.rs` — paint overlay
- `crates/agentmonitor/src/i18n.rs` — `help.title`, `help.dismiss`

**Tests**:
- Unit: `show_help` toggles on `?` keypress (mock `App` + `handle_event`)
- Unit: any key with `show_help=true` closes the overlay and is NOT dispatched
- Manual: `?` opens, `?`/`Esc`/`q` closes, opens in viewer mode too

**Risk**: Need to make sure `?` doesn't conflict with existing `q`/`Esc`. None
do — `?` is unbound today. Settings keybinding-edit panel must NOT trigger
the overlay (it already swallows keys).

---

### #10 `gh api` 改 async 不阻塞 event loop

**Why**: `event.rs::open_github_star` calls `.output()` (blocking) inside the
sync `handle_event`. For up to ~30s (HTTP timeout) the entire TUI freezes,
no key input, no rendering.

**Plan**:
1. Convert `open_github_star` to spawn a tokio task:
   ```rust
   fn open_github_star(app: &mut App) {
       let state = app.state.clone();
       app.state.write().toast = Some(t("star.checking").into());
       app.state.write().dirty = true;
       tokio::spawn(async move {
           // tokio::process::Command + .output().await
           // Update settings + state.toast on completion.
       });
   }
   ```
2. The `dirty` Notify that the spawned task signals lives in `event::run_event_loop`.
   We need to either:
   - Pass an `Arc<Notify>` through to `App` (cleaner, lasting), or
   - Have the spawned task write `state.dirty = true` and rely on the next event tick to redraw (worse, but simpler — the user will likely press a key soon).
   
   Choose option A: thread `dirty: Arc<Notify>` through `App` so any task can
   `notify_one()`. This unblocks future async work (item #2 / #3 / #15).
3. Same treatment for `open_url` — `spawn()` is already non-blocking, but wrap
   anyway for consistency.

**Files**:
- `crates/agentmonitor/src/app.rs` — `App.dirty: Arc<Notify>` field
- `crates/agentmonitor/src/event.rs` — `run_event_loop` initializes & passes; `open_github_star` rewritten
- `Cargo.toml` — `tokio` already has `process` ✓ (it's in default features when `rt-multi-thread` is on; double-check)

**Tests**:
- Manual: trigger `*`, observe TUI stays responsive while `gh api` runs
- (No new unit tests — async testing requires more harness)

**Risk**: `App.dirty` is currently created inside `run_event_loop`. Moving it
to `App::new()` slightly changes ownership; make sure tests still pass. Also
make sure we don't double-fire dirty (one for the immediate toast, one for
the completion).

---

## P1 — High value, larger scope

### #2 Token 趋势 sparkline

**Why**: RSS sparkline exists but the more user-relevant signal is "tokens
burned per minute"; no visibility today.

**Plan**:
1. New `collector/token_trend.rs`:
   ```rust
   pub struct TokenTrend {
       samples: RwLock<VecDeque<(SystemTime, u64)>>,  // (ts, total_tokens_at_ts)
       capacity: usize, // ~3600 / 5 = 720 samples = 1h at 5s cadence
   }
   ```
2. `token_refresh::write_back` records the cumulative total (sum across all
   sessions) into `TokenTrend` whenever any write happens.
3. `tui/widgets.rs::braille_spark` already exists; reuse for the trend.
4. Dashboard Overview: add a 4th stat "Σ rate ≈ 42K/min" + a small inline
   sparkline, alongside Sessions / live / Σ tokens.
5. Numbers: bucket samples into 30 1-min slots → delta per slot → braille spark.

**Files**:
- `crates/agentmonitor/src/collector/token_trend.rs` — new
- `crates/agentmonitor/src/collector/mod.rs` — `pub mod token_trend;`
- `crates/agentmonitor/src/collector/token_refresh.rs` — record on write
- `crates/agentmonitor/src/app.rs` — `App.token_trend: Arc<TokenTrend>`
- `crates/agentmonitor/src/event.rs` — pass through to `token_refresh::run`
- `crates/agentmonitor/src/tui/dashboard.rs` — render new widget

**Tests**:
- Unit: `TokenTrend::record` enforces monotone-nondecreasing total (regress=skip)
- Unit: `TokenTrend::buckets(60)` returns 60 deltas, last bucket is delta from previous total
- Unit: `TokenTrend::rate_per_min(window=5min)` returns expected delta/5

**Risk**: Records must be non-blocking (spinlock is fine, samples small).
Careful not to double-count: write_back rejects regressions, so we record
*after* accept, summing across all sessions from `state.read().sessions`.

---

### #4 Top Projects → Sessions filter 跳转

**Why**: Top Projects shows "AgentMonitor: 155 sessions" but no way to drill
in.

**Plan**:
1. New cursor: `App.dashboard_cursor: DashboardCursor` (enum: `Process(usize)`,
   `Project(usize)`).
2. New keybindings: `Tab` cycles cursor between Processes and Top Projects
   panels within Dashboard tab.
3. `Enter` action behavior depends on current cursor:
   - Process → existing `jump_to_selected_process_session`
   - Project → set `app.session_filter = format!("cwd:{}", project.cwd)`,
     switch to `Tab::Sessions`, clear selection.
4. Dashboard render highlights the selected row with the same `▶ ` indicator.

**Files**:
- `crates/agentmonitor/src/app.rs` — `DashboardCursor` enum, `App.dashboard_cursor`
- `crates/agentmonitor/src/keybinding.rs` — `KeyAction::DashboardCycleCursor` (Tab in dashboard)
- `crates/agentmonitor/src/event.rs` — branch on cursor in `jump_to_selected_process_session`
- `crates/agentmonitor/src/tui/dashboard.rs` — render selection
- `crates/agentmonitor/src/tui/process.rs` — accept selected index from outside

**Tests**:
- Unit: `app::tests::cursor_tab_cycles_process_and_project`
- Unit: cursor=Project, Enter → filter contains `cwd:/repos/foo`, tab=Sessions

**Risk**: Tab key currently switches tabs (Global). Need to carve out
"Tab inside Dashboard cycles internal cursor; outside Dashboard switches
tabs". Two ways:
- Make `DashboardCycleCursor` Tab-with-context-priority (per-tab actions
  fire before global).
- Or use a different key (`Shift+Tab` is already prev-tab, so use `o`?).
  
  → Actually, the cleanest is `key_matches(DashboardCycleCursor, ...)` with
  `KeyContext::Dashboard` — and `handle_tab_specific_normal_action` already
  runs *before* global keys, so it just works. Default = `Tab` works only
  within Dashboard.

---

### #8 Sessions Arc 化（性能）

**Why**: Dashboard renders on every dirty notify (potentially several per
second). Each render does `state.sessions.clone()` — for 1000+ sessions this
is several MB of allocation per second.

**Plan**:
1. Replace `AppState.sessions: Vec<SessionMeta>` with
   `AppState.sessions: Arc<Vec<SessionMeta>>`.
2. Mutators in `fs_watch.rs`/`token_refresh.rs`/`app.rs::initial_scan`:
   ```rust
   let mut new = (*s.sessions).clone();  // clone once
   // mutate new
   s.sessions = Arc::new(new);
   ```
3. Renderers borrow via `let sessions = state.read().sessions.clone()` — this
   is a cheap `Arc::clone`, not a deep copy.
4. Drop the read guard immediately, render against the snapshot.

**Files**:
- `crates/agentmonitor/src/app.rs` — type change + `visible_session_indices` etc.
- `crates/agentmonitor/src/collector/fs_watch.rs` — use Arc::clone + mutate
- `crates/agentmonitor/src/collector/token_refresh.rs` — same
- `crates/agentmonitor/src/tui/dashboard.rs` — drop deep clone
- `crates/agentmonitor/src/tui/sessions.rs` — drop deep clone
- `crates/agentmonitor/src/tui/viewer.rs` — drop deep clone

**Tests**:
- All existing fs_watch / token_refresh tests should still pass with no logic change
- Bench (optional): `examples/bench_loader.rs` time before/after

**Risk**: This is the largest mechanical change. The invariants in CLAUDE.md
§1, §2, §7 are all about field-level preservation; the path through the
mutator changes (we clone-then-mutate-then-store) but the invariants are
identical. Test coverage on these should catch any regression.

---

### #14 端到端集成测试

**Why**: Unit tests cover parsers and merge logic, but the actual data flow
(fs event → fs_watch → token_refresh → state) has no test. CLAUDE.md §1–§9
were all bugs in this flow.

**Plan**:
1. `crates/agentmonitor/tests/integration.rs`:
   ```rust
   #[tokio::test]
   async fn fs_event_propagates_to_token_total() {
       let dir = tempdir().unwrap();
       let claude_root = dir.path().join("projects/test");
       fs::create_dir_all(&claude_root).unwrap();
       
       let config = Config { claude_root: Some(claude_root.clone()), .. };
       let app = App::new(config).await.unwrap();
       
       // Start collectors via run_event_loop equivalent.
       let dirty = Arc::new(Notify::new());
       let token_dirty = Arc::new(Notify::new());
       spawn fs_watch + token_refresh;
       
       // Write a fixture jsonl with 100 input tokens.
       fs::write(claude_root.join("abc.jsonl"), FIXTURE).unwrap();
       
       // Wait up to 2s for token_refresh to pick it up.
       wait_for(|| state.read().sessions.iter().any(|m| m.tokens.input == 100), 2_000).await;
   }
   ```
2. Cover: §1 fast-parse not clobbering tokens, §2 monotone, §5 unknown first
   type still tracked, §9 reconcile carries forward missing.

**Files**:
- `crates/agentmonitor/tests/integration.rs` — new
- `Cargo.toml` (dev-deps) — `tempfile = "3"` (likely already in tree)

**Tests**: This *is* the test.

**Risk**: tokio runtime + multi-task = potential flakiness. Use a generous
wait timeout. Skip on CI if it proves flaky (mark `#[ignore]`).

---

## P2 — Net-positive but not urgent

### #3 成本估算

**Why**: Users want to know "how much $$$ did I burn today".

**Plan**:
1. New `src/pricing.rs`:
   ```rust
   pub struct ModelPrice {
       pub input_per_mtok: f64,
       pub output_per_mtok: f64,
       pub cache_read_per_mtok: f64,
       pub cache_creation_per_mtok: f64,
   }
   pub fn lookup(model: &str) -> Option<ModelPrice> { ... }
   ```
2. Hardcoded prices for Claude / Codex / Gemini / GPT family circa 2026-05.
3. Dashboard Overview: extra row "Σ cost ≈ $42.50".
4. Settings: toggle `show_cost: bool` (default true).
5. Optional override: `~/.config/agent-monitor/pricing.toml` for users who
   want custom per-model rates.

**Files**:
- `crates/agentmonitor/src/pricing.rs` — new
- `crates/agentmonitor/src/lib.rs` — `pub mod pricing;`
- `crates/agentmonitor/src/settings.rs` — `show_cost` field
- `crates/agentmonitor/src/tui/dashboard.rs` — render Σ cost
- `crates/agentmonitor/src/tui/stats.rs` — `aggregate_cost()` helper

**Tests**:
- Unit: lookup returns expected price for known model
- Unit: aggregate_cost is sum across all sessions weighted by their model's price
- Unit: unknown model → price 0 (counted but no contribution)

**Risk**: Prices change frequently. Make it easy to update; document the
update process. Wrong prices are misleading — show "(estimate)" suffix.

---

### #5 Session 收藏 / 置顶

**Why**: After hundreds of sessions, important ones get buried. Star them.

**Plan**:
1. `Settings.starred_paths: Vec<PathBuf>` — persisted.
2. New `KeyAction::SessionsToggleStar` (default `b` for bookmark; `*` is taken
   by GitHub star).
3. `SessionMeta::is_starred` helper queries `Settings.starred_paths`.
4. Default sort: starred sessions float to top.
5. New filter token `starred:true` / `starred:false`.
6. Sessions list: render ★ or ☆ before the agent label.

**Files**:
- `crates/agentmonitor/src/settings.rs` — `starred_paths`
- `crates/agentmonitor/src/keybinding.rs` — `SessionsToggleStar`
- `crates/agentmonitor/src/event.rs` — handler
- `crates/agentmonitor/src/app.rs` — sort + filter token in `visible_session_indices`
- `crates/agentmonitor/src/tui/sessions.rs` — render icon

**Tests**:
- Unit: `visible_session_indices("starred:true", _)` returns only starred
- Unit: starred floats above non-starred under `UpdatedDesc`
- Manual: `b` toggles, persists across restart

**Risk**: Sort interaction — when user explicitly cycles to `TokensDesc`,
should starred still float? No — explicit sort wins. Only `UpdatedDesc`
(default) gets the starred-first behavior. Document this.

---

### #13 event.rs 拆分

**Why**: 1952 lines violates `~/.claude/rules/coding-style.md` (800 max),
hard to navigate.

**Plan**:
- `src/event/mod.rs` — `run_event_loop` (~120 lines)
- `src/event/dispatch.rs` — `handle_event`, mode/state branching (~150 lines)
- `src/event/normal.rs` — `handle_normal`, `handle_tab_specific_normal_action` (~250 lines)
- `src/event/viewer.rs` — `handle_viewer`, scroll math (~100 lines)
- `src/event/filter.rs` — `handle_session_filter_input`, toggles (~80 lines)
- `src/event/keybindings.rs` — capture/conflict logic (~80 lines)
- `src/event/preview.rs` — `maybe_load_preview`, `ensure_conversation` (~120 lines)
- `src/event/resume.rs` — `open_terminal_*`, `build_cd_command`, AppleScript (~250 lines)
- `src/event/star.rs` — `open_github_star` (~80 lines)
- `src/event/delete.rs` — `prompt_delete_*`, `confirm_delete` (~100 lines)
- `src/event/rescan.rs` — `rescan_sessions` (~30 lines)

Tests stay in their module's `#[cfg(test)] mod tests`.

**Files**: Above list. `src/event.rs` is deleted.

**Tests**: All existing event.rs tests must continue passing.

**Risk**: Pure structural refactor; if all tests pass, behavior is preserved.
Recommend doing this AFTER #1, #6, #10 land — those add code to event.rs and
splitting first creates needless merge work.

---

### #16 Panic hook

**Why**: TUI crash leaves terminal in raw mode → garbled output. Lost log.

**Plan**:
1. In `main.rs::main`, before `run_tui`, install a panic hook:
   ```rust
   let original_hook = std::panic::take_hook();
   std::panic::set_hook(Box::new(move |info| {
       let _ = disable_raw_mode();
       let _ = execute!(io::stderr(), LeaveAlternateScreen, DisableMouseCapture);
       eprintln!("{}", info);
       tracing::error!(panic = %info, "panic!");
       original_hook(info);
   }));
   ```
2. Document: panic stack trace goes to log file when `--debug` is on.

**Files**:
- `crates/agentmonitor/src/main.rs` — install hook

**Tests**:
- Unit (separate process): `panic!()` produces clean terminal restoration
  - Skip if hard to test; manual verify suffices

**Risk**: Hook runs on the panicking thread. `disable_raw_mode` failures during
panic are silently ignored — already best-effort.

---

## P3 — Nice to have, defer if low traffic

### #7 导出 / 复制 session

**Why**: Power users want to grep / archive transcripts.

**Plan**:
1. New `src/export.rs`:
   ```rust
   pub fn to_markdown(events: &[ConversationEvent]) -> String { ... }
   pub fn to_clipboard(text: &str) -> Result<()>;  // pbcopy / wl-copy / clip.exe
   ```
2. `KeyAction::ViewerExportMarkdown` (default `E`), `ViewerCopyToClipboard`
   (default `y`).
3. Export path: `~/Downloads/agent-monitor/<agent>-<short_id>.md`. Toast on success.

**Files**:
- `crates/agentmonitor/src/export.rs` — new
- `crates/agentmonitor/src/keybinding.rs` — 2 new actions
- `crates/agentmonitor/src/event.rs` (or `event/viewer.rs` after #13) — handlers

**Tests**:
- Unit: `to_markdown` produces expected structure for fixture events
- Unit: clipboard helper picks correct binary per OS (mock `which_exists`)

**Risk**: macOS clipboard via `pbcopy` is straightforward; Wayland needs
`wl-copy`, X11 needs `xclip`/`xsel`. Probe in order, fall back to error toast.

---

### #9 缓存 dashboard aggregates

**Why**: `top_projects` / `activity_buckets` / `tokens_by_agent` recompute
every dirty notify.

**Plan**:
1. `AppState.aggregates: Option<Aggregates>` (None = stale).
2. Any sessions mutation sets `aggregates = None`.
3. `tui/dashboard.rs::render` checks: if `None`, recompute + write back.
4. Need to be careful not to deadlock — compute outside the write lock.

**Files**:
- `crates/agentmonitor/src/app.rs` — `Aggregates` struct, invalidation hooks
- `crates/agentmonitor/src/tui/dashboard.rs` — read-or-compute path
- `crates/agentmonitor/src/tui/stats.rs` — same code, just called less often
- `crates/agentmonitor/src/collector/fs_watch.rs` / `token_refresh.rs` — invalidate on session mutation

**Tests**:
- Unit: aggregates recomputed when sessions change
- Unit: same render produces same aggregates twice (cache hit)

**Risk**: If invalidation misses a path, dashboard goes stale. Pair every
`s.sessions = ...` with `s.aggregates = None`. Probably worth a setter method.

---

### #11 fs_watch path-aware debounce

**Why**: Current `sleep(50ms)` per event runs serially on burst events. With
1000 file events in a second, that's a 50s backlog.

**Plan**:
1. `pending: HashMap<PathBuf, Instant>` of paths needing re-parse.
2. Single 30ms ticker drains pending: for each path whose
   `Instant + 30ms < now`, run `update_for_path` and remove from pending.
3. Notify Modify handler just inserts/refreshes the timestamp.

**Files**:
- `crates/agentmonitor/src/collector/fs_watch.rs` — restructure event handler

**Tests**:
- Unit: 100 modifies on the same path within 30ms → 1 update_for_path call
- Unit: 30ms+50ms+50ms+50ms across different paths → all 4 get parsed

**Risk**: Edge case: the ticker can drift (tokio interval semantics). Use
`MissedTickBehavior::Skip` to avoid pile-up. Test convergence.

---

### #12 TokenCache LRU + 上限

**Why**: `HashMap<PathBuf, CacheEntry>` grows unbounded over weeks of usage.

**Plan**:
1. Bound at 5000 entries. When over, evict oldest by `mtime` (proxy for last
   touch).
2. After every `replace_preserving_tokens`, remove cache entries whose path is
   no longer in `state.sessions`.

**Files**:
- `crates/agentmonitor/src/collector/token_refresh.rs` — LRU + eviction
- `crates/agentmonitor/src/collector/fs_watch.rs` — call cleanup on reconcile

**Tests**:
- Unit: 5001 inserts → exactly 5000 in cache, oldest evicted
- Unit: cleanup_orphans removes entries not in given path set

**Risk**: Cache miss = re-parse of multi-MB file. Eviction policy must keep
hot sessions. mtime is a reasonable proxy.

---

### #15 自身运行时指标 / Diagnostics

**Why**: Hard to debug user reports without runtime visibility ("token totals
seem stuck").

**Plan**:
1. `collector/diagnostics.rs::DiagnosticsStore` — atomic counters + ring buffer
   of recent timings.
2. token_refresh / fs_watch instrument their key callsites (`refresh_one`,
   `handle_event`, `replace_preserving_tokens`).
3. Settings tab gets a folded "Diagnostics" panel:
   ```
   Token refresh: 127 passes, avg 12ms, last 4ms
   Cache hit rate: 89.2%
   FS events: 4513 total, 5.2/s last min
   ```

**Files**:
- `crates/agentmonitor/src/collector/diagnostics.rs` — new
- `crates/agentmonitor/src/collector/mod.rs` — `pub mod diagnostics;`
- `crates/agentmonitor/src/collector/fs_watch.rs` — record events + reconcile
- `crates/agentmonitor/src/collector/token_refresh.rs` — record passes + cache hits
- `crates/agentmonitor/src/app.rs` — `App.diagnostics: Arc<DiagnosticsStore>`
- `crates/agentmonitor/src/tui/settings.rs` — render panel

**Tests**:
- Unit: counters increment correctly
- Unit: ring buffer evicts oldest when full

**Risk**: Atomic counters are cheap; don't over-instrument hot paths.

---

### #17 Windows 终端启动支持

**Why**: Windows users can't `r` resume.

**Plan**:
1. `TerminalApp::WindowsTerminal` (`wt.exe`), `TerminalApp::PowerShell`
   (`pwsh.exe`), `TerminalApp::Cmd` (`cmd.exe`).
2. `TerminalApp::detect()` checks `where wt` etc. on Windows.
3. `open_cli_terminal` branches:
   ```rust
   TerminalApp::WindowsTerminal => ("wt.exe", vec!["-d", cwd_str, "cmd", "/k", &cmd]),
   ```
4. Build_cd_command for Windows uses `cd /d "..." && cmd`.

**Files**:
- `crates/agentmonitor/src/settings.rs` — new variants + detect
- `crates/agentmonitor/src/event.rs` — `open_cli_terminal` Windows branch
- (after #13) `event/resume.rs` — same

**Tests**:
- Unit: `build_cd_command_windows` produces expected string
- Manual on Windows: `r` opens new wt with correct cwd

**Risk**: No Windows CI yet (visible from `.github/workflows`). Land behind
`#[cfg(target_os = "windows")]` so non-Windows builds don't drag in dead code.

---

### #18 可配置 session roots

**Why**: Users with non-default paths (symlink, dotfiles, custom install) can
silently miss data.

**Plan**:
1. CLI flags: `--claude-root <PATH>`, `--codex-root`, `--gemini-root`,
   `--hermes-root`, `--opencode-root`, `--claude-desktop-root`.
2. Optional config file `~/.config/agent-monitor/agents.toml`:
   ```toml
   [roots]
   claude = "/custom/path"
   codex = "/another/path"
   ```
3. Precedence: CLI flag > config file > built-in default.

**Files**:
- `crates/agentmonitor/src/main.rs` — clap args
- `crates/agentmonitor/src/config.rs` — `Config::load_or_default()`
- new `crates/agentmonitor/src/agents_config.rs`? Or merge into `config.rs`.

**Tests**:
- Unit: CLI flag overrides config, config overrides default
- Unit: missing file → defaults

**Risk**: None — purely additive.

---

## Sequencing

A reasonable shipping order:

1. **Sprint 1 (P0)**: #1 → #6 → #10 (each a separate commit)
2. **Sprint 2 (P1)**: #14 first (locks in correctness), then #8 (perf foundation), then #2/#4 in parallel
3. **Sprint 3 (P2)**: #13 (refactor for cleaner future work), then #16 (small),
   then #3 / #5 (parallel)
4. **Sprint 4 (P3)**: #11 → #12 (collector polish), #9 (perf), #15 (observability),
   #7 (export), #17 / #18 (portability)

Each item ends with `cargo test -p agentmonitor --lib && cargo clippy -p
agentmonitor --all-targets -- -D warnings` clean.

## Validation per item

Before marking any item complete, the following must hold:

- [ ] `cargo test -p agentmonitor --lib` passes (existing + new tests)
- [ ] `cargo clippy -p agentmonitor --all-targets -- -D warnings` passes
- [ ] `cargo run -p agentmonitor --release -- --once-and-exit` returns the
      same session count as before (sanity check)
- [ ] If the change touches data flow (#2, #8, #11, #12, #14, #15) — manual TUI
      smoke test confirms Dashboard tokens still match `--once-and-exit` snapshot

---

End of plan.
