use semver::Version;
use serde::Serialize;
use starcil_config::{Severity, UpdateChannel};
use starcil_update::{apply, ApplyOutcome, Channel, HttpClient, UpdateError, Updater};
use std::collections::BTreeSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use toml_edit::{value, DocumentMut, Item, Table, Value as TomlValue};

pub(crate) fn resolved_config_path() -> io::Result<PathBuf> {
    starcil_config::config_path()
        .or_else(starcil_config::default_config_path)
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "could not determine the Starcil config path"))
}

pub fn configured_channel(path: &Path) -> io::Result<Channel> {
    let report = starcil_config::load(path);
    if let Some(error) = report.diagnostics.iter().find(|diagnostic| diagnostic.severity == Severity::Error) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{}: {}", error.toml_path(), error.message),
        ));
    }
    Ok(match report.config.update.channel {
        UpdateChannel::Stable => Channel::Stable,
        UpdateChannel::Preview => Channel::Preview,
    })
}

pub fn set_channel_at(path: &Path, channel: Channel) -> io::Result<()> {
    let source = match fs::read_to_string(path) {
        Ok(source) => source,
        Err(error) if error.kind() == io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(error),
    };
    let mut document = DocumentMut::from_str(&source)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;

    if !document.as_table().contains_key("update") {
        document["update"] = Item::Table(Table::new());
    }
    match document.get_mut("update") {
        Some(Item::Table(update)) => {
            if let Some(Item::Value(existing)) = update.get_mut("channel") {
                replace_toml_value_preserving_decor(existing, channel.to_string());
            } else {
                update.insert("channel", value(channel.to_string()));
            }
        }
        Some(Item::Value(TomlValue::InlineTable(update))) => {
            if let Some(existing) = update.get_mut("channel") {
                replace_toml_value_preserving_decor(existing, channel.to_string());
            } else {
                update.insert("channel", TomlValue::from(channel.to_string()));
            }
        }
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "[update] must be a TOML table",
            ));
        }
    }

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, document.to_string())
}

fn replace_toml_value_preserving_decor(existing: &mut TomlValue, replacement: String) {
    let decor = existing.decor().clone();
    let mut replacement = TomlValue::from(replacement);
    *replacement.decor_mut() = decor;
    *existing = replacement;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SessionInfo {
    pub name: String,
    pub state_dir: Option<PathBuf>,
    pub runtime_dir: Option<PathBuf>,
    pub running: bool,
}

pub fn session_state_root(paths: &starcil_platform::PlatformPaths) -> PathBuf {
    paths.data_dir().join("sessions")
}

pub fn discover_sessions<F>(state_root: &Path, runtime_root: &Path, mut probe: F) -> io::Result<Vec<SessionInfo>>
where
    F: FnMut(&str) -> bool,
{
    let mut names = BTreeSet::new();
    collect_session_directories(state_root, &mut names)?;
    collect_session_directories(runtime_root, &mut names)?;

    Ok(names
        .into_iter()
        .map(|name| {
            let state_dir = state_root.join(&name);
            let runtime_dir = runtime_root.join(&name);
            SessionInfo {
                running: probe(&name),
                name,
                state_dir: state_dir.is_dir().then_some(state_dir),
                runtime_dir: runtime_dir.is_dir().then_some(runtime_dir),
            }
        })
        .collect())
}

fn collect_session_directories(root: &Path, names: &mut BTreeSet<String>) -> io::Result<()> {
    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    for entry in entries {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else { continue };
        if valid_session_name(&name) {
            names.insert(name);
        }
    }
    Ok(())
}

pub fn delete_session_directories(
    state_root: &Path,
    runtime_root: &Path,
    name: &str,
    running: bool,
) -> io::Result<Vec<PathBuf>> {
    if !valid_session_name(name) {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "invalid session name"));
    }
    if running {
        return Err(io::Error::new(
            io::ErrorKind::WouldBlock,
            format!("session '{name}' is running; stop it before deleting it"),
        ));
    }

    let mut removed = Vec::new();
    for path in [state_root.join(name), runtime_root.join(name)] {
        if path.exists() {
            fs::remove_dir_all(&path)?;
            removed.push(path);
        }
    }
    if removed.is_empty() {
        return Err(io::Error::new(io::ErrorKind::NotFound, format!("session '{name}' was not found")));
    }
    Ok(removed)
}

fn valid_session_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateFlowOutcome {
    NoUpdate,
    NeedsRestart { version: Version, backup_executable: PathBuf },
}

pub fn perform_update<C: HttpClient>(
    updater: &Updater<C>,
    channel: Channel,
    current_version: &Version,
) -> Result<UpdateFlowOutcome, UpdateError> {
    let Some(release) = updater.check(channel, current_version)? else {
        return Ok(UpdateFlowOutcome::NoUpdate);
    };
    let staged = updater.download_and_stage(&release)?;
    match apply(&staged)? {
        ApplyOutcome::NeedsRestart { backup_executable } => Ok(UpdateFlowOutcome::NeedsRestart {
            version: release.version,
            backup_executable,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use starcil_update::{HttpError, HttpRequest, HttpResponse, Platform, UpdateConfig};
    use std::cell::RefCell;
    use std::collections::HashMap;

    #[test]
    fn channel_round_trip_preserves_unrelated_comments() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("config.toml");
        fs::write(
            &path,
            "# owner note\nonboarding = false\n\n[update]\n# keep this\nchannel = \"stable\"\nversion_check = false\n",
        )
        .unwrap();

        set_channel_at(&path, Channel::Preview).unwrap();
        let result = fs::read_to_string(&path).unwrap();
        assert!(result.contains("# owner note"));
        assert!(result.contains("# keep this"));
        assert!(result.contains("version_check = false"));
        assert!(result.contains("channel = \"preview\""));
        assert_eq!(configured_channel(&path).unwrap(), Channel::Preview);
    }

    #[test]
    fn session_discovery_unions_roots_and_delete_refuses_running() {
        let temp = tempfile::tempdir().unwrap();
        let state = temp.path().join("state");
        let runtime = temp.path().join("runtime");
        fs::create_dir_all(state.join("work")).unwrap();
        fs::create_dir_all(runtime.join("work")).unwrap();
        fs::create_dir_all(runtime.join("review")).unwrap();

        let sessions = discover_sessions(&state, &runtime, |name| name == "work").unwrap();
        assert_eq!(sessions.iter().map(|item| item.name.as_str()).collect::<Vec<_>>(), ["review", "work"]);
        assert!(sessions.iter().find(|item| item.name == "work").unwrap().running);
        assert_eq!(
            delete_session_directories(&state, &runtime, "work", true).unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        let removed = delete_session_directories(&state, &runtime, "review", false).unwrap();
        assert_eq!(removed, [runtime.join("review")]);
    }

    #[derive(Default)]
    struct FakeHttp {
        responses: RefCell<HashMap<String, Result<HttpResponse, HttpError>>>,
    }

    impl FakeHttp {
        fn respond(&self, url: &str, body: impl Into<Vec<u8>>) {
            self.responses.borrow_mut().insert(url.to_owned(), Ok(HttpResponse { status: 200, body: body.into() }));
        }
    }

    impl HttpClient for FakeHttp {
        fn get(&self, request: &HttpRequest) -> Result<HttpResponse, HttpError> {
            self.responses
                .borrow()
                .get(&request.url)
                .cloned()
                .unwrap_or_else(|| Err(HttpError::new("offline")))
        }
    }

    #[test]
    fn update_flow_uses_injected_http_and_applies_verified_stage() {
        let temp = tempfile::tempdir().unwrap();
        let platform = Platform::WindowsX86_64Gnu;
        let current = temp.path().join("starcil.exe");
        fs::write(&current, b"old").unwrap();
        let config = UpdateConfig {
            repository: "owner/repo".to_owned(),
            data_directory: temp.path().join("data"),
            current_executable: current.clone(),
            platform,
        };
        let release_url = "https://api.github.com/repos/owner/repo/releases/latest";
        let binary_url = "https://download.invalid/starcil.exe";
        let sums_url = "https://download.invalid/SHA256SUMS";
        let http = FakeHttp::default();
        http.respond(
            release_url,
            format!(
                r#"{{"tag_name":"v0.2.0","prerelease":false,"assets":[{{"name":"{}","browser_download_url":"{binary_url}"}},{{"name":"SHA256SUMS","browser_download_url":"{sums_url}"}}]}}"#,
                platform.update_asset_name()
            ),
        );
        http.respond(binary_url, Vec::new());
        http.respond(
            sums_url,
            format!(
                "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855  {}\n",
                platform.update_asset_name()
            ),
        );
        let updater = Updater::new(http, config);

        let outcome = perform_update(&updater, Channel::Stable, &Version::parse("0.1.0").unwrap()).unwrap();
        assert!(matches!(outcome, UpdateFlowOutcome::NeedsRestart { .. }));
        assert_eq!(fs::read(current).unwrap(), b"");
    }
}
