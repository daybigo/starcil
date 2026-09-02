//! Pure session-state model: the workspace → tab → pane hierarchy, id
//! allocation, focus bookkeeping, and structural mutations. No I/O here；the
//! server actor applies side effects only after these transformations succeed.

use crate::ids::{PaneId, TabId, WorkspaceId};
use crate::tree::{clamp_ratio, Node, SplitDirection};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Agent lifecycle as surfaced everywhere (sidebar, CLI, waits).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentStatus {
    Idle,
    Working,
    Blocked,
    Done,
    Unknown,
}

impl AgentStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            AgentStatus::Idle => "idle",
            AgentStatus::Working => "working",
            AgentStatus::Blocked => "blocked",
            AgentStatus::Done => "done",
            AgentStatus::Unknown => "unknown",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "idle" => AgentStatus::Idle,
            "working" => AgentStatus::Working,
            "blocked" => AgentStatus::Blocked,
            "done" => AgentStatus::Done,
            "unknown" => AgentStatus::Unknown,
            _ => return None,
        })
    }

    /// Rollup priority: blocked > working > done > idle > unknown.
    pub fn priority(&self) -> u8 {
        match self {
            AgentStatus::Blocked => 4,
            AgentStatus::Working => 3,
            AgentStatus::Done => 2,
            AgentStatus::Idle => 1,
            AgentStatus::Unknown => 0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PaneMeta {
    pub id: PaneId,
    /// Server-side terminal handle (opaque to clients).
    pub terminal_id: String,
    pub cwd: String,
    /// Manual pane label (rename), distinct from detected titles.
    pub label: Option<String>,
    /// Live agent name if one was assigned (`agent start` / `agent rename`).
    pub agent_name: Option<String>,
    /// Extra env for the pane process (never persisted with secrets).
    #[serde(default)]
    pub env: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Tab {
    pub id: TabId,
    pub label: String,
    pub tree: Node,
    pub focused_pane: PaneId,
    pub zoomed: Option<PaneId>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Workspace {
    pub id: WorkspaceId,
    pub label: String,
    pub cwd: String,
    pub tabs: Vec<Tab>,
    pub focused_tab: TabId,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// Metadata tokens reported via `workspace report-metadata`.
    #[serde(default)]
    pub tokens: BTreeMap<String, String>,
    /// Per-workspace tab counter (monotonic, never reused).
    next_tab: u64,
    /// Per-workspace pane counter (monotonic, never reused).
    next_pane: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionModel {
    pub workspaces: Vec<Workspace>,
    pub panes: BTreeMap<PaneId, PaneMeta>,
    pub focused_workspace: WorkspaceId,
    /// Monotonic revision, bumped by every structural mutation.
    pub revision: u64,
    next_workspace: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ModelError {
    #[error("not_found: {0}")]
    NotFound(String),
    #[error("invalid_state: {0}")]
    InvalidState(String),
}

pub type ModelResult<T> = Result<T, ModelError>;

impl SessionModel {
    /// A fresh session: one workspace, one tab, one pane.
    pub fn new(cwd: &str, make_terminal_id: impl FnOnce() -> String) -> Self {
        let mut model = SessionModel {
            workspaces: Vec::new(),
            panes: BTreeMap::new(),
            focused_workspace: WorkspaceId(1),
            revision: 0,
            next_workspace: 1,
        };
        model.create_workspace(cwd, None, BTreeMap::new(), make_terminal_id);
        model
    }

    pub fn bump(&mut self) -> u64 {
        self.revision += 1;
        self.revision
    }

    // ---- lookups ----

    pub fn workspace(&self, id: WorkspaceId) -> ModelResult<&Workspace> {
        self.workspaces
            .iter()
            .find(|w| w.id == id)
            .ok_or_else(|| ModelError::NotFound(format!("workspace {id}")))
    }

    pub fn workspace_mut(&mut self, id: WorkspaceId) -> ModelResult<&mut Workspace> {
        self.workspaces
            .iter_mut()
            .find(|w| w.id == id)
            .ok_or_else(|| ModelError::NotFound(format!("workspace {id}")))
    }

    pub fn tab(&self, id: TabId) -> ModelResult<&Tab> {
        self.workspace(WorkspaceId(id.workspace))?
            .tabs
            .iter()
            .find(|t| t.id == id)
            .ok_or_else(|| ModelError::NotFound(format!("tab {id}")))
    }

    pub fn tab_mut(&mut self, id: TabId) -> ModelResult<&mut Tab> {
        self.workspace_mut(WorkspaceId(id.workspace))?
            .tabs
            .iter_mut()
            .find(|t| t.id == id)
            .ok_or_else(|| ModelError::NotFound(format!("tab {id}")))
    }

    pub fn pane(&self, id: PaneId) -> ModelResult<&PaneMeta> {
        self.panes
            .get(&id)
            .ok_or_else(|| ModelError::NotFound(format!("pane {id}")))
    }

    pub fn pane_mut(&mut self, id: PaneId) -> ModelResult<&mut PaneMeta> {
        self.panes
            .get_mut(&id)
            .ok_or_else(|| ModelError::NotFound(format!("pane {id}")))
    }

    /// The tab currently holding `pane`.
    pub fn tab_of_pane(&self, pane: PaneId) -> ModelResult<TabId> {
        for w in &self.workspaces {
            for t in &w.tabs {
                if t.tree.contains(pane) {
                    return Ok(t.id);
                }
            }
        }
        Err(ModelError::NotFound(format!("pane {pane}")))
    }

    pub fn resolve_agent_name(&self, name: &str) -> Option<PaneId> {
        self.panes
            .values()
            .find(|p| p.agent_name.as_deref() == Some(name))
            .map(|p| p.id)
    }

    // ---- id peeks (what the NEXT allocation in a scope will return) ----
    // The server spawns terminals with STARCIL_* env before mutating the
    // model, so it needs the ids ahead of time. Single-threaded actor makes
    // peek-then-mutate safe.

    pub fn next_workspace_id(&self) -> WorkspaceId {
        WorkspaceId(self.next_workspace)
    }

    pub fn next_tab_id(&self, workspace: WorkspaceId) -> ModelResult<TabId> {
        Ok(TabId { workspace: workspace.0, tab: self.workspace(workspace)?.next_tab })
    }

    pub fn next_pane_id(&self, workspace: WorkspaceId) -> ModelResult<PaneId> {
        Ok(PaneId { workspace: workspace.0, pane: self.workspace(workspace)?.next_pane })
    }

    /// Ids the initial pane/tab of a BRAND NEW workspace will get.
    pub fn next_workspace_initial_ids(&self) -> (WorkspaceId, TabId, PaneId) {
        let w = self.next_workspace;
        (WorkspaceId(w), TabId { workspace: w, tab: 1 }, PaneId { workspace: w, pane: 1 })
    }

    // ---- mutations (all bump revision on success) ----

    /// Allocate the next pane id in a workspace (bumps the counter).
    pub fn allocate_pane_id(&mut self, workspace: WorkspaceId) -> ModelResult<PaneId> {
        let ws = self.workspace_mut(workspace)?;
        let id = PaneId { workspace: workspace.0, pane: ws.next_pane };
        ws.next_pane += 1;
        Ok(id)
    }

    /// Insert a fully prebuilt tab (used by layout.apply): the tree's pane set
    /// must exactly match the provided metas.
    pub fn insert_tab_prebuilt(
        &mut self,
        workspace: WorkspaceId,
        label: Option<String>,
        tree: Node,
        panes: Vec<PaneMeta>,
    ) -> ModelResult<TabId> {
        let mut tree_set: Vec<PaneId> = tree.panes();
        tree_set.sort();
        let mut meta_set: Vec<PaneId> = panes.iter().map(|p| p.id).collect();
        meta_set.sort();
        if tree_set != meta_set || tree_set.is_empty() {
            return Err(ModelError::InvalidState("tree panes and metas must match".into()));
        }
        let focused_pane = tree.panes()[0];
        let ws = self.workspace_mut(workspace)?;
        let tid = TabId { workspace: workspace.0, tab: ws.next_tab };
        ws.next_tab += 1;
        ws.tabs.push(Tab {
            id: tid,
            label: label.unwrap_or_else(|| format!("tab {}", tid.tab)),
            tree,
            focused_pane,
            zoomed: None,
        });
        for meta in panes {
            self.panes.insert(meta.id, meta);
        }
        self.bump();
        Ok(tid)
    }

    /// Reorder one workspace to `insert_index` (clamped). Returns false if
    /// the id is unknown.
    pub fn move_workspace(&mut self, id: WorkspaceId, insert_index: usize) -> bool {
        let Some(pos) = self.workspaces.iter().position(|w| w.id == id) else {
            return false;
        };
        let ws = self.workspaces.remove(pos);
        let idx = insert_index.min(self.workspaces.len());
        self.workspaces.insert(idx, ws);
        self.bump();
        true
    }

    /// Atomically move an ordered block of workspaces before `before` (None =
    /// to the end). Ids must be unique, known, and must not contain `before`.
    pub fn move_workspace_block(
        &mut self,
        block: &[WorkspaceId],
        before: Option<WorkspaceId>,
    ) -> ModelResult<()> {
        let mut seen = std::collections::BTreeSet::new();
        for id in block {
            if !seen.insert(*id) {
                return Err(ModelError::InvalidState(format!("duplicate workspace {id} in block")));
            }
            self.workspace(*id)?;
            if before == Some(*id) {
                return Err(ModelError::InvalidState("anchor cannot be part of the block".into()));
            }
        }
        if let Some(b) = before {
            self.workspace(b)?;
        }
        let mut moved = Vec::new();
        self.workspaces.retain(|w| {
            if block.contains(&w.id) {
                moved.push(w.clone());
                false
            } else {
                true
            }
        });
        moved.sort_by_key(|w| block.iter().position(|b| *b == w.id).unwrap());
        let idx = match before {
            Some(b) => self.workspaces.iter().position(|w| w.id == b).unwrap_or(self.workspaces.len()),
            None => self.workspaces.len(),
        };
        for (offset, w) in moved.into_iter().enumerate() {
            self.workspaces.insert(idx + offset, w);
        }
        self.bump();
        Ok(())
    }

    /// Reorder a tab within its workspace to `insert_index` (clamped).
    pub fn move_tab(&mut self, id: TabId, insert_index: usize) -> ModelResult<()> {
        let ws = self.workspace_mut(WorkspaceId(id.workspace))?;
        let Some(pos) = ws.tabs.iter().position(|t| t.id == id) else {
            return Err(ModelError::NotFound(format!("tab {id}")));
        };
        let tab = ws.tabs.remove(pos);
        let idx = insert_index.min(ws.tabs.len());
        ws.tabs.insert(idx, tab);
        self.bump();
        Ok(())
    }

    pub fn create_workspace(
        &mut self,
        cwd: &str,
        label: Option<String>,
        env: BTreeMap<String, String>,
        make_terminal_id: impl FnOnce() -> String,
    ) -> (WorkspaceId, TabId, PaneId) {
        let wid = WorkspaceId(self.next_workspace);
        self.next_workspace += 1;
        let mut ws = Workspace {
            id: wid,
            label: label.unwrap_or_else(|| format!("workspace {}", wid.0)),
            cwd: cwd.to_string(),
            tabs: Vec::new(),
            focused_tab: TabId { workspace: wid.0, tab: 1 },
            env,
            tokens: BTreeMap::new(),
            next_tab: 1,
            next_pane: 1,
        };
        let tid = TabId { workspace: wid.0, tab: ws.next_tab };
        ws.next_tab += 1;
        let pid = PaneId { workspace: wid.0, pane: ws.next_pane };
        ws.next_pane += 1;
        ws.tabs.push(Tab {
            id: tid,
            label: format!("tab {}", tid.tab),
            tree: Node::leaf(pid),
            focused_pane: pid,
            zoomed: None,
        });
        ws.focused_tab = tid;
        self.workspaces.push(ws);
        self.panes.insert(
            pid,
            PaneMeta {
                id: pid,
                terminal_id: make_terminal_id(),
                cwd: cwd.to_string(),
                label: None,
                agent_name: None,
                env: BTreeMap::new(),
            },
        );
        self.bump();
        (wid, tid, pid)
    }

    pub fn create_tab(
        &mut self,
        workspace: WorkspaceId,
        cwd: Option<&str>,
        label: Option<String>,
        env: BTreeMap<String, String>,
        make_terminal_id: impl FnOnce() -> String,
    ) -> ModelResult<(TabId, PaneId)> {
        let ws_cwd = self.workspace(workspace)?.cwd.clone();
        let cwd = cwd.map(str::to_string).unwrap_or(ws_cwd);
        let ws = self.workspace_mut(workspace)?;
        let tid = TabId { workspace: workspace.0, tab: ws.next_tab };
        ws.next_tab += 1;
        let pid = PaneId { workspace: workspace.0, pane: ws.next_pane };
        ws.next_pane += 1;
        ws.tabs.push(Tab {
            id: tid,
            label: label.unwrap_or_else(|| format!("tab {}", tid.tab)),
            tree: Node::leaf(pid),
            focused_pane: pid,
            zoomed: None,
        });
        self.panes.insert(
            pid,
            PaneMeta {
                id: pid,
                terminal_id: make_terminal_id(),
                cwd,
                label: None,
                agent_name: None,
                env,
            },
        );
        self.bump();
        Ok((tid, pid))
    }

    pub fn split_pane(
        &mut self,
        target: PaneId,
        direction: SplitDirection,
        ratio: f32,
        cwd: Option<&str>,
        env: BTreeMap<String, String>,
        make_terminal_id: impl FnOnce() -> String,
    ) -> ModelResult<PaneId> {
        let tab_id = self.tab_of_pane(target)?;
        let inherited_cwd = self.pane(target)?.cwd.clone();
        let ws = self.workspace_mut(WorkspaceId(target.workspace))?;
        let new_id = PaneId { workspace: target.workspace, pane: ws.next_pane };
        ws.next_pane += 1;
        let tab = self.tab_mut(tab_id)?;
        if !tab.tree.split(target, direction, new_id, clamp_ratio(ratio)) {
            return Err(ModelError::NotFound(format!("pane {target}")));
        }
        tab.zoomed = None;
        self.panes.insert(
            new_id,
            PaneMeta {
                id: new_id,
                terminal_id: make_terminal_id(),
                cwd: cwd.map(str::to_string).unwrap_or(inherited_cwd),
                label: None,
                agent_name: None,
                env,
            },
        );
        self.bump();
        Ok(new_id)
    }

    /// Close a pane. Returns the terminal_id to tear down and whether the
    /// containing tab (and possibly workspace) went away with it.
    pub fn close_pane(&mut self, target: PaneId) -> ModelResult<ClosedPane> {
        let tab_id = self.tab_of_pane(target)?;
        let meta = self
            .panes
            .remove(&target)
            .ok_or_else(|| ModelError::NotFound(format!("pane {target}")))?;
        let tab = self.tab_mut(tab_id)?;
        let mut closed_tab = false;
        match tab.tree.remove(target) {
            Ok(true) => {
                if tab.focused_pane == target {
                    tab.focused_pane = tab.tree.panes()[0];
                }
                if tab.zoomed == Some(target) {
                    tab.zoomed = None;
                }
            }
            Ok(false) => {
                closed_tab = true;
            }
            Err(()) => return Err(ModelError::NotFound(format!("pane {target}"))),
        }
        let mut closed_workspace = false;
        if closed_tab {
            let ws = self.workspace_mut(WorkspaceId(tab_id.workspace))?;
            ws.tabs.retain(|t| t.id != tab_id);
            if ws.tabs.is_empty() {
                let wid = ws.id;
                self.workspaces.retain(|w| w.id != wid);
                closed_workspace = true;
                if self.focused_workspace == wid {
                    if let Some(first) = self.workspaces.first() {
                        self.focused_workspace = first.id;
                    }
                }
            } else if ws.focused_tab == tab_id {
                ws.focused_tab = ws.tabs[0].id;
            }
        }
        self.bump();
        Ok(ClosedPane {
            terminal_id: meta.terminal_id,
            closed_tab: closed_tab.then_some(tab_id),
            closed_workspace: closed_workspace.then_some(WorkspaceId(tab_id.workspace)),
        })
    }

    pub fn close_tab(&mut self, id: TabId) -> ModelResult<Vec<ClosedPane>> {
        let panes = self.tab(id)?.tree.panes();
        let mut out = Vec::new();
        for p in panes {
            out.push(self.close_pane(p)?);
        }
        Ok(out)
    }

    pub fn close_workspace(&mut self, id: WorkspaceId) -> ModelResult<Vec<ClosedPane>> {
        let tabs: Vec<TabId> = self.workspace(id)?.tabs.iter().map(|t| t.id).collect();
        let mut out = Vec::new();
        for t in tabs {
            out.extend(self.close_tab(t)?);
        }
        Ok(out)
    }

    /// Move a pane into another tab (existing, splitting `target_pane` or the
    /// tab's focused pane). Pane keeps its terminal; it gets a NEW id when it
    /// changes workspace. Returns (new_pane_id, previous_pane_id).
    pub fn move_pane_to_tab(
        &mut self,
        pane: PaneId,
        dest_tab: TabId,
        direction: SplitDirection,
        target_pane: Option<PaneId>,
        ratio: f32,
    ) -> ModelResult<(PaneId, PaneId)> {
        let src_tab = self.tab_of_pane(pane)?;
        if src_tab == dest_tab {
            return Err(ModelError::InvalidState("pane already in target tab".into()));
        }
        let anchor = match target_pane {
            Some(t) => {
                if self.tab_of_pane(t)? != dest_tab {
                    return Err(ModelError::InvalidState(format!(
                        "target pane {t} is not in tab {dest_tab}"
                    )));
                }
                t
            }
            None => self.tab(dest_tab)?.focused_pane,
        };
        // Detach from source (keep meta + terminal).
        let mut meta = self.panes.remove(&pane).ok_or_else(|| ModelError::NotFound(format!("pane {pane}")))?;
        let tab = self.tab_mut(src_tab)?;
        match tab.tree.remove(pane) {
            Ok(true) => {
                if tab.focused_pane == pane {
                    tab.focused_pane = tab.tree.panes()[0];
                }
                if tab.zoomed == Some(pane) {
                    tab.zoomed = None;
                }
            }
            Ok(false) => {
                let ws = self.workspace_mut(WorkspaceId(src_tab.workspace))?;
                ws.tabs.retain(|t| t.id != src_tab);
                if ws.tabs.is_empty() {
                    let wid = ws.id;
                    self.workspaces.retain(|w| w.id != wid);
                    if self.focused_workspace == wid {
                        if let Some(first) = self.workspaces.first() {
                            self.focused_workspace = first.id;
                        }
                    }
                } else if ws.focused_tab == src_tab {
                    ws.focused_tab = ws.tabs[0].id;
                }
            }
            Err(()) => return Err(ModelError::NotFound(format!("pane {pane}"))),
        }
        // Re-id when crossing workspaces (ids are workspace-qualified).
        let new_id = if dest_tab.workspace == pane.workspace {
            pane
        } else {
            let ws = self.workspace_mut(WorkspaceId(dest_tab.workspace))?;
            let id = PaneId { workspace: dest_tab.workspace, pane: ws.next_pane };
            ws.next_pane += 1;
            id
        };
        meta.id = new_id;
        self.panes.insert(new_id, meta);
        let dest = self.tab_mut(dest_tab)?;
        if !dest.tree.split(anchor, direction, new_id, clamp_ratio(ratio)) {
            return Err(ModelError::InvalidState("destination anchor vanished".into()));
        }
        dest.zoomed = None;
        self.bump();
        Ok((new_id, pane))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClosedPane {
    pub terminal_id: String,
    pub closed_tab: Option<TabId>,
    pub closed_workspace: Option<WorkspaceId>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tree::SplitDirection;

    fn term_ids() -> impl FnMut() -> String {
        let mut n = 0;
        move || {
            n += 1;
            format!("term_{n}")
        }
    }

    #[test]
    fn fresh_session_shape() {
        let mut ids = term_ids();
        let m = SessionModel::new("C:/dev", &mut ids);
        assert_eq!(m.workspaces.len(), 1);
        assert_eq!(m.panes.len(), 1);
        assert_eq!(m.revision, 1);
        let w = &m.workspaces[0];
        assert_eq!(w.id.to_string(), "w1");
        assert_eq!(w.tabs[0].id.to_string(), "w1:t1");
        assert_eq!(w.tabs[0].focused_pane.to_string(), "w1:p1");
    }

    #[test]
    fn ids_never_reused() {
        let mut ids = term_ids();
        let mut m = SessionModel::new("C:/dev", &mut ids);
        let p1 = m.workspaces[0].tabs[0].focused_pane;
        let p2 = m.split_pane(p1, SplitDirection::Right, 0.5, None, Default::default(), &mut ids).unwrap();
        m.close_pane(p2).unwrap();
        let p3 = m.split_pane(p1, SplitDirection::Right, 0.5, None, Default::default(), &mut ids).unwrap();
        assert_ne!(p2, p3, "closed pane id must not be reused");
    }

    #[test]
    fn split_inherits_cwd() {
        let mut ids = term_ids();
        let mut m = SessionModel::new("C:/dev", &mut ids);
        let p1 = m.workspaces[0].tabs[0].focused_pane;
        let p2 = m.split_pane(p1, SplitDirection::Down, 0.5, None, Default::default(), &mut ids).unwrap();
        assert_eq!(m.pane(p2).unwrap().cwd, "C:/dev");
        let p3 = m
            .split_pane(p1, SplitDirection::Down, 0.5, Some("C:/other"), Default::default(), &mut ids)
            .unwrap();
        assert_eq!(m.pane(p3).unwrap().cwd, "C:/other");
    }

    #[test]
    fn closing_last_pane_closes_tab_and_workspace() {
        let mut ids = term_ids();
        let mut m = SessionModel::new("C:/dev", &mut ids);
        let (w2, t2, p) = {
            let (w, t, p) = m.create_workspace("C:/two", None, Default::default(), &mut ids);
            (w, t, p)
        };
        let closed = m.close_pane(p).unwrap();
        assert_eq!(closed.closed_tab, Some(t2));
        assert_eq!(closed.closed_workspace, Some(w2));
        assert!(m.workspace(w2).is_err());
    }

    #[test]
    fn move_pane_across_workspaces_reids() {
        let mut ids = term_ids();
        let mut m = SessionModel::new("C:/dev", &mut ids);
        let p1 = m.workspaces[0].tabs[0].focused_pane;
        let p2 = m.split_pane(p1, SplitDirection::Right, 0.5, None, Default::default(), &mut ids).unwrap();
        let (_, t2, _) = m.create_workspace("C:/two", None, Default::default(), &mut ids);
        let term_before = m.pane(p2).unwrap().terminal_id.clone();
        let (new_id, old_id) = m
            .move_pane_to_tab(p2, t2, SplitDirection::Right, None, 0.5)
            .unwrap();
        assert_eq!(old_id, p2);
        assert_ne!(new_id.workspace, p2.workspace);
        assert_eq!(m.pane(new_id).unwrap().terminal_id, term_before, "terminal survives the move");
        assert!(m.pane(p2).is_err());
    }

    #[test]
    fn agent_name_resolution() {
        let mut ids = term_ids();
        let mut m = SessionModel::new("C:/dev", &mut ids);
        let p1 = m.workspaces[0].tabs[0].focused_pane;
        m.pane_mut(p1).unwrap().agent_name = Some("reviewer".into());
        assert_eq!(m.resolve_agent_name("reviewer"), Some(p1));
        assert_eq!(m.resolve_agent_name("nobody"), None);
    }

    #[test]
    fn rollup_priority() {
        assert!(AgentStatus::Blocked.priority() > AgentStatus::Working.priority());
        assert!(AgentStatus::Working.priority() > AgentStatus::Done.priority());
        assert!(AgentStatus::Done.priority() > AgentStatus::Idle.priority());
        assert!(AgentStatus::Idle.priority() > AgentStatus::Unknown.priority());
    }
}
