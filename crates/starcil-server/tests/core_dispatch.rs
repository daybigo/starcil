//! ServerCore dispatch tests over the fake terminal host — no PTY involved.

use serde_json::{json, Value};
use starcil_protocol::methods;
use starcil_server::dispatch::STUBBED;
use starcil_server::hosttraits::TerminalHost;
use starcil_server::ServerCore;
use starcil_testkit::{FakeHost, FakeWrite};

fn core() -> ServerCore<FakeHost> {
    ServerCore::new("default", "C:/dev/proj", FakeHost::new()).expect("core boots")
}

#[test]
fn boot_creates_one_workspace_tab_pane_with_env() {
    let c = core();
    assert_eq!(c.model.workspaces.len(), 1);
    let term = c.model.pane("w1:p1".parse().unwrap()).unwrap().terminal_id.clone();
    let t = c.host.terminal(&term);
    assert_eq!(t.env.get("STARCIL_ENV").map(String::as_str), Some("1"));
    assert_eq!(t.env.get("STARCIL_PANE_ID").map(String::as_str), Some("w1:p1"));
    assert_eq!(t.env.get("STARCIL_WORKSPACE_ID").map(String::as_str), Some("w1"));
    assert_eq!(t.spec_cwd, "C:/dev/proj");
}

#[test]
fn ping_pongs_with_versions() {
    let mut c = core();
    let r = c.handle("ping", &json!({})).unwrap();
    assert_eq!(r["type"], "pong");
    assert_eq!(r["protocol_major"], 1);
    assert_eq!(r["session"], "default");
}

#[test]
fn every_cataloged_method_is_routed() {
    let mut c = core();
    for m in methods::ALL {
        let out = c.handle(m, &json!({}));
        if let Err(e) = &out {
            assert_ne!(
                serde_json::to_value(&e.code).unwrap(),
                json!("unknown_method"),
                "method {m} fell through the dispatcher"
            );
        }
    }
    // And a truly unknown one is rejected properly.
    let err = c.handle("pane.explode", &json!({})).unwrap_err();
    assert_eq!(serde_json::to_value(&err.code).unwrap(), json!("unknown_method"));
}

#[test]
fn stub_list_matches_reality() {
    let mut c = core();
    for m in methods::ALL {
        let out = c.handle(m, &json!({}));
        let is_stub = matches!(&out, Err(e) if e.message.starts_with("not implemented yet"));
        assert_eq!(
            is_stub,
            STUBBED.contains(m),
            "method {m}: stub reality ({is_stub}) diverges from STUBBED list"
        );
    }
}

#[test]
fn pane_focus_by_id_moves_session_focus_and_emits_pane_focused() {
    let mut c = core();
    c.handle("pane.split", &json!({"pane_id": "w1:p1", "direction": "right"}))
        .unwrap();
    c.pending_events.clear();
    let r = c.handle("pane.focus", &json!({"pane_id": "w1:p2"})).unwrap();
    assert_eq!(r["type"], "pane_info");
    assert_eq!(r["pane"]["pane_id"], "w1:p2");
    let tab = c.model.tab("w1:t1".parse().unwrap()).unwrap();
    assert_eq!(tab.focused_pane.to_string(), "w1:p2");
    assert!(
        c.pending_events
            .iter()
            .any(|(name, data)| name == "pane.focused" && data["pane_id"] == "w1:p2"),
        "pane.focus must emit the structural pane.focused event"
    );
    let err = c.handle("pane.focus", &json!({})).unwrap_err();
    assert_eq!(
        serde_json::to_value(&err.code).unwrap(),
        json!("invalid_params")
    );
}

#[test]
fn structural_mutations_resync_pty_sizes_immediately() {
    let mut c = core();
    let t1 = c.model.pane("w1:p1".parse().unwrap()).unwrap().terminal_id.clone();
    c.handle("pane.split", &json!({"pane_id": "w1:p1", "direction": "right"}))
        .unwrap();
    let t2 = c.model.pane("w1:p2".parse().unwrap()).unwrap().terminal_id.clone();
    let full_width = c.client_area.width;
    let after_split_1 = c.host.terminal(&t1).size;
    let after_split_2 = c.host.terminal(&t2).size;
    assert!(
        after_split_1.0 < full_width && after_split_2.0 < full_width,
        "both PTYs must shrink to their halves right after the split, got {after_split_1:?} / {after_split_2:?}"
    );

    c.handle(
        "pane.resize",
        &json!({"pane_id": "w1:p1", "direction": "right", "amount": 0.2}),
    )
    .unwrap();
    let after_resize_1 = c.host.terminal(&t1).size;
    let after_resize_2 = c.host.terminal(&t2).size;
    assert!(
        after_resize_1.0 > after_split_1.0,
        "the grown pane's PTY must widen immediately ({:?} -> {:?})",
        after_split_1,
        after_resize_1
    );
    assert!(
        after_resize_2.0 < after_split_2.0,
        "the shrunk pane's PTY must narrow immediately ({:?} -> {:?})",
        after_split_2,
        after_resize_2
    );

    c.handle("pane.close", &json!({"pane_id": "w1:p2"})).unwrap();
    let after_close = c.host.terminal(&t1).size;
    assert!(
        after_close.0 > after_resize_1.0,
        "closing the sibling must hand its columns back immediately ({:?} -> {:?})",
        after_resize_1,
        after_close
    );
}

#[test]
fn split_creates_pane_with_inherited_cwd_and_env_ids() {
    let mut c = core();
    let r = c
        .handle("pane.split", &json!({"pane_id": "w1:p1", "direction": "right", "ratio": 0.6}))
        .unwrap();
    assert_eq!(r["type"], "pane_info");
    assert_eq!(r["pane"]["pane_id"], "w1:p2");
    assert_eq!(r["pane"]["cwd"], "C:/dev/proj");
    let term = r["pane"]["terminal_id"].as_str().unwrap();
    let t = c.host.terminal(term);
    assert_eq!(t.env.get("STARCIL_PANE_ID").map(String::as_str), Some("w1:p2"));
}

#[test]
fn split_rejects_bad_ratio_and_direction() {
    let mut c = core();
    let e = c
        .handle("pane.split", &json!({"pane_id": "w1:p1", "direction": "left"}))
        .unwrap_err();
    assert!(e.message.contains("right|down"));
    let e = c
        .handle("pane.split", &json!({"pane_id": "w1:p1", "direction": "right", "ratio": 3.0}))
        .unwrap_err();
    assert!(e.message.contains("ratio"));
}

#[test]
fn run_sends_text_and_enter_separately() {
    let mut c = core();
    c.handle("pane.run", &json!({"pane_id": "w1:p1", "command": "echo hi"})).unwrap();
    let term = c.model.pane("w1:p1".parse().unwrap()).unwrap().terminal_id.clone();
    let writes = &c.host.terminal(&term).writes;
    assert_eq!(
        writes,
        &vec![FakeWrite::Text("echo hi".into()), FakeWrite::Enter],
        "text and Enter must be separate PTY writes"
    );
}

#[test]
fn read_returns_screen_tail() {
    let mut c = core();
    let term = c.model.pane("w1:p1".parse().unwrap()).unwrap().terminal_id.clone();
    c.host.set_screen(&term, "line1\nline2\nline3");
    let r = c
        .handle("pane.read", &json!({"pane_id": "w1:p1", "source": "visible", "lines": 2}))
        .unwrap();
    assert_eq!(r["text"], "line2\nline3");
    let e = c
        .handle("pane.read", &json!({"pane_id": "w1:p1", "source": "wat"}))
        .unwrap_err();
    assert!(e.message.contains("invalid source"));
}

#[test]
fn zoom_swap_and_reasons() {
    let mut c = core();
    // Single pane: zoom refuses with single_pane.
    let r = c.handle("pane.zoom", &json!({"pane_id": "w1:p1"})).unwrap();
    assert_eq!(r["changed"], false);
    assert_eq!(r["reason"], "single_pane");
    // Split, then zoom toggles on and off.
    c.handle("pane.split", &json!({"pane_id": "w1:p1", "direction": "right"})).unwrap();
    let r = c.handle("pane.zoom", &json!({"pane_id": "w1:p1"})).unwrap();
    assert_eq!(r["changed"], true);
    assert_eq!(r["zoomed"], "w1:p1");
    let r = c.handle("pane.zoom", &json!({"pane_id": "w1:p1", "mode": "on"})).unwrap();
    assert_eq!(r["reason"], "already_zoomed");
    let r = c.handle("pane.zoom", &json!({"pane_id": "w1:p1", "mode": "off"})).unwrap();
    assert_eq!(r["changed"], true);
    // Swap right moves p1 into p2's slot.
    let r = c.handle("pane.swap", &json!({"pane_id": "w1:p1", "direction": "right"})).unwrap();
    assert_eq!(r["changed"], true);
    assert_eq!(r["target_pane_id"], "w1:p2");
    // No neighbor to the far right now that p1 sits there.
    let r = c.handle("pane.swap", &json!({"pane_id": "w1:p1", "direction": "right"})).unwrap();
    assert_eq!(r["changed"], false);
    assert_eq!(r["reason"], "no_neighbor");
}

#[test]
fn focus_direction_and_neighbor() {
    let mut c = core();
    c.handle("pane.split", &json!({"pane_id": "w1:p1", "direction": "right"})).unwrap();
    let r = c
        .handle("pane.neighbor", &json!({"pane_id": "w1:p1", "direction": "right"}))
        .unwrap();
    assert_eq!(r["neighbor"], "w1:p2");
    let r = c
        .handle("pane.focus_direction", &json!({"caller_pane_id": "w1:p1", "direction": "right"}))
        .unwrap();
    assert_eq!(r["pane"]["pane_id"], "w1:p2");
    assert_eq!(r["pane"]["focused"], true);
}

#[test]
fn close_pane_kills_terminal_and_collapses() {
    let mut c = core();
    c.handle("pane.split", &json!({"pane_id": "w1:p1", "direction": "down"})).unwrap();
    let term2 = c.model.pane("w1:p2".parse().unwrap()).unwrap().terminal_id.clone();
    let r = c.handle("pane.close", &json!({"pane_id": "w1:p2"})).unwrap();
    assert_eq!(r["type"], "pane_closed");
    assert!(!c.host.is_alive(&term2), "terminal must be killed");
    // Closing the last pane is refused.
    let e = c.handle("pane.close", &json!({"pane_id": "w1:p1"})).unwrap_err();
    assert!(e.message.contains("last pane"));
}

#[test]
fn workspace_and_tab_lifecycle() {
    let mut c = core();
    let r = c
        .handle("workspace.create", &json!({"cwd": "C:/two", "label": "api", "focus": true}))
        .unwrap();
    assert_eq!(r["workspace"]["workspace_id"], "w2");
    assert_eq!(r["workspace"]["label"], "api");
    assert_eq!(r["root_pane"]["pane_id"], "w2:p1");
    assert_eq!(c.model.focused_workspace.to_string(), "w2");

    let r = c.handle("tab.create", &json!({"workspace_id": "w2", "label": "logs"})).unwrap();
    assert_eq!(r["tab"]["tab_id"], "w2:t2");
    let r = c.handle("tab.list", &json!({"workspace_id": "w2"})).unwrap();
    assert_eq!(r["tabs"].as_array().unwrap().len(), 2);

    let r = c.handle("workspace.rename", &json!({"workspace_id": "w2", "label": "renamed"})).unwrap();
    assert_eq!(r["workspace"]["label"], "renamed");

    c.handle("workspace.close", &json!({"workspace_id": "w2"})).unwrap();
    let r = c.handle("workspace.list", &json!({})).unwrap();
    assert_eq!(r["workspaces"].as_array().unwrap().len(), 1);
    // Cannot close the last one.
    let e = c.handle("workspace.close", &json!({"workspace_id": "w1"})).unwrap_err();
    assert!(e.message.contains("last workspace"));
}

#[test]
fn pane_move_to_new_workspace_keeps_terminal() {
    let mut c = core();
    c.handle("pane.split", &json!({"pane_id": "w1:p1", "direction": "right"})).unwrap();
    let term_before = c.model.pane("w1:p2".parse().unwrap()).unwrap().terminal_id.clone();
    let r = c
        .handle(
            "pane.move",
            &json!({"pane_id": "w1:p2", "destination": {"type": "new_workspace", "label": "solo"}, "focus": true}),
        )
        .unwrap();
    assert_eq!(r["changed"], true);
    assert_eq!(r["previous_pane_id"], "w1:p2");
    let new_id = r["pane"]["pane_id"].as_str().unwrap().to_string();
    assert!(new_id.starts_with("w2:"), "cross-workspace move re-ids, got {new_id}");
    assert_eq!(r["pane"]["terminal_id"], Value::String(term_before.clone()));
    assert!(c.host.is_alive(&term_before), "moved pane keeps its live terminal");
}

#[test]
fn layout_export_and_ratio() {
    let mut c = core();
    c.handle("pane.split", &json!({"pane_id": "w1:p1", "direction": "right", "ratio": 0.5})).unwrap();
    let r = c.handle("layout.export", &json!({})).unwrap();
    assert_eq!(r["root"]["type"], "split");
    assert_eq!(r["root"]["direction"], "right");
    let r = c
        .handle("layout.set_split_ratio", &json!({"tab_id": "w1:t1", "path": [], "ratio": 0.7}))
        .unwrap();
    assert_eq!(r["type"], "layout_split_ratio_set");
    let r = c.handle("layout.export", &json!({})).unwrap();
    let ratio = r["root"]["ratio"].as_f64().unwrap();
    assert!((ratio - 0.7).abs() < 1e-6);
}

#[test]
fn session_snapshot_is_complete() {
    let mut c = core();
    c.handle("pane.split", &json!({"pane_id": "w1:p1", "direction": "right"})).unwrap();
    c.handle("workspace.create", &json!({"cwd": "C:/two"})).unwrap();
    let r = c.handle("session.snapshot", &json!({})).unwrap();
    assert_eq!(r["type"], "session_snapshot");
    assert_eq!(r["workspaces"].as_array().unwrap().len(), 2);
    assert_eq!(r["panes"].as_array().unwrap().len(), 3);
    assert_eq!(r["layouts"].as_array().unwrap().len(), 2);
    assert_eq!(r["focused_workspace_id"], "w1");
}

#[test]
fn send_keys_validates() {
    let mut c = core();
    let e = c
        .handle("pane.send_keys", &json!({"pane_id": "w1:p1", "keys": ["bad key"]}))
        .unwrap_err();
    assert!(e.message.contains("invalid key"));
    c.handle("pane.send_keys", &json!({"pane_id": "w1:p1", "keys": ["esc", "ctrl+c"]}))
        .unwrap();
    let e = c.handle("pane.send_keys", &json!({"pane_id": "w1:p1", "keys": []})).unwrap_err();
    assert!(e.message.contains("not be empty"));
}

#[test]
fn pty_sizes_match_the_frame_rule_lone_pane_unframed_split_panes_framed() {
    let mut c = core();
    c.client_area = starcil_domain::Rect { x: 0, y: 0, width: 100, height: 30 };
    c.pane_borders = true;
    c.pane_gap = 0;
    c.sync_pty_sizes();
    let t1 = c.model.pane("w1:p1".parse().unwrap()).unwrap().terminal_id.clone();
    assert_eq!(
        c.host.terminal(&t1).size,
        (100, 30),
        "a lone pane is drawn without a frame, so its PTY gets the whole layout area"
    );

    c.handle("pane.split", &json!({"pane_id": "w1:p1", "direction": "right", "ratio": 0.5}))
        .unwrap();
    let t2 = c.model.pane("w1:p2".parse().unwrap()).unwrap().terminal_id.clone();
    let (c1, r1) = c.host.terminal(&t1).size;
    let (c2, r2) = c.host.terminal(&t2).size;
    assert_eq!((r1, r2), (28, 28), "framed panes lose one row top and bottom");
    assert_eq!(c1 + c2, 100 - 4, "each framed pane loses one column per side");

    c.pane_borders = false;
    c.sync_pty_sizes();
    let (c1, r1) = c.host.terminal(&t1).size;
    let (c2, r2) = c.host.terminal(&t2).size;
    assert_eq!((r1, r2), (30, 30));
    assert_eq!(c1 + c2, 100, "no frames, no lost cells");
}

#[test]
fn reserved_rows_shrink_only_that_panes_pty_until_cleared() {
    let mut c = core();
    c.client_area = starcil_domain::Rect { x: 0, y: 0, width: 100, height: 30 };
    c.pane_borders = false;
    c.sync_pty_sizes();
    let p1: starcil_domain::PaneId = "w1:p1".parse().unwrap();
    let t1 = c.model.pane(p1).unwrap().terminal_id.clone();
    assert_eq!(c.host.terminal(&t1).size, (100, 30));

    // The in-pane composer cedes 6 bottom rows.
    c.reserved_rows.insert(p1, 6);
    c.sync_pty_sizes();
    assert_eq!(c.host.terminal(&t1).size, (100, 24));

    // Structural changes keep honoring the reservation.
    c.handle("pane.split", &json!({"pane_id": "w1:p1", "direction": "right", "ratio": 0.5}))
        .unwrap();
    let t2 = c.model.pane("w1:p2".parse().unwrap()).unwrap().terminal_id.clone();
    assert_eq!(c.host.terminal(&t1).size.1, 24, "reserved pane stays short");
    assert_eq!(c.host.terminal(&t2).size.1, 30, "sibling keeps every row");

    // Clearing restores the full height.
    c.reserved_rows.remove(&p1);
    c.sync_pty_sizes();
    assert_eq!(c.host.terminal(&t1).size.1, 30);
}
