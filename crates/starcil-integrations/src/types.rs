use serde::Serialize;
use std::path::{Path, PathBuf};
use thiserror::Error;

pub const INTEGRATION_VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum IntegrationKind {
    LifecycleAuthority,
    SessionIdentity,
    UnsupportedYet,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct InstallReport {
    pub integration_id: String,
    pub changed: bool,
    pub paths: Vec<PathBuf>,
    pub backup: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct IntegrationStatus {
    pub integration_id: String,
    pub supported: bool,
    pub installed: bool,
    pub version: Option<String>,
    pub outdated: bool,
}

pub trait Integration {
    fn id(&self) -> &'static str;
    fn display_name(&self) -> &'static str;
    fn kind(&self) -> IntegrationKind;
    fn install(&self, home: &Path) -> Result<InstallReport, IntegrationError>;
    fn uninstall(&self, home: &Path) -> Result<InstallReport, IntegrationError>;
    fn status(&self, home: &Path) -> Result<IntegrationStatus, IntegrationError>;
}

#[derive(Debug, Error)]
pub enum IntegrationError {
    #[error("could not access `{path}`: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("invalid JSON integration config: {0}")]
    Json(#[from] serde_json::Error),
    #[error("invalid TOML integration config: {0}")]
    Toml(#[from] toml_edit::TomlError),
    #[error("integration config is not valid UTF-8: {0}")]
    Utf8(#[from] std::string::FromUtf8Error),
    #[error("integration config `{0}` must contain a JSON object")]
    ExpectedJsonObject(PathBuf),
    #[error("integration config `{path}` has invalid `{field}` structure")]
    InvalidStructure { path: PathBuf, field: String },
    #[error("integration config directory does not exist: `{0}`")]
    MissingConfigDirectory(PathBuf),
    #[error("not yet supported in starcil {version}: {integration_id}")]
    NotYetSupported {
        integration_id: &'static str,
        version: &'static str,
    },
}

pub(crate) fn read_optional(path: &Path) -> Result<Option<Vec<u8>>, IntegrationError> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(IntegrationError::Io {
            path: path.to_owned(),
            source,
        }),
    }
}

pub(crate) fn write_file(path: &Path, bytes: &[u8]) -> Result<(), IntegrationError> {
    std::fs::write(path, bytes).map_err(|source| IntegrationError::Io {
        path: path.to_owned(),
        source,
    })
}

pub(crate) fn remove_optional(path: &Path) -> Result<bool, IntegrationError> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(true),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(source) => Err(IntegrationError::Io {
            path: path.to_owned(),
            source,
        }),
    }
}
