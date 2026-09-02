use crate::{
    types::{read_optional, remove_optional, write_file},
    InstallReport, Integration, IntegrationError, IntegrationKind, IntegrationStatus,
    INTEGRATION_VERSION,
};
use serde::Deserialize;
use serde_json::{Map as JsonMap, Value as JsonValue};
use std::path::{Path, PathBuf};
use toml_edit::{value, Array, DocumentMut, Item};

const CONFIG_RELATIVE_PATH: &str = ".codex/config.toml";
const BACKUP_FILE_NAME: &str = "config.toml.starcil-bak";
const VERSION_FILE_NAME: &str = "starcil-notify-version";

pub const CODEX_NOTIFY_COMMAND: [&str; 4] = ["starcil", "integration", "hook", "codex-notify"];

/// Codex appends this JSON as one final argv value after the configured `notify` command.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct CodexNotifyPayload {
    #[serde(rename = "type")]
    pub event_type: String,
    #[serde(rename = "thread-id")]
    pub thread_id: Option<String>,
    #[serde(rename = "turn-id")]
    pub turn_id: Option<String>,
    #[serde(rename = "input-messages", default)]
    pub input_messages: Vec<String>,
    #[serde(rename = "last-assistant-message")]
    pub last_assistant_message: Option<String>,
    #[serde(flatten)]
    pub additional_fields: JsonMap<String, JsonValue>,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct CodexIntegration;

impl Integration for CodexIntegration {
    fn id(&self) -> &'static str {
        "codex"
    }

    fn display_name(&self) -> &'static str {
        "Codex"
    }

    fn kind(&self) -> IntegrationKind {
        IntegrationKind::SessionIdentity
    }

    fn install(&self, home: &Path) -> Result<InstallReport, IntegrationError> {
        let (config_directory, config_path, backup_path, version_path) = paths(home);
        if !config_directory.is_dir() {
            return Err(IntegrationError::MissingConfigDirectory(config_directory));
        }
        let original = read_optional(&config_path)?.unwrap_or_default();
        let mut document = parse_document(&original)?;
        let config_changed = !notify_is_managed(&document);
        let current_stamp = read_optional(&version_path)?;
        let stamp_changed = current_stamp
            .as_deref()
            .map(|bytes| String::from_utf8_lossy(bytes).trim() != INTEGRATION_VERSION)
            .unwrap_or(true);
        let mut backup = None;

        if config_changed {
            if !backup_path.exists() {
                write_file(&backup_path, &original)?;
                backup = Some(backup_path.clone());
            }
            set_notify(&mut document);
            write_file(&config_path, document.to_string().as_bytes())?;
        }
        if stamp_changed {
            write_file(&version_path, format!("{INTEGRATION_VERSION}\n").as_bytes())?;
        }

        Ok(InstallReport {
            integration_id: self.id().to_owned(),
            changed: config_changed || stamp_changed,
            paths: vec![config_path, version_path],
            backup,
        })
    }

    fn uninstall(&self, home: &Path) -> Result<InstallReport, IntegrationError> {
        let (_, config_path, backup_path, version_path) = paths(home);
        let mut changed = false;
        if let Some(current) = read_optional(&config_path)? {
            let mut document = parse_document(&current)?;
            if notify_is_managed(&document) {
                if let Some(original) = read_optional(&backup_path)? {
                    let original_document = parse_document(&original)?;
                    if let Some(previous_notify) = original_document.get("notify") {
                        document["notify"] = previous_notify.clone();
                    } else {
                        document.remove("notify");
                    }
                } else {
                    document.remove("notify");
                }
                let updated = document.to_string().into_bytes();
                if updated != current {
                    write_file(&config_path, &updated)?;
                    changed = true;
                }
            }
        }
        changed |= remove_optional(&version_path)?;
        changed |= remove_optional(&backup_path)?;
        Ok(InstallReport {
            integration_id: self.id().to_owned(),
            changed,
            paths: vec![config_path, version_path],
            backup: None,
        })
    }

    fn status(&self, home: &Path) -> Result<IntegrationStatus, IntegrationError> {
        let (_, config_path, _, version_path) = paths(home);
        let installed = match read_optional(&config_path)? {
            Some(bytes) => notify_is_managed(&parse_document(&bytes)?),
            None => false,
        };
        let version = read_optional(&version_path)?
            .map(String::from_utf8)
            .transpose()?
            .map(|version| version.trim().to_owned())
            .filter(|version| !version.is_empty());
        let outdated = installed && version.as_deref() != Some(INTEGRATION_VERSION);
        Ok(IntegrationStatus {
            integration_id: self.id().to_owned(),
            supported: true,
            installed,
            version,
            outdated,
        })
    }
}

fn paths(home: &Path) -> (PathBuf, PathBuf, PathBuf, PathBuf) {
    let config_path = home.join(CONFIG_RELATIVE_PATH);
    let config_directory = config_path
        .parent()
        .expect("config path always has a parent")
        .to_owned();
    (
        config_directory.clone(),
        config_path,
        config_directory.join(BACKUP_FILE_NAME),
        config_directory.join(VERSION_FILE_NAME),
    )
}

fn parse_document(bytes: &[u8]) -> Result<DocumentMut, IntegrationError> {
    let text = String::from_utf8(bytes.to_vec())?;
    if text.trim().is_empty() {
        Ok(DocumentMut::new())
    } else {
        Ok(text.parse::<DocumentMut>()?)
    }
}

fn set_notify(document: &mut DocumentMut) {
    let mut command = Array::new();
    for argument in CODEX_NOTIFY_COMMAND {
        command.push(argument);
    }
    document["notify"] = value(command);
}

fn notify_is_managed(document: &DocumentMut) -> bool {
    let Some(command) = document.get("notify").and_then(Item::as_array) else {
        return false;
    };
    command.len() == CODEX_NOTIFY_COMMAND.len()
        && command
            .iter()
            .zip(CODEX_NOTIFY_COMMAND)
            .all(|(actual, expected)| actual.as_str() == Some(expected))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn setup_home() -> tempfile::TempDir {
        let home = tempfile::tempdir().unwrap();
        fs::create_dir(home.path().join(".codex")).unwrap();
        home
    }

    #[test]
    fn install_and_uninstall_round_trip_preserves_comments_and_previous_notify() {
        let home = setup_home();
        let config_path = home.path().join(CONFIG_RELATIVE_PATH);
        let fixture = concat!(
            "# user header\n",
            "notify = [\"desktop-notifier\", \"--quiet\"] # keep this notify\n\n",
            "[model_providers.local]\n",
            "# provider comment\n",
            "name = \"Local\"\n",
        );
        fs::write(&config_path, fixture).unwrap();
        let integration = CodexIntegration;

        let installed = integration.install(home.path()).unwrap();
        assert!(installed.changed);
        let installed_text = fs::read_to_string(&config_path).unwrap();
        assert!(installed_text.contains("# user header"));
        assert!(installed_text.contains("# provider comment"));
        assert!(notify_is_managed(&parse_document(installed_text.as_bytes()).unwrap()));
        let status = integration.status(home.path()).unwrap();
        assert!(status.installed);
        assert!(!status.outdated);

        let second = integration.install(home.path()).unwrap();
        assert!(!second.changed);
        let removed = integration.uninstall(home.path()).unwrap();
        assert!(removed.changed);
        assert_eq!(fs::read_to_string(&config_path).unwrap(), fixture);
        assert!(!home
            .path()
            .join(".codex/config.toml.starcil-bak")
            .exists());
    }

    #[test]
    fn payload_parses_documented_turn_and_thread_fields() {
        let payload: CodexNotifyPayload = serde_json::from_str(
            r#"{
                "type":"agent-turn-complete",
                "thread-id":"thread-1",
                "turn-id":"turn-2",
                "input-messages":["Fix it"],
                "last-assistant-message":"Done"
            }"#,
        )
        .unwrap();
        assert_eq!(payload.event_type, "agent-turn-complete");
        assert_eq!(payload.thread_id.as_deref(), Some("thread-1"));
        assert_eq!(payload.turn_id.as_deref(), Some("turn-2"));
    }
}
