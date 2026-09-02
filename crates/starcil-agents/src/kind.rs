use serde::{Deserialize, Serialize};
use std::{fmt, str::FromStr};
use thiserror::Error;

/// Built-in agent identifiers understood by Starcil.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(rename_all = "lowercase")]
pub enum AgentKind {
    Pi,
    Claude,
    Codex,
    Gemini,
    Cursor,
    Devin,
    Agy,
    Cline,
    Omp,
    Mastracode,
    Opencode,
    Copilot,
    Kimi,
    Kiro,
    Droid,
    Amp,
    Grok,
    Hermes,
    Kilo,
    Qodercli,
    Maki,
}

impl AgentKind {
    pub const ALL: [Self; 21] = [
        Self::Pi,
        Self::Claude,
        Self::Codex,
        Self::Gemini,
        Self::Cursor,
        Self::Devin,
        Self::Agy,
        Self::Cline,
        Self::Omp,
        Self::Mastracode,
        Self::Opencode,
        Self::Copilot,
        Self::Kimi,
        Self::Kiro,
        Self::Droid,
        Self::Amp,
        Self::Grok,
        Self::Hermes,
        Self::Kilo,
        Self::Qodercli,
        Self::Maki,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pi => "pi",
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::Gemini => "gemini",
            Self::Cursor => "cursor",
            Self::Devin => "devin",
            Self::Agy => "agy",
            Self::Cline => "cline",
            Self::Omp => "omp",
            Self::Mastracode => "mastracode",
            Self::Opencode => "opencode",
            Self::Copilot => "copilot",
            Self::Kimi => "kimi",
            Self::Kiro => "kiro",
            Self::Droid => "droid",
            Self::Amp => "amp",
            Self::Grok => "grok",
            Self::Hermes => "hermes",
            Self::Kilo => "kilo",
            Self::Qodercli => "qodercli",
            Self::Maki => "maki",
        }
    }
}

impl fmt::Display for AgentKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for AgentKind {
    type Err = ParseAgentKindError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let normalized = value.trim();
        Self::ALL
            .into_iter()
            .find(|kind| kind.as_str().eq_ignore_ascii_case(normalized))
            .ok_or_else(|| ParseAgentKindError(value.to_owned()))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("unsupported agent kind `{0}`")]
pub struct ParseAgentKindError(pub String);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_kind_round_trips_through_its_manifest_name() {
        for kind in AgentKind::ALL {
            assert_eq!(kind.as_str().parse::<AgentKind>(), Ok(kind));
            assert_eq!(kind.to_string(), kind.as_str());
        }
    }

    #[test]
    fn parsing_is_ascii_case_insensitive_but_strict() {
        assert_eq!(" CoDeX ".parse::<AgentKind>(), Ok(AgentKind::Codex));
        assert!("aider".parse::<AgentKind>().is_err());
    }
}
