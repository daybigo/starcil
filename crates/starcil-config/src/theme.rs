use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// User-facing theme selection and optional token overrides.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Theme {
    pub name: String,
    pub auto_switch: bool,
    pub dark_name: Option<String>,
    pub light_name: Option<String>,
    pub custom: BTreeMap<String, String>,
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            name: "starcil".to_owned(),
            auto_switch: false,
            dark_name: None,
            light_name: None,
            custom: BTreeMap::new(),
        }
    }
}

impl Theme {
    pub fn resolve(&self, appearance: HostAppearance) -> Result<ResolvedTheme, ThemeError> {
        resolve_theme(self, appearance)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostAppearance {
    Dark,
    Light,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum NamedColor {
    Black,
    DarkGray,
    Gray,
    White,
    Red,
    LightRed,
    Green,
    LightGreen,
    Yellow,
    LightYellow,
    Blue,
    LightBlue,
    Magenta,
    LightMagenta,
    Cyan,
    LightCyan,
}

impl fmt::Display for NamedColor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Self::Black => "black",
            Self::DarkGray => "dark-gray",
            Self::Gray => "gray",
            Self::White => "white",
            Self::Red => "red",
            Self::LightRed => "light-red",
            Self::Green => "green",
            Self::LightGreen => "light-green",
            Self::Yellow => "yellow",
            Self::LightYellow => "light-yellow",
            Self::Blue => "blue",
            Self::LightBlue => "light-blue",
            Self::Magenta => "magenta",
            Self::LightMagenta => "light-magenta",
            Self::Cyan => "cyan",
            Self::LightCyan => "light-cyan",
        };
        f.write_str(name)
    }
}

/// A concrete RGB color, a host-terminal named color, or the terminal default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Color {
    Rgb(u8, u8, u8),
    Named(NamedColor),
    Reset,
}

impl Color {
    pub const fn rgb(red: u8, green: u8, blue: u8) -> Self {
        Self::Rgb(red, green, blue)
    }
}

impl fmt::Display for Color {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Rgb(red, green, blue) => write!(f, "#{red:02x}{green:02x}{blue:02x}"),
            Self::Named(color) => color.fmt(f),
            Self::Reset => f.write_str("reset"),
        }
    }
}

impl FromStr for Color {
    type Err = ThemeError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        let value = input.trim().to_ascii_lowercase();
        if matches!(
            value.as_str(),
            "reset" | "default" | "none" | "transparent"
        ) {
            return Ok(Self::Reset);
        }

        if let Some(hex) = value.strip_prefix('#') {
            return parse_hex(hex).ok_or_else(|| ThemeError::InvalidColor(input.to_owned()));
        }

        if value.starts_with("rgb(") && value.ends_with(')') {
            let inner = &value[4..value.len() - 1];
            let components = inner
                .split(',')
                .map(str::trim)
                .map(str::parse::<u8>)
                .collect::<Result<Vec<_>, _>>()
                .map_err(|_| ThemeError::InvalidColor(input.to_owned()))?;
            if components.len() == 3 {
                return Ok(Self::Rgb(components[0], components[1], components[2]));
            }
            return Err(ThemeError::InvalidColor(input.to_owned()));
        }

        let normalized = value.replace('_', "-").replace(' ', "-");
        let named = match normalized.as_str() {
            "black" => NamedColor::Black,
            "dark-gray" | "dark-grey" | "bright-black" => NamedColor::DarkGray,
            "gray" | "grey" => NamedColor::Gray,
            "white" | "bright-white" => NamedColor::White,
            "red" => NamedColor::Red,
            "light-red" | "bright-red" => NamedColor::LightRed,
            "green" => NamedColor::Green,
            "light-green" | "bright-green" => NamedColor::LightGreen,
            "yellow" => NamedColor::Yellow,
            "light-yellow" | "bright-yellow" => NamedColor::LightYellow,
            "blue" => NamedColor::Blue,
            "light-blue" | "bright-blue" => NamedColor::LightBlue,
            "magenta" | "purple" => NamedColor::Magenta,
            "light-magenta" | "bright-magenta" | "light-purple" => NamedColor::LightMagenta,
            "cyan" => NamedColor::Cyan,
            "light-cyan" | "bright-cyan" => NamedColor::LightCyan,
            _ => return Err(ThemeError::InvalidColor(input.to_owned())),
        };
        Ok(Self::Named(named))
    }
}

impl Serialize for Color {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for Color {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        value.parse().map_err(serde::de::Error::custom)
    }
}

fn parse_hex(hex: &str) -> Option<Color> {
    if !hex.is_ascii() {
        return None;
    }
    match hex.len() {
        3 => {
            let mut digits = hex.chars().map(|digit| digit.to_digit(16).map(|v| v as u8));
            let red = digits.next()??;
            let green = digits.next()??;
            let blue = digits.next()??;
            Some(Color::Rgb(red * 17, green * 17, blue * 17))
        }
        6 => {
            let red = u8::from_str_radix(&hex[0..2], 16).ok()?;
            let green = u8::from_str_radix(&hex[2..4], 16).ok()?;
            let blue = u8::from_str_radix(&hex[4..6], 16).ok()?;
            Some(Color::Rgb(red, green, blue))
        }
        _ => None,
    }
}

/// The platform's brand purple: the single definition (#8B5CF6).
pub const BRAND_PURPLE: Color = Color::Rgb(0x8B, 0x5C, 0xF6);

pub const TOKEN_NAMES: &[&str] = &[
    "bg",
    "panel_bg",
    "surface0",
    "surface1",
    "surface_dim",
    "overlay0",
    "overlay1",
    "text",
    "subtext0",
    "mauve",
    "teal",
    "peach",
    "fg",
    "dim_fg",
    "accent",
    "brand",
    "border",
    "selection",
    "cursor",
    "red",
    "green",
    "yellow",
    "blue",
    "magenta",
    "cyan",
    "state_idle",
    "state_working",
    "state_blocked",
    "state_done",
    "state_unknown",
    "toast_bg",
    "toast_fg",
    "sidebar_bg",
    "sidebar_fg",
    "sidebar_selected_bg",
    "sidebar_selected_fg",
    "sidebar_border",
    "tab_active_bg",
    "tab_active_fg",
    "tab_inactive_fg",
    "pane_border_active",
    "pane_border_inactive",
    "status_bg",
    "status_fg",
];

/// Complete semantic token palette consumed by the TUI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThemeTokens {
    pub bg: Color,
    pub panel_bg: Color,
    pub surface0: Color,
    pub surface1: Color,
    pub surface_dim: Color,
    pub overlay0: Color,
    pub overlay1: Color,
    pub text: Color,
    pub subtext0: Color,
    pub mauve: Color,
    pub teal: Color,
    pub peach: Color,
    pub fg: Color,
    pub dim_fg: Color,
    pub accent: Color,
    /// Platform brand color (the purple of the command bar). One definition;
    /// widgets never hardcode the hex. Override with `[theme.custom] brand`.
    pub brand: Color,
    pub border: Color,
    pub selection: Color,
    pub cursor: Color,
    pub red: Color,
    pub green: Color,
    pub yellow: Color,
    pub blue: Color,
    pub magenta: Color,
    pub cyan: Color,
    pub state_idle: Color,
    pub state_working: Color,
    pub state_blocked: Color,
    pub state_done: Color,
    pub state_unknown: Color,
    pub toast_bg: Color,
    pub toast_fg: Color,
    pub sidebar_bg: Color,
    pub sidebar_fg: Color,
    pub sidebar_selected_bg: Color,
    pub sidebar_selected_fg: Color,
    pub sidebar_border: Color,
    pub tab_active_bg: Color,
    pub tab_active_fg: Color,
    pub tab_inactive_fg: Color,
    pub pane_border_active: Color,
    pub pane_border_inactive: Color,
    pub status_bg: Color,
    pub status_fg: Color,
}

impl ThemeTokens {
    #[allow(clippy::too_many_arguments)]
    fn from_core(
        bg: Color,
        panel_bg: Color,
        fg: Color,
        dim_fg: Color,
        accent: Color,
        border: Color,
        selection: Color,
        cursor: Color,
        red: Color,
        green: Color,
        yellow: Color,
        blue: Color,
        magenta: Color,
        cyan: Color,
    ) -> Self {
        Self {
            bg,
            panel_bg,
            surface0: selection,
            surface1: border,
            surface_dim: panel_bg,
            overlay0: dim_fg,
            overlay1: dim_fg,
            text: fg,
            subtext0: dim_fg,
            mauve: magenta,
            teal: cyan,
            peach: yellow,
            fg,
            dim_fg,
            accent,
            brand: BRAND_PURPLE,
            border,
            selection,
            cursor,
            red,
            green,
            yellow,
            blue,
            magenta,
            cyan,
            state_idle: dim_fg,
            state_working: blue,
            state_blocked: yellow,
            state_done: green,
            state_unknown: magenta,
            toast_bg: panel_bg,
            toast_fg: fg,
            sidebar_bg: panel_bg,
            sidebar_fg: fg,
            sidebar_selected_bg: selection,
            sidebar_selected_fg: fg,
            sidebar_border: border,
            tab_active_bg: selection,
            tab_active_fg: fg,
            tab_inactive_fg: dim_fg,
            pane_border_active: accent,
            pane_border_inactive: border,
            status_bg: panel_bg,
            status_fg: fg,
        }
    }

    pub fn get(&self, name: &str) -> Option<Color> {
        Some(match name {
            "bg" => self.bg,
            "panel_bg" => self.panel_bg,
            "surface0" => self.surface0,
            "surface1" => self.surface1,
            "surface_dim" => self.surface_dim,
            "overlay0" => self.overlay0,
            "overlay1" => self.overlay1,
            "text" => self.text,
            "subtext0" => self.subtext0,
            "mauve" => self.mauve,
            "teal" => self.teal,
            "peach" => self.peach,
            "fg" => self.fg,
            "dim_fg" => self.dim_fg,
            "accent" => self.accent,
            "brand" => self.brand,
            "border" => self.border,
            "selection" => self.selection,
            "cursor" => self.cursor,
            "red" => self.red,
            "green" => self.green,
            "yellow" => self.yellow,
            "blue" => self.blue,
            "magenta" => self.magenta,
            "cyan" => self.cyan,
            "state_idle" => self.state_idle,
            "state_working" => self.state_working,
            "state_blocked" => self.state_blocked,
            "state_done" => self.state_done,
            "state_unknown" => self.state_unknown,
            "toast_bg" => self.toast_bg,
            "toast_fg" => self.toast_fg,
            "sidebar_bg" => self.sidebar_bg,
            "sidebar_fg" => self.sidebar_fg,
            "sidebar_selected_bg" => self.sidebar_selected_bg,
            "sidebar_selected_fg" => self.sidebar_selected_fg,
            "sidebar_border" => self.sidebar_border,
            "tab_active_bg" => self.tab_active_bg,
            "tab_active_fg" => self.tab_active_fg,
            "tab_inactive_fg" => self.tab_inactive_fg,
            "pane_border_active" => self.pane_border_active,
            "pane_border_inactive" => self.pane_border_inactive,
            "status_bg" => self.status_bg,
            "status_fg" => self.status_fg,
            _ => return None,
        })
    }

    pub fn set(&mut self, name: &str, color: Color) -> Result<(), ThemeError> {
        let slot = match name {
            "bg" => &mut self.bg,
            "panel_bg" => &mut self.panel_bg,
            "surface0" => &mut self.surface0,
            "surface1" => &mut self.surface1,
            "surface_dim" => &mut self.surface_dim,
            "overlay0" => &mut self.overlay0,
            "overlay1" => &mut self.overlay1,
            "text" => &mut self.text,
            "subtext0" => &mut self.subtext0,
            "mauve" => &mut self.mauve,
            "teal" => &mut self.teal,
            "peach" => &mut self.peach,
            "fg" => &mut self.fg,
            "dim_fg" => &mut self.dim_fg,
            "accent" => &mut self.accent,
            "brand" => &mut self.brand,
            "border" => &mut self.border,
            "selection" => &mut self.selection,
            "cursor" => &mut self.cursor,
            "red" => &mut self.red,
            "green" => &mut self.green,
            "yellow" => &mut self.yellow,
            "blue" => &mut self.blue,
            "magenta" => &mut self.magenta,
            "cyan" => &mut self.cyan,
            "state_idle" => &mut self.state_idle,
            "state_working" => &mut self.state_working,
            "state_blocked" => &mut self.state_blocked,
            "state_done" => &mut self.state_done,
            "state_unknown" => &mut self.state_unknown,
            "toast_bg" => &mut self.toast_bg,
            "toast_fg" => &mut self.toast_fg,
            "sidebar_bg" => &mut self.sidebar_bg,
            "sidebar_fg" => &mut self.sidebar_fg,
            "sidebar_selected_bg" => &mut self.sidebar_selected_bg,
            "sidebar_selected_fg" => &mut self.sidebar_selected_fg,
            "sidebar_border" => &mut self.sidebar_border,
            "tab_active_bg" => &mut self.tab_active_bg,
            "tab_active_fg" => &mut self.tab_active_fg,
            "tab_inactive_fg" => &mut self.tab_inactive_fg,
            "pane_border_active" => &mut self.pane_border_active,
            "pane_border_inactive" => &mut self.pane_border_inactive,
            "status_bg" => &mut self.status_bg,
            "status_fg" => &mut self.status_fg,
            _ => return Err(ThemeError::UnknownToken(name.to_owned())),
        };
        *slot = color;
        match name {
            "panel_bg" => {
                self.toast_bg = color;
                self.sidebar_bg = color;
                self.status_bg = color;
            }
            "surface0" => {
                self.selection = color;
                self.sidebar_selected_bg = color;
                self.tab_active_bg = color;
            }
            "surface1" => {
                self.border = color;
                self.sidebar_border = color;
                self.pane_border_inactive = color;
            }
            "text" | "fg" => {
                self.text = color;
                self.fg = color;
                self.toast_fg = color;
                self.sidebar_fg = color;
                self.sidebar_selected_fg = color;
                self.tab_active_fg = color;
                self.status_fg = color;
            }
            "subtext0" | "dim_fg" => {
                self.subtext0 = color;
                self.dim_fg = color;
                self.tab_inactive_fg = color;
                self.state_idle = color;
            }
            "mauve" | "magenta" => {
                self.mauve = color;
                self.magenta = color;
                self.state_unknown = color;
            }
            "teal" | "cyan" => {
                self.teal = color;
                self.cyan = color;
            }
            "peach" => self.peach = color,
            "accent" => self.pane_border_active = color,
            "green" => self.state_done = color,
            "yellow" => self.state_blocked = color,
            "blue" => self.state_working = color,
            _ => {}
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedTheme {
    pub name: String,
    pub tokens: ThemeTokens,
}

/// Primary theme names shown in the default configuration. Catppuccin Latte is
/// an additional built-in used by appearance switching.
pub const BUILTIN_THEME_NAMES: &[&str] = &[
    "starcil",
    "catppuccin",
    "terminal",
    "tokyo-night",
    "dracula",
    "nord",
    "gruvbox",
    "one-dark",
    "solarized",
    "kanagawa",
    "rose-pine",
    "vesper",
];

pub fn resolve_theme(config: &Theme, appearance: HostAppearance) -> Result<ResolvedTheme, ThemeError> {
    let selected = if config.auto_switch {
        match appearance {
            HostAppearance::Light => config.light_name.as_deref().unwrap_or_else(|| light_sibling(&config.name)),
            HostAppearance::Dark => config.dark_name.as_deref().unwrap_or_else(|| dark_sibling(&config.name)),
            HostAppearance::Unknown => &config.name,
        }
    } else {
        &config.name
    };

    let mut tokens = builtin_theme(selected)?;
    for (token, value) in &config.custom {
        let color = value
            .parse::<Color>()
            .map_err(|_| ThemeError::InvalidOverride {
                token: token.clone(),
                value: value.clone(),
            })?;
        tokens.set(token, color)?;
    }

    Ok(ResolvedTheme {
        name: selected.to_owned(),
        tokens,
    })
}

fn light_sibling(name: &str) -> &str {
    match name {
        "catppuccin" | "catppuccin-latte" => "catppuccin-latte",
        "tokyo-night" | "tokyo-night-day" => "tokyo-night-day",
        "gruvbox" | "gruvbox-light" => "gruvbox-light",
        other => other,
    }
}

fn dark_sibling(name: &str) -> &str {
    match name {
        "catppuccin-latte" => "catppuccin",
        "tokyo-night-day" => "tokyo-night",
        "gruvbox-light" => "gruvbox",
        other => other,
    }
}

pub fn builtin_theme(name: &str) -> Result<ThemeTokens, ThemeError> {
    fn rgb(hex: u32) -> Color {
        Color::Rgb(
            ((hex >> 16) & 0xff) as u8,
            ((hex >> 8) & 0xff) as u8,
            (hex & 0xff) as u8,
        )
    }

    let mut colors = match name {
        "starcil" => ThemeTokens::from_core(
            rgb(0x1a1d22), rgb(0x20242c), rgb(0xc8cdd4), rgb(0x5a6068),
            rgb(0x4a9eff), rgb(0x2b3038), rgb(0x20242c), rgb(0x4a9eff),
            rgb(0xe05a5a), rgb(0x52c97a), rgb(0xe6c060), rgb(0x4a9eff),
            rgb(0x9b7ede), rgb(0x94e2d5),
        ),
        "catppuccin" => ThemeTokens::from_core(
            rgb(0x1e1e2e), rgb(0x181825), rgb(0xcdd6f4), rgb(0x7f849c),
            rgb(0xcba6f7), rgb(0x45475a), rgb(0x313244), rgb(0xf5e0dc),
            rgb(0xf38ba8), rgb(0xa6e3a1), rgb(0xf9e2af), rgb(0x89b4fa),
            rgb(0xf5c2e7), rgb(0x94e2d5),
        ),
        "catppuccin-latte" => ThemeTokens::from_core(
            rgb(0xeff1f5), rgb(0xe6e9ef), rgb(0x4c4f69), rgb(0x8c8fa1),
            rgb(0x8839ef), rgb(0xacb0be), rgb(0xdce0e8), rgb(0xdc8a78),
            rgb(0xd20f39), rgb(0x40a02b), rgb(0xdf8e1d), rgb(0x1e66f5),
            rgb(0xea76cb), rgb(0x179299),
        ),
        "terminal" => ThemeTokens::from_core(
            Color::Reset, Color::Reset, Color::Named(NamedColor::White),
            Color::Named(NamedColor::Gray), Color::Named(NamedColor::Cyan),
            Color::Named(NamedColor::DarkGray), Color::Named(NamedColor::Blue),
            Color::Named(NamedColor::White), Color::Named(NamedColor::Red),
            Color::Named(NamedColor::Green), Color::Named(NamedColor::Yellow),
            Color::Named(NamedColor::Blue), Color::Named(NamedColor::Magenta),
            Color::Named(NamedColor::Cyan),
        ),
        "tokyo-night" => ThemeTokens::from_core(
            rgb(0x1a1b26), rgb(0x16161e), rgb(0xc0caf5), rgb(0x565f89),
            rgb(0x7aa2f7), rgb(0x3b4261), rgb(0x33467c), rgb(0xc0caf5),
            rgb(0xf7768e), rgb(0x9ece6a), rgb(0xe0af68), rgb(0x7aa2f7),
            rgb(0xbb9af7), rgb(0x7dcfff),
        ),
        "tokyo-night-day" => ThemeTokens::from_core(
            rgb(0xe1e2e7), rgb(0xd5d6db), rgb(0x3760bf), rgb(0x6172b0),
            rgb(0x2e7de9), rgb(0xa8aecb), rgb(0xb7c1e3), rgb(0x3760bf),
            rgb(0xf52a65), rgb(0x587539), rgb(0x8c6c3e), rgb(0x2e7de9),
            rgb(0x9854f1), rgb(0x007197),
        ),
        "dracula" => ThemeTokens::from_core(
            rgb(0x282a36), rgb(0x21222c), rgb(0xf8f8f2), rgb(0x6272a4),
            rgb(0xbd93f9), rgb(0x44475a), rgb(0x44475a), rgb(0xf8f8f2),
            rgb(0xff5555), rgb(0x50fa7b), rgb(0xf1fa8c), rgb(0x8be9fd),
            rgb(0xff79c6), rgb(0x8be9fd),
        ),
        "nord" => ThemeTokens::from_core(
            rgb(0x2e3440), rgb(0x3b4252), rgb(0xeceff4), rgb(0x4c566a),
            rgb(0x88c0d0), rgb(0x4c566a), rgb(0x434c5e), rgb(0xd8dee9),
            rgb(0xbf616a), rgb(0xa3be8c), rgb(0xebcb8b), rgb(0x81a1c1),
            rgb(0xb48ead), rgb(0x8fbcbb),
        ),
        "gruvbox" => ThemeTokens::from_core(
            rgb(0x282828), rgb(0x1d2021), rgb(0xebdbb2), rgb(0x928374),
            rgb(0xd79921), rgb(0x504945), rgb(0x3c3836), rgb(0xebdbb2),
            rgb(0xfb4934), rgb(0xb8bb26), rgb(0xfabd2f), rgb(0x83a598),
            rgb(0xd3869b), rgb(0x8ec07c),
        ),
        "gruvbox-light" => ThemeTokens::from_core(
            rgb(0xfbf1c7), rgb(0xebdbb2), rgb(0x3c3836), rgb(0x7c6f64),
            rgb(0xb57614), rgb(0xbdae93), rgb(0xd5c4a1), rgb(0x3c3836),
            rgb(0x9d0006), rgb(0x79740e), rgb(0xb57614), rgb(0x076678),
            rgb(0x8f3f71), rgb(0x427b58),
        ),
        "one-dark" => ThemeTokens::from_core(
            rgb(0x282c34), rgb(0x21252b), rgb(0xabb2bf), rgb(0x5c6370),
            rgb(0x61afef), rgb(0x3e4451), rgb(0x3e4451), rgb(0x528bff),
            rgb(0xe06c75), rgb(0x98c379), rgb(0xe5c07b), rgb(0x61afef),
            rgb(0xc678dd), rgb(0x56b6c2),
        ),
        "solarized" => ThemeTokens::from_core(
            rgb(0x002b36), rgb(0x073642), rgb(0x839496), rgb(0x586e75),
            rgb(0x268bd2), rgb(0x586e75), rgb(0x073642), rgb(0x93a1a1),
            rgb(0xdc322f), rgb(0x859900), rgb(0xb58900), rgb(0x268bd2),
            rgb(0xd33682), rgb(0x2aa198),
        ),
        "kanagawa" => ThemeTokens::from_core(
            rgb(0x1f1f28), rgb(0x16161d), rgb(0xdcd7ba), rgb(0x727169),
            rgb(0x7e9cd8), rgb(0x54546d), rgb(0x2d4f67), rgb(0xc8c093),
            rgb(0xe46876), rgb(0x98bb6c), rgb(0xe6c384), rgb(0x7e9cd8),
            rgb(0x957fb8), rgb(0x7fb4ca),
        ),
        "rose-pine" => ThemeTokens::from_core(
            rgb(0x191724), rgb(0x1f1d2e), rgb(0xe0def4), rgb(0x6e6a86),
            rgb(0xc4a7e7), rgb(0x403d52), rgb(0x26233a), rgb(0xe0def4),
            rgb(0xeb6f92), rgb(0x9ccfd8), rgb(0xf6c177), rgb(0x31748f),
            rgb(0xc4a7e7), rgb(0xebbcba),
        ),
        "vesper" => ThemeTokens::from_core(
            rgb(0x101010), rgb(0x161616), rgb(0xffffff), rgb(0xa0a0a0),
            rgb(0xffc799), rgb(0x282828), rgb(0x232323), rgb(0xffffff),
            rgb(0xff8080), rgb(0x99ffe4), rgb(0xffc799), rgb(0xafb1ff),
            rgb(0xd5a3ff), rgb(0x99ffe4),
        ),
        _ => return Err(ThemeError::UnknownTheme(name.to_owned())),
    };
    match name {
        "starcil" => {
            colors.surface0 = rgb(0x20242c);
            colors.surface1 = rgb(0x2b3038);
            colors.surface_dim = rgb(0x15181d);
            colors.overlay0 = rgb(0x6a7078);
            colors.overlay1 = rgb(0x6a7078);
            colors.text = rgb(0xc8cdd4);
            colors.subtext0 = rgb(0x5a6068);
            colors.teal = rgb(0x94e2d5);
            colors.sidebar_bg = rgb(0x15181d);
            colors.sidebar_fg = rgb(0xc8cdd4);
            colors.sidebar_selected_bg = rgb(0x20242c);
            colors.sidebar_selected_fg = rgb(0xc8cdd4);
            colors.sidebar_border = rgb(0x2b3038);
            colors.tab_active_bg = rgb(0x4a9eff);
            colors.tab_active_fg = rgb(0x0c0c0b);
            colors.tab_inactive_fg = rgb(0x5a6068);
            colors.pane_border_active = rgb(0x4a9eff);
            colors.pane_border_inactive = rgb(0x2b3038);
            colors.state_idle = rgb(0x52c97a);
            colors.state_working = rgb(0xe6c060);
            colors.state_blocked = rgb(0xe05a5a);
            colors.state_done = rgb(0x94e2d5);
            colors.status_bg = rgb(0x15181d);
            colors.status_fg = rgb(0x5a6068);
        }
        "catppuccin" => {
            colors.surface0 = rgb(0x313244);
            colors.surface1 = rgb(0x45475a);
            colors.surface_dim = rgb(0x11111b);
            colors.overlay0 = rgb(0x6c7086);
            colors.overlay1 = rgb(0x7f849c);
            colors.text = rgb(0xcdd6f4);
            colors.subtext0 = rgb(0xa6adc8);
            colors.mauve = rgb(0xcba6f7);
            colors.teal = rgb(0x94e2d5);
            colors.peach = rgb(0xfab387);
        }
        "catppuccin-latte" => {
            colors.surface0 = rgb(0xccd0da);
            colors.surface1 = rgb(0xbcc0cc);
            colors.surface_dim = rgb(0xdce0e8);
            colors.overlay0 = rgb(0x9ca0b0);
            colors.overlay1 = rgb(0x8c8fa1);
            colors.text = rgb(0x4c4f69);
            colors.subtext0 = rgb(0x6c6f85);
            colors.mauve = rgb(0x8839ef);
            colors.teal = rgb(0x179299);
            colors.peach = rgb(0xfe640b);
        }
        _ => {}
    }
    Ok(colors)
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ThemeError {
    #[error("unknown theme `{0}`")]
    UnknownTheme(String),
    #[error("unknown theme token `{0}`")]
    UnknownToken(String),
    #[error("invalid color `{0}`; expected #rgb, #rrggbb, rgb(r,g,b), a named color, or reset")]
    InvalidColor(String),
    #[error("invalid color `{value}` for theme token `{token}`")]
    InvalidOverride { token: String, value: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_builtin_resolves_every_token() {
        assert_eq!(BUILTIN_THEME_NAMES.len(), 12);
        for name in BUILTIN_THEME_NAMES
            .iter()
            .copied()
            .chain(["catppuccin-latte"])
        {
            let tokens = builtin_theme(name).unwrap_or_else(|error| panic!("{name}: {error}"));
            for token in TOKEN_NAMES {
                assert!(tokens.get(token).is_some(), "{name}.{token}");
            }
        }
    }

    #[test]
    fn color_formats_and_custom_overrides_work() {
        assert_eq!("#abc".parse::<Color>().unwrap(), Color::Rgb(0xaa, 0xbb, 0xcc));
        assert_eq!("#102030".parse::<Color>().unwrap(), Color::Rgb(0x10, 0x20, 0x30));
        assert_eq!("rgb(1, 2, 255)".parse::<Color>().unwrap(), Color::Rgb(1, 2, 255));
        assert_eq!("cyan".parse::<Color>().unwrap(), Color::Named(NamedColor::Cyan));
        assert_eq!("reset".parse::<Color>().unwrap(), Color::Reset);
        assert_eq!("default".parse::<Color>().unwrap(), Color::Reset);

        let mut config = Theme::default();
        config.custom.insert("accent".to_owned(), "rgb(1,2,3)".to_owned());
        config.custom.insert("panel_bg".to_owned(), "reset".to_owned());
        config.custom.insert("surface0".to_owned(), "#123".to_owned());
        let resolved = config.resolve(HostAppearance::Dark).unwrap();
        assert_eq!(resolved.tokens.accent, Color::Rgb(1, 2, 3));
        assert_eq!(resolved.tokens.panel_bg, Color::Reset);
        assert_eq!(resolved.tokens.surface0, Color::Rgb(0x11, 0x22, 0x33));
        assert_eq!(resolved.tokens.selection, Color::Rgb(0x11, 0x22, 0x33));
    }

    #[test]
    fn brand_token_defaults_to_platform_purple_and_is_overridable() {
        let resolved = Theme::default().resolve(HostAppearance::Dark).unwrap();
        assert_eq!(resolved.tokens.brand, BRAND_PURPLE);
        assert_eq!(BRAND_PURPLE, Color::Rgb(0x8B, 0x5C, 0xF6));
        // The brand is theme-independent: switching themes keeps it.
        let rose = Theme { name: "rose-pine".to_owned(), ..Theme::default() }
            .resolve(HostAppearance::Dark)
            .unwrap();
        assert_eq!(rose.tokens.brand, BRAND_PURPLE);
        // One override point, like every other token.
        let mut custom = Theme::default();
        custom.custom.insert("brand".to_owned(), "#112233".to_owned());
        let resolved = custom.resolve(HostAppearance::Dark).unwrap();
        assert_eq!(resolved.tokens.brand, Color::Rgb(0x11, 0x22, 0x33));
    }

    #[test]
    fn default_theme_matches_the_starcil_visual_reference() {
        let resolved = Theme::default().resolve(HostAppearance::Dark).unwrap();
        assert_eq!(resolved.name, "starcil");
        assert_eq!(resolved.tokens.bg, Color::Rgb(0x1a, 0x1d, 0x22));
        assert_eq!(resolved.tokens.sidebar_bg, Color::Rgb(0x15, 0x18, 0x1d));
        assert_eq!(resolved.tokens.tab_active_bg, Color::Rgb(0x4a, 0x9e, 0xff));
        assert_eq!(resolved.tokens.tab_active_fg, Color::Rgb(0x0c, 0x0c, 0x0b));
        assert_eq!(resolved.tokens.pane_border_inactive, Color::Rgb(0x2b, 0x30, 0x38));
        assert_eq!(resolved.tokens.state_working, Color::Rgb(0xe6, 0xc0, 0x60));
        assert_eq!(resolved.tokens.state_blocked, Color::Rgb(0xe0, 0x5a, 0x5a));
        assert_eq!(resolved.tokens.state_done, Color::Rgb(0x94, 0xe2, 0xd5));
    }

    #[test]
    fn auto_switch_uses_named_light_siblings() {
        let config = Theme {
            name: "catppuccin".to_owned(),
            auto_switch: true,
            ..Theme::default()
        };
        assert_eq!(
            config.resolve(HostAppearance::Light).unwrap().name,
            "catppuccin-latte"
        );
        assert_eq!(
            config.resolve(HostAppearance::Dark).unwrap().name,
            "catppuccin"
        );

        let tokyo = Theme {
            name: "tokyo-night".to_owned(),
            auto_switch: true,
            ..Theme::default()
        };
        assert_eq!(
            tokyo.resolve(HostAppearance::Light).unwrap().name,
            "tokyo-night-day"
        );
        assert_eq!(
            tokyo.resolve(HostAppearance::Dark).unwrap().name,
            "tokyo-night"
        );
    }
}
