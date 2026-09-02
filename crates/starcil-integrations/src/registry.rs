use crate::{
    ClaudeIntegration, CodexIntegration, InstallReport, Integration, IntegrationError,
    IntegrationKind, IntegrationStatus, INTEGRATION_VERSION,
};
use std::path::Path;

#[derive(Debug, Clone, Copy)]
pub enum RegisteredIntegration {
    Claude(ClaudeIntegration),
    Codex(CodexIntegration),
    Unsupported(UnsupportedIntegration),
}

#[derive(Debug, Clone, Copy)]
pub struct UnsupportedIntegration {
    id: &'static str,
    display_name: &'static str,
}

impl Integration for RegisteredIntegration {
    fn id(&self) -> &'static str {
        match self {
            Self::Claude(integration) => integration.id(),
            Self::Codex(integration) => integration.id(),
            Self::Unsupported(integration) => integration.id(),
        }
    }

    fn display_name(&self) -> &'static str {
        match self {
            Self::Claude(integration) => integration.display_name(),
            Self::Codex(integration) => integration.display_name(),
            Self::Unsupported(integration) => integration.display_name(),
        }
    }

    fn kind(&self) -> IntegrationKind {
        match self {
            Self::Claude(integration) => integration.kind(),
            Self::Codex(integration) => integration.kind(),
            Self::Unsupported(integration) => integration.kind(),
        }
    }

    fn install(&self, home: &Path) -> Result<InstallReport, IntegrationError> {
        match self {
            Self::Claude(integration) => integration.install(home),
            Self::Codex(integration) => integration.install(home),
            Self::Unsupported(integration) => integration.install(home),
        }
    }

    fn uninstall(&self, home: &Path) -> Result<InstallReport, IntegrationError> {
        match self {
            Self::Claude(integration) => integration.uninstall(home),
            Self::Codex(integration) => integration.uninstall(home),
            Self::Unsupported(integration) => integration.uninstall(home),
        }
    }

    fn status(&self, home: &Path) -> Result<IntegrationStatus, IntegrationError> {
        match self {
            Self::Claude(integration) => integration.status(home),
            Self::Codex(integration) => integration.status(home),
            Self::Unsupported(integration) => integration.status(home),
        }
    }
}

impl Integration for UnsupportedIntegration {
    fn id(&self) -> &'static str {
        self.id
    }

    fn display_name(&self) -> &'static str {
        self.display_name
    }

    fn kind(&self) -> IntegrationKind {
        IntegrationKind::UnsupportedYet
    }

    fn install(&self, _home: &Path) -> Result<InstallReport, IntegrationError> {
        Err(not_supported(self.id))
    }

    fn uninstall(&self, _home: &Path) -> Result<InstallReport, IntegrationError> {
        Err(not_supported(self.id))
    }

    fn status(&self, _home: &Path) -> Result<IntegrationStatus, IntegrationError> {
        Ok(IntegrationStatus {
            integration_id: self.id.to_owned(),
            supported: false,
            installed: false,
            version: None,
            outdated: false,
        })
    }
}

const UNSUPPORTED: [(&str, &str); 14] = [
    ("pi", "Pi"),
    ("omp", "OMP"),
    ("copilot", "GitHub Copilot CLI"),
    ("devin", "Devin CLI"),
    ("droid", "Droid"),
    ("kimi", "Kimi Code CLI"),
    ("opencode", "OpenCode"),
    ("kilo", "Kilo Code CLI"),
    ("hermes", "Hermes Agent"),
    ("qodercli", "Qoder CLI"),
    ("cursor", "Cursor Agent CLI"),
    ("mastracode", "MastraCode"),
    ("antigravity-cli", "Antigravity CLI"),
    ("grok", "Grok CLI"),
];

pub fn registry() -> Vec<RegisteredIntegration> {
    let mut integrations = Vec::with_capacity(16);
    integrations.push(RegisteredIntegration::Claude(ClaudeIntegration));
    integrations.push(RegisteredIntegration::Codex(CodexIntegration));
    integrations.extend(UNSUPPORTED.map(|(id, display_name)| {
        RegisteredIntegration::Unsupported(UnsupportedIntegration { id, display_name })
    }));
    integrations
}

pub fn integration(id: &str) -> Option<RegisteredIntegration> {
    registry()
        .into_iter()
        .find(|integration| integration.id().eq_ignore_ascii_case(id.trim()))
}

/// Returns the executable followed by its argv template with the session reference rendered.
pub fn resume_command(agent: &str, session_reference: &str) -> Option<Vec<String>> {
    let spec = starcil_agents::integration_spec(agent)?;
    let command = spec.resume_command(session_reference)?;
    let mut argv = Vec::with_capacity(command.args.len() + 1);
    argv.push(command.program);
    argv.extend(command.args);
    Some(argv)
}

fn not_supported(id: &'static str) -> IntegrationError {
    IntegrationError::NotYetSupported {
        integration_id: id,
        version: INTEGRATION_VERSION,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_is_complete_and_only_real_integrations_claim_support() {
        let integrations = registry();
        assert_eq!(integrations.len(), 16);
        assert_eq!(
            integrations
                .iter()
                .filter(|integration| integration.kind() != IntegrationKind::UnsupportedYet)
                .count(),
            2
        );
        let home = tempfile::tempdir().unwrap();
        let pi = integration("pi").unwrap();
        let status = pi.status(home.path()).unwrap();
        assert!(!status.supported);
        assert!(!status.installed);
        assert!(pi
            .install(home.path())
            .unwrap_err()
            .to_string()
            .contains("not yet supported in starcil"));
    }

    #[test]
    fn resume_table_renders_native_argv() {
        assert_eq!(
            resume_command("claude", "session-1").unwrap(),
            ["claude", "--resume", "session-1"]
        );
        assert_eq!(
            resume_command("codex", "thread-2").unwrap(),
            ["codex", "resume", "thread-2"]
        );
        assert_eq!(
            resume_command("omp", "session-3").unwrap(),
            ["omp", "--resume=session-3"]
        );
        assert!(resume_command("unknown", "session").is_none());
    }
}
