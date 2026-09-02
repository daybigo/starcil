//! Server-side session persistence and boot restoration.

use crate::agents_glue::AgentRegistry;
use crate::core::ServerCore;
use crate::hosttraits::TerminalHost;
use serde_json::json;
use starcil_domain::{Node, PaneId, PaneMeta, SessionModel, TabId, WorkspaceId};
use starcil_persist::{
    plan_restore, PaneExtras, RestoreLaunch, RestoreLayout, RestoreOptions, RestorePane,
    RestorePlan, RestoreTab, RestoreWorkspace, ResumeFailure, SaveError, SessionRef, SlotKey,
    StateDoc,
};
use starcil_platform::{PathError, PlatformPaths};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

pub const SAVE_DEBOUNCE: Duration = Duration::from_millis(500);

type SessionRecipes = BTreeMap<String, BTreeMap<PaneId, Vec<String>>>;

static RECIPES: OnceLock<Mutex<SessionRecipes>> = OnceLock::new();

fn recipes() -> &'static Mutex<SessionRecipes> {
    RECIPES.get_or_init(|| Mutex::new(BTreeMap::new()))
}

/// Mutable save scheduling state. The actor owns one instance per session.
#[derive(Debug)]
pub struct PersistenceState {
    pub path: PathBuf,
    pub dirty: bool,
    pub last_save: Instant,
}

impl PersistenceState {
    pub fn new(path: PathBuf) -> Self {
        let now = Instant::now();
        Self {
            path,
            dirty: false,
            last_save: now.checked_sub(SAVE_DEBOUNCE).unwrap_or(now),
        }
    }

    pub fn for_session(paths: &PlatformPaths, session: &str) -> Result<Self, PathError> {
        state_path(paths, session).map(Self::new)
    }

    pub fn mark_dirty(&mut self) {
        self.dirty = true;
    }
}

/// Runtime bindings produced while old persisted ids are mapped to fresh ids.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RuntimeBindings {
    pub workspaces: BTreeMap<WorkspaceId, WorkspaceId>,
    pub tabs: BTreeMap<TabId, TabId>,
    pub panes: BTreeMap<PaneId, PaneId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestoreLaunchFailure {
    pub old_pane_id: PaneId,
    pub pane_id: PaneId,
    pub command: Vec<String>,
    pub message: String,
    pub fell_back_to_shell: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RestoreReport {
    pub success: bool,
    pub restored_workspaces: usize,
    pub restored_tabs: usize,
    pub restored_panes: usize,
    pub rebound_sessions: usize,
    pub bindings: RuntimeBindings,
    pub could_not_resume: Vec<ResumeFailure>,
    pub launch_failures: Vec<RestoreLaunchFailure>,
    pub warnings: Vec<String>,
    pub fatal_error: Option<String>,
}

/// Record a recipe outside `ServerCore`, keeping concurrent core edits isolated.
pub fn remember_recipe(session: &str, pane: PaneId, argv: Vec<String>) {
    if argv.is_empty() {
        forget_recipe(session, pane);
        return;
    }
    recipes()
        .lock()
        .expect("recipe registry lock poisoned")
        .entry(session.to_owned())
        .or_default()
        .insert(pane, argv);
}

pub fn forget_recipe(session: &str, pane: PaneId) {
    let mut all = recipes().lock().expect("recipe registry lock poisoned");
    if let Some(session_recipes) = all.get_mut(session) {
        session_recipes.remove(&pane);
        if session_recipes.is_empty() {
            all.remove(session);
        }
    }
}

pub fn remap_recipe(session: &str, from: PaneId, to: PaneId) {
    let mut all = recipes().lock().expect("recipe registry lock poisoned");
    let Some(session_recipes) = all.get_mut(session) else {
        return;
    };
    if let Some(argv) = session_recipes.remove(&from) {
        session_recipes.insert(to, argv);
    }
}

pub fn clear_recipes(session: &str) {
    recipes()
        .lock()
        .expect("recipe registry lock poisoned")
        .remove(session);
}

pub fn state_path(paths: &PlatformPaths, session: &str) -> Result<PathBuf, PathError> {
    Ok(paths
        .session_runtime_dir(session)?
        .join(format!("state-{session}.json")))
}

/// Capture the durable portion of the live server state.
pub fn capture<H: TerminalHost>(core: &ServerCore<H>) -> StateDoc {
    let session_recipes = recipes()
        .lock()
        .expect("recipe registry lock poisoned")
        .get(&core.session_name)
        .cloned()
        .unwrap_or_default();
    let mut pane_extras = BTreeMap::new();

    for pane_id in core.model.panes.keys().copied() {
        let agent = core.agents.panes.get(&pane_id);
        let agent_session = agent
            .and_then(|entry| entry.session_ref.clone())
            .and_then(|value| serde_json::from_value::<SessionRef>(value).ok());
        let agent_kind = agent
            .and_then(|entry| entry.agent_id.clone())
            .or_else(|| agent_session.as_ref().map(|session| session.agent.clone()));
        let recipe_argv = session_recipes.get(&pane_id).cloned();
        let extras = PaneExtras {
            agent_kind,
            agent_session,
            recipe_argv,
        };
        if extras != PaneExtras::default() {
            pane_extras.insert(pane_id, extras);
        }
    }

    StateDoc::new(core.session_name.clone(), core.model.clone(), pane_extras)
        .expect("ServerCore always has a valid focused workspace, tab, and pane")
}

/// Save at most once per debounce window. Returns true only when a file was written.
pub fn save_if_dirty<H: TerminalHost>(
    state: &mut PersistenceState,
    core: &ServerCore<H>,
) -> Result<bool, SaveError> {
    if !state.dirty || state.last_save.elapsed() < SAVE_DEBOUNCE {
        return Ok(false);
    }
    let doc = capture(core);
    starcil_persist::save_atomic(&state.path, &doc)?;
    state.dirty = false;
    state.last_save = Instant::now();
    Ok(true)
}

/// Recreate a saved session using fresh model ids and fresh terminal processes.
/// The existing model is replaced only after every required shell can be spawned.
pub fn restore_at_boot<H: TerminalHost>(
    core: &mut ServerCore<H>,
    doc: StateDoc,
    resume_agents: bool,
) -> RestoreReport {
    let resume = |agent: &str, session: &SessionRef| {
        starcil_integrations::resume_command(agent, &session.value)
    };
    let plan = plan_restore(
        &doc,
        RestoreOptions {
            resume_agents,
            resume_command: &resume,
        },
    );
    let old_agent_names: BTreeMap<PaneId, Option<String>> = doc
        .model
        .panes
        .iter()
        .map(|(pane, meta)| (*pane, meta.agent_name.clone()))
        .collect();
    let mut report = RestoreReport {
        could_not_resume: plan.could_not_resume.clone(),
        ..RestoreReport::default()
    };

    let execution = match execute_plan(core, &plan, &old_agent_names, &mut report) {
        Ok(execution) => execution,
        Err(error) => {
            report.fatal_error = Some(error);
            return report;
        }
    };

    let old_terminals: Vec<String> = core
        .model
        .panes
        .values()
        .map(|pane| pane.terminal_id.clone())
        .collect();
    for terminal_id in old_terminals {
        let _ = core.host.kill(&terminal_id);
    }

    core.model = execution.model;
    core.agents = AgentRegistry::new();
    core.pane_metadata.clear();
    core.workspace_metadata.clear();
    core.worktree_provenance.clear();
    core.pending_events.clear();
    clear_recipes(&core.session_name);
    for (pane, argv) in execution.recipes {
        remember_recipe(&core.session_name, pane, argv);
    }

    for workspace in &plan.workspaces {
        for tab in &workspace.tabs {
            for pane in &tab.panes {
                let Some(session) = &pane.agent_session else {
                    continue;
                };
                let Some(new_pane) = execution.slot_panes.get(&pane.slot).copied() else {
                    continue;
                };
                let mut params = json!({
                    "pane_id": new_pane.to_string(),
                    "source": session.source,
                    "state": "unknown",
                    "agent": pane.agent_kind.as_deref().unwrap_or(&session.agent),
                });
                let key = if session.kind == "path" {
                    "agent_session_path"
                } else {
                    "agent_session_id"
                };
                params[key] = json!(session.value);
                match core.report_agent_handler(&params) {
                    Ok(_) => report.rebound_sessions += 1,
                    Err(error) => report.warnings.push(format!(
                        "could not rebind session for {}: {error}",
                        pane.old_id
                    )),
                }
            }
        }
    }

    core.sync_pty_sizes();
    report.success = true;
    report.restored_workspaces = plan.workspaces.len();
    report.restored_tabs = plan
        .workspaces
        .iter()
        .map(|workspace| workspace.tabs.len())
        .sum();
    report.restored_panes = plan
        .workspaces
        .iter()
        .flat_map(|workspace| &workspace.tabs)
        .map(|tab| tab.panes.len())
        .sum();
    report.bindings = execution.bindings;
    report
}

struct RestoreExecution {
    model: SessionModel,
    bindings: RuntimeBindings,
    slot_panes: BTreeMap<SlotKey, PaneId>,
    recipes: Vec<(PaneId, Vec<String>)>,
}

fn execute_plan<H: TerminalHost>(
    core: &mut ServerCore<H>,
    plan: &RestorePlan,
    old_agent_names: &BTreeMap<PaneId, Option<String>>,
    report: &mut RestoreReport,
) -> Result<RestoreExecution, String> {
    let first_workspace = plan
        .workspaces
        .first()
        .ok_or_else(|| "restore plan has no workspace".to_owned())?;
    let first_tab = first_workspace
        .tabs
        .first()
        .ok_or_else(|| "restore plan has no tab".to_owned())?;
    let first_pane = first_tab
        .panes
        .first()
        .ok_or_else(|| "restore plan has no pane".to_owned())?;
    let (first_workspace_id, first_tab_id, first_pane_id) =
        (WorkspaceId(1), TabId { workspace: 1, tab: 1 }, PaneId { workspace: 1, pane: 1 });
    let mut spawned = Vec::new();
    let mut restored_recipes = Vec::new();
    let first_terminal = match spawn_planned(
        core,
        first_workspace_id,
        first_tab_id,
        first_pane_id,
        first_pane,
        report,
        &mut restored_recipes,
    ) {
        Ok(terminal) => terminal,
        Err(error) => return Err(error),
    };
    spawned.push(first_terminal.clone());

    let mut model = SessionModel::new(&first_workspace.cwd, || first_terminal);
    let mut bindings = RuntimeBindings::default();
    let mut slot_workspaces = BTreeMap::new();
    let mut slot_tabs = BTreeMap::new();
    let mut slot_panes = BTreeMap::new();

    let result = (|| {
        populate_workspace(
            core,
            &mut model,
            first_workspace,
            first_workspace_id,
            first_tab_id,
            first_pane_id,
            old_agent_names,
            report,
            &mut spawned,
            &mut restored_recipes,
            &mut bindings,
            &mut slot_workspaces,
            &mut slot_tabs,
            &mut slot_panes,
        )?;

        for workspace in plan.workspaces.iter().skip(1) {
            let first_tab = workspace
                .tabs
                .first()
                .ok_or_else(|| format!("workspace {} has no tab", workspace.old_id))?;
            let first_pane = first_tab
                .panes
                .first()
                .ok_or_else(|| format!("tab {} has no pane", first_tab.old_id))?;
            let (workspace_id, tab_id, pane_id) = model.next_workspace_initial_ids();
            let terminal = spawn_planned(
                core,
                workspace_id,
                tab_id,
                pane_id,
                first_pane,
                report,
                &mut restored_recipes,
            )?;
            spawned.push(terminal.clone());
            let created = model.create_workspace(
                &workspace.cwd,
                Some(workspace.label.clone()),
                BTreeMap::new(),
                || terminal,
            );
            if created != (workspace_id, tab_id, pane_id) {
                return Err("workspace allocator diverged during restore".to_owned());
            }
            populate_workspace(
                core,
                &mut model,
                workspace,
                workspace_id,
                tab_id,
                pane_id,
                old_agent_names,
                report,
                &mut spawned,
                &mut restored_recipes,
                &mut bindings,
                &mut slot_workspaces,
                &mut slot_tabs,
                &mut slot_panes,
            )?;
        }

        apply_global_focus(
            &mut model,
            plan,
            &slot_workspaces,
            &slot_tabs,
            &slot_panes,
        )?;
        Ok(())
    })();

    if let Err(error) = result {
        for terminal_id in spawned {
            let _ = core.host.kill(&terminal_id);
        }
        return Err(error);
    }

    Ok(RestoreExecution {
        model,
        bindings,
        slot_panes,
        recipes: restored_recipes,
    })
}

#[allow(clippy::too_many_arguments)]
fn populate_workspace<H: TerminalHost>(
    core: &mut ServerCore<H>,
    model: &mut SessionModel,
    workspace: &RestoreWorkspace,
    workspace_id: WorkspaceId,
    initial_tab_id: TabId,
    initial_pane_id: PaneId,
    old_agent_names: &BTreeMap<PaneId, Option<String>>,
    report: &mut RestoreReport,
    spawned: &mut Vec<String>,
    restored_recipes: &mut Vec<(PaneId, Vec<String>)>,
    bindings: &mut RuntimeBindings,
    slot_workspaces: &mut BTreeMap<SlotKey, WorkspaceId>,
    slot_tabs: &mut BTreeMap<SlotKey, TabId>,
    slot_panes: &mut BTreeMap<SlotKey, PaneId>,
) -> Result<(), String> {
    let first_tab = workspace
        .tabs
        .first()
        .ok_or_else(|| format!("workspace {} has no tab", workspace.old_id))?;
    let first_pane = first_tab
        .panes
        .first()
        .ok_or_else(|| format!("tab {} has no pane", first_tab.old_id))?;
    bindings.workspaces.insert(workspace.old_id, workspace_id);
    slot_workspaces.insert(workspace.slot.clone(), workspace_id);
    bindings.tabs.insert(first_tab.old_id, initial_tab_id);
    slot_tabs.insert(first_tab.slot.clone(), initial_tab_id);
    bind_pane(
        first_pane,
        initial_pane_id,
        bindings,
        slot_panes,
    );
    configure_existing_pane(model, first_pane, initial_pane_id, old_agent_names)?;

    for pane in first_tab.panes.iter().skip(1) {
        let pane_id = model
            .allocate_pane_id(workspace_id)
            .map_err(|error| error.to_string())?;
        let terminal = spawn_planned(
            core,
            workspace_id,
            initial_tab_id,
            pane_id,
            pane,
            report,
            restored_recipes,
        )?;
        spawned.push(terminal.clone());
        bind_pane(pane, pane_id, bindings, slot_panes);
        model.panes.insert(
            pane_id,
            pane_meta(pane, pane_id, terminal, old_agent_names),
        );
    }
    configure_tab(model, first_tab, initial_tab_id, slot_panes)?;

    for tab in workspace.tabs.iter().skip(1) {
        let tab_id = model
            .next_tab_id(workspace_id)
            .map_err(|error| error.to_string())?;
        let mut metas = Vec::with_capacity(tab.panes.len());
        for pane in &tab.panes {
            let pane_id = model
                .allocate_pane_id(workspace_id)
                .map_err(|error| error.to_string())?;
            let terminal = spawn_planned(
                core,
                workspace_id,
                tab_id,
                pane_id,
                pane,
                report,
                restored_recipes,
            )?;
            spawned.push(terminal.clone());
            bind_pane(pane, pane_id, bindings, slot_panes);
            metas.push(pane_meta(pane, pane_id, terminal, old_agent_names));
        }
        let tree = remap_layout(&tab.layout, slot_panes)?;
        let inserted = model
            .insert_tab_prebuilt(workspace_id, Some(tab.label.clone()), tree, metas)
            .map_err(|error| error.to_string())?;
        if inserted != tab_id {
            return Err("tab allocator diverged during restore".to_owned());
        }
        bindings.tabs.insert(tab.old_id, tab_id);
        slot_tabs.insert(tab.slot.clone(), tab_id);
        configure_tab_focus(model, tab, tab_id, slot_panes)?;
    }

    {
        let restored = model
            .workspace_mut(workspace_id)
            .map_err(|error| error.to_string())?;
        restored.label = workspace.label.clone();
        restored.cwd = workspace.cwd.clone();
        if let Some(slot) = &workspace.focused_tab {
            if let Some(tab_id) = slot_tabs.get(slot).copied() {
                restored.focused_tab = tab_id;
            }
        }
    }
    Ok(())
}

fn bind_pane(
    pane: &RestorePane,
    pane_id: PaneId,
    bindings: &mut RuntimeBindings,
    slot_panes: &mut BTreeMap<SlotKey, PaneId>,
) {
    bindings.panes.insert(pane.old_id, pane_id);
    slot_panes.insert(pane.slot.clone(), pane_id);
}

fn configure_existing_pane(
    model: &mut SessionModel,
    pane: &RestorePane,
    pane_id: PaneId,
    old_agent_names: &BTreeMap<PaneId, Option<String>>,
) -> Result<(), String> {
    let meta = model.pane_mut(pane_id).map_err(|error| error.to_string())?;
    meta.cwd = pane.cwd.clone();
    meta.label = pane.label.clone();
    meta.agent_name = old_agent_names.get(&pane.old_id).cloned().flatten();
    meta.env.clear();
    Ok(())
}

fn pane_meta(
    pane: &RestorePane,
    pane_id: PaneId,
    terminal_id: String,
    old_agent_names: &BTreeMap<PaneId, Option<String>>,
) -> PaneMeta {
    PaneMeta {
        id: pane_id,
        terminal_id,
        cwd: pane.cwd.clone(),
        label: pane.label.clone(),
        agent_name: old_agent_names.get(&pane.old_id).cloned().flatten(),
        env: BTreeMap::new(),
    }
}

fn configure_tab(
    model: &mut SessionModel,
    tab: &RestoreTab,
    tab_id: TabId,
    slot_panes: &BTreeMap<SlotKey, PaneId>,
) -> Result<(), String> {
    let tree = remap_layout(&tab.layout, slot_panes)?;
    let restored = model.tab_mut(tab_id).map_err(|error| error.to_string())?;
    restored.label = tab.label.clone();
    restored.tree = tree;
    configure_tab_fields(restored, tab, slot_panes)
}

fn configure_tab_focus(
    model: &mut SessionModel,
    tab: &RestoreTab,
    tab_id: TabId,
    slot_panes: &BTreeMap<SlotKey, PaneId>,
) -> Result<(), String> {
    let restored = model.tab_mut(tab_id).map_err(|error| error.to_string())?;
    configure_tab_fields(restored, tab, slot_panes)
}

fn configure_tab_fields(
    restored: &mut starcil_domain::Tab,
    tab: &RestoreTab,
    slot_panes: &BTreeMap<SlotKey, PaneId>,
) -> Result<(), String> {
    if let Some(slot) = &tab.focused_pane {
        restored.focused_pane = slot_panes
            .get(slot)
            .copied()
            .ok_or_else(|| format!("missing focused pane slot {}", slot.0))?;
    }
    restored.zoomed = tab
        .zoomed_pane
        .as_ref()
        .and_then(|slot| slot_panes.get(slot).copied());
    Ok(())
}

fn apply_global_focus(
    model: &mut SessionModel,
    plan: &RestorePlan,
    slot_workspaces: &BTreeMap<SlotKey, WorkspaceId>,
    slot_tabs: &BTreeMap<SlotKey, TabId>,
    slot_panes: &BTreeMap<SlotKey, PaneId>,
) -> Result<(), String> {
    if let Some(slot) = &plan.focus.workspace {
        model.focused_workspace = slot_workspaces
            .get(slot)
            .copied()
            .ok_or_else(|| format!("missing focused workspace slot {}", slot.0))?;
    }
    if let Some(slot) = &plan.focus.tab {
        let tab_id = slot_tabs
            .get(slot)
            .copied()
            .ok_or_else(|| format!("missing focused tab slot {}", slot.0))?;
        model
            .workspace_mut(WorkspaceId(tab_id.workspace))
            .map_err(|error| error.to_string())?
            .focused_tab = tab_id;
    }
    if let Some(slot) = &plan.focus.pane {
        let pane_id = slot_panes
            .get(slot)
            .copied()
            .ok_or_else(|| format!("missing focused pane slot {}", slot.0))?;
        let tab_id = model
            .tab_of_pane(pane_id)
            .map_err(|error| error.to_string())?;
        model
            .tab_mut(tab_id)
            .map_err(|error| error.to_string())?
            .focused_pane = pane_id;
    }
    Ok(())
}

fn remap_layout(
    layout: &RestoreLayout,
    slot_panes: &BTreeMap<SlotKey, PaneId>,
) -> Result<Node, String> {
    match layout {
        RestoreLayout::Pane { slot } => slot_panes
            .get(slot)
            .copied()
            .map(Node::Leaf)
            .ok_or_else(|| format!("missing pane slot {}", slot.0)),
        RestoreLayout::Split {
            axis,
            ratio,
            first,
            second,
        } => Ok(Node::Split {
            axis: *axis,
            ratio: *ratio,
            first: Box::new(remap_layout(first, slot_panes)?),
            second: Box::new(remap_layout(second, slot_panes)?),
        }),
    }
}

fn spawn_planned<H: TerminalHost>(
    core: &mut ServerCore<H>,
    workspace_id: WorkspaceId,
    tab_id: TabId,
    pane_id: PaneId,
    pane: &RestorePane,
    report: &mut RestoreReport,
    restored_recipes: &mut Vec<(PaneId, Vec<String>)>,
) -> Result<String, String> {
    let command = match &pane.launch {
        RestoreLaunch::Shell => None,
        RestoreLaunch::Resume { argv } | RestoreLaunch::Recipe { argv } => Some(argv.clone()),
    };
    match core.spawn_for(
        &pane.cwd,
        workspace_id,
        tab_id,
        pane_id,
        command.clone(),
        BTreeMap::new(),
    ) {
        Ok(terminal_id) => {
            if let RestoreLaunch::Recipe { argv } = &pane.launch {
                restored_recipes.push((pane_id, argv.clone()));
            }
            Ok(terminal_id)
        }
        Err(primary) if command.is_some() => {
            report.launch_failures.push(RestoreLaunchFailure {
                old_pane_id: pane.old_id,
                pane_id,
                command: command.unwrap_or_default(),
                message: primary.to_string(),
                fell_back_to_shell: true,
            });
            core.spawn_for(
                &pane.cwd,
                workspace_id,
                tab_id,
                pane_id,
                None,
                BTreeMap::new(),
            )
            .map_err(|fallback| {
                format!(
                    "could not restore pane {}: command failed ({primary}); shell fallback failed ({fallback})",
                    pane.old_id
                )
            })
        }
        Err(error) => Err(format!(
            "could not spawn shell for pane {}: {error}",
            pane.old_id
        )),
    }
}
