//! Server glue for the plugin system: registry lifecycle, action invocation,
//! event hook fan-out, pane placement (split/tab/zoomed/overlay) and the
//! session-modal popup.

use crate::core::{ApiResult, ServerCore};
use crate::hosttraits::TerminalHost;
use serde_json::{json, Value};
use starcil_domain::{Node, PaneMeta, SplitDirection, WorkspaceId};
use starcil_plugins::{
    ActiveContext, HostEnvironment, LogStore, PaneOpenOptions, PanePlacement, PluginError,
    PluginExecutor, PluginRegistry, RegistryPaths, SourceMetadata,
};
use starcil_protocol::error::{ApiError, ErrorCode};
use std::collections::BTreeMap;
use std::path::PathBuf;

pub struct PluginHost {
    pub registry: PluginRegistry,
    pub executor: PluginExecutor,
}

#[derive(Debug, Clone)]
pub struct PopupState {
    pub terminal_id: String,
    pub plugin_id: Option<String>,
    pub width: Option<String>,
    pub height: Option<String>,
}

fn plugin_err(e: PluginError) -> ApiError {
    e.into()
}

impl<H: TerminalHost> ServerCore<H> {
    /// Initialize the plugin host (called by the async layer at boot with real
    /// paths; tests pass temp dirs).
    pub fn init_plugins(
        &mut self,
        registry_file: PathBuf,
        data_root: PathBuf,
        socket_path: String,
        bin_path: PathBuf,
    ) -> Result<(), ApiError> {
        let paths = RegistryPaths::new(registry_file, data_root);
        let registry = PluginRegistry::open_for_current_binary(paths).map_err(plugin_err)?;
        let host = HostEnvironment::new(socket_path, bin_path, starcil_plugins::Platform::current());
        let executor = PluginExecutor::new(host, LogStore::new(256, 8 * 1024));
        self.plugins = Some(PluginHost { registry, executor });
        Ok(())
    }

    fn plugins(&mut self) -> Result<&mut PluginHost, ApiError> {
        self.plugins
            .as_mut()
            .ok_or_else(|| ApiError::new(ErrorCode::Internal, "plugin host not initialized"))
    }

    fn active_context(&self) -> ActiveContext {
        let pane = self.focused_pane();
        let tab = self.focused_tab();
        ActiveContext {
            workspace_id: Some(self.model.focused_workspace.to_string()),
            tab_id: Some(tab.to_string()),
            pane_id: Some(pane.to_string()),
            worktree: self
                .worktree_provenance
                .get(&self.model.focused_workspace)
                .map(|w| serde_json::to_value(w).unwrap()),
            request_id: None,
        }
    }

    pub(crate) fn plugin_link(&mut self, params: &Value) -> ApiResult {
        let path = params
            .get("path")
            .and_then(Value::as_str)
            .ok_or_else(|| ApiError::invalid_params("missing `path`"))?
            .to_string();
        let enabled = params.get("enabled").and_then(Value::as_bool).unwrap_or(true);
        let source: Option<SourceMetadata> = match params.get("source") {
            None | Some(Value::Null) => None,
            Some(v) => Some(
                serde_json::from_value(v.clone())
                    .map_err(|e| ApiError::invalid_params(format!("invalid source metadata: {e}")))?,
            ),
        };
        let entry = self
            .plugins()?
            .registry
            .link(&path, enabled, source)
            .map_err(plugin_err)?;
        Ok(json!({"type": "plugin_linked", "plugin": entry}))
    }

    pub(crate) fn plugin_list(&mut self) -> ApiResult {
        let entries = self.plugins()?.registry.entries().to_vec();
        Ok(json!({"type": "plugin_list", "plugins": entries}))
    }

    pub(crate) fn plugin_set_enabled(&mut self, params: &Value, enable: bool) -> ApiResult {
        let id = params
            .get("plugin_id")
            .and_then(Value::as_str)
            .ok_or_else(|| ApiError::invalid_params("missing `plugin_id`"))?
            .to_string();
        let host = self.plugins()?;
        let entry = if enable {
            host.registry.enable(&id)
        } else {
            host.registry.disable(&id)
        }
        .map_err(plugin_err)?;
        Ok(json!({"type": if enable { "plugin_enabled" } else { "plugin_disabled" }, "plugin": entry}))
    }

    pub(crate) fn plugin_unlink(&mut self, params: &Value) -> ApiResult {
        let id = params
            .get("plugin_id")
            .and_then(Value::as_str)
            .ok_or_else(|| ApiError::invalid_params("missing `plugin_id`"))?
            .to_string();
        let entry = self.plugins()?.registry.unlink(&id).map_err(plugin_err)?;
        Ok(json!({"type": "plugin_unlinked", "plugin": entry}))
    }

    pub(crate) fn plugin_action_list(&mut self, params: &Value) -> ApiResult {
        let filter = params.get("plugin_id").and_then(Value::as_str).map(str::to_string);
        let host = self.plugins()?;
        let actions = host.executor.action_list(&host.registry, filter.as_deref());
        Ok(json!({"type": "plugin_action_list", "actions": actions}))
    }

    pub(crate) fn plugin_action_invoke(&mut self, params: &Value) -> ApiResult {
        let action_id = params
            .get("action_id")
            .and_then(Value::as_str)
            .ok_or_else(|| ApiError::invalid_params("missing `action_id`"))?
            .to_string();
        let provided = params.get("context").cloned();
        let active = self.active_context();
        let host = self.plugins()?;
        let invocation = host
            .executor
            .invoke_action(&host.registry, &action_id, provided, &active)
            .map_err(plugin_err)?;
        Ok(json!({"type": "plugin_action_invoked", "invocation": invocation}))
    }

    pub(crate) fn plugin_log_list(&mut self, params: &Value) -> ApiResult {
        let filter = params.get("plugin_id").and_then(Value::as_str).map(str::to_string);
        let limit = params.get("limit").and_then(Value::as_u64).map(|n| n as usize);
        let host = self.plugins()?;
        let logs = host
            .executor
            .logs()
            .list(filter.as_deref(), limit)
            .map_err(plugin_err)?;
        Ok(json!({"type": "plugin_log_list", "logs": logs}))
    }

    pub(crate) fn plugin_pane_open(&mut self, params: &Value) -> ApiResult {
        let plugin_id = params
            .get("plugin_id")
            .and_then(Value::as_str)
            .ok_or_else(|| ApiError::invalid_params("missing `plugin_id`"))?
            .to_string();
        let entrypoint = params
            .get("entrypoint")
            .and_then(Value::as_str)
            .ok_or_else(|| ApiError::invalid_params("missing `entrypoint`"))?
            .to_string();
        let options: PaneOpenOptions = PaneOpenOptions {
            placement: match params.get("placement").and_then(Value::as_str) {
                None => None,
                Some(p) => Some(
                    serde_json::from_value(json!(p))
                        .map_err(|_| ApiError::invalid_params(format!("invalid placement `{p}`")))?,
                ),
            },
            width: parse_dimension(params.get("width"))?,
            height: parse_dimension(params.get("height"))?,
            context: params.get("context").cloned(),
            env: params
                .get("env")
                .and_then(Value::as_object)
                .map(|m| {
                    m.iter()
                        .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                        .collect()
                })
                .unwrap_or_default(),
        };
        let active = self.active_context();
        let prepared = {
            let host = self.plugins()?;
            host.executor
                .prepare_pane(&host.registry, &plugin_id, &entrypoint, options, &active)
                .map_err(plugin_err)?
        };
        let focus = params.get("focus").and_then(Value::as_bool).unwrap_or(false);
        let argv = prepared.command.argv.clone();
        let cwd = prepared.command.cwd.display().to_string();
        let env: BTreeMap<String, String> = prepared.command.env.clone().into_iter().collect();

        match prepared.placement {
            PanePlacement::Popup => {
                let term = self.spawn_popup_terminal(&cwd, argv, env)?;
                self.popup = Some(PopupState {
                    terminal_id: term,
                    plugin_id: Some(plugin_id),
                    width: prepared.width.map(|d| format!("{d:?}")),
                    height: prepared.height.map(|d| format!("{d:?}")),
                });
                Ok(json!({"type": "ok"}))
            }
            PanePlacement::Tab => {
                let wid = self.model.focused_workspace;
                let pid = self.model.allocate_pane_id(wid)?;
                let tid = self.model.next_tab_id(wid)?;
                let term = self.spawn_for(&cwd, wid, tid, pid, Some(argv), env)?;
                let meta = PaneMeta {
                    id: pid,
                    terminal_id: term,
                    cwd,
                    label: Some(entrypoint),
                    agent_name: None,
                    env: BTreeMap::new(),
                };
                let tid = self.model.insert_tab_prebuilt(wid, None, Node::Leaf(pid), vec![meta])?;
                if focus {
                    self.model.workspace_mut(wid)?.focused_tab = tid;
                }
                self.emit("pane.created", json!({"pane_id": pid.to_string()}));
                Ok(json!({"type": "plugin_pane_opened", "pane": self.pane_info(pid)?}))
            }
            PanePlacement::Split | PanePlacement::Zoomed | PanePlacement::Overlay => {
                let target = match params.get("target_pane_id").and_then(Value::as_str) {
                    Some(s) => s
                        .parse()
                        .map_err(|_| ApiError::invalid_params(format!("invalid pane id `{s}`")))?,
                    None => self.focused_pane(),
                };
                let tid = self.model.tab_of_pane(target)?;
                let wid = WorkspaceId(target.workspace);
                let pid = self.model.next_pane_id(wid)?;
                let term = self.spawn_for(&cwd, wid, tid, pid, Some(argv), env)?;
                let new_id =
                    self.model
                        .split_pane(target, SplitDirection::Right, 0.5, Some(&cwd), BTreeMap::new(), || term)?;
                if matches!(prepared.placement, PanePlacement::Zoomed | PanePlacement::Overlay) {
                    let tab = self.model.tab_mut(tid)?;
                    tab.zoomed = Some(new_id);
                    tab.focused_pane = new_id;
                } else if focus {
                    self.model.tab_mut(tid)?.focused_pane = new_id;
                }
                self.emit("pane.created", json!({"pane_id": new_id.to_string()}));
                Ok(json!({"type": "plugin_pane_opened", "pane": self.pane_info(new_id)?}))
            }
        }
    }

    fn spawn_popup_terminal(
        &mut self,
        cwd: &str,
        argv: Vec<String>,
        env: BTreeMap<String, String>,
    ) -> Result<String, ApiError> {
        // Popups live outside the pane tree: no STARCIL_PANE_ID, no pane events.
        self.host
            .spawn(crate::hosttraits::TerminalSpawn {
                cwd: cwd.to_string(),
                command: Some(argv),
                env,
                rows: 30,
                cols: 100,
            })
            .map_err(|e| ApiError::new(ErrorCode::Internal, format!("popup spawn failed: {e}")))
    }

    /// plugin.pane.focus: focus a plugin-opened (or any) pane by id.
    pub(crate) fn pane_focus_direction_alias(&mut self, params: &Value) -> ApiResult {
        let pane = self
            .parse_pane_id(params, "pane_id")?
            .ok_or_else(|| ApiError::invalid_params("missing `pane_id`"))?;
        let tid = self.model.tab_of_pane(pane)?;
        self.model.tab_mut(tid)?.focused_pane = pane;
        self.model.workspace_mut(WorkspaceId(tid.workspace))?.focused_tab = tid;
        self.model.focused_workspace = WorkspaceId(tid.workspace);
        self.model.bump();
        self.agents.mark_seen(pane);
        self.emit("pane.focused", json!({"pane_id": pane.to_string()}));
        Ok(json!({"type": "pane_info", "pane": self.pane_info(pane)?}))
    }

    pub(crate) fn popup_close_real(&mut self) -> ApiResult {
        match self.popup.take() {
            Some(p) => {
                let _ = self.host.kill(&p.terminal_id);
                Ok(json!({"type": "ok"}))
            }
            None => Err(ApiError::new(ErrorCode::PopupNotOpen, "no popup is open")),
        }
    }

    /// Fan an emitted event out to plugin event hooks (called by the actor).
    pub fn fan_out_plugin_event(&mut self, event_name: &str, data: &Value) {
        let active = self.active_context();
        if let Some(host) = self.plugins.as_mut() {
            if let Ok(commands) = host
                .executor
                .resolve_event_hooks(&host.registry, event_name, data, &active)
            {
                if !commands.is_empty() {
                    let _ = host.executor.launch_all(commands);
                }
            }
        }
    }

    /// Run [[startup]] hooks once after boot/restore.
    pub fn run_plugin_startup_hooks(&mut self) {
        let active = self.active_context();
        if let Some(host) = self.plugins.as_mut() {
            if let Ok(commands) = host.executor.resolve_startup_hooks(&host.registry, &active) {
                if !commands.is_empty() {
                    let _ = host.executor.launch_all(commands);
                }
            }
        }
    }
}

fn parse_dimension(v: Option<&Value>) -> Result<Option<starcil_plugins::PaneDimension>, ApiError> {
    match v {
        None | Some(Value::Null) => Ok(None),
        Some(val) => serde_json::from_value(val.clone())
            .map(Some)
            .map_err(|_| ApiError::invalid_params("width/height must be cells or a percentage string")),
    }
}
