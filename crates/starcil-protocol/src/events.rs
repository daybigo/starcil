//! Pushed event frames and the catalog of event names clients/plugins can
//! subscribe to. An event line is `{"event":"<name>","data":{...}}` plus
//! optional `revision` for state-bearing events.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventFrame {
    pub event: String,
    #[serde(default)]
    pub data: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub revision: Option<u64>,
}

pub const ALL: &[&str] = &[
    // workspace
    "workspace.created",
    "workspace.updated",
    "workspace.metadata_updated",
    "workspace.renamed",
    "workspace.moved",
    "workspace.reordered",
    "workspace.closed",
    "workspace.focused",
    // tab
    "tab.created",
    "tab.closed",
    "tab.focused",
    "tab.renamed",
    "tab.moved",
    // pane
    "pane.created",
    "pane.updated",
    "pane.closed",
    "pane.focused",
    "pane.moved",
    "pane.exited",
    "pane.agent_detected",
    "pane.agent_exited",
    "pane.shell_idle_changed",
    "pane.cwd_changed",
    "pane.output_matched",
    "pane.agent_status_changed",
    "pane.scroll_changed",
    // layout
    "layout.updated",
    // worktree
    "worktree.created",
    "worktree.opened",
    "worktree.removed",
];

pub fn is_known(event: &str) -> bool {
    ALL.contains(&event)
}

/// One subscription filter as accepted by events.subscribe.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Subscription {
    #[serde(rename = "type")]
    pub event_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pane_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tab_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_status: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_frame_roundtrip() {
        let f = EventFrame {
            event: "pane.agent_status_changed".into(),
            data: serde_json::json!({"pane_id":"w1:p1","agent_status":"blocked"}),
            revision: Some(7),
        };
        let s = serde_json::to_string(&f).unwrap();
        let back: EventFrame = serde_json::from_str(&s).unwrap();
        assert_eq!(back.event, "pane.agent_status_changed");
        assert_eq!(back.revision, Some(7));
    }

    #[test]
    fn catalog_sane() {
        assert!(is_known("worktree.created"));
        assert!(is_known("layout.updated"));
        assert!(!is_known("pane.win_lottery"));
        let mut seen = std::collections::BTreeSet::new();
        for e in ALL {
            assert!(seen.insert(*e), "duplicate event {e}");
        }
    }
}
