# agent-monitor

[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Rust 1.82+](https://img.shields.io/badge/rust-1.82%2B-orange.svg)](https://www.rust-lang.org/)
[![npm](https://img.shields.io/npm/v/@yeheboo/agentmonitor.svg)](https://www.npmjs.com/package/@yeheboo/agentmonitor)

**[English](README.md)**

高性能终端 UI，实时监控 AI 编程 Agent 会话。

---

## 支持的 Agent

| Agent | 会话路径 | 说明 |
| --- | --- | --- |
| [Claude Code](https://docs.anthropic.com/en/docs/claude-code) | `~/.claude/projects/**/*.jsonl` | CLI Agent |
| [Claude Desktop](https://claude.ai/download) | `~/Library/Application Support/Claude-3p/local-agent-mode-sessions/` | macOS 本地 Agent 模式 |
| [Codex CLI](https://github.com/openai/codex) | `~/.codex/sessions/**/rollout-*.jsonl` | CLI Agent |
| [Codex App](https://github.com/openai/codex) | 同 Codex CLI | 桌面应用（`Codex.app`）会话 |
| [OpenCode](https://github.com/sst/opencode) | `~/.local/share/opencode/opencode.db` | SQLite 数据库，使用虚拟会话路径 |
| [Gemini CLI](https://github.com/google-gemini/gemini-cli) | `~/.gemini/tmp/<project_hash>/session-*.json` | 按项目目录组织的会话历史 |
| [Hermes Agent](https://github.com/NousResearch/hermes-agent) | `~/.hermes/state.db` | SQLite 数据库，使用虚拟会话路径 |

## 功能特性

- **实时会话追踪** — 基于文件系统事件驱动，10s 对账兜底
- **实时进程指标** — 周期性采样运行中 Agent 进程的 CPU 和 RSS
- **Token 统计** — 精确的每会话 input / output / cache token 总量，保证单调非递减
- **仪表盘** — 按项目展示会话数、聚合 token 用量、活跃进程
- **会话浏览器** — 按 Agent、状态、cwd、分支或模型过滤；按更新时间、token 数、大小、消息数或状态排序
- **对话查看器** — 浏览完整对话历史，可折叠 thinking 块、工具调用和附件
- **会话恢复** — 一键在新终端中恢复会话
- **一键更新** — `agent-monitor update` 检查新版本并原地升级
- **跨平台** — macOS (ARM64, x64)、Linux (ARM64, x64)、Windows (ARM64, x64)

## 安装

### npm（推荐）

直接运行：

```bash
npx @yeheboo/agentmonitor
```

全局安装：

```bash
npm install -g @yeheboo/agentmonitor
```

也支持其他包管理器：

```bash
yarn global add @yeheboo/agentmonitor
pnpm add -g @yeheboo/agentmonitor
bun install -g @yeheboo/agentmonitor
```

npm 包内置所有平台的原生二进制文件，无需安装 Rust 工具链。

### 从源码构建

```bash
git clone https://github.com/jiweiyeah/AgentMonitor.git
cd AgentMonitor
cargo run -p agentmonitor --release
```

构建产物位于 `target/release/agent-monitor`，可复制到任意 `$PATH` 目录。

## 使用

### 启动

通过 npm 安装后，任一命令均可启动：

```bash
agent-monitor
agentm
agentmonitor
```

从源码构建时：

```bash
cargo run -p agentmonitor --release
```

### 更新

检查新版本并一键升级：

```bash
agent-monitor update
agentm update
agentmonitor update
```

更新命令会自动检测你的包管理器（npm / yarn / pnpm / bun）并执行对应的全局安装命令。

### CLI 参数

```
agent-monitor [选项]

选项:
  --once-and-exit          扫描一次会话列表后打印到 stdout 并退出
  --sample-interval <秒>   进程采样间隔（秒），默认 2
  --debug                  将追踪日志写入平台缓存目录
  -h, --help               显示帮助
  -V, --version            显示版本
```

### 快捷键

#### 导航（Normal 模式）

| 按键 | 动作 |
| --- | --- |
| `1` / `2` / `3` | 切换到 Dashboard / Sessions / Settings |
| `Tab` / `→` | 下一个 tab |
| `Shift+Tab` / `←` | 上一个 tab |
| `j` / `↓` | 向下移动选中项 |
| `k` / `↑` | 向上移动选中项 |
| `Enter` | Dashboard: 跳转到选中进程对应的会话 · Sessions: 打开对话查看器 · Settings: 切换设置 |
| `f` | 强制重新扫描会话 |
| `q` / `Ctrl+C` | 退出 |

#### Sessions tab 专用

| 按键 | 动作 |
| --- | --- |
| `/` | 打开过滤输入（支持普通子串，也支持结构化条件：`agent:codex status:active cwd:foo`） |
| `a` | 切换 `status:active` 过滤 |
| `s` | 循环切换排序（updated ↓ → tokens ↓ → size ↓ → msgs ↓ → status ↓） |
| `c` | 清除当前过滤 |
| `r` | 在新终端中恢复选中的会话 |
| `d` / `Delete` | 删除选中的会话（需确认） |

#### 对话查看器

| 按键 | 动作 |
| --- | --- |
| `Esc` / `q` | 返回 Sessions |
| `j` / `↓` | 下滚一行 |
| `k` / `↑` | 上滚一行 |
| `Ctrl+D` / `Ctrl+U` | 半屏翻页 |
| `PgDn` / `PgUp` | 整屏翻页 |
| `g` / `G` | 跳到顶部 / 底部 |
| `e` | 展开所有折叠区域（thinking、tool_use、tool_result、attachment） |
| `c` | 折叠所有区域 |
| `r` | 在新终端中恢复当前查看的会话 |
| `d` / `Delete` | 删除当前查看的会话（需确认） |

查看器按 mtime 缓存已解析的会话，仅将可见行交给 ratatui 渲染——即使 1 MB+ 的会话也能保持常量时间滚动。

## 架构

```
crates/agentmonitor/src/
  adapter/         各 Agent 的会话解析
    claude.rs          Claude Code JSONL schema
    claude_desktop.rs  Claude Desktop 本地 Agent 模式 schema
    codex.rs           Codex CLI & App rollout-*.jsonl schema
    gemini.rs          Gemini CLI session-*.json schema
    hermes.rs          Hermes Agent SQLite 适配器（虚拟路径）
    opencode.rs        OpenCode SQLite 适配器（虚拟路径）
    conversation.rs    共享对话事件模型
    types.rs           SessionMeta、TokenStats、MessagePreview
  collector/       后台数据源
    fs_watch.rs    基于 notify 的文件监听 + 10s 对账兜底
    proc_sampler.rs ps 风格进程采样
    token_refresh.rs 全量解析 token 计算 + (path, mtime) 缓存
  tui/             ratatui 渲染器（dashboard / sessions / process / viewer）
  app.rs           AppState（RwLock 守护）、App、SessionSort
  event.rs         事件循环 + 按键分派 + Notify 管道
  main.rs          入口
npm/               预编译二进制的 npm 发布管道
```

三个后台任务通过 `Arc<Notify>` 信号驱动 `AppState`：

1. **proc_sampler** 写入 `MetricsStore`，通知 `dirty` → 触发渲染。
2. **fs_watch** 写入会话**元数据**（id / cwd / mtime / size / status / model），通知 `dirty` 和 `token_dirty`。
3. **token_refresh** 写入会话 **tokens + message_count**，由 `token_dirty` 驱动，附带 5s 兜底定时器。

详见 [`CLAUDE.md`](CLAUDE.md) 中的关键不变量和调试技巧。

## 调试

使用 `--debug` 运行可将日志写入平台缓存目录：

- **macOS**: `~/Library/Caches/dev.agentmonitor.agent-monitor/agent-monitor.log`
- **Linux**: `$XDG_CACHE_HOME/agent-monitor/agent-monitor.log`

关键日志行：

- `token_refresh: first pass done updated=N` — 确认后台扫描已执行
- `fs_watch: new session tracked path=...` — 每个会话应只触发一次；重复触发说明路径规范化有问题
- `write_back: accepted path=... old=X new=Y delta=Z` — 权威的 token 变化记录

## 发布

由 GitHub Actions 在推送 `v*` tag 时自动发布。

1. 更新以下文件中的版本号：
   - `Cargo.toml`（`workspace.package.version`）
   - `npm/agent-monitor/package.json`（`version` + `optionalDependencies`）
   - `npm/platforms/*/package.json`（`version`）

2. 验证：

   ```bash
   cargo test -p agentmonitor --lib
   cargo clippy -p agentmonitor --all-targets -- -D warnings
   npm pack ./npm/agent-monitor --dry-run
   ```

3. 打 tag 并推送：

   ```bash
   git add Cargo.toml Cargo.lock npm/agent-monitor/package.json npm/platforms/*/package.json
   git commit -m "chore: release 0.x.y"
   git push origin main
   git tag v0.x.y
   git push origin v0.x.y
   ```

## 参与贡献

欢迎在 [GitHub Issues](https://github.com/jiweiyeah/AgentMonitor/issues) 提交 bug 报告和 pull request。

提交 PR 前请确保通过：

```bash
cargo test -p agentmonitor --lib
cargo clippy -p agentmonitor --all-targets -- -D warnings
```

Adapter 解析相关的改动请附带使用 `serde_json::json!` 构造的表驱动测试。

## 许可证

[MIT](LICENSE)

## Star History

[![Star History Chart](https://api.star-history.com/svg?repos=jiweiyeah/AgentMonitor&type=Date)](https://star-history.com/#jiweiyeah/AgentMonitor&Date)
