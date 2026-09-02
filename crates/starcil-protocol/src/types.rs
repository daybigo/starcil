//! Public record shapes returned by the API: panes, agents, workspaces, tabs,
//! layout snapshots, scroll metrics, session snapshot. Field names are part of
//! the wire contract — do not rename casually. Unknown fields must be ignored
//! by clients (serde default behavior).

use serde::{Deserialize, Serialize};
use starcil_domain::AgentStatus;
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScrollMetrics {
    pub offset_from_bottom: u64,
    pub max_offset_from_bottom: u64,
    pub viewport_rows: u16,
}

/// Native agent session reference stored from integration reports.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentSessionRef {
    pub source: String,
    pub agent: String,
    /// "id" or "path".
    pub kind: String,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PaneInfo {
    pub pane_id: String,
    pub terminal_id: String,
    pub workspace_id: String,
    pub tab_id: String,
    pub focused: bool,
    pub cwd: String,
    pub agent_status: AgentStatus,
    pub revision: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub terminal_title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub terminal_title_stripped: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub foreground_cwd: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_session: Option<AgentSessionRef>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scroll: Option<ScrollMetrics>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub tokens: BTreeMap<String, String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state_change_seq: Option<u64>,
    /// Whether the pane's shell sits at its prompt (`true`) or runs a
    /// program (`false`), from the host's process tree; absent when the host
    /// cannot tell. The TUI's composer holds the keyboard only at the prompt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shell_idle: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentInfo {
    /// Canonical agent kind label ("claude", "codex", ...).
    pub agent: String,
    pub agent_status: AgentStatus,
    pub pane_id: String,
    pub terminal_id: String,
    pub workspace_id: String,
    pub tab_id: String,
    pub cwd: String,
    pub focused: bool,
    pub revision: u64,
    pub state_change_seq: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub terminal_title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub terminal_title_stripped: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_session: Option<AgentSessionRef>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub foreground_cwd: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub tokens: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorktreeProvenance {
    pub parent_workspace_id: String,
    pub branch: String,
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkspaceInfo {
    pub workspace_id: String,
    pub label: String,
    pub cwd: String,
    pub focused: bool,
    pub revision: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tabs: Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub tokens: BTreeMap<String, String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub worktree: Option<WorktreeProvenance>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TabInfo {
    pub tab_id: String,
    pub workspace_id: String,
    pub label: String,
    pub focused: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub panes: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub zoomed: Option<String>,
}

/// One pane rectangle inside a tab layout snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneRect {
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PaneLayoutEntry {
    pub pane_id: String,
    pub rect: PaneRect,
    pub focused: bool,
}

/// The layout snapshot included by pane.layout / pane.neighbor / pane.edges
/// and pushed by layout.updated.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PaneLayoutSnapshot {
    pub workspace_id: String,
    pub tab_id: String,
    pub area: PaneRect,
    pub focused_pane_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub zoomed: Option<String>,
    pub panes: Vec<PaneLayoutEntry>,
}

/// Portable layout tree for layout.export / layout.apply.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PortableLayoutNode {
    Pane {
        #[serde(skip_serializing_if = "Option::is_none")]
        pane_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        label: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        cwd: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        command: Option<Vec<String>>,
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        env: BTreeMap<String, String>,
    },
    Split {
        /// "right" or "down".
        direction: String,
        ratio: f32,
        first: Box<PortableLayoutNode>,
        second: Box<PortableLayoutNode>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionSnapshot {
    pub version: String,
    pub protocol_major: u32,
    pub protocol_minor: u32,
    pub session: String,
    pub revision: u64,
    pub focused_workspace_id: String,
    pub focused_tab_id: String,
    pub focused_pane_id: String,
    pub workspaces: Vec<WorkspaceInfo>,
    pub tabs: Vec<TabInfo>,
    pub panes: Vec<PaneInfo>,
    pub layouts: Vec<PaneLayoutSnapshot>,
    pub agents: Vec<AgentInfo>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorktreeInfo {
    pub path: String,
    pub branch: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    pub is_primary: bool,
}

/// Foreground process info for pane.process_info.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProcessInfo {
    pub shell_pid: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub foreground_pgid: Option<u32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub foreground: Vec<ForegroundProcess>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ForegroundProcess {
    pub pid: u32,
    pub name: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub argv: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pane_info_matches_observed_shape() {
        let p = PaneInfo {
            pane_id: "w1:p1".into(),
            terminal_id: "term_abc123".into(),
            workspace_id: "w1".into(),
            tab_id: "w1:t1".into(),
            focused: true,
            cwd: "C:/dev".into(),
            agent_status: AgentStatus::Working,
            revision: 42,
            label: None,
            agent: None,
            agent_name: None,
            terminal_title: None,
            terminal_title_stripped: None,
            foreground_cwd: None,
            agent_session: None,
            scroll: Some(ScrollMetrics { offset_from_bottom: 0, max_offset_from_bottom: 240, viewport_rows: 30 }),
            tokens: BTreeMap::new(),
            state_change_seq: None,
            shell_idle: None,
        };
        let v = serde_json::to_value(&p).unwrap();
        assert_eq!(v["pane_id"], "w1:p1");
        assert_eq!(v["agent_status"], "working");
        assert_eq!(v["scroll"]["offset_from_bottom"], 0);
        assert!(v.get("label").is_none(), "None fields stay off the wire");
    }

    #[test]
    fn portable_layout_roundtrip() {
        let json = serde_json::json!({
            "type": "split",
            "direction": "right",
            "ratio": 0.65,
            "first": {"type": "pane", "label": "editor", "cwd": "/repo"},
            "second": {"type": "pane", "command": ["sh", "-c", "just test"]}
        });
        let node: PortableLayoutNode = serde_json::from_value(json.clone()).unwrap();
        let back = serde_json::to_value(&node).unwrap();
        assert_eq!(back["type"], "split");
        assert_eq!(back["first"]["type"], "pane");
        assert_eq!(back["second"]["command"][0], "sh");
    }

    #[test]
    fn clients_ignore_unknown_fields() {
        let json = r#"{"pane_id":"w1:p1","terminal_id":"t","workspace_id":"w1","tab_id":"w1:t1",
            "focused":false,"cwd":"/","agent_status":"idle","revision":1,"brand_new_field":123}"#;
        let p: PaneInfo = serde_json::from_str(json).unwrap();
        assert_eq!(p.pane_id, "w1:p1");
    }
}
