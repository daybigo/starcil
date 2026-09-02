//! Method dispatcher: every name in starcil_protocol::methods::ALL resolves
//! here. Structural methods are implemented against the domain model; methods
//! owned by later phases return a typed `internal` error naming the gap (the
//! stub list is asserted by tests and must shrink to empty before release).

use crate::core::{parse_direction, parse_env_map, parse_nav_direction, ApiResult, ServerCore};
use crate::hosttraits::{ReadFormat, ReadSource, TerminalHost};
use serde_json::{json, Value};
use starcil_domain::{PaneId, SplitDirection, TabId, WorkspaceId};
use starcil_protocol::error::{ApiError, ErrorCode};
use starcil_protocol::methods;
use starcil_protocol::types::PortableLayoutNode;
use std::collections::BTreeMap;

/// Methods intentionally not implemented yet (wired by later phases/lanes).
pub const STUBBED: &[&str] = &[
    "server.stop",
    "pane.wait_for_output",
    "agent.wait",
    "agent.attach",
    "events.subscribe",
    "events.wait",
];

impl<H: TerminalHost> ServerCore<H> {
    pub fn handle(&mut self, method: &str, params: &Value) -> ApiResult {
        let revision_before = self.model.revision;
        let result = self.handle_inner(method, params);
        // Any structural mutation (split/close/resize/zoom/move/…) must
        // immediately re-fit every PTY to its new layout rect, or the app
        // inside keeps rendering at the stale size. Same-size resizes are
        // no-ops at the terminal layer, so over-calling here is free.
        if self.model.revision != revision_before {
            self.sync_pty_sizes();
        }
        result
    }

    fn handle_inner(&mut self, method: &str, params: &Value) -> ApiResult {
        if !methods::is_known(method) {
            return Err(ApiError::new(ErrorCode::UnknownMethod, format!("unknown method `{method}`")));
        }
        match method {
            "ping" => Ok(json!({
                "type": "pong",
                "version": self.version,
                "protocol_major": starcil_protocol::PROTOCOL_MAJOR,
                "protocol_minor": starcil_protocol::PROTOCOL_MINOR,
                "session": self.session_name,
            })),
            "session.snapshot" => self.session_snapshot(),
            // workspace
            "workspace.create" => self.workspace_create(params),
            "workspace.list" => self.workspace_list(),
            "workspace.get" => self.workspace_get(params),
            "workspace.focus" => self.workspace_focus(params),
            "workspace.rename" => self.workspace_rename(params),
            "workspace.close" => { let r = self.workspace_close(params); self.gc_agents(); r }
            // tab
            "tab.create" => self.tab_create(params),
            "tab.list" => self.tab_list(params),
            "tab.get" => self.tab_get(params),
            "tab.focus" => self.tab_focus(params),
            "tab.rename" => self.tab_rename(params),
            "tab.close" => { let r = self.tab_close(params); self.gc_agents(); r }
            // pane
            "pane.split" => self.pane_split(params),
            "pane.close" => { let r = self.pane_close(params); self.gc_agents(); r }
            "pane.list" => self.pane_list(params),
            "pane.get" => self.pane_get(params),
            "pane.current" => self.pane_current(params),
            "pane.rename" => self.pane_rename(params),
            "pane.read" => self.pane_read(params),
            "pane.send_text" => self.pane_send_text(params),
            "pane.send_keys" => self.pane_send_keys(params),
            "pane.run" => self.pane_run(params),
            "pane.zoom" => self.pane_zoom(params),
            "pane.swap" => self.pane_swap(params),
            "pane.move" => { let r = self.pane_move(params); self.gc_agents(); r }
            "pane.layout" => self.pane_layout(params),
            "pane.neighbor" => self.pane_neighbor(params),
            "pane.edges" => self.pane_edges(params),
            "pane.focus_direction" => self.pane_focus_direction(params),
            "pane.focus" => self.pane_focus_direction_alias(params),
            "pane.resize" => self.pane_resize(params),
            "pane.process_info" => self.pane_process_info(params),
            // metadata + notification
            "workspace.report_metadata" => self.workspace_report_metadata(params),
            "pane.report_metadata" => self.pane_report_metadata(params),
            "notification.show" => self.notification_show(params),
            // agents
            "agent.list" => self.agent_list_handler(),
            "agent.get" => self.agent_get_handler(params),
            "agent.read" => self.agent_read_handler(params),
            "agent.send_keys" => self.agent_send_keys_handler(params),
            "agent.prompt" => self.agent_prompt_handler(params),
            "agent.rename" => self.agent_rename_handler(params),
            "agent.focus" => self.agent_focus_handler(params),
            "agent.explain" => self.agent_explain_handler(params),
            "agent.start" => self.agent_start_handler(params),
            "pane.report_agent" => self.report_agent_handler(params),
            "pane.report_agent_session" => self.report_agent_session_handler(params),
            "pane.release_agent" => self.release_agent_handler(params),
            "pane.clear_agent_authority" => self.clear_agent_authority_handler(params),
            "pane.send_input" => self.pane_send_input(params),
            "pane.graphics.info" | "pane.graphics.set" | "pane.graphics.clear" | "pane.graphics.stream" => {
                self.graphics_gate()
            }
            "popup.close" => self.popup_close_real(),
            "plugin.link" => self.plugin_link(params),
            "plugin.list" => self.plugin_list(),
            "plugin.unlink" => self.plugin_unlink(params),
            "plugin.enable" => self.plugin_set_enabled(params, true),
            "plugin.disable" => self.plugin_set_enabled(params, false),
            "plugin.action.list" => self.plugin_action_list(params),
            "plugin.action.invoke" => self.plugin_action_invoke(params),
            "plugin.log.list" => self.plugin_log_list(params),
            "plugin.pane.open" => self.plugin_pane_open(params),
            "plugin.pane.focus" => self.pane_focus_direction_alias(params),
            "plugin.pane.close" => { let r = self.pane_close(params); self.gc_agents(); r }
            "agent.view.set" => self.agent_view_set(params),
            "agent.view.clear" => self.agent_view_clear(params),
            "integration.install" => self.integration_install(params, false),
            "integration.uninstall" => self.integration_install(params, true),
            "integration.status" => self.integration_status(params),
            // reorders + worktrees + admin (dispatch_ext)
            "workspace.move" => self.workspace_move(params),
            "workspace.move_block" => self.workspace_move_block(params),
            "tab.move" => self.tab_move(params),
            "layout.apply" => self.layout_apply(params),
            "worktree.list" => self.worktree_list(params),
            "worktree.create" => self.worktree_create(params),
            "worktree.open" => self.worktree_open(params),
            "worktree.remove" => self.worktree_remove(params),
            "client.window_title.set" => self.window_title_set(params),
            "client.window_title.clear" => self.window_title_clear(),
            "server.reload_config" => self.reload_config_handler(),
            "server.agent_manifests" => self.agent_manifests_status(false),
            "server.reload_agent_manifests" => self.agent_manifests_status(true),
            // layout
            "layout.export" => self.layout_export(params),
            "layout.set_split_ratio" => self.layout_set_split_ratio(params),
            other => Err(ApiError::new(
                ErrorCode::Internal,
                format!("not implemented yet: {other} (tracked in dispatch::STUBBED)"),
            )),
        }
    }

    fn session_snapshot(&self) -> ApiResult {
        let mut workspaces = Vec::new();
        let mut tabs = Vec::new();
        let mut panes = Vec::new();
        let mut layouts = Vec::new();
        for ws in &self.model.workspaces {
            workspaces.push(self.workspace_info(ws.id)?);
            for tab in &ws.tabs {
                tabs.push(self.tab_info(tab.id)?);
                layouts.push(self.layout_snapshot(tab.id)?);
                for p in tab.tree.panes() {
                    panes.push(self.pane_info(p)?);
                }
            }
        }
        let focused_tab = self.focused_tab();
        let agents = self
            .agent_list_handler()?
            .get("agents")
            .cloned()
            .unwrap_or_else(|| json!([]));
        Ok(json!({
            "type": "session_snapshot",
            "version": self.version,
            "protocol_major": starcil_protocol::PROTOCOL_MAJOR,
            "protocol_minor": starcil_protocol::PROTOCOL_MINOR,
            "session": self.session_name,
            "revision": self.model.revision,
            "focused_workspace_id": self.model.focused_workspace.to_string(),
            "focused_tab_id": focused_tab.to_string(),
            "focused_pane_id": self.focused_pane().to_string(),
            "workspaces": workspaces,
            "tabs": tabs,
            "panes": panes,
            "layouts": layouts,
            "agents": agents,
        }))
    }

    // ---- workspace ----

    fn workspace_create(&mut self, params: &Value) -> ApiResult {
        let cwd = params
            .get("cwd")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| self.default_cwd());
        let label = params.get("label").and_then(Value::as_str).map(str::to_string);
        let env = parse_env_map(params)?;
        let focus = params.get("focus").and_then(Value::as_bool).unwrap_or(false);
        let (wid, tid, pid) = self.model.next_workspace_initial_ids();
        let term = self.spawn_for(&cwd, wid, tid, pid, None, env.clone())?;
        let (wid, tid, pid) = self.model.create_workspace(&cwd, label, env, || term);
        if focus {
            self.model.focused_workspace = wid;
        }
        self.emit("workspace.created", json!({"workspace_id": wid.to_string()}));
        self.emit("tab.created", json!({"tab_id": tid.to_string()}));
        self.emit("pane.created", json!({"pane_id": pid.to_string()}));
        Ok(json!({
            "type": "workspace_created",
            "workspace": self.workspace_info(wid)?,
            "tab": self.tab_info(tid)?,
            "root_pane": self.pane_info(pid)?,
        }))
    }

    fn workspace_list(&self) -> ApiResult {
        let mut out = Vec::new();
        for ws in &self.model.workspaces {
            out.push(self.workspace_info(ws.id)?);
        }
        Ok(json!({"type": "workspace_list", "workspaces": out}))
    }

    fn parse_workspace(&self, params: &Value, key: &str) -> Result<WorkspaceId, ApiError> {
        let s = params
            .get(key)
            .and_then(Value::as_str)
            .ok_or_else(|| ApiError::invalid_params(format!("missing `{key}`")))?;
        let id = s
            .parse::<WorkspaceId>()
            .map_err(|_| ApiError::invalid_params(format!("invalid workspace id `{s}`")))?;
        self.model.workspace(id)?;
        Ok(id)
    }

    fn workspace_get(&self, params: &Value) -> ApiResult {
        let id = self.parse_workspace(params, "workspace_id")?;
        Ok(json!({"type": "workspace_info", "workspace": self.workspace_info(id)?}))
    }

    fn workspace_focus(&mut self, params: &Value) -> ApiResult {
        let id = self.parse_workspace(params, "workspace_id")?;
        self.model.focused_workspace = id;
        self.model.bump();
        self.emit("workspace.focused", json!({"workspace_id": id.to_string()}));
        Ok(json!({"type": "workspace_info", "workspace": self.workspace_info(id)?}))
    }

    fn workspace_rename(&mut self, params: &Value) -> ApiResult {
        let id = self.parse_workspace(params, "workspace_id")?;
        let label = params
            .get("label")
            .and_then(Value::as_str)
            .ok_or_else(|| ApiError::invalid_params("missing `label`"))?;
        self.model.workspace_mut(id)?.label = label.to_string();
        self.model.bump();
        self.emit("workspace.renamed", json!({"workspace_id": id.to_string(), "label": label}));
        Ok(json!({"type": "workspace_info", "workspace": self.workspace_info(id)?}))
    }

    fn workspace_close(&mut self, params: &Value) -> ApiResult {
        let id = self.parse_workspace(params, "workspace_id")?;
        if self.model.workspaces.len() == 1 {
            return Err(ApiError::new(ErrorCode::InvalidState, "cannot close the last workspace"));
        }
        let closed = self.model.close_workspace(id)?;
        for c in &closed {
            let _ = self.host.kill(&c.terminal_id);
        }
        self.emit("workspace.closed", json!({"workspace_id": id.to_string()}));
        Ok(json!({"type": "workspace_closed", "workspace_id": id.to_string()}))
    }

    // ---- tab ----

    fn tab_create(&mut self, params: &Value) -> ApiResult {
        let wid = match params.get("workspace_id").and_then(Value::as_str) {
            Some(_) => self.parse_workspace(params, "workspace_id")?,
            None => self.model.focused_workspace,
        };
        let cwd = params.get("cwd").and_then(Value::as_str).map(str::to_string);
        let label = params.get("label").and_then(Value::as_str).map(str::to_string);
        let env = parse_env_map(params)?;
        let focus = params.get("focus").and_then(Value::as_bool).unwrap_or(false);
        let tid = self.model.next_tab_id(wid)?;
        let pid = self.model.next_pane_id(wid)?;
        let spawn_cwd = cwd.clone().unwrap_or_else(|| self.model.workspace(wid).map(|w| w.cwd.clone()).unwrap_or_default());
        let term = self.spawn_for(&spawn_cwd, wid, tid, pid, None, env.clone())?;
        let (tid, pid) = self.model.create_tab(wid, cwd.as_deref(), label, env, || term)?;
        if focus {
            self.model.workspace_mut(wid)?.focused_tab = tid;
        }
        self.emit("tab.created", json!({"tab_id": tid.to_string()}));
        self.emit("pane.created", json!({"pane_id": pid.to_string()}));
        Ok(json!({
            "type": "tab_created",
            "tab": self.tab_info(tid)?,
            "root_pane": self.pane_info(pid)?,
        }))
    }

    fn parse_tab(&self, params: &Value, key: &str) -> Result<TabId, ApiError> {
        let s = params
            .get(key)
            .and_then(Value::as_str)
            .ok_or_else(|| ApiError::invalid_params(format!("missing `{key}`")))?;
        let id = s
            .parse::<TabId>()
            .map_err(|_| ApiError::invalid_params(format!("invalid tab id `{s}`")))?;
        self.model.tab(id)?;
        Ok(id)
    }

    fn tab_list(&self, params: &Value) -> ApiResult {
        let filter = match params.get("workspace_id").and_then(Value::as_str) {
            Some(_) => Some(self.parse_workspace(params, "workspace_id")?),
            None => None,
        };
        let mut out = Vec::new();
        for ws in &self.model.workspaces {
            if filter.map(|f| f == ws.id).unwrap_or(true) {
                for tab in &ws.tabs {
                    out.push(self.tab_info(tab.id)?);
                }
            }
        }
        Ok(json!({"type": "tab_list", "tabs": out}))
    }

    fn tab_get(&self, params: &Value) -> ApiResult {
        let id = self.parse_tab(params, "tab_id")?;
        Ok(json!({"type": "tab_info", "tab": self.tab_info(id)?}))
    }

    fn tab_focus(&mut self, params: &Value) -> ApiResult {
        let id = self.parse_tab(params, "tab_id")?;
        let wid = WorkspaceId(id.workspace);
        self.model.workspace_mut(wid)?.focused_tab = id;
        self.model.focused_workspace = wid;
        self.model.bump();
        self.emit("tab.focused", json!({"tab_id": id.to_string()}));
        Ok(json!({"type": "tab_info", "tab": self.tab_info(id)?}))
    }

    fn tab_rename(&mut self, params: &Value) -> ApiResult {
        let id = self.parse_tab(params, "tab_id")?;
        let label = params
            .get("label")
            .and_then(Value::as_str)
            .ok_or_else(|| ApiError::invalid_params("missing `label`"))?;
        self.model.tab_mut(id)?.label = label.to_string();
        self.model.bump();
        self.emit("tab.renamed", json!({"tab_id": id.to_string(), "label": label}));
        Ok(json!({"type": "tab_info", "tab": self.tab_info(id)?}))
    }

    fn tab_close(&mut self, params: &Value) -> ApiResult {
        let id = self.parse_tab(params, "tab_id")?;
        let ws = self.model.workspace(WorkspaceId(id.workspace))?;
        if self.model.workspaces.len() == 1 && ws.tabs.len() == 1 {
            return Err(ApiError::new(ErrorCode::InvalidState, "cannot close the last tab of the last workspace"));
        }
        let closed = self.model.close_tab(id)?;
        for c in &closed {
            let _ = self.host.kill(&c.terminal_id);
        }
        self.emit("tab.closed", json!({"tab_id": id.to_string()}));
        Ok(json!({"type": "tab_closed", "tab_id": id.to_string()}))
    }

    // ---- pane ----

    fn pane_split(&mut self, params: &Value) -> ApiResult {
        let target = self.resolve_pane(params)?;
        let direction = parse_direction(params, "direction")?;
        let ratio = params.get("ratio").and_then(Value::as_f64).unwrap_or(0.5) as f32;
        if !(0.0..=1.0).contains(&ratio) {
            return Err(ApiError::invalid_params("ratio must be between 0 and 1"));
        }
        let cwd = params.get("cwd").and_then(Value::as_str).map(str::to_string);
        let env = parse_env_map(params)?;
        let focus = params.get("focus").and_then(Value::as_bool).unwrap_or(false);
        let wid = WorkspaceId(target.workspace);
        let tid = self.model.tab_of_pane(target)?;
        let pid = self.model.next_pane_id(wid)?;
        let spawn_cwd = cwd.clone().unwrap_or_else(|| {
            self.model.pane(target).map(|p| p.cwd.clone()).unwrap_or_default()
        });
        let term = self.spawn_for(&spawn_cwd, wid, tid, pid, None, env.clone())?;
        let new_id = self.model.split_pane(target, direction, ratio, cwd.as_deref(), env, || term)?;
        if focus {
            self.model.tab_mut(tid)?.focused_pane = new_id;
        }
        self.emit("pane.created", json!({"pane_id": new_id.to_string()}));
        self.emit("layout.updated", serde_json::to_value(self.layout_snapshot(tid)?).unwrap());
        Ok(json!({"type": "pane_info", "pane": self.pane_info(new_id)?}))
    }

    fn pane_close(&mut self, params: &Value) -> ApiResult {
        let target = self.resolve_pane(params)?;
        if self.model.panes.len() == 1 {
            return Err(ApiError::new(ErrorCode::InvalidState, "cannot close the last pane"));
        }
        let closed = self.model.close_pane(target)?;
        let _ = self.host.kill(&closed.terminal_id);
        self.emit("pane.closed", json!({"pane_id": target.to_string()}));
        if let Some(t) = closed.closed_tab {
            self.emit("tab.closed", json!({"tab_id": t.to_string()}));
        }
        if let Some(w) = closed.closed_workspace {
            self.emit("workspace.closed", json!({"workspace_id": w.to_string()}));
        }
        Ok(json!({"type": "pane_closed", "pane_id": target.to_string()}))
    }

    fn pane_list(&self, params: &Value) -> ApiResult {
        let filter = match params.get("workspace_id").and_then(Value::as_str) {
            Some(_) => Some(self.parse_workspace(params, "workspace_id")?),
            None => None,
        };
        let mut out = Vec::new();
        for ws in &self.model.workspaces {
            if filter.map(|f| f == ws.id).unwrap_or(true) {
                for tab in &ws.tabs {
                    for p in tab.tree.panes() {
                        out.push(self.pane_info(p)?);
                    }
                }
            }
        }
        Ok(json!({"type": "pane_list", "panes": out}))
    }

    fn pane_get(&self, params: &Value) -> ApiResult {
        let target = self.resolve_pane(params)?;
        Ok(json!({"type": "pane_info", "pane": self.pane_info(target)?}))
    }

    fn pane_current(&self, params: &Value) -> ApiResult {
        let target = self.resolve_pane(params)?;
        Ok(json!({"type": "pane_info", "pane": self.pane_info(target)?}))
    }

    fn pane_rename(&mut self, params: &Value) -> ApiResult {
        let target = self.resolve_pane(params)?;
        if params.get("clear").and_then(Value::as_bool).unwrap_or(false) {
            self.model.pane_mut(target)?.label = None;
        } else {
            let label = params
                .get("label")
                .and_then(Value::as_str)
                .ok_or_else(|| ApiError::invalid_params("missing `label` (or pass clear:true)"))?;
            self.model.pane_mut(target)?.label = Some(label.to_string());
        }
        self.model.bump();
        self.emit("pane.updated", json!({"pane_id": target.to_string()}));
        Ok(json!({"type": "pane_info", "pane": self.pane_info(target)?}))
    }

    fn pane_read(&self, params: &Value) -> ApiResult {
        let target = self.resolve_pane(params)?;
        let source = match params.get("source").and_then(Value::as_str).unwrap_or("visible") {
            "visible" => ReadSource::Visible,
            "recent" => ReadSource::Recent,
            "recent-unwrapped" => ReadSource::RecentUnwrapped,
            "detection" => ReadSource::Detection,
            other => return Err(ApiError::invalid_params(format!("invalid source `{other}`"))),
        };
        let format = match params.get("format").and_then(Value::as_str).unwrap_or("text") {
            "text" => ReadFormat::Text,
            "ansi" => ReadFormat::Ansi,
            other => return Err(ApiError::invalid_params(format!("invalid format `{other}`"))),
        };
        let lines = params.get("lines").and_then(Value::as_u64).unwrap_or(0) as usize;
        self.read_terminal(target, source, lines, format)
    }

    fn pane_send_text(&mut self, params: &Value) -> ApiResult {
        let target = self.resolve_pane(params)?;
        let text = params
            .get("text")
            .and_then(Value::as_str)
            .ok_or_else(|| ApiError::invalid_params("missing `text`"))?;
        let term = self.model.pane(target)?.terminal_id.clone();
        self.host
            .write_text(&term, text)
            .map_err(|e| ApiError::new(ErrorCode::Internal, e.to_string()))?;
        Ok(json!({"type": "ok"}))
    }

    fn pane_send_keys(&mut self, params: &Value) -> ApiResult {
        let target = self.resolve_pane(params)?;
        let keys: Vec<String> = params
            .get("keys")
            .and_then(Value::as_array)
            .ok_or_else(|| ApiError::invalid_params("missing `keys` array"))?
            .iter()
            .map(|k| k.as_str().map(str::to_string).ok_or_else(|| ApiError::invalid_params("keys must be strings")))
            .collect::<Result<_, _>>()?;
        if keys.is_empty() {
            return Err(ApiError::invalid_params("keys must not be empty"));
        }
        let term = self.model.pane(target)?.terminal_id.clone();
        self.host.write_keys(&term, &keys).map_err(|e| match e {
            crate::hosttraits::HostError::InvalidKey(k) => {
                ApiError::invalid_params(format!("invalid key `{k}`"))
            }
            other => ApiError::new(ErrorCode::Internal, other.to_string()),
        })?;
        Ok(json!({"type": "ok"}))
    }

    fn pane_run(&mut self, params: &Value) -> ApiResult {
        let target = self.resolve_pane(params)?;
        let command = params
            .get("command")
            .and_then(Value::as_str)
            .ok_or_else(|| ApiError::invalid_params("missing `command`"))?;
        let term = self.model.pane(target)?.terminal_id.clone();
        // Text and Enter as separate writes: TUIs treat a joined write as paste.
        self.host
            .write_text(&term, command)
            .and_then(|_| self.host.write_enter(&term))
            .map_err(|e| ApiError::new(ErrorCode::Internal, e.to_string()))?;
        Ok(json!({"type": "ok"}))
    }

    fn pane_zoom(&mut self, params: &Value) -> ApiResult {
        let target = self.resolve_pane(params)?;
        let tid = self.model.tab_of_pane(target)?;
        let mode = params.get("mode").and_then(Value::as_str).unwrap_or("toggle");
        let (changed, reason, focus_changed) = {
            let tab = self.model.tab_mut(tid)?;
            let single = tab.tree.panes().len() == 1;
            let (changed, reason): (bool, Option<&str>) = match mode {
                "on" => {
                    if single {
                        (false, Some("single_pane"))
                    } else if tab.zoomed == Some(target) {
                        (false, Some("already_zoomed"))
                    } else {
                        tab.zoomed = Some(target);
                        (true, None)
                    }
                }
                "off" => {
                    if tab.zoomed.is_none() {
                        (false, Some("already_unzoomed"))
                    } else {
                        tab.zoomed = None;
                        (true, None)
                    }
                }
                "toggle" => {
                    if single {
                        (false, Some("single_pane"))
                    } else if tab.zoomed == Some(target) {
                        tab.zoomed = None;
                        (true, None)
                    } else {
                        tab.zoomed = Some(target);
                        (true, None)
                    }
                }
                other => return Err(ApiError::invalid_params(format!("invalid zoom mode `{other}`"))),
            };
            let focus_changed = changed && tab.focused_pane != target;
            if changed {
                tab.focused_pane = target;
            }
            (changed, reason, focus_changed)
        };
        if changed {
            self.model.bump();
        }
        let tab = self.model.tab(tid)?;
        let mut out = json!({
            "type": "pane_zoom",
            "changed": changed,
            "zoom_changed": changed,
            "focus_changed": focus_changed,
            "pane_id": target.to_string(),
            "focused_pane_id": tab.focused_pane.to_string(),
            "zoomed": tab.zoomed.map(|p| p.to_string()),
            "layout": self.layout_snapshot(tid)?,
        });
        if let Some(r) = reason {
            out["reason"] = json!(r);
        }
        Ok(out)
    }

    fn pane_swap(&mut self, params: &Value) -> ApiResult {
        let (source, target): (PaneId, Option<PaneId>) =
            if let Some(sp) = self.parse_pane_id(params, "source_pane_id")? {
                (sp, self.parse_pane_id(params, "target_pane_id")?)
            } else {
                let src = self.resolve_pane(params)?;
                (src, None)
            };
        let tid = self.model.tab_of_pane(source)?;
        let resolved_target = match target {
            Some(t) => Some(t),
            None => {
                let dir = parse_nav_direction(params, "direction")?;
                let tab = self.model.tab(tid)?;
                tab.tree.neighbor(source, dir, self.client_area, self.pane_gap)
            }
        };
        let (changed, reason) = match resolved_target {
            None => (false, Some("no_neighbor")),
            Some(t) if t == source => (false, Some("same_pane")),
            Some(t) => match self.model.tab_of_pane(t) {
                Err(_) => (false, Some("not_found")),
                Ok(other_tab) if other_tab != tid => (false, Some("cross_tab")),
                Ok(_) => {
                    let tab = self.model.tab_mut(tid)?;
                    tab.tree.swap(source, t);
                    self.model.bump();
                    (true, None)
                }
            },
        };
        let tab = self.model.tab(tid)?;
        let mut out = json!({
            "type": "pane_swap",
            "changed": changed,
            "source_pane_id": source.to_string(),
            "focused_pane_id": tab.focused_pane.to_string(),
            "layout": self.layout_snapshot(tid)?,
        });
        if let Some(t) = resolved_target {
            out["target_pane_id"] = json!(t.to_string());
        }
        if let Some(r) = reason {
            out["reason"] = json!(r);
        }
        if changed {
            self.emit("layout.updated", serde_json::to_value(self.layout_snapshot(tid)?).unwrap());
        }
        Ok(out)
    }

    fn pane_move(&mut self, params: &Value) -> ApiResult {
        let pane = self
            .parse_pane_id(params, "pane_id")?
            .ok_or_else(|| ApiError::invalid_params("pane.move requires `pane_id`"))?;
        self.model.pane(pane)?;
        let src_tab = self.model.tab_of_pane(pane)?;
        let dest = params
            .get("destination")
            .ok_or_else(|| ApiError::invalid_params("missing `destination`"))?;
        let focus = params.get("focus").and_then(Value::as_bool).unwrap_or(false);
        let dtype = dest.get("type").and_then(Value::as_str).unwrap_or("");
        // Zoom guard: moves involving zoomed source/target return changed:false.
        if self.model.tab(src_tab)?.zoomed.is_some() {
            return Ok(json!({"type":"pane_move","changed":false,"reason":"zoomed_tab"}));
        }
        let result = match dtype {
            "tab" => {
                let dest_tab = {
                    let s = dest
                        .get("tab_id")
                        .and_then(Value::as_str)
                        .ok_or_else(|| ApiError::invalid_params("destination.tab_id required"))?;
                    s.parse::<TabId>()
                        .map_err(|_| ApiError::invalid_params(format!("invalid tab id `{s}`")))?
                };
                self.model.tab(dest_tab)?;
                if dest_tab == src_tab {
                    return Ok(json!({"type":"pane_move","changed":false,"reason":"same_tab"}));
                }
                if self.model.tab(dest_tab)?.zoomed.is_some() {
                    return Ok(json!({"type":"pane_move","changed":false,"reason":"zoomed_tab"}));
                }
                let split = match dest.get("split").and_then(Value::as_str) {
                    Some("right") => SplitDirection::Right,
                    Some("down") => SplitDirection::Down,
                    _ => return Err(ApiError::invalid_params("destination.split must be right|down")),
                };
                let target_pane = match dest.get("target_pane_id").and_then(Value::as_str) {
                    Some(s) => Some(
                        s.parse::<PaneId>()
                            .map_err(|_| ApiError::invalid_params(format!("invalid pane id `{s}`")))?,
                    ),
                    None => None,
                };
                let ratio = dest.get("ratio").and_then(Value::as_f64).unwrap_or(0.5) as f32;
                self.model.move_pane_to_tab(pane, dest_tab, split, target_pane, ratio)?
            }
            "new_tab" => {
                let wid = match dest.get("workspace_id").and_then(Value::as_str) {
                    Some(s) => s
                        .parse::<WorkspaceId>()
                        .map_err(|_| ApiError::invalid_params(format!("invalid workspace id `{s}`")))?,
                    None => WorkspaceId(pane.workspace),
                };
                self.model.workspace(wid)?;
                let label = dest.get("label").and_then(Value::as_str).map(str::to_string);
                // Create the tab around the moved pane: create with a fresh
                // shell would spawn a terminal we don't need, so build the tab
                // directly by moving into a synthetic split-less tab.
                let (tid, placeholder) = {
                    let cwd = self.model.pane(pane)?.cwd.clone();
                    let tid = self.model.next_tab_id(wid)?;
                    let pid_placeholder = self.model.next_pane_id(wid)?;
                    let term = self.spawn_for(&cwd, wid, tid, pid_placeholder, None, BTreeMap::new())?;
                    let (tid, pid) = self.model.create_tab(wid, Some(&cwd), label, BTreeMap::new(), || term)?;
                    (tid, pid)
                };
                let moved = self.model.move_pane_to_tab(pane, tid, SplitDirection::Right, Some(placeholder), 0.5)?;
                // Drop the placeholder shell so the tab holds only the moved pane.
                let closed = self.model.close_pane(placeholder)?;
                let _ = self.host.kill(&closed.terminal_id);
                moved
            }
            "new_workspace" => {
                let label = dest.get("label").and_then(Value::as_str).map(str::to_string);
                let tab_label = dest.get("tab_label").and_then(Value::as_str).map(str::to_string);
                let cwd = self.model.pane(pane)?.cwd.clone();
                let (wid, tid, pid) = self.model.next_workspace_initial_ids();
                let term = self.spawn_for(&cwd, wid, tid, pid, None, BTreeMap::new())?;
                let (wid, tid, placeholder) = self.model.create_workspace(&cwd, label, BTreeMap::new(), || term);
                if let Some(tl) = tab_label {
                    self.model.tab_mut(tid)?.label = tl;
                }
                let moved = self.model.move_pane_to_tab(pane, tid, SplitDirection::Right, Some(placeholder), 0.5)?;
                let closed = self.model.close_pane(placeholder)?;
                let _ = self.host.kill(&closed.terminal_id);
                let _ = wid;
                moved
            }
            other => return Err(ApiError::invalid_params(format!("invalid destination type `{other}`"))),
        };
        let (new_id, previous_id) = result;
        let dest_tab = self.model.tab_of_pane(new_id)?;
        if focus {
            self.model.workspace_mut(WorkspaceId(dest_tab.workspace))?.focused_tab = dest_tab;
            self.model.tab_mut(dest_tab)?.focused_pane = new_id;
            self.model.focused_workspace = WorkspaceId(dest_tab.workspace);
        }
        self.emit("pane.moved", json!({
            "pane_id": new_id.to_string(),
            "previous_pane_id": previous_id.to_string(),
        }));
        Ok(json!({
            "type": "pane_move",
            "changed": true,
            "previous_pane_id": previous_id.to_string(),
            "previous_workspace_id": WorkspaceId(previous_id.workspace).to_string(),
            "previous_tab_id": src_tab.to_string(),
            "pane": self.pane_info(new_id)?,
            "target_layout": self.layout_snapshot(dest_tab)?,
            "focused_pane_id": self.focused_pane().to_string(),
        }))
    }

    fn pane_layout(&self, params: &Value) -> ApiResult {
        let target = self.resolve_pane(params)?;
        let tid = self.model.tab_of_pane(target)?;
        Ok(json!({"type": "pane_layout", "layout": self.layout_snapshot(tid)?}))
    }

    fn pane_neighbor(&self, params: &Value) -> ApiResult {
        let target = self.resolve_pane(params)?;
        let dir = parse_nav_direction(params, "direction")?;
        let tid = self.model.tab_of_pane(target)?;
        let tab = self.model.tab(tid)?;
        let neighbor = tab.tree.neighbor(target, dir, self.client_area, self.pane_gap);
        Ok(json!({
            "type": "pane_neighbor",
            "pane_id": target.to_string(),
            "neighbor": neighbor.map(|p| p.to_string()),
            "layout": self.layout_snapshot(tid)?,
        }))
    }

    fn pane_edges(&self, params: &Value) -> ApiResult {
        let target = self.resolve_pane(params)?;
        let tid = self.model.tab_of_pane(target)?;
        let tab = self.model.tab(tid)?;
        let edges = tab
            .tree
            .edges(target, self.client_area, self.pane_gap)
            .ok_or_else(|| ApiError::not_found(format!("pane {target}")))?;
        Ok(json!({
            "type": "pane_edges",
            "pane_id": target.to_string(),
            "edges": edges,
            "layout": self.layout_snapshot(tid)?,
        }))
    }

    fn pane_focus_direction(&mut self, params: &Value) -> ApiResult {
        let from = self.resolve_pane(params)?;
        let dir = parse_nav_direction(params, "direction")?;
        let tid = self.model.tab_of_pane(from)?;
        let tab = self.model.tab(tid)?;
        let next = tab.tree.neighbor(from, dir, self.client_area, self.pane_gap);
        if let Some(next) = next {
            self.model.tab_mut(tid)?.focused_pane = next;
            self.model.workspace_mut(WorkspaceId(tid.workspace))?.focused_tab = tid;
            self.model.focused_workspace = WorkspaceId(tid.workspace);
            self.model.bump();
            self.emit("pane.focused", json!({"pane_id": next.to_string()}));
            Ok(json!({"type": "pane_info", "pane": self.pane_info(next)?}))
        } else {
            Ok(json!({"type": "pane_info", "pane": self.pane_info(from)?}))
        }
    }

    fn pane_resize(&mut self, params: &Value) -> ApiResult {
        let target = self.resolve_pane(params)?;
        let dir = parse_nav_direction(params, "direction")?;
        let amount = params.get("amount").and_then(Value::as_f64).unwrap_or(0.05) as f32;
        if !(0.0..=1.0).contains(&amount) {
            return Err(ApiError::invalid_params("amount must be between 0 and 1"));
        }
        let tid = self.model.tab_of_pane(target)?;
        let tab = self.model.tab_mut(tid)?;
        let changed = tab.tree.resize(target, dir, amount);
        if changed {
            self.model.bump();
            self.emit("layout.updated", serde_json::to_value(self.layout_snapshot(tid)?).unwrap());
        }
        Ok(json!({
            "type": "pane_resize",
            "changed": changed,
            "pane_id": target.to_string(),
            "layout": self.layout_snapshot(tid)?,
        }))
    }

    fn pane_process_info(&self, params: &Value) -> ApiResult {
        let target = self.resolve_pane(params)?;
        let term = self.model.pane(target)?.terminal_id.clone();
        let info = self
            .host
            .process_info(&term)
            .map_err(|e| ApiError::new(ErrorCode::Internal, e.to_string()))?;
        Ok(json!({"type": "pane_process_info", "pane_id": target.to_string(), "process": info}))
    }

    // ---- layout ----

    fn layout_export(&self, params: &Value) -> ApiResult {
        let tid = if let Some(s) = params.get("tab_id").and_then(Value::as_str) {
            s.parse::<TabId>()
                .map_err(|_| ApiError::invalid_params(format!("invalid tab id `{s}`")))?
        } else if let Some(p) = self.parse_pane_id(params, "pane_id")? {
            self.model.tab_of_pane(p)?
        } else {
            self.focused_tab()
        };
        let tab = self.model.tab(tid)?;
        let root = export_node(&tab.tree, &self.model);
        Ok(json!({
            "type": "layout_export",
            "workspace_id": WorkspaceId(tid.workspace).to_string(),
            "tab_id": tid.to_string(),
            "zoomed": tab.zoomed.map(|p| p.to_string()),
            "focused_pane_id": tab.focused_pane.to_string(),
            "root": root,
        }))
    }

    fn layout_set_split_ratio(&mut self, params: &Value) -> ApiResult {
        let tid = if let Some(s) = params.get("tab_id").and_then(Value::as_str) {
            s.parse::<TabId>()
                .map_err(|_| ApiError::invalid_params(format!("invalid tab id `{s}`")))?
        } else {
            self.focused_tab()
        };
        let path: Vec<usize> = params
            .get("path")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Value::as_u64).map(|n| n as usize).collect())
            .unwrap_or_default();
        let ratio = params
            .get("ratio")
            .and_then(Value::as_f64)
            .ok_or_else(|| ApiError::invalid_params("missing `ratio`"))? as f32;
        let tab = self.model.tab_mut(tid)?;
        if !set_ratio_at(&mut tab.tree, &path, ratio) {
            return Err(ApiError::invalid_params("path does not address a split node"));
        }
        self.model.bump();
        let export = self.layout_export(&json!({"tab_id": tid.to_string()}))?;
        self.emit("layout.updated", serde_json::to_value(self.layout_snapshot(tid)?).unwrap());
        Ok(json!({
            "type": "layout_split_ratio_set",
            "layout": export,
        }))
    }

    // ---- metadata + notification ----

    fn build_metadata_report<'a>(params: &'a Value) -> Result<crate::metadata::MetadataReport<'a>, ApiError> {
        let source = params
            .get("source")
            .and_then(Value::as_str)
            .ok_or_else(|| ApiError::invalid_params("missing `source`"))?;
        // Presentation fields: absent = unchanged, null/clear_* = clear, string = set.
        let title = if params.get("clear_title").and_then(Value::as_bool).unwrap_or(false) {
            Some(None)
        } else {
            params.get("title").map(|v| v.as_str())
        };
        let display_agent = if params
            .get("clear_display_agent")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            Some(None)
        } else {
            params.get("display_agent").map(|v| v.as_str())
        };
        Ok(crate::metadata::MetadataReport {
            source,
            seq: params.get("seq").and_then(Value::as_u64),
            ttl_ms: params.get("ttl_ms").and_then(Value::as_u64),
            title,
            display_agent,
            state_labels: params.get("state_labels").and_then(Value::as_object),
            clear_state_labels: params
                .get("clear_state_labels")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            tokens: params.get("tokens").and_then(Value::as_object),
        })
    }

    fn pane_report_metadata(&mut self, params: &Value) -> ApiResult {
        let target = self
            .parse_pane_id(params, "pane_id")?
            .ok_or_else(|| ApiError::invalid_params("missing `pane_id`"))?;
        self.model.pane(target)?;
        let report = Self::build_metadata_report(params)?;
        let applied = self
            .pane_metadata
            .entry(target)
            .or_default()
            .apply(report, std::time::Instant::now())?;
        if applied {
            self.emit("pane.updated", json!({"pane_id": target.to_string()}));
        }
        Ok(json!({"type": "metadata_reported", "applied": applied}))
    }

    fn workspace_report_metadata(&mut self, params: &Value) -> ApiResult {
        let id = self.parse_workspace(params, "workspace_id")?;
        let report = Self::build_metadata_report(params)?;
        let applied = self
            .workspace_metadata
            .entry(id)
            .or_default()
            .apply(report, std::time::Instant::now())?;
        if applied {
            let ws = self.workspace_info(id)?;
            self.emit("workspace.metadata_updated", serde_json::to_value(ws).unwrap());
        }
        Ok(json!({"type": "metadata_reported", "applied": applied}))
    }

    fn notification_show(&mut self, params: &Value) -> ApiResult {
        let raw_title = params
            .get("title")
            .and_then(Value::as_str)
            .ok_or_else(|| ApiError::invalid_params("missing `title`"))?;
        let title = crate::metadata::sanitize_notification(raw_title, 80);
        if title.is_empty() {
            return Err(ApiError::invalid_params("title must contain visible text"));
        }
        let body = params
            .get("body")
            .and_then(Value::as_str)
            .map(|b| crate::metadata::sanitize_notification(b, 240));
        if let Some(pos) = params.get("position").and_then(Value::as_str) {
            if !matches!(pos, "top-left" | "top-right" | "bottom-left" | "bottom-right") {
                return Err(ApiError::invalid_params(format!("invalid position `{pos}`")));
            }
        }
        let sound = params.get("sound").and_then(Value::as_str).unwrap_or("none");
        if !matches!(sound, "none" | "done" | "request") {
            return Err(ApiError::invalid_params(format!("invalid sound `{sound}`")));
        }
        let (shown, reason) = if self.toast_delivery == "off" {
            (false, "disabled")
        } else if !self.has_foreground_client {
            (false, "no_foreground_client")
        } else {
            self.emit(
                "notification.requested",
                json!({"title": title, "body": body, "position": params.get("position"), "sound": sound}),
            );
            (true, "shown")
        };
        Ok(json!({"type": "notification_show", "shown": shown, "reason": reason}))
    }

    fn default_cwd(&self) -> String {
        self.model
            .workspace(self.model.focused_workspace)
            .map(|w| w.cwd.clone())
            .unwrap_or_else(|_| ".".into())
    }
}

fn export_node(node: &starcil_domain::Node, model: &starcil_domain::SessionModel) -> Value {
    match node {
        starcil_domain::Node::Leaf(p) => {
            let meta = model.pane(*p).ok();
            let mut v = json!({"type": "pane", "pane_id": p.to_string()});
            if let Some(m) = meta {
                if let Some(l) = &m.label {
                    v["label"] = json!(l);
                }
                v["cwd"] = json!(m.cwd);
            }
            v
        }
        starcil_domain::Node::Split { axis, ratio, first, second } => json!({
            "type": "split",
            "direction": match axis {
                starcil_domain::Axis::Horizontal => "right",
                starcil_domain::Axis::Vertical => "down",
            },
            "ratio": ratio,
            "first": export_node(first, model),
            "second": export_node(second, model),
        }),
    }
}

/// Address a split node by child-index path (0=first, 1=second) and set its ratio.
fn set_ratio_at(node: &mut starcil_domain::Node, path: &[usize], ratio: f32) -> bool {
    match node {
        starcil_domain::Node::Split { ratio: r, first, second, .. } => {
            if path.is_empty() {
                *r = starcil_domain::tree::clamp_ratio(ratio);
                return true;
            }
            let next = if path[0] == 0 { first } else { second };
            set_ratio_at(next, &path[1..], ratio)
        }
        starcil_domain::Node::Leaf(_) => false,
    }
}

// Silence unused import warning until layout.apply lands.
#[allow(unused)]
fn _use(_: PortableLayoutNode) {}
