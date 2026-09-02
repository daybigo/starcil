use starcil_protocol::error::{ApiError, ErrorCode};
use std::path::PathBuf;

pub type PluginResult<T> = Result<T, PluginError>;

#[derive(Debug, thiserror::Error)]
pub enum PluginError {
    #[error("failed to read plugin manifest {path}: {message}")]
    ManifestRead { path: PathBuf, message: String },
    #[error("failed to parse plugin manifest {path}: {message}")]
    ManifestParse { path: PathBuf, message: String },
    #[error("invalid plugin manifest: {0}")]
    InvalidManifest(String),
    #[error("failed to read plugin registry {path}: {message}")]
    RegistryRead { path: PathBuf, message: String },
    #[error("failed to parse plugin registry {path}: {message}")]
    RegistryParse { path: PathBuf, message: String },
    #[error("failed to write plugin registry {path}: {message}")]
    RegistryWrite { path: PathBuf, message: String },
    #[error("plugin '{0}' not found")]
    PluginNotFound(String),
    #[error("action '{0}' not found")]
    ActionNotFound(String),
    #[error("pane entrypoint '{entrypoint}' not found in plugin '{plugin_id}'")]
    PaneNotFound { plugin_id: String, entrypoint: String },
    #[error("plugin '{0}' is disabled")]
    PluginDisabled(String),
    #[error("plugin item '{item}' does not support platform '{platform}'")]
    PlatformUnsupported { item: String, platform: String },
    #[error("plugin command is empty")]
    EmptyCommand,
    #[error("invalid invocation context: {0}")]
    InvalidContext(String),
    #[error("failed to create plugin directory {path}: {message}")]
    DirectoryCreate { path: PathBuf, message: String },
    #[error("failed to spawn plugin command '{command}': {message}")]
    Spawn { command: String, message: String },
    #[error("plugin log store lock is poisoned")]
    LogStorePoisoned,
}

impl PluginError {
    pub fn error_code(&self) -> ErrorCode {
        match self {
            Self::PluginDisabled(_) => ErrorCode::PluginDisabled,
            Self::PlatformUnsupported { .. } => ErrorCode::PlatformUnsupported,
            Self::PluginNotFound(_) | Self::ActionNotFound(_) | Self::PaneNotFound { .. } => ErrorCode::NotFound,
            Self::InvalidManifest(_) | Self::EmptyCommand | Self::InvalidContext(_) => ErrorCode::InvalidParams,
            Self::ManifestRead { .. }
            | Self::ManifestParse { .. }
            | Self::RegistryRead { .. }
            | Self::RegistryParse { .. }
            | Self::RegistryWrite { .. }
            | Self::DirectoryCreate { .. }
            | Self::Spawn { .. }
            | Self::LogStorePoisoned => ErrorCode::Internal,
        }
    }

    pub fn into_api_error(self) -> ApiError {
        ApiError::new(self.error_code(), self.to_string())
    }
}

impl From<PluginError> for ApiError {
    fn from(error: PluginError) -> Self {
        error.into_api_error()
    }
}
