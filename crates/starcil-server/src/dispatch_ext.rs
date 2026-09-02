//! Second dispatcher wave: reorders, portable layouts, worktrees, window
//! title, config/manifest reloads.

use crate::core::{parse_env_map, ApiResult, ServerCore};
use crate::hosttraits::TerminalHost;
use serde_json::{json, Value};
use starcil_domain::{Node, PaneMeta, SplitDirection, TabId, WorkspaceId};
use starcil_protocol::error::{ApiError, ErrorCode};
use starcil_protocol::types::WorktreeProvenance;
use starcil_worktree::{CreateOptions, SystemCommandRunner, WorktreeManager, WorktreeSelector};
use std::collections::BTreeMap;
use std::path::PathBuf;

impl<H: TerminalHost> ServerCore<H> {
    // ---- reorder ----

    pub(crate) fn workspace_move(&mut self, params: &Value) -> ApiResult {
        let id = self.parse_workspace_pub(params, "workspace_id")?;
        let insert_index = params
            .get("insert_index")
            .and_then(Value::as_u64)
            .ok_or_else(|| ApiError::invalid_params("missing `insert_index`"))? as usize;
        if !self.model.move_workspace(id, insert_index) {
            return Err(ApiError::not_found(format!("workspace {id}")));
        }
        let ordered: Vec<String> = self.model.workspaces.iter().map(|w| w.id.to_string()).collect();
        self.emit(
            "workspace.moved",
            json!({"workspace_id": id.to_string(), "insert_index": insert_index, "workspaces": ordered}),
        );
        let ordered: Vec<String> = self.model.workspaces.iter().map(|w| w.id.to_string()).collect();
        Ok(json!({"type": "workspace_moved", "workspace_id": id.to_string(), "workspaces": ordered}))
    }

    pub(crate) fn workspace_move_block(&mut self, params: &Value) -> ApiResult {
        let ids: Vec<WorkspaceId> = params
            .get("workspace_ids")
            .and_then(Value::as_array)
            .ok_or_else(|| ApiError::invalid_params("missing `workspace_ids`"))?
            .iter()
            .map(|v| {
                v.as_str()
                    .and_then(|s| s.parse::<WorkspaceId>().ok())
                    .ok_or_else(|| ApiError::invalid_params("workspace_ids must be workspace id strings"))
            })
            .collect::<Result<_, _>>()?;
        let before = match params.get("before_workspace_id").and_then(Value::as_str) {
            Some(s) => Some(
                s.parse::<WorkspaceId>()
                    .map_err(|_| ApiError::invalid_params(format!("invalid workspace id `{s}`")))?,
            ),
            None => None,
        };
        self.model.move_workspace_block(&ids, before)?;
        let ordered: Vec<String> = self.model.workspaces.iter().map(|w| w.id.to_string()).collect();
        self.emit(
            "workspace.reordered",
            json!({
                "workspace_ids": ids.iter().map(|i| i.to_string()).collect::<Vec<_>>(),
                "before_workspace_id": before.map(|b| b.to_string()),
                "workspaces": ordered,
            }),
        );
        let ordered: Vec<String> = self.model.workspaces.iter().map(|w| w.id.to_string()).collect();
        Ok(json!({"type": "workspace_reordered", "workspaces": ordered}))
    }

    pub(crate) fn tab_move(&mut self, params: &Value) -> ApiResult {
        let id = {
            let s = params
                .get("tab_id")
                .and_then(Value::as_str)
                .ok_or_else(|| ApiError::invalid_params("missing `tab_id`"))?;
            s.parse::<TabId>()
                .map_err(|_| ApiError::invalid_params(format!("invalid tab id `{s}`")))?
        };
        let insert_index = params
            .get("insert_index")
            .and_then(Value::as_u64)
            .ok_or_else(|| ApiError::invalid_params("missing `insert_index`"))? as usize;
        self.model.move_tab(id, insert_index)?;
        let ws = self.model.workspace(WorkspaceId(id.workspace))?;
        let ordered: Vec<String> = ws.tabs.iter().map(|t| t.id.to_string()).collect();
        self.emit(
            "tab.moved",
            json!({
                "tab_id": id.to_string(),
                "workspace_id": WorkspaceId(id.workspace).to_string(),
                "insert_index": insert_index,
                "tabs": ordered.clone(),
            }),
        );
        Ok(json!({"type": "tab_moved", "tab_id": id.to_string(), "tabs": ordered}))
    }

    // ---- layout.apply ----

    pub(crate) fn layout_apply(&mut self, params: &Value) -> ApiResult {
        let wid = match params.get("workspace_id").and_then(Value::as_str) {
            Some(s) => {
                let id = s
                    .parse::<WorkspaceId>()
                    .map_err(|_| ApiError::invalid_params(format!("invalid workspace id `{s}`")))?;
                self.model.workspace(id)?;
                id
            }
            None => self.model.focused_workspace,
        };
        let root = params
            .get("root")
            .ok_or_else(|| ApiError::invalid_params("missing `root`"))?
            .clone();
        let replace_tab = match params.get("tab_id").and_then(Value::as_str) {
            Some(s) => {
                let id = s
                    .parse::<TabId>()
                    .map_err(|_| ApiError::invalid_params(format!("invalid tab id `{s}`")))?;
                self.model.tab(id)?;
                Some(id)
            }
            None => None,
        };
        let default_cwd = self.model.workspace(wid)?.cwd.clone();
        let mut metas: Vec<PaneMeta> = Vec::new();
        let tab_peek = self.model.next_tab_id(wid)?;
        let tree = self.build_tree(&root, wid, tab_peek, &default_cwd, &mut metas)?;
        let label = params.get("tab_label").and_then(Value::as_str).map(str::to_string);
        let tid = self.model.insert_tab_prebuilt(wid, label, tree, metas)?;
        if params.get("focus").and_then(Value::as_bool).unwrap_or(false) {
            self.model.workspace_mut(wid)?.focused_tab = tid;
            self.model.focused_workspace = wid;
        }
        if let Some(old) = replace_tab {
            let closed = self.model.close_tab(old)?;
            for c in &closed {
                let _ = self.host.kill(&c.terminal_id);
            }
            self.emit("tab.closed", json!({"tab_id": old.to_string()}));
        }
        self.emit("tab.created", json!({"tab_id": tid.to_string()}));
        Ok(json!({
            "type": "layout_applied",
            "tab": self.tab_info(tid)?,
            "layout": self.layout_snapshot(tid)?,
        }))
    }

    fn build_tree(
        &mut self,
        node: &Value,
        wid: WorkspaceId,
        tid: TabId,
        default_cwd: &str,
        metas: &mut Vec<PaneMeta>,
    ) -> Result<Node, ApiError> {
        match node.get("type").and_then(Value::as_str) {
            Some("pane") => {
                let cwd = node
                    .get("cwd")
                    .and_then(Value::as_str)
                    .unwrap_or(default_cwd)
                    .to_string();
                let command: Option<Vec<String>> = node.get("command").and_then(Value::as_array).map(|a| {
                    a.iter()
                        .filter_map(Value::as_str)
                        .map(str::to_string)
                        .collect()
                });
                let env = parse_env_map(node)?;
                let pid = self.model.allocate_pane_id(wid)?;
                let term = self.spawn_for(&cwd, wid, tid, pid, command, env.clone())?;
                metas.push(PaneMeta {
                    id: pid,
                    terminal_id: term,
                    cwd,
                    label: node.get("label").and_then(Value::as_str).map(str::to_string),
                    agent_name: None,
                    env: env.into_iter().collect::<BTreeMap<_, _>>(),
                });
                Ok(Node::Leaf(pid))
            }
            Some("split") => {
                let direction = match node.get("direction").and_then(Value::as_str) {
                    Some("right") => SplitDirection::Right,
                    Some("down") => SplitDirection::Down,
                    other => {
                        return Err(ApiError::invalid_params(format!(
                            "split direction must be right|down, got {other:?}"
                        )))
                    }
                };
                let ratio = node.get("ratio").and_then(Value::as_f64).unwrap_or(0.5) as f32;
                let first = node
                    .get("first")
                    .ok_or_else(|| ApiError::invalid_params("split missing `first`"))?;
                let second = node
                    .get("second")
                    .ok_or_else(|| ApiError::invalid_params("split missing `second`"))?;
                let first = self.build_tree(first, wid, tid, default_cwd, metas)?;
                let second = self.build_tree(second, wid, tid, default_cwd, metas)?;
                Ok(Node::Split {
                    axis: match direction {
                        SplitDirection::Right => starcil_domain::Axis::Horizontal,
                        SplitDirection::Down => starcil_domain::Axis::Vertical,
                    },
                    ratio: starcil_domain::tree::clamp_ratio(ratio),
                    first: Box::new(first),
                    second: Box::new(second),
                })
            }
            other => Err(ApiError::invalid_params(format!("invalid layout node type {other:?}"))),
        }
    }

    // ---- worktrees ----

    fn worktree_manager(&self, params: &Value) -> Result<(WorktreeManager<SystemCommandRunner>, WorkspaceId), ApiError> {
        let (repo, anchor_ws) = if let Some(s) = params.get("workspace_id").and_then(Value::as_str) {
            let id = s
                .parse::<WorkspaceId>()
                .map_err(|_| ApiError::invalid_params(format!("invalid workspace id `{s}`")))?;
            (PathBuf::from(self.model.workspace(id)?.cwd.clone()), id)
        } else if let Some(cwd) = params.get("cwd").and_then(Value::as_str) {
            (PathBuf::from(cwd), self.model.focused_workspace)
        } else {
            let id = self.model.focused_workspace;
            (PathBuf::from(self.model.workspace(id)?.cwd.clone()), id)
        };
        if params.get("workspace_id").is_some() && params.get("cwd").is_some() {
            return Err(ApiError::invalid_params("use at most one of workspace_id or cwd"));
        }
        Ok((
            WorktreeManager::new(SystemCommandRunner::default(), repo, self.worktrees_dir.clone()),
            anchor_ws,
        ))
    }

    fn wt_info_json(&self, info: &starcil_worktree::WorktreeInfo, is_primary: bool) -> Value {
        let path = info.path.display().to_string();
        let workspace_id = self
            .model
            .workspaces
            .iter()
            .find(|w| PathBuf::from(&w.cwd) == info.path)
            .map(|w| w.id.to_string());
        json!({
            "path": path,
            "branch": info.branch,
            "head": info.head,
            "detached": info.detached,
            "is_primary": is_primary,
            "workspace_id": workspace_id,
        })
    }

    pub(crate) fn worktree_list(&mut self, params: &Value) -> ApiResult {
        let (mgr, _) = self.worktree_manager(params)?;
        let list = mgr
            .list()
            .map_err(|e| ApiError::new(ErrorCode::Internal, e.to_string()))?;
        let out: Vec<Value> = list
            .iter()
            .enumerate()
            .map(|(i, w)| self.wt_info_json(w, i == 0))
            .collect();
        Ok(json!({"type": "worktree_list", "worktrees": out}))
    }

    /// Open a workspace rooted at a worktree checkout.
    fn open_worktree_workspace(
        &mut self,
        parent: WorkspaceId,
        info: &starcil_worktree::WorktreeInfo,
        label: Option<String>,
        focus: bool,
    ) -> Result<Value, ApiError> {
        let path = info.path.display().to_string();
        // Already open?
        if let Some(existing) = self.model.workspaces.iter().find(|w| w.cwd == path).map(|w| w.id) {
            return Ok(json!({
                "type": "worktree_opened",
                "already_open": true,
                "workspace": self.workspace_info(existing)?,
            }));
        }
        let (wid, tid, pid) = self.model.next_workspace_initial_ids();
        let term = self.spawn_for(&path, wid, tid, pid, None, BTreeMap::new())?;
        let label = label.or_else(|| info.branch.clone()).unwrap_or_else(|| path.clone());
        let (wid, tid, pid) = self.model.create_workspace(&path, Some(label), BTreeMap::new(), || term);
        if focus {
            self.model.focused_workspace = wid;
        }
        self.worktree_provenance.insert(
            wid,
            WorktreeProvenance {
                parent_workspace_id: parent.to_string(),
                branch: info.branch.clone().unwrap_or_default(),
                path: path.clone(),
            },
        );
        self.emit("workspace.created", json!({"workspace_id": wid.to_string()}));
        self.emit("tab.created", json!({"tab_id": tid.to_string()}));
        self.emit("pane.created", json!({"pane_id": pid.to_string()}));
        Ok(json!({
            "type": "worktree_workspace",
            "workspace": self.workspace_info(wid)?,
            "tab": self.tab_info(tid)?,
            "root_pane": self.pane_info(pid)?,
        }))
    }

    pub(crate) fn worktree_create(&mut self, params: &Value) -> ApiResult {
        let (mgr, parent) = self.worktree_manager(params)?;
        let options = CreateOptions {
            branch: params.get("branch").and_then(Value::as_str).map(str::to_string),
            base: params.get("base").and_then(Value::as_str).map(str::to_string),
            path: params.get("path").and_then(Value::as_str).map(PathBuf::from),
            label: params.get("label").and_then(Value::as_str).map(str::to_string),
        };
        let info = mgr
            .create(options)
            .map_err(|e| ApiError::new(ErrorCode::Internal, e.to_string()))?;
        let focus = params.get("focus").and_then(Value::as_bool).unwrap_or(false);
        let label = params.get("label").and_then(Value::as_str).map(str::to_string);
        let mut out = self.open_worktree_workspace(parent, &info, label, focus)?;
        out["type"] = json!("worktree_created");
        out["worktree"] = self.wt_info_json(&info, false);
        self.emit("worktree.created", out["worktree"].clone());
        Ok(out)
    }

    pub(crate) fn worktree_open(&mut self, params: &Value) -> ApiResult {
        let (mgr, parent) = self.worktree_manager(params)?;
        let selector = match (
            params.get("path").and_then(Value::as_str),
            params.get("branch").and_then(Value::as_str),
        ) {
            (Some(p), None) => WorktreeSelector::Path(PathBuf::from(p)),
            (None, Some(b)) => WorktreeSelector::Branch(b.to_string()),
            _ => return Err(ApiError::invalid_params("use exactly one of `path` or `branch`")),
        };
        let info = mgr
            .open(selector)
            .map_err(|e| ApiError::new(ErrorCode::Internal, e.to_string()))?;
        let focus = params.get("focus").and_then(Value::as_bool).unwrap_or(false);
        let label = params.get("label").and_then(Value::as_str).map(str::to_string);
        let mut out = self.open_worktree_workspace(parent, &info, label, focus)?;
        let already = out["already_open"].as_bool().unwrap_or(false);
        out["type"] = json!("worktree_opened");
        out["already_open"] = json!(already);
        out["worktree"] = self.wt_info_json(&info, false);
        self.emit("worktree.opened", out["worktree"].clone());
        Ok(out)
    }

    pub(crate) fn worktree_remove(&mut self, params: &Value) -> ApiResult {
        let id = self.parse_workspace_pub(params, "workspace_id")?;
        let force = params.get("force").and_then(Value::as_bool).unwrap_or(false);
        let prov = self
            .worktree_provenance
            .get(&id)
            .cloned()
            .ok_or_else(|| {
                ApiError::new(
                    ErrorCode::InvalidState,
                    format!("workspace {id} is not a linked worktree workspace"),
                )
            })?;
        let parent_cwd = {
            let parent: WorkspaceId = prov
                .parent_workspace_id
                .parse()
                .map_err(|_| ApiError::new(ErrorCode::Internal, "corrupt provenance"))?;
            self.model
                .workspace(parent)
                .map(|w| w.cwd.clone())
                .unwrap_or_else(|_| prov.path.clone())
        };
        let mgr = WorktreeManager::new(
            SystemCommandRunner::default(),
            PathBuf::from(parent_cwd),
            self.worktrees_dir.clone(),
        );
        // Close the linked workspace FIRST: on Windows a live shell rooted in
        // the checkout makes `git worktree remove` fail with permission denied.
        if self.model.workspace(id).is_ok() && self.model.workspaces.len() > 1 {
            let closed = self.model.close_workspace(id)?;
            for c in &closed {
                let _ = self.host.kill(&c.terminal_id);
            }
            self.emit("workspace.closed", json!({"workspace_id": id.to_string()}));
        }
        // Give ConPTY a moment to release the working directory handles.
        let mut last_err = None;
        for attempt in 0..10 {
            match mgr.remove(WorktreeSelector::Path(PathBuf::from(&prov.path)), force) {
                Ok(_) => {
                    last_err = None;
                    break;
                }
                Err(e) => {
                    last_err = Some(e);
                    if attempt < 9 {
                        std::thread::sleep(std::time::Duration::from_millis(200));
                    }
                }
            }
        }
        if let Some(e) = last_err {
            return Err(ApiError::new(ErrorCode::Internal, e.to_string()));
        }
        self.worktree_provenance.remove(&id);
        self.emit(
            "worktree.removed",
            json!({"workspace_id": id.to_string(), "worktree": {"path": prov.path, "branch": prov.branch}, "forced": force}),
        );
        Ok(json!({"type": "worktree_removed", "workspace_id": id.to_string(), "forced": force}))
    }

    // ---- window title + server admin ----

    pub(crate) fn window_title_set(&mut self, params: &Value) -> ApiResult {
        let title = params
            .get("title")
            .and_then(Value::as_str)
            .ok_or_else(|| ApiError::invalid_params("missing `title`"))?;
        let (changed, reason) = if self.has_foreground_client {
            self.emit("client.window_title", json!({"title": title}));
            (true, "set")
        } else {
            (false, "no_foreground_client")
        };
        Ok(json!({"type": "client_window_title", "changed": changed, "reason": reason}))
    }

    pub(crate) fn window_title_clear(&mut self) -> ApiResult {
        let (changed, reason) = if self.has_foreground_client {
            self.emit("client.window_title", json!({"title": Value::Null}));
            (true, "cleared")
        } else {
            (false, "no_foreground_client")
        };
        Ok(json!({"type": "client_window_title", "changed": changed, "reason": reason}))
    }

    pub(crate) fn agent_manifests_status(&self, reloaded: bool) -> ApiResult {
        let doc = self.agents.detector.document();
        let manifests: Vec<Value> = doc
            .agents
            .iter()
            .map(|a| {
                let meta = self.agents.detector.manifest_metadata(&a.kind);
                json!({
                    "agent": a.kind,
                    "source": "bundled",
                    "source_kind": "bundled",
                    "active_version": meta.map(|m| m.version),
                })
            })
            .collect();
        Ok(json!({
            "type": if reloaded { "agent_manifest_reload" } else { "agent_manifest_status" },
            "last_result": "bundled",
            "manifests": manifests,
        }))
    }

    // ---- config ----

    /// Apply the loaded config to live server settings.
    pub fn apply_config(&mut self, cfg: &starcil_config::Config) {
        self.toast_delivery = serde_json::to_value(&cfg.ui.toast.delivery)
            .ok()
            .and_then(|v| v.as_str().map(str::to_string))
            .unwrap_or_else(|| "off".to_string());
        self.pane_gap = if cfg.ui.pane_gaps { 1 } else { 0 };
        self.pane_borders = cfg.ui.pane_borders;
        self.kitty_graphics = cfg.experimental.kitty_graphics;
        let dir = cfg.worktrees.directory.trim();
        if !dir.is_empty() {
            let expanded = if let Some(rest) = dir.strip_prefix("~/") {
                self.home_dir.join(rest)
            } else if dir == "~" {
                self.home_dir.clone()
            } else {
                PathBuf::from(dir)
            };
            self.worktrees_dir = expanded;
        }
    }

    pub(crate) fn reload_config_handler(&mut self) -> ApiResult {
        let path = starcil_config::config_path()
            .or_else(starcil_config::default_config_path)
            .unwrap_or_else(|| self.home_dir.join(".config").join("starcil").join("config.toml"));
        let report = starcil_config::load(&path);
        let warnings = report.diagnostics.len();
        self.apply_config(&report.config);
        Ok(json!({
            "type": "config_reloaded",
            "path": path.display().to_string(),
            "diagnostics": warnings,
        }))
    }

    // ---- integrations ----

    pub(crate) fn integration_install(&mut self, params: &Value, uninstall: bool) -> ApiResult {
        use starcil_integrations::Integration as _;
        let id = params
            .get("integration")
            .and_then(Value::as_str)
            .or_else(|| params.get("id").and_then(Value::as_str))
            .ok_or_else(|| ApiError::invalid_params("missing `integration`"))?;
        let integ = starcil_integrations::integration(id)
            .ok_or_else(|| ApiError::not_found(format!("integration `{id}`")))?;
        let report = if uninstall {
            integ.uninstall(&self.home_dir)
        } else {
            integ.install(&self.home_dir)
        }
        .map_err(|e| ApiError::new(ErrorCode::InvalidState, e.to_string()))?;
        Ok(json!({
            "type": if uninstall { "integration_uninstalled" } else { "integration_installed" },
            "report": report,
        }))
    }

    pub(crate) fn integration_status(&mut self, params: &Value) -> ApiResult {
        use starcil_integrations::Integration as _;
        let outdated_only = params.get("outdated_only").and_then(Value::as_bool).unwrap_or(false);
        let mut out = Vec::new();
        for integ in starcil_integrations::registry() {
            let st = integ
                .status(&self.home_dir)
                .map_err(|e| ApiError::new(ErrorCode::Internal, e.to_string()))?;
            if !outdated_only || st.outdated {
                out.push(serde_json::to_value(st).unwrap());
            }
        }
        Ok(json!({"type": "integration_status", "integrations": out}))
    }

    // ---- small contract endpoints ----

    /// Pane graphics are gated behind [experimental] kitty_graphics; while the
    /// flag is off every graphics method returns feature_disabled (that IS the
    /// documented behavior for the default config).
    pub(crate) fn graphics_gate(&self) -> ApiResult {
        if !self.kitty_graphics {
            return Err(ApiError::new(
                ErrorCode::FeatureDisabled,
                "pane graphics require [experimental] kitty_graphics = true",
            ));
        }
        Err(ApiError::new(
            ErrorCode::Internal,
            "kitty graphics rendering is not implemented yet",
        ))
    }

    /// pane.send_input: combined text/keys/bytes in one request.
    pub(crate) fn pane_send_input(&mut self, params: &Value) -> ApiResult {
        let target = self.resolve_pane(params)?;
        let term = self.model.pane(target)?.terminal_id.clone();
        let mut did = false;
        if let Some(text) = params.get("text").and_then(Value::as_str) {
            self.host
                .write_text(&term, text)
                .map_err(|e| ApiError::new(ErrorCode::Internal, e.to_string()))?;
            did = true;
        }
        if let Some(keys) = params.get("keys").and_then(Value::as_array) {
            let keys: Vec<String> = keys
                .iter()
                .map(|k| {
                    k.as_str()
                        .map(str::to_string)
                        .ok_or_else(|| ApiError::invalid_params("keys must be strings"))
                })
                .collect::<Result<_, _>>()?;
            if !keys.is_empty() {
                self.host.write_keys(&term, &keys).map_err(|e| match e {
                    crate::hosttraits::HostError::InvalidKey(k) => {
                        ApiError::invalid_params(format!("invalid key `{k}`"))
                    }
                    other => ApiError::new(ErrorCode::Internal, other.to_string()),
                })?;
                did = true;
            }
        }
        if !did {
            return Err(ApiError::invalid_params("send_input requires `text` and/or `keys`"));
        }
        Ok(json!({"type": "ok"}))
    }

    /// agent.view.set/clear: one transient declarative projection for the
    /// Agents view. Stored server-side; clients re-read it via events.
    pub(crate) fn agent_view_set(&mut self, params: &Value) -> ApiResult {
        let source = params
            .get("source")
            .and_then(Value::as_str)
            .ok_or_else(|| ApiError::invalid_params("missing `source`"))?
            .to_string();
        crate::metadata::validate_source(&source)?;
        if let Some(rest) = source.strip_prefix("plugin:") {
            // Plugin-owned views require that plugin to exist and be enabled;
            // until plugin glue lands, reject as the docs demand.
            return Err(ApiError::new(
                ErrorCode::PluginDisabled,
                format!("plugin `{rest}` is missing or disabled"),
            ));
        }
        let label = params.get("label").and_then(Value::as_str).map(str::to_string);
        self.agent_view = Some(json!({
            "source": source,
            "label": label,
            "filter": params.get("filter").cloned(),
            "sort": params.get("sort").cloned(),
        }));
        self.emit("agent.view_changed", self.agent_view.clone().unwrap());
        Ok(json!({"type": "agent_view", "active": true, "source": source, "label": label}))
    }

    pub(crate) fn agent_view_clear(&mut self, params: &Value) -> ApiResult {
        let requested = params.get("source").and_then(Value::as_str);
        let active_source = self
            .agent_view
            .as_ref()
            .and_then(|v| v.get("source").and_then(Value::as_str))
            .map(str::to_string);
        match (requested, &active_source) {
            (Some(req), Some(act)) if req != act => {
                // Source mismatch leaves the active view unchanged.
                Ok(json!({"type": "agent_view", "active": true, "source": act}))
            }
            _ => {
                self.agent_view = None;
                self.emit("agent.view_changed", Value::Null);
                Ok(json!({"type": "agent_view", "active": false}))
            }
        }
    }

    fn parse_workspace_pub(&self, params: &Value, key: &str) -> Result<WorkspaceId, ApiError> {
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
}
