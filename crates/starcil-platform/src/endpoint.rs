use std::fmt;
use std::path::PathBuf;
use std::str::FromStr;

use thiserror::Error;

use crate::paths::{validate_session_name, PathError};

#[cfg(not(windows))]
use crate::paths::PlatformPaths;

const WINDOWS_PREFIX: &str = r"\\.\pipe\starcil-";

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum EndpointError {
    #[error(transparent)]
    InvalidSession(#[from] PathError),
    #[error("invalid Starcil transport endpoint: {0}")]
    InvalidEndpoint(String),
    #[error("unable to resolve platform paths: {0}")]
    Paths(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransportEndpoint {
    WindowsPipe {
        user_hash: String,
        session: String,
    },
    UnixSocket {
        path: PathBuf,
        session: String,
    },
}

impl TransportEndpoint {
    pub fn for_session(session: &str) -> Result<Self, EndpointError> {
        validate_session_name(session)?;

        #[cfg(windows)]
        {
            Ok(Self::WindowsPipe {
                user_hash: current_user_hash(),
                session: session.to_owned(),
            })
        }

        #[cfg(not(windows))]
        {
            let paths = PlatformPaths::discover()
                .map_err(|error| EndpointError::Paths(error.to_string()))?;
            Ok(Self::UnixSocket {
                path: paths.runtime_dir().join(format!("{session}.sock")),
                session: session.to_owned(),
            })
        }
    }

    pub fn session(&self) -> &str {
        match self {
            Self::WindowsPipe { session, .. } | Self::UnixSocket { session, .. } => session,
        }
    }

    pub fn as_address(&self) -> String {
        self.to_string()
    }
}

impl fmt::Display for TransportEndpoint {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WindowsPipe { user_hash, session } => {
                write!(formatter, "{WINDOWS_PREFIX}{user_hash}-{session}")
            }
            Self::UnixSocket { path, .. } => formatter.write_str(&path.to_string_lossy()),
        }
    }
}

impl FromStr for TransportEndpoint {
    type Err = EndpointError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if let Some(rest) = value.strip_prefix(WINDOWS_PREFIX) {
            let (user_hash, session) = rest
                .split_once('-')
                .ok_or_else(|| EndpointError::InvalidEndpoint(value.to_owned()))?;
            if user_hash.len() != 16 || !user_hash.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                return Err(EndpointError::InvalidEndpoint(value.to_owned()));
            }
            validate_session_name(session)?;
            return Ok(Self::WindowsPipe {
                user_hash: user_hash.to_ascii_lowercase(),
                session: session.to_owned(),
            });
        }

        let path = PathBuf::from(value);
        let file_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| EndpointError::InvalidEndpoint(value.to_owned()))?;
        let session = file_name
            .strip_suffix(".sock")
            .ok_or_else(|| EndpointError::InvalidEndpoint(value.to_owned()))?
            .to_owned();
        validate_session_name(&session)?;
        Ok(Self::UnixSocket {
            path,
            session,
        })
    }
}

fn current_user_hash() -> String {
    let domain = std::env::var("USERDOMAIN").unwrap_or_default();
    let user = std::env::var("USERNAME")
        .or_else(|_| std::env::var("USER"))
        .unwrap_or_else(|_| "unknown-user".to_owned());
    stable_hash(&format!("{domain}\\{user}"))
}

fn stable_hash(value: &str) -> String {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in value.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_endpoint_format_parse_round_trip() {
        let endpoint = TransportEndpoint::WindowsPipe {
            user_hash: "0123456789abcdef".to_owned(),
            session: "agents-1".to_owned(),
        };
        let parsed: TransportEndpoint = endpoint.to_string().parse().expect("parse endpoint");
        assert_eq!(parsed, endpoint);
    }

    #[test]
    fn unix_endpoint_format_parse_round_trip() {
        let endpoint = TransportEndpoint::UnixSocket {
            path: PathBuf::from("/tmp/starcil/review.sock"),
            session: "review".to_owned(),
        };
        let parsed: TransportEndpoint = endpoint.to_string().parse().expect("parse endpoint");
        assert_eq!(parsed, endpoint);
    }

    #[test]
    fn hash_is_stable_and_does_not_expose_identity() {
        assert_eq!(stable_hash("domain\\user"), stable_hash("domain\\user"));
        assert_eq!(stable_hash("domain\\user").len(), 16);
        assert_ne!(stable_hash("domain\\user"), stable_hash("domain\\other"));
    }
}
