use crate::{
    types::{read_optional, write_file},
    InstallReport, Integration, IntegrationError, IntegrationKind, IntegrationStatus,
    INTEGRATION_VERSION,
};
use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};

const SETTINGS_RELATIVE_PATH: &str = ".claude/settings.json";
const BACKUP_FILE_NAME: &str = "settings.json.starcil-bak";
const MARKER: &str = "starcil_integration";

pub const CLAUDE_HOOK_COMMANDS: [(&str, &str); 3] = [
    (
        "Notification",
        "starcil integration hook claude-notification",
    ),
    ("Stop", "starcil integration hook claude-stop"),
    (
        "SessionStart",
        "starcil integration hook claude-session-start",
    ),
];

#[derive(Debug, Clone, Copy, Default)]
pub struct ClaudeIntegration;

impl Integration for ClaudeIntegration {
    fn id(&self) -> &'static str {
        "claude"
    }

    fn display_name(&self) -> &'static str {
        "Claude Code"
    }

    fn kind(&self) -> IntegrationKind {
        IntegrationKind::LifecycleAuthority
    }

    fn install(&self, home: &Path) -> Result<InstallReport, IntegrationError> {
        let (config_directory, settings_path, backup_path) = paths(home);
        if !config_directory.is_dir() {
            return Err(IntegrationError::MissingConfigDirectory(config_directory));
        }
        let original = read_optional(&settings_path)?.unwrap_or_else(|| b"{}\n".to_vec());
        let mut settings = parse_settings(&settings_path, &original)?;
        install_managed_hooks(&settings_path, &mut settings)?;
        let updated = serialize_settings(&settings)?;
        let changed = updated != original;
        if changed {
            write_file(&backup_path, &original)?;
            write_file(&settings_path, &updated)?;
        }
        Ok(InstallReport {
            integration_id: self.id().to_owned(),
            changed,
            paths: vec![settings_path],
            backup: changed.then_some(backup_path),
        })
    }

    fn uninstall(&self, home: &Path) -> Result<InstallReport, IntegrationError> {
        let (_, settings_path, backup_path) = paths(home);
        let Some(original) = read_optional(&settings_path)? else {
            return Ok(InstallReport {
                integration_id: self.id().to_owned(),
                changed: false,
                paths: vec![settings_path],
                backup: None,
            });
        };
        let mut settings = parse_settings(&settings_path, &original)?;
        let removed = remove_managed_hooks(&settings_path, &mut settings)?;
        if removed {
            let updated = serialize_settings(&settings)?;
            write_file(&backup_path, &original)?;
            write_file(&settings_path, &updated)?;
        }
        Ok(InstallReport {
            integration_id: self.id().to_owned(),
            changed: removed,
            paths: vec![settings_path],
            backup: removed.then_some(backup_path),
        })
    }

    fn status(&self, home: &Path) -> Result<IntegrationStatus, IntegrationError> {
        let (_, settings_path, _) = paths(home);
        let Some(bytes) = read_optional(&settings_path)? else {
            return Ok(not_installed_status(self.id()));
        };
        let settings = parse_settings(&settings_path, &bytes)?;
        let versions = managed_versions(&settings);
        let installed = !versions.is_empty();
        let version = versions
            .first()
            .filter(|first| versions.iter().all(|version| version == *first))
            .cloned();
        let outdated = installed
            && (versions.len() != CLAUDE_HOOK_COMMANDS.len()
                || versions
                    .iter()
                    .any(|version| version != INTEGRATION_VERSION));
        Ok(IntegrationStatus {
            integration_id: self.id().to_owned(),
            supported: true,
            installed,
            version,
            outdated,
        })
    }
}

fn paths(home: &Path) -> (PathBuf, PathBuf, PathBuf) {
    let settings_path = home.join(SETTINGS_RELATIVE_PATH);
    let config_directory = settings_path
        .parent()
        .expect("settings path always has a parent")
        .to_owned();
    let backup_path = config_directory.join(BACKUP_FILE_NAME);
    (config_directory, settings_path, backup_path)
}

fn parse_settings(path: &Path, bytes: &[u8]) -> Result<Value, IntegrationError> {
    let settings = if bytes.iter().all(u8::is_ascii_whitespace) {
        json!({})
    } else {
        serde_json::from_slice(bytes)?
    };
    if !settings.is_object() {
        return Err(IntegrationError::ExpectedJsonObject(path.to_owned()));
    }
    Ok(settings)
}

fn install_managed_hooks(path: &Path, settings: &mut Value) -> Result<(), IntegrationError> {
    let root = settings
        .as_object_mut()
        .ok_or_else(|| IntegrationError::ExpectedJsonObject(path.to_owned()))?;
    let hooks = hooks_object(path, root, true)?.expect("created hooks object");
    for (event, command) in CLAUDE_HOOK_COMMANDS {
        let entries = hooks.entry(event).or_insert_with(|| json!([]));
        let entries = entries
            .as_array_mut()
            .ok_or_else(|| IntegrationError::InvalidStructure {
                path: path.to_owned(),
                field: format!("hooks.{event}"),
            })?;
        entries.retain(|entry| !is_managed(entry));
        entries.push(managed_hook(command));
    }
    Ok(())
}

fn remove_managed_hooks(path: &Path, settings: &mut Value) -> Result<bool, IntegrationError> {
    let root = settings
        .as_object_mut()
        .ok_or_else(|| IntegrationError::ExpectedJsonObject(path.to_owned()))?;
    let Some(hooks) = hooks_object(path, root, false)? else {
        return Ok(false);
    };
    let mut removed = false;
    let mut empty_events = Vec::new();
    for (event, _) in CLAUDE_HOOK_COMMANDS {
        let Some(entries) = hooks.get_mut(event) else {
            continue;
        };
        let entries = entries
            .as_array_mut()
            .ok_or_else(|| IntegrationError::InvalidStructure {
                path: path.to_owned(),
                field: format!("hooks.{event}"),
            })?;
        let before = entries.len();
        entries.retain(|entry| !is_managed(entry));
        removed |= entries.len() != before;
        if entries.is_empty() {
            empty_events.push(event);
        }
    }
    for event in empty_events {
        hooks.remove(event);
    }
    if hooks.is_empty() {
        root.remove("hooks");
    }
    Ok(removed)
}

fn hooks_object<'a>(
    path: &Path,
    root: &'a mut Map<String, Value>,
    create: bool,
) -> Result<Option<&'a mut Map<String, Value>>, IntegrationError> {
    if !root.contains_key("hooks") {
        if !create {
            return Ok(None);
        }
        root.insert("hooks".to_owned(), json!({}));
    }
    root.get_mut("hooks")
        .and_then(Value::as_object_mut)
        .map(Some)
        .ok_or_else(|| IntegrationError::InvalidStructure {
            path: path.to_owned(),
            field: "hooks".to_owned(),
        })
}

fn managed_hook(command: &str) -> Value {
    json!({
        "matcher": "",
        MARKER: INTEGRATION_VERSION,
        "hooks": [{
            "type": "command",
            "command": command,
            "timeout": 10
        }]
    })
}

fn is_managed(entry: &Value) -> bool {
    entry.get(MARKER).and_then(Value::as_str).is_some()
}

fn managed_versions(settings: &Value) -> Vec<String> {
    let Some(hooks) = settings.get("hooks").and_then(Value::as_object) else {
        return Vec::new();
    };
    CLAUDE_HOOK_COMMANDS
        .iter()
        .flat_map(|(event, _)| {
            hooks
                .get(*event)
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|entry| entry.get(MARKER).and_then(Value::as_str))
                .map(str::to_owned)
        })
        .collect()
}

fn serialize_settings(settings: &Value) -> Result<Vec<u8>, IntegrationError> {
    let mut bytes = serde_json::to_vec_pretty(settings)?;
    bytes.push(b'\n');
    Ok(bytes)
}

fn not_installed_status(id: &str) -> IntegrationStatus {
    IntegrationStatus {
        integration_id: id.to_owned(),
        supported: true,
        installed: false,
        version: None,
        outdated: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn setup_home() -> tempfile::TempDir {
        let home = tempfile::tempdir().unwrap();
        fs::create_dir(home.path().join(".claude")).unwrap();
        home
    }

    #[test]
    fn install_is_non_destructive_idempotent_and_uninstall_is_scoped() {
        let home = setup_home();
        let settings_path = home.path().join(SETTINGS_RELATIVE_PATH);
        let user_hook = json!({
            "matcher": "permission_prompt",
            "hooks": [{"type": "command", "command": "user-hook"}]
        });
        let fixture = json!({
            "theme": "dark",
            "hooks": {"Notification": [user_hook.clone()]}
        });
        fs::write(
            &settings_path,
            format!("{}\n", serde_json::to_string_pretty(&fixture).unwrap()),
        )
        .unwrap();

        let integration = ClaudeIntegration;
        let first = integration.install(home.path()).unwrap();
        assert!(first.changed);
        assert!(home
            .path()
            .join(".claude/settings.json.starcil-bak")
            .is_file());
        let installed_bytes = fs::read(&settings_path).unwrap();
        let installed: Value = serde_json::from_slice(&installed_bytes).unwrap();
        assert_eq!(
            installed["hooks"]["Notification"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|entry| entry == &&user_hook)
                .count(),
            1
        );
        assert_eq!(managed_versions(&installed).len(), 3);

        let second = integration.install(home.path()).unwrap();
        assert!(!second.changed);
        assert_eq!(fs::read(&settings_path).unwrap(), installed_bytes);

        let removed = integration.uninstall(home.path()).unwrap();
        assert!(removed.changed);
        let after: Value = serde_json::from_slice(&fs::read(&settings_path).unwrap()).unwrap();
        assert_eq!(after["theme"], "dark");
        assert_eq!(after["hooks"]["Notification"], json!([user_hook]));
        assert!(after["hooks"].get("Stop").is_none());
        assert!(after["hooks"].get("SessionStart").is_none());
    }

    #[test]
    fn status_detects_an_older_stamp() {
        let home = setup_home();
        let integration = ClaudeIntegration;
        integration.install(home.path()).unwrap();
        let settings_path = home.path().join(SETTINGS_RELATIVE_PATH);
        let mut settings: Value = serde_json::from_slice(&fs::read(&settings_path).unwrap()).unwrap();
        for entries in settings["hooks"].as_object_mut().unwrap().values_mut() {
            for entry in entries.as_array_mut().unwrap() {
                if is_managed(entry) {
                    entry[MARKER] = json!("0.0.1");
                }
            }
        }
        fs::write(&settings_path, serialize_settings(&settings).unwrap()).unwrap();
        let status = integration.status(home.path()).unwrap();
        assert!(status.installed);
        assert!(status.outdated);
        assert_eq!(status.version.as_deref(), Some("0.0.1"));
    }
}
