use crate::{
    CommandKind, CommandLog, LogStore, PaneDimension, PanePlacement, Platform, PluginEntry,
    PluginError, PluginRegistry, PluginResult,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ActiveContext {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tab_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pane_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostEnvironment {
    pub socket_path: String,
    pub bin_path: PathBuf,
    pub platform: Platform,
}

impl HostEnvironment {
    pub fn new(socket_path: impl Into<String>, bin_path: impl Into<PathBuf>, platform: Platform) -> Self {
        Self { socket_path: socket_path.into(), bin_path: bin_path.into(), platform }
    }

    pub fn for_current_platform(socket_path: impl Into<String>, bin_path: impl Into<PathBuf>) -> Self {
        Self::new(socket_path, bin_path, Platform::current())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ActionInfo {
    pub action_id: String,
    pub plugin_id: String,
    pub local_id: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub contexts: Vec<String>,
    pub command: Vec<String>,
    pub platforms: Vec<Platform>,
    pub enabled: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PreparedCommand {
    pub plugin_id: String,
    pub kind: CommandKind,
    pub argv: Vec<String>,
    pub cwd: PathBuf,
    pub env: BTreeMap<String, String>,
    pub context: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ActionInvocation {
    pub context: Value,
    pub log: CommandLog,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct PaneOpenOptions {
    pub placement: Option<PanePlacement>,
    pub width: Option<PaneDimension>,
    pub height: Option<PaneDimension>,
    pub context: Option<Value>,
    pub env: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PreparedPane {
    pub plugin_id: String,
    pub entrypoint: String,
    pub title: String,
    pub placement: PanePlacement,
    pub width: Option<PaneDimension>,
    pub height: Option<PaneDimension>,
    pub command: PreparedCommand,
}

#[derive(Debug, Clone)]
pub struct PluginExecutor {
    host: HostEnvironment,
    logs: LogStore,
}

impl PluginExecutor {
    pub fn new(host: HostEnvironment, logs: LogStore) -> Self {
        Self { host, logs }
    }

    pub fn host(&self) -> &HostEnvironment {
        &self.host
    }

    pub fn logs(&self) -> &LogStore {
        &self.logs
    }

    pub fn action_list(&self, registry: &PluginRegistry, plugin_id: Option<&str>) -> Vec<ActionInfo> {
        let mut actions = Vec::new();
        for entry in registry.entries() {
            if plugin_id.is_some_and(|filter| filter != entry.plugin_id) {
                continue;
            }
            let Some(manifest) = &entry.manifest else { continue };
            for action in &manifest.actions {
                actions.push(ActionInfo {
                    action_id: manifest.qualified_action_id(action),
                    plugin_id: manifest.id.clone(),
                    local_id: action.id.clone(),
                    title: action.title.clone(),
                    contexts: action.contexts.clone(),
                    command: action.command.clone(),
                    platforms: manifest.effective_platforms(action.platforms.as_deref()),
                    enabled: entry.enabled,
                });
            }
        }
        actions.sort_by(|left, right| left.action_id.cmp(&right.action_id));
        actions
    }

    pub fn invoke_action(
        &self,
        registry: &PluginRegistry,
        action_id: &str,
        provided_context: Option<Value>,
        active: &ActiveContext,
    ) -> PluginResult<ActionInvocation> {
        let (entry, action) = find_action(registry, action_id)?;
        if !entry.enabled {
            return Err(PluginError::PluginDisabled(entry.plugin_id.clone()));
        }
        let manifest = entry.manifest.as_ref().expect("find_action only returns loaded manifests");
        ensure_platform(
            manifest.supports(action.platforms.as_deref(), self.host.platform),
            action_id,
            self.host.platform,
        )?;
        let context = build_invocation_context(provided_context, active)?;
        let mut env = self.base_environment(entry, &context);
        env.insert("STARCIL_PLUGIN_ACTION_ID".to_owned(), action_id.to_owned());
        inject_link_context_env(&context, &mut env);
        let prepared = PreparedCommand {
            plugin_id: entry.plugin_id.clone(),
            kind: CommandKind::Action,
            argv: action.command.clone(),
            cwd: entry.plugin_root.clone(),
            env,
            context: context.clone(),
        };
        let log = self.launch(prepared)?;
        Ok(ActionInvocation { context, log })
    }

    pub fn resolve_event_hooks(
        &self,
        registry: &PluginRegistry,
        event_name: &str,
        event_json: &Value,
        active: &ActiveContext,
    ) -> PluginResult<Vec<PreparedCommand>> {
        let context = build_invocation_context(None, active)?;
        let event_json = serde_json::to_string(event_json)
            .map_err(|error| PluginError::InvalidContext(error.to_string()))?;
        let mut commands = Vec::new();
        for entry in registry.entries().iter().filter(|entry| entry.enabled) {
            let Some(manifest) = &entry.manifest else { continue };
            for hook in manifest.events.iter().filter(|hook| hook.on == event_name) {
                if !manifest.supports(hook.platforms.as_deref(), self.host.platform) {
                    continue;
                }
                let mut env = self.base_environment(entry, &context);
                env.insert("STARCIL_PLUGIN_EVENT".to_owned(), event_name.to_owned());
                env.insert("STARCIL_PLUGIN_EVENT_JSON".to_owned(), event_json.clone());
                commands.push(PreparedCommand {
                    plugin_id: entry.plugin_id.clone(),
                    kind: CommandKind::Event,
                    argv: hook.command.clone(),
                    cwd: entry.plugin_root.clone(),
                    env,
                    context: context.clone(),
                });
            }
        }
        Ok(commands)
    }

    pub fn resolve_startup_hooks(
        &self,
        registry: &PluginRegistry,
        active: &ActiveContext,
    ) -> PluginResult<Vec<PreparedCommand>> {
        let context = build_invocation_context(None, active)?;
        let mut commands = Vec::new();
        for entry in registry.entries().iter().filter(|entry| entry.enabled) {
            let Some(manifest) = &entry.manifest else { continue };
            for hook in &manifest.startup {
                if !manifest.supports(hook.platforms.as_deref(), self.host.platform) {
                    continue;
                }
                let mut env = self.base_environment(entry, &context);
                env.insert("STARCIL_PLUGIN_EVENT".to_owned(), "startup".to_owned());
                commands.push(PreparedCommand {
                    plugin_id: entry.plugin_id.clone(),
                    kind: CommandKind::Startup,
                    argv: hook.command.clone(),
                    cwd: entry.plugin_root.clone(),
                    env,
                    context: context.clone(),
                });
            }
        }
        Ok(commands)
    }

    pub fn prepare_pane(
        &self,
        registry: &PluginRegistry,
        plugin_id: &str,
        entrypoint: &str,
        options: PaneOpenOptions,
        active: &ActiveContext,
    ) -> PluginResult<PreparedPane> {
        let entry = registry.get(plugin_id).ok_or_else(|| PluginError::PluginNotFound(plugin_id.to_owned()))?;
        if !entry.enabled {
            return Err(PluginError::PluginDisabled(plugin_id.to_owned()));
        }
        let manifest = entry.manifest.as_ref().ok_or_else(|| PluginError::PluginNotFound(plugin_id.to_owned()))?;
        let pane = manifest
            .panes
            .iter()
            .find(|pane| pane.id == entrypoint)
            .ok_or_else(|| PluginError::PaneNotFound { plugin_id: plugin_id.to_owned(), entrypoint: entrypoint.to_owned() })?;
        ensure_platform(
            manifest.supports(pane.platforms.as_deref(), self.host.platform),
            &format!("{plugin_id}.{entrypoint}"),
            self.host.platform,
        )?;

        let context = build_invocation_context(options.context, active)?;
        let placement = options.placement.unwrap_or(pane.placement);
        let mut env = options.env;
        env.extend(self.base_environment(entry, &context));
        env.insert("STARCIL_PLUGIN_ENTRYPOINT_ID".to_owned(), entrypoint.to_owned());
        if placement == PanePlacement::Popup {
            env.remove("STARCIL_PANE_ID");
        }
        let command = PreparedCommand {
            plugin_id: plugin_id.to_owned(),
            kind: CommandKind::Pane,
            argv: pane.command.clone(),
            cwd: entry.plugin_root.clone(),
            env,
            context,
        };
        Ok(PreparedPane {
            plugin_id: plugin_id.to_owned(),
            entrypoint: entrypoint.to_owned(),
            title: pane.title.clone(),
            placement,
            width: options.width.or_else(|| pane.width.clone()),
            height: options.height.or_else(|| pane.height.clone()),
            command,
        })
    }

    pub fn launch_all(&self, commands: Vec<PreparedCommand>) -> Vec<PluginResult<CommandLog>> {
        commands.into_iter().map(|command| self.launch(command)).collect()
    }

    pub fn launch(&self, prepared: PreparedCommand) -> PluginResult<CommandLog> {
        let Some(program) = prepared.argv.first() else { return Err(PluginError::EmptyCommand) };
        let resolved_program = resolve_program(program);
        let mut command = Command::new(resolved_program);
        command
            .args(&prepared.argv[1..])
            .current_dir(&prepared.cwd)
            .envs(&prepared.env)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());

        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(error) => {
                let _ = self.logs.record_spawn_failed(&prepared.plugin_id, prepared.kind, &prepared.argv, &error.to_string());
                return Err(PluginError::Spawn { command: prepared.argv.join(" "), message: error.to_string() });
            }
        };
        let log = self.logs.record_started(&prepared.plugin_id, prepared.kind, &prepared.argv, child.id())?;
        let stderr = child.stderr.take();
        let logs = self.logs.clone();
        let log_id = log.id;
        let tail_limit = logs.stderr_tail_bytes();
        thread::spawn(move || {
            let stderr_tail = stderr.map(|stderr| read_stderr_tail(stderr, tail_limit)).unwrap_or_default();
            let exit_code = child.wait().ok().and_then(|status| status.code());
            let _ = logs.record_exit(log_id, exit_code, stderr_tail);
        });
        Ok(log)
    }

    fn base_environment(&self, entry: &PluginEntry, context: &Value) -> BTreeMap<String, String> {
        let mut env = BTreeMap::new();
        env.insert("STARCIL_SOCKET_PATH".to_owned(), self.host.socket_path.clone());
        env.insert("STARCIL_BIN_PATH".to_owned(), self.host.bin_path.to_string_lossy().into_owned());
        env.insert("STARCIL_ENV".to_owned(), "1".to_owned());
        env.insert("STARCIL_PLUGIN_ID".to_owned(), entry.plugin_id.clone());
        env.insert("STARCIL_PLUGIN_ROOT".to_owned(), entry.plugin_root.to_string_lossy().into_owned());
        env.insert("STARCIL_PLUGIN_CONFIG_DIR".to_owned(), entry.config_dir.to_string_lossy().into_owned());
        env.insert("STARCIL_PLUGIN_STATE_DIR".to_owned(), entry.state_dir.to_string_lossy().into_owned());
        env.insert(
            "STARCIL_PLUGIN_CONTEXT_JSON".to_owned(),
            serde_json::to_string(context).expect("serde_json::Value serialization cannot fail"),
        );
        for (context_key, env_key) in [
            ("workspace_id", "STARCIL_WORKSPACE_ID"),
            ("tab_id", "STARCIL_TAB_ID"),
            ("pane_id", "STARCIL_PANE_ID"),
        ] {
            if let Some(value) = context.get(context_key).and_then(Value::as_str) {
                env.insert(env_key.to_owned(), value.to_owned());
            }
        }
        env
    }
}

pub fn build_invocation_context(provided: Option<Value>, active: &ActiveContext) -> PluginResult<Value> {
    let mut context = match provided {
        None | Some(Value::Null) => Map::new(),
        Some(Value::Object(context)) => context,
        Some(_) => return Err(PluginError::InvalidContext("context must be a JSON object".to_owned())),
    };
    fill_string(&mut context, "workspace_id", active.workspace_id.as_deref());
    fill_string(&mut context, "tab_id", active.tab_id.as_deref());
    fill_string(&mut context, "pane_id", active.pane_id.as_deref());
    if !context.contains_key("worktree") {
        if let Some(worktree) = &active.worktree {
            context.insert("worktree".to_owned(), worktree.clone());
        }
    }
    fill_string(&mut context, "request_id", active.request_id.as_deref());
    Ok(Value::Object(context))
}

fn find_action<'a>(registry: &'a PluginRegistry, action_id: &str) -> PluginResult<(&'a PluginEntry, &'a crate::ActionSpec)> {
    for entry in registry.entries() {
        let Some(manifest) = &entry.manifest else { continue };
        for action in &manifest.actions {
            if manifest.qualified_action_id(action) == action_id {
                return Ok((entry, action));
            }
        }
    }
    Err(PluginError::ActionNotFound(action_id.to_owned()))
}

fn ensure_platform(supported: bool, item: &str, platform: Platform) -> PluginResult<()> {
    if supported {
        Ok(())
    } else {
        Err(PluginError::PlatformUnsupported { item: item.to_owned(), platform: platform.as_str().to_owned() })
    }
}

fn fill_string(context: &mut Map<String, Value>, key: &str, value: Option<&str>) {
    if !context.contains_key(key) {
        if let Some(value) = value {
            context.insert(key.to_owned(), Value::String(value.to_owned()));
        }
    }
}

fn inject_link_context_env(context: &Value, env: &mut BTreeMap<String, String>) {
    for (context_key, env_key) in [
        ("clicked_url", "STARCIL_PLUGIN_CLICKED_URL"),
        ("link_handler_id", "STARCIL_PLUGIN_LINK_HANDLER_ID"),
    ] {
        if let Some(value) = context.get(context_key).and_then(Value::as_str) {
            env.insert(env_key.to_owned(), value.to_owned());
        }
    }
}

fn read_stderr_tail(mut stderr: impl Read, limit: usize) -> String {
    if limit == 0 {
        let mut sink = [0_u8; 4096];
        while stderr.read(&mut sink).ok().is_some_and(|read| read > 0) {}
        return String::new();
    }
    let mut tail = Vec::new();
    let mut buffer = [0_u8; 4096];
    loop {
        let read = match stderr.read(&mut buffer) {
            Ok(0) | Err(_) => break,
            Ok(read) => read,
        };
        tail.extend_from_slice(&buffer[..read]);
        if tail.len() > limit {
            tail.drain(..tail.len() - limit);
        }
    }
    String::from_utf8_lossy(&tail).into_owned()
}

#[cfg(windows)]
fn resolve_program(program: &str) -> PathBuf {
    let requested = Path::new(program);
    if requested.components().count() > 1 || requested.extension().is_some() {
        return requested.to_path_buf();
    }
    let Some(path) = std::env::var_os("PATH") else { return requested.to_path_buf() };
    let extensions = std::env::var("PATHEXT")
        .unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".to_owned())
        .split(';')
        .filter(|extension| !extension.is_empty())
        .map(str::to_owned)
        .collect::<Vec<_>>();
    for directory in std::env::split_paths(&path) {
        let exact = directory.join(program);
        if exact.is_file() {
            return exact;
        }
        for extension in &extensions {
            let candidate = directory.join(format!("{program}{extension}"));
            if candidate.is_file() {
                return candidate;
            }
        }
    }
    requested.to_path_buf()
}

#[cfg(not(windows))]
fn resolve_program(program: &str) -> PathBuf {
    PathBuf::from(program)
}
