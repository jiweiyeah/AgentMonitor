# agent-monitor

[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Rust 1.82+](https://img.shields.io/badge/rust-1.82%2B-orange.svg)](https://www.rust-lang.org/)
[![npm](https://img.shields.io/npm/v/@yeheboo/agentmonitor.svg)](https://www.npmjs.com/package/@yeheboo/agentmonitor)

**[中文文档](README.zh-CN.md)**

A high-performance terminal UI for monitoring AI coding agent sessions in real time.

---

## Supported Agents

| Agent | Session Path | Notes |
| --- | --- | --- |
| [Claude Code](https://docs.anthropic.com/en/docs/claude-code) | `~/.claude/projects/**/*.jsonl` | CLI agent |
| [Claude Desktop](https://claude.ai/download) | `~/Library/Application Support/Claude-3p/local-agent-mode-sessions/` | macOS local agent mode |
| [Codex CLI](https://github.com/openai/codex) | `~/.codex/sessions/**/rollout-*.jsonl` | CLI agent |
| [Codex App](https://github.com/openai/codex) | Same as Codex CLI | Desktop app (`Codex.app`) sessions |
| [OpenCode](https://github.com/sst/opencode) | `~/.local/share/opencode/opencode.db` | SQLite-backed; virtual session paths |
| [Gemini CLI](https://github.com/google-gemini/gemini-cli) | `~/.gemini/tmp/<project_hash>/session-*.json` | Per-project chat history |

## Features

- **Real-time session tracking** — filesystem-event-driven watcher with 10s reconcile fallback
- **Live process metrics** — periodic CPU and RSS sampling for running agent processes
- **Token accounting** — accurate per-session input/output/cache token totals with monotone-nondecreasing guarantees
- **Dashboard** — top projects by session count, aggregate token usage, active processes
- **Session browser** — filter by agent, status, cwd, branch, or model; sort by updated time, tokens, size, messages, or status
- **Conversation viewer** — scroll through full conversation history with collapsible thinking blocks, tool calls, and attachments
- **Session restore** — resume a session in a new terminal with a single keypress
- **One-click update** — `agent-monitor update` checks for newer versions and upgrades in place
- **Cross-platform** — macOS (ARM64, x64), Linux (ARM64, x64), Windows (ARM64, x64)

## Installation

### npm (recommended)

Run directly:

```bash
npx @yeheboo/agentmonitor
```

Install globally:

```bash
npm install -g @yeheboo/agentmonitor
```

Or with your preferred package manager:

```bash
yarn global add @yeheboo/agentmonitor
pnpm add -g @yeheboo/agentmonitor
bun install -g @yeheboo/agentmonitor
```

The package ships native binaries for all supported platforms — no Rust toolchain required.

### From source

```bash
git clone https://github.com/jiweiyeah/AgentMonitor.git
cd AgentMonitor
cargo run -p agentmonitor --release
```

The release binary is at `target/release/agent-monitor`. Copy it anywhere on `$PATH`.

## Usage

### Launch

If installed via npm, use any of:

```bash
agent-monitor
agentm
agentmonitor
```

If built from source:

```bash
cargo run -p agentmonitor --release
```

### Update

Check for a newer version and upgrade in one step:

```bash
agent-monitor update
agentm update
agentmonitor update
```

The update command detects your package manager (npm / yarn / pnpm / bun) and runs the appropriate global install command.

### CLI Options

```
agent-monitor [OPTIONS]

Options:
  --once-and-exit          Print a snapshot of all sessions to stdout and exit
  --sample-interval <SEC>  Process sampling interval in seconds [default: 2]
  --debug                  Write tracing logs to the platform cache directory
  -h, --help               Print help
  -V, --version            Print version
```

### Keybindings

#### Navigation (Normal mode)

| Key | Action |
| --- | --- |
| `1` / `2` / `3` | Switch to Dashboard / Sessions / Settings |
| `Tab` / `→` | Next tab |
| `Shift+Tab` / `←` | Previous tab |
| `j` / `↓` | Move selection down |
| `k` / `↑` | Move selection up |
| `Enter` | Dashboard: jump to session for selected process · Sessions: open viewer · Settings: toggle |
| `f` | Force rescan sessions |
| `q` / `Ctrl+C` | Quit |

#### Sessions tab

| Key | Action |
| --- | --- |
| `/` | Open filter input (plain substring or structured: `agent:codex status:active cwd:foo`) |
| `a` | Toggle `status:active` filter |
| `s` | Cycle sort order (updated ↓ → tokens ↓ → size ↓ → msgs ↓ → status ↓) |
| `c` | Clear filter |
| `r` | Restore selected session in a new terminal |
| `d` / `Delete` | Delete selected session (with confirmation) |

#### Conversation viewer

| Key | Action |
| --- | --- |
| `Esc` / `q` | Back to Sessions |
| `j` / `↓` | Scroll down one line |
| `k` / `↑` | Scroll up one line |
| `Ctrl+D` / `Ctrl+U` | Half-page scroll |
| `PgDn` / `PgUp` | Full-page scroll |
| `g` / `G` | Jump to top / bottom |
| `e` | Expand all collapsed sections (thinking, tool_use, tool_result, attachment) |
| `c` | Collapse all sections |
| `r` | Restore this session in a new terminal |
| `d` / `Delete` | Delete this session (with confirmation) |

The viewer caches parsed sessions by mtime and only hands visible lines to ratatui — even 1 MB+ sessions scroll at constant speed.

## Architecture

```
crates/agentmonitor/src/
  adapter/         per-agent session parsing
    claude.rs          Claude Code JSONL schema
    claude_desktop.rs  Claude Desktop local-agent-mode schema
    codex.rs           Codex CLI & App rollout-*.jsonl schema
    gemini.rs          Gemini CLI session-*.json schema
    opencode.rs        OpenCode SQLite-backed adapter (virtual paths)
    conversation.rs    shared conversation event model
    types.rs           SessionMeta, TokenStats, MessagePreview
  collector/       background data sources
    fs_watch.rs    notify-backed file watcher + 10s reconcile fallback
    proc_sampler.rs ps-style process sampling
    token_refresh.rs full-parse token computation + (path, mtime) cache
  tui/             ratatui renderers (dashboard / sessions / process / viewer)
  app.rs           AppState (RwLock-guarded), App, SessionSort
  event.rs         event loop + key dispatch + Notify plumbing
  main.rs          entry point
npm/               npm publishing pipeline with platform-specific packages
```

Three background tasks feed `AppState` through `Arc<Notify>` signals:

1. **proc_sampler** writes `MetricsStore`, notifies `dirty` → render.
2. **fs_watch** writes session **metadata** (id / cwd / mtime / size / status / model), notifies `dirty` and `token_dirty`.
3. **token_refresh** writes session **tokens + message_count**, notified by `token_dirty` with a 5s safety-net ticker.

See [`CLAUDE.md`](CLAUDE.md) for hard-won invariants and debugging tips.

## Debugging

Run with `--debug` to write logs to the platform cache directory:

- **macOS**: `~/Library/Caches/dev.agentmonitor.agent-monitor/agent-monitor.log`
- **Linux**: `$XDG_CACHE_HOME/agent-monitor/agent-monitor.log`

Key log lines to grep:

- `token_refresh: first pass done updated=N` — confirms the background sweep ran
- `fs_watch: new session tracked path=...` — should fire once per session; repeated fires indicate a path normalization issue
- `write_back: accepted path=... old=X new=Y delta=Z` — authoritative token change record

## Releasing

Publishing is handled by GitHub Actions on `v*` tags.

1. Update version in:
   - `Cargo.toml` (`workspace.package.version`)
   - `npm/agent-monitor/package.json` (`version` + `optionalDependencies`)
   - `npm/platforms/*/package.json` (`version`)

2. Verify:

   ```bash
   cargo test -p agentmonitor --lib
   cargo clippy -p agentmonitor --all-targets -- -D warnings
   npm pack ./npm/agent-monitor --dry-run
   ```

3. Tag and push:

   ```bash
   git add Cargo.toml Cargo.lock npm/agent-monitor/package.json npm/platforms/*/package.json
   git commit -m "chore: release 0.x.y"
   git push origin main
   git tag v0.x.y
   git push origin v0.x.y
   ```

## Contributing

Bug reports and pull requests are welcome at [GitHub Issues](https://github.com/jiweiyeah/AgentMonitor/issues).

Before submitting a PR:

```bash
cargo test -p agentmonitor --lib
cargo clippy -p agentmonitor --all-targets -- -D warnings
```

Adapter parsing changes should include table-driven tests using `serde_json::json!` fixtures.

## License

[MIT](LICENSE)

## Star History

[![Star History Chart](https://api.star-history.com/svg?repos=jiweiyeah/AgentMonitor&type=Date)](https://star-history.com/#jiweiyeah/AgentMonitor&Date)
