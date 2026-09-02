use crate::AgentKind;
use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum IntegrationRole {
    LifecycleAndSession,
    SessionOnly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ResumeTemplate {
    pub executable: &'static str,
    pub arguments: &'static [&'static str],
}

impl ResumeTemplate {
    pub fn render(self, session_reference: &str) -> Option<ResumeCommand> {
        if session_reference.is_empty() {
            return None;
        }
        Some(ResumeCommand {
            program: self.executable.to_owned(),
            args: self
                .arguments
                .iter()
                .map(|argument| argument.replace("{session}", session_reference))
                .collect(),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ResumeCommand {
    pub program: String,
    pub args: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct IntegrationSpec {
    pub name: &'static str,
    pub kind: AgentKind,
    pub role: IntegrationRole,
    pub minimum_restore_version: u32,
    pub resume: ResumeTemplate,
}

impl IntegrationSpec {
    pub const fn authors_lifecycle(self) -> bool {
        matches!(self.role, IntegrationRole::LifecycleAndSession)
    }

    pub fn resume_command(self, session_reference: &str) -> Option<ResumeCommand> {
        self.resume.render(session_reference)
    }
}

const PI_RESUME: &[&str] = &["--session", "{session}"];
const OMP_RESUME: &[&str] = &["--resume={session}"];
const CLAUDE_RESUME: &[&str] = &["--resume", "{session}"];
const CODEX_RESUME: &[&str] = &["resume", "{session}"];
const COPILOT_RESUME: &[&str] = &["--resume={session}"];
const DEVIN_RESUME: &[&str] = &["--resume", "{session}"];
const DROID_RESUME: &[&str] = &["--resume", "{session}"];
const KIMI_RESUME: &[&str] = &["--session", "{session}"];
const OPENCODE_RESUME: &[&str] = &["--session", "{session}"];
const KILO_RESUME: &[&str] = &["--session", "{session}"];
const HERMES_RESUME: &[&str] = &["--resume", "{session}"];
const QODER_RESUME: &[&str] = &["--resume", "{session}"];
const CURSOR_RESUME: &[&str] = &["--resume", "{session}"];
const MASTRACODE_RESUME: &[&str] = &["--thread", "{session}"];
const AGY_RESUME: &[&str] = &["--conversation", "{session}"];
const GROK_RESUME: &[&str] = &["--resume", "{session}"];

pub const INTEGRATIONS: [IntegrationSpec; 16] = [
    IntegrationSpec {
        name: "pi",
        kind: AgentKind::Pi,
        role: IntegrationRole::LifecycleAndSession,
        minimum_restore_version: 2,
        resume: ResumeTemplate {
            executable: "pi",
            arguments: PI_RESUME,
        },
    },
    IntegrationSpec {
        name: "omp",
        kind: AgentKind::Omp,
        role: IntegrationRole::LifecycleAndSession,
        minimum_restore_version: 3,
        resume: ResumeTemplate {
            executable: "omp",
            arguments: OMP_RESUME,
        },
    },
    IntegrationSpec {
        name: "claude",
        kind: AgentKind::Claude,
        role: IntegrationRole::SessionOnly,
        minimum_restore_version: 6,
        resume: ResumeTemplate {
            executable: "claude",
            arguments: CLAUDE_RESUME,
        },
    },
    IntegrationSpec {
        name: "codex",
        kind: AgentKind::Codex,
        role: IntegrationRole::SessionOnly,
        minimum_restore_version: 5,
        resume: ResumeTemplate {
            executable: "codex",
            arguments: CODEX_RESUME,
        },
    },
    IntegrationSpec {
        name: "copilot",
        kind: AgentKind::Copilot,
        role: IntegrationRole::SessionOnly,
        minimum_restore_version: 2,
        resume: ResumeTemplate {
            executable: "copilot",
            arguments: COPILOT_RESUME,
        },
    },
    IntegrationSpec {
        name: "devin",
        kind: AgentKind::Devin,
        role: IntegrationRole::SessionOnly,
        minimum_restore_version: 2,
        resume: ResumeTemplate {
            executable: "devin",
            arguments: DEVIN_RESUME,
        },
    },
    IntegrationSpec {
        name: "droid",
        kind: AgentKind::Droid,
        role: IntegrationRole::SessionOnly,
        minimum_restore_version: 2,
        resume: ResumeTemplate {
            executable: "droid",
            arguments: DROID_RESUME,
        },
    },
    IntegrationSpec {
        name: "kimi",
        kind: AgentKind::Kimi,
        role: IntegrationRole::LifecycleAndSession,
        minimum_restore_version: 3,
        resume: ResumeTemplate {
            executable: "kimi",
            arguments: KIMI_RESUME,
        },
    },
    IntegrationSpec {
        name: "opencode",
        kind: AgentKind::Opencode,
        role: IntegrationRole::LifecycleAndSession,
        minimum_restore_version: 5,
        resume: ResumeTemplate {
            executable: "opencode",
            arguments: OPENCODE_RESUME,
        },
    },
    IntegrationSpec {
        name: "kilo",
        kind: AgentKind::Kilo,
        role: IntegrationRole::LifecycleAndSession,
        minimum_restore_version: 1,
        resume: ResumeTemplate {
            executable: "kilo",
            arguments: KILO_RESUME,
        },
    },
    IntegrationSpec {
        name: "hermes",
        kind: AgentKind::Hermes,
        role: IntegrationRole::SessionOnly,
        minimum_restore_version: 2,
        resume: ResumeTemplate {
            executable: "hermes",
            arguments: HERMES_RESUME,
        },
    },
    IntegrationSpec {
        name: "qodercli",
        kind: AgentKind::Qodercli,
        role: IntegrationRole::SessionOnly,
        minimum_restore_version: 2,
        resume: ResumeTemplate {
            executable: "qodercli",
            arguments: QODER_RESUME,
        },
    },
    IntegrationSpec {
        name: "cursor",
        kind: AgentKind::Cursor,
        role: IntegrationRole::SessionOnly,
        minimum_restore_version: 1,
        resume: ResumeTemplate {
            executable: "cursor-agent",
            arguments: CURSOR_RESUME,
        },
    },
    IntegrationSpec {
        name: "mastracode",
        kind: AgentKind::Mastracode,
        role: IntegrationRole::LifecycleAndSession,
        minimum_restore_version: 1,
        resume: ResumeTemplate {
            executable: "mastracode",
            arguments: MASTRACODE_RESUME,
        },
    },
    IntegrationSpec {
        name: "antigravity-cli",
        kind: AgentKind::Agy,
        role: IntegrationRole::SessionOnly,
        minimum_restore_version: 1,
        resume: ResumeTemplate {
            executable: "agy",
            arguments: AGY_RESUME,
        },
    },
    IntegrationSpec {
        name: "grok",
        kind: AgentKind::Grok,
        role: IntegrationRole::SessionOnly,
        minimum_restore_version: 1,
        resume: ResumeTemplate {
            executable: "grok",
            arguments: GROK_RESUME,
        },
    },
];

pub fn integration_spec(name: &str) -> Option<&'static IntegrationSpec> {
    INTEGRATIONS
        .iter()
        .find(|integration| integration.name.eq_ignore_ascii_case(name.trim()))
}

pub fn integration_for_kind(kind: AgentKind) -> Option<&'static IntegrationSpec> {
    INTEGRATIONS
        .iter()
        .find(|integration| integration.kind == kind)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn integration_catalog_has_sixteen_unique_entries_and_six_lifecycle_authorities() {
        assert_eq!(INTEGRATIONS.len(), 16);
        let names: HashSet<&str> = INTEGRATIONS.iter().map(|spec| spec.name).collect();
        assert_eq!(names.len(), INTEGRATIONS.len());
        assert_eq!(
            INTEGRATIONS
                .iter()
                .filter(|spec| spec.authors_lifecycle())
                .count(),
            6
        );
        assert!(!integration_for_kind(AgentKind::Claude)
            .unwrap()
            .authors_lifecycle());
        assert!(integration_for_kind(AgentKind::Kimi)
            .unwrap()
            .authors_lifecycle());
    }

    #[test]
    fn every_resume_template_renders_without_placeholders() {
        for integration in INTEGRATIONS {
            let command = integration.resume_command("session-42").unwrap();
            assert!(!command.program.is_empty());
            assert!(command.args.iter().all(|arg| !arg.contains("{session}")));
            assert!(command.args.iter().any(|arg| arg.contains("session-42")));
        }
    }

    #[test]
    fn documented_resume_shapes_are_preserved() {
        assert_eq!(
            integration_spec("omp").unwrap().resume_command("abc").unwrap().args,
            ["--resume=abc"]
        );
        assert_eq!(
            integration_spec("codex")
                .unwrap()
                .resume_command("abc")
                .unwrap()
                .args,
            ["resume", "abc"]
        );
        assert_eq!(
            integration_spec("mastracode")
                .unwrap()
                .resume_command("abc")
                .unwrap().args,
            ["--thread", "abc"]
        );
    }
}
