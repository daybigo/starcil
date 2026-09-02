use crate::{PluginError, PluginResult};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandKind {
    Action,
    Event,
    Startup,
    Pane,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandState {
    Running,
    Exited,
    SpawnFailed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandLog {
    pub id: u64,
    pub plugin_id: String,
    pub kind: CommandKind,
    pub command: Vec<String>,
    pub started_unix_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finished_unix_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    pub state: CommandState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub stderr_tail: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub spawn_error: Option<String>,
}

#[derive(Debug)]
struct LogInner {
    by_plugin: HashMap<String, VecDeque<CommandLog>>,
}

#[derive(Debug, Clone)]
pub struct LogStore {
    inner: Arc<Mutex<LogInner>>,
    next_id: Arc<AtomicU64>,
    capacity_per_plugin: usize,
    stderr_tail_bytes: usize,
}

impl LogStore {
    pub fn new(capacity_per_plugin: usize, stderr_tail_bytes: usize) -> Self {
        Self {
            inner: Arc::new(Mutex::new(LogInner { by_plugin: HashMap::new() })),
            next_id: Arc::new(AtomicU64::new(1)),
            capacity_per_plugin: capacity_per_plugin.max(1),
            stderr_tail_bytes,
        }
    }

    pub fn stderr_tail_bytes(&self) -> usize {
        self.stderr_tail_bytes
    }

    pub fn list(&self, plugin_id: Option<&str>, limit: Option<usize>) -> PluginResult<Vec<CommandLog>> {
        let inner = self.inner.lock().map_err(|_| PluginError::LogStorePoisoned)?;
        let mut logs = match plugin_id {
            Some(plugin_id) => inner
                .by_plugin
                .get(plugin_id)
                .map(|entries| entries.iter().cloned().collect::<Vec<_>>())
                .unwrap_or_default(),
            None => inner.by_plugin.values().flat_map(|entries| entries.iter().cloned()).collect(),
        };
        logs.sort_by(|left, right| right.id.cmp(&left.id));
        logs.truncate(limit.unwrap_or(logs.len()));
        Ok(logs)
    }

    pub(crate) fn record_started(
        &self,
        plugin_id: &str,
        kind: CommandKind,
        command: &[String],
        pid: u32,
    ) -> PluginResult<CommandLog> {
        let log = CommandLog {
            id: self.next_id.fetch_add(1, Ordering::Relaxed),
            plugin_id: plugin_id.to_owned(),
            kind,
            command: command.to_vec(),
            started_unix_ms: now_unix_ms(),
            finished_unix_ms: None,
            pid: Some(pid),
            state: CommandState::Running,
            exit_code: None,
            stderr_tail: String::new(),
            spawn_error: None,
        };
        self.push(log.clone())?;
        Ok(log)
    }

    pub(crate) fn record_spawn_failed(
        &self,
        plugin_id: &str,
        kind: CommandKind,
        command: &[String],
        error: &str,
    ) -> PluginResult<CommandLog> {
        let now = now_unix_ms();
        let log = CommandLog {
            id: self.next_id.fetch_add(1, Ordering::Relaxed),
            plugin_id: plugin_id.to_owned(),
            kind,
            command: command.to_vec(),
            started_unix_ms: now,
            finished_unix_ms: Some(now),
            pid: None,
            state: CommandState::SpawnFailed,
            exit_code: None,
            stderr_tail: String::new(),
            spawn_error: Some(error.to_owned()),
        };
        self.push(log.clone())?;
        Ok(log)
    }

    pub(crate) fn record_exit(&self, id: u64, exit_code: Option<i32>, stderr_tail: String) -> PluginResult<()> {
        let mut inner = self.inner.lock().map_err(|_| PluginError::LogStorePoisoned)?;
        for entries in inner.by_plugin.values_mut() {
            if let Some(log) = entries.iter_mut().find(|log| log.id == id) {
                log.finished_unix_ms = Some(now_unix_ms());
                log.state = CommandState::Exited;
                log.exit_code = exit_code;
                log.stderr_tail = stderr_tail;
                return Ok(());
            }
        }
        Ok(())
    }

    fn push(&self, log: CommandLog) -> PluginResult<()> {
        let mut inner = self.inner.lock().map_err(|_| PluginError::LogStorePoisoned)?;
        let entries = inner.by_plugin.entry(log.plugin_id.clone()).or_default();
        entries.push_back(log);
        while entries.len() > self.capacity_per_plugin {
            entries.pop_front();
        }
        Ok(())
    }
}

impl Default for LogStore {
    fn default() -> Self {
        Self::new(100, 16 * 1024)
    }
}

fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(u128::from(u64::MAX)) as u64)
        .unwrap_or_default()
}
