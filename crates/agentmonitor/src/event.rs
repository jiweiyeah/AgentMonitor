use std::io::ErrorKind;
use std::io::Stdout;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Result;
use crossterm::event::{Event, EventStream, KeyCode, KeyEventKind, KeyModifiers};
use futures::StreamExt;
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;

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
    // The render-trigger and token-refresh-trigger Notifies live on `App` so
    // any spawned task (including ones created by event handlers) can wake the
    // loop without having to be threaded the handle. Take clones here for the
    // collectors and the local select.
    let dirty = app.dirty.clone();
    let token_dirty = app.token_dirty.clone();

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
    let cache_fs = app.token_cache.clone();
    let diagnostics_fs = app.diagnostics.clone();
    let dirty_fs = dirty.clone();
    let token_dirty_fs = token_dirty.clone();
    tokio::spawn(async move {
        fs_watch::run_with_cache(
            adapters_fs,
            state_fs,
            Some(cache_fs),
            Some(diagnostics_fs),
            token_dirty_fs,
            dirty_fs,
        )
        .await;
    });

    let adapters_tok = app.adapters.clone();
    let state_tok = state.clone();
    let cache_tok = app.token_cache.clone();
    let trend_tok = app.token_trend.clone();
    let diagnostics_tok = app.diagnostics.clone();
    let dirty_tok = dirty.clone();
    let token_dirty_tok = token_dirty.clone();
    tokio::spawn(async move {
        token_refresh::run(
            adapters_tok,
            state_tok,
            cache_tok,
            trend_tok,
            diagnostics_tok,
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
    // Help overlay is modal: any keypress dismisses it and is NOT dispatched
    // to the underlying tab. This makes `?` discoverable without surprising
    // the user with side effects (e.g. `q` quitting after they hit `?`).
    if app.state.read().show_help {
        let mut s = app.state.write();
        s.show_help = false;
        s.dirty = true;
        return false;
    }
    // `?` toggles help open. Checked before delete-confirm / mode dispatch so
    // it works in every context. Delete-confirm modal still wins in the next
    // step if it's already open.
    if !app.state.read().show_help
        && app.delete_confirm.is_none()
        && key_matches(KeyAction::ShowHelp, key.code, key.modifiers)
    {
        let mut s = app.state.write();
        s.show_help = true;
        s.dirty = true;
        return false;
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
    // Clear any transient toast before processing the next key.
    {
        let mut state = app.state.write();
        if state.toast.is_some() {
            state.toast = None;
            state.dirty = true;
        }
    }
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
        (code, modifiers) if key_matches(KeyAction::Star, code, modifiers) => {
            open_github_star(app);
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
            jump_dashboard(app);
            true
        }
        (code, modifiers)
            if app.tab == Tab::Dashboard
                && key_matches(KeyAction::DashboardCycleCursor, code, modifiers) =>
        {
            cycle_dashboard_cursor(app);
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
        (code, modifiers)
            if app.tab == Tab::Sessions
                && key_matches(KeyAction::SessionsToggleStar, code, modifiers) =>
        {
            toggle_selected_session_star(app);
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
    let idx = app.selected_setting.min(items.len());
    if idx == items.len() {
        // GitHub link row — open in browser.
        open_url("https://github.com/jiweiyeah/AgentMonitor", app);
        return;
    }
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
    let idx = app.selected_setting.min(items.len());
    if idx == items.len() {
        // GitHub link row — open in browser on any cycle attempt.
        open_url("https://github.com/jiweiyeah/AgentMonitor", app);
        return;
    }
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
            // Dashboard now has TWO cursor modes: Process and Project. j/k
            // walks rows of whichever panel is focused; `Tab` toggles.
            match app.dashboard_cursor {
                crate::app::DashboardCursor::Process => {
                    let len = app.metrics.snapshot().len();
                    if len == 0 {
                        return;
                    }
                    app.selected_process = clamp_step(app.selected_process, delta, len);
                    app.state.write().dirty = true;
                }
                crate::app::DashboardCursor::Project => {
                    // Recompute the projects list snapshot to clamp.
                    let len = current_top_project_count(app);
                    if len == 0 {
                        return;
                    }
                    app.selected_project = clamp_step(app.selected_project, delta, len);
                    app.state.write().dirty = true;
                }
            }
        }
        Tab::Settings => {
            // +1 for the GitHub link row appended after SettingsItem::all().
            let len = SettingsItem::all().len() + 1;
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

/// Enter dispatch on the Dashboard. Branches on which panel cursor is on.
fn jump_dashboard(app: &mut App) {
    match app.dashboard_cursor {
        crate::app::DashboardCursor::Process => jump_to_selected_process_session(app),
        crate::app::DashboardCursor::Project => jump_to_selected_project(app),
    }
}

/// Tab inside Dashboard cycles between the Process panel and the Top
/// Projects panel.
fn cycle_dashboard_cursor(app: &mut App) {
    app.dashboard_cursor = app.dashboard_cursor.toggle();
    // Selection in the now-focused panel may be out of bounds (e.g. user has
    // selected_project=5 but there are only 3 project rows). Clamp here.
    match app.dashboard_cursor {
        crate::app::DashboardCursor::Process => {
            let len = app.metrics.snapshot().len();
            app.selected_process = app.selected_process.min(len.saturating_sub(1));
        }
        crate::app::DashboardCursor::Project => {
            let len = current_top_project_count(app);
            app.selected_project = app.selected_project.min(len.saturating_sub(1));
        }
    }
    app.state.write().dirty = true;
}

/// Compute the same Top Projects snapshot the Dashboard renderer uses, so
/// j/k clamping and Enter dispatch stay consistent with the visible list.
/// Cap matches the renderer's truncation in `tui::dashboard::render`: we
/// truncate to a generous 100 because the actual visible row count depends
/// on the terminal height the renderer doesn't know about here. j/k clamping
/// would otherwise diverge from "what's on screen", but in practice the user
/// only sees ≤ visible_rows projects, so clamping to 100 is a strict
/// superset that doesn't matter for selection logic.
fn current_top_project_count(app: &App) -> usize {
    let s = app.state.read();
    crate::tui::stats::top_projects(&s.sessions, 100).len()
}

/// Enter on a project row: set a `cwd:<path>` filter and switch to the
/// Sessions tab. The user lands on the first session matching that cwd.
fn jump_to_selected_project(app: &mut App) {
    let cwd = {
        let s = app.state.read();
        let projects = crate::tui::stats::top_projects(&s.sessions, 100);
        projects
            .get(app.selected_project)
            .map(|row| row.cwd.clone())
    };
    let Some(cwd) = cwd else {
        return;
    };

    // The filter syntax is `cwd:<substring>`; the project's cwd is already
    // an exact path. We pass it through as-is; `session_matches_token` does
    // substring matching, so any slashes / spaces in the path are fine
    // (split_whitespace tokenizes the filter, but a single token is enough
    // for unique-cwd matching, and project paths rarely contain whitespace).
    app.session_filter = format!("cwd:{}", cwd);
    app.session_filter_input = false;
    app.tab = Tab::Sessions;
    {
        let mut state = app.state.write();
        state.selected_session = 0;
        state.dirty = true;
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
    // If the user is currently typing a search query, swallow normal viewer
    // shortcuts and route the keypress to the search input handler. We
    // deliberately check this before any `key_matches` calls so a binding for
    // (e.g.) `q` doesn't quit the viewer mid-search.
    if viewer_search_is_editing(app) {
        handle_viewer_search_input(code, modifiers, app);
        return false;
    }
    match (code, modifiers) {
        (code, modifiers) if key_matches(KeyAction::ViewerSearchCancel, code, modifiers)
            && viewer_has_active_search(app) =>
        {
            // Esc on a committed search clears it without leaving the viewer.
            // ViewerBack still wins when there's no active search (handled below).
            clear_viewer_search(app);
        }
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
        (code, modifiers) if key_matches(KeyAction::ViewerSearchStart, code, modifiers) => {
            start_viewer_search(app);
        }
        (code, modifiers) if key_matches(KeyAction::ViewerSearchNext, code, modifiers) => {
            advance_viewer_search(app, 1);
        }
        (code, modifiers) if key_matches(KeyAction::ViewerSearchPrev, code, modifiers) => {
            advance_viewer_search(app, -1);
        }
        (code, modifiers) if key_matches(KeyAction::ViewerExportMarkdown, code, modifiers) => {
            export_viewer_session(app, path);
        }
        (code, modifiers) if key_matches(KeyAction::ViewerCopyToClipboard, code, modifiers) => {
            copy_viewer_session(app, path);
        }
        _ => {}
    }
    false
}

/// True if the user is currently typing inside the viewer search bar.
/// Calls into the conversation cache, so it must not be called while the
/// state lock is held by the caller.
fn viewer_search_is_editing(app: &App) -> bool {
    let s = app.state.read();
    s.conversation
        .as_ref()
        .and_then(|c| c.search.as_ref())
        .is_some_and(|sr| sr.editing)
}

fn viewer_has_active_search(app: &App) -> bool {
    let s = app.state.read();
    s.conversation
        .as_ref()
        .and_then(|c| c.search.as_ref())
        .is_some_and(|sr| !sr.query.is_empty() || sr.editing)
}

/// `/` pressed in viewer mode. Open the search bar in editing mode, clearing
/// any prior query so the user starts fresh.
fn start_viewer_search(app: &App) {
    let mut s = app.state.write();
    let Some(cache) = s.conversation.as_mut() else {
        return;
    };
    cache.search = Some(crate::app::ViewerSearch {
        query: String::new(),
        matches: Vec::new(),
        current: 0,
        editing: true,
    });
    s.dirty = true;
}

/// Drop the current search entirely (`Esc` on a committed search). Frees up
/// the search-bar row and removes any highlight from the body.
fn clear_viewer_search(app: &App) {
    let mut s = app.state.write();
    let Some(cache) = s.conversation.as_mut() else {
        return;
    };
    cache.search = None;
    s.dirty = true;
}

/// Handle keystrokes while the search bar is in editing mode. Esc cancels
/// the search outright; Enter commits + computes matches and jumps to the
/// nearest one; Backspace deletes one char and recomputes; printable chars
/// append + recompute.
fn handle_viewer_search_input(code: KeyCode, modifiers: KeyModifiers, app: &mut App) {
    if key_matches(KeyAction::ViewerSearchCancel, code, modifiers) {
        clear_viewer_search(app);
        return;
    }
    if key_matches(KeyAction::FilterApply, code, modifiers) {
        commit_viewer_search(app);
        return;
    }
    if key_matches(KeyAction::FilterDeleteChar, code, modifiers) {
        let mut s = app.state.write();
        if let Some(cache) = s.conversation.as_mut() {
            if let Some(sr) = cache.search.as_mut() {
                sr.query.pop();
            }
        }
        drop(s);
        // Live-recompute on every keypress so the user sees match count update.
        recompute_search_matches(app);
        return;
    }
    if let KeyCode::Char(c) = code {
        if !c.is_control() && !modifiers.contains(KeyModifiers::CONTROL) {
            let mut s = app.state.write();
            if let Some(cache) = s.conversation.as_mut() {
                if let Some(sr) = cache.search.as_mut() {
                    sr.query.push(c);
                }
            }
            drop(s);
            recompute_search_matches(app);
        }
    }
}

/// Switch from editing → committed: matches were already live-computed by the
/// keystroke handler, so all we do is flip the editing flag and scroll to the
/// current match if any. No-op if the query is still empty.
fn commit_viewer_search(app: &App) {
    let mut s = app.state.write();
    let Some(cache) = s.conversation.as_mut() else {
        return;
    };
    let Some(sr) = cache.search.as_mut() else {
        return;
    };
    if sr.query.is_empty() {
        cache.search = None;
        s.dirty = true;
        return;
    }
    sr.editing = false;
    let target_line = sr.matches.get(sr.current).copied();
    if let Some(line) = target_line {
        cache.scroll = line as u16;
    }
    s.dirty = true;
}

/// Reuse viewer's `refresh_search_matches` to recompute the match index list
/// against the current events + expand mode. This is cheap on small sessions
/// and stays under a few ms even for transcripts north of 100K lines.
fn recompute_search_matches(app: &App) {
    let mut s = app.state.write();
    let Some(cache) = s.conversation.as_mut() else {
        return;
    };
    crate::tui::viewer::refresh_search_matches(cache);
    s.dirty = true;
}

/// `n` / `N` after commit: step current match forward (delta=1) or backward
/// (delta=-1) with wrap-around, scrolling to the new line. Does nothing if
/// no committed matches.
fn advance_viewer_search(app: &App, delta: i32) {
    let mut s = app.state.write();
    let Some(cache) = s.conversation.as_mut() else {
        return;
    };
    let Some(sr) = cache.search.as_mut() else {
        return;
    };
    if sr.matches.is_empty() {
        return;
    }
    let len = sr.matches.len() as i32;
    let next = (sr.current as i32 + delta).rem_euclid(len) as usize;
    sr.current = next;
    let target = sr.matches[next] as u16;
    cache.scroll = target;
    s.dirty = true;
}

/// Snapshot the current viewer's events without holding the state read lock
/// across the (potentially slow) markdown render. Returns None if the viewer
/// is in an empty/error state.
fn snapshot_viewer_events(app: &App, path: &Path) -> Option<(String, String, Vec<crate::adapter::conversation::ConversationEvent>)> {
    let s = app.state.read();
    let cache = s.conversation.as_ref()?;
    if cache.path != path || cache.events.is_empty() {
        return None;
    }
    let session = s.sessions.iter().find(|m| m.path == path)?;
    Some((session.agent.to_string(), session.short_id(), cache.events.clone()))
}

/// `E` in viewer: render the current conversation as Markdown and write it
/// to `~/Downloads/agent-monitor/<agent>-<short_id>.md`. Toast on completion.
fn export_viewer_session(app: &App, path: &Path) {
    let Some((agent, short_id, events)) = snapshot_viewer_events(app, path) else {
        let mut s = app.state.write();
        s.toast = Some("Nothing to export — viewer is empty".into());
        s.dirty = true;
        return;
    };
    let target = match crate::export::default_export_path(&agent, &short_id) {
        Ok(p) => p,
        Err(err) => {
            tracing::warn!(?err, "export path build failed");
            let mut s = app.state.write();
            s.toast = Some(format!("Export failed: {err}"));
            s.dirty = true;
            return;
        }
    };
    let md = crate::export::to_markdown(&events);
    match crate::export::write_atomic(&target, &md) {
        Ok(()) => {
            let mut s = app.state.write();
            s.toast = Some(format!("Exported to {}", target.display()));
            s.dirty = true;
        }
        Err(err) => {
            tracing::warn!(?err, target = %target.display(), "export write failed");
            let mut s = app.state.write();
            s.toast = Some(format!("Export failed: {err}"));
            s.dirty = true;
        }
    }
}

/// `y` in viewer: render Markdown and pipe it to the platform clipboard tool.
fn copy_viewer_session(app: &App, path: &Path) {
    let Some((_, _, events)) = snapshot_viewer_events(app, path) else {
        let mut s = app.state.write();
        s.toast = Some("Nothing to copy — viewer is empty".into());
        s.dirty = true;
        return;
    };
    let md = crate::export::to_markdown(&events);
    match crate::export::copy_to_clipboard(&md) {
        Ok(()) => {
            let mut s = app.state.write();
            s.toast = Some(format!("Copied {} chars to clipboard", md.len()));
            s.dirty = true;
        }
        Err(err) => {
            tracing::warn!(?err, "clipboard copy failed");
            let mut s = app.state.write();
            s.toast = Some(format!("Copy failed: {err}"));
            s.dirty = true;
        }
    }
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

fn open_url(url: &str, app: &mut App) {
    // Both `open` (macOS) and `xdg-open` (Linux) return immediately after
    // forking the actual handler, so the synchronous `spawn()` here is
    // already non-blocking. We still wrap in tokio::spawn for consistency
    // with the rest of the event-handler "fire and forget" model and so
    // future Windows support (#17) can use a longer-lived `Start-Process`
    // without re-architecting.
    let url_owned = url.to_string();
    tokio::spawn(async move {
        let res = if cfg!(target_os = "macos") {
            tokio::process::Command::new("open").arg(&url_owned).status().await
        } else {
            tokio::process::Command::new("xdg-open").arg(&url_owned).status().await
        };
        if let Err(err) = res {
            tracing::warn!(?err, "failed to open browser");
        }
    });
    app.state.write().toast = Some("Opening in browser...".into());
    app.state.write().dirty = true;
}

/// Star the project on GitHub. Tries `gh api` if available — but in an async
/// task so the network round-trip never blocks the TUI event loop. The user
/// sees a "checking…" toast immediately and a "starred!" / "already starred"
/// toast on completion.
fn open_github_star(app: &mut App) {
    let repo = "jiweiyeah/AgentMonitor";
    let dirty = app.dirty.clone();
    let state = app.state.clone();

    // If `gh` isn't on PATH, skip the API path entirely and just open browser.
    if !crate::settings::which_exists("gh") {
        open_url(&format!("https://github.com/{}", repo), app);
        return;
    }

    // Show the in-flight toast immediately so the user knows their `*` was
    // received even if the network is slow.
    app.state.write().toast = Some("Checking GitHub star…".into());
    app.state.write().dirty = true;

    tokio::spawn(async move {
        let outcome = star_via_gh_api(repo).await;
        match outcome {
            StarOutcome::Already => {
                crate::settings::update(|s| {
                    s.star_status = crate::settings::StarStatus::Starred
                });
                let mut s = state.write();
                s.toast = Some("Already starred on GitHub".into());
                s.dirty = true;
            }
            StarOutcome::Starred => {
                crate::settings::update(|s| {
                    s.star_status = crate::settings::StarStatus::Starred
                });
                let mut s = state.write();
                s.toast = Some("Starred on GitHub!".into());
                s.dirty = true;
            }
            StarOutcome::Failed(reason) => {
                tracing::warn!(?reason, "gh api star fell back to browser");
                {
                    let mut s = state.write();
                    s.toast = Some("Opening GitHub in browser…".into());
                    s.dirty = true;
                } // drop guard before awaiting
                let url = format!("https://github.com/{}", repo);
                let res = if cfg!(target_os = "macos") {
                    tokio::process::Command::new("open").arg(&url).status().await
                } else {
                    tokio::process::Command::new("xdg-open").arg(&url).status().await
                };
                if let Err(err) = res {
                    tracing::warn!(?err, "failed to open browser fallback");
                }
            }
        }
        dirty.notify_one();
    });
}

/// Outcome of one `gh api` round-trip. Kept simple — we only care whether to
/// flip the persisted star status and what to say in the toast.
enum StarOutcome {
    Already,
    Starred,
    Failed(String),
}

async fn star_via_gh_api(repo: &str) -> StarOutcome {
    // Step 1: check current state. The endpoint returns 204 (success) when
    // starred and 404 (failure) when not.
    let check = tokio::process::Command::new("gh")
        .args(["api", &format!("user/starred/{}", repo)])
        .output()
        .await;
    match check {
        Ok(out) if out.status.success() => return StarOutcome::Already,
        Ok(_) => {
            // 404 — not starred yet. Continue to PUT.
        }
        Err(err) => return StarOutcome::Failed(format!("check: {err}")),
    }

    // Step 2: actually star.
    let put = tokio::process::Command::new("gh")
        .args(["api", "-X", "PUT", &format!("user/starred/{}", repo)])
        .output()
        .await;
    match put {
        Ok(out) if out.status.success() => StarOutcome::Starred,
        Ok(out) => {
            let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
            StarOutcome::Failed(stderr)
        }
        Err(err) => StarOutcome::Failed(format!("put: {err}")),
    }
}

fn prompt_delete_selected_session(app: &mut App) {
    let Some(path) = current_selected_path(app) else {
        return;
    };
    prompt_delete_for_path(app, &path);
}

/// Toggle the bookmark on the currently-selected session. Persists to disk
/// via `settings::toggle_starred`. Triggers a redraw so the star icon and
/// the new sort position appear immediately.
fn toggle_selected_session_star(app: &mut App) {
    let Some(path) = current_selected_path(app) else {
        return;
    };
    crate::settings::toggle_starred(&path);
    let mut s = app.state.write();
    s.dirty = true;
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
        "hermes" => format!("hermes --resume {}", meta.id),
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
        "hermes" => format!("hermes --resume {}", meta.id),
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
        TerminalApp::WindowsTerminal => open_windows_terminal(cwd, cmd),
        TerminalApp::PowerShell => open_powershell(cwd, cmd),
        TerminalApp::Cmd => open_cmd(cwd, cmd),
        _ => open_cli_terminal(terminal, cwd, cmd),
    }
}

/// Build the command-line that runs in the user's home shell after `cd`.
/// Same single-quote escaping as `build_cd_command` but for Windows we use
/// `cd /d` (cmd.exe) so cross-drive jumps work without a sneaky `pushd`.
#[cfg(target_os = "windows")]
fn build_cd_command_windows(cwd: Option<&Path>, cmd: &str) -> String {
    match cwd {
        Some(dir) => {
            let escaped = dir.display().to_string().replace('"', "\\\"");
            format!("cd /d \"{escaped}\" && {cmd}")
        }
        None => cmd.to_string(),
    }
}

/// Build a PowerShell-friendly composite. PS uses `Set-Location` and the
/// statement separator is `;`. Single quotes inside the path are doubled
/// for PowerShell's literal-string escaping.
#[cfg(target_os = "windows")]
fn build_cd_command_powershell(cwd: Option<&Path>, cmd: &str) -> String {
    match cwd {
        Some(dir) => {
            let escaped = dir.display().to_string().replace('\'', "''");
            format!("Set-Location -LiteralPath '{escaped}'; {cmd}")
        }
        None => cmd.to_string(),
    }
}

#[cfg(target_os = "windows")]
fn open_windows_terminal(cwd: Option<&Path>, cmd: &str) -> anyhow::Result<()> {
    let cwd_str = cwd
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| ".".to_string());
    // wt.exe accepts `-d <dir>` for the new tab's working directory and
    // forwards the rest of argv to the chosen profile's command. We use
    // `cmd /k` so the user gets a persistent shell window rather than one
    // that exits immediately when `cmd` finishes.
    std::process::Command::new("wt.exe")
        .arg("-d")
        .arg(&cwd_str)
        .arg("cmd")
        .arg("/k")
        .arg(cmd)
        .spawn()?;
    Ok(())
}

#[cfg(not(target_os = "windows"))]
fn open_windows_terminal(_cwd: Option<&Path>, _cmd: &str) -> anyhow::Result<()> {
    anyhow::bail!("Windows Terminal is only available on Windows")
}

#[cfg(target_os = "windows")]
fn open_powershell(cwd: Option<&Path>, cmd: &str) -> anyhow::Result<()> {
    // Prefer pwsh (cross-platform Core), fall back to powershell.exe.
    let exe = if crate::settings::which_exists("pwsh.exe")
        || crate::settings::which_exists("pwsh")
    {
        "pwsh.exe"
    } else {
        "powershell.exe"
    };
    let composite = build_cd_command_powershell(cwd, cmd);
    // -NoExit keeps the window open after the command runs so the user can
    // see output. -Command is the argument that takes our composite.
    std::process::Command::new(exe)
        .arg("-NoExit")
        .arg("-Command")
        .arg(composite)
        .spawn()?;
    Ok(())
}

#[cfg(not(target_os = "windows"))]
fn open_powershell(_cwd: Option<&Path>, _cmd: &str) -> anyhow::Result<()> {
    anyhow::bail!("PowerShell is only available on Windows")
}

#[cfg(target_os = "windows")]
fn open_cmd(cwd: Option<&Path>, cmd: &str) -> anyhow::Result<()> {
    let composite = build_cd_command_windows(cwd, cmd);
    // `start` opens a new console window. The empty quoted string is the
    // window title — required positional arg when title contains spaces.
    std::process::Command::new("cmd.exe")
        .arg("/c")
        .arg("start")
        .arg("")
        .arg("cmd.exe")
        .arg("/k")
        .arg(composite)
        .spawn()?;
    Ok(())
}

#[cfg(not(target_os = "windows"))]
fn open_cmd(_cwd: Option<&Path>, _cmd: &str) -> anyhow::Result<()> {
    anyhow::bail!("cmd.exe is only available on Windows")
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
    use tokio::sync::Notify;
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
            selected_project: 0,
            dashboard_cursor: crate::app::DashboardCursor::default(),
            selected_setting: 0,
            settings_keybindings_open: false,
            selected_keybinding: 0,
            capturing_keybinding: None,
            keybinding_conflict: None,
            token_cache: Arc::new(TokenCache::new()),
            token_trend: Arc::new(crate::collector::token_trend::TokenTrend::default()),
            diagnostics: Arc::new(crate::collector::diagnostics::DiagnosticsStore::new()),
            dirty: Arc::new(Notify::new()),
            token_dirty: Arc::new(Notify::new()),
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
        app.state.write().sessions = Arc::new(vec![SessionMeta {
            agent: "claude",
            id: "delete-me".into(),
            path: session_path.clone(),
            cwd: Some(base.clone()),
            model: None,
            version: None,
            git_branch: None,

            source: None,
            started_at: None,
            updated_at: None,
            message_count: 0,
            tokens: TokenStats::default(),
            status: SessionStatus::Idle,
            byte_offset: 0,
            size_bytes: 2,
        }]);

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
        app.state.write().sessions = Arc::new(vec![SessionMeta {
            agent: "claude",
            id: "delete-me".into(),
            path: session_path.clone(),
            cwd: Some(base.clone()),
            model: None,
            version: None,
            git_branch: None,

            source: None,
            started_at: None,
            updated_at: None,
            message_count: 0,
            tokens: TokenStats::default(),
            status: SessionStatus::Idle,
            byte_offset: 0,
            size_bytes: 2,
        }]);

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
            state.sessions = Arc::new(vec![SessionMeta {
                agent: "claude",
                id: "delete-me".into(),
                path: session_path.clone(),
                cwd: Some(base.clone()),
                model: None,
                version: None,
                git_branch: None,

                source: None,
                started_at: None,
                updated_at: None,
                message_count: 0,
                tokens: TokenStats::default(),
                status: SessionStatus::Idle,
                byte_offset: 0,
                size_bytes: 2,
            }]);
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
        app.state.write().sessions = Arc::new(vec![SessionMeta {
            agent: "claude",
            id: "delete-me".into(),
            path: session_path.clone(),
            cwd: Some(base.clone()),
            model: None,
            version: None,
            git_branch: None,

            source: None,
            started_at: None,
            updated_at: None,
            message_count: 1,
            tokens: TokenStats::default(),
            status: SessionStatus::Idle,
            byte_offset: 0,
            size_bytes: 16,
        }]);
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
        app.state.write().sessions = Arc::new(vec![SessionMeta {
            agent: "claude",
            id: "delete-me".into(),
            path: session_path.clone(),
            cwd: Some(base.clone()),
            model: None,
            version: None,
            git_branch: None,

            source: None,
            started_at: None,
            updated_at: None,
            message_count: 1,
            tokens: TokenStats::default(),
            status: SessionStatus::Idle,
            byte_offset: 0,
            size_bytes: 16,
        }]);
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
        app.state.write().sessions = Arc::new(vec![SessionMeta {
            agent: "claude",
            id: "active".into(),
            path: session_path.clone(),
            cwd: Some(base.clone()),
            model: None,
            version: None,
            git_branch: None,

            source: None,
            started_at: None,
            updated_at: None,
            message_count: 1,
            tokens: TokenStats::default(),
            status: SessionStatus::Active,
            byte_offset: 0,
            size_bytes: 16,
        }]);
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
            state.sessions = Arc::new(vec![SessionMeta {
                agent: "claude",
                id: "active".into(),
                path: session_path.clone(),
                cwd: Some(base.clone()),
                model: None,
                version: None,
                git_branch: None,

                source: None,
                started_at: None,
                updated_at: None,
                message_count: 1,
                tokens: TokenStats::default(),
                status: SessionStatus::Active,
                byte_offset: 0,
                size_bytes: 16,
            }]);
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
                sessions: Arc::new(vec![SessionMeta {
                    agent: "codex",
                    id: "019dc3ba-a222-7631-96e6-2e1ebb238e53".into(),
                    path: session_path.clone(),
                    cwd: Some(PathBuf::from("/tmp/repo")),
                    model: Some("openai".into()),
                    version: Some("0.125.0-alpha.3".into()),
                    git_branch: None,

                    source: None,
                    started_at: None,
                    updated_at: None,
                    message_count: 0,
                    tokens: TokenStats::default(),
                    status: SessionStatus::Idle,
                    byte_offset: 0,
                    size_bytes: 0,
                }]),
                selected_session: 0,
                dirty: false,
                preview: None,
                mode: Mode::Normal,
                conversation: None,
                toast: None,
                show_help: false,
                session_generation: 0,
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
            selected_project: 0,
            dashboard_cursor: crate::app::DashboardCursor::default(),
            selected_setting: 0,
            settings_keybindings_open: false,
            selected_keybinding: 0,
            capturing_keybinding: None,
            keybinding_conflict: None,
            token_cache: Arc::new(TokenCache::new()),
            token_trend: Arc::new(crate::collector::token_trend::TokenTrend::default()),
            diagnostics: Arc::new(crate::collector::diagnostics::DiagnosticsStore::new()),
            dirty: Arc::new(Notify::new()),
            token_dirty: Arc::new(Notify::new()),
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

    #[test]
    fn show_help_toggles_on_question_mark_and_dismisses_on_any_key() {
        let _guard = crate::settings::test_lock();
        crate::settings::settings().write().keybindings = crate::settings::KeyBindings::default();

        let mut app = test_app();

        // Initially hidden.
        assert!(!app.state.read().show_help);

        // `?` opens.
        let press = |code: KeyCode, modifiers: KeyModifiers| crossterm::event::KeyEvent {
            code,
            modifiers,
            kind: crossterm::event::KeyEventKind::Press,
            state: crossterm::event::KeyEventState::empty(),
        };
        let exit = handle_event(
            crossterm::event::Event::Key(press(KeyCode::Char('?'), KeyModifiers::empty())),
            &mut app,
        );
        assert!(!exit);
        assert!(app.state.read().show_help, "? must open the overlay");

        // Any key (e.g. `q`) while overlay is open: dismisses but does NOT quit.
        let exit = handle_event(
            crossterm::event::Event::Key(press(KeyCode::Char('q'), KeyModifiers::empty())),
            &mut app,
        );
        assert!(!exit, "key while help is open must not propagate to quit");
        assert!(!app.state.read().show_help, "any key dismisses overlay");
        assert!(!app.should_quit, "the q must not have quit the app");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn dashboard_project_cursor_jump_sets_cwd_filter_and_switches_tab() {
        let _guard = crate::settings::test_lock();
        crate::settings::settings().write().keybindings = crate::settings::KeyBindings::default();

        let mut app = test_app();
        app.tab = Tab::Dashboard;
        app.dashboard_cursor = crate::app::DashboardCursor::Project;
        app.selected_project = 0;

        // Seed sessions with two distinct cwds so top_projects has two rows.
        let s1 = SessionMeta {
            agent: "claude",
            id: "a".into(),
            path: PathBuf::from("/tmp/a.jsonl"),
            cwd: Some(PathBuf::from("/Users/me/code/foo")),
            model: None,
            version: None,
            git_branch: None,
            source: None,
            started_at: None,
            updated_at: Some(chrono::Utc::now()),
            message_count: 0,
            tokens: TokenStats::default(),
            status: SessionStatus::Active,
            byte_offset: 0,
            size_bytes: 0,
        };
        let s2 = SessionMeta {
            id: "b".into(),
            path: PathBuf::from("/tmp/b.jsonl"),
            cwd: Some(PathBuf::from("/Users/me/code/bar")),
            ..s1.clone()
        };
        app.state.write().sessions = Arc::new(vec![s1, s2]);

        // First project (most recent / most sessions) should be /foo or /bar
        // depending on tie-break — both have count=1 so latest wins. Since
        // they were created with the same `Utc::now()` we can't depend on
        // order, just confirm the filter ends up matching ONE of them.
        jump_dashboard(&mut app);
        assert_eq!(app.tab, Tab::Sessions);
        assert!(
            app.session_filter.starts_with("cwd:"),
            "expected cwd: filter, got {}",
            app.session_filter
        );
        let visible = app
            .state
            .read()
            .visible_session_indices(&app.session_filter, app.session_sort);
        assert_eq!(
            visible.len(),
            1,
            "filter should narrow to a single project; got {visible:?} for filter `{}`",
            app.session_filter,
        );
    }

    #[test]
    fn cycle_dashboard_cursor_toggles_focus() {
        let _guard = crate::settings::test_lock();
        crate::settings::settings().write().keybindings = crate::settings::KeyBindings::default();

        let mut app = test_app();
        app.tab = Tab::Dashboard;
        assert_eq!(app.dashboard_cursor, crate::app::DashboardCursor::Process);

        cycle_dashboard_cursor(&mut app);
        assert_eq!(app.dashboard_cursor, crate::app::DashboardCursor::Project);

        cycle_dashboard_cursor(&mut app);
        assert_eq!(app.dashboard_cursor, crate::app::DashboardCursor::Process);
    }
}
