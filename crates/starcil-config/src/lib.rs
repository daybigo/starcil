//! Typed configuration, keymaps, and built-in themes for Starcil.

mod config;
mod keys;
mod theme;

pub use config::{
    Advanced, AgentPanelSort, AgentRows, CjkImeCursorShape, ClipboardToast, Config, ConfigSetting,
    ConfigFileError, ConfigReport, Diagnostic, Experimental, HostCursor, InAppToast,
    InAppToastPosition, NewCwd,
    Remote, RowToken, Session, Severity, ShellMode, Sidebar, SidebarCollapsedMode, Sound,
    SoundAgents, SoundPolicy, SpaceRows, StyledToken, TabBarPosition, Terminal, Toast, ToastDelivery,
    ToastPosition, Ui, Update, UpdateChannel, Worktrees, backup_path, check, config_path,
    default_config_path, default_config_template, load, parse_config, reset_keys, validate_config,
    save_config_setting, save_onboarding_choice, CONFIG_REFERENCE_KEYS,
};
pub use keys::{
    Action, BoundAction, CommandDimension, CommandType, EffectiveKeymap, Key, KeyChord,
    KeyCommand, KeyContext, KeyParseError, KeyBindings, KeymapBuild, Keys, KeysIndexed,
    MetaModifier, Modifiers, NamedKey, build_effective_keymap, validate_navigate_chord,
};
pub use theme::{
    BUILTIN_THEME_NAMES, Color, HostAppearance, NamedColor, ResolvedTheme, Theme, ThemeError,
    ThemeTokens, TOKEN_NAMES, builtin_theme, resolve_theme,
};
