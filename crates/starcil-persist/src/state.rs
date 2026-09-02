use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
#[cfg(unix)]
use std::fs::File;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use starcil_domain::{PaneId, SessionModel, TabId, WorkspaceId};
use thiserror::Error;

pub const CURRENT_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StateDoc {
    pub schema_version: u32,
    /// Milliseconds since the Unix epoch.
    pub saved_at: u64,
    pub session: String,
    #[serde(with = "session_model_serde")]
    pub model: SessionModel,
    #[serde(default, with = "pane_extras_serde")]
    pub pane_extras: BTreeMap<PaneId, PaneExtras>,
    pub focused_workspace: WorkspaceId,
    pub focused_tab: TabId,
    pub focused_pane: PaneId,
}

impl StateDoc {
    /// Builds a state document and strips process environment maps so secrets
    /// are not persisted as part of the otherwise serde-ready session model.
    pub fn new(
        session: impl Into<String>,
        mut model: SessionModel,
        pane_extras: BTreeMap<PaneId, PaneExtras>,
    ) -> Result<Self, StateDocError> {
        let focused_workspace = model.focused_workspace;
        let focused_tab = model
            .workspace(focused_workspace)
            .map_err(|error| StateDocError::InvalidFocus(error.to_string()))?
            .focused_tab;
        let focused_pane = model
            .tab(focused_tab)
            .map_err(|error| StateDocError::InvalidFocus(error.to_string()))?
            .focused_pane;

        for workspace in &mut model.workspaces {
            workspace.env.clear();
        }
        for pane in model.panes.values_mut() {
            pane.env.clear();
        }

        Ok(Self {
            schema_version: CURRENT_SCHEMA_VERSION,
            saved_at: unix_time_millis(),
            session: session.into(),
            model,
            pane_extras,
            focused_workspace,
            focused_tab,
            focused_pane,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct PaneExtras {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_session: Option<SessionRef>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recipe_argv: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionRef {
    pub source: String,
    pub agent: String,
    pub kind: String,
    pub value: String,
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum StateDocError {
    #[error("state document has invalid focus: {0}")]
    InvalidFocus(String),
}

#[derive(Debug, Error)]
pub enum SaveError {
    #[error("cannot save schema version {found}; supported version is {supported}")]
    UnsupportedSchemaVersion { found: u32, supported: u32 },
    #[error("failed to serialize state document: {0}")]
    Serialize(#[from] serde_json::Error),
    #[error("state file I/O failed at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

#[derive(Debug, Error)]
pub enum LoadError {
    #[error("state file I/O failed at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("corrupted state file {path}: {message}")]
    Corrupt { path: PathBuf, message: String },
    #[error("state schema version {found} is unsupported; supported version is {supported}")]
    UnsupportedSchemaVersion { found: u32, supported: u32 },
    #[error("primary state failed ({primary}); backup also failed ({backup})")]
    PrimaryAndBackupFailed { primary: String, backup: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum LoadWarning {
    RecoveredFromBackup {
        backup_path: PathBuf,
        primary_error: String,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LoadOutcome {
    pub doc: StateDoc,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warning: Option<LoadWarning>,
}

pub fn save_atomic(path: impl AsRef<Path>, doc: &StateDoc) -> Result<(), SaveError> {
    if doc.schema_version != CURRENT_SCHEMA_VERSION {
        return Err(SaveError::UnsupportedSchemaVersion {
            found: doc.schema_version,
            supported: CURRENT_SCHEMA_VERSION,
        });
    }
    let path = path.as_ref();
    ensure_parent(path)?;
    let mut encoded = serde_json::to_vec_pretty(doc)?;
    encoded.push(b'\n');

    let backup = backup_path(path);
    let previous = fs::read(path).ok().and_then(|bytes| {
        decode_state_doc(path, &bytes)
            .ok()
            .map(|_| bytes)
    });
    if let Some(previous) = previous {
        write_atomic_bytes(&backup, &previous)?;
    }

    write_atomic_bytes(path, &encoded)?;
    if !backup.is_file() {
        write_atomic_bytes(&backup, &encoded)?;
    }
    Ok(())
}

pub fn load(path: impl AsRef<Path>) -> Result<LoadOutcome, LoadError> {
    let path = path.as_ref();
    match load_exact(path) {
        Ok(doc) => Ok(LoadOutcome { doc, warning: None }),
        Err(primary_error) => {
            let backup = backup_path(path);
            if !backup.is_file() {
                return Err(primary_error);
            }
            match load_exact(&backup) {
                Ok(doc) => Ok(LoadOutcome {
                    doc,
                    warning: Some(LoadWarning::RecoveredFromBackup {
                        backup_path: backup,
                        primary_error: primary_error.to_string(),
                    }),
                }),
                Err(backup_error) => Err(LoadError::PrimaryAndBackupFailed {
                    primary: primary_error.to_string(),
                    backup: backup_error.to_string(),
                }),
            }
        }
    }
}

pub fn backup_path(path: impl AsRef<Path>) -> PathBuf {
    sibling_with_suffix(path.as_ref(), ".bak")
}

pub fn temporary_path(path: impl AsRef<Path>) -> PathBuf {
    sibling_with_suffix(path.as_ref(), ".tmp")
}

fn load_exact(path: &Path) -> Result<StateDoc, LoadError> {
    let encoded = fs::read(path).map_err(|source| LoadError::Io {
        path: path.to_owned(),
        source,
    })?;
    decode_state_doc(path, &encoded)
}

fn decode_state_doc(path: &Path, encoded: &[u8]) -> Result<StateDoc, LoadError> {
    let value: Value = serde_json::from_slice(encoded).map_err(|error| LoadError::Corrupt {
        path: path.to_owned(),
        message: format!("invalid or truncated JSON: {error}"),
    })?;
    let version = value
        .get("schema_version")
        .and_then(Value::as_u64)
        .and_then(|version| u32::try_from(version).ok())
        .ok_or_else(|| LoadError::Corrupt {
            path: path.to_owned(),
            message: "missing unsigned integer schema_version".to_owned(),
        })?;
    migrate_value(path, version, value)
}

fn migrate_value(path: &Path, version: u32, value: Value) -> Result<StateDoc, LoadError> {
    match version {
        CURRENT_SCHEMA_VERSION => serde_json::from_value(value).map_err(|error| {
            LoadError::Corrupt {
                path: path.to_owned(),
                message: format!("schema v1 document is invalid: {error}"),
            }
        }),
        found => Err(LoadError::UnsupportedSchemaVersion {
            found,
            supported: CURRENT_SCHEMA_VERSION,
        }),
    }
}

fn write_atomic_bytes(path: &Path, encoded: &[u8]) -> Result<(), SaveError> {
    ensure_parent(path)?;
    let temporary = temporary_path(path);
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&temporary)
        .map_err(|source| SaveError::Io {
            path: temporary.clone(),
            source,
        })?;
    file.write_all(encoded).map_err(|source| SaveError::Io {
        path: temporary.clone(),
        source,
    })?;
    file.sync_all().map_err(|source| SaveError::Io {
        path: temporary.clone(),
        source,
    })?;
    drop(file);
    fs::rename(&temporary, path).map_err(|source| SaveError::Io {
        path: path.to_owned(),
        source,
    })?;
    sync_parent(path)?;
    Ok(())
}

fn ensure_parent(path: &Path) -> Result<(), SaveError> {
    let parent = path.parent().filter(|parent| !parent.as_os_str().is_empty());
    if let Some(parent) = parent {
        fs::create_dir_all(parent).map_err(|source| SaveError::Io {
            path: parent.to_owned(),
            source,
        })?;
    }
    Ok(())
}

#[cfg(unix)]
fn sync_parent(path: &Path) -> Result<(), SaveError> {
    if let Some(parent) = path.parent().filter(|parent| !parent.as_os_str().is_empty()) {
        File::open(parent)
            .and_then(|directory| directory.sync_all())
            .map_err(|source| SaveError::Io {
                path: parent.to_owned(),
                source,
            })?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn sync_parent(_path: &Path) -> Result<(), SaveError> {
    Ok(())
}

fn sibling_with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut value = path.as_os_str().to_os_string();
    value.push(suffix);
    PathBuf::from(value)
}

fn unix_time_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

mod pane_extras_serde {
    use super::*;
    use serde::de::Error as _;

    pub fn serialize<S>(
        panes: &BTreeMap<PaneId, PaneExtras>,
        serializer: S,
    ) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let string_keys: BTreeMap<String, &PaneExtras> = panes
            .iter()
            .map(|(pane_id, extras)| (pane_id.to_string(), extras))
            .collect();
        string_keys.serialize(serializer)
    }

    pub fn deserialize<'de, D>(
        deserializer: D,
    ) -> Result<BTreeMap<PaneId, PaneExtras>, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let string_keys = BTreeMap::<String, PaneExtras>::deserialize(deserializer)?;
        string_keys
            .into_iter()
            .map(|(key, value)| {
                key.parse()
                    .map(|pane_id| (pane_id, value))
                    .map_err(D::Error::custom)
            })
            .collect()
    }
}

mod session_model_serde {
    use super::*;
    use serde::de::{DeserializeSeed, Error as _, IntoDeserializer, MapAccess, Visitor};
    use serde::ser::Error as _;

    pub fn serialize<S>(model: &SessionModel, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut without_panes = model.clone();
        without_panes.panes.clear();
        let mut value = serde_json::to_value(without_panes).map_err(S::Error::custom)?;
        let object = value
            .as_object_mut()
            .ok_or_else(|| S::Error::custom("SessionModel did not serialize as an object"))?;
        let panes = model
            .panes
            .iter()
            .map(|(pane_id, pane)| {
                serde_json::to_value(pane)
                    .map(|value| (pane_id.to_string(), value))
            })
            .collect::<Result<serde_json::Map<String, Value>, _>>()
            .map_err(S::Error::custom)?;
        object.insert("panes".to_owned(), Value::Object(panes));
        value.serialize(serializer)
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<SessionModel, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = Value::deserialize(deserializer)?;
        deserialize_value(value).map_err(D::Error::custom)
    }

    fn deserialize_value(value: Value) -> Result<SessionModel, serde_json::Error> {
        let mut object = value
            .as_object()
            .cloned()
            .ok_or_else(|| <serde_json::Error as serde::de::Error>::custom(
                "SessionModel must be an object",
            ))?;
        let panes = object
            .remove("panes")
            .unwrap_or_else(|| Value::Object(serde_json::Map::new()));
        let pane_entries = panes
            .as_object()
            .ok_or_else(|| <serde_json::Error as serde::de::Error>::custom(
                "SessionModel panes must be an object",
            ))?
            .iter()
            .map(|(pane_id, pane)| {
                pane_id
                    .parse::<PaneId>()
                    .map(|pane_id| (pane_id, pane.clone()))
                    .map_err(<serde_json::Error as serde::de::Error>::custom)
            })
            .collect::<Result<Vec<_>, _>>()?;

        let mut fields: Vec<(String, ModelFieldValue)> = object
            .into_iter()
            .map(|(key, value)| (key, ModelFieldValue::Json(value)))
            .collect();
        fields.push(("panes".to_owned(), ModelFieldValue::Panes(pane_entries)));
        let deserializer = serde::de::value::MapDeserializer::<_, serde_json::Error>::new(
            fields.into_iter(),
        );
        SessionModel::deserialize(deserializer)
    }

    enum ModelFieldValue {
        Json(Value),
        Panes(Vec<(PaneId, Value)>),
    }

    impl<'de> IntoDeserializer<'de, serde_json::Error> for ModelFieldValue {
        type Deserializer = Self;

        fn into_deserializer(self) -> Self::Deserializer {
            self
        }
    }

    impl<'de> serde::Deserializer<'de> for ModelFieldValue {
        type Error = serde_json::Error;

        fn deserialize_any<V>(self, visitor: V) -> Result<V::Value, Self::Error>
        where
            V: Visitor<'de>,
        {
            match self {
                Self::Json(value) => value.into_deserializer().deserialize_any(visitor),
                Self::Panes(entries) => visitor.visit_map(PaneMapAccess {
                    entries: entries.into_iter(),
                    pending_value: None,
                }),
            }
        }

        serde::forward_to_deserialize_any! {
            bool i8 i16 i32 i64 u8 u16 u32 u64 f32 f64 char str string
            bytes byte_buf option unit unit_struct newtype_struct seq tuple
            tuple_struct map struct enum identifier ignored_any
        }
    }

    struct PaneMapAccess {
        entries: std::vec::IntoIter<(PaneId, Value)>,
        pending_value: Option<Value>,
    }

    impl<'de> MapAccess<'de> for PaneMapAccess {
        type Error = serde_json::Error;

        fn next_key_seed<K>(&mut self, seed: K) -> Result<Option<K::Value>, Self::Error>
        where
            K: DeserializeSeed<'de>,
        {
            let Some((pane_id, value)) = self.entries.next() else {
                return Ok(None);
            };
            self.pending_value = Some(value);
            let pane_id = serde_json::to_value(pane_id)?;
            seed.deserialize(pane_id.into_deserializer()).map(Some)
        }

        fn next_value_seed<V>(&mut self, seed: V) -> Result<V::Value, Self::Error>
        where
            V: DeserializeSeed<'de>,
        {
            let value = self.pending_value.take().ok_or_else(|| {
                <serde_json::Error as serde::de::Error>::custom(
                    "pane map value requested before key",
                )
            })?;
            seed.deserialize(value.into_deserializer())
        }

        fn size_hint(&self) -> Option<usize> {
            Some(self.entries.len())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_doc(session: &str) -> StateDoc {
        let model = SessionModel::new("C:/dev", || "term_old".to_owned());
        let mut doc = StateDoc::new(session, model, BTreeMap::new()).unwrap();
        doc.saved_at = 123;
        doc
    }

    #[test]
    fn save_and_load_round_trip() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("state.json");
        let expected = sample_doc("roundtrip");

        save_atomic(&path, &expected).unwrap();
        let loaded = load(&path).unwrap();
        assert_eq!(loaded.doc, expected);
        assert_eq!(loaded.warning, None);
        assert!(backup_path(&path).is_file());
    }

    #[test]
    fn abandoned_garbage_temp_does_not_damage_primary() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("state.json");
        let expected = sample_doc("before-crash");
        save_atomic(&path, &expected).unwrap();

        fs::write(temporary_path(&path), b"{garbage").unwrap();
        let loaded = load(&path).unwrap();
        assert_eq!(loaded.doc, expected);
        assert_eq!(loaded.warning, None);
    }

    #[test]
    fn corrupted_primary_falls_back_to_last_good_backup() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("state.json");
        let first = sample_doc("first");
        let second = sample_doc("second");
        save_atomic(&path, &first).unwrap();
        save_atomic(&path, &second).unwrap();
        fs::write(&path, b"{\"schema_version\":1").unwrap();

        let loaded = load(&path).unwrap();
        assert_eq!(loaded.doc, first);
        assert!(matches!(
            loaded.warning,
            Some(LoadWarning::RecoveredFromBackup { .. })
        ));
    }

    #[test]
    fn schema_zero_is_rejected_with_migration_error() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("state.json");
        fs::write(&path, br#"{"schema_version":0}"#).unwrap();

        let error = load(&path).unwrap_err();
        assert!(matches!(
            error,
            LoadError::UnsupportedSchemaVersion {
                found: 0,
                supported: CURRENT_SCHEMA_VERSION
            }
        ));
        assert!(error.to_string().contains("unsupported"));
    }
}
