use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

use portable_pty::CommandBuilder;

use crate::TerminalError;

#[derive(Debug, Clone)]
pub struct PaneCommand {
    program: OsString,
    args: Vec<OsString>,
    cwd: Option<PathBuf>,
    environment: BTreeMap<OsString, OsString>,
    starcil_environment: BTreeMap<OsString, OsString>,
}

impl PaneCommand {
    pub fn new(program: impl Into<OsString>) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            cwd: None,
            environment: BTreeMap::new(),
            starcil_environment: BTreeMap::new(),
        }
    }

    pub fn arg(mut self, arg: impl Into<OsString>) -> Self {
        self.args.push(arg.into());
        self
    }

    pub fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        self.args.extend(args.into_iter().map(Into::into));
        self
    }

    pub fn cwd(mut self, cwd: impl Into<PathBuf>) -> Self {
        self.cwd = Some(cwd.into());
        self
    }

    pub fn env(mut self, key: impl Into<OsString>, value: impl Into<OsString>) -> Self {
        self.environment.insert(key.into(), value.into());
        self
    }

    pub fn starcil_env(
        mut self,
        key: impl Into<OsString>,
        value: impl Into<OsString>,
    ) -> Self {
        self.starcil_environment.insert(key.into(), value.into());
        self
    }

    pub(crate) fn into_builder(self) -> Result<CommandBuilder, TerminalError> {
        let prepared = prepare_environment(self.starcil_environment)?;
        let mut builder = CommandBuilder::new(&self.program);
        builder.args(self.args);
        if let Some(cwd) = self.cwd {
            builder.cwd(cwd);
        }

        for key in prepared.removed() {
            builder.env_remove(key);
        }
        for (key, value) in prepared.values() {
            builder.env(key, value);
        }
        for (key, value) in self.environment {
            if should_apply_caller_environment(&key) {
                builder.env(key, value);
            }
        }
        Ok(builder)
    }

    pub fn program(&self) -> &OsStr {
        &self.program
    }

    pub fn cwd_path(&self) -> Option<&Path> {
        self.cwd.as_deref()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedEnvironment {
    removed: Vec<OsString>,
    values: BTreeMap<OsString, OsString>,
}

impl PreparedEnvironment {
    pub fn removed(&self) -> &[OsString] {
        &self.removed
    }

    pub fn values(&self) -> &BTreeMap<OsString, OsString> {
        &self.values
    }
}

pub fn prepare_environment<I, K, V>(
    starcil_environment: I,
) -> Result<PreparedEnvironment, TerminalError>
where
    I: IntoIterator<Item = (K, V)>,
    K: Into<OsString>,
    V: Into<OsString>,
{
    prepare_environment_from(std::env::vars_os(), starcil_environment)
}

fn prepare_environment_from<P, PK, PV, I, K, V>(
    parent_environment: P,
    starcil_environment: I,
) -> Result<PreparedEnvironment, TerminalError>
where
    P: IntoIterator<Item = (PK, PV)>,
    PK: Into<OsString>,
    PV: Into<OsString>,
    I: IntoIterator<Item = (K, V)>,
    K: Into<OsString>,
    V: Into<OsString>,
{
    let parent: BTreeMap<OsString, OsString> = parent_environment
        .into_iter()
        .map(|(key, value)| (key.into(), value.into()))
        .collect();
    let removed = parent
        .keys()
        .filter(|key| should_scrub(key))
        .cloned()
        .collect();

    let mut values = BTreeMap::new();
    if !contains_key_ignore_ascii_case(&parent, "TERM") {
        values.insert(OsString::from("TERM"), OsString::from("xterm-256color"));
    }
    if !contains_key_ignore_ascii_case(&parent, "COLORTERM") {
        values.insert(OsString::from("COLORTERM"), OsString::from("truecolor"));
    }

    for (key, value) in starcil_environment {
        let key = key.into();
        if !key.to_string_lossy().starts_with("STARCIL_") {
            return Err(TerminalError::InvalidStarcilEnvironmentKey(
                key.to_string_lossy().into_owned(),
            ));
        }
        values.insert(key, value.into());
    }

    Ok(PreparedEnvironment { removed, values })
}

fn contains_key_ignore_ascii_case(
    environment: &BTreeMap<OsString, OsString>,
    expected: &str,
) -> bool {
    environment
        .keys()
        .any(|key| key.to_string_lossy().eq_ignore_ascii_case(expected))
}

fn should_scrub(key: &OsStr) -> bool {
    let key = key.to_string_lossy();
    key.eq_ignore_ascii_case("NO_COLOR")
        || key.to_ascii_uppercase().starts_with("CLAUDECODE")
        || key.to_ascii_uppercase().starts_with("CLAUDE_CODE_")
}

fn should_apply_caller_environment(key: &OsStr) -> bool {
    !should_scrub(key)
        && !key
            .to_string_lossy()
            .to_ascii_uppercase()
            .starts_with("STARCIL_")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn environment_scrubs_agent_markers_and_adds_terminal_defaults() {
        let parent = [
            ("NO_COLOR", "1"),
            ("CLAUDECODE", "1"),
            ("CLAUDE_CODE_ENTRYPOINT", "nested"),
            ("PATH", "bin"),
        ];
        let prepared = prepare_environment_from(
            parent,
            [
                ("STARCIL_ENV", "1"),
                ("STARCIL_PANE_ID", "w1:p1"),
            ],
        )
        .unwrap();

        let removed: Vec<_> = prepared
            .removed()
            .iter()
            .map(|key| key.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            removed,
            vec!["CLAUDECODE", "CLAUDE_CODE_ENTRYPOINT", "NO_COLOR"]
        );
        assert_eq!(prepared.values()[OsStr::new("TERM")], "xterm-256color");
        assert_eq!(prepared.values()[OsStr::new("COLORTERM")], "truecolor");
        assert_eq!(prepared.values()[OsStr::new("STARCIL_ENV")], "1");
    }

    #[test]
    fn environment_preserves_existing_terminal_capabilities() {
        let prepared = prepare_environment_from(
            [("TERM", "xterm-kitty"), ("COLORTERM", "24bit")],
            [("STARCIL_ENV", "1")],
        )
        .unwrap();
        assert!(!prepared.values().contains_key(OsStr::new("TERM")));
        assert!(!prepared.values().contains_key(OsStr::new("COLORTERM")));
    }

    #[test]
    fn environment_rejects_non_starcil_injected_keys() {
        let error = prepare_environment_from(
            std::iter::empty::<(&str, &str)>(),
            [("OTHER", "value")],
        )
        .unwrap_err();
        assert!(matches!(
            error,
            TerminalError::InvalidStarcilEnvironmentKey(key) if key == "OTHER"
        ));
    }

    #[test]
    fn caller_environment_cannot_override_managed_starcil_values() {
        assert!(!should_apply_caller_environment(OsStr::new(
            "STARCIL_SOCKET_PATH"
        )));
        assert!(!should_apply_caller_environment(OsStr::new(
            "starcil_pane_id"
        )));
        assert!(should_apply_caller_environment(OsStr::new("PATH")));
    }
}
