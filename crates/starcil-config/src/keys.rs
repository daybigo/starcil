use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;

use serde::de::{self, Visitor};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::config::Diagnostic;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct KeyBindings(pub Vec<String>);

impl KeyBindings {
    pub fn one(binding: impl Into<String>) -> Self {
        Self(vec![binding.into()])
    }

    pub fn is_empty(&self) -> bool {
        self.0.iter().all(|binding| binding.trim().is_empty())
    }

    pub fn iter(&self) -> impl Iterator<Item = &str> {
        self.0
            .iter()
            .map(String::as_str)
            .filter(|binding| !binding.trim().is_empty())
    }

    pub fn as_slice(&self) -> &[String] {
        &self.0
    }
}

impl From<&str> for KeyBindings {
    fn from(value: &str) -> Self {
        if value.is_empty() {
            Self::default()
        } else {
            Self::one(value)
        }
    }
}

impl Serialize for KeyBindings {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self.0.as_slice() {
            [] => serializer.serialize_str(""),
            [binding] => serializer.serialize_str(binding),
            bindings => bindings.serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for KeyBindings {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum OneOrMany {
            One(String),
            Many(Vec<String>),
        }

        Ok(match OneOrMany::deserialize(deserializer)? {
            OneOrMany::One(value) if value.is_empty() => Self::default(),
            OneOrMany::One(value) => Self::one(value),
            OneOrMany::Many(values) => Self(values),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Keys {
    pub prefix: String,
    pub help: KeyBindings,
    pub settings: KeyBindings,
    pub detach: KeyBindings,
    pub reload_config: KeyBindings,
    pub open_notification_target: KeyBindings,
    pub workspace_picker: KeyBindings,
    pub goto: KeyBindings,
    pub new_workspace: KeyBindings,
    pub new_worktree: KeyBindings,
    pub open_worktree: KeyBindings,
    pub remove_worktree: KeyBindings,
    pub rename_workspace: KeyBindings,
    pub close_workspace: KeyBindings,
    pub previous_workspace: KeyBindings,
    pub next_workspace: KeyBindings,
    pub previous_agent: KeyBindings,
    pub next_agent: KeyBindings,
    pub focus_agent: KeyBindings,
    pub remote_image_paste: KeyBindings,
    pub new_tab: KeyBindings,
    pub rename_tab: KeyBindings,
    pub previous_tab: KeyBindings,
    pub next_tab: KeyBindings,
    pub switch_tab: KeyBindings,
    pub switch_workspace: KeyBindings,
    pub close_tab: KeyBindings,
    pub rename_pane: KeyBindings,
    pub edit_scrollback: KeyBindings,
    pub copy_mode: KeyBindings,
    pub focus_pane_left: KeyBindings,
    pub focus_pane_down: KeyBindings,
    pub focus_pane_up: KeyBindings,
    pub focus_pane_right: KeyBindings,
    pub swap_pane_left: KeyBindings,
    pub swap_pane_down: KeyBindings,
    pub swap_pane_up: KeyBindings,
    pub swap_pane_right: KeyBindings,
    pub cycle_pane_next: KeyBindings,
    pub cycle_pane_previous: KeyBindings,
    pub last_pane: KeyBindings,
    pub split_vertical: KeyBindings,
    pub split_horizontal: KeyBindings,
    pub close_pane: KeyBindings,
    #[serde(alias = "fullscreen")]
    pub zoom: KeyBindings,
    pub resize_mode: KeyBindings,
    pub toggle_sidebar: KeyBindings,
    pub navigate_workspace_up: KeyBindings,
    pub navigate_workspace_down: KeyBindings,
    pub navigate_pane_left: KeyBindings,
    pub navigate_pane_down: KeyBindings,
    pub navigate_pane_up: KeyBindings,
    pub navigate_pane_right: KeyBindings,
    pub focus_input: KeyBindings,
    pub open_folder: KeyBindings,
    pub dock_agent: KeyBindings,
    #[serde(rename = "command")]
    pub commands: Vec<KeyCommand>,
    pub indexed: KeysIndexed,
}

impl Default for Keys {
    fn default() -> Self {
        Self {
            prefix: "ctrl+b".to_owned(),
            help: "prefix+?".into(),
            settings: "prefix+s".into(),
            detach: "prefix+q".into(),
            reload_config: "prefix+shift+r".into(),
            open_notification_target: "prefix+o".into(),
            workspace_picker: "prefix+w".into(),
            goto: "prefix+g".into(),
            new_workspace: "prefix+shift+n".into(),
            new_worktree: "prefix+shift+g".into(),
            open_worktree: KeyBindings::default(),
            remove_worktree: KeyBindings::default(),
            rename_workspace: "prefix+shift+w".into(),
            close_workspace: "prefix+shift+d".into(),
            previous_workspace: KeyBindings::default(),
            next_workspace: KeyBindings::default(),
            previous_agent: KeyBindings::default(),
            next_agent: KeyBindings::default(),
            focus_agent: KeyBindings::default(),
            remote_image_paste: "ctrl+v".into(),
            new_tab: "prefix+c".into(),
            rename_tab: "prefix+shift+t".into(),
            previous_tab: "prefix+p".into(),
            next_tab: "prefix+n".into(),
            switch_tab: "prefix+1..9".into(),
            switch_workspace: KeyBindings::default(),
            close_tab: "prefix+shift+x".into(),
            rename_pane: "prefix+shift+p".into(),
            edit_scrollback: "prefix+e".into(),
            copy_mode: "prefix+[".into(),
            focus_pane_left: "prefix+h".into(),
            focus_pane_down: "prefix+j".into(),
            focus_pane_up: "prefix+k".into(),
            focus_pane_right: "prefix+l".into(),
            swap_pane_left: "prefix+shift+h".into(),
            swap_pane_down: "prefix+shift+j".into(),
            swap_pane_up: "prefix+shift+k".into(),
            swap_pane_right: "prefix+shift+l".into(),
            cycle_pane_next: "prefix+tab".into(),
            cycle_pane_previous: "prefix+shift+tab".into(),
            last_pane: KeyBindings::default(),
            split_vertical: "prefix+v".into(),
            split_horizontal: "prefix+minus".into(),
            close_pane: "prefix+x".into(),
            zoom: "prefix+z".into(),
            resize_mode: "prefix+r".into(),
            toggle_sidebar: "prefix+b".into(),
            navigate_workspace_up: "up".into(),
            navigate_workspace_down: "down".into(),
            navigate_pane_left: "h".into(),
            navigate_pane_down: "j".into(),
            navigate_pane_up: "k".into(),
            navigate_pane_right: "l".into(),
            focus_input: "ctrl+space".into(),
            open_folder: "prefix+f".into(),
            dock_agent: "alt+1..9".into(),
            commands: Vec::new(),
            indexed: KeysIndexed::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct KeysIndexed {
    pub tabs: String,
    pub workspaces: String,
    pub agents: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum CommandType {
    #[default]
    Shell,
    Pane,
    Popup,
    /// Documented compatibility surface for installed plugin actions.
    PluginAction,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandDimension {
    Cells(u16),
    Percent(u8),
}

impl Serialize for CommandDimension {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self {
            Self::Cells(cells) => serializer.serialize_u16(*cells),
            Self::Percent(percent) => serializer.serialize_str(&format!("{percent}%")),
        }
    }
}

impl<'de> Deserialize<'de> for CommandDimension {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct DimensionVisitor;

        impl<'de> Visitor<'de> for DimensionVisitor {
            type Value = CommandDimension;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a positive cell count or percentage such as \"80%\"")
            }

            fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                let value = u16::try_from(value)
                    .map_err(|_| E::custom("cell count must fit in 16 bits"))?;
                if value == 0 {
                    return Err(E::custom("cell count must be greater than zero"));
                }
                Ok(CommandDimension::Cells(value))
            }

            fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                let value = u64::try_from(value)
                    .map_err(|_| E::custom("cell count must be greater than zero"))?;
                self.visit_u64(value)
            }

            fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                let Some(number) = value.trim().strip_suffix('%') else {
                    return Err(E::custom("string dimensions must end in `%`"));
                };
                let percent = number
                    .parse::<u8>()
                    .map_err(|_| E::custom("percentage must be an integer from 1 to 100"))?;
                if !(1..=100).contains(&percent) {
                    return Err(E::custom("percentage must be from 1 to 100"));
                }
                Ok(CommandDimension::Percent(percent))
            }
        }

        deserializer.deserialize_any(DimensionVisitor)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct KeyCommand {
    pub key: KeyBindings,
    #[serde(rename = "type")]
    pub command_type: CommandType,
    pub command: String,
    pub description: Option<String>,
    pub width: Option<CommandDimension>,
    pub height: Option<CommandDimension>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum MetaModifier {
    Cmd,
    Super,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Modifiers {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub meta: Option<MetaModifier>,
}

impl Modifiers {
    pub const fn is_empty(self) -> bool {
        !self.ctrl && !self.alt && !self.shift && self.meta.is_none()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum NamedKey {
    Enter,
    Tab,
    Esc,
    Left,
    Right,
    Up,
    Down,
    Backspace,
    Delete,
    Home,
    End,
    PageUp,
    PageDown,
    Space,
    Minus,
    Comma,
    Ampersand,
    Plus,
    Backtick,
}

impl fmt::Display for NamedKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Enter => "enter",
            Self::Tab => "tab",
            Self::Esc => "esc",
            Self::Left => "left",
            Self::Right => "right",
            Self::Up => "up",
            Self::Down => "down",
            Self::Backspace => "backspace",
            Self::Delete => "delete",
            Self::Home => "home",
            Self::End => "end",
            Self::PageUp => "pageup",
            Self::PageDown => "pagedown",
            Self::Space => "space",
            Self::Minus => "minus",
            Self::Comma => "comma",
            Self::Ampersand => "ampersand",
            Self::Plus => "plus",
            Self::Backtick => "backtick",
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Key {
    Character(char),
    Function(u8),
    Named(NamedKey),
    DigitRange1To9,
}

impl fmt::Display for Key {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Character(character) => character.fmt(f),
            Self::Function(number) => write!(f, "f{number}"),
            Self::Named(key) => key.fmt(f),
            Self::DigitRange1To9 => f.write_str("1..9"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct KeyChord {
    pub mods: Modifiers,
    pub key: Key,
    pub requires_prefix: bool,
}

impl KeyChord {
    fn with_key(self, key: Key) -> Self {
        Self { key, ..self }
    }
}

impl fmt::Display for KeyChord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut parts = Vec::new();
        if self.requires_prefix {
            parts.push("prefix".to_owned());
        }
        if self.mods.ctrl {
            parts.push("ctrl".to_owned());
        }
        if self.mods.alt {
            parts.push("alt".to_owned());
        }
        if self.mods.shift {
            parts.push("shift".to_owned());
        }
        if let Some(meta) = self.mods.meta {
            parts.push(match meta {
                MetaModifier::Cmd => "cmd".to_owned(),
                MetaModifier::Super => "super".to_owned(),
            });
        }
        parts.push(self.key.to_string());
        f.write_str(&parts.join("+"))
    }
}

impl FromStr for KeyChord {
    type Err = KeyParseError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        let normalized = input.trim().to_ascii_lowercase();
        if normalized.is_empty() {
            return Err(KeyParseError::new(input, "binding is empty"));
        }

        let tokens = if normalized == "+" {
            vec!["plus".to_owned()]
        } else if normalized.ends_with("++") {
            let head = &normalized[..normalized.len() - 1];
            let mut values = head
                .split('+')
                .filter(|part| !part.is_empty())
                .map(str::to_owned)
                .collect::<Vec<_>>();
            values.push("plus".to_owned());
            values
        } else {
            let values = normalized.split('+').map(str::to_owned).collect::<Vec<_>>();
            if values.iter().any(String::is_empty) {
                return Err(KeyParseError::new(
                    input,
                    "empty component; use the named key `plus` for +",
                ));
            }
            values
        };

        let mut requires_prefix = false;
        let mut mods = Modifiers::default();
        let mut key = None;

        for token in tokens {
            match token.as_str() {
                "prefix" => {
                    if requires_prefix {
                        return Err(KeyParseError::new(input, "`prefix` appears more than once"));
                    }
                    requires_prefix = true;
                }
                "ctrl" | "control" => set_modifier(input, &mut mods.ctrl, "ctrl")?,
                "alt" | "option" => set_modifier(input, &mut mods.alt, "alt")?,
                "shift" => set_modifier(input, &mut mods.shift, "shift")?,
                "cmd" | "command" => {
                    if mods.meta.is_some() {
                        return Err(KeyParseError::new(input, "cmd/super modifier appears more than once"));
                    }
                    mods.meta = Some(MetaModifier::Cmd);
                }
                "super" | "meta" | "win" => {
                    if mods.meta.is_some() {
                        return Err(KeyParseError::new(input, "cmd/super modifier appears more than once"));
                    }
                    mods.meta = Some(MetaModifier::Super);
                }
                _ => {
                    let parsed = parse_key(&token).ok_or_else(|| {
                        KeyParseError::new(
                            input,
                            format!(
                                "unknown key `{token}`; use a character, f1..f24, enter/tab/esc/arrows, or a named punctuation key"
                            ),
                        )
                    })?;
                    if key.replace(parsed).is_some() {
                        return Err(KeyParseError::new(input, "binding contains more than one key"));
                    }
                }
            }
        }

        let key = key.ok_or_else(|| KeyParseError::new(input, "binding has modifiers but no key"))?;
        Ok(Self {
            mods,
            key,
            requires_prefix,
        })
    }
}

fn set_modifier(input: &str, slot: &mut bool, name: &str) -> Result<(), KeyParseError> {
    if *slot {
        return Err(KeyParseError::new(
            input,
            format!("`{name}` modifier appears more than once"),
        ));
    }
    *slot = true;
    Ok(())
}

fn parse_key(token: &str) -> Option<Key> {
    let named = match token {
        "enter" | "return" => Some(NamedKey::Enter),
        "tab" => Some(NamedKey::Tab),
        "esc" | "escape" => Some(NamedKey::Esc),
        "left" => Some(NamedKey::Left),
        "right" => Some(NamedKey::Right),
        "up" => Some(NamedKey::Up),
        "down" => Some(NamedKey::Down),
        "backspace" => Some(NamedKey::Backspace),
        "delete" | "del" => Some(NamedKey::Delete),
        "home" => Some(NamedKey::Home),
        "end" => Some(NamedKey::End),
        "pageup" | "page-up" => Some(NamedKey::PageUp),
        "pagedown" | "page-down" => Some(NamedKey::PageDown),
        "space" => Some(NamedKey::Space),
        "minus" | "-" => Some(NamedKey::Minus),
        "comma" | "," => Some(NamedKey::Comma),
        "ampersand" | "&" => Some(NamedKey::Ampersand),
        "plus" => Some(NamedKey::Plus),
        "backtick" | "`" => Some(NamedKey::Backtick),
        _ => None,
    };
    if let Some(named) = named {
        return Some(Key::Named(named));
    }
    if token == "1..9" {
        return Some(Key::DigitRange1To9);
    }
    if let Some(number) = token.strip_prefix('f').and_then(|value| value.parse::<u8>().ok()) {
        if (1..=24).contains(&number) {
            return Some(Key::Function(number));
        }
    }
    let mut characters = token.chars();
    let character = characters.next()?;
    if characters.next().is_none() && !character.is_control() && !character.is_whitespace() {
        return Some(Key::Character(character));
    }
    None
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("invalid key binding `{input}`: {reason}")]
pub struct KeyParseError {
    pub input: String,
    pub reason: String,
}

impl KeyParseError {
    fn new(input: impl Into<String>, reason: impl Into<String>) -> Self {
        Self {
            input: input.into(),
            reason: reason.into(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum KeyContext {
    Terminal,
    Navigate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Action {
    Help,
    Settings,
    Detach,
    ReloadConfig,
    OpenNotificationTarget,
    WorkspacePicker,
    Goto,
    NewWorkspace,
    NewWorktree,
    OpenWorktree,
    RemoveWorktree,
    RenameWorkspace,
    CloseWorkspace,
    PreviousWorkspace,
    NextWorkspace,
    PreviousAgent,
    NextAgent,
    FocusAgent,
    RemoteImagePaste,
    NewTab,
    RenameTab,
    PreviousTab,
    NextTab,
    SwitchTab,
    SwitchWorkspace,
    CloseTab,
    RenamePane,
    EditScrollback,
    CopyMode,
    FocusPaneLeft,
    FocusPaneDown,
    FocusPaneUp,
    FocusPaneRight,
    SwapPaneLeft,
    SwapPaneDown,
    SwapPaneUp,
    SwapPaneRight,
    CyclePaneNext,
    CyclePanePrevious,
    LastPane,
    SplitVertical,
    SplitHorizontal,
    ClosePane,
    Zoom,
    ResizeMode,
    ToggleSidebar,
    NavigateWorkspaceUp,
    NavigateWorkspaceDown,
    NavigatePaneLeft,
    NavigatePaneDown,
    NavigatePaneUp,
    NavigatePaneRight,
    /// Toggle keyboard focus between the bottom command input and the pane.
    FocusInput,
    /// Open the native OS folder picker (visual cd).
    OpenFolder,
    /// Launch dock agent N in a new pane (indexed 1..9).
    DockAgent,
    CustomCommand(usize),
}

impl Action {
    pub fn name(self) -> &'static str {
        match self {
            Self::Help => "help",
            Self::Settings => "settings",
            Self::Detach => "detach",
            Self::ReloadConfig => "reload_config",
            Self::OpenNotificationTarget => "open_notification_target",
            Self::WorkspacePicker => "workspace_picker",
            Self::Goto => "goto",
            Self::NewWorkspace => "new_workspace",
            Self::NewWorktree => "new_worktree",
            Self::OpenWorktree => "open_worktree",
            Self::RemoveWorktree => "remove_worktree",
            Self::RenameWorkspace => "rename_workspace",
            Self::CloseWorkspace => "close_workspace",
            Self::PreviousWorkspace => "previous_workspace",
            Self::NextWorkspace => "next_workspace",
            Self::PreviousAgent => "previous_agent",
            Self::NextAgent => "next_agent",
            Self::FocusAgent => "focus_agent",
            Self::RemoteImagePaste => "remote_image_paste",
            Self::NewTab => "new_tab",
            Self::RenameTab => "rename_tab",
            Self::PreviousTab => "previous_tab",
            Self::NextTab => "next_tab",
            Self::SwitchTab => "switch_tab",
            Self::FocusInput => "focus_input",
            Self::OpenFolder => "open_folder",
            Self::DockAgent => "dock_agent",
            Self::SwitchWorkspace => "switch_workspace",
            Self::CloseTab => "close_tab",
            Self::RenamePane => "rename_pane",
            Self::EditScrollback => "edit_scrollback",
            Self::CopyMode => "copy_mode",
            Self::FocusPaneLeft => "focus_pane_left",
            Self::FocusPaneDown => "focus_pane_down",
            Self::FocusPaneUp => "focus_pane_up",
            Self::FocusPaneRight => "focus_pane_right",
            Self::SwapPaneLeft => "swap_pane_left",
            Self::SwapPaneDown => "swap_pane_down",
            Self::SwapPaneUp => "swap_pane_up",
            Self::SwapPaneRight => "swap_pane_right",
            Self::CyclePaneNext => "cycle_pane_next",
            Self::CyclePanePrevious => "cycle_pane_previous",
            Self::LastPane => "last_pane",
            Self::SplitVertical => "split_vertical",
            Self::SplitHorizontal => "split_horizontal",
            Self::ClosePane => "close_pane",
            Self::Zoom => "zoom",
            Self::ResizeMode => "resize_mode",
            Self::ToggleSidebar => "toggle_sidebar",
            Self::NavigateWorkspaceUp => "navigate_workspace_up",
            Self::NavigateWorkspaceDown => "navigate_workspace_down",
            Self::NavigatePaneLeft => "navigate_pane_left",
            Self::NavigatePaneDown => "navigate_pane_down",
            Self::NavigatePaneUp => "navigate_pane_up",
            Self::NavigatePaneRight => "navigate_pane_right",
            Self::CustomCommand(_) => "command",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BoundAction {
    pub action: Action,
    /// One-based target for indexed actions.
    pub index: Option<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectiveKeymap {
    pub prefix: KeyChord,
    pub terminal: BTreeMap<KeyChord, BoundAction>,
    pub navigate: BTreeMap<KeyChord, BoundAction>,
    pub commands: Vec<KeyCommand>,
}

impl EffectiveKeymap {
    pub fn binding(&self, context: KeyContext, chord: &KeyChord) -> Option<&BoundAction> {
        match context {
            KeyContext::Terminal => self.terminal.get(chord),
            KeyContext::Navigate => self.navigate.get(chord).or_else(|| self.terminal.get(chord)),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeymapBuild {
    pub keymap: EffectiveKeymap,
    pub diagnostics: Vec<Diagnostic>,
}

pub fn build_effective_keymap(keys: &Keys) -> KeymapBuild {
    let mut diagnostics = Vec::new();
    let prefix = match keys.prefix.parse::<KeyChord>() {
        Ok(chord) if !chord.requires_prefix && chord.key != Key::DigitRange1To9 => chord,
        Ok(_) => {
            diagnostics.push(Diagnostic::error(
                "keys.prefix",
                "prefix must be one direct key chord and cannot contain `prefix+` or `1..9`",
            ));
            "ctrl+b".parse().expect("built-in prefix is valid")
        }
        Err(error) => {
            diagnostics.push(Diagnostic::error("keys.prefix", error.to_string()));
            "ctrl+b".parse().expect("built-in prefix is valid")
        }
    };

    let mut builder = Builder {
        prefix,
        terminal: BTreeMap::new(),
        navigate: BTreeMap::new(),
        terminal_origins: BTreeMap::new(),
        navigate_origins: BTreeMap::new(),
        diagnostics,
    };

    macro_rules! bind {
        ($field:ident, $action:ident) => {
            builder.bind_many(
                &keys.$field,
                Action::$action,
                concat!("keys.", stringify!($field)),
                KeyContext::Terminal,
                false,
            );
        };
    }

    bind!(help, Help);
    bind!(settings, Settings);
    bind!(detach, Detach);
    bind!(reload_config, ReloadConfig);
    bind!(open_notification_target, OpenNotificationTarget);
    bind!(workspace_picker, WorkspacePicker);
    bind!(goto, Goto);
    bind!(new_workspace, NewWorkspace);
    bind!(new_worktree, NewWorktree);
    bind!(open_worktree, OpenWorktree);
    bind!(remove_worktree, RemoveWorktree);
    bind!(rename_workspace, RenameWorkspace);
    bind!(close_workspace, CloseWorkspace);
    bind!(previous_workspace, PreviousWorkspace);
    bind!(next_workspace, NextWorkspace);
    bind!(previous_agent, PreviousAgent);
    bind!(next_agent, NextAgent);
    builder.bind_many(&keys.focus_agent, Action::FocusAgent, "keys.focus_agent", KeyContext::Terminal, true);
    bind!(remote_image_paste, RemoteImagePaste);
    bind!(new_tab, NewTab);
    bind!(rename_tab, RenameTab);
    bind!(previous_tab, PreviousTab);
    bind!(next_tab, NextTab);
    builder.bind_many(&keys.switch_tab, Action::SwitchTab, "keys.switch_tab", KeyContext::Terminal, true);
    builder.bind_many(&keys.focus_input, Action::FocusInput, "keys.focus_input", KeyContext::Terminal, false);
    builder.bind_many(&keys.open_folder, Action::OpenFolder, "keys.open_folder", KeyContext::Terminal, false);
    builder.bind_many(&keys.dock_agent, Action::DockAgent, "keys.dock_agent", KeyContext::Terminal, true);
    builder.bind_many(
        &keys.switch_workspace,
        Action::SwitchWorkspace,
        "keys.switch_workspace",
        KeyContext::Terminal,
        true,
    );
    bind!(close_tab, CloseTab);
    bind!(rename_pane, RenamePane);
    bind!(edit_scrollback, EditScrollback);
    bind!(copy_mode, CopyMode);
    bind!(focus_pane_left, FocusPaneLeft);
    bind!(focus_pane_down, FocusPaneDown);
    bind!(focus_pane_up, FocusPaneUp);
    bind!(focus_pane_right, FocusPaneRight);
    bind!(swap_pane_left, SwapPaneLeft);
    bind!(swap_pane_down, SwapPaneDown);
    bind!(swap_pane_up, SwapPaneUp);
    bind!(swap_pane_right, SwapPaneRight);
    bind!(cycle_pane_next, CyclePaneNext);
    bind!(cycle_pane_previous, CyclePanePrevious);
    bind!(last_pane, LastPane);
    bind!(split_vertical, SplitVertical);
    bind!(split_horizontal, SplitHorizontal);
    bind!(close_pane, ClosePane);
    bind!(zoom, Zoom);
    bind!(resize_mode, ResizeMode);
    bind!(toggle_sidebar, ToggleSidebar);

    builder.bind_many(
        &keys.navigate_workspace_up,
        Action::NavigateWorkspaceUp,
        "keys.navigate_workspace_up",
        KeyContext::Navigate,
        false,
    );
    builder.bind_many(
        &keys.navigate_workspace_down,
        Action::NavigateWorkspaceDown,
        "keys.navigate_workspace_down",
        KeyContext::Navigate,
        false,
    );
    builder.bind_many(
        &keys.navigate_pane_left,
        Action::NavigatePaneLeft,
        "keys.navigate_pane_left",
        KeyContext::Navigate,
        false,
    );
    builder.bind_many(
        &keys.navigate_pane_down,
        Action::NavigatePaneDown,
        "keys.navigate_pane_down",
        KeyContext::Navigate,
        false,
    );
    builder.bind_many(
        &keys.navigate_pane_up,
        Action::NavigatePaneUp,
        "keys.navigate_pane_up",
        KeyContext::Navigate,
        false,
    );
    builder.bind_many(
        &keys.navigate_pane_right,
        Action::NavigatePaneRight,
        "keys.navigate_pane_right",
        KeyContext::Navigate,
        false,
    );

    if keys.switch_tab.is_empty() {
        builder.bind_legacy_indexed(&keys.indexed.tabs, Action::SwitchTab, "keys.indexed.tabs");
    }
    if keys.switch_workspace.is_empty() {
        builder.bind_legacy_indexed(
            &keys.indexed.workspaces,
            Action::SwitchWorkspace,
            "keys.indexed.workspaces",
        );
    }
    if keys.focus_agent.is_empty() {
        builder.bind_legacy_indexed(&keys.indexed.agents, Action::FocusAgent, "keys.indexed.agents");
    }

    for (index, command) in keys.commands.iter().enumerate() {
        let path = format!("keys.command[{index}]");
        if command.key.is_empty() {
            builder
                .diagnostics
                .push(Diagnostic::error(format!("{path}.key"), "custom command key is required"));
        }
        if command.command.trim().is_empty() {
            builder.diagnostics.push(Diagnostic::error(
                format!("{path}.command"),
                "custom command text or plugin action id is required",
            ));
        }
        builder.bind_many(
            &command.key,
            Action::CustomCommand(index),
            &format!("{path}.key"),
            KeyContext::Terminal,
            false,
        );
    }

    KeymapBuild {
        keymap: EffectiveKeymap {
            prefix,
            terminal: builder.terminal,
            navigate: builder.navigate,
            commands: keys.commands.clone(),
        },
        diagnostics: builder.diagnostics,
    }
}

struct Builder {
    prefix: KeyChord,
    terminal: BTreeMap<KeyChord, BoundAction>,
    navigate: BTreeMap<KeyChord, BoundAction>,
    terminal_origins: BTreeMap<KeyChord, String>,
    navigate_origins: BTreeMap<KeyChord, String>,
    diagnostics: Vec<Diagnostic>,
}

impl Builder {
    fn bind_many(
        &mut self,
        bindings: &KeyBindings,
        action: Action,
        path: &str,
        context: KeyContext,
        indexed: bool,
    ) {
        for (position, source) in bindings.iter().enumerate() {
            let item_path = if bindings.0.len() > 1 {
                format!("{path}[{position}]")
            } else {
                path.to_owned()
            };
            let chord = match source.parse::<KeyChord>() {
                Ok(chord) => chord,
                Err(error) => {
                    self.diagnostics
                        .push(Diagnostic::error(item_path, error.to_string()));
                    continue;
                }
            };

            if context == KeyContext::Navigate {
                if let Err(reason) = validate_navigate_chord(&chord) {
                    self.diagnostics.push(Diagnostic::error(item_path, reason));
                    continue;
                }
            }

            if indexed {
                self.bind_indexed(chord, action, &item_path, context);
            } else if chord.key == Key::DigitRange1To9 {
                self.diagnostics.push(Diagnostic::error(
                    item_path,
                    "`1..9` is only valid for switch_tab, switch_workspace, focus_agent, or dock_agent",
                ));
            } else {
                self.insert(chord, BoundAction { action, index: None }, &item_path, context);
            }
        }
    }

    fn bind_indexed(&mut self, chord: KeyChord, action: Action, path: &str, context: KeyContext) {
        match chord.key {
            Key::DigitRange1To9 => {
                for index in 1..=9 {
                    self.insert(
                        chord.with_key(Key::Character(char::from(b'0' + index))),
                        BoundAction {
                            action,
                            index: Some(index),
                        },
                        path,
                        context,
                    );
                }
            }
            Key::Character(character @ '1'..='9') => self.insert(
                chord,
                BoundAction {
                    action,
                    index: Some(character as u8 - b'0'),
                },
                path,
                context,
            ),
            _ => self.diagnostics.push(Diagnostic::error(
                path,
                "indexed action must use `1..9` or one digit from 1 through 9",
            )),
        }
    }

    fn bind_legacy_indexed(&mut self, modifiers: &str, action: Action, path: &str) {
        let modifiers = modifiers.trim();
        if modifiers.is_empty() {
            return;
        }
        for index in 1..=9u8 {
            let source = format!("{modifiers}+{index}");
            match source.parse::<KeyChord>() {
                Ok(chord) => self.insert(
                    chord,
                    BoundAction {
                        action,
                        index: Some(index),
                    },
                    path,
                    KeyContext::Terminal,
                ),
                Err(error) => {
                    self.diagnostics.push(Diagnostic::error(path, error.to_string()));
                    break;
                }
            }
        }
    }

    fn insert(&mut self, chord: KeyChord, action: BoundAction, path: &str, context: KeyContext) {
        if context == KeyContext::Terminal && !chord.requires_prefix && chord == self.prefix {
            self.diagnostics.push(Diagnostic::error(
                path,
                format!("binding `{chord}` conflicts with keys.prefix"),
            ));
            return;
        }

        let (map, origins) = match context {
            KeyContext::Terminal => (&mut self.terminal, &mut self.terminal_origins),
            KeyContext::Navigate => (&mut self.navigate, &mut self.navigate_origins),
        };
        if let Some(existing) = map.get(&chord) {
            if existing != &action {
                let origin = origins.get(&chord).map(String::as_str).unwrap_or("another action");
                self.diagnostics.push(Diagnostic::error(
                    path,
                    format!(
                        "binding `{chord}` for {} conflicts with {} ({origin})",
                        action.action.name(),
                        existing.action.name(),
                    ),
                ));
            }
            return;
        }
        map.insert(chord, action);
        origins.insert(chord, path.to_owned());
    }
}

pub fn validate_navigate_chord(chord: &KeyChord) -> Result<(), String> {
    if chord.requires_prefix {
        return Err("navigate-mode bindings cannot contain `prefix+`".to_owned());
    }
    match chord.key {
        Key::Named(NamedKey::Esc | NamedKey::Enter | NamedKey::Tab) => {
            Err("navigate-mode bindings cannot use esc, enter, tab, or shift+tab".to_owned())
        }
        Key::Named(NamedKey::Left | NamedKey::Right) => Err(
            "left and right arrows are reserved navigate-mode pane aliases".to_owned(),
        ),
        Key::Character('1'..='9') if chord.mods.is_empty() => {
            Err("unmodified 1 through 9 are reserved in navigate mode".to_owned())
        }
        Key::DigitRange1To9 => Err("`1..9` is reserved in navigate mode".to_owned()),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_bar_bindings_have_working_defaults() {
        let keys = Keys::default();
        let built = build_effective_keymap(&keys);
        let map = &built.keymap;
        let focus = map
            .binding(KeyContext::Terminal, &"ctrl+space".parse().unwrap())
            .expect("ctrl+space bound");
        assert_eq!(focus.action, Action::FocusInput);
        let folder = map
            .binding(KeyContext::Terminal, &"prefix+f".parse().unwrap())
            .expect("prefix+f bound");
        assert_eq!(folder.action, Action::OpenFolder);
        let dock = map
            .binding(KeyContext::Terminal, &"alt+3".parse().unwrap())
            .expect("alt+3 bound");
        assert_eq!(dock.action, Action::DockAgent);
        assert_eq!(dock.index, Some(3));
    }

    #[test]
    fn key_parser_supports_documented_surface_and_display_round_trips() {
        let bindings = [
            "ctrl+b",
            "prefix+shift+r",
            "f12",
            "minus",
            "comma",
            "ampersand",
            "plus",
            "backtick",
            "enter",
            "tab",
            "esc",
            "left",
            "right",
            "up",
            "down",
            "cmd+k",
            "super+alt+1",
            "prefix+1..9",
            "prefix+[",
            "ctrl++",
        ];
        for source in bindings {
            let parsed = source.parse::<KeyChord>().unwrap_or_else(|error| panic!("{source}: {error}"));
            let displayed = parsed.to_string();
            assert_eq!(displayed.parse::<KeyChord>().unwrap(), parsed, "{source}");
        }
    }

    #[test]
    fn junk_has_a_helpful_error() {
        let error = "ctrl+definitely-not-a-key".parse::<KeyChord>().unwrap_err();
        assert!(error.to_string().contains("unknown key"));
        assert!(error.to_string().contains("definitely-not-a-key"));
    }

    #[test]
    fn every_default_binding_parses() {
        let keys = Keys::default();
        let fields = [
            &keys.help,
            &keys.settings,
            &keys.detach,
            &keys.reload_config,
            &keys.open_notification_target,
            &keys.workspace_picker,
            &keys.goto,
            &keys.new_workspace,
            &keys.new_worktree,
            &keys.open_worktree,
            &keys.remove_worktree,
            &keys.rename_workspace,
            &keys.close_workspace,
            &keys.previous_workspace,
            &keys.next_workspace,
            &keys.previous_agent,
            &keys.next_agent,
            &keys.focus_agent,
            &keys.remote_image_paste,
            &keys.new_tab,
            &keys.rename_tab,
            &keys.previous_tab,
            &keys.next_tab,
            &keys.switch_tab,
            &keys.switch_workspace,
            &keys.close_tab,
            &keys.rename_pane,
            &keys.edit_scrollback,
            &keys.copy_mode,
            &keys.focus_pane_left,
            &keys.focus_pane_down,
            &keys.focus_pane_up,
            &keys.focus_pane_right,
            &keys.swap_pane_left,
            &keys.swap_pane_down,
            &keys.swap_pane_up,
            &keys.swap_pane_right,
            &keys.cycle_pane_next,
            &keys.cycle_pane_previous,
            &keys.last_pane,
            &keys.split_vertical,
            &keys.split_horizontal,
            &keys.close_pane,
            &keys.zoom,
            &keys.resize_mode,
            &keys.toggle_sidebar,
            &keys.navigate_workspace_up,
            &keys.navigate_workspace_down,
            &keys.navigate_pane_left,
            &keys.navigate_pane_down,
            &keys.navigate_pane_up,
            &keys.navigate_pane_right,
        ];
        assert_eq!(fields.len(), 52);
        for source in fields.into_iter().flat_map(KeyBindings::iter) {
            source.parse::<KeyChord>().unwrap_or_else(|error| panic!("{source}: {error}"));
        }
        let built = build_effective_keymap(&keys);
        assert!(built.diagnostics.is_empty(), "{:#?}", built.diagnostics);
    }

    #[test]
    fn keymap_detects_conflicts() {
        let mut keys = Keys::default();
        keys.settings = keys.help.clone();
        let built = build_effective_keymap(&keys);
        assert!(built.diagnostics.iter().any(|diagnostic| {
            diagnostic.path == "keys.settings" && diagnostic.message.contains("conflicts")
        }));
    }

    #[test]
    fn navigate_mode_rejects_reserved_bindings() {
        for source in ["prefix+j", "esc", "enter", "tab", "shift+tab", "left", "right", "1", "1..9"] {
            let chord = source.parse::<KeyChord>().unwrap();
            assert!(validate_navigate_chord(&chord).is_err(), "{source}");
        }
        for source in ["h", "j", "up", "down", "ctrl+1"] {
            let chord = source.parse::<KeyChord>().unwrap();
            assert!(validate_navigate_chord(&chord).is_ok(), "{source}");
        }
    }

    #[test]
    fn command_dimensions_support_cells_and_percentages() {
        #[derive(Deserialize)]
        struct Dimensions {
            width: CommandDimension,
            height: CommandDimension,
        }
        let parsed: Dimensions = toml::from_str("width = 72\nheight = \"80%\"").unwrap();
        assert_eq!(parsed.width, CommandDimension::Cells(72));
        assert_eq!(parsed.height, CommandDimension::Percent(80));
        assert!(toml::from_str::<Dimensions>("width = 0\nheight = \"101%\"").is_err());
    }
}
