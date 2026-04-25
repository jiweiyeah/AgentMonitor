//! Tiny key → localized string lookup.
//!
//! Keeping all translations in one file (vs. an `.ftl` bundle or equivalent)
//! is fine at this scale — the key set is small, and a `match` compiles to a
//! jump table so hot-path renderers pay essentially zero runtime cost.
//!
//! Extension rules:
//! - Add every new key to `t_en` first. English is the fallback when `t_zh`
//!   returns `None`, so missing translations degrade gracefully to English
//!   instead of echoing the raw key back to the user.
//! - Keep keys dotted-namespaced (`section.subsection.name`) so future
//!   refactors can grep scoped sets easily.

use crate::settings::{get, Language};

/// Look up a UI string for the current language. Falls back to English for
/// any key the current locale hasn't translated yet.
pub fn t(key: &str) -> &'static str {
    match get().language {
        Language::En => t_en(key),
        Language::Zh => t_zh(key).unwrap_or_else(|| t_en(key)),
    }
}

fn t_en(key: &str) -> &'static str {
    match key {
        // ── Tabs ─────────────────────────────────────────
        "tab.dashboard" => "Dashboard",
        "tab.sessions" => "Sessions",
        "tab.settings" => "Settings",

        // ── Top bar / footer ────────────────────────────
        "footer.quit" => "quit",
        "footer.switch" => "switch",
        "footer.move" => "move",
        "footer.refresh" => "refresh",
        "footer.filter" => "filter",
        "footer.active_only" => "active-only",
        "footer.sort" => "sort",
        "footer.clear" => "clear",
        "footer.open_viewer" => "open viewer",
        "footer.resume" => "resume",
        "footer.cancel" => "cancel",
        "footer.apply" => "apply",
        "footer.delete" => "delete",
        "footer.change" => "change value",
        "footer.jump_session" => "jump to session",
        "footer.save" => "saved automatically",
        "footer.back" => "back",
        "footer.scroll" => "scroll",
        "footer.half_page" => "half-page",
        "footer.top_bottom" => "top/bottom",
        "footer.expand_collapse" => "expand/collapse",

        // ── Dashboard ───────────────────────────────────
        "dashboard.overview" => " Overview ",
        "dashboard.sessions" => "Sessions",
        "dashboard.last24h" => "last24h",
        "dashboard.total_tokens" => "Σ tokens",
        "dashboard.agents" => "Agents",
        "dashboard.process" => "Process",
        "dashboard.live" => "live",
        "dashboard.activity" => " 24h Activity ",
        "dashboard.top_projects" => " Top Projects ",
        "dashboard.tokens_by_agent" => " Tokens by agent ",
        "dashboard.sessions_sum" => "sessions",
        "dashboard.no_sessions" => "No sessions yet — start `claude` or `codex` in a project.",

        // ── Sessions ────────────────────────────────────
        "sessions.title" => " Sessions ",
        "sessions.detail" => " Detail ",
        "sessions.recent_messages" => " Recent messages ",
        "sessions.recent_messages_loading" => " Recent messages (loading) ",
        "sessions.filter_label" => "Filter ",
        "sessions.filter_hint" => {
            "(press /, or `a` for active-only; try `agent:codex status:active`)"
        }
        "sessions.filter_clear_hint" => " (c to clear)",
        "sessions.filter_edit_hint" => "  Esc cancel · Enter apply · Backspace delete",
        "sessions.sort_label" => "    Sort ",
        "sessions.sort_hint" => " (s to cycle)",
        "sessions.empty" => "No sessions found yet. Run `claude` or `codex` to see them here.",
        "sessions.empty_filtered" => "No sessions match the current filter.",
        "sessions.preview_select" => "Select a session to preview.",
        "sessions.preview_loading" => "Loading…",
        "sessions.preview_no_messages" => {
            "No user/assistant messages found in the tail of this file."
        }

        // ── Detail fields ───────────────────────────────
        "detail.agent" => "Agent    ",
        "detail.session" => "Session  ",
        "detail.cwd" => "CWD      ",
        "detail.model" => "Model    ",
        "detail.version" => "   Version  ",
        "detail.branch" => "Branch   ",
        "detail.started" => "Started  ",
        "detail.updated" => "Updated  ",
        "detail.messages" => "Messages ",
        "detail.file_size" => "    File size  ",
        "detail.tokens" => "Tokens",
        "detail.resume_hint" => "r: resume in Terminal",

        // ── Viewer ──────────────────────────────────────
        "viewer.title" => " Conversation ",
        "viewer.loading" => "Loading conversation…",
        "viewer.no_data" => " no data ",
        "viewer.loading_chip" => " loading… ",
        "viewer.no_conversation" => "No conversation loaded.",
        "viewer.no_messages" => "This session has no readable messages.",
        "viewer.cwd" => "CWD     ",
        "viewer.model" => "Model   ",
        "viewer.updated" => "    Updated ",
        "viewer.events" => "events",
        "viewer.collapsed" => "collapsed",
        "viewer.expanded" => "expanded",

        // ── Settings ────────────────────────────────────
        "settings.title" => " Settings ",
        "settings.hint" => "↑↓ select · ←→ change value · Enter cycle · r reset",
        "settings.saved" => "Changes saved to disk immediately.",
        "settings.language" => "Language",
        "settings.theme" => "Theme color",
        "settings.time_format" => "Time format",
        "settings.token_unit" => "Token display",
        "settings.sample_interval" => "Sampling interval",
        "settings.include_cache" => "Count cache tokens in Σ",
        "settings.terminal" => "Terminal",
        "settings.on" => "on",
        "settings.off" => "off",
        "settings.note_interval" => "Sampling interval changes take effect after restart.",
        "settings.note_theme" => "Theme and language apply instantly.",

        // ── Process ─────────────────────────────────────
        "process.title" => " Live Processes ",
        "process.no_live" => "No Claude/Codex processes running right now.",

        // Unknown key: return a visible placeholder. We deliberately do NOT
        // echo the input back (that would require a non-'static lifetime) —
        // the placeholder makes missing translations obvious in development.
        _ => "??",
    }
}

fn t_zh(key: &str) -> Option<&'static str> {
    Some(match key {
        "tab.dashboard" => "仪表盘",
        "tab.sessions" => "会话",
        "tab.settings" => "设置",

        "footer.quit" => "退出",
        "footer.switch" => "切换",
        "footer.move" => "移动",
        "footer.refresh" => "刷新",
        "footer.filter" => "过滤",
        "footer.active_only" => "仅活跃",
        "footer.sort" => "排序",
        "footer.clear" => "清空",
        "footer.open_viewer" => "打开查看器",
        "footer.resume" => "恢复",
        "footer.cancel" => "取消",
        "footer.apply" => "应用",
        "footer.delete" => "删除",
        "footer.change" => "改值",
        "footer.jump_session" => "跳到会话",
        "footer.save" => "自动保存",
        "footer.back" => "返回",
        "footer.scroll" => "滚动",
        "footer.half_page" => "半页",
        "footer.top_bottom" => "顶/底",
        "footer.expand_collapse" => "展开/收起",

        "dashboard.overview" => " 概览 ",
        "dashboard.sessions" => "会话数",
        "dashboard.last24h" => "近24小时",
        "dashboard.total_tokens" => "Σ tokens",
        "dashboard.agents" => "智能体",
        "dashboard.process" => "进程",
        "dashboard.live" => "活跃",
        "dashboard.activity" => " 24小时活跃度 ",
        "dashboard.top_projects" => " 热门项目 ",
        "dashboard.tokens_by_agent" => " 按智能体统计 Tokens ",
        "dashboard.sessions_sum" => "个会话",
        "dashboard.no_sessions" => "暂无会话 — 在项目里启动 `claude` 或 `codex` 后就会出现。",

        "sessions.title" => " 会话列表 ",
        "sessions.detail" => " 详情 ",
        "sessions.recent_messages" => " 最近消息 ",
        "sessions.recent_messages_loading" => " 最近消息 (加载中) ",
        "sessions.filter_label" => "过滤 ",
        "sessions.filter_hint" => {
            "(按 / 过滤，或按 `a` 只看活跃，例如 `agent:codex status:active`)"
        }
        "sessions.filter_clear_hint" => " (c 清空)",
        "sessions.filter_edit_hint" => "  Esc 取消 · Enter 应用 · Backspace 删除",
        "sessions.sort_label" => "    排序 ",
        "sessions.sort_hint" => " (s 切换)",
        "sessions.empty" => "暂无会话。运行 `claude` 或 `codex` 后就能看到。",
        "sessions.empty_filtered" => "没有会话符合当前过滤条件。",
        "sessions.preview_select" => "选择一个会话查看预览。",
        "sessions.preview_loading" => "加载中…",
        "sessions.preview_no_messages" => "文件末尾没找到用户或助手消息。",

        "detail.agent" => "智能体   ",
        "detail.session" => "会话 ID  ",
        "detail.cwd" => "目录     ",
        "detail.model" => "模型     ",
        "detail.version" => "   版本     ",
        "detail.branch" => "分支     ",
        "detail.started" => "开始时间 ",
        "detail.updated" => "更新时间 ",
        "detail.messages" => "消息数   ",
        "detail.file_size" => "    文件大小   ",
        "detail.tokens" => "Tokens",
        "detail.resume_hint" => "r: 在终端恢复会话",

        "viewer.title" => " 对话查看器 ",
        "viewer.loading" => "加载对话中…",
        "viewer.no_data" => " 无数据 ",
        "viewer.loading_chip" => " 加载中… ",
        "viewer.no_conversation" => "尚未加载对话。",
        "viewer.no_messages" => "此会话没有可读消息。",
        "viewer.cwd" => "目录    ",
        "viewer.model" => "模型    ",
        "viewer.updated" => "    更新 ",
        "viewer.events" => "条事件",
        "viewer.collapsed" => "收起",
        "viewer.expanded" => "展开",

        "settings.title" => " 设置 ",
        "settings.hint" => "↑↓ 选择 · ←→ 切换值 · Enter 循环 · r 恢复默认",
        "settings.saved" => "修改已自动保存到磁盘。",
        "settings.language" => "语言",
        "settings.theme" => "主题色",
        "settings.time_format" => "时间格式",
        "settings.token_unit" => "Token 显示",
        "settings.sample_interval" => "进程采样间隔",
        "settings.include_cache" => "Σ 中包含 cache tokens",
        "settings.terminal" => "终端",
        "settings.on" => "开",
        "settings.off" => "关",
        "settings.note_interval" => "采样间隔需要重启后生效。",
        "settings.note_theme" => "主题与语言即时生效。",

        "process.title" => " 活跃进程 ",
        "process.no_live" => "当前没有 Claude/Codex 进程在运行。",

        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::{settings, Language};

    /// Global settings can't be reset between tests, so the i18n tests run
    /// serially-by-convention. Each test restores the default language at the
    /// end so it doesn't poison downstream test threads that assume defaults.
    #[test]
    fn english_and_chinese_cover_the_same_keys() {
        // Sample a handful of high-visibility keys. If a new key is added and
        // only wired into English, this test won't *fail* (t_zh falls back to
        // English) — that's intentional. The check here is that the declared
        // translations actually differ for the Chinese locale.
        let keys = [
            "tab.dashboard",
            "tab.sessions",
            "tab.settings",
            "dashboard.overview",
            "sessions.title",
            "settings.title",
        ];
        for key in keys {
            let en = t_en(key);
            let zh = t_zh(key).expect("zh translation missing for important key");
            assert_ne!(
                en, zh,
                "key {key} has identical en/zh translations — likely untranslated",
            );
        }
    }

    #[test]
    fn unknown_key_returns_placeholder() {
        assert_eq!(t_en("no.such.key"), "??");
        assert_eq!(t_zh("no.such.key"), None);
    }

    #[test]
    fn t_dispatches_on_current_language() {
        let _guard = crate::settings::test_lock();
        let original = settings().read().language;
        settings().write().language = Language::En;
        assert_eq!(t("tab.dashboard"), "Dashboard");
        settings().write().language = Language::Zh;
        assert_eq!(t("tab.dashboard"), "仪表盘");
        settings().write().language = original;
    }
}
