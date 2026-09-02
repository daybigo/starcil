use crate::{Channel, HttpClient, HttpError, HttpRequest, Platform};
use semver::Version;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    env, fs,
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use thiserror::Error;

pub const DEFAULT_REPOSITORY: &str = "daybigo/starcil";
pub const CHECKSUM_ASSET: &str = "SHA256SUMS";
const HTTP_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseAsset {
    pub name: String,
    pub download_url: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseInfo {
    pub version: Version,
    pub tag: String,
    pub prerelease: bool,
    pub assets: Vec<ReleaseAsset>,
}

impl ReleaseInfo {
    pub fn asset(&self, name: &str) -> Option<&ReleaseAsset> {
        self.assets.iter().find(|asset| asset.name == name)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateConfig {
    pub repository: String,
    pub data_directory: PathBuf,
    pub current_executable: PathBuf,
    pub platform: Platform,
}

impl UpdateConfig {
    pub fn new(
        data_directory: impl Into<PathBuf>,
        current_executable: impl Into<PathBuf>,
        platform: Platform,
    ) -> Self {
        Self {
            repository: default_repo_slug(),
            data_directory: data_directory.into(),
            current_executable: current_executable.into(),
            platform,
        }
    }
}

pub fn default_repo_slug() -> String {
    env::var("STARCIL_UPDATE_REPO")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_REPOSITORY.to_owned())
}

pub struct Updater<C> {
    http: C,
    config: UpdateConfig,
}

impl<C: HttpClient> Updater<C> {
    pub fn new(http: C, config: UpdateConfig) -> Self {
        Self { http, config }
    }

    pub fn config(&self) -> &UpdateConfig {
        &self.config
    }

    /// Network and HTTP failures are intentionally treated as no available update.
    pub fn check(
        &self,
        channel: Channel,
        current_version: &Version,
    ) -> Result<Option<ReleaseInfo>, UpdateError> {
        let url = match channel {
            Channel::Stable => format!(
                "https://api.github.com/repos/{}/releases/latest",
                self.config.repository
            ),
            Channel::Preview => format!(
                "https://api.github.com/repos/{}/releases?per_page=30",
                self.config.repository
            ),
        };
        let response = match self.http.get(&HttpRequest {
            url,
            timeout: HTTP_TIMEOUT,
        }) {
            Ok(response) if (200..300).contains(&response.status) => response,
            Ok(_) | Err(_) => return Ok(None),
        };

        let release = match channel {
            Channel::Stable => {
                let raw: GitHubRelease = serde_json::from_slice(&response.body)?;
                Some(raw.try_into_release()?)
            }
            Channel::Preview => {
                let raw: Vec<GitHubRelease> = serde_json::from_slice(&response.body)?;
                raw.into_iter()
                    .filter(|release| !release.draft && release.prerelease)
                    .filter_map(|release| release.try_into_release().ok())
                    .max_by(|left, right| left.version.cmp(&right.version))
            }
        };
        Ok(release.filter(|release| &release.version > current_version))
    }

    pub fn download_and_stage(
        &self,
        release: &ReleaseInfo,
    ) -> Result<StagedUpdate, UpdateError> {
        let asset_name = self.config.platform.update_asset_name();
        let executable_asset = release
            .asset(asset_name)
            .ok_or_else(|| UpdateError::MissingAsset(asset_name.to_owned()))?;
        let checksum_asset = release
            .asset(CHECKSUM_ASSET)
            .ok_or_else(|| UpdateError::MissingAsset(CHECKSUM_ASSET.to_owned()))?;
        let executable = self.download_required(executable_asset)?;
        let checksums = self.download_required(checksum_asset)?;
        verify_checksum(asset_name, &executable, &checksums)?;

        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        let staging_directory = self.config.data_directory.join("updates").join(format!(
            "{}-{}-{timestamp}",
            release.version,
            std::process::id()
        ));
        fs::create_dir_all(&staging_directory).map_err(|source| UpdateError::Io {
            path: staging_directory.clone(),
            source,
        })?;
        let staged_executable = staging_directory.join(self.config.platform.executable_name());
        fs::write(&staged_executable, executable).map_err(|source| UpdateError::Io {
            path: staged_executable.clone(),
            source,
        })?;
        make_executable(&staged_executable)?;

        Ok(StagedUpdate {
            release: release.clone(),
            staging_directory,
            staged_executable,
            current_executable: self.config.current_executable.clone(),
            backup_executable: self
                .config
                .current_executable
                .with_file_name(self.config.platform.backup_name()),
        })
    }

    fn download_required(&self, asset: &ReleaseAsset) -> Result<Vec<u8>, UpdateError> {
        let response = self
            .http
            .get(&HttpRequest {
                url: asset.download_url.clone(),
                timeout: HTTP_TIMEOUT,
            })
            .map_err(|source| UpdateError::Http {
                url: asset.download_url.clone(),
                source,
            })?;
        if !(200..300).contains(&response.status) {
            return Err(UpdateError::HttpStatus {
                url: asset.download_url.clone(),
                status: response.status,
            });
        }
        Ok(response.body)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StagedUpdate {
    pub release: ReleaseInfo,
    pub staging_directory: PathBuf,
    pub staged_executable: PathBuf,
    pub current_executable: PathBuf,
    pub backup_executable: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApplyOutcome {
    NeedsRestart { backup_executable: PathBuf },
}

pub fn apply(staged: &StagedUpdate) -> Result<ApplyOutcome, UpdateError> {
    if staged.backup_executable.exists() {
        fs::remove_file(&staged.backup_executable).map_err(|source| UpdateError::Io {
            path: staged.backup_executable.clone(),
            source,
        })?;
    }
    fs::rename(&staged.current_executable, &staged.backup_executable).map_err(|source| {
        UpdateError::Io {
            path: staged.current_executable.clone(),
            source,
        }
    })?;

    if let Err(swap_error) = fs::rename(&staged.staged_executable, &staged.current_executable) {
        return match fs::rename(&staged.backup_executable, &staged.current_executable) {
            Ok(()) => Err(UpdateError::SwapFailed {
                source: swap_error,
                rolled_back: true,
            }),
            Err(rollback_error) => Err(UpdateError::RollbackFailed {
                swap_error: swap_error.to_string(),
                rollback_error: rollback_error.to_string(),
            }),
        };
    }

    Ok(ApplyOutcome::NeedsRestart {
        backup_executable: staged.backup_executable.clone(),
    })
}

#[derive(Debug, Error)]
pub enum UpdateError {
    #[error("invalid GitHub release response: {0}")]
    Json(#[from] serde_json::Error),
    #[error("release tag `{tag}` is not semantic versioning: {source}")]
    InvalidVersion {
        tag: String,
        #[source]
        source: semver::Error,
    },
    #[error("release is missing required asset `{0}`")]
    MissingAsset(String),
    #[error("HTTP request failed for `{url}`: {source}")]
    Http {
        url: String,
        #[source]
        source: HttpError,
    },
    #[error("HTTP request for `{url}` returned status {status}")]
    HttpStatus { url: String, status: u16 },
    #[error("checksums asset has no valid SHA-256 entry for `{0}`")]
    MissingChecksum(String),
    #[error("SHA-256 mismatch for `{asset}`: expected {expected}, got {actual}")]
    ChecksumMismatch {
        asset: String,
        expected: String,
        actual: String,
    },
    #[error("could not access `{path}`: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("new executable swap failed (rolled_back={rolled_back}): {source}")]
    SwapFailed {
        #[source]
        source: std::io::Error,
        rolled_back: bool,
    },
    #[error("new executable swap failed and rollback failed: swap={swap_error}; rollback={rollback_error}")]
    RollbackFailed {
        swap_error: String,
        rollback_error: String,
    },
}

#[derive(Debug, Deserialize)]
struct GitHubRelease {
    tag_name: String,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    assets: Vec<GitHubAsset>,
}

impl GitHubRelease {
    fn try_into_release(self) -> Result<ReleaseInfo, UpdateError> {
        let version_text = self.tag_name.strip_prefix('v').unwrap_or(&self.tag_name);
        let version = Version::parse(version_text).map_err(|source| UpdateError::InvalidVersion {
            tag: self.tag_name.clone(),
            source,
        })?;
        Ok(ReleaseInfo {
            version,
            tag: self.tag_name,
            prerelease: self.prerelease,
            assets: self
                .assets
                .into_iter()
                .map(|asset| ReleaseAsset {
                    name: asset.name,
                    download_url: asset.browser_download_url,
                })
                .collect(),
        })
    }
}

#[derive(Debug, Deserialize)]
struct GitHubAsset {
    name: String,
    browser_download_url: String,
}

fn verify_checksum(asset_name: &str, bytes: &[u8], checksums: &[u8]) -> Result<(), UpdateError> {
    let checksums = String::from_utf8_lossy(checksums);
    let expected = checksums.lines().find_map(|line| {
        let mut fields = line.split_whitespace();
        let checksum = fields.next()?;
        let name = fields.next()?.trim_start_matches('*');
        (name == asset_name && checksum.len() == 64 && checksum.bytes().all(|byte| byte.is_ascii_hexdigit()))
            .then(|| checksum.to_ascii_lowercase())
    });
    let expected = expected.ok_or_else(|| UpdateError::MissingChecksum(asset_name.to_owned()))?;
    let actual = sha256_hex(bytes);
    if actual != expected {
        return Err(UpdateError::ChecksumMismatch {
            asset: asset_name.to_owned(),
            expected,
            actual,
        });
    }
    Ok(())
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn make_executable(path: &Path) -> Result<(), UpdateError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = fs::metadata(path)
            .map_err(|source| UpdateError::Io {
                path: path.to_owned(),
                source,
            })?
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(path, permissions).map_err(|source| UpdateError::Io {
            path: path.to_owned(),
            source,
        })?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::HttpResponse;
    use std::{cell::RefCell, collections::HashMap};

    #[derive(Default)]
    struct CannedHttp {
        responses: RefCell<HashMap<String, Result<HttpResponse, HttpError>>>,
    }

    impl CannedHttp {
        fn respond(&self, url: &str, status: u16, body: impl Into<Vec<u8>>) {
            self.responses.borrow_mut().insert(
                url.to_owned(),
                Ok(HttpResponse {
                    status,
                    body: body.into(),
                }),
            );
        }

        fn fail(&self, url: &str) {
            self.responses
                .borrow_mut()
                .insert(url.to_owned(), Err(HttpError::new("offline")));
        }
    }

    impl HttpClient for CannedHttp {
        fn get(&self, request: &HttpRequest) -> Result<HttpResponse, HttpError> {
            self.responses
                .borrow()
                .get(&request.url)
                .cloned()
                .unwrap_or_else(|| Err(HttpError::new(format!("no response for {}", request.url))))
        }
    }

    fn config(temp: &tempfile::TempDir, platform: Platform) -> UpdateConfig {
        UpdateConfig {
            repository: "owner/repo".to_owned(),
            data_directory: temp.path().join("data"),
            current_executable: temp.path().join(platform.executable_name()),
            platform,
        }
    }

    #[test]
    fn stable_check_compares_semver_and_offline_is_silent() {
        let temp = tempfile::tempdir().unwrap();
        let url = "https://api.github.com/repos/owner/repo/releases/latest";
        let http = CannedHttp::default();
        http.respond(
            url,
            200,
            br#"{"tag_name":"v1.2.0","prerelease":false,"assets":[]}"#.to_vec(),
        );
        let updater = Updater::new(http, config(&temp, Platform::WindowsX86_64Gnu));
        assert_eq!(
            updater
                .check(Channel::Stable, &Version::parse("1.1.9").unwrap())
                .unwrap()
                .unwrap()
                .version,
            Version::parse("1.2.0").unwrap()
        );
        assert!(updater
            .check(Channel::Stable, &Version::parse("1.2.0").unwrap())
            .unwrap()
            .is_none());

        let offline = CannedHttp::default();
        offline.fail(url);
        let updater = Updater::new(offline, config(&temp, Platform::WindowsX86_64Gnu));
        assert!(updater
            .check(Channel::Stable, &Version::parse("1.0.0").unwrap())
            .unwrap()
            .is_none());
    }

    #[test]
    fn preview_check_selects_highest_prerelease_only() {
        let temp = tempfile::tempdir().unwrap();
        let url = "https://api.github.com/repos/owner/repo/releases?per_page=30";
        let http = CannedHttp::default();
        http.respond(
            url,
            200,
            br#"[
                {"tag_name":"v2.0.0","prerelease":false,"assets":[]},
                {"tag_name":"v2.1.0-beta.1","prerelease":true,"assets":[]},
                {"tag_name":"v2.1.0-beta.3","prerelease":true,"assets":[]}
            ]"#
            .to_vec(),
        );
        let updater = Updater::new(http, config(&temp, Platform::LinuxX86_64));
        let release = updater
            .check(Channel::Preview, &Version::parse("2.0.0").unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(release.version, Version::parse("2.1.0-beta.3").unwrap());
    }

    #[test]
    fn tampered_download_fails_checksum_verification() {
        let temp = tempfile::tempdir().unwrap();
        let platform = Platform::WindowsX86_64Gnu;
        let asset_name = platform.update_asset_name();
        let asset_url = "https://downloads.invalid/starcil.exe";
        let sums_url = "https://downloads.invalid/SHA256SUMS";
        let http = CannedHttp::default();
        http.respond(asset_url, 200, b"tampered".to_vec());
        http.respond(
            sums_url,
            200,
            format!("{}  {asset_name}\n", sha256_hex(b"expected")).into_bytes(),
        );
        let updater = Updater::new(http, config(&temp, platform));
        let release = release(asset_name, asset_url, sums_url);
        assert!(matches!(
            updater.download_and_stage(&release),
            Err(UpdateError::ChecksumMismatch { .. })
        ));
    }

    #[test]
    fn verified_stage_applies_to_dummy_copy_and_keeps_restart_backup() {
        let temp = tempfile::tempdir().unwrap();
        let platform = Platform::WindowsX86_64Gnu;
        let asset_name = platform.update_asset_name();
        let asset_url = "https://downloads.invalid/starcil.exe";
        let sums_url = "https://downloads.invalid/SHA256SUMS";
        let http = CannedHttp::default();
        http.respond(asset_url, 200, b"new-binary".to_vec());
        http.respond(
            sums_url,
            200,
            format!("{}  {asset_name}\n", sha256_hex(b"new-binary")).into_bytes(),
        );
        let config = config(&temp, platform);
        fs::write(&config.current_executable, b"old-binary").unwrap();
        let updater = Updater::new(http, config.clone());
        let staged = updater
            .download_and_stage(&release(asset_name, asset_url, sums_url))
            .unwrap();
        assert!(staged.staged_executable.starts_with(&config.data_directory));
        let outcome = apply(&staged).unwrap();
        assert!(matches!(outcome, ApplyOutcome::NeedsRestart { .. }));
        assert_eq!(fs::read(&config.current_executable).unwrap(), b"new-binary");
        assert_eq!(fs::read(&staged.backup_executable).unwrap(), b"old-binary");
    }

    #[test]
    fn failed_swap_rolls_back_dummy_executable() {
        let temp = tempfile::tempdir().unwrap();
        let current = temp.path().join("starcil.exe");
        let backup = temp.path().join("starcil-old.exe");
        fs::write(&current, b"old-binary").unwrap();
        let staged = StagedUpdate {
            release: ReleaseInfo {
                version: Version::parse("2.0.0").unwrap(),
                tag: "v2.0.0".to_owned(),
                prerelease: false,
                assets: Vec::new(),
            },
            staging_directory: temp.path().join("staging"),
            staged_executable: temp.path().join("missing-new.exe"),
            current_executable: current.clone(),
            backup_executable: backup.clone(),
        };
        assert!(matches!(
            apply(&staged),
            Err(UpdateError::SwapFailed {
                rolled_back: true,
                ..
            })
        ));
        assert_eq!(fs::read(&current).unwrap(), b"old-binary");
        assert!(!backup.exists());
    }

    fn release(asset_name: &str, asset_url: &str, sums_url: &str) -> ReleaseInfo {
        ReleaseInfo {
            version: Version::parse("2.0.0").unwrap(),
            tag: "v2.0.0".to_owned(),
            prerelease: false,
            assets: vec![
                ReleaseAsset {
                    name: asset_name.to_owned(),
                    download_url: asset_url.to_owned(),
                },
                ReleaseAsset {
                    name: CHECKSUM_ASSET.to_owned(),
                    download_url: sums_url.to_owned(),
                },
            ],
        }
    }
}
