use std::{fmt, str::FromStr};

use thiserror::Error;

/// An OpenSSH destination passed to `ssh` without rewriting.
///
/// OpenSSH accepts aliases/hosts, `[user@]host`, and URI targets such as
/// `ssh://user@host:2222`. `user@host:2222` is retained verbatim for callers
/// that rely on a custom SSH implementation, but portable OpenSSH callers
/// should use the URI form (or configure `Port` in ssh config).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RemoteTarget(String);

impl RemoteTarget {
    pub fn parse(value: impl Into<String>) -> Result<Self, RemoteTargetError> {
        let value = value.into();
        if value.is_empty() || value.chars().any(char::is_whitespace) {
            return Err(RemoteTargetError::EmptyOrWhitespace);
        }
        if value.starts_with('-') {
            return Err(RemoteTargetError::OptionLike);
        }
        if value.contains('\0') {
            return Err(RemoteTargetError::Nul);
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn into_string(self) -> String {
        self.0
    }
}

impl fmt::Display for RemoteTarget {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl FromStr for RemoteTarget {
    type Err = RemoteTargetError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value.to_owned())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum RemoteTargetError {
    #[error("remote SSH target must be non-empty and contain no whitespace")]
    EmptyOrWhitespace,
    #[error("remote SSH target must not begin with '-' because it would be parsed as an ssh option")]
    OptionLike,
    #[error("remote SSH target must not contain a NUL byte")]
    Nul,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepted_targets_are_preserved_verbatim() {
        for target in [
            "buildbox",
            "dev@buildbox",
            "dev@buildbox:2222",
            "ssh://dev@buildbox:2222",
        ] {
            assert_eq!(RemoteTarget::parse(target).unwrap().as_str(), target);
        }
    }

    #[test]
    fn empty_whitespace_and_option_like_targets_are_rejected() {
        for target in ["", "   ", "dev @host", "host\nnext", "-oProxyCommand=bad"] {
            assert!(RemoteTarget::parse(target).is_err(), "accepted {target:?}");
        }
    }
}
