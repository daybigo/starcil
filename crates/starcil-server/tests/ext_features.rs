//! dispatch_ext features: reorders, layout.apply, worktrees over a real
//! temporary git repository.

use serde_json::json;
use starcil_server::ServerCore;
use starcil_testkit::FakeHost;
use std::process::Command;

fn core_at(cwd: &str) -> ServerCore<FakeHost> {
    ServerCore::new("default", cwd, FakeHost::new()).expect("core boots")
}

#[test]
fn workspace_reorder_and_block_move() {
    let mut c = core_at("C:/dev/proj");
    c.handle("workspace.create", &json!({"cwd": "C:/b"})).unwrap();
    c.handle("workspace.create", &json!({"cwd": "C:/c"})).unwrap();
    c.handle("workspace.create", &json!({"cwd": "C:/d"})).unwrap();
    // Order now w1 w2 w3 w4.
    let r = c.handle("workspace.move", &json!({"workspace_id": "w4", "insert_index": 0})).unwrap();
    assert_eq!(r["workspaces"][0], "w4");
    // Move [w2, w3] before w4 (index 0).
    let r = c
        .handle(
            "workspace.move_block",
            &json!({"workspace_ids": ["w2", "w3"], "before_workspace_id": "w4"}),
        )
        .unwrap();
    assert_eq!(
        r["workspaces"].as_array().unwrap().iter().map(|v| v.as_str().unwrap()).collect::<Vec<_>>(),
        vec!["w2", "w3", "w4", "w1"]
    );
    // Anchor inside the block is refused.
    let e = c
        .handle(
            "workspace.move_block",
            &json!({"workspace_ids": ["w2"], "before_workspace_id": "w2"}),
        )
        .unwrap_err();
    assert!(e.message.contains("anchor"));
}

#[test]
fn tab_reorder() {
    let mut c = core_at("C:/dev/proj");
    c.handle("tab.create", &json!({"label": "b"})).unwrap();
    c.handle("tab.create", &json!({"label": "c"})).unwrap();
    let r = c.handle("tab.move", &json!({"tab_id": "w1:t3", "insert_index": 0})).unwrap();
    assert_eq!(r["tabs"][0], "w1:t3");
}

#[test]
fn layout_apply_builds_tab_with_commands() {
    let mut c = core_at("C:/dev/proj");
    let r = c
        .handle(
            "layout.apply",
            &json!({
                "tab_label": "dev",
                "focus": true,
                "root": {
                    "type": "split",
                    "direction": "right",
                    "ratio": 0.65,
                    "first": {"type": "pane", "label": "editor", "cwd": "C:/repo"},
                    "second": {"type": "pane", "cwd": "C:/repo", "command": ["cmd", "/c", "echo hi"], "env": {"ROLE": "tests"}}
                }
            }),
        )
        .unwrap();
    assert_eq!(r["type"], "layout_applied");
    assert_eq!(r["tab"]["label"], "dev");
    let panes = r["tab"]["panes"].as_array().unwrap();
    assert_eq!(panes.len(), 2);
    // Second pane's terminal carries the argv command and env.
    let p2: starcil_domain::PaneId = panes[1].as_str().unwrap().parse().unwrap();
    let term = c.model.pane(p2).unwrap().terminal_id.clone();
    let t = c.host.terminal(&term);
    assert_eq!(t.env.get("ROLE").map(String::as_str), Some("tests"));
    assert_eq!(t.spec_cwd, "C:/repo");
    // Replace flow: applying with tab_id closes the old tab.
    let old_tab = r["tab"]["tab_id"].as_str().unwrap().to_string();
    let r2 = c
        .handle(
            "layout.apply",
            &json!({"tab_id": old_tab, "root": {"type": "pane", "cwd": "C:/repo"}}),
        )
        .unwrap();
    assert_ne!(r2["tab"]["tab_id"], old_tab);
    let tabs = c.handle("tab.list", &json!({})).unwrap();
    let listed: Vec<String> = tabs["tabs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["tab_id"].as_str().unwrap().to_string())
        .collect();
    assert!(!listed.contains(&old_tab), "replaced tab must be closed");
}

fn git(dir: &std::path::Path, args: &[&str]) {
    let out = Command::new("git").current_dir(dir).args(args).output().expect("git runs");
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
}

#[test]
fn worktree_lifecycle_against_real_repo() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "--allow-empty", "-qm", "init"]);

    let mut c = core_at(repo.to_str().unwrap());
    c.worktrees_dir = tmp.path().join("wts");

    // Create a worktree: new workspace with provenance.
    let r = c
        .handle("worktree.create", &json!({"workspace_id": "w1", "branch": "feature/x", "focus": false}))
        .unwrap();
    assert_eq!(r["type"], "worktree_created");
    let ws_id = r["workspace"]["workspace_id"].as_str().unwrap().to_string();
    assert_eq!(r["workspace"]["worktree"]["parent_workspace_id"], "w1");
    assert_eq!(r["workspace"]["worktree"]["branch"], "feature/x");

    // List shows primary + the new one, with workspace linkage.
    let r = c.handle("worktree.list", &json!({"workspace_id": "w1"})).unwrap();
    let wts = r["worktrees"].as_array().unwrap();
    assert_eq!(wts.len(), 2);
    assert!(wts.iter().any(|w| w["branch"] == "feature/x" && w["workspace_id"] == ws_id.as_str()));

    // Open is idempotent: returns already_open.
    let r = c
        .handle("worktree.open", &json!({"workspace_id": "w1", "branch": "feature/x"}))
        .unwrap();
    assert_eq!(r["already_open"], true);

    // Remove: git worktree gone, workspace closed, branch preserved.
    c.handle("worktree.remove", &json!({"workspace_id": ws_id, "force": false})).unwrap();
    let r = c.handle("worktree.list", &json!({"workspace_id": "w1"})).unwrap();
    assert_eq!(r["worktrees"].as_array().unwrap().len(), 1);
    let out = Command::new("git")
        .current_dir(&repo)
        .args(["branch", "--list", "feature/x"])
        .output()
        .unwrap();
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("feature/x"),
        "branch must never be deleted by worktree.remove"
    );
    let r = c.handle("workspace.list", &json!({})).unwrap();
    assert_eq!(r["workspaces"].as_array().unwrap().len(), 1);
}
