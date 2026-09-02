use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use thiserror::Error;

const CONTROL_PERSIST_SECONDS: u32 = 60;
static NEXT_ATTACH_ID: AtomicU64 = AtomicU64::new(1);

/// Owns a generated per-attach OpenSSH config and its private control path.
/// Both paths are removed best-effort when the manager is dropped.
#[derive(Debug)]
pub struct SshConfigManager {
    config_path: Option<PathBuf>,
    control_path: Option<PathBuf>,
}

impl SshConfigManager {
    pub fn create(
        runtime_dir: &Path,
        user_ssh_config: Option<&Path>,
        managed: bool,
    ) -> Result<Self, SshConfigError> {
        if !managed {
            return Ok(Self {
                config_path: None,
                control_path: None,
            });
        }

        fs::create_dir_all(runtime_dir).map_err(|source| SshConfigError::Io {
            path: runtime_dir.to_owned(),
            source,
        })?;

        let (config_path, control_path, mut config_file) =
            create_unique_config_file(runtime_dir)?;
        let rendered = match render_config(user_ssh_config, &control_path) {
            Ok(rendered) => rendered,
            Err(error) => {
                let _ = fs::remove_file(&config_path);
                return Err(error);
            }
        };
        if let Err(source) = config_file.write_all(rendered.as_bytes()) {
            let _ = fs::remove_file(&config_path);
            return Err(SshConfigError::Io {
                path: config_path,
                source,
            });
        }

        Ok(Self {
            config_path: Some(config_path),
            control_path: Some(control_path),
        })
    }

    pub fn is_managed(&self) -> bool {
        self.config_path.is_some()
    }

    pub fn config_path(&self) -> Option<&Path> {
        self.config_path.as_deref()
    }

    pub fn control_path(&self) -> Option<&Path> {
        self.control_path.as_deref()
    }
}

impl Drop for SshConfigManager {
    fn drop(&mut self) {
        if let Some(path) = self.config_path.as_deref() {
            let _ = fs::remove_file(path);
        }
        if let Some(path) = self.control_path.as_deref() {
            let _ = fs::remove_file(path);
        }
    }
}

fn create_unique_config_file(
    runtime_dir: &Path,
) -> Result<(PathBuf, PathBuf, fs::File), SshConfigError> {
    for _ in 0..32 {
        let id = NEXT_ATTACH_ID.fetch_add(1, Ordering::Relaxed);
        let stem = format!("{}-{id}", std::process::id());
        let config_path = runtime_dir.join(format!("ssh-{stem}.conf"));
        let control_path = runtime_dir.join(format!("ssh-{stem}.ctl"));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        match options.open(&config_path) {
            Ok(file) => return Ok((config_path, control_path, file)),
            Err(source) if source.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(source) => {
                return Err(SshConfigError::Io {
                    path: config_path,
                    source,
                });
            }
        }
    }
    Err(SshConfigError::NoUniquePath(runtime_dir.to_owned()))
}

fn render_config(
    user_ssh_config: Option<&Path>,
    control_path: &Path,
) -> Result<String, SshConfigError> {
    let mut config = String::new();
    if let Some(path) = user_ssh_config.filter(|path| path.is_file()) {
        config.push_str("Include ");
        config.push_str(&quote_path(path)?);
        config.push_str("\n\n");
    }
    config.push_str("Host *\n");
    config.push_str("    ServerAliveInterval 15\n");
    config.push_str("    ServerAliveCountMax 4\n");
    config.push_str("    ControlMaster auto\n");
    config.push_str("    ControlPath ");
    config.push_str(&quote_path(control_path)?);
    config.push('\n');
    config.push_str(&format!("    ControlPersist {CONTROL_PERSIST_SECONDS}\n"));
    Ok(config)
}

fn quote_path(path: &Path) -> Result<String, SshConfigError> {
    let rendered = path.to_string_lossy();
    #[cfg(windows)]
    let normalized = rendered.replace('\\', "/");
    #[cfg(not(windows))]
    let normalized = rendered.into_owned();
    if normalized.chars().any(|character| matches!(character, '\0' | '\r' | '\n')) {
        return Err(SshConfigError::UnsafePath(path.to_owned()));
    }
    Ok(format!("\"{}\"", normalized.replace('"', "\\\"")))
}

#[derive(Debug, Error)]
pub enum SshConfigError {
    #[error("could not access SSH config path `{path}`: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("could not allocate a unique managed SSH config below `{0}`")]
    NoUniquePath(PathBuf),
    #[error("SSH config path contains a line-breaking or NUL character: `{0}`")]
    UnsafePath(PathBuf),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn managed_config_matches_the_golden_order_and_cleans_up() {
        let temp = tempfile::tempdir().unwrap();
        let runtime = temp.path().join("runtime with spaces");
        let user_config = temp.path().join("user config").join("config");
        fs::create_dir_all(user_config.parent().unwrap()).unwrap();
        fs::write(&user_config, "Host buildbox\n    Port 2222\n").unwrap();

        let manager = SshConfigManager::create(&runtime, Some(&user_config), true).unwrap();
        let config_path = manager.config_path().unwrap().to_owned();
        let control_path = manager.control_path().unwrap().to_owned();
        fs::write(&control_path, b"fake control socket").unwrap();
        let expected = format!(
            "Include {}\n\nHost *\n    ServerAliveInterval 15\n    ServerAliveCountMax 4\n    ControlMaster auto\n    ControlPath {}\n    ControlPersist 60\n",
            quote_path(&user_config).unwrap(),
            quote_path(&control_path).unwrap(),
        );
        assert_eq!(fs::read_to_string(&config_path).unwrap(), expected);

        drop(manager);
        assert!(!config_path.exists());
        assert!(!control_path.exists());
    }

    #[test]
    fn missing_user_config_is_omitted_and_disabled_mode_is_plain() {
        let temp = tempfile::tempdir().unwrap();
        let runtime = temp.path().join("runtime");
        let missing = temp.path().join("missing-config");
        let manager = SshConfigManager::create(&runtime, Some(&missing), true).unwrap();
        let rendered = fs::read_to_string(manager.config_path().unwrap()).unwrap();
        assert!(rendered.starts_with("Host *\n"));
        drop(manager);

        let disabled = SshConfigManager::create(&runtime, Some(&missing), false).unwrap();
        assert!(!disabled.is_managed());
        assert!(disabled.config_path().is_none());
    }
}
