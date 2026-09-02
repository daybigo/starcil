use std::env;
use std::path::{Path, PathBuf};

use thiserror::Error;

const APP_DIR_NAME: &str = "starcil";

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum PathError {
    #[error("required environment variable {0} is not set")]
    MissingEnvironment(&'static str),
    #[error("invalid session name {0:?}; use 1-64 ASCII letters, digits, '.', '_' or '-'")]
    InvalidSessionName(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlatformPaths {
    config_dir: PathBuf,
    data_dir: PathBuf,
    log_dir: PathBuf,
    runtime_dir: PathBuf,
}

impl PlatformPaths {
    pub fn discover() -> Result<Self, PathError> {
        #[cfg(windows)]
        {
            let config_dir = required_path("APPDATA")?.join(APP_DIR_NAME);
            let data_dir = env::var_os("LOCALAPPDATA")
                .map(PathBuf::from)
                .map(|base| base.join(APP_DIR_NAME))
                .unwrap_or_else(|| config_dir.clone());
            let runtime_dir = data_dir.join("runtime");

            Ok(Self {
                log_dir: config_dir.clone(),
                config_dir,
                data_dir,
                runtime_dir,
            })
        }

        #[cfg(not(windows))]
        {
            let home = required_path("HOME")?;
            let config_dir = env::var_os("XDG_CONFIG_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join(".config"))
                .join(APP_DIR_NAME);
            let data_dir = env::var_os("XDG_DATA_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join(".local").join("share"))
                .join(APP_DIR_NAME);
            let runtime_dir = env::var_os("XDG_RUNTIME_DIR")
                .map(PathBuf::from)
                .map(|base| base.join(APP_DIR_NAME))
                .unwrap_or_else(|| home.join(".starcil"));

            Ok(Self {
                log_dir: config_dir.clone(),
                config_dir,
                data_dir,
                runtime_dir,
            })
        }
    }

    pub fn config_dir(&self) -> &Path {
        &self.config_dir
    }

    pub fn config_file(&self) -> PathBuf {
        self.config_dir.join("config.toml")
    }

    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    pub fn log_dir(&self) -> &Path {
        &self.log_dir
    }

    pub fn runtime_dir(&self) -> &Path {
        &self.runtime_dir
    }

    pub fn session_runtime_dir(&self, session: &str) -> Result<PathBuf, PathError> {
        validate_session_name(session)?;
        Ok(self.runtime_dir.join(session))
    }
}

pub(crate) fn validate_session_name(session: &str) -> Result<(), PathError> {
    let valid = !session.is_empty()
        && session.len() <= 64
        && session
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'));

    if valid {
        Ok(())
    } else {
        Err(PathError::InvalidSessionName(session.to_owned()))
    }
}

fn required_path(name: &'static str) -> Result<PathBuf, PathError> {
    env::var_os(name)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .ok_or(PathError::MissingEnvironment(name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_runtime_dir_rejects_path_traversal() {
        let paths = PlatformPaths {
            config_dir: PathBuf::from("config"),
            data_dir: PathBuf::from("data"),
            log_dir: PathBuf::from("logs"),
            runtime_dir: PathBuf::from("runtime"),
        };

        assert!(paths.session_runtime_dir("work-1").is_ok());
        assert!(paths.session_runtime_dir("../escape").is_err());
        assert!(paths.session_runtime_dir("a/b").is_err());
    }

    #[test]
    fn discovered_logs_are_next_to_config() {
        let paths = PlatformPaths::discover().expect("platform paths");
        assert_eq!(paths.log_dir(), paths.config_dir());
        assert_eq!(paths.config_file(), paths.config_dir().join("config.toml"));
    }
}
