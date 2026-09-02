use serde::{Deserialize, Serialize};
use std::{fmt, str::FromStr};
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Channel {
    Stable,
    Preview,
}

impl Channel {
    /// Stable on every platform: releases are tagged `vX.Y.Z` and Windows is
    /// the primary target, so nobody is opted into previews by default.
    /// `update.channel = "preview"` opts in.
    pub const fn platform_default() -> Self {
        Self::Stable
    }

    pub const fn resolve(config_override: Option<Self>) -> Self {
        match config_override {
            Some(channel) => channel,
            None => Self::platform_default(),
        }
    }
}

impl fmt::Display for Channel {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Stable => "stable",
            Self::Preview => "preview",
        })
    }
}

impl FromStr for Channel {
    type Err = ParseChannelError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "stable" => Ok(Self::Stable),
            "preview" => Ok(Self::Preview),
            _ => Err(ParseChannelError(value.to_owned())),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("unknown update channel `{0}`")]
pub struct ParseChannelError(pub String);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Platform {
    WindowsX86_64Gnu,
    LinuxX86_64,
    LinuxAarch64,
    MacosX86_64,
    MacosAarch64,
}

impl Platform {
    pub const fn current() -> Option<Self> {
        if cfg!(all(windows, target_arch = "x86_64")) {
            Some(Self::WindowsX86_64Gnu)
        } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
            Some(Self::LinuxX86_64)
        } else if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
            Some(Self::LinuxAarch64)
        } else if cfg!(all(target_os = "macos", target_arch = "x86_64")) {
            Some(Self::MacosX86_64)
        } else if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
            Some(Self::MacosAarch64)
        } else {
            None
        }
    }

    /// Plain executable asset used until an archive crate is approved for the update engine.
    pub const fn update_asset_name(self) -> &'static str {
        match self {
            Self::WindowsX86_64Gnu => "starcil-x86_64-pc-windows-gnu.exe",
            Self::LinuxX86_64 => "starcil-x86_64-unknown-linux-gnu",
            Self::LinuxAarch64 => "starcil-aarch64-unknown-linux-gnu",
            Self::MacosX86_64 => "starcil-x86_64-apple-darwin",
            Self::MacosAarch64 => "starcil-aarch64-apple-darwin",
        }
    }

    pub const fn executable_name(self) -> &'static str {
        match self {
            Self::WindowsX86_64Gnu => "starcil.exe",
            _ => "starcil",
        }
    }

    pub const fn backup_name(self) -> &'static str {
        match self {
            Self::WindowsX86_64Gnu => "starcil-old.exe",
            _ => "starcil-old",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_override_wins_over_platform_default() {
        assert_eq!(Channel::resolve(Some(Channel::Stable)), Channel::Stable);
        assert_eq!(Channel::resolve(Some(Channel::Preview)), Channel::Preview);
        assert_eq!(Channel::platform_default(), Channel::Stable);
    }

    #[test]
    fn channel_parsing_is_strict_and_case_insensitive() {
        assert_eq!(" PREVIEW ".parse::<Channel>(), Ok(Channel::Preview));
        assert!("nightly".parse::<Channel>().is_err());
    }
}
