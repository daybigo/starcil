use crate::{load_manifest, ManifestValidator, PluginError, PluginManifest, PluginResult};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

const REGISTRY_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegistryPaths {
    pub registry_file: PathBuf,
    pub plugin_data_root: PathBuf,
}

impl RegistryPaths {
    pub fn new(registry_file: impl Into<PathBuf>, plugin_data_root: impl Into<PathBuf>) -> Self {
        Self { registry_file: registry_file.into(), plugin_data_root: plugin_data_root.into() }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum SourceMetadata {
    Github {
        owner: String,
        repo: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        subdir: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        requested_ref: Option<String>,
        resolved_commit: String,
        managed_path: String,
        installed_unix_ms: u64,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GithubSourceMetadata {
    pub owner: String,
    pub repo: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subdir: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested_ref: Option<String>,
    pub resolved_commit: String,
    pub managed_path: String,
    pub installed_unix_ms: u64,
}

impl From<GithubSourceMetadata> for SourceMetadata {
    fn from(value: GithubSourceMetadata) -> Self {
        Self::Github {
            owner: value.owner,
            repo: value.repo,
            subdir: value.subdir,
            requested_ref: value.requested_ref,
            resolved_commit: value.resolved_commit,
            managed_path: value.managed_path,
            installed_unix_ms: value.installed_unix_ms,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginEntry {
    pub plugin_id: String,
    pub name: String,
    pub version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub manifest_path: PathBuf,
    pub plugin_root: PathBuf,
    pub config_dir: PathBuf,
    pub state_dir: PathBuf,
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<SourceMetadata>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
    #[serde(skip)]
    pub manifest: Option<PluginManifest>,
}

#[derive(Debug, Serialize, Deserialize)]
struct RegistryDocument {
    version: u32,
    #[serde(default)]
    entries: Vec<PluginEntry>,
}

#[derive(Debug)]
pub struct PluginRegistry {
    paths: RegistryPaths,
    validator: ManifestValidator,
    entries: Vec<PluginEntry>,
}

impl PluginRegistry {
    pub fn open(paths: RegistryPaths, current_version: &str) -> PluginResult<Self> {
        let validator = ManifestValidator::new(current_version)?;
        Self::open_with_validator(paths, validator)
    }

    pub fn open_for_current_binary(paths: RegistryPaths) -> PluginResult<Self> {
        Self::open_with_validator(paths, ManifestValidator::for_current_binary())
    }

    pub fn open_with_validator(paths: RegistryPaths, validator: ManifestValidator) -> PluginResult<Self> {
        let mut entries = match fs::read_to_string(&paths.registry_file) {
            Ok(contents) => {
                let document = serde_json::from_str::<RegistryDocument>(&contents).map_err(|error| PluginError::RegistryParse {
                    path: paths.registry_file.clone(),
                    message: error.to_string(),
                })?;
                if document.version != REGISTRY_VERSION {
                    return Err(PluginError::RegistryParse {
                        path: paths.registry_file.clone(),
                        message: format!("unsupported registry version {}", document.version),
                    });
                }
                document.entries
            }
            Err(error) if error.kind() == ErrorKind::NotFound => Vec::new(),
            Err(error) => {
                return Err(PluginError::RegistryRead { path: paths.registry_file.clone(), message: error.to_string() });
            }
        };

        for entry in &mut entries {
            refresh_entry(entry, &validator);
        }
        Ok(Self { paths, validator, entries })
    }

    pub fn paths(&self) -> &RegistryPaths {
        &self.paths
    }

    pub fn entries(&self) -> &[PluginEntry] {
        &self.entries
    }

    pub fn get(&self, plugin_id: &str) -> Option<&PluginEntry> {
        self.entries.iter().find(|entry| entry.plugin_id == plugin_id)
    }

    pub fn link(
        &mut self,
        path: impl AsRef<Path>,
        enabled: bool,
        source: Option<SourceMetadata>,
    ) -> PluginResult<PluginEntry> {
        let loaded = load_manifest(path, &self.validator)?;
        let config_dir = self.plugin_dir(&loaded.manifest.id).join("config");
        let state_dir = self.plugin_dir(&loaded.manifest.id).join("state");
        create_directory(&config_dir)?;
        create_directory(&state_dir)?;

        let entry = PluginEntry {
            plugin_id: loaded.manifest.id.clone(),
            name: loaded.manifest.name.clone(),
            version: loaded.manifest.version.clone(),
            description: loaded.manifest.description.clone(),
            manifest_path: loaded.manifest_path,
            plugin_root: loaded.plugin_root,
            config_dir,
            state_dir,
            enabled,
            source,
            warnings: loaded.report.warnings,
            manifest: Some(loaded.manifest),
        };

        let mut entries = self.entries.clone();
        if let Some(existing) = entries.iter_mut().find(|candidate| candidate.plugin_id == entry.plugin_id) {
            *existing = entry.clone();
        } else {
            entries.push(entry.clone());
            entries.sort_by(|left, right| left.plugin_id.cmp(&right.plugin_id));
        }
        self.persist_entries(&entries)?;
        self.entries = entries;
        Ok(entry)
    }

    pub fn unlink(&mut self, plugin_id: &str) -> PluginResult<PluginEntry> {
        let Some(index) = self.entries.iter().position(|entry| entry.plugin_id == plugin_id) else {
            return Err(PluginError::PluginNotFound(plugin_id.to_owned()));
        };
        let mut entries = self.entries.clone();
        let removed = entries.remove(index);
        self.persist_entries(&entries)?;
        self.entries = entries;
        Ok(removed)
    }

    pub fn enable(&mut self, plugin_id: &str) -> PluginResult<PluginEntry> {
        self.set_enabled(plugin_id, true)
    }

    pub fn disable(&mut self, plugin_id: &str) -> PluginResult<PluginEntry> {
        self.set_enabled(plugin_id, false)
    }

    pub fn persist(&self) -> PluginResult<()> {
        self.persist_entries(&self.entries)
    }

    fn set_enabled(&mut self, plugin_id: &str, enabled: bool) -> PluginResult<PluginEntry> {
        let Some(index) = self.entries.iter().position(|entry| entry.plugin_id == plugin_id) else {
            return Err(PluginError::PluginNotFound(plugin_id.to_owned()));
        };
        let mut entries = self.entries.clone();
        entries[index].enabled = enabled;
        let updated = entries[index].clone();
        self.persist_entries(&entries)?;
        self.entries = entries;
        Ok(updated)
    }

    fn plugin_dir(&self, plugin_id: &str) -> PathBuf {
        self.paths.plugin_data_root.join(safe_directory_name(plugin_id))
    }

    fn persist_entries(&self, entries: &[PluginEntry]) -> PluginResult<()> {
        if let Some(parent) = self.paths.registry_file.parent() {
            create_directory(parent)?;
        }
        let document = RegistryDocument { version: REGISTRY_VERSION, entries: entries.to_vec() };
        let mut contents = serde_json::to_vec_pretty(&document).map_err(|error| PluginError::RegistryWrite {
            path: self.paths.registry_file.clone(),
            message: error.to_string(),
        })?;
        contents.push(b'\n');
        fs::write(&self.paths.registry_file, contents).map_err(|error| PluginError::RegistryWrite {
            path: self.paths.registry_file.clone(),
            message: error.to_string(),
        })
    }
}

fn refresh_entry(entry: &mut PluginEntry, validator: &ManifestValidator) {
    match load_manifest(&entry.manifest_path, validator) {
        Ok(loaded) if loaded.manifest.id == entry.plugin_id => {
            entry.name = loaded.manifest.name.clone();
            entry.version = loaded.manifest.version.clone();
            entry.description = loaded.manifest.description.clone();
            entry.plugin_root = loaded.plugin_root;
            entry.warnings = loaded.report.warnings;
            entry.manifest = Some(loaded.manifest);
        }
        Ok(loaded) => {
            entry.manifest = None;
            entry.warnings = vec![format!(
                "manifest id changed from '{}' to '{}'; entry remains registered but inactive",
                entry.plugin_id, loaded.manifest.id
            )];
        }
        Err(error) => {
            entry.manifest = None;
            entry.warnings = vec![format!("manifest could not be loaded: {error}")];
        }
    }
}

fn create_directory(path: &Path) -> PluginResult<()> {
    fs::create_dir_all(path).map_err(|error| PluginError::DirectoryCreate {
        path: path.to_path_buf(),
        message: error.to_string(),
    })
}

fn safe_directory_name(plugin_id: &str) -> String {
    let mut output = String::new();
    for byte in plugin_id.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-') {
            output.push(char::from(byte));
        } else {
            output.push('%');
            output.push_str(&format!("{byte:02X}"));
        }
    }
    output
}
