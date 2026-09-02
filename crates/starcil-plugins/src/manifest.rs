use crate::{PluginError, PluginResult};
use semver::Version;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

pub const STARCIL_PLUGIN_MANIFEST: &str = "starcil-plugin.toml";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Platform {
    Linux,
    Macos,
    Windows,
}

impl Platform {
    pub const ALL: [Platform; 3] = [Platform::Linux, Platform::Macos, Platform::Windows];

    pub const fn current() -> Self {
        #[cfg(target_os = "windows")]
        { return Platform::Windows; }
        #[cfg(target_os = "macos")]
        { return Platform::Macos; }
        #[cfg(all(not(target_os = "windows"), not(target_os = "macos")))]
        { Platform::Linux }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Linux => "linux",
            Self::Macos => "macos",
            Self::Windows => "windows",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuildSpec {
    pub command: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub platforms: Option<Vec<Platform>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StartupSpec {
    pub command: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub platforms: Option<Vec<Platform>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionSpec {
    pub id: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub contexts: Vec<String>,
    pub command: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub platforms: Option<Vec<Platform>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventHookSpec {
    pub on: String,
    pub command: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub platforms: Option<Vec<Platform>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum PanePlacement {
    #[default]
    Overlay,
    Popup,
    Split,
    Tab,
    Zoomed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum PaneDimension {
    Cells(u16),
    Percent(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneSpec {
    pub id: String,
    pub title: String,
    pub command: Vec<String>,
    #[serde(default)]
    pub placement: PanePlacement,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<PaneDimension>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<PaneDimension>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub platforms: Option<Vec<Platform>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinkHandlerSpec {
    pub id: String,
    pub title: String,
    pub pattern: String,
    pub action: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub platforms: Option<Vec<Platform>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct PluginManifest {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub min_starcil_version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub platforms: Option<Vec<Platform>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub build: Vec<BuildSpec>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub startup: Vec<StartupSpec>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub actions: Vec<ActionSpec>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub events: Vec<EventHookSpec>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub panes: Vec<PaneSpec>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub link_handlers: Vec<LinkHandlerSpec>,
}

impl PluginManifest {
    pub fn effective_platforms(&self, item_platforms: Option<&[Platform]>) -> Vec<Platform> {
        item_platforms
            .map(ToOwned::to_owned)
            .or_else(|| self.platforms.clone())
            .unwrap_or_else(|| Platform::ALL.to_vec())
    }

    pub fn supports(&self, item_platforms: Option<&[Platform]>, platform: Platform) -> bool {
        self.effective_platforms(item_platforms).contains(&platform)
    }

    pub fn qualified_action_id(&self, action: &ActionSpec) -> String {
        format!("{}.{}", self.id, action.id)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ValidationReport {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct ManifestValidator {
    current_version: Version,
}

impl ManifestValidator {
    pub fn for_current_binary() -> Self {
        Self::new(env!("CARGO_PKG_VERSION")).expect("CARGO_PKG_VERSION must be valid semver")
    }

    pub fn new(current_version: &str) -> PluginResult<Self> {
        let current_version = Version::parse(current_version)
            .map_err(|error| PluginError::InvalidManifest(format!("invalid current Starcil version '{current_version}': {error}")))?;
        Ok(Self { current_version })
    }

    pub fn current_version(&self) -> &Version {
        &self.current_version
    }

    pub fn validate(&self, manifest: &PluginManifest) -> PluginResult<ValidationReport> {
        require_text(&manifest.id, "id")?;
        require_text(&manifest.name, "name")?;
        require_text(&manifest.version, "version")?;
        require_text(&manifest.min_starcil_version, "min_starcil_version")?;
        validate_plugin_id(&manifest.id)?;

        Version::parse(&manifest.version)
            .map_err(|error| PluginError::InvalidManifest(format!("invalid plugin version '{}': {error}", manifest.version)))?;
        let minimum = Version::parse(&manifest.min_starcil_version).map_err(|error| {
            PluginError::InvalidManifest(format!("invalid min_starcil_version '{}': {error}", manifest.min_starcil_version))
        })?;
        if minimum > self.current_version {
            return Err(PluginError::InvalidManifest(format!(
                "plugin requires Starcil {minimum}, current version is {}",
                self.current_version
            )));
        }

        let mut warnings = Vec::new();
        if manifest.platforms.is_none() {
            warnings.push("plugin does not declare top-level platforms; treating it as portable".to_owned());
        }
        validate_platform_list("plugin", manifest.platforms.as_deref(), &mut warnings);

        for (index, build) in manifest.build.iter().enumerate() {
            validate_command(&build.command, &format!("build[{index}]"))?;
            validate_platform_list(&format!("build[{index}]"), build.platforms.as_deref(), &mut warnings);
        }
        for (index, startup) in manifest.startup.iter().enumerate() {
            validate_command(&startup.command, &format!("startup[{index}]"))?;
            validate_platform_list(&format!("startup[{index}]"), startup.platforms.as_deref(), &mut warnings);
        }

        validate_unique_local_ids("action", manifest.actions.iter().map(|item| item.id.as_str()))?;
        for action in &manifest.actions {
            require_text(&action.title, &format!("action '{}'.title", action.id))?;
            validate_command(&action.command, &format!("action '{}'", action.id))?;
            validate_platform_list(&format!("action '{}'", action.id), action.platforms.as_deref(), &mut warnings);
        }

        for (index, event) in manifest.events.iter().enumerate() {
            require_text(&event.on, &format!("events[{index}].on"))?;
            validate_command(&event.command, &format!("event '{}'", event.on))?;
            validate_platform_list(&format!("event '{}'", event.on), event.platforms.as_deref(), &mut warnings);
            if !starcil_protocol::events::is_known(&event.on) {
                warnings.push(format!("unknown event name '{}'; hook is retained", event.on));
            }
        }

        validate_unique_local_ids("pane", manifest.panes.iter().map(|item| item.id.as_str()))?;
        for pane in &manifest.panes {
            require_text(&pane.title, &format!("pane '{}'.title", pane.id))?;
            validate_command(&pane.command, &format!("pane '{}'", pane.id))?;
            validate_dimension(pane.width.as_ref(), &format!("pane '{}'.width", pane.id))?;
            validate_dimension(pane.height.as_ref(), &format!("pane '{}'.height", pane.id))?;
            validate_platform_list(&format!("pane '{}'", pane.id), pane.platforms.as_deref(), &mut warnings);
        }

        validate_unique_local_ids("link handler", manifest.link_handlers.iter().map(|item| item.id.as_str()))?;
        let action_ids = manifest.actions.iter().map(|item| item.id.as_str()).collect::<HashSet<_>>();
        for handler in &manifest.link_handlers {
            require_text(&handler.title, &format!("link handler '{}'.title", handler.id))?;
            require_text(&handler.pattern, &format!("link handler '{}'.pattern", handler.id))?;
            if !action_ids.contains(handler.action.as_str()) {
                return Err(PluginError::InvalidManifest(format!(
                    "link handler '{}' references unknown action '{}'",
                    handler.id, handler.action
                )));
            }
            validate_platform_list(&format!("link handler '{}'", handler.id), handler.platforms.as_deref(), &mut warnings);
        }

        Ok(ValidationReport { warnings })
    }
}

#[derive(Debug, Clone)]
pub struct LoadedManifest {
    pub manifest: PluginManifest,
    pub manifest_path: PathBuf,
    pub plugin_root: PathBuf,
    pub report: ValidationReport,
}

pub fn load_manifest(path: impl AsRef<Path>, validator: &ManifestValidator) -> PluginResult<LoadedManifest> {
    let requested = path.as_ref();
    let manifest_path = if requested.is_dir() { requested.join(STARCIL_PLUGIN_MANIFEST) } else { requested.to_path_buf() };
    let contents = fs::read_to_string(&manifest_path).map_err(|error| PluginError::ManifestRead {
        path: manifest_path.clone(),
        message: error.to_string(),
    })?;
    let manifest = toml::from_str::<PluginManifest>(&contents).map_err(|error| PluginError::ManifestParse {
        path: manifest_path.clone(),
        message: error.to_string(),
    })?;
    let report = validator.validate(&manifest)?;
    let manifest_path = fs::canonicalize(&manifest_path).map_err(|error| PluginError::ManifestRead {
        path: manifest_path.clone(),
        message: error.to_string(),
    })?;
    let plugin_root = manifest_path
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| PluginError::InvalidManifest("manifest path has no parent directory".to_owned()))?;
    Ok(LoadedManifest { manifest, manifest_path, plugin_root, report })
}

fn require_text(value: &str, field: &str) -> PluginResult<()> {
    if value.trim().is_empty() {
        Err(PluginError::InvalidManifest(format!("required field '{field}' is missing or empty")))
    } else {
        Ok(())
    }
}

fn validate_plugin_id(id: &str) -> PluginResult<()> {
    if !id.chars().all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | ':' | '_' | '-')) {
        return Err(PluginError::InvalidManifest(format!("plugin id '{id}' contains invalid characters")));
    }
    Ok(())
}

fn validate_local_id(kind: &str, id: &str) -> PluginResult<()> {
    if id.is_empty() || !id.chars().all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, ':' | '_' | '-')) {
        return Err(PluginError::InvalidManifest(format!("{kind} id '{id}' contains invalid characters")));
    }
    Ok(())
}

fn validate_unique_local_ids<'a>(kind: &str, ids: impl Iterator<Item = &'a str>) -> PluginResult<()> {
    let mut seen = HashSet::new();
    for id in ids {
        validate_local_id(kind, id)?;
        if !seen.insert(id) {
            return Err(PluginError::InvalidManifest(format!("duplicate {kind} id '{id}'")));
        }
    }
    Ok(())
}

fn validate_command(command: &[String], field: &str) -> PluginResult<()> {
    if command.is_empty() || command[0].trim().is_empty() {
        return Err(PluginError::InvalidManifest(format!("{field} command must contain an executable")));
    }
    Ok(())
}

fn validate_platform_list(label: &str, platforms: Option<&[Platform]>, warnings: &mut Vec<String>) {
    let Some(platforms) = platforms else { return };
    if platforms.is_empty() {
        warnings.push(format!("{label} declares an empty platform list and cannot run"));
        return;
    }
    let mut seen = HashSet::new();
    for platform in platforms {
        if !seen.insert(*platform) {
            warnings.push(format!("{label} repeats platform '{}'", platform.as_str()));
        }
    }
}

fn validate_dimension(value: Option<&PaneDimension>, field: &str) -> PluginResult<()> {
    match value {
        None => Ok(()),
        Some(PaneDimension::Cells(0)) => Err(PluginError::InvalidManifest(format!("{field} must be greater than zero"))),
        Some(PaneDimension::Cells(_)) => Ok(()),
        Some(PaneDimension::Percent(value)) => {
            let number = value.strip_suffix('%').and_then(|number| number.parse::<u16>().ok());
            if number.is_some_and(|number| (1..=100).contains(&number)) {
                Ok(())
            } else {
                Err(PluginError::InvalidManifest(format!("{field} must be a percentage from 1% to 100%")))
            }
        }
    }
}
