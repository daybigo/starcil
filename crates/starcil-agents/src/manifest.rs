use crate::AgentKind;
use aho_corasick::{AhoCorasick, AhoCorasickBuilder, MatchKind};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, HashSet},
    fs,
    path::{Path, PathBuf},
    str::FromStr,
};
use thiserror::Error;

pub const DETECTION_ROWS: usize = 16;
pub const DETECTION_MANIFEST_SCHEMA_VERSION: u32 = 1;
const BUNDLED_MANIFEST: &str = include_str!("builtin_manifest.toml");

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentManifest {
    #[serde(default = "default_manifest_version")]
    pub version: u32,
    #[serde(default)]
    pub agents: Vec<AgentDefinition>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generic_rules: Option<Vec<ScreenRule>>,
}

const fn default_manifest_version() -> u32 {
    1
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentDefinition {
    /// A built-in `AgentKind` name or a custom manifest identifier.
    pub kind: String,
    #[serde(default)]
    pub process_name_patterns: Vec<String>,
    #[serde(default)]
    pub launch_command_hints: Vec<String>,
    #[serde(default)]
    pub osc_title_patterns: Vec<String>,
    #[serde(default)]
    pub osc_progress_patterns: Vec<String>,
    #[serde(default)]
    pub screen_rules: Vec<ScreenRule>,
}

/// Remotely cacheable or locally overrideable per-agent TOML document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DetectionManifest {
    #[serde(default = "default_detection_schema_version")]
    pub schema_version: u32,
    #[serde(default = "default_manifest_version")]
    pub version: u32,
    pub kind: String,
    #[serde(default)]
    pub process_name_patterns: Vec<String>,
    #[serde(default)]
    pub launch_command_hints: Vec<String>,
    #[serde(default)]
    pub osc_title_patterns: Vec<String>,
    #[serde(default)]
    pub osc_progress_patterns: Vec<String>,
    #[serde(default)]
    pub screen_rules: Vec<ScreenRule>,
}

const fn default_detection_schema_version() -> u32 {
    DETECTION_MANIFEST_SCHEMA_VERSION
}

impl DetectionManifest {
    pub fn from_toml(document: &str) -> Result<Self, ManifestError> {
        let manifest: Self = toml::from_str(document)?;
        manifest.validate(None)?;
        Ok(manifest)
    }

    fn validate(&self, expected_kind: Option<&str>) -> Result<(), ManifestError> {
        if self.schema_version != DETECTION_MANIFEST_SCHEMA_VERSION {
            return Err(ManifestError::IncompatibleSchema {
                found: self.schema_version,
                supported: DETECTION_MANIFEST_SCHEMA_VERSION,
            });
        }
        if self.version == 0 {
            return Err(ManifestError::Validation(
                "detection manifest version must be greater than zero".to_owned(),
            ));
        }
        if let Some(expected_kind) = expected_kind {
            if !self.kind.eq_ignore_ascii_case(expected_kind) {
                return Err(ManifestError::Validation(format!(
                    "manifest kind `{}` does not match file agent `{expected_kind}`",
                    self.kind
                )));
            }
        }
        AgentManifest {
            version: self.version,
            agents: vec![self.definition()],
            generic_rules: None,
        }
        .validate()
    }

    fn definition(&self) -> AgentDefinition {
        AgentDefinition {
            kind: self.kind.clone(),
            process_name_patterns: self.process_name_patterns.clone(),
            launch_command_hints: self.launch_command_hints.clone(),
            osc_title_patterns: self.osc_title_patterns.clone(),
            osc_progress_patterns: self.osc_progress_patterns.clone(),
            screen_rules: self.screen_rules.clone(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ManifestSource {
    Bundled,
    CachedRemote,
    LocalOverride,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RemoteUpdateStatus {
    NotConfigured,
    Missing,
    Active,
    NotNewerThanBundled,
    Invalid,
    Incompatible,
    ShadowedByLocal,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ManifestMetadata {
    pub source: ManifestSource,
    pub version: u32,
    pub cached_remote_version: Option<u32>,
    pub local_override_shadowing: bool,
    pub remote_update_status: RemoteUpdateStatus,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScreenRule {
    pub id: String,
    pub state: ScreenState,
    pub matcher: MatcherKind,
    pub pattern: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ScreenState {
    Idle,
    Working,
    Blocked,
}

impl ScreenState {
    const fn priority(self) -> u8 {
        match self {
            Self::Blocked => 3,
            Self::Working => 2,
            Self::Idle => 1,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MatcherKind {
    /// The trimmed terminal row must equal the pattern.
    Literal,
    /// The pattern may occur anywhere in a terminal row.
    Substring,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ScreenMatch {
    pub agent_id: String,
    pub rule_id: String,
    pub state: ScreenState,
    pub matcher: MatcherKind,
    pub pattern: String,
    pub matched_region: String,
    pub row_from_tail: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentDetectionSource {
    EnvironmentHint,
    ProcessName,
    LaunchCommand,
    OscTitle,
    OscProgress,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AgentDetection {
    pub agent_id: String,
    pub kind: Option<AgentKind>,
    pub source: AgentDetectionSource,
    pub pattern: String,
    pub observed: String,
}

#[derive(Debug, Error)]
pub enum ManifestError {
    #[error("invalid agent manifest JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("invalid agent manifest TOML: {0}")]
    Toml(#[from] toml::de::Error),
    #[error("could not read agent manifest `{path}`: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("invalid agent manifest: {0}")]
    Validation(String),
    #[error("could not compile agent screen rules: {0}")]
    Compile(String),
    #[error("incompatible detection manifest schema {found}; supported schema is {supported}")]
    IncompatibleSchema { found: u32, supported: u32 },
}

impl AgentManifest {
    pub fn from_json(json: &str) -> Result<Self, ManifestError> {
        let manifest: Self = serde_json::from_str(json)?;
        manifest.validate()?;
        Ok(manifest)
    }

    pub fn from_toml(document: &str) -> Result<Self, ManifestError> {
        let manifest: Self = toml::from_str(document)?;
        manifest.validate()?;
        Ok(manifest)
    }

    pub fn bundled() -> Result<Self, ManifestError> {
        Self::from_toml(BUNDLED_MANIFEST)
    }

    /// Replaces bundled definitions with matching user definitions and appends custom agents.
    pub fn with_overlay(mut self, overlay: Self) -> Result<Self, ManifestError> {
        overlay.validate()?;
        for definition in overlay.agents {
            if let Some(index) = self
                .agents
                .iter()
                .position(|current| current.kind.eq_ignore_ascii_case(&definition.kind))
            {
                self.agents[index] = definition;
            } else {
                self.agents.push(definition);
            }
        }
        if overlay.generic_rules.is_some() {
            self.generic_rules = overlay.generic_rules;
        }
        self.version = self.version.max(overlay.version);
        self.validate()?;
        Ok(self)
    }

    pub fn validate(&self) -> Result<(), ManifestError> {
        if self.version == 0 {
            return Err(ManifestError::Validation(
                "manifest version must be greater than zero".to_owned(),
            ));
        }

        let mut kinds = HashSet::new();
        for agent in &self.agents {
            let kind = agent.kind.trim();
            if kind.is_empty() {
                return Err(ManifestError::Validation(
                    "agent kind must not be empty".to_owned(),
                ));
            }
            if !kinds.insert(kind.to_ascii_lowercase()) {
                return Err(ManifestError::Validation(format!(
                    "duplicate agent definition `{kind}`"
                )));
            }
            validate_rules(kind, &agent.screen_rules)?;
        }
        validate_rules("generic", self.generic_rules.as_deref().unwrap_or_default())?;
        Ok(())
    }
}

fn validate_rules(owner: &str, rules: &[ScreenRule]) -> Result<(), ManifestError> {
    let mut ids = HashSet::new();
    for rule in rules {
        if rule.id.trim().is_empty() {
            return Err(ManifestError::Validation(format!(
                "{owner} has a screen rule with an empty id"
            )));
        }
        if !ids.insert(rule.id.to_ascii_lowercase()) {
            return Err(ManifestError::Validation(format!(
                "{owner} has duplicate screen rule `{}`",
                rule.id
            )));
        }
        if rule.pattern.is_empty() {
            return Err(ManifestError::Validation(format!(
                "screen rule `{}` has an empty pattern",
                rule.id
            )));
        }
    }
    Ok(())
}

pub struct CompiledManifest {
    document: AgentManifest,
    agents: Vec<CompiledAgent>,
    generic_rules: CompiledRules,
    metadata: BTreeMap<String, ManifestMetadata>,
}

struct CompiledAgent {
    definition: AgentDefinition,
    rules: CompiledRules,
}

struct CompiledRules {
    rules: Vec<ScreenRule>,
    matcher: Option<AhoCorasick>,
}

impl CompiledManifest {
    pub fn bundled() -> Result<Self, ManifestError> {
        Self::compile(AgentManifest::bundled()?)
    }

    pub fn bundled_with_json_overlay(overlay_json: &str) -> Result<Self, ManifestError> {
        let manifest = AgentManifest::bundled()?.with_overlay(AgentManifest::from_json(overlay_json)?)?;
        Self::compile(manifest)
    }

    pub fn bundled_with_toml_overlay(overlay_toml: &str) -> Result<Self, ManifestError> {
        let manifest = AgentManifest::bundled()?.with_overlay(AgentManifest::from_toml(overlay_toml)?)?;
        Self::compile(manifest)
    }

    /// Resolves per-agent TOML with local > newer compatible cached remote > bundled precedence.
    pub fn load_layered(
        cached_remote_directory: Option<&Path>,
        local_override_directory: Option<&Path>,
    ) -> Result<Self, ManifestError> {
        let bundled = AgentManifest::bundled()?;
        let bundled_version = bundled.version;
        let mut selected_agents = Vec::with_capacity(bundled.agents.len());
        let mut metadata = BTreeMap::new();
        let mut active_version = bundled_version;

        for bundled_agent in &bundled.agents {
            let kind = bundled_agent.kind.clone();
            let remote = read_detection_candidate(cached_remote_directory, &kind, "cached remote");
            let cached_remote_version = remote.valid().map(|manifest| manifest.version);
            let (mut selected, mut source, mut version, mut remote_status) = match &remote {
                CandidateRead::NotConfigured => (
                    bundled_agent.clone(),
                    ManifestSource::Bundled,
                    bundled_version,
                    RemoteUpdateStatus::NotConfigured,
                ),
                CandidateRead::Missing => (
                    bundled_agent.clone(),
                    ManifestSource::Bundled,
                    bundled_version,
                    RemoteUpdateStatus::Missing,
                ),
                CandidateRead::Valid(manifest) if manifest.version > bundled_version => (
                    manifest.definition(),
                    ManifestSource::CachedRemote,
                    manifest.version,
                    RemoteUpdateStatus::Active,
                ),
                CandidateRead::Valid(_) => (
                    bundled_agent.clone(),
                    ManifestSource::Bundled,
                    bundled_version,
                    RemoteUpdateStatus::NotNewerThanBundled,
                ),
                CandidateRead::Invalid => (
                    bundled_agent.clone(),
                    ManifestSource::Bundled,
                    bundled_version,
                    RemoteUpdateStatus::Invalid,
                ),
                CandidateRead::Incompatible => (
                    bundled_agent.clone(),
                    ManifestSource::Bundled,
                    bundled_version,
                    RemoteUpdateStatus::Incompatible,
                ),
            };

            let local = read_detection_candidate(local_override_directory, &kind, "local override");
            let local_override_shadowing = if let CandidateRead::Valid(local) = local {
                selected = local.definition();
                source = ManifestSource::LocalOverride;
                version = local.version;
                remote_status = RemoteUpdateStatus::ShadowedByLocal;
                true
            } else {
                false
            };

            active_version = active_version.max(version);
            metadata.insert(
                kind.to_ascii_lowercase(),
                ManifestMetadata {
                    source,
                    version,
                    cached_remote_version,
                    local_override_shadowing,
                    remote_update_status: remote_status,
                },
            );
            selected_agents.push(selected);
        }

        let selected = AgentManifest {
            version: active_version,
            agents: selected_agents,
            generic_rules: bundled.generic_rules,
        };
        Self::compile_with_metadata(selected, metadata)
    }

    /// Loads the bundled manifest and overlays a local file when it exists.
    pub fn load(user_manifest_path: Option<&Path>) -> Result<Self, ManifestError> {
        let mut manifest = AgentManifest::bundled()?;
        if let Some(path) = user_manifest_path {
            match fs::read_to_string(path) {
                Ok(document) => {
                    let overlay = if path
                        .extension()
                        .and_then(|extension| extension.to_str())
                        .is_some_and(|extension| extension.eq_ignore_ascii_case("toml"))
                    {
                        AgentManifest::from_toml(&document)?
                    } else {
                        AgentManifest::from_json(&document)?
                    };
                    manifest = manifest.with_overlay(overlay)?;
                }
                Err(source) if source.kind() == std::io::ErrorKind::NotFound => {}
                Err(source) => {
                    return Err(ManifestError::Io {
                        path: path.to_owned(),
                        source,
                    });
                }
            }
        }
        Self::compile(manifest)
    }

    pub fn compile(document: AgentManifest) -> Result<Self, ManifestError> {
        let metadata = document
            .agents
            .iter()
            .map(|agent| {
                (
                    agent.kind.to_ascii_lowercase(),
                    ManifestMetadata {
                        source: ManifestSource::Bundled,
                        version: document.version,
                        cached_remote_version: None,
                        local_override_shadowing: false,
                        remote_update_status: RemoteUpdateStatus::NotConfigured,
                    },
                )
            })
            .collect();
        Self::compile_with_metadata(document, metadata)
    }

    fn compile_with_metadata(
        document: AgentManifest,
        metadata: BTreeMap<String, ManifestMetadata>,
    ) -> Result<Self, ManifestError> {
        document.validate()?;
        let mut agents = Vec::with_capacity(document.agents.len());
        for definition in &document.agents {
            agents.push(CompiledAgent {
                definition: definition.clone(),
                rules: CompiledRules::compile(&definition.screen_rules)?,
            });
        }
        let generic_rules = CompiledRules::compile(
            document.generic_rules.as_deref().unwrap_or_default(),
        )?;
        Ok(Self {
            document,
            agents,
            generic_rules,
            metadata,
        })
    }

    pub fn document(&self) -> &AgentManifest {
        &self.document
    }

    pub fn manifest_metadata(&self, agent_id: &str) -> Option<&ManifestMetadata> {
        self.metadata.get(&agent_id.to_ascii_lowercase())
    }

    pub fn is_known_agent(&self, agent_id: &str) -> bool {
        AgentKind::from_str(agent_id).is_ok()
            || self
                .agents
                .iter()
                .any(|agent| agent.definition.kind.eq_ignore_ascii_case(agent_id))
    }

    /// Whether this kind has agent-specific screen rules. Without them the
    /// engine can only fall back to screen stability, which never positively
    /// recognizes the agent's UI.
    pub fn has_screen_rules(&self, agent_id: &str) -> bool {
        self.agents.iter().any(|agent| {
            agent.definition.kind.eq_ignore_ascii_case(agent_id)
                && !agent.definition.screen_rules.is_empty()
        })
    }

    pub fn detect_agent(
        &self,
        process_name: Option<&str>,
        launch_command: Option<&str>,
        osc_title: Option<&str>,
    ) -> Option<AgentDetection> {
        self.detect_agent_with_evidence(
            None,
            process_name,
            launch_command,
            osc_title,
            None,
        )
    }

    pub fn detect_agent_with_evidence(
        &self,
        agent_hint: Option<&str>,
        process_name: Option<&str>,
        launch_command: Option<&str>,
        osc_title: Option<&str>,
        osc_progress: Option<&str>,
    ) -> Option<AgentDetection> {
        if let Some(hint) = agent_hint.filter(|value| !value.trim().is_empty()) {
            if let Some(agent) = self
                .agents
                .iter()
                .find(|agent| agent.definition.kind.eq_ignore_ascii_case(hint.trim()))
            {
                return Some(AgentDetection {
                    agent_id: agent.definition.kind.clone(),
                    kind: AgentKind::from_str(&agent.definition.kind).ok(),
                    source: AgentDetectionSource::EnvironmentHint,
                    pattern: agent.definition.kind.clone(),
                    observed: hint.to_owned(),
                });
            }
        }
        let sources = [
            (AgentDetectionSource::ProcessName, process_name),
            (AgentDetectionSource::LaunchCommand, launch_command),
            (AgentDetectionSource::OscTitle, osc_title),
            (AgentDetectionSource::OscProgress, osc_progress),
        ];

        for (source, observed) in sources {
            let Some(observed) = observed.filter(|value| !value.trim().is_empty()) else {
                continue;
            };
            for agent in &self.agents {
                let patterns = match source {
                    AgentDetectionSource::ProcessName => &agent.definition.process_name_patterns,
                    AgentDetectionSource::LaunchCommand => &agent.definition.launch_command_hints,
                    AgentDetectionSource::OscTitle => &agent.definition.osc_title_patterns,
                    AgentDetectionSource::OscProgress => &agent.definition.osc_progress_patterns,
                    AgentDetectionSource::EnvironmentHint => unreachable!("handled above"),
                };
                if let Some(pattern) = patterns.iter().find(|pattern| match source {
                    AgentDetectionSource::ProcessName => process_pattern_matches(pattern, observed),
                    AgentDetectionSource::LaunchCommand
                    | AgentDetectionSource::OscTitle
                    | AgentDetectionSource::OscProgress => {
                        contains_ascii_case_insensitive(observed, pattern)
                    }
                    AgentDetectionSource::EnvironmentHint => false,
                }) {
                    return Some(AgentDetection {
                        agent_id: agent.definition.kind.clone(),
                        kind: AgentKind::from_str(&agent.definition.kind).ok(),
                        source,
                        pattern: pattern.clone(),
                        observed: observed.to_owned(),
                    });
                }
            }
        }
        None
    }

    /// Matches agent-specific rules first, then the generic safety-net rules.
    pub fn match_screen(&self, agent_id: Option<&str>, snapshot: &str) -> Option<ScreenMatch> {
        let detection_buffer = detection_tail(snapshot);
        if let Some(agent_id) = agent_id {
            if let Some(agent) = self
                .agents
                .iter()
                .find(|agent| agent.definition.kind.eq_ignore_ascii_case(agent_id))
            {
                if let Some(found) = agent.rules.find(&agent.definition.kind, &detection_buffer) {
                    return Some(found);
                }
            }
        }
        self.generic_rules.find("generic", &detection_buffer)
    }
}

enum CandidateRead {
    NotConfigured,
    Missing,
    Valid(DetectionManifest),
    Invalid,
    Incompatible,
}

impl CandidateRead {
    fn valid(&self) -> Option<&DetectionManifest> {
        match self {
            Self::Valid(manifest) => Some(manifest),
            _ => None,
        }
    }
}

fn read_detection_candidate(
    directory: Option<&Path>,
    expected_kind: &str,
    layer: &str,
) -> CandidateRead {
    let Some(directory) = directory else {
        return CandidateRead::NotConfigured;
    };
    let path = directory.join(format!("{expected_kind}.toml"));
    let document = match fs::read_to_string(&path) {
        Ok(document) => document,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return CandidateRead::Missing;
        }
        Err(error) => {
            tracing::warn!(
                path = %path.display(),
                error = %error,
                "ignoring unreadable agent detection manifest"
            );
            return CandidateRead::Invalid;
        }
    };

    match DetectionManifest::from_toml(&document)
        .and_then(|manifest| {
            manifest.validate(Some(expected_kind))?;
            Ok(manifest)
        }) {
        Ok(manifest) => CandidateRead::Valid(manifest),
        Err(ManifestError::IncompatibleSchema { found, supported }) => {
            tracing::warn!(
                path = %path.display(),
                layer,
                found,
                supported,
                "ignoring incompatible agent detection manifest"
            );
            CandidateRead::Incompatible
        }
        Err(error) => {
            tracing::warn!(
                path = %path.display(),
                layer,
                error = %error,
                "ignoring invalid agent detection manifest"
            );
            CandidateRead::Invalid
        }
    }
}

impl CompiledRules {
    fn compile(rules: &[ScreenRule]) -> Result<Self, ManifestError> {
        if rules.is_empty() {
            return Ok(Self {
                rules: Vec::new(),
                matcher: None,
            });
        }
        let patterns: Vec<&str> = rules.iter().map(|rule| rule.pattern.as_str()).collect();
        let matcher = AhoCorasickBuilder::new()
            .ascii_case_insensitive(true)
            .match_kind(MatchKind::Standard)
            .build(patterns)
            .map_err(|error| ManifestError::Compile(error.to_string()))?;
        Ok(Self {
            rules: rules.to_vec(),
            matcher: Some(matcher),
        })
    }

    fn find(&self, agent_id: &str, buffer: &str) -> Option<ScreenMatch> {
        let matcher = self.matcher.as_ref()?;
        let mut best: Option<(u8, usize, usize, usize)> = None;
        for found in matcher.find_overlapping_iter(buffer) {
            let index = found.pattern().as_usize();
            let rule = &self.rules[index];
            let (line_start, line_end) = line_bounds(buffer, found.start(), found.end());
            if rule.matcher == MatcherKind::Literal
                && !buffer[line_start..line_end]
                    .trim()
                    .eq_ignore_ascii_case(rule.pattern.trim())
            {
                continue;
            }
            let candidate = (rule.state.priority(), index, line_start, line_end);
            let should_replace = best
                .map(|current| {
                    candidate.0 > current.0
                        || (candidate.0 == current.0 && candidate.1 < current.1)
                })
                .unwrap_or(true);
            if should_replace {
                best = Some(candidate);
            }
        }

        let (_, index, line_start, line_end) = best?;
        let rule = &self.rules[index];
        let row_index = buffer[..line_start].bytes().filter(|byte| *byte == b'\n').count();
        let row_count = buffer.lines().count().max(1);
        Some(ScreenMatch {
            agent_id: agent_id.to_owned(),
            rule_id: rule.id.clone(),
            state: rule.state,
            matcher: rule.matcher,
            pattern: rule.pattern.clone(),
            matched_region: buffer[line_start..line_end].trim().to_owned(),
            row_from_tail: row_count.saturating_sub(row_index + 1),
        })
    }
}

fn detection_tail(snapshot: &str) -> String {
    let rows: Vec<&str> = snapshot.lines().collect();
    rows[rows.len().saturating_sub(DETECTION_ROWS)..].join("\n")
}

fn line_bounds(buffer: &str, match_start: usize, match_end: usize) -> (usize, usize) {
    let start = buffer[..match_start]
        .rfind('\n')
        .map(|position| position + 1)
        .unwrap_or(0);
    let end = buffer[match_end..]
        .find('\n')
        .map(|position| match_end + position)
        .unwrap_or(buffer.len());
    (start, end)
}

fn contains_ascii_case_insensitive(haystack: &str, needle: &str) -> bool {
    haystack
        .to_ascii_lowercase()
        .contains(&needle.to_ascii_lowercase())
}

fn process_pattern_matches(pattern: &str, observed: &str) -> bool {
    let observed = observed.replace('\\', "/");
    let filename = observed.rsplit('/').next().unwrap_or(&observed);
    let stem = filename
        .strip_suffix(".exe")
        .or_else(|| filename.strip_suffix(".EXE"))
        .unwrap_or(filename);
    let normalized_pattern = pattern
        .strip_suffix(".exe")
        .or_else(|| pattern.strip_suffix(".EXE"))
        .unwrap_or(pattern);
    // Whole names (or whole `-`/`_`/`.` separated words) only: a substring
    // rule let `pi` claim every `python` and `amp` every `example`.
    stem.eq_ignore_ascii_case(normalized_pattern)
        || stem
            .split(['-', '_', '.'])
            .any(|word| word.eq_ignore_ascii_case(normalized_pattern))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_definition_replaces_bundled_definition() {
        let overlay = r#"{
            "agents": [{
                "kind": "codex",
                "process_name_patterns": ["custom-codex"],
                "screen_rules": [{
                    "id": "custom.idle",
                    "state": "idle",
                    "matcher": "literal",
                    "pattern": "READY"
                }]
            }]
        }"#;
        let compiled = CompiledManifest::bundled_with_json_overlay(overlay).unwrap();
        let codex = compiled
            .document()
            .agents
            .iter()
            .find(|agent| agent.kind == "codex")
            .unwrap();
        assert_eq!(codex.process_name_patterns, ["custom-codex"]);
        assert_eq!(codex.screen_rules.len(), 1);
        assert_eq!(
            compiled.match_screen(Some("codex"), "READY").unwrap().rule_id,
            "custom.idle"
        );
    }

    #[test]
    fn detects_agent_from_process_before_weaker_signals() {
        let compiled = CompiledManifest::bundled().unwrap();
        let found = compiled
            .detect_agent(
                Some(r"C:\tools\codex.exe"),
                Some("claude --continue"),
                Some("Gemini CLI"),
            )
            .unwrap();
        assert_eq!(found.kind, Some(AgentKind::Codex));
        assert_eq!(found.source, AgentDetectionSource::ProcessName);
    }

    #[test]
    fn process_names_match_whole_words_only() {
        let compiled = CompiledManifest::bundled().unwrap();
        // `pi` (the first bundled agent) must not claim every `python`, nor
        // `amp` every `example`: process names come from the live tree now.
        for innocent in ["python", "python.exe", "pip", "example", "compiler", "node"] {
            assert!(
                compiled.detect_agent(Some(innocent), None, None).is_none(),
                "{innocent} is not an agent"
            );
        }
        for (name, kind) in [
            ("claude", AgentKind::Claude),
            ("Claude.EXE", AgentKind::Claude),
            (r"C:\Users\x\.local\bin\claude-code.exe", AgentKind::Claude),
            ("gemini-cli", AgentKind::Gemini),
            ("codex", AgentKind::Codex),
        ] {
            assert_eq!(
                compiled.detect_agent(Some(name), None, None).unwrap().kind,
                Some(kind),
                "{name}"
            );
        }
    }

    #[test]
    fn claude_turn_summary_reads_idle_and_a_running_turn_reads_working() {
        let compiled = CompiledManifest::bundled().unwrap();
        // The turn summary reuses the spinner glyph; it must never keep the
        // pane "working" (Cesar's pane sat like this for hours). A draft in
        // the input box is still idle.
        let done = "  ¿Querés que arranque por el 1 y el 2?

            ✻ Cogitated for 5m 10s · done 2:17 p.m.

            ───────────
❯ dale, arranca por el 1 y el 2
───────────
              ⏵⏵ auto mode on (shift+tab to cycle) · ← 1 agent";
        let found = compiled.match_screen(Some("claude"), done).unwrap();
        assert_eq!(found.rule_id, "claude.idle.composer");
        // While the turn runs, the interrupt hint is the working signal even
        // with the input box visible underneath.
        let running = "  reading files…

            ✻ Cogitating… (esc to interrupt · 12s · ↓ 1.2k tokens)

            ───────────
❯ 
───────────
  ⏵⏵ auto mode on (shift+tab to cycle)";
        let found = compiled.match_screen(Some("claude"), running).unwrap();
        assert_eq!(found.rule_id, "claude.working.interrupt");
        // Codex: "› <draft>" is idle too.
        let codex = "• Worked for 1m 3s

› fix the flaky test
";
        assert_eq!(
            compiled.match_screen(Some("codex"), codex).unwrap().rule_id,
            "codex.idle.composer"
        );
    }

    #[test]
    fn explicit_wrapper_hint_identifies_known_agent_before_wrapper_process() {
        let compiled = CompiledManifest::bundled().unwrap();
        let found = compiled
            .detect_agent_with_evidence(
                Some("claude"),
                Some("fence"),
                Some("fence -- claude"),
                None,
                None,
            )
            .unwrap();
        assert_eq!(found.kind, Some(AgentKind::Claude));
        assert_eq!(found.source, AgentDetectionSource::EnvironmentHint);
    }

    #[test]
    fn only_the_last_detection_rows_are_considered() {
        let compiled = CompiledManifest::bundled().unwrap();
        let mut rows = vec!["Do you trust this repository?"];
        rows.extend(std::iter::repeat("ordinary output").take(DETECTION_ROWS));
        assert!(compiled.match_screen(None, &rows.join("\n")).is_none());
    }

    #[test]
    fn bundled_toml_contains_every_builtin_agent_kind() {
        let manifest = AgentManifest::bundled().unwrap();
        assert_eq!(manifest.agents.len(), AgentKind::ALL.len());
        for kind in AgentKind::ALL {
            assert!(manifest
                .agents
                .iter()
                .any(|agent| agent.kind == kind.as_str()));
        }
    }

    #[test]
    fn local_toml_wins_over_newer_cached_remote_and_exposes_metadata() {
        let remote = tempfile::tempdir().unwrap();
        let local = tempfile::tempdir().unwrap();
        let candidate = |version: u32, pattern: &str| {
            format!(
                r#"schema_version = 1
version = {version}
kind = "codex"
process_name_patterns = ["codex"]

[[screen_rules]]
id = "codex.layered"
state = "blocked"
matcher = "substring"
pattern = "{pattern}"
"#
            )
        };
        fs::write(remote.path().join("codex.toml"), candidate(5, "REMOTE")).unwrap();
        fs::write(local.path().join("codex.toml"), candidate(2, "LOCAL")).unwrap();

        let compiled =
            CompiledManifest::load_layered(Some(remote.path()), Some(local.path())).unwrap();
        let found = compiled.match_screen(Some("codex"), "LOCAL approval").unwrap();
        assert_eq!(found.rule_id, "codex.layered");
        let metadata = compiled.manifest_metadata("codex").unwrap();
        assert_eq!(metadata.source, ManifestSource::LocalOverride);
        assert_eq!(metadata.version, 2);
        assert_eq!(metadata.cached_remote_version, Some(5));
        assert!(metadata.local_override_shadowing);
        assert_eq!(metadata.remote_update_status, RemoteUpdateStatus::ShadowedByLocal);
    }

    #[test]
    fn invalid_local_toml_falls_back_to_newer_compatible_remote() {
        let remote = tempfile::tempdir().unwrap();
        let local = tempfile::tempdir().unwrap();
        fs::write(
            remote.path().join("claude.toml"),
            r#"schema_version = 1
version = 3
kind = "claude"

[[screen_rules]]
id = "claude.remote"
state = "idle"
matcher = "literal"
pattern = "REMOTE READY"
"#,
        )
        .unwrap();
        fs::write(local.path().join("claude.toml"), "not = [valid").unwrap();

        let compiled =
            CompiledManifest::load_layered(Some(remote.path()), Some(local.path())).unwrap();
        assert_eq!(
            compiled
                .match_screen(Some("claude"), "REMOTE READY")
                .unwrap()
                .rule_id,
            "claude.remote"
        );
        let metadata = compiled.manifest_metadata("claude").unwrap();
        assert_eq!(metadata.source, ManifestSource::CachedRemote);
        assert_eq!(metadata.remote_update_status, RemoteUpdateStatus::Active);
        assert!(!metadata.local_override_shadowing);
    }
}
