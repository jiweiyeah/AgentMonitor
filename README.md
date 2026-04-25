# agent-monitor

一个监控 Claude Code / Codex 会话和进程的终端 UI 工具。扫描两者的会话 JSONL，采样进程 CPU / RSS，在 TUI 中实时展示。

## 环境要求

- Rust 1.82+（cargo、rustc）
- macOS / Linux / Windows
- 可选：Claude Code 会话目录 `~/.claude/projects`、Codex 会话目录 `~/.codex/sessions`（缺失时对应面板为空）

## 从源码运行

开发模式（带调试信息）：

```bash
cargo run -p agentmonitor
```

发布模式（推荐日常使用，二进制更小、启动更快）：

```bash
cargo run -p agentmonitor --release
```

构建产物在 `target/release/agent-monitor`，可直接拷贝到 `$PATH`。

## CLI 参数

```
agent-monitor [OPTIONS]

  --once-and-exit          扫描一次会话列表后打印到 stdout 退出（冷启动基准用）
  --sample-interval <SEC>  进程采样间隔，默认 2 秒
  --debug                  将 tracing 日志写到 $XDG_CACHE_HOME/agent-monitor.log
  -h, --help               帮助
  -V, --version            版本
```

示例：

```bash
cargo run -p agentmonitor --release -- --sample-interval 1 --debug
```

## 快捷键

### Tab 间导航（Normal 模式）

| 按键 | 动作 |
| --- | --- |
| `1` / `2` / `3` | 切换 Dashboard / Sessions / Settings 三个 tab |
| `Tab` / `→` | 下一个 tab |
| `Shift+Tab` / `←` | 上一个 tab |
| `j` / `↓` | 向下选择（Dashboard 中为进程表；Sessions / Settings 中为当前列表） |
| `k` / `↑` | 向上选择 |
| `Enter` | Dashboard: 跳到选中进程对应的 session；Sessions: 打开对话查看器；Settings: 切换当前设置 |
| `f` | 强制重扫会话 |
| `q` / `Esc` / `Ctrl+C` | 退出 |

### Sessions tab 专用

| 按键 | 动作 |
| --- | --- |
| `/` | 进入过滤输入（支持普通子串，也支持 `agent:codex status:active cwd:foo branch:main model:gpt` 这种结构化条件） |
| `a` | 一键切换 `status:active` 过滤 |
| `s` | 循环切换排序（updated↓ → tokens↓ → size↓ → msgs↓ → status↓） |
| `c` | 清除当前过滤 |
| `r` | 在新终端中恢复当前选中的 session |

过滤输入模式内:`Enter` 应用并退出输入,`Esc` 取消并清空,`Backspace` 删最后一个字符,其他字符追加。

### Dashboard 专用

| 按键 | 动作 |
| --- | --- |
| `j` / `k` | 在嵌入的 Live Processes 表中移动选中行 |
| `Enter` | 跳到该进程最可能对应的 session（同 agent，优先同 cwd 和最近活跃） |

### 对话查看器（Viewer 模式）

| 按键 | 动作 |
| --- | --- |
| `Esc` / `q` | 返回 Sessions |
| `j` / `↓` | 下滚一行 |
| `k` / `↑` | 上滚一行 |
| `Ctrl+D` / `Ctrl+U` | 半屏翻页 |
| `PgDn` / `PgUp` | 整屏翻页 |
| `g` / `G` | 跳到顶 / 底 |
| `e` / `c` | 全部展开 / 全部折叠（thinking、tool_use、tool_result、attachment 默认折叠） |
| `r` | 在新终端中恢复当前查看的 session |
| `Ctrl+C` | 退出程序 |

查看器按 mtime 缓存已解析的会话,并只把 visible 行交给 ratatui 渲染,1MB+ 会话也能常量时间滚动。

## 数据源

默认路径定义在 `crates/agentmonitor/src/config.rs`：

- Claude Code: `~/.claude/projects/**/*.jsonl`
- Codex: `~/.codex/sessions/**/rollout-*.jsonl`

进程识别规则在 `crates/agentmonitor/src/adapter/{claude,codex}.rs` 的 `matches_process`。启动器（如 shell/node wrapper）会自动被父子去重逻辑折叠，一个会话对应一个 PID。

## 目录结构

```
crates/agentmonitor/
  src/
    adapter/       各 agent 的会话解析、进程识别、对话事件模型（conversation.rs）
    collector/     进程采样、文件监听、指标存储
    tui/           ratatui 渲染，包括全屏 viewer
    app.rs         共享状态（AppState、Mode、ConversationCache）
    event.rs       事件循环 + 键位分派
    main.rs        入口
npm/               预编译二进制的 npm 发布管道
```

## License

MIT
