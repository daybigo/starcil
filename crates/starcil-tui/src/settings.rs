//! Interactive sectioned settings backed by targeted TOML edits.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use starcil_config::{
    BUILTIN_THEME_NAMES, Color, Config, ConfigSetting, SidebarCollapsedMode, ToastDelivery,
    UpdateChannel,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingId {
    Accent,
    ToastDelivery,
    SoundEnabled,
    MouseCapture,
    CopyOnSelect,
    PaneBorders,
    PaneGaps,
    AgentLabels,
    SidebarWidth,
    SidebarStartCollapsed,
    SidebarCollapsedMode,
    ConfirmClose,
    PromptNewTabName,
    UpdateChannel,
}

impl SettingId {
    pub fn label(self) -> &'static str {
        match self {
            Self::Accent => "Accent",
            Self::ToastDelivery => "Toast delivery",
            Self::SoundEnabled => "Sounds",
            Self::MouseCapture => "Mouse capture",
            Self::CopyOnSelect => "Copy on select",
            Self::PaneBorders => "Pane borders",
            Self::PaneGaps => "Pane gaps",
            Self::AgentLabels => "Agent labels on borders",
            Self::SidebarWidth => "Sidebar width",
            Self::SidebarStartCollapsed => "Start collapsed",
            Self::SidebarCollapsedMode => "Collapsed mode",
            Self::ConfirmClose => "Confirm workspace close",
            Self::PromptNewTabName => "Prompt for tab name",
            Self::UpdateChannel => "Update channel",
        }
    }
}

/// One selectable row inside a settings section.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsRow {
    /// A theme name that Enter applies directly.
    Theme(&'static str),
    /// A typed setting whose value cycles or edits in place.
    Setting(SettingId),
}

pub const SECTION_NAMES: [&str; 6] = ["theme", "sound", "toasts", "panes", "sidebar", "general"];

pub fn section_rows(section: usize) -> Vec<SettingsRow> {
    match section {
        0 => BUILTIN_THEME_NAMES
            .iter()
            .map(|name| SettingsRow::Theme(name))
            .chain([SettingsRow::Setting(SettingId::Accent)])
            .collect(),
        1 => vec![SettingsRow::Setting(SettingId::SoundEnabled)],
        2 => vec![SettingsRow::Setting(SettingId::ToastDelivery)],
        3 => vec![
            SettingsRow::Setting(SettingId::PaneBorders),
            SettingsRow::Setting(SettingId::PaneGaps),
            SettingsRow::Setting(SettingId::AgentLabels),
            SettingsRow::Setting(SettingId::CopyOnSelect),
            SettingsRow::Setting(SettingId::MouseCapture),
        ],
        4 => vec![
            SettingsRow::Setting(SettingId::SidebarWidth),
            SettingsRow::Setting(SettingId::SidebarStartCollapsed),
            SettingsRow::Setting(SettingId::SidebarCollapsedMode),
        ],
        _ => vec![
            SettingsRow::Setting(SettingId::ConfirmClose),
            SettingsRow::Setting(SettingId::PromptNewTabName),
            SettingsRow::Setting(SettingId::UpdateChannel),
        ],
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SettingsEvent {
    None,
    Close,
    Changed(ConfigSetting),
}

#[derive(Debug, Default)]
pub struct SettingsEditor {
    section: usize,
    selected: usize,
    editing: Option<String>,
    error: Option<String>,
}

impl SettingsEditor {
    pub fn section_index(&self) -> usize {
        self.section.min(SECTION_NAMES.len() - 1)
    }

    pub fn selected_index(&self) -> usize {
        let rows = section_rows(self.section_index());
        self.selected.min(rows.len().saturating_sub(1))
    }

    pub fn rows(&self) -> Vec<SettingsRow> {
        section_rows(self.section_index())
    }

    pub fn select_section(&mut self, section: usize) {
        if section < SECTION_NAMES.len() && section != self.section_index() {
            self.section = section;
            self.selected = 0;
            self.editing = None;
            self.error = None;
        }
    }

    pub fn select_row(&mut self, row: usize) {
        if row < self.rows().len() && row != self.selected_index() {
            self.selected = row;
            self.editing = None;
            self.error = None;
        }
    }

    pub fn selected_row(&self) -> SettingsRow {
        let rows = self.rows();
        rows[self.selected_index()]
    }

    pub fn editing(&self) -> Option<&str> {
        self.editing.as_deref()
    }

    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    pub fn value(&self, setting: SettingId, config: &Config) -> String {
        match setting {
            SettingId::Accent => config.ui.accent.clone(),
            SettingId::ToastDelivery => match config.ui.toast.delivery {
                ToastDelivery::Off => "off",
                ToastDelivery::Starcil => "starcil",
                ToastDelivery::Terminal => "terminal",
                ToastDelivery::System => "system",
            }
            .to_owned(),
            SettingId::SoundEnabled => on_off(config.ui.sound.enabled),
            SettingId::MouseCapture => on_off(config.ui.mouse_capture),
            SettingId::CopyOnSelect => on_off(config.ui.copy_on_select),
            SettingId::PaneBorders => on_off(config.ui.pane_borders),
            SettingId::PaneGaps => on_off(config.ui.pane_gaps),
            SettingId::AgentLabels => on_off(config.ui.show_agent_labels_on_pane_borders),
            SettingId::SidebarWidth => config.ui.sidebar_width.to_string(),
            SettingId::SidebarStartCollapsed => on_off(config.ui.sidebar_start_collapsed),
            SettingId::SidebarCollapsedMode => match config.ui.sidebar_collapsed_mode {
                SidebarCollapsedMode::Compact => "compact",
                SidebarCollapsedMode::Hidden => "hidden",
            }
            .to_owned(),
            SettingId::ConfirmClose => on_off(config.ui.confirm_close),
            SettingId::PromptNewTabName => on_off(config.ui.prompt_new_tab_name),
            SettingId::UpdateChannel => match config.update.channel {
                UpdateChannel::Stable => "stable",
                UpdateChannel::Preview => "preview",
            }
            .to_owned(),
        }
    }

    pub fn handle_key(&mut self, event: KeyEvent, config: &mut Config) -> SettingsEvent {
        self.error = None;
        if let Some(mut buffer) = self.editing.take() {
            return match event.code {
                KeyCode::Esc => SettingsEvent::None,
                KeyCode::Backspace => {
                    buffer.pop();
                    self.editing = Some(buffer);
                    SettingsEvent::None
                }
                KeyCode::Enter => match self.commit_string(config, buffer) {
                    Ok(setting) => SettingsEvent::Changed(setting),
                    Err((message, buffer)) => {
                        self.error = Some(message);
                        self.editing = Some(buffer);
                        SettingsEvent::None
                    }
                },
                KeyCode::Char(character)
                    if !event.modifiers.intersects(
                        KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER,
                    ) =>
                {
                    buffer.push(character);
                    self.editing = Some(buffer);
                    SettingsEvent::None
                }
                _ => {
                    self.editing = Some(buffer);
                    SettingsEvent::None
                }
            };
        }

        let rows = self.rows();
        match event.code {
            KeyCode::Esc => SettingsEvent::Close,
            KeyCode::Tab => {
                self.section = (self.section_index() + 1) % SECTION_NAMES.len();
                self.selected = 0;
                SettingsEvent::None
            }
            KeyCode::BackTab => {
                self.section =
                    (self.section_index() + SECTION_NAMES.len() - 1) % SECTION_NAMES.len();
                self.selected = 0;
                SettingsEvent::None
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.selected = self.selected_index().saturating_sub(1);
                SettingsEvent::None
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.selected = (self.selected_index() + 1).min(rows.len().saturating_sub(1));
                SettingsEvent::None
            }
            KeyCode::Left | KeyCode::Char('h') => match self.selected_row() {
                SettingsRow::Setting(setting) => self.change(config, setting, -1),
                SettingsRow::Theme(_) => SettingsEvent::None,
            },
            KeyCode::Right | KeyCode::Char('l') => match self.selected_row() {
                SettingsRow::Setting(setting) => self.change(config, setting, 1),
                SettingsRow::Theme(_) => SettingsEvent::None,
            },
            KeyCode::Enter | KeyCode::Char(' ') => match self.selected_row() {
                SettingsRow::Theme(name) => {
                    config.theme.name = name.to_owned();
                    SettingsEvent::Changed(ConfigSetting::ThemeName(name.to_owned()))
                }
                SettingsRow::Setting(SettingId::Accent) => {
                    self.editing = Some(config.ui.accent.clone());
                    SettingsEvent::None
                }
                SettingsRow::Setting(setting) => self.change(config, setting, 1),
            },
            _ => SettingsEvent::None,
        }
    }

    fn commit_string(
        &self,
        config: &mut Config,
        buffer: String,
    ) -> Result<ConfigSetting, (String, String)> {
        match self.selected_row() {
            SettingsRow::Setting(SettingId::Accent) => match buffer.parse::<Color>() {
                Ok(Color::Reset) => Err(("Accent cannot be reset".to_owned(), buffer)),
                Ok(_) => {
                    config.ui.accent = buffer.clone();
                    Ok(ConfigSetting::Accent(buffer))
                }
                Err(error) => Err((error.to_string(), buffer)),
            },
            _ => Err(("This setting is not text-editable".to_owned(), buffer)),
        }
    }

    fn change(&mut self, config: &mut Config, setting: SettingId, delta: i32) -> SettingsEvent {
        let setting = match setting {
            SettingId::Accent => {
                self.editing = Some(config.ui.accent.clone());
                return SettingsEvent::None;
            }
            SettingId::ToastDelivery => {
                const VALUES: [ToastDelivery; 4] = [
                    ToastDelivery::Off,
                    ToastDelivery::Starcil,
                    ToastDelivery::Terminal,
                    ToastDelivery::System,
                ];
                let current = VALUES
                    .iter()
                    .position(|value| *value == config.ui.toast.delivery)
                    .unwrap_or_default();
                config.ui.toast.delivery = VALUES[wrap(current, VALUES.len(), delta)];
                ConfigSetting::ToastDelivery(config.ui.toast.delivery)
            }
            SettingId::SoundEnabled => {
                config.ui.sound.enabled = !config.ui.sound.enabled;
                ConfigSetting::SoundEnabled(config.ui.sound.enabled)
            }
            SettingId::MouseCapture => {
                config.ui.mouse_capture = !config.ui.mouse_capture;
                ConfigSetting::MouseCapture(config.ui.mouse_capture)
            }
            SettingId::CopyOnSelect => {
                config.ui.copy_on_select = !config.ui.copy_on_select;
                ConfigSetting::CopyOnSelect(config.ui.copy_on_select)
            }
            SettingId::PaneBorders => {
                config.ui.pane_borders = !config.ui.pane_borders;
                ConfigSetting::PaneBorders(config.ui.pane_borders)
            }
            SettingId::PaneGaps => {
                config.ui.pane_gaps = !config.ui.pane_gaps;
                ConfigSetting::PaneGaps(config.ui.pane_gaps)
            }
            SettingId::AgentLabels => {
                config.ui.show_agent_labels_on_pane_borders =
                    !config.ui.show_agent_labels_on_pane_borders;
                ConfigSetting::ShowAgentLabels(config.ui.show_agent_labels_on_pane_borders)
            }
            SettingId::SidebarWidth => {
                config.ui.sidebar_width = if delta >= 0 {
                    config.ui.sidebar_width.saturating_add(1)
                } else {
                    config.ui.sidebar_width.saturating_sub(1)
                }
                .clamp(config.ui.sidebar_min_width, config.ui.sidebar_max_width);
                ConfigSetting::SidebarWidth(config.ui.sidebar_width)
            }
            SettingId::SidebarStartCollapsed => {
                config.ui.sidebar_start_collapsed = !config.ui.sidebar_start_collapsed;
                ConfigSetting::SidebarStartCollapsed(config.ui.sidebar_start_collapsed)
            }
            SettingId::SidebarCollapsedMode => {
                config.ui.sidebar_collapsed_mode = match config.ui.sidebar_collapsed_mode {
                    SidebarCollapsedMode::Compact => SidebarCollapsedMode::Hidden,
                    SidebarCollapsedMode::Hidden => SidebarCollapsedMode::Compact,
                };
                ConfigSetting::SidebarCollapsedMode(config.ui.sidebar_collapsed_mode)
            }
            SettingId::ConfirmClose => {
                config.ui.confirm_close = !config.ui.confirm_close;
                ConfigSetting::ConfirmClose(config.ui.confirm_close)
            }
            SettingId::PromptNewTabName => {
                config.ui.prompt_new_tab_name = !config.ui.prompt_new_tab_name;
                ConfigSetting::PromptNewTabName(config.ui.prompt_new_tab_name)
            }
            SettingId::UpdateChannel => {
                config.update.channel = match config.update.channel {
                    UpdateChannel::Stable => UpdateChannel::Preview,
                    UpdateChannel::Preview => UpdateChannel::Stable,
                };
                ConfigSetting::UpdateChannel(config.update.channel)
            }
        };
        SettingsEvent::Changed(setting)
    }
}

fn on_off(value: bool) -> String {
    if value { "on" } else { "off" }.to_owned()
}

fn wrap(current: usize, len: usize, delta: i32) -> usize {
    (current as i32 + delta).rem_euclid(len.max(1) as i32) as usize
}
