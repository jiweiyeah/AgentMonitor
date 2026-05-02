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

- **实时会话追踪** — 基于文件系统事件驱动,10s 对账兜底
- **实时进程指标** — 周期性采样运行中 Agent 进程的 CPU 和 RSS,并解析到启动器应用与责任 PID
- **Token 统计** — 精确的每会话 input / output / cache token 总量,保证单调非递减
- **仪表盘** — 热门项目、按 Agent 拆分的 token、24 小时活跃度迷你图、聚合美元成本与 token 速率、活跃进程
- **会话浏览器** — 按 Agent、状态、cwd、分支或模型过滤;按更新时间、token 数、大小、消息数或状态排序;星标会话浮到默认排序顶部
- **对话查看器** — 浏览完整对话历史,可折叠 thinking 块、工具调用和附件
- **查看器搜索** — `/` 在会话内搜索,`n` / `N` 跳转上一/下一处,实时显示匹配数
- **导出 / 复制** — 将当前查看的会话渲染为 Markdown 写入磁盘,或直接拷贝到系统剪贴板
- **会话恢复** — 一键在新终端中恢复会话(macOS Terminal / iTerm2 / Warp · Ghostty / Alacritty / Kitty / WezTerm · Windows Terminal / PowerShell / cmd.exe)
- **诊断面板** — 设置页展示 token-refresh / 缓存 / fs-watch 的运行计数,便于排障
- **可定制快捷键** — 所有快捷键均可重绑定,修改即时落盘
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
  --once-and-exit              扫描一次会话列表后打印到 stdout 并退出
  --sample-interval <秒>       进程采样间隔(秒),默认 2
  --debug                      将追踪日志写入平台缓存目录
  --claude-root <PATH>         覆盖 Claude Code 项目目录(默认 `~/.claude/projects`)
  --codex-root <PATH>          覆盖 Codex 会话目录(默认 `~/.codex/sessions`)
  --claude-desktop-root <PATH> 覆盖 Claude Desktop local-agent-mode 目录
  --gemini-root <PATH>         覆盖 Gemini CLI tmp 目录(默认 `~/.gemini/tmp`)
  --hermes-root <PATH>         覆盖 Hermes Agent 状态目录(默认 `~/.hermes`)
  --opencode-root <PATH>       覆盖 OpenCode 共享目录(默认 `~/.local/share/opencode`)
  -h, --help                   显示帮助
  -V, --version                显示版本
```

当会话存放在自定义 dotfiles 路径或 symlink 之后导致默认探测失败时,使用对应的 `--*-root` 参数显式指定即可。

### 快捷键

所有快捷键都可在 **Settings → Keybindings** 中自定义(改动即时落盘)。

#### 全局 / Normal 模式

| 按键 | 动作 |
| --- | --- |
| `1` / `2` / `3` | 切换到 Dashboard / Sessions / Settings |
| `Tab` / `→` | 下一个 tab |
| `Shift+Tab` / `←` | 上一个 tab |
| `j` / `↓` | 向下移动选中项 |
| `k` / `↑` | 向上移动选中项 |
| `Enter` | Dashboard:跳转到选中的会话/项目 · Sessions:打开对话查看器 · Settings:切换设置 |
| `f` | 强制重新扫描会话 |
| `?` | 显示/隐藏快捷键浮层 |
| `*` | 在 GitHub 上为本项目加星(若安装了 `gh` 则直接通过 API 加) |
| `q` / `Ctrl+C` | 退出 |

#### Dashboard tab 专用

| 按键 | 动作 |
| --- | --- |
| `Tab`(在 Dashboard 内) | 在「活跃进程」与「热门项目」面板之间切换焦点 |
| `Enter` | 进程面板:跳转到该进程对应的会话 · 项目面板:把 Sessions 过滤为 `cwd:<path>` |

#### Sessions tab 专用

| 按键 | 动作 |
| --- | --- |
| `/` | 打开过滤输入(支持普通子串,也支持结构化条件:`agent:codex status:active cwd:foo`) |
| `a` | 切换 `status:active` 过滤 |
| `s` | 循环切换排序(updated ↓ → tokens ↓ → size ↓ → msgs ↓ → status ↓) |
| `c` | 清除当前过滤 |
| `b` | 切换会话收藏 — 默认排序下星标会话置顶 |
| `r` | 在新终端中恢复选中的会话 |
| `d` / `Delete` | 删除选中的会话(需确认) |
| `Enter` | 打开对话查看器 |

#### 对话查看器

| 按键 | 动作 |
| --- | --- |
| `Esc` | 返回 Sessions |
| `j` / `↓` · `k` / `↑` | 下滚 / 上滚一行 |
| `Ctrl+D` / `Ctrl+U` | 半屏翻页 |
| `PgDn` / `PgUp` | 整屏翻页 |
| `g` / `G` | 跳到顶部 / 底部 |
| `e` / `c` | 展开 / 折叠所有区域(thinking、tool_use、tool_result、attachment) |
| `/` | 启动搜索;输入即实时筛选,`Enter` 提交 |
| `n` / `N` | 跳转到下一处 / 上一处匹配 |
| `Esc`(已有搜索时) | 清除搜索而不离开查看器 |
| `E` | 将当前对话导出为 `~/Downloads/agent-monitor/<agent>-<short_id>.md` |
| `y` | 将渲染后的 Markdown 复制到系统剪贴板 |
| `r` | 在新终端中恢复当前查看的会话 |
| `d` / `Delete` | 删除当前查看的会话(需确认) |

查看器按 mtime 缓存已解析的会话，仅将可见行交给 ratatui 渲染——即使 1 MB+ 的会话也能保持常量时间滚动。

## 架构

```
crates/agentmonitor/src/
  adapter/         各 Agent 的会话解析
    claude.rs          Claude Code JSONL schema
    claude_desktop.rs  Claude Desktop 本地 Agent 模式 schema
    codex.rs           Codex CLI & App rollout-*.jsonl schema
    gemini.rs          Gemini CLI session-*.json schema
    hermes.rs          Hermes Agent SQLite 适配器(虚拟路径)
    opencode.rs        OpenCode SQLite 适配器(虚拟路径)
    conversation.rs    共享对话事件模型
    types.rs           SessionMeta、TokenStats、MessagePreview
  collector/       后台数据源
    fs_watch.rs        基于 notify 的文件监听 + 10s 对账兜底
    proc_sampler.rs    ps 风格进程采样
    token_refresh.rs   全量解析 token 计算 + (path, mtime) 缓存
    token_trend.rs     滑动窗口采样,用于仪表盘的 token 速率
    responsible.rs     将 Agent 进程解析到启动器应用与责任 PID
    diagnostics.rs     在 Settings 标签页展示的运行计数
  tui/             ratatui 渲染器
    dashboard.rs   概览、热门项目、24h 活跃度、按 Agent 拆分、成本与速率
    sessions.rs    会话列表 + 过滤 + 详情面板 + 最近消息预览
    viewer.rs      全屏对话查看器,带搜索、导出、复制
    settings.rs    设置面板 + 快捷键编辑器 + 诊断
    help.rs        快捷键帮助浮层
    render.rs      顶层布局 + toast 横幅
  app.rs           AppState(RwLock 守护)、App、SessionSort
  event.rs         事件循环 + 按键分派 + Notify 管道
  export.rs        对话 → Markdown 渲染器 + 原子写入 + 剪贴板
  pricing.rs       仪表盘成本聚合所用的按模型美元定价
  settings.rs      持久化设置(主题、语言、终端、快捷键……)
  i18n.rs          en / zh-CN 翻译
  main.rs          入口
npm/               预编译二进制的 npm 发布管道
```

三个后台任务通过 `Arc<Notify>` 信号驱动 `AppState`:

1. **proc_sampler** 写入 `MetricsStore`,通知 `dirty` → 触发渲染。
2. **fs_watch** 写入会话**元数据**(id / cwd / mtime / size / status / model),通知 `dirty` 和 `token_dirty`。
3. **token_refresh** 写入会话 **tokens + message_count**,由 `token_dirty` 驱动,附带 5s 兜底定时器。

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

## 友情链接

- [LINUX DO](https://linux.do) — 技术爱好者社区。

## 许可证

[MIT](LICENSE)

## Star History

[![Star History Chart](https://api.star-history.com/svg?repos=jiweiyeah/AgentMonitor&type=Date)](https://star-history.com/#jiweiyeah/AgentMonitor&Date)
