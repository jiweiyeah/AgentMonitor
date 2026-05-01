use crossterm::event::{KeyCode, KeyModifiers};
use serde::{Deserialize, Deserializer, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KeyContext {
    Global,
    Dashboard,
    Sessions,
    Settings,
    Viewer,
    DeleteConfirm,
    FilterInput,
}

impl KeyContext {
    pub fn label_key(self) -> &'static str {
        match self {
            KeyContext::Global => "key_context.global",
            KeyContext::Dashboard => "key_context.dashboard",
            KeyContext::Sessions => "key_context.sessions",
            KeyContext::Settings => "key_context.settings",
            KeyContext::Viewer => "key_context.viewer",
            KeyContext::DeleteConfirm => "key_context.delete_confirm",
            KeyContext::FilterInput => "key_context.filter_input",
        }
    }

    pub fn conflicts_with(self, other: KeyContext) -> bool {
        self == other
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KeyAction {
    Quit,
    TabNext,
    TabPrevious,
    OpenDashboardTab,
    OpenSessionsTab,
    OpenSettingsTab,
    MoveDown,
    MoveUp,
    Refresh,
    DashboardJumpSession,
    DashboardCycleCursor,
    SessionsOpenViewer,
    SessionsStartFilter,
    SessionsCycleSort,
    SessionsToggleActiveOnly,
    SessionsClearFilter,
    SessionsResume,
    SessionsDelete,
    SessionsToggleStar,
    SettingsActivate,
    SettingsChangeNext,
    SettingsChangePrevious,
    SettingsReset,
    SettingsCancel,
    SettingsClearKeybinding,
    SettingsResetAllKeybindings,
    ViewerBack,
    ViewerScrollDown,
    ViewerScrollUp,
    ViewerHalfPageDown,
    ViewerHalfPageUp,
    ViewerPageDown,
    ViewerPageUp,
    ViewerTop,
    ViewerBottom,
    ViewerExpand,
    ViewerCollapse,
    ViewerResume,
    ViewerDelete,
    ViewerSearchStart,
    ViewerSearchNext,
    ViewerSearchPrev,
    ViewerSearchCancel,
    DeleteCancel,
    DeleteConfirm,
    FilterCancel,
    FilterApply,
    FilterDeleteChar,
    Star,
    ShowHelp,
}

impl KeyAction {
    pub fn all() -> &'static [KeyAction] {
        &[
            KeyAction::Quit,
            KeyAction::TabNext,
            KeyAction::TabPrevious,
            KeyAction::OpenDashboardTab,
            KeyAction::OpenSessionsTab,
            KeyAction::OpenSettingsTab,
            KeyAction::MoveDown,
            KeyAction::MoveUp,
            KeyAction::Refresh,
            KeyAction::DashboardJumpSession,
            KeyAction::DashboardCycleCursor,
            KeyAction::SessionsOpenViewer,
            KeyAction::SessionsStartFilter,
            KeyAction::SessionsCycleSort,
            KeyAction::SessionsToggleActiveOnly,
            KeyAction::SessionsClearFilter,
            KeyAction::SessionsResume,
            KeyAction::SessionsDelete,
            KeyAction::SessionsToggleStar,
            KeyAction::SettingsActivate,
            KeyAction::SettingsChangeNext,
            KeyAction::SettingsChangePrevious,
            KeyAction::SettingsReset,
            KeyAction::SettingsCancel,
            KeyAction::SettingsClearKeybinding,
            KeyAction::SettingsResetAllKeybindings,
            KeyAction::ViewerBack,
            KeyAction::ViewerScrollDown,
            KeyAction::ViewerScrollUp,
            KeyAction::ViewerHalfPageDown,
            KeyAction::ViewerHalfPageUp,
            KeyAction::ViewerPageDown,
            KeyAction::ViewerPageUp,
            KeyAction::ViewerTop,
            KeyAction::ViewerBottom,
            KeyAction::ViewerExpand,
            KeyAction::ViewerCollapse,
            KeyAction::ViewerResume,
            KeyAction::ViewerDelete,
            KeyAction::ViewerSearchStart,
            KeyAction::ViewerSearchNext,
            KeyAction::ViewerSearchPrev,
            KeyAction::ViewerSearchCancel,
            KeyAction::DeleteCancel,
            KeyAction::DeleteConfirm,
            KeyAction::FilterCancel,
            KeyAction::FilterApply,
            KeyAction::FilterDeleteChar,
            KeyAction::Star,
            KeyAction::ShowHelp,
        ]
    }

    pub fn context(self) -> KeyContext {
        match self {
            KeyAction::Quit
            | KeyAction::TabNext
            | KeyAction::TabPrevious
            | KeyAction::OpenDashboardTab
            | KeyAction::OpenSessionsTab
            | KeyAction::OpenSettingsTab
            | KeyAction::MoveDown
            | KeyAction::MoveUp
            | KeyAction::Refresh => KeyContext::Global,
            KeyAction::DashboardJumpSession => KeyContext::Dashboard,
            KeyAction::DashboardCycleCursor => KeyContext::Dashboard,
            KeyAction::SessionsOpenViewer
            | KeyAction::SessionsStartFilter
            | KeyAction::SessionsCycleSort
            | KeyAction::SessionsToggleActiveOnly
            | KeyAction::SessionsClearFilter
            | KeyAction::SessionsResume
            | KeyAction::SessionsDelete
            | KeyAction::SessionsToggleStar => KeyContext::Sessions,
            KeyAction::SettingsActivate
            | KeyAction::SettingsChangeNext
            | KeyAction::SettingsChangePrevious
            | KeyAction::SettingsReset
            | KeyAction::SettingsCancel
            | KeyAction::SettingsClearKeybinding
            | KeyAction::SettingsResetAllKeybindings => KeyContext::Settings,
            KeyAction::ViewerBack
            | KeyAction::ViewerScrollDown
            | KeyAction::ViewerScrollUp
            | KeyAction::ViewerHalfPageDown
            | KeyAction::ViewerHalfPageUp
            | KeyAction::ViewerPageDown
            | KeyAction::ViewerPageUp
            | KeyAction::ViewerTop
            | KeyAction::ViewerBottom
            | KeyAction::ViewerExpand
            | KeyAction::ViewerCollapse
            | KeyAction::ViewerResume
            | KeyAction::ViewerDelete
            | KeyAction::ViewerSearchStart
            | KeyAction::ViewerSearchNext
            | KeyAction::ViewerSearchPrev
            | KeyAction::ViewerSearchCancel => KeyContext::Viewer,
            KeyAction::DeleteCancel => KeyContext::DeleteConfirm,
            KeyAction::DeleteConfirm => KeyContext::DeleteConfirm,
            KeyAction::FilterCancel | KeyAction::FilterApply | KeyAction::FilterDeleteChar => {
                KeyContext::FilterInput
            }
            KeyAction::Star => KeyContext::Global,
            KeyAction::ShowHelp => KeyContext::Global,
        }
    }

    pub fn label_key(self) -> &'static str {
        match self {
            KeyAction::Quit => "key_action.quit",
            KeyAction::TabNext => "key_action.tab_next",
            KeyAction::TabPrevious => "key_action.tab_previous",
            KeyAction::OpenDashboardTab => "key_action.open_dashboard_tab",
            KeyAction::OpenSessionsTab => "key_action.open_sessions_tab",
            KeyAction::OpenSettingsTab => "key_action.open_settings_tab",
            KeyAction::MoveDown => "key_action.move_down",
            KeyAction::MoveUp => "key_action.move_up",
            KeyAction::Refresh => "key_action.refresh",
            KeyAction::DashboardJumpSession => "key_action.dashboard_jump_session",
            KeyAction::DashboardCycleCursor => "key_action.dashboard_cycle_cursor",
            KeyAction::SessionsOpenViewer => "key_action.sessions_open_viewer",
            KeyAction::SessionsStartFilter => "key_action.sessions_start_filter",
            KeyAction::SessionsCycleSort => "key_action.sessions_cycle_sort",
            KeyAction::SessionsToggleActiveOnly => "key_action.sessions_toggle_active_only",
            KeyAction::SessionsClearFilter => "key_action.sessions_clear_filter",
            KeyAction::SessionsResume => "key_action.sessions_resume",
            KeyAction::SessionsDelete => "key_action.sessions_delete",
            KeyAction::SessionsToggleStar => "key_action.sessions_toggle_star",
            KeyAction::SettingsActivate => "key_action.settings_activate",
            KeyAction::SettingsChangeNext => "key_action.settings_change_next",
            KeyAction::SettingsChangePrevious => "key_action.settings_change_previous",
            KeyAction::SettingsReset => "key_action.settings_reset",
            KeyAction::SettingsCancel => "key_action.settings_cancel",
            KeyAction::SettingsClearKeybinding => "key_action.settings_clear_keybinding",
            KeyAction::SettingsResetAllKeybindings => "key_action.settings_reset_all_keybindings",
            KeyAction::ViewerBack => "key_action.viewer_back",
            KeyAction::ViewerScrollDown => "key_action.viewer_scroll_down",
            KeyAction::ViewerScrollUp => "key_action.viewer_scroll_up",
            KeyAction::ViewerHalfPageDown => "key_action.viewer_half_page_down",
            KeyAction::ViewerHalfPageUp => "key_action.viewer_half_page_up",
            KeyAction::ViewerPageDown => "key_action.viewer_page_down",
            KeyAction::ViewerPageUp => "key_action.viewer_page_up",
            KeyAction::ViewerTop => "key_action.viewer_top",
            KeyAction::ViewerBottom => "key_action.viewer_bottom",
            KeyAction::ViewerExpand => "key_action.viewer_expand",
            KeyAction::ViewerCollapse => "key_action.viewer_collapse",
            KeyAction::ViewerResume => "key_action.viewer_resume",
            KeyAction::ViewerDelete => "key_action.viewer_delete",
            KeyAction::ViewerSearchStart => "key_action.viewer_search_start",
            KeyAction::ViewerSearchNext => "key_action.viewer_search_next",
            KeyAction::ViewerSearchPrev => "key_action.viewer_search_prev",
            KeyAction::ViewerSearchCancel => "key_action.viewer_search_cancel",
            KeyAction::DeleteCancel => "key_action.delete_cancel",
            KeyAction::DeleteConfirm => "key_action.delete_confirm",
            KeyAction::FilterCancel => "key_action.filter_cancel",
            KeyAction::FilterApply => "key_action.filter_apply",
            KeyAction::FilterDeleteChar => "key_action.filter_delete_char",
            KeyAction::Star => "key_action.star",
            KeyAction::ShowHelp => "key_action.show_help",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type", content = "value")]
pub enum KeyCodeSpec {
    Char(char),
    Enter,
    Esc,
    Tab,
    BackTab,
    Backspace,
    Delete,
    Up,
    Down,
    Left,
    Right,
    PageUp,
    PageDown,
}

impl KeyCodeSpec {
    fn from_key_code(code: KeyCode) -> Option<Self> {
        match code {
            KeyCode::Char(c) => Some(KeyCodeSpec::Char(c)),
            KeyCode::Enter => Some(KeyCodeSpec::Enter),
            KeyCode::Esc => Some(KeyCodeSpec::Esc),
            KeyCode::Tab => Some(KeyCodeSpec::Tab),
            KeyCode::BackTab => Some(KeyCodeSpec::BackTab),
            KeyCode::Backspace => Some(KeyCodeSpec::Backspace),
            KeyCode::Delete => Some(KeyCodeSpec::Delete),
            KeyCode::Up => Some(KeyCodeSpec::Up),
            KeyCode::Down => Some(KeyCodeSpec::Down),
            KeyCode::Left => Some(KeyCodeSpec::Left),
            KeyCode::Right => Some(KeyCodeSpec::Right),
            KeyCode::PageUp => Some(KeyCodeSpec::PageUp),
            KeyCode::PageDown => Some(KeyCodeSpec::PageDown),
            _ => None,
        }
    }

    fn display(self, shifted: bool) -> String {
        match self {
            KeyCodeSpec::Char(c) if shifted => c.to_uppercase().collect(),
            KeyCodeSpec::Char(c) => c.to_string(),
            KeyCodeSpec::Enter => "Enter".to_string(),
            KeyCodeSpec::Esc => "Esc".to_string(),
            KeyCodeSpec::Tab => "Tab".to_string(),
            KeyCodeSpec::BackTab => "Shift+Tab".to_string(),
            KeyCodeSpec::Backspace => "Backspace".to_string(),
            KeyCodeSpec::Delete => "Delete".to_string(),
            KeyCodeSpec::Up => "↑".to_string(),
            KeyCodeSpec::Down => "↓".to_string(),
            KeyCodeSpec::Left => "←".to_string(),
            KeyCodeSpec::Right => "→".to_string(),
            KeyCodeSpec::PageUp => "PageUp".to_string(),
            KeyCodeSpec::PageDown => "PageDown".to_string(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct KeyBinding {
    pub code: KeyCodeSpec,
    #[serde(default)]
    pub ctrl: bool,
    #[serde(default)]
    pub alt: bool,
    #[serde(default)]
    pub shift: bool,
}

impl KeyBinding {
    pub const fn plain(code: KeyCodeSpec) -> Self {
        Self {
            code,
            ctrl: false,
            alt: false,
            shift: false,
        }
    }

    pub const fn ctrl(code: KeyCodeSpec) -> Self {
        Self {
            code,
            ctrl: true,
            alt: false,
            shift: false,
        }
    }

    pub fn from_event(code: KeyCode, modifiers: KeyModifiers) -> Option<Self> {
        let code = KeyCodeSpec::from_key_code(code)?;
        let mut binding = Self {
            code,
            ctrl: modifiers.contains(KeyModifiers::CONTROL),
            alt: modifiers.contains(KeyModifiers::ALT),
            shift: modifiers.contains(KeyModifiers::SHIFT),
        };
        if matches!(binding.code, KeyCodeSpec::Char(_) | KeyCodeSpec::BackTab) {
            binding.shift = false;
        }
        Some(binding)
    }

    pub fn matches_event(self, code: KeyCode, modifiers: KeyModifiers) -> bool {
        Self::from_event(code, modifiers).is_some_and(|binding| binding == self)
    }

    pub fn display(self) -> String {
        let mut parts = Vec::new();
        if self.ctrl {
            parts.push("Ctrl".to_string());
        }
        if self.alt {
            parts.push("Alt".to_string());
        }
        if self.shift && !matches!(self.code, KeyCodeSpec::Char(_) | KeyCodeSpec::BackTab) {
            parts.push("Shift".to_string());
        }
        parts.push(self.code.display(self.ctrl || self.alt || self.shift));
        parts.join("+")
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyBindingEntry {
    pub action: KeyAction,
    #[serde(default)]
    pub bindings: Vec<KeyBinding>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct KeyBindings {
    pub entries: Vec<KeyBindingEntry>,
}

impl Default for KeyBindings {
    fn default() -> Self {
        Self {
            entries: KeyAction::all()
                .iter()
                .map(|&action| KeyBindingEntry {
                    action,
                    bindings: default_bindings(action),
                })
                .collect(),
        }
    }
}

impl<'de> Deserialize<'de> for KeyBindings {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct Wire {
            #[serde(default)]
            entries: Vec<KeyBindingEntry>,
        }

        let wire = Wire::deserialize(deserializer)?;
        let mut out = KeyBindings {
            entries: wire.entries,
        };
        out.fill_missing_defaults();
        out.normalize_conflicts();
        Ok(out)
    }
}

impl KeyBindings {
    pub fn bindings_for(&self, action: KeyAction) -> &[KeyBinding] {
        self.entries
            .iter()
            .find(|entry| entry.action == action)
            .map(|entry| entry.bindings.as_slice())
            .unwrap_or(&[])
    }

    pub fn binding_display(&self, action: KeyAction) -> String {
        let bindings = self.bindings_for(action);
        if bindings.is_empty() {
            return "unbound".to_string();
        }
        bindings
            .iter()
            .map(|binding| binding.display())
            .collect::<Vec<_>>()
            .join("/")
    }

    pub fn matches_action(
        &self,
        action: KeyAction,
        code: KeyCode,
        modifiers: KeyModifiers,
    ) -> bool {
        self.bindings_for(action)
            .iter()
            .any(|binding| binding.matches_event(code, modifiers))
    }

    pub fn conflict_for(&self, action: KeyAction, binding: KeyBinding) -> Option<KeyAction> {
        let context = action.context();
        self.entries.iter().find_map(|entry| {
            (entry.action != action
                && context.conflicts_with(entry.action.context())
                && entry.bindings.contains(&binding))
            .then_some(entry.action)
        })
    }

    pub fn set_binding(&mut self, action: KeyAction, binding: KeyBinding) -> Option<KeyAction> {
        let replaced = self.conflict_for(action, binding);
        let context = action.context();
        for entry in &mut self.entries {
            if entry.action != action && context.conflicts_with(entry.action.context()) {
                entry.bindings.retain(|candidate| *candidate != binding);
            }
        }
        self.entry_mut(action).bindings = vec![binding];
        replaced
    }

    pub fn clear_binding(&mut self, action: KeyAction) {
        self.entry_mut(action).bindings.clear();
    }

    pub fn reset_binding(&mut self, action: KeyAction) {
        let bindings = default_bindings(action);
        let context = action.context();
        for entry in &mut self.entries {
            if entry.action != action && context.conflicts_with(entry.action.context()) {
                entry
                    .bindings
                    .retain(|candidate| !bindings.contains(candidate));
            }
        }
        self.entry_mut(action).bindings = bindings;
    }

    pub fn reset_all(&mut self) {
        *self = Self::default();
    }

    fn normalize_conflicts(&mut self) {
        let actions: Vec<KeyAction> = self.entries.iter().map(|entry| entry.action).collect();
        for action in actions {
            let bindings = self.bindings_for(action).to_vec();
            let context = action.context();
            for binding in bindings {
                let mut seen_owner = false;
                for entry in &mut self.entries {
                    if !context.conflicts_with(entry.action.context()) {
                        continue;
                    }
                    if entry.action == action && !seen_owner {
                        seen_owner = entry.bindings.contains(&binding);
                        continue;
                    }
                    entry.bindings.retain(|candidate| *candidate != binding);
                }
            }
        }
    }

    fn entry_mut(&mut self, action: KeyAction) -> &mut KeyBindingEntry {
        if let Some(idx) = self.entries.iter().position(|entry| entry.action == action) {
            return &mut self.entries[idx];
        }
        self.entries.push(KeyBindingEntry {
            action,
            bindings: default_bindings(action).to_vec(),
        });
        self.entries.last_mut().expect("just pushed entry")
    }

    fn fill_missing_defaults(&mut self) {
        for &action in KeyAction::all() {
            if !self.entries.iter().any(|entry| entry.action == action) {
                self.reset_binding(action);
            }
        }
    }
}

pub fn default_bindings(action: KeyAction) -> Vec<KeyBinding> {
    match action {
        KeyAction::Quit => vec![KeyBinding::plain(KeyCodeSpec::Char('q'))],
        KeyAction::TabNext => vec![
            KeyBinding::plain(KeyCodeSpec::Tab),
            KeyBinding::plain(KeyCodeSpec::Right),
        ],
        KeyAction::TabPrevious => vec![
            KeyBinding::plain(KeyCodeSpec::BackTab),
            KeyBinding::plain(KeyCodeSpec::Left),
        ],
        KeyAction::OpenDashboardTab => vec![KeyBinding::plain(KeyCodeSpec::Char('1'))],
        KeyAction::OpenSessionsTab => vec![KeyBinding::plain(KeyCodeSpec::Char('2'))],
        KeyAction::OpenSettingsTab => vec![KeyBinding::plain(KeyCodeSpec::Char('3'))],
        KeyAction::MoveDown => vec![
            KeyBinding::plain(KeyCodeSpec::Char('j')),
            KeyBinding::plain(KeyCodeSpec::Down),
        ],
        KeyAction::MoveUp => vec![
            KeyBinding::plain(KeyCodeSpec::Char('k')),
            KeyBinding::plain(KeyCodeSpec::Up),
        ],
        KeyAction::Refresh => vec![KeyBinding::plain(KeyCodeSpec::Char('f'))],
        KeyAction::DashboardJumpSession => vec![KeyBinding::plain(KeyCodeSpec::Enter)],
        KeyAction::DashboardCycleCursor => vec![KeyBinding::plain(KeyCodeSpec::Tab)],
        KeyAction::SessionsOpenViewer => vec![KeyBinding::plain(KeyCodeSpec::Enter)],
        KeyAction::SessionsStartFilter => vec![KeyBinding::plain(KeyCodeSpec::Char('/'))],
        KeyAction::SessionsCycleSort => vec![KeyBinding::plain(KeyCodeSpec::Char('s'))],
        KeyAction::SessionsToggleActiveOnly => vec![KeyBinding::plain(KeyCodeSpec::Char('a'))],
        KeyAction::SessionsClearFilter => vec![KeyBinding::plain(KeyCodeSpec::Char('c'))],
        KeyAction::SessionsResume => vec![KeyBinding::plain(KeyCodeSpec::Char('r'))],
        KeyAction::SessionsDelete => vec![
            KeyBinding::plain(KeyCodeSpec::Char('d')),
            KeyBinding::plain(KeyCodeSpec::Delete),
        ],
        KeyAction::SessionsToggleStar => vec![KeyBinding::plain(KeyCodeSpec::Char('b'))],
        KeyAction::SettingsActivate => vec![KeyBinding::plain(KeyCodeSpec::Enter)],
        KeyAction::SettingsChangeNext => vec![KeyBinding::plain(KeyCodeSpec::Right)],
        KeyAction::SettingsChangePrevious => vec![KeyBinding::plain(KeyCodeSpec::Left)],
        KeyAction::SettingsReset => vec![KeyBinding::plain(KeyCodeSpec::Char('r'))],
        KeyAction::SettingsCancel => vec![KeyBinding::plain(KeyCodeSpec::Esc)],
        KeyAction::SettingsClearKeybinding => vec![KeyBinding::plain(KeyCodeSpec::Backspace)],
        KeyAction::SettingsResetAllKeybindings => vec![KeyBinding::plain(KeyCodeSpec::Char('R'))],
        KeyAction::ViewerBack => vec![
            KeyBinding::plain(KeyCodeSpec::Esc),
            KeyBinding::plain(KeyCodeSpec::Char('q')),
        ],
        KeyAction::ViewerScrollDown => vec![
            KeyBinding::plain(KeyCodeSpec::Char('j')),
            KeyBinding::plain(KeyCodeSpec::Down),
        ],
        KeyAction::ViewerScrollUp => vec![
            KeyBinding::plain(KeyCodeSpec::Char('k')),
            KeyBinding::plain(KeyCodeSpec::Up),
        ],
        KeyAction::ViewerHalfPageDown => vec![KeyBinding::ctrl(KeyCodeSpec::Char('d'))],
        KeyAction::ViewerHalfPageUp => vec![KeyBinding::ctrl(KeyCodeSpec::Char('u'))],
        KeyAction::ViewerPageDown => vec![KeyBinding::plain(KeyCodeSpec::PageDown)],
        KeyAction::ViewerPageUp => vec![KeyBinding::plain(KeyCodeSpec::PageUp)],
        KeyAction::ViewerTop => vec![KeyBinding::plain(KeyCodeSpec::Char('g'))],
        KeyAction::ViewerBottom => vec![KeyBinding::plain(KeyCodeSpec::Char('G'))],
        KeyAction::ViewerExpand => vec![KeyBinding::plain(KeyCodeSpec::Char('e'))],
        KeyAction::ViewerCollapse => vec![KeyBinding::plain(KeyCodeSpec::Char('c'))],
        KeyAction::ViewerResume => vec![KeyBinding::plain(KeyCodeSpec::Char('r'))],
        KeyAction::ViewerDelete => vec![
            KeyBinding::plain(KeyCodeSpec::Char('d')),
            KeyBinding::plain(KeyCodeSpec::Delete),
        ],
        KeyAction::ViewerSearchStart => vec![KeyBinding::plain(KeyCodeSpec::Char('/'))],
        KeyAction::ViewerSearchNext => vec![KeyBinding::plain(KeyCodeSpec::Char('n'))],
        KeyAction::ViewerSearchPrev => vec![KeyBinding::plain(KeyCodeSpec::Char('N'))],
        KeyAction::ViewerSearchCancel => vec![KeyBinding::plain(KeyCodeSpec::Esc)],
        KeyAction::DeleteCancel => vec![
            KeyBinding::plain(KeyCodeSpec::Esc),
            KeyBinding::plain(KeyCodeSpec::Char('q')),
        ],
        KeyAction::DeleteConfirm => vec![
            KeyBinding::plain(KeyCodeSpec::Enter),
            KeyBinding::plain(KeyCodeSpec::Char('y')),
        ],
        KeyAction::FilterCancel => vec![KeyBinding::plain(KeyCodeSpec::Esc)],
        KeyAction::FilterApply => vec![KeyBinding::plain(KeyCodeSpec::Enter)],
        KeyAction::FilterDeleteChar => vec![KeyBinding::plain(KeyCodeSpec::Backspace)],
        KeyAction::Star => vec![KeyBinding::plain(KeyCodeSpec::Char('*'))],
        KeyAction::ShowHelp => vec![KeyBinding::plain(KeyCodeSpec::Char('?'))],
    }
}
