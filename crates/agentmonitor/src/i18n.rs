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
        "footer.reset" => "reset",
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
        "sessions.delete_title" => " Delete session? ",
        "sessions.delete_body" => "This removes the backing session file from disk.",
        "sessions.delete_target" => "Session ",
        "sessions.delete_cwd" => "CWD     ",
        "sessions.delete_path" => "Path    ",
        "sessions.delete_confirm_hint" => "Enter/y/Delete delete · Esc/q cancel",
        "sessions.delete_error" => "Delete failed",

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
        "detail.actions_hint" => "r: resume in Terminal · d/Delete: delete session",

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
        "settings.keybindings" => "Keybindings",
        "settings.keybindings_title" => " Keybindings ",
        "settings.keybindings_hint" => {
            "↑↓ select · Enter edit · Esc back · Backspace unbind · r reset · R reset all"
        }
        "settings.keybindings_capture" => "Press a new key for",
        "settings.keybindings_saved" => "Keybinding changes saved to disk immediately.",
        "settings.keybindings_note" => "Ctrl+C is always reserved for emergency quit.",
        "settings.keybindings_replaced" => "Replaced binding from",
        "key_context.global" => "Global",
        "key_context.dashboard" => "Dashboard",
        "key_context.sessions" => "Sessions",
        "key_context.settings" => "Settings",
        "key_context.viewer" => "Viewer",
        "key_context.delete_confirm" => "Delete",
        "key_context.filter_input" => "Filter",
        "key_action.quit" => "Quit",
        "key_action.tab_next" => "Next tab",
        "key_action.tab_previous" => "Previous tab",
        "key_action.open_dashboard_tab" => "Open Dashboard tab",
        "key_action.open_sessions_tab" => "Open Sessions tab",
        "key_action.open_settings_tab" => "Open Settings tab",
        "key_action.move_down" => "Move down",
        "key_action.move_up" => "Move up",
        "key_action.refresh" => "Refresh sessions",
        "key_action.dashboard_jump_session" => "Jump to session",
        "key_action.sessions_open_viewer" => "Open viewer",
        "key_action.sessions_start_filter" => "Start filter",
        "key_action.sessions_cycle_sort" => "Cycle sort",
        "key_action.sessions_toggle_active_only" => "Toggle active-only",
        "key_action.sessions_clear_filter" => "Clear filter",
        "key_action.sessions_resume" => "Resume session",
        "key_action.sessions_delete" => "Delete session",
        "key_action.settings_activate" => "Activate setting",
        "key_action.settings_change_next" => "Next setting value",
        "key_action.settings_change_previous" => "Previous setting value",
        "key_action.settings_reset" => "Reset settings",
        "key_action.settings_cancel" => "Back/cancel settings panel",
        "key_action.settings_clear_keybinding" => "Clear keybinding",
        "key_action.settings_reset_all_keybindings" => "Reset all keybindings",
        "key_action.viewer_back" => "Back from viewer",
        "key_action.viewer_scroll_down" => "Scroll down",
        "key_action.viewer_scroll_up" => "Scroll up",
        "key_action.viewer_half_page_down" => "Half page down",
        "key_action.viewer_half_page_up" => "Half page up",
        "key_action.viewer_page_down" => "Page down",
        "key_action.viewer_page_up" => "Page up",
        "key_action.viewer_top" => "Go to top",
        "key_action.viewer_bottom" => "Go to bottom",
        "key_action.viewer_expand" => "Expand blocks",
        "key_action.viewer_collapse" => "Collapse blocks",
        "key_action.viewer_resume" => "Resume viewed session",
        "key_action.viewer_delete" => "Delete viewed session",
        "key_action.delete_cancel" => "Cancel delete",
        "key_action.delete_confirm" => "Confirm delete",
        "key_action.filter_cancel" => "Cancel filter",
        "key_action.filter_apply" => "Apply filter",
        "key_action.filter_delete_char" => "Delete filter character",
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
        "footer.reset" => "恢复默认",
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
        "sessions.delete_title" => " 删除会话？ ",
        "sessions.delete_body" => "这会把底层会话文件从磁盘删除。",
        "sessions.delete_target" => "会话     ",
        "sessions.delete_cwd" => "目录     ",
        "sessions.delete_path" => "路径     ",
        "sessions.delete_confirm_hint" => "Enter/y/Delete 删除 · Esc/q 取消",
        "sessions.delete_error" => "删除失败",

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
        "detail.actions_hint" => "r: 在终端恢复会话 · d/Delete: 删除会话",

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
        "settings.keybindings" => "快捷键",
        "settings.keybindings_title" => " 快捷键 ",
        "settings.keybindings_hint" => {
            "↑↓ 选择 · Enter 编辑 · Esc 返回 · Backspace 解绑 · r 恢复 · R 全部恢复"
        }
        "settings.keybindings_capture" => "按下新的快捷键：",
        "settings.keybindings_saved" => "快捷键修改已立即保存到磁盘。",
        "settings.keybindings_note" => "Ctrl+C 始终保留为紧急退出。",
        "settings.keybindings_replaced" => "已替换绑定：",
        "key_context.global" => "全局",
        "key_context.dashboard" => "仪表盘",
        "key_context.sessions" => "会话",
        "key_context.settings" => "设置",
        "key_context.viewer" => "查看器",
        "key_context.delete_confirm" => "删除确认",
        "key_context.filter_input" => "过滤输入",
        "key_action.quit" => "退出",
        "key_action.tab_next" => "下一个标签",
        "key_action.tab_previous" => "上一个标签",
        "key_action.open_dashboard_tab" => "打开仪表盘标签",
        "key_action.open_sessions_tab" => "打开会话标签",
        "key_action.open_settings_tab" => "打开设置标签",
        "key_action.move_down" => "向下移动",
        "key_action.move_up" => "向上移动",
        "key_action.refresh" => "刷新会话",
        "key_action.dashboard_jump_session" => "跳到会话",
        "key_action.sessions_open_viewer" => "打开查看器",
        "key_action.sessions_start_filter" => "开始过滤",
        "key_action.sessions_cycle_sort" => "切换排序",
        "key_action.sessions_toggle_active_only" => "切换仅活跃",
        "key_action.sessions_clear_filter" => "清空过滤",
        "key_action.sessions_resume" => "恢复会话",
        "key_action.sessions_delete" => "删除会话",
        "key_action.settings_activate" => "激活设置项",
        "key_action.settings_change_next" => "下一个设置值",
        "key_action.settings_change_previous" => "上一个设置值",
        "key_action.settings_reset" => "恢复设置默认值",
        "key_action.settings_cancel" => "返回/取消设置面板",
        "key_action.settings_clear_keybinding" => "清除快捷键",
        "key_action.settings_reset_all_keybindings" => "重置全部快捷键",
        "key_action.viewer_back" => "返回查看器",
        "key_action.viewer_scroll_down" => "向下滚动",
        "key_action.viewer_scroll_up" => "向上滚动",
        "key_action.viewer_half_page_down" => "向下半页",
        "key_action.viewer_half_page_up" => "向上半页",
        "key_action.viewer_page_down" => "向下一页",
        "key_action.viewer_page_up" => "向上一页",
        "key_action.viewer_top" => "到顶部",
        "key_action.viewer_bottom" => "到底部",
        "key_action.viewer_expand" => "展开块",
        "key_action.viewer_collapse" => "收起块",
        "key_action.viewer_resume" => "恢复当前会话",
        "key_action.viewer_delete" => "删除当前会话",
        "key_action.delete_cancel" => "取消删除",
        "key_action.delete_confirm" => "确认删除",
        "key_action.filter_cancel" => "取消过滤",
        "key_action.filter_apply" => "应用过滤",
        "key_action.filter_delete_char" => "删除过滤字符",
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
