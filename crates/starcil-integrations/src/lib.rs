//! Installable agent hooks and native session-resume metadata.

mod claude;
mod codex;
mod registry;
mod types;

pub use claude::{ClaudeIntegration, CLAUDE_HOOK_COMMANDS};
pub use codex::{CodexIntegration, CodexNotifyPayload, CODEX_NOTIFY_COMMAND};
pub use registry::{
    integration, registry, resume_command, RegisteredIntegration, UnsupportedIntegration,
};
pub use types::{
    InstallReport, Integration, IntegrationError, IntegrationKind, IntegrationStatus,
    INTEGRATION_VERSION,
};
