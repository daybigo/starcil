//! ServerCore: the single-authority state machine behind the socket API.
//! Sync and I/O-free except through `TerminalHost`; the async layer (actor,
//! transport, subscriptions) wraps this and serializes all calls.

use crate::hosttraits::{ReadFormat, ReadSource, TerminalHost, TerminalSpawn};
use serde_json::{json, Value};
use starcil_domain::{
    AgentStatus, Node, PaneId, Rect, SessionModel, SplitDirection, TabId, WorkspaceId,
};
use starcil_protocol::error::{ApiError, ErrorCode};
use starcil_protocol::types::{
    PaneInfo, PaneLayoutEntry, PaneLayoutSnapshot, PaneRect, ScrollMetrics, TabInfo, WorkspaceInfo,
};
use std::collections::BTreeMap;

pub struct ServerCore<H: TerminalHost> {
    pub model: SessionModel,
    pub host: H,
    pub session_name: String,
    pub version: String,
    /// Content area of the most recently attached client (defaults 120x40).
    pub client_area: Rect,
    /// `ui.pane_borders`: whether the TUI frames panes in multi-pane tabs.
    pub pane_borders: bool,
    /// Bottom rows each pane cedes to client-drawn chrome (the in-pane
    /// composer). Keyed by pane; missing = 0.
    pub reserved_rows: std::collections::HashMap<starcil_domain::PaneId, u16>,
    pub pane_gap: u16,
    /// Events emitted by the last handled request; the async layer drains
    /// these into subscriber queues.
    pub pending_events: Vec<(String, Value)>,
    /// Display-only metadata per pane / workspace (socket API contract).
    pub pane_metadata: BTreeMap<PaneId, crate::metadata::MetadataStore>,
    pub workspace_metadata: BTreeMap<WorkspaceId, crate::metadata::MetadataStore>,
    /// ui.toast.delivery from config ("off" until the async layer loads config).
    pub toast_delivery: String,
    /// Whether a foreground client is currently attached.
    pub has_foreground_client: bool,
    /// Per-pane agent lifecycle state.
    pub agents: crate::agents_glue::AgentRegistry,
    /// Worktree provenance for linked child workspaces.
    pub worktree_provenance: BTreeMap<WorkspaceId, starcil_protocol::types::WorktreeProvenance>,
    /// Directory for new worktree checkouts ([worktrees] directory).
    pub worktrees_dir: std::path::PathBuf,
    /// [experimental] kitty_graphics flag from config.
    pub kitty_graphics: bool,
    /// Active transient Agents-view projection (agent.view.set).
    pub agent_view: Option<Value>,
    /// Home directory used by integration installs (injectable in tests).
    pub home_dir: std::path::PathBuf,
    /// Plugin host (initialized by the async layer with real paths).
    pub plugins: Option<crate::plugins_glue::PluginHost>,
    /// Active session-modal popup, if any.
    pub popup: Option<crate::plugins_glue::PopupState>,
}

pub type ApiResult = Result<Value, ApiError>;

impl<H: TerminalHost> ServerCore<H> {
    pub fn new(session_name: &str, cwd: &str, mut host: H) -> Result<Self, ApiError> {
        let (wid, tid, pid) = SessionModel::new_ids_probe();
        let term = spawn_shell(&mut host, cwd, wid, tid, pid, session_name)?;
        let model = SessionModel::new(cwd, || term);
        Ok(ServerCore {
            model,
            host,
            session_name: session_name.to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            client_area: Rect { x: 0, y: 0, width: 120, height: 40 },
            pane_borders: true,
            reserved_rows: std::collections::HashMap::new(),
            pane_gap: 1,
            pending_events: Vec::new(),
            pane_metadata: BTreeMap::new(),
            workspace_metadata: BTreeMap::new(),
            toast_delivery: "off".to_string(),
            has_foreground_client: false,
            agents: crate::agents_glue::AgentRegistry::new(),
            worktree_provenance: BTreeMap::new(),
            worktrees_dir: default_worktrees_dir(),
            kitty_graphics: false,
            agent_view: None,
            home_dir: std::env::var_os("USERPROFILE")
                .or_else(|| std::env::var_os("HOME"))
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|| std::path::PathBuf::from(".")),
            plugins: None,
            popup: None,
        })
    }

    pub fn emit(&mut self, event: &str, data: Value) {
        self.pending_events.push((event.to_string(), data));
    }

    // ---- param helpers ----

    pub fn parse_pane_id(&self, v: &Value, key: &str) -> Result<Option<PaneId>, ApiError> {
        match v.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::String(s)) => s
                .parse::<PaneId>()
                .map(Some)
                .map_err(|_| ApiError::invalid_params(format!("invalid pane id `{s}`"))),
            Some(other) => Err(ApiError::invalid_params(format!("{key} must be a string, got {other}"))),
        }
    }

    /// Resolve the target pane: explicit param, else caller pane, else the
    /// server's focused pane.
    pub fn resolve_pane(&self, params: &Value) -> Result<PaneId, ApiError> {
        if let Some(p) = self.parse_pane_id(params, "pane_id")? {
            self.model.pane(p)?;
            return Ok(p);
        }
        if let Some(p) = self.parse_pane_id(params, "caller_pane_id")? {
            self.model.pane(p)?;
            return Ok(p);
        }
        Ok(self.focused_pane())
    }

    pub fn focused_pane(&self) -> PaneId {
        let ws = self
            .model
            .workspace(self.model.focused_workspace)
            .expect("focused workspace always exists");
        let tab = self.model.tab(ws.focused_tab).expect("focused tab always exists");
        tab.focused_pane
    }

    pub fn focused_tab(&self) -> TabId {
        self.model
            .workspace(self.model.focused_workspace)
            .expect("focused workspace always exists")
            .focused_tab
    }

    // ---- info builders ----

    pub fn pane_info(&self, id: PaneId) -> Result<PaneInfo, ApiError> {
        let meta = self.model.pane(id)?;
        let tab = self.model.tab_of_pane(id)?;
        let title = self.host.terminal_title(&meta.terminal_id);
        let agent_entry = self.agents.panes.get(&id);
        Ok(PaneInfo {
            pane_id: id.to_string(),
            terminal_id: meta.terminal_id.clone(),
            workspace_id: WorkspaceId(id.workspace).to_string(),
            tab_id: tab.to_string(),
            focused: self.focused_pane() == id,
            cwd: meta.cwd.clone(),
            agent_status: agent_entry.map(|a| a.status).unwrap_or(AgentStatus::Unknown),
            revision: self.model.revision,
            label: meta.label.clone(),
            agent: agent_entry.and_then(|a| a.agent_id.clone()),
            agent_name: meta.agent_name.clone(),
            terminal_title_stripped: title.as_deref().map(strip_activity_glyph),
            terminal_title: title,
            foreground_cwd: None,
            agent_session: None,
            scroll: self.host.scroll_info(&meta.terminal_id).map(|s| ScrollMetrics {
                offset_from_bottom: s.offset_from_bottom,
                max_offset_from_bottom: s.max_offset_from_bottom,
                viewport_rows: s.viewport_rows,
            }),
            tokens: self
                .pane_metadata
                .get(&id)
                .map(|m| m.token_values())
                .unwrap_or_default(),
            state_change_seq: None,
            shell_idle: agent_entry.and_then(|a| a.shell_idle),
        })
    }

    pub fn workspace_info(&self, id: WorkspaceId) -> Result<WorkspaceInfo, ApiError> {
        let ws = self.model.workspace(id)?;
        Ok(WorkspaceInfo {
            workspace_id: id.to_string(),
            label: ws.label.clone(),
            cwd: ws.cwd.clone(),
            focused: self.model.focused_workspace == id,
            revision: self.model.revision,
            tabs: ws.tabs.iter().map(|t| t.id.to_string()).collect(),
            tokens: self
                .workspace_metadata
                .get(&id)
                .map(|m| m.token_values())
                .unwrap_or_default(),
            worktree: self.worktree_provenance.get(&id).cloned(),
        })
    }

    pub fn tab_info(&self, id: TabId) -> Result<TabInfo, ApiError> {
        let ws = self.model.workspace(WorkspaceId(id.workspace))?;
        let tab = self.model.tab(id)?;
        Ok(TabInfo {
            tab_id: id.to_string(),
            workspace_id: ws.id.to_string(),
            label: tab.label.clone(),
            focused: ws.focused_tab == id && self.model.focused_workspace == ws.id,
            panes: tab.tree.panes().iter().map(|p| p.to_string()).collect(),
            zoomed: tab.zoomed.map(|p| p.to_string()),
        })
    }

    pub fn layout_snapshot(&self, tab_id: TabId) -> Result<PaneLayoutSnapshot, ApiError> {
        let tab = self.model.tab(tab_id)?;
        let area = self.client_area;
        let rects = tab.tree.rects(area, self.pane_gap);
        Ok(PaneLayoutSnapshot {
            workspace_id: WorkspaceId(tab_id.workspace).to_string(),
            tab_id: tab_id.to_string(),
            area: to_pane_rect(area),
            focused_pane_id: tab.focused_pane.to_string(),
            zoomed: tab.zoomed.map(|p| p.to_string()),
            panes: rects
                .iter()
                .map(|(p, r)| PaneLayoutEntry {
                    pane_id: p.to_string(),
                    rect: to_pane_rect(*r),
                    focused: tab.focused_pane == *p,
                })
                .collect(),
        })
    }

    /// Resize every pane terminal to its current layout rect (content area,
    /// minus the border frame when pane borders are on). Called after
    /// structural mutations and client-area changes.
    pub fn sync_pty_sizes(&mut self) {
        let mut work: Vec<(String, u16, u16)> = Vec::new();
        for ws in &self.model.workspaces {
            for tab in &ws.tabs {
                // The TUI frames panes (1 cell each side) only when the tab has
                // more than one pane and borders are on; a lone pane gets the
                // whole area. Mirror that rule or the PTY is 2 cells off.
                let border = if self.pane_borders && tab.tree.panes().len() > 1 {
                    2u16
                } else {
                    0
                };
                if let Some(zoomed) = tab.zoomed {
                    if let Ok(meta) = self.model.pane(zoomed) {
                        let a = self.client_area;
                        let reserved = self.reserved_rows.get(&zoomed).copied().unwrap_or(0);
                        work.push((
                            meta.terminal_id.clone(),
                            a.width.saturating_sub(border).max(10),
                            a.height.saturating_sub(border).saturating_sub(reserved).max(3),
                        ));
                    }
                    continue;
                }
                for (pane, rect) in tab.tree.rects(self.client_area, self.pane_gap) {
                    if let Ok(meta) = self.model.pane(pane) {
                        let reserved = self.reserved_rows.get(&pane).copied().unwrap_or(0);
                        work.push((
                            meta.terminal_id.clone(),
                            rect.width.saturating_sub(border).max(10),
                            rect.height.saturating_sub(border).saturating_sub(reserved).max(3),
                        ));
                    }
                }
            }
        }
        for (term, cols, rows) in work {
            let _ = self.host.resize(&term, cols, rows);
        }
    }

    /// Spawn a terminal for a pane that is about to exist.
    pub fn spawn_for(
        &mut self,
        cwd: &str,
        wid: WorkspaceId,
        tid: TabId,
        pid: PaneId,
        command: Option<Vec<String>>,
        extra_env: BTreeMap<String, String>,
    ) -> Result<String, ApiError> {
        let mut env = extra_env;
        // Starcil-managed variables are authoritative on conflict.
        env.insert("STARCIL_ENV".into(), "1".into());
        env.insert("STARCIL_SESSION".into(), self.session_name.clone());
        env.insert("STARCIL_WORKSPACE_ID".into(), wid.to_string());
        env.insert("STARCIL_TAB_ID".into(), tid.to_string());
        env.insert("STARCIL_PANE_ID".into(), pid.to_string());
        let (rows, cols) = (self.client_area.height.max(4), self.client_area.width.max(20));
        self.host
            .spawn(TerminalSpawn { cwd: cwd.to_string(), command, env, rows, cols })
            .map_err(|e| ApiError::new(ErrorCode::Internal, format!("terminal spawn failed: {e}")))
    }

    pub fn read_terminal(
        &self,
        pane: PaneId,
        source: ReadSource,
        lines: usize,
        format: ReadFormat,
    ) -> Result<Value, ApiError> {
        let meta = self.model.pane(pane)?;
        let out = self
            .host
            .read(&meta.terminal_id, source, lines, format)
            .map_err(|e| ApiError::new(ErrorCode::Internal, e.to_string()))?;
        Ok(json!({
            "type": "pane_read",
            "pane_id": pane.to_string(),
            "source": source,
            "format": format,
            "lines": out.lines,
            "text": out.text,
        }))
    }
}

fn spawn_shell<H: TerminalHost>(
    host: &mut H,
    cwd: &str,
    wid: WorkspaceId,
    tid: TabId,
    pid: PaneId,
    session: &str,
) -> Result<String, ApiError> {
    let mut env = BTreeMap::new();
    env.insert("STARCIL_ENV".into(), "1".into());
    env.insert("STARCIL_SESSION".into(), session.to_string());
    env.insert("STARCIL_WORKSPACE_ID".into(), wid.to_string());
    env.insert("STARCIL_TAB_ID".into(), tid.to_string());
    env.insert("STARCIL_PANE_ID".into(), pid.to_string());
    host.spawn(TerminalSpawn { cwd: cwd.to_string(), command: None, env, rows: 40, cols: 120 })
        .map_err(|e| ApiError::new(ErrorCode::Internal, format!("terminal spawn failed: {e}")))
}

/// ~/.starcil/worktrees (USERPROFILE on Windows, HOME elsewhere).
pub fn default_worktrees_dir() -> std::path::PathBuf {
    let home = std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    home.join(".starcil").join("worktrees")
}

pub fn to_pane_rect(r: Rect) -> PaneRect {
    PaneRect { x: r.x, y: r.y, width: r.width, height: r.height }
}

/// Remove one recognized leading activity/spinner glyph plus whitespace.
pub fn strip_activity_glyph(title: &str) -> String {
    const GLYPHS: &[char] = &[
        '◐', '◓', '◑', '◒', '⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏', '✻', '✽', '·', '∗', '●', '○',
    ];
    let trimmed = title.trim_start();
    if let Some(first) = trimmed.chars().next() {
        if GLYPHS.contains(&first) {
            return trimmed[first.len_utf8()..].trim_start().to_string();
        }
    }
    title.to_string()
}

// Small extension so ServerCore::new can compute initial env ids without a
// chicken-and-egg dance with SessionModel::new.
trait ProbeIds {
    fn new_ids_probe() -> (WorkspaceId, TabId, PaneId);
}

impl ProbeIds for SessionModel {
    fn new_ids_probe() -> (WorkspaceId, TabId, PaneId) {
        (
            WorkspaceId(1),
            TabId { workspace: 1, tab: 1 },
            PaneId { workspace: 1, pane: 1 },
        )
    }
}

/// Parse split direction from params.
pub fn parse_direction(v: &Value, key: &str) -> Result<SplitDirection, ApiError> {
    match v.get(key).and_then(Value::as_str) {
        Some("right") => Ok(SplitDirection::Right),
        Some("down") => Ok(SplitDirection::Down),
        Some(other) => Err(ApiError::invalid_params(format!("direction must be right|down, got `{other}`"))),
        None => Err(ApiError::invalid_params(format!("missing `{key}`"))),
    }
}

pub fn parse_nav_direction(v: &Value, key: &str) -> Result<starcil_domain::Direction, ApiError> {
    match v.get(key).and_then(Value::as_str) {
        Some("left") => Ok(starcil_domain::Direction::Left),
        Some("right") => Ok(starcil_domain::Direction::Right),
        Some("up") => Ok(starcil_domain::Direction::Up),
        Some("down") => Ok(starcil_domain::Direction::Down),
        Some(other) => Err(ApiError::invalid_params(format!(
            "direction must be left|right|up|down, got `{other}`"
        ))),
        None => Err(ApiError::invalid_params(format!("missing `{key}`"))),
    }
}

pub fn parse_env_map(v: &Value) -> Result<BTreeMap<String, String>, ApiError> {
    match v.get("env") {
        None | Some(Value::Null) => Ok(BTreeMap::new()),
        Some(Value::Object(map)) => {
            let mut out = BTreeMap::new();
            for (k, val) in map {
                match val.as_str() {
                    Some(s) => {
                        out.insert(k.clone(), s.to_string());
                    }
                    None => return Err(ApiError::invalid_params(format!("env.{k} must be a string"))),
                }
            }
            Ok(out)
        }
        Some(_) => Err(ApiError::invalid_params("env must be an object of strings")),
    }
}

pub fn node_contains(tree: &Node, pane: PaneId) -> bool {
    tree.contains(pane)
}
