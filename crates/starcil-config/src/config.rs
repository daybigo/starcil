use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use toml::Value;
use toml_edit::DocumentMut;

use crate::keys::{Keys, build_effective_keymap};
use crate::theme::{Color, Theme, ThemeError, TOKEN_NAMES, builtin_theme};

/// Every documented configuration path: the checklist the validator and the
/// model tests must cover in full.
pub const CONFIG_REFERENCE_KEYS: &[&str] = &[
    "onboarding",
    "theme.name",
    "theme.auto_switch",
    "theme.dark_name",
    "theme.light_name",
    "theme.custom.accent",
    "theme.custom.panel_bg",
    "theme.custom.surface0",
    "theme.custom.surface1",
    "theme.custom.surface_dim",
    "theme.custom.overlay0",
    "theme.custom.overlay1",
    "theme.custom.text",
    "theme.custom.subtext0",
    "theme.custom.mauve",
    "theme.custom.green",
    "theme.custom.yellow",
    "theme.custom.red",
    "theme.custom.blue",
    "theme.custom.teal",
    "theme.custom.peach",
    "terminal.default_shell",
    "terminal.shell_mode",
    "terminal.new_cwd",
    "update.channel",
    "update.version_check",
    "update.manifest_check",
    "keys.prefix",
    "keys.help",
    "keys.settings",
    "keys.new_workspace",
    "keys.new_worktree",
    "keys.open_worktree",
    "keys.remove_worktree",
    "keys.rename_workspace",
    "keys.close_workspace",
    "keys.workspace_picker",
    "keys.goto",
    "keys.navigate_workspace_up",
    "keys.navigate_workspace_down",
    "keys.navigate_pane_left",
    "keys.navigate_pane_down",
    "keys.navigate_pane_up",
    "keys.navigate_pane_right",
    "keys.detach",
    "keys.reload_config",
    "keys.open_notification_target",
    "keys.previous_workspace",
    "keys.next_workspace",
    "keys.previous_agent",
    "keys.next_agent",
    "keys.focus_agent",
    "keys.remote_image_paste",
    "keys.new_tab",
    "keys.rename_tab",
    "keys.previous_tab",
    "keys.next_tab",
    "keys.switch_tab",
    "keys.switch_workspace",
    "keys.close_tab",
    "keys.rename_pane",
    "keys.edit_scrollback",
    "keys.copy_mode",
    "keys.focus_input",
    "keys.open_folder",
    "keys.dock_agent",
    "keys.focus_pane_left",
    "keys.focus_pane_down",
    "keys.focus_pane_up",
    "keys.focus_pane_right",
    "keys.swap_pane_left",
    "keys.swap_pane_down",
    "keys.swap_pane_up",
    "keys.swap_pane_right",
    "keys.cycle_pane_next",
    "keys.cycle_pane_previous",
    "keys.last_pane",
    "keys.split_vertical",
    "keys.split_horizontal",
    "keys.close_pane",
    "keys.zoom",
    "keys.resize_mode",
    "keys.toggle_sidebar",
    "keys.indexed.tabs",
    "keys.indexed.workspaces",
    "keys.indexed.agents",
    "ui.dock.agents",
    "ui.sidebar_width",
    "ui.sidebar_min_width",
    "ui.sidebar_max_width",
    "ui.sidebar_section_split",
    "ui.sidebar_start_collapsed",
    "ui.sidebar_collapsed_mode",
    "ui.mobile_width_threshold",
    "ui.mouse_capture",
    "ui.copy_on_select",
    "ui.host_cursor",
    "ui.right_click_passthrough_modifier",
    "ui.redraw_on_focus_gained",
    "ui.mouse_scroll_lines",
    "ui.confirm_close",
    "ui.prompt_new_tab_name",
    "ui.prompt_new_workspace_name",
    "ui.pane_borders",
    "ui.pane_scrollbars",
    "ui.pane_gaps",
    "ui.show_agent_labels_on_pane_borders",
    "ui.hide_tab_bar_when_single_tab",
    "ui.tab_bar_position",
    "ui.agent_panel_sort",
    "ui.sidebar.agents.row_gap",
    "ui.sidebar.agents.rows",
    "ui.sidebar.agents.rows_by_agent",
    "ui.sidebar.spaces.row_gap",
    "ui.sidebar.spaces.rows",
    "ui.accent",
    "ui.toast.delivery",
    "ui.toast.delay_seconds",
    "ui.toast.starcil.position",
    "ui.toast.clipboard.enabled",
    "ui.toast.clipboard.position",
    "ui.sound.enabled",
    "ui.sound.path",
    "ui.sound.done_path",
    "ui.sound.request_path",
    "ui.sound.agents.pi",
    "ui.sound.agents.claude",
    "ui.sound.agents.codex",
    "ui.sound.agents.gemini",
    "ui.sound.agents.cursor",
    "ui.sound.agents.devin",
    "ui.sound.agents.agy",
    "ui.sound.agents.cline",
    "ui.sound.agents.open_code",
    "ui.sound.agents.github_copilot",
    "ui.sound.agents.kimi",
    "ui.sound.agents.kiro",
    "ui.sound.agents.droid",
    "ui.sound.agents.amp",
    "ui.sound.agents.grok",
    "ui.sound.agents.hermes",
    "ui.sound.agents.kilo",
    "ui.sound.agents.qodercli",
    "ui.sound.agents.maki",
    "session.resume_agents_on_restore",
    "worktrees.directory",
    "remote.manage_ssh_config",
    "advanced.scrollback_limit_bytes",
    "experimental.allow_nested",
    "experimental.kitty_graphics",
    "experimental.pane_history",
    "experimental.reveal_hidden_cursor_for_cjk_ime",
    "experimental.cjk_ime_agents",
    "experimental.cjk_ime_cursor_shape",
    "experimental.switch_ascii_input_source_in_prefix",
];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// `None` is the first-run default; missing and explicit `true` both show onboarding.
    pub onboarding: Option<bool>,
    pub theme: Theme,
    pub terminal: Terminal,
    pub update: Update,
    pub keys: Keys,
    pub worktrees: Worktrees,
    pub ui: Ui,
    pub session: Session,
    pub remote: Remote,
    pub experimental: Experimental,
    pub advanced: Advanced,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            onboarding: None,
            theme: Theme::default(),
            terminal: Terminal::default(),
            update: Update::default(),
            keys: Keys::default(),
            worktrees: Worktrees::default(),
            ui: Ui::default(),
            session: Session::default(),
            remote: Remote::default(),
            experimental: Experimental::default(),
            advanced: Advanced::default(),
        }
    }
}

impl Config {
    pub fn from_toml(source: &str) -> ConfigReport {
        parse_config(source)
    }

    pub fn should_show_onboarding(&self) -> bool {
        self.onboarding.unwrap_or(true)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ShellMode {
    #[default]
    Auto,
    Login,
    NonLogin,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NewCwd {
    Follow,
    Home,
    Current,
    Path(String),
}

impl Default for NewCwd {
    fn default() -> Self {
        Self::Follow
    }
}

impl Serialize for NewCwd {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(match self {
            Self::Follow => "follow",
            Self::Home => "home",
            Self::Current => "current",
            Self::Path(path) => path,
        })
    }
}

impl<'de> Deserialize<'de> for NewCwd {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Ok(match value.as_str() {
            "follow" => Self::Follow,
            "home" => Self::Home,
            "current" => Self::Current,
            _ => Self::Path(value),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Terminal {
    pub default_shell: String,
    pub shell_mode: ShellMode,
    pub new_cwd: NewCwd,
}

impl Default for Terminal {
    fn default() -> Self {
        Self {
            default_shell: String::new(),
            shell_mode: ShellMode::Auto,
            new_cwd: NewCwd::Follow,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UpdateChannel {
    Stable,
    Preview,
}

impl Default for UpdateChannel {
    /// Stable everywhere: releases are plain `vX.Y.Z` tags and nobody is
    /// opted into previews by default (`update.channel = "preview"` opts in).
    /// Windows used to default to preview, which made the in-app updater
    /// report "up to date" forever on a stable-only release train.
    fn default() -> Self {
        Self::Stable
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Update {
    pub channel: UpdateChannel,
    pub version_check: bool,
    pub manifest_check: bool,
}

impl Default for Update {
    fn default() -> Self {
        Self {
            channel: UpdateChannel::default(),
            version_check: true,
            manifest_check: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Worktrees {
    pub directory: String,
}

impl Default for Worktrees {
    fn default() -> Self {
        Self {
            directory: "~/.starcil/worktrees".to_owned(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SidebarCollapsedMode {
    #[default]
    Compact,
    Hidden,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum HostCursor {
    #[default]
    Auto,
    Native,
    Drawn,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum AgentPanelSort {
    #[default]
    #[serde(alias = "workspaces")]
    Spaces,
    Priority,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum TabBarPosition {
    #[default]
    Top,
    Bottom,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum RowToken {
    Plain(String),
    Styled(StyledToken),
}

impl RowToken {
    pub fn token(&self) -> &str {
        match self {
            Self::Plain(token) => token,
            Self::Styled(styled) => &styled.token,
        }
    }
}

impl From<&str> for RowToken {
    fn from(value: &str) -> Self {
        Self::Plain(value.to_owned())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StyledToken {
    pub token: String,
    #[serde(default)]
    pub fg: Option<String>,
    #[serde(default)]
    pub bold: Option<bool>,
    #[serde(default)]
    pub dim: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AgentRows {
    pub row_gap: u8,
    pub rows: Vec<Vec<RowToken>>,
    pub rows_by_agent: BTreeMap<String, Vec<Vec<RowToken>>>,
}

impl Default for AgentRows {
    fn default() -> Self {
        Self {
            row_gap: 0,
            rows: vec![
                vec!["state_icon".into(), "pane".into(), "agent_kind".into()],
                vec!["state_text".into()],
            ],
            rows_by_agent: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SpaceRows {
    pub row_gap: u8,
    pub rows: Vec<Vec<RowToken>>,
}

impl Default for SpaceRows {
    fn default() -> Self {
        Self {
            row_gap: 0,
            rows: vec![
                vec!["index".into(), "workspace".into(), "state_icon".into()],
                vec!["branch".into(), "git_status".into()],
            ],
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Sidebar {
    pub agents: AgentRows,
    pub spaces: SpaceRows,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum ToastPosition {
    TopLeft,
    TopCenter,
    TopRight,
    BottomLeft,
    BottomCenter,
    #[default]
    BottomRight,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum InAppToastPosition {
    TopLeft,
    TopRight,
    BottomLeft,
    #[default]
    BottomRight,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ToastDelivery {
    #[default]
    Off,
    Starcil,
    Terminal,
    System,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct InAppToast {
    pub position: InAppToastPosition,
}

impl Default for InAppToast {
    fn default() -> Self {
        Self {
            position: InAppToastPosition::BottomRight,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ClipboardToast {
    pub enabled: bool,
    pub position: ToastPosition,
}

impl Default for ClipboardToast {
    fn default() -> Self {
        Self {
            enabled: true,
            position: ToastPosition::BottomCenter,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Toast {
    pub delivery: ToastDelivery,
    pub delay_seconds: u64,
    pub starcil: InAppToast,
    pub clipboard: ClipboardToast,
}

impl Default for Toast {
    fn default() -> Self {
        Self {
            delivery: ToastDelivery::Off,
            delay_seconds: 1,
            starcil: InAppToast::default(),
            clipboard: ClipboardToast::default(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SoundPolicy {
    #[default]
    Default,
    On,
    Off,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SoundAgents {
    pub pi: SoundPolicy,
    pub claude: SoundPolicy,
    pub codex: SoundPolicy,
    pub gemini: SoundPolicy,
    pub cursor: SoundPolicy,
    pub devin: SoundPolicy,
    pub agy: SoundPolicy,
    pub cline: SoundPolicy,
    pub open_code: SoundPolicy,
    pub github_copilot: SoundPolicy,
    pub kimi: SoundPolicy,
    pub kiro: SoundPolicy,
    pub droid: SoundPolicy,
    pub amp: SoundPolicy,
    pub grok: SoundPolicy,
    pub hermes: SoundPolicy,
    pub kilo: SoundPolicy,
    pub qodercli: SoundPolicy,
    pub maki: SoundPolicy,
}

impl Default for SoundAgents {
    fn default() -> Self {
        Self {
            pi: SoundPolicy::Default,
            claude: SoundPolicy::Default,
            codex: SoundPolicy::Default,
            gemini: SoundPolicy::Default,
            cursor: SoundPolicy::Default,
            devin: SoundPolicy::Default,
            agy: SoundPolicy::Default,
            cline: SoundPolicy::Default,
            open_code: SoundPolicy::Default,
            github_copilot: SoundPolicy::Default,
            kimi: SoundPolicy::Default,
            kiro: SoundPolicy::Default,
            droid: SoundPolicy::Off,
            amp: SoundPolicy::Default,
            grok: SoundPolicy::Default,
            hermes: SoundPolicy::Default,
            kilo: SoundPolicy::Default,
            qodercli: SoundPolicy::Default,
            maki: SoundPolicy::Default,
        }
    }
}

impl SoundAgents {
    pub fn get(&self, agent: &str) -> Option<SoundPolicy> {
        Some(match agent {
            "pi" => self.pi,
            "claude" => self.claude,
            "codex" => self.codex,
            "gemini" => self.gemini,
            "cursor" => self.cursor,
            "devin" => self.devin,
            "agy" => self.agy,
            "cline" => self.cline,
            "open_code" => self.open_code,
            "github_copilot" => self.github_copilot,
            "kimi" => self.kimi,
            "kiro" => self.kiro,
            "droid" => self.droid,
            "amp" => self.amp,
            "grok" => self.grok,
            "hermes" => self.hermes,
            "kilo" => self.kilo,
            "qodercli" => self.qodercli,
            "maki" => self.maki,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Sound {
    pub enabled: bool,
    pub path: Option<String>,
    pub done_path: Option<String>,
    pub request_path: Option<String>,
    pub agents: SoundAgents,
}

impl Default for Sound {
    fn default() -> Self {
        Self {
            enabled: true,
            path: None,
            done_path: None,
            request_path: None,
            agents: SoundAgents::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Ui {
    pub sidebar_width: u16,
    pub sidebar_min_width: u16,
    pub sidebar_max_width: u16,
    /// Share of the sidebar height given to the workspaces section
    /// (`sidebar_section_split`, a 0.0–1.0 ratio in TOML). Kept as a percent
    /// in memory so the config stays `Eq`; the divider between the sections
    /// drags it.
    #[serde(rename = "sidebar_section_split", with = "ratio_percent")]
    pub sidebar_section_split_percent: u8,
    pub sidebar_start_collapsed: bool,
    pub sidebar_collapsed_mode: SidebarCollapsedMode,
    pub mobile_width_threshold: u16,
    pub mouse_capture: bool,
    pub copy_on_select: bool,
    pub host_cursor: HostCursor,
    pub right_click_passthrough_modifier: String,
    pub redraw_on_focus_gained: bool,
    pub mouse_scroll_lines: u16,
    pub confirm_close: bool,
    pub prompt_new_tab_name: bool,
    pub prompt_new_workspace_name: bool,
    pub pane_borders: bool,
    pub pane_scrollbars: bool,
    pub pane_gaps: bool,
    pub show_agent_labels_on_pane_borders: bool,
    pub hide_tab_bar_when_single_tab: bool,
    pub agent_panel_sort: AgentPanelSort,
    pub tab_bar_position: TabBarPosition,
    pub sidebar: Sidebar,
    pub accent: String,
    pub dock: Dock,
    pub toast: Toast,
    pub sound: Sound,
}

/// The agent dock: clickable launchers above the bottom command input.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Dock {
    /// CLI binaries looked up on PATH at startup (and on config reload).
    /// Extend or trim the list freely; order is the dock order.
    pub agents: Vec<String>,
}

impl Default for Dock {
    fn default() -> Self {
        Self {
            agents: [
                "claude", "codex", "opencode", "aider", "gemini", "kimi", "deepseek",
            ]
            .iter()
            .map(|name| (*name).to_owned())
            .collect(),
        }
    }
}

impl Default for Ui {
    fn default() -> Self {
        Self {
            sidebar_width: 26,
            sidebar_min_width: 18,
            sidebar_max_width: 36,
            sidebar_section_split_percent: 50,
            sidebar_start_collapsed: false,
            sidebar_collapsed_mode: SidebarCollapsedMode::Compact,
            mobile_width_threshold: 64,
            mouse_capture: true,
            copy_on_select: true,
            host_cursor: HostCursor::Auto,
            right_click_passthrough_modifier: String::new(),
            redraw_on_focus_gained: true,
            mouse_scroll_lines: 3,
            confirm_close: true,
            prompt_new_tab_name: true,
            prompt_new_workspace_name: false,
            pane_borders: true,
            pane_scrollbars: true,
            pane_gaps: false,
            show_agent_labels_on_pane_borders: true,
            hide_tab_bar_when_single_tab: false,
            agent_panel_sort: AgentPanelSort::Spaces,
            tab_bar_position: TabBarPosition::Top,
            sidebar: Sidebar::default(),
            accent: "#4a9eff".to_owned(),
            dock: Dock::default(),
            toast: Toast::default(),
            sound: Sound::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Session {
    pub resume_agents_on_restore: bool,
}

impl Default for Session {
    fn default() -> Self {
        Self {
            resume_agents_on_restore: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Remote {
    pub manage_ssh_config: bool,
}

impl Default for Remote {
    fn default() -> Self {
        Self {
            manage_ssh_config: true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum CjkImeCursorShape {
    Block,
    #[default]
    SteadyBlock,
    Underline,
    SteadyUnderline,
    Bar,
    SteadyBar,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Experimental {
    pub allow_nested: bool,
    pub kitty_graphics: bool,
    pub pane_history: bool,
    pub switch_ascii_input_source_in_prefix: bool,
    pub reveal_hidden_cursor_for_cjk_ime: bool,
    pub cjk_ime_agents: Vec<String>,
    pub cjk_ime_cursor_shape: CjkImeCursorShape,
}

impl Default for Experimental {
    fn default() -> Self {
        Self {
            allow_nested: false,
            kitty_graphics: false,
            pane_history: false,
            switch_ascii_input_source_in_prefix: false,
            reveal_hidden_cursor_for_cjk_ime: false,
            cjk_ime_agents: Vec::new(),
            cjk_ime_cursor_shape: CjkImeCursorShape::SteadyBlock,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Advanced {
    #[serde(alias = "scrollback_lines")]
    pub scrollback_limit_bytes: u64,
}

impl Default for Advanced {
    fn default() -> Self {
        Self {
            scrollback_limit_bytes: 10_000_000,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Warning,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Diagnostic {
    pub severity: Severity,
    pub path: String,
    pub message: String,
}

impl Diagnostic {
    pub fn warning(path: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Warning,
            path: path.into(),
            message: message.into(),
        }
    }

    pub fn error(path: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Error,
            path: path.into(),
            message: message.into(),
        }
    }

    pub fn toml_path(&self) -> &str {
        &self.path
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigReport {
    pub config: Config,
    pub diagnostics: Vec<Diagnostic>,
}

impl ConfigReport {
    pub fn is_valid(&self) -> bool {
        !self
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.severity == Severity::Error)
    }
}

/// Parse a configuration while retaining unknown-key warnings. A type or syntax
/// error returns safe defaults alongside an error diagnostic.
pub fn parse_config(source: &str) -> ConfigReport {
    let value = match toml::from_str::<Value>(source) {
        Ok(value) => value,
        Err(error) => {
            let path = diagnostic_path(source, &error);
            return ConfigReport {
                config: Config::default(),
                diagnostics: vec![Diagnostic::error(path, error.message().to_owned())],
            };
        }
    };

    let mut diagnostics = collect_unknown_keys(&value);
    let config = match toml::from_str::<Config>(source) {
        Ok(config) => config,
        Err(error) => {
            let path = diagnostic_path(source, &error);
            diagnostics.push(Diagnostic::error(path, error.message().to_owned()));
            return ConfigReport {
                config: Config::default(),
                diagnostics,
            };
        }
    };
    diagnostics.extend(validate_config(&config));
    ConfigReport { config, diagnostics }
}

pub fn load(path: impl AsRef<Path>) -> ConfigReport {
    let path = path.as_ref();
    match fs::read_to_string(path) {
        Ok(source) => parse_config(&source),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => ConfigReport {
            config: Config::default(),
            diagnostics: Vec::new(),
        },
        Err(error) => ConfigReport {
            config: Config::default(),
            diagnostics: vec![Diagnostic::error(
                "$",
                format!("could not read {}: {error}", path.display()),
            )],
        },
    }
}

pub fn check(path: impl AsRef<Path>) -> Vec<Diagnostic> {
    load(path).diagnostics
}

/// Resolve `STARCIL_CONFIG_PATH` or the platform's conventional Starcil path.
pub fn config_path() -> Option<PathBuf> {
    std::env::var_os("STARCIL_CONFIG_PATH")
        .map(PathBuf::from)
        .or_else(default_config_path)
}

pub fn default_config_path() -> Option<PathBuf> {
    if cfg!(windows) {
        std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .map(|directory| directory.join("starcil").join("config.toml"))
    } else if let Some(directory) = std::env::var_os("XDG_CONFIG_HOME") {
        Some(PathBuf::from(directory).join("starcil").join("config.toml"))
    } else {
        std::env::var_os("HOME")
            .map(PathBuf::from)
            .map(|directory| directory.join(".config").join("starcil").join("config.toml"))
    }
}

pub fn validate_config(config: &Config) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();

    let mut theme_names = BTreeSet::new();
    theme_names.insert(("theme.name", config.theme.name.as_str()));
    if let Some(name) = config.theme.dark_name.as_deref() {
        theme_names.insert(("theme.dark_name", name));
    }
    if let Some(name) = config.theme.light_name.as_deref() {
        theme_names.insert(("theme.light_name", name));
    }
    for (path, name) in theme_names {
        if let Err(error) = builtin_theme(name) {
            diagnostics.push(Diagnostic::error(path, error.to_string()));
        }
    }
    for (token, value) in &config.theme.custom {
        if !TOKEN_NAMES.contains(&token.as_str()) {
            diagnostics.push(Diagnostic::warning(
                format!("theme.custom.{token}"),
                ThemeError::UnknownToken(token.clone()).to_string(),
            ));
            continue;
        }
        if let Err(error) = value.parse::<Color>() {
            diagnostics.push(Diagnostic::error(
                format!("theme.custom.{token}"),
                error.to_string(),
            ));
        }
    }

    if matches!(&config.terminal.new_cwd, NewCwd::Path(path) if path.trim().is_empty()) {
        diagnostics.push(Diagnostic::error(
            "terminal.new_cwd",
            "fixed working-directory path cannot be empty",
        ));
    }
    if config.worktrees.directory.trim().is_empty() {
        diagnostics.push(Diagnostic::error(
            "worktrees.directory",
            "worktree directory cannot be empty",
        ));
    }

    diagnostics.extend(build_effective_keymap(&config.keys).diagnostics);

    if config.ui.sidebar_min_width == 0 {
        diagnostics.push(Diagnostic::error(
            "ui.sidebar_min_width",
            "sidebar minimum width must be greater than zero",
        ));
    }
    if config.ui.sidebar_min_width > config.ui.sidebar_max_width {
        diagnostics.push(Diagnostic::error(
            "ui.sidebar_min_width",
            "sidebar_min_width cannot exceed sidebar_max_width",
        ));
    }
    if config.ui.sidebar_width < config.ui.sidebar_min_width
        || config.ui.sidebar_width > config.ui.sidebar_max_width
    {
        diagnostics.push(Diagnostic::warning(
            "ui.sidebar_width",
            "sidebar_width is outside the configured min/max range and will be clamped",
        ));
    }
    if config.ui.mobile_width_threshold == 0 {
        diagnostics.push(Diagnostic::error(
            "ui.mobile_width_threshold",
            "mobile width threshold must be greater than zero",
        ));
    }
    if config.ui.mouse_scroll_lines == 0 {
        diagnostics.push(Diagnostic::error(
            "ui.mouse_scroll_lines",
            "mouse scroll lines must be greater than zero",
        ));
    }

    if let Err(message) = validate_mouse_modifier_combo(&config.ui.right_click_passthrough_modifier) {
        diagnostics.push(Diagnostic::error(
            "ui.right_click_passthrough_modifier",
            message,
        ));
    }

    if config.ui.toast.delay_seconds > 3600 {
        diagnostics.push(Diagnostic::error(
            "ui.toast.delay_seconds",
            "toast delay must be between 0 and 3600 seconds",
        ));
    }

    match config.ui.accent.parse::<Color>() {
        Ok(Color::Reset) => diagnostics.push(Diagnostic::error(
            "ui.accent",
            "UI accent cannot be reset",
        )),
        Ok(_) => {}
        Err(error) => diagnostics.push(Diagnostic::error("ui.accent", error.to_string())),
    }

    validate_rows(
        &config.ui.sidebar.agents.rows,
        RowKind::Agent,
        "ui.sidebar.agents.rows",
        &mut diagnostics,
    );
    for (agent, rows) in &config.ui.sidebar.agents.rows_by_agent {
        if !is_canonical_id(agent) {
            diagnostics.push(Diagnostic::error(
                format!("ui.sidebar.agents.rows_by_agent.{agent}"),
                "agent id must be a lowercase canonical identifier",
            ));
        }
        validate_rows(
            rows,
            RowKind::Agent,
            &format!("ui.sidebar.agents.rows_by_agent.{agent}"),
            &mut diagnostics,
        );
    }
    validate_rows(
        &config.ui.sidebar.spaces.rows,
        RowKind::Space,
        "ui.sidebar.spaces.rows",
        &mut diagnostics,
    );

    for (field, path) in [
        (&config.ui.sound.path, "ui.sound.path"),
        (&config.ui.sound.done_path, "ui.sound.done_path"),
        (&config.ui.sound.request_path, "ui.sound.request_path"),
    ] {
        if let Some(file) = field {
            let lowercase = file.to_ascii_lowercase();
            if !lowercase.ends_with(".mp3") && !lowercase.ends_with(".wav") {
                diagnostics.push(Diagnostic::error(
                    path,
                    "custom sound path must end in .mp3 or .wav",
                ));
            }
        }
    }

    const CJK_AGENTS: &[&str] = &[
        "pi", "claude", "codex", "gemini", "cursor", "devin", "cline", "opencode",
        "copilot", "kimi", "kiro", "droid", "amp", "grok", "hermes", "kilo", "qodercli",
        "qoder", "maki",
    ];
    for (index, agent) in config.experimental.cjk_ime_agents.iter().enumerate() {
        if !CJK_AGENTS.contains(&agent.to_ascii_lowercase().as_str()) {
            diagnostics.push(Diagnostic::warning(
                format!("experimental.cjk_ime_agents[{index}]"),
                format!("unknown agent `{agent}`; this entry will not match a pane"),
            ));
        }
    }
    if config.advanced.scrollback_limit_bytes == 0 {
        diagnostics.push(Diagnostic::error(
            "advanced.scrollback_limit_bytes",
            "scrollback limit must be greater than zero",
        ));
    }

    diagnostics
}

#[derive(Clone, Copy)]
enum RowKind {
    Agent,
    Space,
}

fn validate_rows(rows: &[Vec<RowToken>], kind: RowKind, path: &str, diagnostics: &mut Vec<Diagnostic>) {
    if rows.len() > 16 {
        diagnostics.push(Diagnostic::error(path, "layout supports at most 16 rows"));
    }
    let allowed = match kind {
        RowKind::Agent => &[
            "state_icon",
            "state_text",
            "workspace",
            "tab",
            "pane",
            "agent",
            "agent_kind",
            "terminal_title",
            "terminal_title_stripped",
        ][..],
        RowKind::Space => &[
            "state_icon",
            "state_text",
            "workspace",
            "index",
            "branch",
            "git_status",
        ][..],
    };

    for (row_index, row) in rows.iter().enumerate() {
        if row.len() > 16 {
            diagnostics.push(Diagnostic::error(
                format!("{path}[{row_index}]"),
                "layout row supports at most 16 tokens",
            ));
        }
        for (token_index, token) in row.iter().enumerate() {
            let token_path = format!("{path}[{row_index}][{token_index}]");
            let name = token.token();
            if !allowed.contains(&name) && !valid_custom_token(name) {
                diagnostics.push(Diagnostic::error(
                    &token_path,
                    format!("unknown sidebar token `{name}`"),
                ));
            }
            if let RowToken::Styled(style) = token {
                if let Some(fg) = style.fg.as_deref() {
                    if !strict_hex_color(fg) {
                        diagnostics.push(Diagnostic::error(
                            format!("{token_path}.fg"),
                            "styled token foreground must be #RGB or #RRGGBB",
                        ));
                    }
                }
            }
        }
    }
}

fn valid_custom_token(token: &str) -> bool {
    token
        .strip_prefix('$')
        .is_some_and(|name| !name.is_empty() && is_canonical_id(name))
}

fn is_canonical_id(value: &str) -> bool {
    value.chars().enumerate().all(|(index, character)| {
        character.is_ascii_lowercase()
            || character.is_ascii_digit() && index > 0
            || matches!(character, '-' | '_') && index > 0
    }) && !value.is_empty()
}

fn strict_hex_color(value: &str) -> bool {
    let Some(hex) = value.strip_prefix('#') else {
        return false;
    };
    matches!(hex.len(), 3 | 6) && hex.chars().all(|character| character.is_ascii_hexdigit())
}

fn validate_mouse_modifier_combo(value: &str) -> Result<(), String> {
    let value = value.trim().to_ascii_lowercase();
    if matches!(value.as_str(), "" | "off" | "none") {
        return Ok(());
    }

    let mut seen = BTreeSet::new();
    for part in value.split('+') {
        let canonical = match part.trim() {
            "ctrl" | "control" => "ctrl",
            "alt" | "option" => "alt",
            "cmd" | "command" => "cmd",
            "super" | "meta" | "win" => "super",
            "hyper" => "hyper",
            "shift" => return Err("shift is reserved by terminals and cannot be used for right-click passthrough".to_owned()),
            "" => return Err("right-click passthrough modifier contains an empty component".to_owned()),
            unknown => return Err(format!("unknown right-click passthrough modifier `{unknown}`")),
        };
        if !seen.insert(canonical) {
            return Err(format!("duplicate right-click passthrough modifier `{canonical}`"));
        }
    }
    Ok(())
}

fn collect_unknown_keys(value: &Value) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();
    let Some(root) = value.as_table() else {
        return diagnostics;
    };

    warn_unknown(
        root,
        &[
            "onboarding", "theme", "terminal", "update", "keys", "worktrees", "ui",
            "session", "remote", "experimental", "advanced",
        ],
        "",
        &mut diagnostics,
    );

    check_child(root, "theme", &["name", "auto_switch", "dark_name", "light_name", "custom"], "theme", &mut diagnostics);
    check_child(root, "terminal", &["default_shell", "shell_mode", "new_cwd"], "terminal", &mut diagnostics);
    check_child(root, "update", &["channel", "version_check", "manifest_check"], "update", &mut diagnostics);
    check_child(root, "worktrees", &["directory"], "worktrees", &mut diagnostics);
    check_child(root, "session", &["resume_agents_on_restore"], "session", &mut diagnostics);
    check_child(root, "remote", &["manage_ssh_config"], "remote", &mut diagnostics);
    check_child(
        root,
        "experimental",
        &[
            "allow_nested", "kitty_graphics", "pane_history", "switch_ascii_input_source_in_prefix",
            "reveal_hidden_cursor_for_cjk_ime", "cjk_ime_agents", "cjk_ime_cursor_shape",
        ],
        "experimental",
        &mut diagnostics,
    );
    check_child(root, "advanced", &["scrollback_limit_bytes", "scrollback_lines"], "advanced", &mut diagnostics);

    if let Some(keys) = root.get("keys").and_then(Value::as_table) {
        warn_unknown(
            keys,
            &[
                "prefix", "help", "settings", "detach", "reload_config", "open_notification_target",
                "workspace_picker", "goto", "new_workspace", "new_worktree", "open_worktree",
                "remove_worktree", "rename_workspace", "close_workspace", "previous_workspace",
                "next_workspace", "previous_agent", "next_agent", "focus_agent", "remote_image_paste",
                "new_tab", "rename_tab", "previous_tab", "next_tab", "switch_tab", "switch_workspace",
                "close_tab", "rename_pane", "edit_scrollback", "focus_pane_left", "focus_pane_down",
                "copy_mode", "focus_pane_up", "focus_pane_right", "swap_pane_left", "swap_pane_down",
                "swap_pane_up", "swap_pane_right", "cycle_pane_next", "cycle_pane_previous",
                "last_pane", "split_vertical", "split_horizontal", "close_pane", "zoom", "fullscreen",
                "resize_mode", "toggle_sidebar", "navigate_workspace_up", "navigate_workspace_down",
                "navigate_pane_left", "navigate_pane_down", "navigate_pane_up", "navigate_pane_right",
                "command", "indexed",
            ],
            "keys",
            &mut diagnostics,
        );
        check_child(keys, "indexed", &["tabs", "workspaces", "agents"], "keys.indexed", &mut diagnostics);
        if let Some(commands) = keys.get("command").and_then(Value::as_array) {
            for (index, command) in commands.iter().enumerate() {
                if let Some(command) = command.as_table() {
                    warn_unknown(
                        command,
                        &["key", "type", "command", "description", "width", "height"],
                        &format!("keys.command[{index}]"),
                        &mut diagnostics,
                    );
                }
            }
        }
    }

    if let Some(ui) = root.get("ui").and_then(Value::as_table) {
        warn_unknown(
            ui,
            &[
                "sidebar_width", "sidebar_min_width", "sidebar_max_width", "sidebar_section_split",
                "sidebar_start_collapsed",
                "sidebar_collapsed_mode", "mobile_width_threshold", "mouse_capture", "copy_on_select",
                "host_cursor", "right_click_passthrough_modifier", "redraw_on_focus_gained",
                "mouse_scroll_lines", "confirm_close", "prompt_new_tab_name",
                "prompt_new_workspace_name", "pane_borders", "pane_scrollbars", "pane_gaps",
                "show_agent_labels_on_pane_borders", "hide_tab_bar_when_single_tab",
                "agent_panel_sort", "tab_bar_position", "sidebar", "accent", "toast", "sound",
            ],
            "ui",
            &mut diagnostics,
        );
        if let Some(sidebar) = ui.get("sidebar").and_then(Value::as_table) {
            warn_unknown(sidebar, &["agents", "spaces"], "ui.sidebar", &mut diagnostics);
            if let Some(agents) = sidebar.get("agents").and_then(Value::as_table) {
                warn_unknown(agents, &["row_gap", "rows", "rows_by_agent"], "ui.sidebar.agents", &mut diagnostics);
                inspect_inline_rows(agents.get("rows"), "ui.sidebar.agents.rows", &mut diagnostics);
                if let Some(overrides) = agents.get("rows_by_agent").and_then(Value::as_table) {
                    for (agent, rows) in overrides {
                        inspect_inline_rows(Some(rows), &format!("ui.sidebar.agents.rows_by_agent.{agent}"), &mut diagnostics);
                    }
                }
            }
            if let Some(spaces) = sidebar.get("spaces").and_then(Value::as_table) {
                warn_unknown(spaces, &["row_gap", "rows"], "ui.sidebar.spaces", &mut diagnostics);
                inspect_inline_rows(spaces.get("rows"), "ui.sidebar.spaces.rows", &mut diagnostics);
            }
        }
        if let Some(toast) = ui.get("toast").and_then(Value::as_table) {
            warn_unknown(toast, &["delivery", "delay_seconds", "starcil", "clipboard"], "ui.toast", &mut diagnostics);
            check_child(toast, "starcil", &["position"], "ui.toast.starcil", &mut diagnostics);
            check_child(toast, "clipboard", &["enabled", "position"], "ui.toast.clipboard", &mut diagnostics);
        }
        if let Some(sound) = ui.get("sound").and_then(Value::as_table) {
            warn_unknown(sound, &["enabled", "path", "done_path", "request_path", "agents"], "ui.sound", &mut diagnostics);
            check_child(
                sound,
                "agents",
                &[
                    "pi", "claude", "codex", "gemini", "cursor", "devin", "agy", "cline",
                    "open_code", "github_copilot", "kimi", "kiro", "droid", "amp", "grok",
                    "hermes", "kilo", "qodercli", "maki",
                ],
                "ui.sound.agents",
                &mut diagnostics,
            );
        }
    }

    diagnostics
}

fn check_child(
    parent: &toml::map::Map<String, Value>,
    key: &str,
    allowed: &[&str],
    path: &str,
    diagnostics: &mut Vec<Diagnostic>,
) {
    if let Some(table) = parent.get(key).and_then(Value::as_table) {
        warn_unknown(table, allowed, path, diagnostics);
    }
}

fn warn_unknown(
    table: &toml::map::Map<String, Value>,
    allowed: &[&str],
    parent: &str,
    diagnostics: &mut Vec<Diagnostic>,
) {
    for key in table.keys() {
        if !allowed.contains(&key.as_str()) {
            let path = if parent.is_empty() {
                key.clone()
            } else {
                format!("{parent}.{key}")
            };
            diagnostics.push(Diagnostic::warning(path, "unknown configuration key; ignored"));
        }
    }
}

fn inspect_inline_rows(value: Option<&Value>, path: &str, diagnostics: &mut Vec<Diagnostic>) {
    let Some(rows) = value.and_then(Value::as_array) else {
        return;
    };
    for (row_index, row) in rows.iter().enumerate() {
        let Some(tokens) = row.as_array() else {
            continue;
        };
        for (token_index, token) in tokens.iter().enumerate() {
            if let Some(style) = token.as_table() {
                warn_unknown(
                    style,
                    &["token", "fg", "bold", "dim"],
                    &format!("{path}[{row_index}][{token_index}]"),
                    diagnostics,
                );
            }
        }
    }
}

fn diagnostic_path(source: &str, error: &toml::de::Error) -> String {
    let rendered = error.to_string();
    for marker in ["in `", "for key `"] {
        if let Some(start) = rendered.rfind(marker) {
            let rest = &rendered[start + marker.len()..];
            if let Some(end) = rest.find('`') {
                let path = &rest[..end];
                if !path.is_empty() {
                    return path.to_owned();
                }
            }
        }
    }

    let Some(span) = error.span() else {
        return "$".to_owned();
    };
    let offset = span.start.min(source.len());
    let before = &source[..offset];
    let mut table = String::new();
    for line in before.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            table = trimmed.trim_matches('[').trim_matches(']').trim().to_owned();
        }
    }
    let line = before.rsplit('\n').next().unwrap_or_default().trim();
    let key = line
        .split_once('=')
        .map(|(key, _)| key.trim().trim_matches('"'))
        .filter(|key| !key.is_empty());
    match (table.is_empty(), key) {
        (false, Some(key)) => format!("{table}.{key}"),
        (true, Some(key)) => key.to_owned(),
        (false, None) => table,
        (true, None) => "$".to_owned(),
    }
}

pub fn default_config_template() -> &'static str {
    include_str!("default-config.toml")
}

#[derive(Debug, Error)]
pub enum ConfigFileError {
    #[error("could not read {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("configuration {path} is not UTF-8: {source}")]
    Utf8 {
        path: PathBuf,
        #[source]
        source: std::string::FromUtf8Error,
    },
    #[error("could not parse {path} while resetting keys: {source}")]
    EditParse {
        path: PathBuf,
        #[source]
        source: toml_edit::TomlError,
    },
    #[error("could not create backup {path}: {source}")]
    Backup {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("could not write {path}: {source}")]
    Write {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("invalid value for {path}: {message}")]
    InvalidSetting { path: String, message: String },
}

/// One settings-editor mutation. Keeping this typed avoids stringly-typed
/// writes while still updating only the selected TOML key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigSetting {
    ThemeName(String),
    Accent(String),
    ToastDelivery(ToastDelivery),
    SoundEnabled(bool),
    MouseCapture(bool),
    CopyOnSelect(bool),
    PaneBorders(bool),
    PaneGaps(bool),
    ShowAgentLabels(bool),
    SidebarWidth(u16),
    SidebarSectionSplit(u8),
    SidebarStartCollapsed(bool),
    SidebarCollapsedMode(SidebarCollapsedMode),
    ConfirmClose(bool),
    PromptNewTabName(bool),
    UpdateChannel(UpdateChannel),
}

/// Persist one interactive setting without serializing the full config, so
/// comments, ordering, and unrelated user formatting survive the edit.
pub fn save_config_setting(
    path: impl AsRef<Path>,
    setting: &ConfigSetting,
) -> Result<(), ConfigFileError> {
    let path = path.as_ref();
    let source = match fs::read(path) {
        Ok(bytes) => String::from_utf8(bytes).map_err(|source| ConfigFileError::Utf8 {
            path: path.to_owned(),
            source,
        })?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            default_config_template().to_owned()
        }
        Err(source) => {
            return Err(ConfigFileError::Read {
                path: path.to_owned(),
                source,
            });
        }
    };
    let mut document = DocumentMut::from_str(&source).map_err(|source| ConfigFileError::EditParse {
        path: path.to_owned(),
        source,
    })?;

    match setting {
        ConfigSetting::ThemeName(name) => {
            builtin_theme(name).map_err(|error| ConfigFileError::InvalidSetting {
                path: "theme.name".to_owned(),
                message: error.to_string(),
            })?;
            document["theme"]["name"] = toml_edit::value(name);
        }
        ConfigSetting::Accent(accent) => {
            let parsed = accent
                .parse::<Color>()
                .map_err(|error| ConfigFileError::InvalidSetting {
                    path: "ui.accent".to_owned(),
                    message: error.to_string(),
                })?;
            if parsed == Color::Reset {
                return Err(ConfigFileError::InvalidSetting {
                    path: "ui.accent".to_owned(),
                    message: "accent cannot be reset".to_owned(),
                });
            }
            document["ui"]["accent"] = toml_edit::value(accent);
        }
        ConfigSetting::ToastDelivery(delivery) => {
            document["ui"]["toast"]["delivery"] = toml_edit::value(match delivery {
                ToastDelivery::Off => "off",
                ToastDelivery::Starcil => "starcil",
                ToastDelivery::Terminal => "terminal",
                ToastDelivery::System => "system",
            });
        }
        ConfigSetting::SoundEnabled(enabled) => {
            document["ui"]["sound"]["enabled"] = toml_edit::value(*enabled);
        }
        ConfigSetting::MouseCapture(enabled) => {
            document["ui"]["mouse_capture"] = toml_edit::value(*enabled);
        }
        ConfigSetting::CopyOnSelect(enabled) => {
            document["ui"]["copy_on_select"] = toml_edit::value(*enabled);
        }
        ConfigSetting::PaneBorders(enabled) => {
            document["ui"]["pane_borders"] = toml_edit::value(*enabled);
        }
        ConfigSetting::PaneGaps(enabled) => {
            document["ui"]["pane_gaps"] = toml_edit::value(*enabled);
        }
        ConfigSetting::ShowAgentLabels(enabled) => {
            document["ui"]["show_agent_labels_on_pane_borders"] = toml_edit::value(*enabled);
        }
        ConfigSetting::SidebarWidth(width) => {
            if *width == 0 {
                return Err(ConfigFileError::InvalidSetting {
                    path: "ui.sidebar_width".to_owned(),
                    message: "width must be greater than zero".to_owned(),
                });
            }
            document["ui"]["sidebar_width"] = toml_edit::value(i64::from(*width));
        }
        ConfigSetting::SidebarSectionSplit(percent) => {
            if *percent > 100 {
                return Err(ConfigFileError::InvalidSetting {
                    path: "ui.sidebar_section_split".to_owned(),
                    message: "split must be between 0.0 and 1.0".to_owned(),
                });
            }
            document["ui"]["sidebar_section_split"] =
                toml_edit::value(f64::from(*percent) / 100.0);
        }
        ConfigSetting::SidebarStartCollapsed(collapsed) => {
            document["ui"]["sidebar_start_collapsed"] = toml_edit::value(*collapsed);
        }
        ConfigSetting::SidebarCollapsedMode(mode) => {
            document["ui"]["sidebar_collapsed_mode"] = toml_edit::value(match mode {
                SidebarCollapsedMode::Compact => "compact",
                SidebarCollapsedMode::Hidden => "hidden",
            });
        }
        ConfigSetting::ConfirmClose(enabled) => {
            document["ui"]["confirm_close"] = toml_edit::value(*enabled);
        }
        ConfigSetting::PromptNewTabName(enabled) => {
            document["ui"]["prompt_new_tab_name"] = toml_edit::value(*enabled);
        }
        ConfigSetting::UpdateChannel(channel) => {
            document["update"]["channel"] = toml_edit::value(match channel {
                UpdateChannel::Stable => "stable",
                UpdateChannel::Preview => "preview",
            });
        }
    }

    if let Some(parent) = path.parent().filter(|parent| !parent.as_os_str().is_empty()) {
        fs::create_dir_all(parent).map_err(|source| ConfigFileError::Write {
            path: parent.to_owned(),
            source,
        })?;
    }
    fs::write(path, document.to_string()).map_err(|source| ConfigFileError::Write {
        path: path.to_owned(),
        source,
    })
}

pub fn reset_keys(path: impl AsRef<Path>) -> Result<(), ConfigFileError> {
    let path = path.as_ref();
    let bytes = fs::read(path).map_err(|source| ConfigFileError::Read {
        path: path.to_owned(),
        source,
    })?;
    let source = String::from_utf8(bytes).map_err(|source| ConfigFileError::Utf8 {
        path: path.to_owned(),
        source,
    })?;
    let mut document = DocumentMut::from_str(&source).map_err(|source| ConfigFileError::EditParse {
        path: path.to_owned(),
        source,
    })?;

    let backup = backup_path(path);
    fs::copy(path, &backup).map_err(|source| ConfigFileError::Backup {
        path: backup,
        source,
    })?;

    if document.as_table_mut().remove("keys").is_some() {
        fs::write(path, document.to_string()).map_err(|source| ConfigFileError::Write {
            path: path.to_owned(),
            source,
        })?;
    }
    Ok(())
}

/// Persist the first-run notification choice while preserving the rest of an
/// existing config. A missing file starts from the commented default template.
pub fn save_onboarding_choice(
    path: impl AsRef<Path>,
    delivery: ToastDelivery,
) -> Result<(), ConfigFileError> {
    let path = path.as_ref();
    let source = match fs::read(path) {
        Ok(bytes) => String::from_utf8(bytes).map_err(|source| ConfigFileError::Utf8 {
            path: path.to_owned(),
            source,
        })?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            default_config_template().to_owned()
        }
        Err(source) => {
            return Err(ConfigFileError::Read {
                path: path.to_owned(),
                source,
            });
        }
    };
    let mut document = DocumentMut::from_str(&source).map_err(|source| ConfigFileError::EditParse {
        path: path.to_owned(),
        source,
    })?;
    document["onboarding"] = toml_edit::value(false);
    document["ui"]["toast"]["delivery"] = toml_edit::value(match delivery {
        ToastDelivery::Off => "off",
        ToastDelivery::Starcil => "starcil",
        ToastDelivery::Terminal => "terminal",
        ToastDelivery::System => "system",
    });

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|source| ConfigFileError::Write {
            path: parent.to_owned(),
            source,
        })?;
    }
    fs::write(path, document.to_string()).map_err(|source| ConfigFileError::Write {
        path: path.to_owned(),
        source,
    })
}

pub fn backup_path(path: &Path) -> PathBuf {
    let mut name = OsString::from(path.as_os_str());
    name.push(".bak");
    PathBuf::from(name)
}

/// `ui.sidebar_section_split` is a 0.0–1.0 ratio on disk and a whole percent
/// in memory.
mod ratio_percent {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(percent: &u8, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_f64(f64::from(*percent) / 100.0)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<u8, D::Error> {
        let ratio = f64::deserialize(deserializer)?;
        if !(0.0..=1.0).contains(&ratio) {
            return Err(serde::de::Error::custom(
                "sidebar_section_split must be between 0.0 and 1.0",
            ));
        }
        Ok((ratio * 100.0).round() as u8)
    }
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    #[test]
    fn template_parses_and_matches_defaults() {
        let report = parse_config(default_config_template());
        assert!(report.diagnostics.is_empty(), "{:#?}", report.diagnostics);
        assert_eq!(report.config, Config::default());
        assert!(default_config_template().contains("%APPDATA%\\starcil\\config.toml"));
        assert!(default_config_template().contains("# starcil = show in-app toasts"));
        assert_eq!(report.config.onboarding, None);
        assert_eq!(report.config.theme.dark_name, None);
        assert_eq!(report.config.theme.light_name, None);

        let template_paths = paths_in_commented_template(default_config_template());
        for path in CONFIG_REFERENCE_KEYS {
            assert!(template_paths.contains(*path), "template is missing {path}");
        }
    }

    #[test]
    fn model_checklist_covers_every_documented_field() {
        let config = Config::default();
        assert_eq!(CONFIG_REFERENCE_KEYS.len(), 155);
        assert!(config.should_show_onboarding());
        let _top_level = (
            config.onboarding,
            &config.theme,
            &config.terminal,
            &config.update,
            &config.keys,
            &config.worktrees,
            &config.ui,
            &config.session,
            &config.remote,
            &config.experimental,
            &config.advanced,
        );
        let _theme = (
            &config.theme.name,
            config.theme.auto_switch,
            &config.theme.dark_name,
            &config.theme.light_name,
            &config.theme.custom,
        );
        let _terminal = (
            &config.terminal.default_shell,
            config.terminal.shell_mode,
            &config.terminal.new_cwd,
        );
        let _update = (
            config.update.channel,
            config.update.version_check,
            config.update.manifest_check,
        );
        let _ui = (
            config.ui.sidebar_width,
            config.ui.sidebar_min_width,
            config.ui.sidebar_max_width,
            config.ui.sidebar_start_collapsed,
            config.ui.sidebar_collapsed_mode,
            config.ui.mobile_width_threshold,
            config.ui.mouse_capture,
            config.ui.copy_on_select,
            config.ui.host_cursor,
            &config.ui.right_click_passthrough_modifier,
            config.ui.redraw_on_focus_gained,
            config.ui.mouse_scroll_lines,
            config.ui.confirm_close,
            config.ui.prompt_new_tab_name,
            config.ui.prompt_new_workspace_name,
            config.ui.pane_borders,
            config.ui.pane_scrollbars,
            config.ui.pane_gaps,
            config.ui.show_agent_labels_on_pane_borders,
            config.ui.hide_tab_bar_when_single_tab,
            config.ui.agent_panel_sort,
            &config.ui.sidebar.agents,
            &config.ui.sidebar.spaces,
            &config.ui.accent,
            &config.ui.toast,
            &config.ui.sound,
        );
        let _sound_agents = (
            config.ui.sound.agents.pi,
            config.ui.sound.agents.claude,
            config.ui.sound.agents.codex,
            config.ui.sound.agents.gemini,
            config.ui.sound.agents.cursor,
            config.ui.sound.agents.devin,
            config.ui.sound.agents.agy,
            config.ui.sound.agents.cline,
            config.ui.sound.agents.open_code,
            config.ui.sound.agents.github_copilot,
            config.ui.sound.agents.kimi,
            config.ui.sound.agents.kiro,
            config.ui.sound.agents.droid,
            config.ui.sound.agents.amp,
            config.ui.sound.agents.grok,
            config.ui.sound.agents.hermes,
            config.ui.sound.agents.kilo,
            config.ui.sound.agents.qodercli,
            config.ui.sound.agents.maki,
        );
        let _tail = (
            &config.worktrees.directory,
            config.session.resume_agents_on_restore,
            config.remote.manage_ssh_config,
            config.experimental.allow_nested,
            config.experimental.kitty_graphics,
            config.experimental.pane_history,
            config.experimental.switch_ascii_input_source_in_prefix,
            config.experimental.reveal_hidden_cursor_for_cjk_ime,
            &config.experimental.cjk_ime_agents,
            config.experimental.cjk_ime_cursor_shape,
            config.advanced.scrollback_limit_bytes,
        );
        assert_eq!(config.advanced.scrollback_limit_bytes, 10_000_000);
    }

    #[test]
    fn unknown_keys_warn_and_type_errors_include_a_toml_path() {
        let unknown = parse_config("[ui]\nsidebar_wdth = 20\n");
        assert!(unknown.is_valid());
        assert!(unknown.diagnostics.iter().any(|diagnostic| {
            diagnostic.severity == Severity::Warning && diagnostic.path == "ui.sidebar_wdth"
        }));

        let wrong_type = parse_config("[ui]\nsidebar_width = \"wide\"\n");
        assert!(!wrong_type.is_valid());
        assert!(wrong_type.diagnostics.iter().any(|diagnostic| {
            diagnostic.severity == Severity::Error && diagnostic.path == "ui.sidebar_width"
        }), "{:#?}", wrong_type.diagnostics);

        let unknown_delivery = parse_config("[ui.toast]\ndelivery = \"bogus\"\n");
        assert!(!unknown_delivery.is_valid());
    }

    #[test]
    fn expanded_defaults_aliases_and_limits_are_enforced() {
        let parsed = parse_config(concat!(
            "onboarding = false\n",
            "[ui]\n",
            "right_click_passthrough_modifier = \"ctrl+alt+hyper\"\n",
            "[ui.toast]\n",
            "delay_seconds = 0\n",
            "[ui.sound.agents]\n",
            "maki = \"on\"\n",
            "[experimental]\n",
            "cjk_ime_agents = [\"CLAUDE\", \"maki\"]\n",
            "[advanced]\n",
            "scrollback_lines = 1234\n",
        ));
        assert!(parsed.is_valid(), "{:#?}", parsed.diagnostics);
        assert!(!parsed.config.should_show_onboarding());
        assert_eq!(parsed.config.ui.sound.agents.maki, SoundPolicy::On);
        assert_eq!(parsed.config.advanced.scrollback_limit_bytes, 1234);

        let mut invalid = Config::default();
        invalid.ui.right_click_passthrough_modifier = "ctrl+shift".to_owned();
        invalid.ui.toast.delay_seconds = 3601;
        let diagnostics = validate_config(&invalid);
        assert!(diagnostics.iter().any(|item| item.path == "ui.right_click_passthrough_modifier"));
        assert!(diagnostics.iter().any(|item| item.path == "ui.toast.delay_seconds"));
    }

    #[test]
    fn reset_keys_keeps_other_content_and_creates_exact_backup() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory = std::env::temp_dir().join(format!("starcil-config-{}-{unique}", std::process::id()));
        fs::create_dir_all(&directory).unwrap();
        let path = directory.join("config.toml");
        let source = concat!(
            "# user header\n",
            "onboarding = false\n\n",
            "[keys]\n",
            "help = \"prefix+h\"\n\n",
            "[[keys.command]]\n",
            "key = \"prefix+t\"\n",
            "type = \"popup\"\n",
            "command = \"powershell\"\n\n",
            "# preserve this UI comment\n",
            "[ui]\n",
            "sidebar_width = 30\n",
        );
        fs::write(&path, source).unwrap();

        reset_keys(&path).unwrap();
        assert_eq!(fs::read_to_string(backup_path(&path)).unwrap(), source);
        let result = fs::read_to_string(&path).unwrap();
        assert_eq!(
            result,
            concat!(
                "# user header\n",
                "onboarding = false\n\n",
                "# preserve this UI comment\n",
                "[ui]\n",
                "sidebar_width = 30\n",
            )
        );
        assert!(!result.contains("[keys]"));
        assert!(!result.contains("[[keys.command]]"));

        fs::remove_dir_all(directory).unwrap();
    }

    fn paths_in_commented_template(source: &str) -> BTreeSet<String> {
        let mut paths = BTreeSet::new();
        let mut section = String::new();
        for line in source.lines() {
            let mut line = line.trim();
            if let Some(comment) = line.strip_prefix('#') {
                line = comment.trim();
            }
            if line.starts_with('[') && line.ends_with(']') {
                section = line.trim_matches('[').trim_matches(']').trim().to_owned();
                paths.insert(section.clone());
                continue;
            }
            let Some((key, _)) = line.split_once('=') else {
                continue;
            };
            let key = key.trim();
            if key.is_empty() || key.contains(' ') {
                continue;
            }
            paths.insert(if section.is_empty() {
                key.to_owned()
            } else {
                format!("{section}.{key}")
            });
        }
        paths
    }
}
