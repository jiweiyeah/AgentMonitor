use std::io::ErrorKind;
use std::io::Stdout;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use crossterm::event::{Event, EventStream, KeyCode, KeyEventKind, KeyModifiers};
use futures::StreamExt;
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use tokio::sync::Notify;

use crate::adapter::{adapter_for_path, DynAdapter};
use crate::adapter::types::SessionStatus;
use crate::app::{
    toggle_filter_token, App, ConversationCache, DeleteConfirm, ExpandMode, Mode, PreviewCache, Tab,
};
use crate::collector::{fs_watch, proc_sampler, token_refresh};
use crate::settings::{KeyAction, KeyBinding, TerminalApp};
use crate::tui::render;
use crate::tui::settings::SettingsItem;

#[derive(Debug, Clone, Copy)]
pub struct EventLoopOptions {
    pub frame_budget: Duration,
}

impl Default for EventLoopOptions {
    fn default() -> Self {
        Self {
            frame_budget: Duration::from_millis(16),
        }
    }
}

pub async fn run_event_loop(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    mut app: App,
    _opts: EventLoopOptions,
) -> Result<()> {
    let dirty = Arc::new(Notify::new());
    // Dedicated signal from fs_watch → token_refresh. Separate from `dirty`
    // so a render-only notification doesn't spin the (heavier) token refresh
    // loop, and a file change doesn't miss redraws.
    let token_dirty = Arc::new(Notify::new());

    // Spawn background collectors. P1 uses the placeholder fs_watch; P3/P4
    // swap in notify-backed implementations without touching this function.
    let adapters = app.adapters.clone();
    let state = app.state.clone();
    let metrics = app.metrics.clone();
    let interval = app.config.sample_interval;

    let dirty_clone = dirty.clone();
    tokio::spawn(async move {
        proc_sampler::run(adapters.clone(), metrics, interval, dirty_clone).await;
    });

    let adapters_fs = app.adapters.clone();
    let state_fs = state.clone();
    let dirty_fs = dirty.clone();
    let token_dirty_fs = token_dirty.clone();
    tokio::spawn(async move {
        fs_watch::run(adapters_fs, state_fs, token_dirty_fs, dirty_fs).await;
    });

    let adapters_tok = app.adapters.clone();
    let state_tok = state.clone();
    let cache_tok = app.token_cache.clone();
    let dirty_tok = dirty.clone();
    let token_dirty_tok = token_dirty.clone();
    tokio::spawn(async move {
        token_refresh::run(
            adapters_tok,
            state_tok,
            cache_tok,
            token_dirty_tok,
            dirty_tok,
        )
        .await;
    });

    let mut events = EventStream::new();
    // Always draw once so the user sees something immediately.
    render::draw(terminal, &app).await?;
    // Kick off the initial preview load if a session is already selected.
    maybe_load_preview(&app);

    loop {
        tokio::select! {
            maybe = events.next() => {
                let Some(Ok(ev)) = maybe else { break };
                let prev_selected_path = current_selected_path(&app);
                if handle_event(ev, &mut app) {
                    break;
                }
                let new_selected_path = current_selected_path(&app);
                if new_selected_path != prev_selected_path {
                    maybe_load_preview(&app);
                }
                render::draw(terminal, &app).await?;
            }
            _ = dirty.notified() => {
                render::draw(terminal, &app).await?;
            }
        }
        if app.should_quit {
            break;
        }
    }
    Ok(())
}

fn current_selected_path(app: &App) -> Option<PathBuf> {
    let s = app.state.read();
    let visible = s.visible_session_indices(&app.session_filter, app.session_sort);
    let row = visible.get(s.selected_session.min(visible.len().saturating_sub(1)))?;
    s.sessions.get(*row).map(|m| m.path.clone())
}

/// Spawn a task to load the preview (last messages + full message count)
/// for the currently selected session. Does nothing if already cached.
fn maybe_load_preview(app: &App) {
    let Some(path) = current_selected_path(app) else {
        return;
    };
    {
        let s = app.state.read();
        if let Some(cache) = &s.preview {
            if cache.path == path {
                return; // already cached
            }
        }
    }
    // Reserve the cache entry in "loading" state.
    {
        let mut s = app.state.write();
        s.preview = Some(PreviewCache {
            path: path.clone(),
            messages: Vec::new(),
            message_count: 0,
            loading: true,
        });
    }
    let state = app.state.clone();
    let adapters: Vec<DynAdapter> = app.adapters.clone();
    tokio::spawn(async move {
        let Some(adapter) = adapter_for_path(&adapters, &path).cloned() else {
            return;
        };
        let (messages, count) = tokio::join!(
            adapter.tail_messages(&path, 5),
            adapter.parse_meta_full(&path),
        );
        let messages = messages.unwrap_or_default();
        let message_count = count.map(|m| m.message_count).unwrap_or(0);
        let mut s = state.write();
        if let Some(cache) = &mut s.preview {
            if cache.path == path {
                cache.messages = messages;
                cache.message_count = message_count;
                cache.loading = false;
                s.dirty = true;
            }
        }
    });
}

/// Ensure the conversation cache is populated for `path`, reusing any fresh
/// entry. If `force` is true or the on-disk mtime is newer than the cached
/// snapshot, a background reload is kicked off.
fn ensure_conversation(app: &App, path: &Path, force: bool) {
    let path_buf = path.to_path_buf();
    // Fast path: already cached and not loading and not forced → nothing to do.
    {
        let s = app.state.read();
        if let Some(cache) = &s.conversation {
            if cache.path == path_buf && !cache.loading && !force && cache.error.is_none() {
                return;
            }
        }
    }
    {
        let mut s = app.state.write();
        s.conversation = Some(ConversationCache::loading(path_buf.clone()));
    }
    let state = app.state.clone();
    let adapters: Vec<DynAdapter> = app.adapters.clone();
    tokio::spawn(async move {
        let Some(adapter) = adapter_for_path(&adapters, &path_buf).cloned() else {
            let mut s = state.write();
            if let Some(cache) = &mut s.conversation {
                if cache.path == path_buf {
                    cache.loading = false;
                    cache.error = Some("no adapter owns this path".into());
                    s.dirty = true;
                }
            }
            return;
        };
        let mtime = tokio::fs::metadata(&path_buf)
            .await
            .ok()
            .and_then(|m| m.modified().ok());
        let result = adapter.load_conversation(&path_buf).await;
        let mut s = state.write();
        let Some(cache) = s.conversation.as_mut() else {
            return;
        };
        if cache.path != path_buf {
            return; // user moved on to another session
        }
        match result {
            Ok(events) => {
                cache.events = events;
                cache.mtime = mtime;
            }
            Err(err) => {
                cache.error = Some(err.to_string());
            }
        }
        cache.loading = false;
        s.dirty = true;
    });
}

/// Returns `true` if the loop should exit.
fn handle_event(ev: Event, app: &mut App) -> bool {
    let Event::Key(key) = ev else { return false };
    if key.kind != KeyEventKind::Press {
        return false;
    }
    // Ctrl+C always quits, regardless of mode.
    if matches!(key.code, KeyCode::Char('c')) && key.modifiers.contains(KeyModifiers::CONTROL) {
        app.should_quit = true;
        return true;
    }
    if app.delete_confirm.is_some() {
        return handle_delete_confirm(key.code, key.modifiers, app);
    }
    let mode = app.state.read().mode.clone();
    match mode {
        Mode::Normal => handle_normal(key.code, key.modifiers, app),
        Mode::Viewer { path } => handle_viewer(key.code, key.modifiers, app, &path),
    }
}

fn handle_normal(code: KeyCode, modifiers: KeyModifiers, app: &mut App) -> bool {
    if app.capturing_keybinding.is_some() && app.tab == Tab::Settings {
        handle_keybinding_capture(code, modifiers, app);
        return false;
    }
    if app.settings_keybindings_open && app.tab == Tab::Settings {
        handle_settings_keybindings(code, modifiers, app);
        return false;
    }
    // Sessions-tab filter input swallows printable keys, Backspace, Esc and
    // Enter so they don't trigger normal-mode actions while the user is typing.
    if app.session_filter_input && app.tab == Tab::Sessions {
        handle_session_filter_input(code, modifiers, app);
        return false;
    }
    if handle_tab_specific_normal_action(code, modifiers, app) {
        return false;
    }
    match (code, modifiers) {
        (code, modifiers) if key_matches(KeyAction::Quit, code, modifiers) => {
            app.should_quit = true;
            return true;
        }
        (code, modifiers) if key_matches(KeyAction::TabNext, code, modifiers) => {
            app.cycle_tab_next()
        }
        (code, modifiers) if key_matches(KeyAction::TabPrevious, code, modifiers) => {
            app.cycle_tab_prev()
        }
        (code, modifiers) if key_matches(KeyAction::OpenDashboardTab, code, modifiers) => {
            app.set_tab(Tab::Dashboard)
        }
        (code, modifiers) if key_matches(KeyAction::OpenSessionsTab, code, modifiers) => {
            app.set_tab(Tab::Sessions)
        }
        (code, modifiers) if key_matches(KeyAction::OpenSettingsTab, code, modifiers) => {
            app.set_tab(Tab::Settings)
        }
        (code, modifiers) if key_matches(KeyAction::MoveDown, code, modifiers) => {
            move_selection(app, 1)
        }
        (code, modifiers) if key_matches(KeyAction::MoveUp, code, modifiers) => {
            move_selection(app, -1)
        }
        (code, modifiers) if key_matches(KeyAction::Refresh, code, modifiers) => {
            rescan_sessions(app);
        }
        _ => {}
    }
    false
}

fn handle_tab_specific_normal_action(
    code: KeyCode,
    modifiers: KeyModifiers,
    app: &mut App,
) -> bool {
    match (code, modifiers) {
        (code, modifiers)
            if app.tab == Tab::Dashboard
                && key_matches(KeyAction::DashboardJumpSession, code, modifiers) =>
        {
            jump_to_selected_process_session(app);
            true
        }
        (code, modifiers)
            if app.tab == Tab::Settings
                && key_matches(KeyAction::SettingsChangeNext, code, modifiers) =>
        {
            activate_or_cycle_selected_setting(app, true);
            true
        }
        (code, modifiers)
            if app.tab == Tab::Settings
                && key_matches(KeyAction::SettingsChangePrevious, code, modifiers) =>
        {
            activate_or_cycle_selected_setting(app, false);
            true
        }
        (code, modifiers)
            if app.tab == Tab::Settings
                && key_matches(KeyAction::SettingsActivate, code, modifiers) =>
        {
            activate_or_cycle_selected_setting(app, true);
            true
        }
        (code, modifiers)
            if app.tab == Tab::Settings
                && key_matches(KeyAction::SettingsReset, code, modifiers) =>
        {
            crate::tui::settings::reset_to_defaults();
            app.state.write().dirty = true;
            true
        }
        (code, modifiers)
            if app.tab == Tab::Sessions
                && key_matches(KeyAction::SessionsOpenViewer, code, modifiers) =>
        {
            open_selected_session_viewer(app);
            true
        }
        (code, modifiers)
            if app.tab == Tab::Sessions
                && key_matches(KeyAction::SessionsStartFilter, code, modifiers) =>
        {
            app.session_filter_input = true;
            app.state.write().dirty = true;
            true
        }
        (code, modifiers)
            if app.tab == Tab::Sessions
                && key_matches(KeyAction::SessionsCycleSort, code, modifiers) =>
        {
            app.session_sort = app.session_sort.cycle();
            let mut s = app.state.write();
            s.selected_session = 0;
            s.dirty = true;
            true
        }
        (code, modifiers)
            if app.tab == Tab::Sessions
                && key_matches(KeyAction::SessionsToggleActiveOnly, code, modifiers) =>
        {
            toggle_filter_token(&mut app.session_filter, "status:active");
            let mut s = app.state.write();
            s.selected_session = 0;
            s.dirty = true;
            true
        }
        (code, modifiers)
            if app.tab == Tab::Sessions
                && key_matches(KeyAction::SessionsClearFilter, code, modifiers) =>
        {
            app.session_filter.clear();
            let mut s = app.state.write();
            s.selected_session = 0;
            s.dirty = true;
            true
        }
        (code, modifiers)
            if app.tab == Tab::Sessions
                && key_matches(KeyAction::SessionsResume, code, modifiers) =>
        {
            resume_session(app);
            true
        }
        (code, modifiers)
            if app.tab == Tab::Sessions
                && key_matches(KeyAction::SessionsDelete, code, modifiers) =>
        {
            prompt_delete_selected_session(app);
            true
        }
        _ => false,
    }
}

fn open_selected_session_viewer(app: &mut App) {
    let path = current_selected_path(app);
    if let Some(p) = path {
        {
            let mut s = app.state.write();
            s.mode = Mode::Viewer { path: p.clone() };
            s.dirty = true;
        }
        ensure_conversation(app, &p, false);
    }
}

fn key_matches(action: KeyAction, code: KeyCode, modifiers: KeyModifiers) -> bool {
    crate::settings::get()
        .keybindings
        .matches_action(action, code, modifiers)
}

fn selected_keybinding_action(app: &App) -> Option<KeyAction> {
    KeyAction::all().get(app.selected_keybinding).copied()
}

fn handle_settings_keybindings(code: KeyCode, modifiers: KeyModifiers, app: &mut App) {
    match (code, modifiers) {
        (code, modifiers) if key_matches(KeyAction::SettingsCancel, code, modifiers) => {
            app.settings_keybindings_open = false;
            app.keybinding_conflict = None;
            app.state.write().dirty = true;
        }
        (code, modifiers) if key_matches(KeyAction::SettingsActivate, code, modifiers) => {
            app.capturing_keybinding = selected_keybinding_action(app);
            app.keybinding_conflict = None;
            app.state.write().dirty = true;
        }
        (code, modifiers) if key_matches(KeyAction::SettingsClearKeybinding, code, modifiers) => {
            if let Some(action) = selected_keybinding_action(app) {
                crate::settings::update(|settings| settings.keybindings.clear_binding(action));
                app.keybinding_conflict = None;
                app.state.write().dirty = true;
            }
        }
        (code, modifiers) if key_matches(KeyAction::SettingsReset, code, modifiers) => {
            if let Some(action) = selected_keybinding_action(app) {
                crate::settings::update(|settings| settings.keybindings.reset_binding(action));
                app.keybinding_conflict = None;
                app.state.write().dirty = true;
            }
        }
        (code, modifiers) if key_matches(KeyAction::SettingsResetAllKeybindings, code, modifiers) => {
            crate::settings::update(|settings| settings.keybindings.reset_all());
            app.keybinding_conflict = None;
            app.state.write().dirty = true;
        }
        (code, modifiers) if key_matches(KeyAction::MoveDown, code, modifiers) => {
            move_keybinding_selection(app, 1)
        }
        (code, modifiers) if key_matches(KeyAction::MoveUp, code, modifiers) => {
            move_keybinding_selection(app, -1)
        }
        _ => {}
    }
}

fn handle_keybinding_capture(code: KeyCode, modifiers: KeyModifiers, app: &mut App) {
    if key_matches(KeyAction::SettingsCancel, code, modifiers) {
        app.capturing_keybinding = None;
        app.keybinding_conflict = None;
        app.state.write().dirty = true;
        return;
    }
    let Some(action) = app.capturing_keybinding else {
        return;
    };
    let Some(binding) = KeyBinding::from_event(code, modifiers) else {
        return;
    };
    let conflict = crate::settings::get()
        .keybindings
        .conflict_for(action, binding);
    crate::settings::update(|settings| {
        settings.keybindings.set_binding(action, binding);
    });
    app.keybinding_conflict = conflict;
    app.capturing_keybinding = None;
    app.state.write().dirty = true;
}

fn move_keybinding_selection(app: &mut App, delta: i32) {
    let len = KeyAction::all().len();
    if len == 0 {
        return;
    }
    app.selected_keybinding = clamp_step(app.selected_keybinding, delta, len);
    app.state.write().dirty = true;
}

fn activate_or_cycle_selected_setting(app: &mut App, forward: bool) {
    let items = SettingsItem::all();
    if items.is_empty() {
        return;
    }
    let idx = app.selected_setting.min(items.len() - 1);
    if items[idx] == SettingsItem::Keybindings {
        app.settings_keybindings_open = true;
        app.keybinding_conflict = None;
        app.state.write().dirty = true;
        return;
    }
    cycle_selected_setting(app, forward);
}

fn cycle_selected_setting(app: &mut App, forward: bool) {
    let items = SettingsItem::all();
    if items.is_empty() {
        return;
    }
    let idx = app.selected_setting.min(items.len() - 1);
    if forward {
        items[idx].cycle_forward();
    } else {
        items[idx].cycle_back();
    }
    app.state.write().dirty = true;
}

/// Handle key events while the Sessions filter input is active. Esc cancels
/// the filter, Enter commits it (leaving input mode), Backspace deletes, and
/// any printable character is appended.
fn handle_session_filter_input(code: KeyCode, modifiers: KeyModifiers, app: &mut App) {
    match (code, modifiers) {
        (code, modifiers) if key_matches(KeyAction::FilterCancel, code, modifiers) => {
            app.session_filter.clear();
            app.session_filter_input = false;
            let mut s = app.state.write();
            s.selected_session = 0;
            s.dirty = true;
        }
        (code, modifiers) if key_matches(KeyAction::FilterApply, code, modifiers) => {
            app.session_filter_input = false;
            app.state.write().dirty = true;
        }
        (code, modifiers) if key_matches(KeyAction::FilterDeleteChar, code, modifiers) => {
            app.session_filter.pop();
            let mut s = app.state.write();
            s.selected_session = 0;
            s.dirty = true;
        }
        (KeyCode::Char(c), _) if !c.is_control() => {
            app.session_filter.push(c);
            let mut s = app.state.write();
            s.selected_session = 0;
            s.dirty = true;
        }
        _ => {}
    }
}

fn move_selection(app: &mut App, delta: i32) {
    match app.tab {
        Tab::Sessions => {
            let mut s = app.state.write();
            let visible_len = s
                .visible_session_indices(&app.session_filter, app.session_sort)
                .len();
            if visible_len == 0 {
                return;
            }
            s.selected_session = clamp_step(s.selected_session, delta, visible_len);
            s.dirty = true;
        }
        Tab::Dashboard => {
            // Dashboard embeds the processes table; j/k walks its rows so
            // Enter's "jump to session" action still targets a real row.
            let len = app.metrics.snapshot().len();
            if len == 0 {
                return;
            }
            app.selected_process = clamp_step(app.selected_process, delta, len);
            app.state.write().dirty = true;
        }
        Tab::Settings => {
            let len = SettingsItem::all().len();
            if len == 0 {
                return;
            }
            app.selected_setting = clamp_step(app.selected_setting, delta, len);
            app.state.write().dirty = true;
        }
    }
}

fn jump_to_selected_process_session(app: &mut App) {
    let processes = app.metrics.snapshot();
    let Some(process) = processes.get(app.selected_process).cloned() else {
        return;
    };

    let path = {
        let state = app.state.read();
        let Some(session_idx) = state.best_session_index_for_process(&process) else {
            return;
        };
        state.sessions[session_idx].path.clone()
    };

    app.tab = Tab::Sessions;
    app.session_filter.clear();
    app.session_filter_input = false;

    {
        let mut state = app.state.write();
        if let Some(row) = state.visible_row_for_path("", app.session_sort, &path) {
            state.selected_session = row;
            state.dirty = true;
        }
    }
    maybe_load_preview(app);
}

fn clamp_step(cur: usize, delta: i32, len: usize) -> usize {
    if len == 0 {
        return 0;
    }
    let max = len - 1;
    if delta >= 0 {
        (cur.saturating_add(delta as usize)).min(max)
    } else {
        cur.saturating_sub((-delta) as usize)
    }
}

fn handle_viewer(code: KeyCode, modifiers: KeyModifiers, app: &mut App, path: &Path) -> bool {
    match (code, modifiers) {
        (code, modifiers) if key_matches(KeyAction::ViewerBack, code, modifiers) => {
            let mut s = app.state.write();
            s.mode = Mode::Normal;
            s.dirty = true;
        }
        (code, modifiers) if key_matches(KeyAction::ViewerScrollDown, code, modifiers) => {
            scroll_by(app, 1)
        }
        (code, modifiers) if key_matches(KeyAction::ViewerScrollUp, code, modifiers) => {
            scroll_by(app, -1)
        }
        (code, modifiers) if key_matches(KeyAction::ViewerHalfPageDown, code, modifiers) => {
            scroll_half(app, 1)
        }
        (code, modifiers) if key_matches(KeyAction::ViewerHalfPageUp, code, modifiers) => {
            scroll_half(app, -1)
        }
        (code, modifiers) if key_matches(KeyAction::ViewerPageDown, code, modifiers) => {
            scroll_page(app, 1)
        }
        (code, modifiers) if key_matches(KeyAction::ViewerPageUp, code, modifiers) => {
            scroll_page(app, -1)
        }
        (code, modifiers) if key_matches(KeyAction::ViewerTop, code, modifiers) => {
            set_scroll(app, 0)
        }
        (code, modifiers) if key_matches(KeyAction::ViewerBottom, code, modifiers) => {
            set_scroll(app, u16::MAX)
        }
        (code, modifiers) if key_matches(KeyAction::ViewerExpand, code, modifiers) => {
            set_expand(app, ExpandMode::Expanded)
        }
        (code, modifiers) if key_matches(KeyAction::ViewerCollapse, code, modifiers) => {
            set_expand(app, ExpandMode::Collapsed)
        }
        (code, modifiers) if key_matches(KeyAction::ViewerResume, code, modifiers) => {
            resume_session_from_viewer(app, path);
        }
        (code, modifiers) if key_matches(KeyAction::ViewerDelete, code, modifiers) => {
            prompt_delete_for_path(app, path)
        }
        _ => {}
    }
    false
}

fn handle_delete_confirm(code: KeyCode, modifiers: KeyModifiers, app: &mut App) -> bool {
    match (code, modifiers) {
        (code, modifiers) if key_matches(KeyAction::DeleteCancel, code, modifiers) => {
            app.delete_confirm = None;
            app.state.write().dirty = true;
        }
        (code, modifiers) if key_matches(KeyAction::DeleteConfirm, code, modifiers) => {
            confirm_delete(app);
        }
        _ => {}
    }
    false
}

fn scroll_by(app: &App, delta: i32) {
    let mut s = app.state.write();
    let Some(c) = s.conversation.as_mut() else {
        return;
    };
    let max = c.max_scroll();
    c.scroll = apply_delta(c.scroll, delta).min(max);
    s.dirty = true;
}

fn scroll_half(app: &App, dir: i32) {
    let h = viewport_height(app);
    let step = (h as i32 / 2).max(1) * dir;
    scroll_by(app, step);
}

fn scroll_page(app: &App, dir: i32) {
    let h = viewport_height(app);
    let step = (h as i32).max(1) * dir;
    scroll_by(app, step);
}

fn viewport_height(app: &App) -> u16 {
    app.state
        .read()
        .conversation
        .as_ref()
        .map(|c| c.viewport_height)
        .unwrap_or(20)
        .max(1)
}

fn set_scroll(app: &App, v: u16) {
    let mut s = app.state.write();
    let Some(c) = s.conversation.as_mut() else {
        return;
    };
    let max = c.max_scroll();
    c.scroll = v.min(max);
    s.dirty = true;
}

fn set_expand(app: &App, m: ExpandMode) {
    let mut s = app.state.write();
    let Some(c) = s.conversation.as_mut() else {
        return;
    };
    if c.expand != m {
        c.expand = m;
        // Line positions shift when we change expand mode; reset scroll so the
        // viewport lands on a predictable spot instead of mid-block.
        c.scroll = 0;
        s.dirty = true;
    }
}

fn apply_delta(scroll: u16, delta: i32) -> u16 {
    if delta < 0 {
        scroll.saturating_sub((-delta) as u16)
    } else {
        scroll.saturating_add(delta as u16)
    }
}

/// One-shot rescan across all adapters. Must not clobber tokens: the
/// fast-parse path can't reproduce them, so preserve whatever token_refresh
/// already wrote into state for each path.
fn rescan_sessions(app: &App) {
    let state = app.state.clone();
    let adapters = app.adapters.clone();
    tokio::spawn(async move {
        let mut fresh = Vec::new();
        for adapter in &adapters {
            if let Ok(mut batch) = adapter.scan_all().await {
                fresh.append(&mut batch);
            }
        }
        fresh.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        fs_watch::replace_preserving_tokens(&state, fresh);
    });
}

fn prompt_delete_selected_session(app: &mut App) {
    let Some(path) = current_selected_path(app) else {
        return;
    };
    prompt_delete_for_path(app, &path);
}

fn prompt_delete_for_path(app: &mut App, path: &Path) {
    app.delete_confirm = Some(DeleteConfirm::new(path.to_path_buf()));
    app.state.write().dirty = true;
}

fn confirm_delete(app: &mut App) {
    let Some(path) = app
        .delete_confirm
        .as_ref()
        .map(|confirm| confirm.path.clone())
    else {
        return;
    };

    if let Err(err) = std::fs::metadata(&path) {
        if err.kind() == ErrorKind::NotFound {
            clear_deleted_session(app, &path);
            app.delete_confirm = None;
            return;
        }
    }

    if session_is_active(app, &path) {
        if let Some(confirm) = app.delete_confirm.as_mut() {
            confirm.error = Some("Refusing to delete an active session".to_string());
        }
        app.state.write().dirty = true;
        return;
    }

    match std::fs::remove_file(&path) {
        Ok(()) => {
            clear_deleted_session(app, &path);
            app.delete_confirm = None;
        }
        Err(err) if err.kind() == ErrorKind::NotFound => {
            clear_deleted_session(app, &path);
            app.delete_confirm = None;
        }
        Err(err) => {
            if let Some(confirm) = app.delete_confirm.as_mut() {
                confirm.error = Some(err.to_string());
            }
            app.state.write().dirty = true;
            tracing::warn!(path = %path.display(), ?err, "failed to delete session file");
        }
    }
}

fn clear_deleted_session(app: &App, path: &Path) {
    app.token_cache.remove(path);
    app.state.write().remove_session_path(path);
}

fn session_is_active(app: &App, path: &Path) -> bool {
    app.state
        .read()
        .sessions
        .iter()
        .find(|session| session.path == path)
        .is_some_and(|session| session.status == SessionStatus::Active)
}

/// Resume the currently list-selected session (Normal mode).
fn resume_session(app: &App) {
    let s = app.state.read();
    let visible = s.visible_session_indices(&app.session_filter, app.session_sort);
    let row = visible.get(s.selected_session);
    let meta = row.and_then(|&i| s.sessions.get(i)).cloned();
    drop(s);

    let Some(meta) = meta else {
        return;
    };

    let cmd = match meta.agent {
        "claude" => format!("claude --resume {}", meta.id),
        "codex" => format!("codex resume {}", meta.id),
        _ => return,
    };

    let terminal = crate::settings::get().terminal;
    if let Err(err) = open_terminal_with_command(terminal, meta.cwd.as_deref(), &cmd) {
        tracing::warn!(?err, "failed to open terminal for resume");
    }
}

/// Resume the session currently being viewed (Viewer mode). Falls back to
/// list-selected if the viewer path doesn't match any known session.
fn resume_session_from_viewer(app: &App, viewer_path: &Path) {
    let s = app.state.read();
    let meta = s
        .sessions
        .iter()
        .find(|m| m.path == viewer_path)
        .cloned()
        .or_else(|| {
            // Fallback: use list-selected session.
            let visible = s.visible_session_indices(&app.session_filter, app.session_sort);
            let row = visible.get(s.selected_session)?;
            s.sessions.get(*row).cloned()
        });
    drop(s);

    let Some(meta) = meta else {
        return;
    };

    let cmd = match meta.agent {
        "claude" => format!("claude --resume {}", meta.id),
        "codex" => format!("codex resume {}", meta.id),
        _ => return,
    };

    let terminal = crate::settings::get().terminal;
    if let Err(err) = open_terminal_with_command(terminal, meta.cwd.as_deref(), &cmd) {
        tracing::warn!(?err, "failed to open terminal for resume");
    }
}

/// Build a shell command string that changes to `cwd` (if given) then runs
/// `cmd`. Uses single-quote escaping to prevent shell injection.
fn build_cd_command(cwd: Option<&Path>, cmd: &str) -> String {
    match cwd {
        Some(dir) => {
            let escaped = dir.display().to_string().replace('\'', "'\\''");
            format!("cd '{}' && {}", escaped, cmd)
        }
        None => cmd.to_string(),
    }
}

/// Open a new terminal window, `cd` to `cwd` (if given), then run `cmd`.
/// Uses the user's configured terminal from Settings; falls back to
/// macOS Terminal.app or a generic CLI launch on Linux.
fn open_terminal_with_command(
    terminal: TerminalApp,
    cwd: Option<&Path>,
    cmd: &str,
) -> anyhow::Result<()> {
    match terminal {
        TerminalApp::Terminal => open_terminal_applescript(cwd, cmd),
        TerminalApp::ITerm2 => open_iterm_applescript(cwd, cmd),
        TerminalApp::Warp => open_warp(cwd, cmd),
        _ => open_cli_terminal(terminal, cwd, cmd),
    }
}

#[cfg(target_os = "macos")]
fn open_terminal_applescript(cwd: Option<&Path>, cmd: &str) -> anyhow::Result<()> {
    let full_cmd = build_cd_command(cwd, cmd);
    let escaped = full_cmd.replace('\\', "\\\\").replace('"', "\\\"");
    std::process::Command::new("osascript")
        .arg("-e")
        .arg(format!(
            "tell application \"Terminal\" to do script \"{escaped}\""
        ))
        .spawn()?;
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn open_terminal_applescript(_cwd: Option<&Path>, _cmd: &str) -> anyhow::Result<()> {
    anyhow::bail!("Terminal.app is only available on macOS")
}

#[cfg(target_os = "macos")]
fn open_iterm_applescript(cwd: Option<&Path>, cmd: &str) -> anyhow::Result<()> {
    let full_cmd = build_cd_command(cwd, cmd);
    let escaped = full_cmd.replace('\\', "\\\\").replace('"', "\\\"");
    std::process::Command::new("osascript")
        .arg("-e")
        .arg(format!(
            "tell application \"iTerm\"\n\
             activate\n\
             create window with default profile\n\
             tell current session of current window\n\
             write text \"{escaped}\"\n\
             end tell\n\
             end tell"
        ))
        .spawn()?;
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn open_iterm_applescript(_cwd: Option<&Path>, _cmd: &str) -> anyhow::Result<()> {
    anyhow::bail!("iTerm2 is only available on macOS")
}

#[cfg(target_os = "macos")]
fn open_warp(cwd: Option<&Path>, cmd: &str) -> anyhow::Result<()> {
    // Prefer CLI launch if the warp binary is on PATH — avoids System Events
    // keystroke injection entirely (which needs accessibility permissions and
    // breaks on special characters).
    if crate::settings::which_exists("warp") {
        return open_cli_terminal(TerminalApp::Warp, cwd, cmd);
    }
    // Fallback: open the app and paste the command via clipboard.
    let full_cmd = build_cd_command(cwd, cmd);
    // Copy command to clipboard.
    let mut pbcopy = std::process::Command::new("pbcopy")
        .stdin(std::process::Stdio::piped())
        .spawn()?;
    if let Some(mut stdin) = pbcopy.stdin.take() {
        use std::io::Write;
        let _ = stdin.write_all(full_cmd.as_bytes());
    }
    pbcopy.wait()?;
    // Activate Warp.
    std::process::Command::new("osascript")
        .arg("-e")
        .arg("tell application \"Warp\" to activate")
        .spawn()?;
    // Small delay for Warp to come to foreground.
    std::thread::sleep(std::time::Duration::from_millis(300));
    // Paste via Cmd+V.
    std::process::Command::new("osascript")
        .arg("-e")
        .arg("tell application \"System Events\" to keystroke \"v\" using command down")
        .spawn()?;
    // Press Enter.
    std::process::Command::new("osascript")
        .arg("-e")
        .arg("tell application \"System Events\" to key code 36")
        .spawn()?;
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn open_warp(_cwd: Option<&Path>, _cmd: &str) -> anyhow::Result<()> {
    anyhow::bail!("Warp is only available on macOS")
}

/// Resolve the user's login shell, falling back to `/bin/sh`.
fn user_shell() -> String {
    std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string())
}

/// Launch a CLI-based terminal (Ghostty, Alacritty, Kitty, WezTerm, Warp)
/// that accepts `-e` or equivalent to run a command.
fn open_cli_terminal(terminal: TerminalApp, cwd: Option<&Path>, cmd: &str) -> anyhow::Result<()> {
    let full_cmd = build_cd_command(cwd, cmd);
    let shell = user_shell();
    let (program, args) = match terminal {
        TerminalApp::Ghostty => ("ghostty", vec!["-e", &shell, "-c", &full_cmd] as Vec<_>),
        TerminalApp::Alacritty => ("alacritty", vec!["-e", &shell, "-c", &full_cmd]),
        TerminalApp::Kitty => ("kitty", vec![&shell, "-c", &full_cmd]),
        TerminalApp::WezTerm => ("wezterm", vec!["start", "--", &shell, "-c", &full_cmd]),
        TerminalApp::Warp => ("warp", vec![&shell, "-c", &full_cmd]),
        _ => anyhow::bail!("unsupported terminal: {:?}", terminal),
    };
    std::process::Command::new(program).args(&args).spawn()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::types::{MessageRole, SessionMeta, SessionStatus, TokenStats};
    use crate::adapter::{ClaudeAdapter, CodexAdapter};
    use crate::app::{AppState, SessionSort};
    use crate::collector::metrics::MetricsStore;
    use crate::collector::token_refresh::TokenCache;
    use crate::config::Config;
    use parking_lot::RwLock;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    fn test_app() -> App {
        App {
            config: Config {
                claude_root: None,
                codex_root: None,
                ..Config::default()
            },
            state: Arc::new(RwLock::new(AppState::default())),
            metrics: Arc::new(MetricsStore::new(8)),
            adapters: Vec::new(),
            tab: Tab::Dashboard,
            should_quit: false,
            session_filter: String::new(),
            session_filter_input: false,
            session_sort: SessionSort::default(),
            delete_confirm: None,
            selected_process: 0,
            selected_setting: 0,
            settings_keybindings_open: false,
            selected_keybinding: 0,
            capturing_keybinding: None,
            keybinding_conflict: None,
            token_cache: Arc::new(TokenCache::new()),
        }
    }

    #[test]
    fn settings_right_key_changes_value_before_global_tab_switch() {
        let _guard = crate::settings::test_lock();
        crate::settings::settings().write().keybindings = crate::settings::KeyBindings::default();

        let mut app = test_app();
        app.tab = Tab::Settings;
        app.selected_setting = SettingsItem::Language as usize;
        let original_language = crate::settings::get().language;

        handle_normal(KeyCode::Right, KeyModifiers::empty(), &mut app);

        assert_eq!(app.tab, Tab::Settings);
        assert_ne!(crate::settings::get().language, original_language);
        crate::settings::settings().write().language = original_language;
    }

    #[test]
    fn normal_mode_uses_configured_quit_binding() {
        let _guard = crate::settings::test_lock();
        let original = crate::settings::get().keybindings;
        crate::settings::settings().write().keybindings.set_binding(
            crate::settings::KeyAction::Quit,
            crate::settings::KeyBinding::plain(crate::settings::KeyCodeSpec::Char('x')),
        );

        let mut app = test_app();
        let q_should_exit = handle_normal(KeyCode::Char('q'), KeyModifiers::empty(), &mut app);
        assert!(!q_should_exit);
        assert!(!app.should_quit);

        let x_should_exit = handle_normal(KeyCode::Char('x'), KeyModifiers::empty(), &mut app);
        assert!(x_should_exit);
        assert!(app.should_quit);

        crate::settings::settings().write().keybindings = original;
    }

    #[test]
    fn settings_capture_rebinds_selected_action() {
        let _guard = crate::settings::test_lock();
        let original = crate::settings::get().keybindings;
        crate::settings::settings().write().keybindings = crate::settings::KeyBindings::default();

        let mut app = test_app();
        app.tab = Tab::Settings;
        app.settings_keybindings_open = true;
        app.selected_keybinding = crate::settings::KeyAction::all()
            .iter()
            .position(|action| *action == crate::settings::KeyAction::Quit)
            .unwrap();

        handle_normal(KeyCode::Enter, KeyModifiers::empty(), &mut app);
        assert_eq!(
            app.capturing_keybinding,
            Some(crate::settings::KeyAction::Quit)
        );

        handle_normal(KeyCode::Char('x'), KeyModifiers::empty(), &mut app);
        assert_eq!(app.capturing_keybinding, None);
        assert_eq!(
            crate::settings::get()
                .keybindings
                .bindings_for(crate::settings::KeyAction::Quit)[0]
                .display(),
            "x"
        );

        crate::settings::settings().write().keybindings = original;
    }

    #[test]
    fn settings_keybindings_panel_uses_configured_cancel_binding() {
        let _guard = crate::settings::test_lock();
        let original = crate::settings::get().keybindings;
        crate::settings::settings().write().keybindings = crate::settings::KeyBindings::default();
        crate::settings::settings().write().keybindings.set_binding(
            crate::settings::KeyAction::SettingsCancel,
            crate::settings::KeyBinding::plain(crate::settings::KeyCodeSpec::Char('x')),
        );

        let mut app = test_app();
        app.tab = Tab::Settings;
        app.settings_keybindings_open = true;

        handle_normal(KeyCode::Esc, KeyModifiers::empty(), &mut app);
        assert!(app.settings_keybindings_open);

        handle_normal(KeyCode::Char('x'), KeyModifiers::empty(), &mut app);
        assert!(!app.settings_keybindings_open);

        crate::settings::settings().write().keybindings = original;
    }

    #[test]
    fn settings_capture_rebinds_selected_action_and_updates_normal_dispatch() {
        let _guard = crate::settings::test_lock();
        let original = crate::settings::get().keybindings;
        crate::settings::settings().write().keybindings = crate::settings::KeyBindings::default();

        let mut app = test_app();
        app.tab = Tab::Settings;
        app.settings_keybindings_open = true;
        app.selected_keybinding = crate::settings::KeyAction::all()
            .iter()
            .position(|action| *action == crate::settings::KeyAction::Quit)
            .unwrap();

        handle_normal(KeyCode::Enter, KeyModifiers::empty(), &mut app);
        handle_normal(KeyCode::Char('x'), KeyModifiers::empty(), &mut app);
        app.settings_keybindings_open = false;
        app.tab = Tab::Dashboard;

        let old_should_exit = handle_normal(KeyCode::Char('q'), KeyModifiers::empty(), &mut app);
        assert!(!old_should_exit);
        assert!(!app.should_quit);

        let new_should_exit = handle_normal(KeyCode::Char('x'), KeyModifiers::empty(), &mut app);
        assert!(new_should_exit);
        assert!(app.should_quit);

        crate::settings::settings().write().keybindings = original;
    }

    #[test]
    fn settings_capture_esc_cancels_without_change() {
        let _guard = crate::settings::test_lock();
        let original = crate::settings::get().keybindings;
        crate::settings::settings().write().keybindings = crate::settings::KeyBindings::default();

        let mut app = test_app();
        app.tab = Tab::Settings;
        app.settings_keybindings_open = true;
        app.selected_keybinding = crate::settings::KeyAction::all()
            .iter()
            .position(|action| *action == crate::settings::KeyAction::Quit)
            .unwrap();

        handle_normal(KeyCode::Enter, KeyModifiers::empty(), &mut app);
        handle_normal(KeyCode::Esc, KeyModifiers::empty(), &mut app);
        assert_eq!(app.capturing_keybinding, None);
        assert_eq!(
            crate::settings::get()
                .keybindings
                .bindings_for(crate::settings::KeyAction::Quit)[0]
                .display(),
            "q"
        );

        crate::settings::settings().write().keybindings = original;
    }

    #[test]
    fn esc_does_not_quit_in_normal_mode() {
        let _guard = crate::settings::test_lock();
        crate::settings::settings().write().keybindings = crate::settings::KeyBindings::default();
        let mut app = test_app();

        let should_exit = handle_normal(KeyCode::Esc, KeyModifiers::empty(), &mut app);

        assert!(!should_exit);
        assert!(!app.should_quit);
    }

    #[test]
    fn q_quits_in_normal_mode() {
        let _guard = crate::settings::test_lock();
        crate::settings::settings().write().keybindings = crate::settings::KeyBindings::default();
        let mut app = test_app();

        let should_exit = handle_normal(KeyCode::Char('q'), KeyModifiers::empty(), &mut app);

        assert!(should_exit);
        assert!(app.should_quit);
    }

    #[test]
    fn d_in_sessions_opens_delete_confirmation_without_removing_file() {
        let unique = format!(
            "agentmonitor-delete-open-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("time went backwards")
                .as_nanos()
        );
        let base = std::env::temp_dir().join(unique);
        std::fs::create_dir_all(&base).expect("create temp dir");
        let session_path = base.join("delete-me.jsonl");
        std::fs::write(&session_path, "{}\n").expect("write session");

        let mut app = test_app();
        app.tab = Tab::Sessions;
        app.state.write().sessions = vec![SessionMeta {
            agent: "claude",
            id: "delete-me".into(),
            path: session_path.clone(),
            cwd: Some(base.clone()),
            model: None,
            version: None,
            git_branch: None,
            started_at: None,
            updated_at: None,
            message_count: 0,
            tokens: TokenStats::default(),
            status: SessionStatus::Idle,
            byte_offset: 0,
            size_bytes: 2,
        }];

        let should_exit = handle_normal(KeyCode::Char('d'), KeyModifiers::empty(), &mut app);

        assert!(!should_exit);
        assert_eq!(
            app.delete_confirm
                .as_ref()
                .map(|confirm| confirm.path.as_path()),
            Some(session_path.as_path())
        );
        assert!(session_path.exists(), "first press should not delete yet");

        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn delete_in_sessions_opens_delete_confirmation_without_removing_file() {
        let unique = format!(
            "agentmonitor-delete-key-open-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("time went backwards")
                .as_nanos()
        );
        let base = std::env::temp_dir().join(unique);
        std::fs::create_dir_all(&base).expect("create temp dir");
        let session_path = base.join("delete-me.jsonl");
        std::fs::write(&session_path, "{}\n").expect("write session");

        let mut app = test_app();
        app.tab = Tab::Sessions;
        app.state.write().sessions = vec![SessionMeta {
            agent: "claude",
            id: "delete-me".into(),
            path: session_path.clone(),
            cwd: Some(base.clone()),
            model: None,
            version: None,
            git_branch: None,
            started_at: None,
            updated_at: None,
            message_count: 0,
            tokens: TokenStats::default(),
            status: SessionStatus::Idle,
            byte_offset: 0,
            size_bytes: 2,
        }];

        let should_exit = handle_normal(KeyCode::Delete, KeyModifiers::empty(), &mut app);

        assert!(!should_exit);
        assert_eq!(
            app.delete_confirm
                .as_ref()
                .map(|confirm| confirm.path.as_path()),
            Some(session_path.as_path())
        );
        assert!(
            session_path.exists(),
            "Delete should not remove until confirmed"
        );

        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn delete_in_viewer_opens_delete_confirmation() {
        let unique = format!(
            "agentmonitor-viewer-delete-key-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("time went backwards")
                .as_nanos()
        );
        let base = std::env::temp_dir().join(unique);
        std::fs::create_dir_all(&base).expect("create temp dir");
        let session_path = base.join("delete-me.jsonl");
        std::fs::write(&session_path, "{}\n").expect("write session");

        let mut app = test_app();
        {
            let mut state = app.state.write();
            state.sessions = vec![SessionMeta {
                agent: "claude",
                id: "delete-me".into(),
                path: session_path.clone(),
                cwd: Some(base.clone()),
                model: None,
                version: None,
                git_branch: None,
                started_at: None,
                updated_at: None,
                message_count: 0,
                tokens: TokenStats::default(),
                status: SessionStatus::Idle,
                byte_offset: 0,
                size_bytes: 2,
            }];
            state.mode = Mode::Viewer {
                path: session_path.clone(),
            };
        }

        let should_exit = handle_viewer(
            KeyCode::Delete,
            KeyModifiers::empty(),
            &mut app,
            &session_path,
        );

        assert!(!should_exit);
        assert_eq!(
            app.delete_confirm
                .as_ref()
                .map(|confirm| confirm.path.as_path()),
            Some(session_path.as_path())
        );
        assert!(
            session_path.exists(),
            "Delete should only open confirmation"
        );

        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn enter_with_delete_confirmation_removes_file_and_clears_viewer_state() {
        let unique = format!(
            "agentmonitor-delete-confirm-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("time went backwards")
                .as_nanos()
        );
        let base = std::env::temp_dir().join(unique);
        std::fs::create_dir_all(&base).expect("create temp dir");
        let session_path = base.join("delete-me.jsonl");
        std::fs::write(&session_path, "{\"type\":\"user\"}\n").expect("write session");

        let mut app = test_app();
        app.tab = Tab::Sessions;
        app.state.write().sessions = vec![SessionMeta {
            agent: "claude",
            id: "delete-me".into(),
            path: session_path.clone(),
            cwd: Some(base.clone()),
            model: None,
            version: None,
            git_branch: None,
            started_at: None,
            updated_at: None,
            message_count: 1,
            tokens: TokenStats::default(),
            status: SessionStatus::Idle,
            byte_offset: 0,
            size_bytes: 16,
        }];
        {
            let mut state = app.state.write();
            state.preview = Some(PreviewCache {
                path: session_path.clone(),
                messages: Vec::new(),
                message_count: 1,
                loading: false,
            });
            state.mode = Mode::Viewer {
                path: session_path.clone(),
            };
            state.conversation = Some(ConversationCache::loading(session_path.clone()));
        }
        app.delete_confirm = Some(DeleteConfirm::new(session_path.clone()));

        let should_exit = handle_event(
            Event::Key(crossterm::event::KeyEvent::new(
                KeyCode::Enter,
                KeyModifiers::empty(),
            )),
            &mut app,
        );

        assert!(!should_exit);
        assert!(
            !session_path.exists(),
            "confirmed delete should remove the session file"
        );
        assert!(
            app.delete_confirm.is_none(),
            "prompt should close after success"
        );
        let state = app.state.read();
        assert!(state.sessions.is_empty());
        assert!(state.preview.is_none());
        assert!(state.conversation.is_none());
        assert!(matches!(state.mode, Mode::Normal));

        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn y_with_delete_confirmation_removes_file_and_clears_state() {
        let unique = format!(
            "agentmonitor-delete-key-confirm-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("time went backwards")
                .as_nanos()
        );
        let base = std::env::temp_dir().join(unique);
        std::fs::create_dir_all(&base).expect("create temp dir");
        let session_path = base.join("delete-me.jsonl");
        std::fs::write(&session_path, "{\"type\":\"user\"}\n").expect("write session");

        let mut app = test_app();
        app.tab = Tab::Sessions;
        app.state.write().sessions = vec![SessionMeta {
            agent: "claude",
            id: "delete-me".into(),
            path: session_path.clone(),
            cwd: Some(base.clone()),
            model: None,
            version: None,
            git_branch: None,
            started_at: None,
            updated_at: None,
            message_count: 1,
            tokens: TokenStats::default(),
            status: SessionStatus::Idle,
            byte_offset: 0,
            size_bytes: 16,
        }];
        app.delete_confirm = Some(DeleteConfirm::new(session_path.clone()));

        let should_exit = handle_event(
            Event::Key(crossterm::event::KeyEvent::new(
                KeyCode::Char('y'),
                KeyModifiers::empty(),
            )),
            &mut app,
        );

        assert!(!should_exit);
        assert!(!session_path.exists(), "y should confirm deletion");
        assert!(app.delete_confirm.is_none());
        assert!(app.state.read().sessions.is_empty());

        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn delete_with_delete_confirmation_does_not_confirm_delete() {
        let unique = format!(
            "agentmonitor-delete-key-no-confirm-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("time went backwards")
                .as_nanos()
        );
        let base = std::env::temp_dir().join(unique);
        std::fs::create_dir_all(&base).expect("create temp dir");
        let session_path = base.join("delete-me.jsonl");
        std::fs::write(&session_path, "{\"type\":\"user\"}\n").expect("write session");

        let mut app = test_app();
        app.delete_confirm = Some(DeleteConfirm::new(session_path.clone()));

        let should_exit = handle_event(
            Event::Key(crossterm::event::KeyEvent::new(
                KeyCode::Delete,
                KeyModifiers::empty(),
            )),
            &mut app,
        );

        assert!(!should_exit);
        assert!(session_path.exists(), "Delete should not confirm deletion");
        assert!(app.delete_confirm.is_some());

        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn active_session_delete_confirmation_keeps_file_and_prompt() {
        let unique = format!(
            "agentmonitor-delete-active-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("time went backwards")
                .as_nanos()
        );
        let base = std::env::temp_dir().join(unique);
        std::fs::create_dir_all(&base).expect("create temp dir");
        let session_path = base.join("active.jsonl");
        std::fs::write(&session_path, "{\"type\":\"user\"}\n").expect("write session");

        let mut app = test_app();
        app.state.write().sessions = vec![SessionMeta {
            agent: "claude",
            id: "active".into(),
            path: session_path.clone(),
            cwd: Some(base.clone()),
            model: None,
            version: None,
            git_branch: None,
            started_at: None,
            updated_at: None,
            message_count: 1,
            tokens: TokenStats::default(),
            status: SessionStatus::Active,
            byte_offset: 0,
            size_bytes: 16,
        }];
        app.delete_confirm = Some(DeleteConfirm::new(session_path.clone()));

        let should_exit = handle_event(
            Event::Key(crossterm::event::KeyEvent::new(
                KeyCode::Enter,
                KeyModifiers::empty(),
            )),
            &mut app,
        );

        assert!(!should_exit);
        assert!(session_path.exists(), "active sessions should not be deleted");
        assert!(app.delete_confirm.is_some());
        assert!(app
            .delete_confirm
            .as_ref()
            .and_then(|confirm| confirm.error.as_ref())
            .is_some());
        assert_eq!(app.state.read().sessions.len(), 1);

        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn missing_active_session_confirm_clears_prompt_and_cached_state() {
        let unique = format!(
            "agentmonitor-delete-missing-active-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("time went backwards")
                .as_nanos()
        );
        let base = std::env::temp_dir().join(unique);
        std::fs::create_dir_all(&base).expect("create temp dir");
        let session_path = base.join("missing-active.jsonl");

        let mut app = test_app();
        {
            let mut state = app.state.write();
            state.sessions = vec![SessionMeta {
                agent: "claude",
                id: "active".into(),
                path: session_path.clone(),
                cwd: Some(base.clone()),
                model: None,
                version: None,
                git_branch: None,
                started_at: None,
                updated_at: None,
                message_count: 1,
                tokens: TokenStats::default(),
                status: SessionStatus::Active,
                byte_offset: 0,
                size_bytes: 16,
            }];
            state.preview = Some(PreviewCache {
                path: session_path.clone(),
                messages: Vec::new(),
                message_count: 1,
                loading: false,
            });
            state.mode = Mode::Viewer {
                path: session_path.clone(),
            };
            state.conversation = Some(ConversationCache::loading(session_path.clone()));
        }
        app.delete_confirm = Some(DeleteConfirm::new(session_path.clone()));

        let should_exit = handle_event(
            Event::Key(crossterm::event::KeyEvent::new(
                KeyCode::Enter,
                KeyModifiers::empty(),
            )),
            &mut app,
        );

        assert!(!should_exit);
        assert!(app.delete_confirm.is_none());
        let state = app.state.read();
        assert!(state.sessions.is_empty());
        assert!(state.preview.is_none());
        assert!(state.conversation.is_none());
        assert!(matches!(state.mode, Mode::Normal));

        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn esc_with_delete_confirmation_cancels_without_removing_file() {
        let unique = format!(
            "agentmonitor-delete-cancel-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("time went backwards")
                .as_nanos()
        );
        let base = std::env::temp_dir().join(unique);
        std::fs::create_dir_all(&base).expect("create temp dir");
        let session_path = base.join("delete-me.jsonl");
        std::fs::write(&session_path, "{}\n").expect("write session");

        let mut app = test_app();
        app.delete_confirm = Some(DeleteConfirm::new(session_path.clone()));

        let should_exit = handle_event(
            Event::Key(crossterm::event::KeyEvent::new(
                KeyCode::Esc,
                KeyModifiers::empty(),
            )),
            &mut app,
        );

        assert!(!should_exit);
        assert!(app.delete_confirm.is_none());
        assert!(session_path.exists(), "cancel should keep the session file");

        let _ = std::fs::remove_dir_all(base);
    }

    #[tokio::test]
    async fn preview_loader_uses_codex_adapter_for_codex_sessions() {
        let unique = format!(
            "agentmonitor-preview-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("time went backwards")
                .as_nanos()
        );
        let base = std::env::temp_dir().join(unique);
        let claude_root = base.join(".claude").join("projects");
        let codex_root = base.join(".codex").join("sessions");
        let session_path = codex_root
            .join("2026")
            .join("04")
            .join("25")
            .join("rollout-2026-04-25T16-21-21-019dc3ba-a222-7631-96e6-2e1ebb238e53.jsonl");
        std::fs::create_dir_all(&claude_root).expect("create claude root");
        std::fs::create_dir_all(session_path.parent().expect("session dir"))
            .expect("create codex session dir");
        std::fs::write(
            &session_path,
            concat!(
                r#"{"timestamp":"2026-04-25T08:21:38.973Z","type":"session_meta","payload":{"id":"019dc3ba-a222-7631-96e6-2e1ebb238e53","timestamp":"2026-04-25T08:21:21.574Z","cwd":"/tmp/repo","originator":"Codex Desktop","cli_version":"0.125.0-alpha.3","model_provider":"openai"}}"#,
                "\n",
                r#"{"timestamp":"2026-04-25T08:21:56.528Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"hello from user"}]}}"#,
                "\n",
                r#"{"timestamp":"2026-04-25T08:22:07.734Z","type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"hello from assistant"}]}}"#,
                "\n"
            ),
        )
        .expect("write codex session");

        let app = App {
            config: Config {
                claude_root: Some(claude_root.clone()),
                codex_root: Some(codex_root.clone()),
                ..Config::default()
            },
            state: Arc::new(RwLock::new(AppState {
                sessions: vec![SessionMeta {
                    agent: "codex",
                    id: "019dc3ba-a222-7631-96e6-2e1ebb238e53".into(),
                    path: session_path.clone(),
                    cwd: Some(PathBuf::from("/tmp/repo")),
                    model: Some("openai".into()),
                    version: Some("0.125.0-alpha.3".into()),
                    git_branch: None,
                    started_at: None,
                    updated_at: None,
                    message_count: 0,
                    tokens: TokenStats::default(),
                    status: SessionStatus::Idle,
                    byte_offset: 0,
                    size_bytes: 0,
                }],
                selected_session: 0,
                dirty: false,
                preview: None,
                mode: Mode::Normal,
                conversation: None,
            })),
            metrics: Arc::new(MetricsStore::new(8)),
            adapters: vec![
                Arc::new(ClaudeAdapter::new(Some(claude_root))),
                Arc::new(CodexAdapter::new(Some(codex_root))),
            ],
            tab: Tab::Sessions,
            should_quit: false,
            session_filter: String::new(),
            session_filter_input: false,
            session_sort: SessionSort::default(),
            delete_confirm: None,
            selected_process: 0,
            selected_setting: 0,
            settings_keybindings_open: false,
            selected_keybinding: 0,
            capturing_keybinding: None,
            keybinding_conflict: None,
            token_cache: Arc::new(TokenCache::new()),
        };

        maybe_load_preview(&app);

        let mut loaded = None;
        for _ in 0..50 {
            {
                let s = app.state.read();
                if let Some(cache) = &s.preview {
                    if !cache.loading {
                        loaded = Some(cache.clone());
                        break;
                    }
                }
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        let preview = loaded.expect("preview should finish loading");
        assert_eq!(
            preview.messages.len(),
            2,
            "preview should show codex messages"
        );
        assert!(matches!(preview.messages[0].role, MessageRole::User));
        assert_eq!(preview.messages[0].text, "hello from user");
        assert!(matches!(preview.messages[1].role, MessageRole::Assistant));
        assert_eq!(preview.messages[1].text, "hello from assistant");

        let _ = std::fs::remove_dir_all(base);
    }
}
