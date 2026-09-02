use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use starcil_domain::{Axis, Node, PaneId, TabId, WorkspaceId};

use crate::{PaneExtras, SessionRef, StateDoc};

pub struct RestoreOptions<'a> {
    pub resume_agents: bool,
    pub resume_command: &'a dyn Fn(&str, &SessionRef) -> Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RestorePlan {
    pub session: String,
    pub workspaces: Vec<RestoreWorkspace>,
    pub bindings: RestoreBindings,
    pub focus: RestoreFocus,
    pub could_not_resume: Vec<ResumeFailure>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RestoreWorkspace {
    pub old_id: WorkspaceId,
    pub slot: SlotKey,
    pub cwd: String,
    pub label: String,
    pub tabs: Vec<RestoreTab>,
    pub focused_tab: Option<SlotKey>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RestoreTab {
    pub old_id: TabId,
    pub slot: SlotKey,
    pub label: String,
    pub panes: Vec<RestorePane>,
    pub layout: RestoreLayout,
    pub focused_pane: Option<SlotKey>,
    pub zoomed_pane: Option<SlotKey>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RestorePane {
    pub old_id: PaneId,
    pub slot: SlotKey,
    pub cwd: String,
    pub label: Option<String>,
    pub agent_kind: Option<String>,
    pub agent_session: Option<SessionRef>,
    pub launch: RestoreLaunch,
    pub resume_failed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RestoreLaunch {
    Shell,
    Resume { argv: Vec<String> },
    Recipe { argv: Vec<String> },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RestoreLayout {
    Pane {
        slot: SlotKey,
    },
    Split {
        axis: Axis,
        ratio: f32,
        first: Box<RestoreLayout>,
        second: Box<RestoreLayout>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SlotKey(pub String);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct RestoreBindings {
    pub workspaces: BTreeMap<WorkspaceId, SlotKey>,
    pub tabs: BTreeMap<TabId, SlotKey>,
    pub panes: BTreeMap<PaneId, SlotKey>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RestoreFocus {
    pub workspace: Option<SlotKey>,
    pub tab: Option<SlotKey>,
    pub pane: Option<SlotKey>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResumeFailure {
    pub old_pane_id: PaneId,
    pub slot: SlotKey,
    pub agent: String,
    pub session: SessionRef,
}

pub fn plan_restore(doc: &StateDoc, options: RestoreOptions<'_>) -> RestorePlan {
    let mut bindings = RestoreBindings::default();

    for (workspace_index, workspace) in doc.model.workspaces.iter().enumerate() {
        let workspace_slot = SlotKey(format!("workspace:{workspace_index}"));
        bindings
            .workspaces
            .insert(workspace.id, workspace_slot.clone());
        for (tab_index, tab) in workspace.tabs.iter().enumerate() {
            let tab_slot = SlotKey(format!(
                "workspace:{workspace_index}/tab:{tab_index}"
            ));
            bindings.tabs.insert(tab.id, tab_slot);
            for (pane_index, pane_id) in tab.tree.panes().into_iter().enumerate() {
                bindings.panes.insert(
                    pane_id,
                    SlotKey(format!(
                        "workspace:{workspace_index}/tab:{tab_index}/pane:{pane_index}"
                    )),
                );
            }
        }
    }

    let mut could_not_resume = Vec::new();
    let workspaces = doc
        .model
        .workspaces
        .iter()
        .map(|workspace| {
            let tabs = workspace
                .tabs
                .iter()
                .map(|tab| {
                    let panes = tab
                        .tree
                        .panes()
                        .into_iter()
                        .filter_map(|pane_id| {
                            let pane = doc.model.panes.get(&pane_id)?;
                            let slot = bindings.panes.get(&pane_id)?.clone();
                            let extras = doc.pane_extras.get(&pane_id).cloned().unwrap_or_default();
                            let (launch, resume_failed) = plan_launch(
                                pane_id,
                                &slot,
                                &extras,
                                &options,
                                &mut could_not_resume,
                            );
                            Some(RestorePane {
                                old_id: pane_id,
                                slot,
                                cwd: pane.cwd.clone(),
                                label: pane.label.clone(),
                                agent_kind: extras.agent_kind,
                                agent_session: extras.agent_session,
                                launch,
                                resume_failed,
                            })
                        })
                        .collect();
                    RestoreTab {
                        old_id: tab.id,
                        slot: bindings.tabs[&tab.id].clone(),
                        label: tab.label.clone(),
                        panes,
                        layout: map_layout(&tab.tree, &bindings.panes),
                        focused_pane: bindings.panes.get(&tab.focused_pane).cloned(),
                        zoomed_pane: tab
                            .zoomed
                            .and_then(|pane| bindings.panes.get(&pane).cloned()),
                    }
                })
                .collect();
            RestoreWorkspace {
                old_id: workspace.id,
                slot: bindings.workspaces[&workspace.id].clone(),
                cwd: workspace.cwd.clone(),
                label: workspace.label.clone(),
                tabs,
                focused_tab: bindings.tabs.get(&workspace.focused_tab).cloned(),
            }
        })
        .collect();

    RestorePlan {
        session: doc.session.clone(),
        workspaces,
        focus: RestoreFocus {
            workspace: bindings.workspaces.get(&doc.focused_workspace).cloned(),
            tab: bindings.tabs.get(&doc.focused_tab).cloned(),
            pane: bindings.panes.get(&doc.focused_pane).cloned(),
        },
        bindings,
        could_not_resume,
    }
}

fn plan_launch(
    pane_id: PaneId,
    slot: &SlotKey,
    extras: &PaneExtras,
    options: &RestoreOptions<'_>,
    failures: &mut Vec<ResumeFailure>,
) -> (RestoreLaunch, bool) {
    if options.resume_agents {
        if let Some(session) = &extras.agent_session {
            let agent = extras
                .agent_kind
                .as_deref()
                .unwrap_or(session.agent.as_str());
            if let Some(argv) = (options.resume_command)(agent, session)
                .filter(|argv| !argv.is_empty())
            {
                return (RestoreLaunch::Resume { argv }, false);
            }
            failures.push(ResumeFailure {
                old_pane_id: pane_id,
                slot: slot.clone(),
                agent: agent.to_owned(),
                session: session.clone(),
            });
            return (RestoreLaunch::Shell, true);
        }
    }

    if let Some(argv) = extras.recipe_argv.clone().filter(|argv| !argv.is_empty()) {
        return (RestoreLaunch::Recipe { argv }, false);
    }
    (RestoreLaunch::Shell, false)
}

fn map_layout(node: &Node, panes: &BTreeMap<PaneId, SlotKey>) -> RestoreLayout {
    match node {
        Node::Leaf(pane_id) => RestoreLayout::Pane {
            slot: panes[pane_id].clone(),
        },
        Node::Split {
            axis,
            ratio,
            first,
            second,
        } => RestoreLayout::Split {
            axis: *axis,
            ratio: *ratio,
            first: Box::new(map_layout(first, panes)),
            second: Box::new(map_layout(second, panes)),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use starcil_domain::SplitDirection;

    #[test]
    fn restore_plan_preserves_order_ratios_recipes_and_resume_fallbacks() {
        let mut terminal = 0;
        let mut terminal_ids = || {
            terminal += 1;
            format!("term_old_{terminal}")
        };
        let mut model = starcil_domain::SessionModel::new("C:/one", &mut terminal_ids);
        let first = model.workspaces[0].tabs[0].focused_pane;
        let recipe = model
            .split_pane(
                first,
                SplitDirection::Right,
                0.37,
                Some("C:/recipe"),
                BTreeMap::new(),
                &mut terminal_ids,
            )
            .unwrap();
        let (_, _, unavailable) = model.create_workspace(
            "C:/two",
            Some("Second".to_owned()),
            BTreeMap::new(),
            &mut terminal_ids,
        );

        let mut extras = BTreeMap::new();
        extras.insert(
            first,
            PaneExtras {
                agent_kind: Some("claude".to_owned()),
                agent_session: Some(session_ref("claude", "live-session")),
                recipe_argv: None,
            },
        );
        extras.insert(
            recipe,
            PaneExtras {
                recipe_argv: Some(vec!["dead-tool".to_owned(), "--restore".to_owned()]),
                ..PaneExtras::default()
            },
        );
        extras.insert(
            unavailable,
            PaneExtras {
                agent_kind: Some("codex".to_owned()),
                agent_session: Some(session_ref("codex", "missing-session")),
                recipe_argv: None,
            },
        );
        let mut doc = StateDoc::new("restore", model, extras).unwrap();
        doc.saved_at = 123;

        let resume = |agent: &str, session: &SessionRef| {
            (agent == "claude").then(|| {
                vec![
                    "claude".to_owned(),
                    "--resume".to_owned(),
                    session.value.clone(),
                ]
            })
        };
        let plan = plan_restore(
            &doc,
            RestoreOptions {
                resume_agents: true,
                resume_command: &resume,
            },
        );

        assert_eq!(plan.workspaces.len(), 2);
        assert_eq!(plan.workspaces[0].panes_in_order(), vec![first, recipe]);
        let first_tab = &plan.workspaces[0].tabs[0];
        assert!(matches!(
            first_tab.layout,
            RestoreLayout::Split { ratio, .. } if (ratio - 0.37).abs() < f32::EPSILON
        ));
        assert!(matches!(
            first_tab.panes[0].launch,
            RestoreLaunch::Resume { ref argv }
                if argv.iter().map(String::as_str).eq(["claude", "--resume", "live-session"])
        ));
        assert!(matches!(
            first_tab.panes[1].launch,
            RestoreLaunch::Recipe { ref argv }
                if argv.iter().map(String::as_str).eq(["dead-tool", "--restore"])
        ));
        let unavailable_pane = &plan.workspaces[1].tabs[0].panes[0];
        assert_eq!(unavailable_pane.old_id, unavailable);
        assert_eq!(unavailable_pane.launch, RestoreLaunch::Shell);
        assert!(unavailable_pane.resume_failed);
        assert_eq!(plan.could_not_resume.len(), 1);
        assert_eq!(plan.could_not_resume[0].old_pane_id, unavailable);
        assert!(plan.focus.workspace.is_some());
        assert!(plan.focus.tab.is_some());
        assert!(plan.focus.pane.is_some());
    }

    impl RestoreWorkspace {
        fn panes_in_order(&self) -> Vec<PaneId> {
            self.tabs
                .iter()
                .flat_map(|tab| tab.panes.iter().map(|pane| pane.old_id))
                .collect()
        }
    }

    fn session_ref(agent: &str, value: &str) -> SessionRef {
        SessionRef {
            source: "starcil:integration".to_owned(),
            agent: agent.to_owned(),
            kind: "id".to_owned(),
            value: value.to_owned(),
        }
    }
}
