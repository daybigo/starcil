//! Public identifiers: opaque, stable handles. `w1`, `w1:t1`, `w1:p1`.
//! Counters are monotonic per scope; closed ids are never reused.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct WorkspaceId(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct TabId {
    pub workspace: u64,
    pub tab: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct PaneId {
    pub workspace: u64,
    pub pane: u64,
}

impl fmt::Display for WorkspaceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "w{}", self.0)
    }
}

impl fmt::Display for TabId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "w{}:t{}", self.workspace, self.tab)
    }
}

impl fmt::Display for PaneId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "w{}:p{}", self.workspace, self.pane)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid id `{0}`")]
pub struct IdParseError(pub String);

fn parse_num(s: &str, prefix: char) -> Option<u64> {
    let rest = s.strip_prefix(prefix)?;
    if rest.is_empty() || !rest.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    rest.parse().ok()
}

impl FromStr for WorkspaceId {
    type Err = IdParseError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        parse_num(s, 'w')
            .map(WorkspaceId)
            .ok_or_else(|| IdParseError(s.to_string()))
    }
}

impl FromStr for TabId {
    type Err = IdParseError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (w, t) = s.split_once(':').ok_or_else(|| IdParseError(s.to_string()))?;
        match (parse_num(w, 'w'), parse_num(t, 't')) {
            (Some(workspace), Some(tab)) => Ok(TabId { workspace, tab }),
            _ => Err(IdParseError(s.to_string())),
        }
    }
}

impl FromStr for PaneId {
    type Err = IdParseError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (w, p) = s.split_once(':').ok_or_else(|| IdParseError(s.to_string()))?;
        match (parse_num(w, 'w'), parse_num(p, 'p')) {
            (Some(workspace), Some(pane)) => Ok(PaneId { workspace, pane }),
            _ => Err(IdParseError(s.to_string())),
        }
    }
}

/// Any id-like CLI target: workspace, tab, or pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnyId {
    Workspace(WorkspaceId),
    Tab(TabId),
    Pane(PaneId),
}

impl FromStr for AnyId {
    type Err = IdParseError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if let Ok(p) = s.parse::<PaneId>() {
            return Ok(AnyId::Pane(p));
        }
        if let Ok(t) = s.parse::<TabId>() {
            return Ok(AnyId::Tab(t));
        }
        if let Ok(w) = s.parse::<WorkspaceId>() {
            return Ok(AnyId::Workspace(w));
        }
        Err(IdParseError(s.to_string()))
    }
}

/// Live-agent names: `[a-z][a-z0-9_-]{0,31}`, unique among live agents.
pub fn valid_agent_name(name: &str) -> bool {
    let mut bytes = name.bytes();
    match bytes.next() {
        Some(b) if b.is_ascii_lowercase() => {}
        _ => return false,
    }
    name.len() <= 32
        && bytes.all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        for s in ["w1", "w42"] {
            assert_eq!(s.parse::<WorkspaceId>().unwrap().to_string(), s);
        }
        for s in ["w1:t1", "w9:t33"] {
            assert_eq!(s.parse::<TabId>().unwrap().to_string(), s);
        }
        for s in ["w1:p1", "w2:p700"] {
            assert_eq!(s.parse::<PaneId>().unwrap().to_string(), s);
        }
    }

    #[test]
    fn rejects_junk() {
        assert!("".parse::<WorkspaceId>().is_err());
        assert!("w".parse::<WorkspaceId>().is_err());
        assert!("w1x".parse::<WorkspaceId>().is_err());
        assert!("t1".parse::<TabId>().is_err());
        assert!("w1:p2".parse::<TabId>().is_err());
        assert!("w1:t2".parse::<PaneId>().is_err());
        assert!("w-1:p2".parse::<PaneId>().is_err());
    }

    #[test]
    fn any_id_precedence() {
        assert!(matches!("w1:p2".parse::<AnyId>(), Ok(AnyId::Pane(_))));
        assert!(matches!("w1:t2".parse::<AnyId>(), Ok(AnyId::Tab(_))));
        assert!(matches!("w3".parse::<AnyId>(), Ok(AnyId::Workspace(_))));
        assert!("agentname".parse::<AnyId>().is_err());
    }

    #[test]
    fn agent_names() {
        assert!(valid_agent_name("reviewer"));
        assert!(valid_agent_name("a1_b-c"));
        assert!(!valid_agent_name("1abc"));
        assert!(!valid_agent_name("Abc"));
        assert!(!valid_agent_name(""));
        assert!(!valid_agent_name(&"a".repeat(33)));
        assert!(valid_agent_name(&"a".repeat(32)));
    }
}
