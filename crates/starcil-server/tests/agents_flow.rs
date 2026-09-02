//! Agent lifecycle flows through the dispatcher over the fake host.

use serde_json::json;
use starcil_server::ServerCore;
use starcil_testkit::{FakeHost, FakeWrite};

fn core() -> ServerCore<FakeHost> {
    ServerCore::new("default", "C:/dev/proj", FakeHost::new()).expect("core boots")
}

fn term_of(c: &ServerCore<FakeHost>, pane: &str) -> String {
    c.model.pane(pane.parse().unwrap()).unwrap().terminal_id.clone()
}

#[test]
fn report_agent_then_list_and_get() {
    let mut c = core();
    let r = c
        .handle(
            "pane.report_agent",
            &json!({"pane_id": "w1:p1", "source": "starcil:claude", "agent": "claude", "state": "working", "message": "building"}),
        )
        .unwrap();
    assert_eq!(r["accepted"], true);
    let r = c.handle("agent.list", &json!({})).unwrap();
    let agents = r["agents"].as_array().unwrap();
    assert_eq!(agents.len(), 1);
    assert_eq!(agents[0]["agent"], "claude");
    assert_eq!(agents[0]["agent_status"], "working");
    let r = c.handle("agent.get", &json!({"target": "w1:p1"})).unwrap();
    assert_eq!(r["agent"]["agent_status"], "working");
}

#[test]
fn agent_name_targeting_and_rename_rules() {
    let mut c = core();
    c.handle(
        "pane.report_agent",
        &json!({"pane_id": "w1:p1", "source": "starcil:codex", "agent": "codex", "state": "idle"}),
    )
    .unwrap();
    let r = c
        .handle("agent.rename", &json!({"target": "w1:p1", "name": "reviewer"}))
        .unwrap();
    assert_eq!(r["agent"]["name"], "reviewer");
    // Name now resolves as a target.
    let r = c.handle("agent.get", &json!({"target": "reviewer"})).unwrap();
    assert_eq!(r["agent"]["pane_id"], "w1:p1");
    // Invalid and duplicate names rejected.
    let e = c
        .handle("agent.rename", &json!({"target": "reviewer", "name": "Bad Name"}))
        .unwrap_err();
    assert!(e.message.contains("[a-z]"));
    c.handle("pane.split", &json!({"pane_id": "w1:p1", "direction": "right"})).unwrap();
    c.handle(
        "pane.report_agent",
        &json!({"pane_id": "w1:p2", "source": "starcil:claude", "agent": "claude", "state": "idle"}),
    )
    .unwrap();
    let e = c
        .handle("agent.rename", &json!({"target": "w1:p2", "name": "reviewer"}))
        .unwrap_err();
    assert!(e.message.contains("already used"));
}

#[test]
fn prompt_pastes_then_enters() {
    let mut c = core();
    c.handle(
        "pane.report_agent",
        &json!({"pane_id": "w1:p1", "source": "starcil:claude", "agent": "claude", "state": "idle"}),
    )
    .unwrap();
    c.handle("agent.prompt", &json!({"target": "w1:p1", "text": "review the diff"})).unwrap();
    let term = term_of(&c, "w1:p1");
    let writes = &c.host.terminal(&term).writes;
    assert_eq!(
        writes,
        &vec![FakeWrite::Paste("review the diff".into()), FakeWrite::Enter],
        "prompt must paste (bracket-aware) then Enter separately"
    );
}

#[test]
fn blocked_report_beats_stale_and_release_falls_back() {
    let mut c = core();
    c.handle(
        "pane.report_agent",
        &json!({"pane_id": "w1:p1", "source": "starcil:claude", "agent": "claude", "state": "working", "seq": 1}),
    )
    .unwrap();
    // Stale seq is accepted by the API but ignored by state.
    let r = c
        .handle(
            "pane.report_agent",
            &json!({"pane_id": "w1:p1", "source": "starcil:claude", "agent": "claude", "state": "idle", "seq": 1}),
        )
        .unwrap();
    assert_eq!(r["accepted"], false);
    let r = c.handle("agent.get", &json!({"target": "w1:p1"})).unwrap();
    assert_eq!(r["agent"]["agent_status"], "working");
    // Fresh blocked report wins.
    c.handle(
        "pane.report_agent",
        &json!({"pane_id": "w1:p1", "source": "starcil:claude", "agent": "claude", "state": "blocked", "seq": 2}),
    )
    .unwrap();
    let r = c.handle("agent.get", &json!({"target": "w1:p1"})).unwrap();
    assert_eq!(r["agent"]["agent_status"], "blocked");
    // Releasing the source drops integration authority.
    c.handle(
        "pane.release_agent",
        &json!({"pane_id": "w1:p1", "source": "starcil:claude"}),
    )
    .unwrap();
    let r = c.handle("agent.get", &json!({"target": "w1:p1"})).unwrap();
    assert_ne!(r["agent"]["agent_status"], "blocked");
}

#[test]
fn session_ref_stored_and_exposed() {
    let mut c = core();
    c.handle(
        "pane.report_agent_session",
        &json!({"pane_id": "w1:p1", "source": "starcil:codex", "agent": "codex", "agent_session_id": "abc-123"}),
    )
    .unwrap();
    // Session-only reports do not create lifecycle authority, but once the
    // pane hosts an agent the ref must surface.
    c.handle(
        "pane.report_agent",
        &json!({"pane_id": "w1:p1", "source": "starcil:codex", "agent": "codex", "state": "idle"}),
    )
    .unwrap();
    let r = c.handle("agent.get", &json!({"target": "w1:p1"})).unwrap();
    assert_eq!(r["agent"]["agent_session"]["kind"], "id");
    assert_eq!(r["agent"]["agent_session"]["value"], "abc-123");
}

#[test]
fn agent_start_launches_program_and_names_pane() {
    let mut c = core();
    c.handle("pane.split", &json!({"pane_id": "w1:p1", "direction": "right"})).unwrap();
    let r = c
        .handle(
            "agent.start",
            &json!({"name": "reviewer", "kind": "codex", "pane_id": "w1:p2", "args": ["--model", "gpt-5.6-sol"]}),
        )
        .unwrap();
    assert_eq!(r["type"], "agent_started");
    let term = term_of(&c, "w1:p2");
    let writes = &c.host.terminal(&term).writes;
    assert_eq!(
        writes,
        &vec![FakeWrite::Text("codex --model gpt-5.6-sol".into()), FakeWrite::Enter]
    );
    // Starting a second agent in the same pane is refused.
    let e = c
        .handle("agent.start", &json!({"name": "other", "kind": "claude", "pane_id": "w1:p2"}))
        .unwrap_err();
    assert!(e.message.contains("already hosts"));
    // Unknown kinds are refused.
    let e = c
        .handle("agent.start", &json!({"name": "x", "kind": "skynet", "pane_id": "w1:p1"}))
        .unwrap_err();
    assert!(e.message.contains("unknown agent kind"));
}

#[test]
fn detection_from_osc_title() {
    let mut c = core();
    let term = term_of(&c, "w1:p1");
    c.host.terminals.get_mut(&term).unwrap().title = Some("Claude Code".to_string());
    let before = c.handle("session.snapshot", &json!({})).unwrap()["revision"]
        .as_u64()
        .unwrap();
    c.tick_agents();
    let r = c.handle("agent.list", &json!({})).unwrap();
    let agents = r["agents"].as_array().unwrap();
    assert_eq!(agents.len(), 1, "OSC title should identify the claude pane");
    assert_eq!(agents[0]["agent"], "claude");
    // TUI clients refetch the snapshot only on a revision change; detection
    // must move it or `pane.agent` stays stale (the composer never hid).
    let snapshot = c.handle("session.snapshot", &json!({})).unwrap();
    assert!(
        snapshot["revision"].as_u64().unwrap() > before,
        "detection bumps the model revision"
    );
    assert_eq!(snapshot["panes"][0]["agent"], "claude");
    // Nothing more to detect: the revision rests between ticks.
    let settled = snapshot["revision"].as_u64().unwrap();
    c.tick_agents();
    assert_eq!(
        c.handle("session.snapshot", &json!({})).unwrap()["revision"].as_u64().unwrap(),
        settled
    );
}

#[test]
fn closing_pane_drops_agent() {
    let mut c = core();
    c.handle("pane.split", &json!({"pane_id": "w1:p1", "direction": "right"})).unwrap();
    c.handle(
        "pane.report_agent",
        &json!({"pane_id": "w1:p2", "source": "starcil:claude", "agent": "claude", "state": "working"}),
    )
    .unwrap();
    c.handle("pane.close", &json!({"pane_id": "w1:p2"})).unwrap();
    let r = c.handle("agent.list", &json!({})).unwrap();
    assert_eq!(r["agents"].as_array().unwrap().len(), 0);
}

#[test]
fn agent_exit_clears_the_pane_once_nothing_runs_under_the_shell() {
    let mut c = core();
    c.set_agent_exit_grace(std::time::Duration::ZERO);
    let term = term_of(&c, "w1:p1");
    fn revision(c: &mut ServerCore<FakeHost>) -> u64 {
        c.handle("session.snapshot", &json!({})).unwrap()["revision"]
            .as_u64()
            .unwrap()
    }
    fn agent_count(c: &mut ServerCore<FakeHost>) -> usize {
        c.handle("agent.list", &json!({})).unwrap()["agents"]
            .as_array()
            .unwrap()
            .len()
    }

    // A host that cannot see processes (None) detects by title and never
    // ends an agent on its own: Claude Code retitles itself all the time.
    c.host.terminals.get_mut(&term).unwrap().title = Some("Claude Code".into());
    c.tick_agents();
    assert_eq!(agent_count(&mut c), 1);
    c.host.terminals.get_mut(&term).unwrap().title = Some("fixing the composer".into());
    c.tick_agents();
    assert_eq!(agent_count(&mut c), 1, "a title change alone never ends an agent");

    // Something still runs under the shell: the agent stays.
    c.host.terminals.get_mut(&term).unwrap().descendants = Some(vec!["node".into()]);
    c.tick_agents();
    assert_eq!(agent_count(&mut c), 1);

    // The shell sits idle: the agent exited, the pane is a plain shell again
    // and the revision moves so TUI clients bring their composer back.
    let before = revision(&mut c);
    c.host.terminals.get_mut(&term).unwrap().descendants = Some(vec![]);
    c.tick_agents();
    assert_eq!(agent_count(&mut c), 0);
    assert!(revision(&mut c) > before);
    let snapshot = c.handle("session.snapshot", &json!({})).unwrap();
    assert!(snapshot["panes"][0]["agent"].is_null());
    assert!(snapshot["panes"][0]["agent_name"].is_null());

    // Stale title on an idle shell (cmd keeps the last program's title): no
    // re-detection, nothing flaps.
    c.host.terminals.get_mut(&term).unwrap().title = Some("Claude Code".into());
    let settled = revision(&mut c);
    c.tick_agents();
    c.tick_agents();
    assert_eq!(agent_count(&mut c), 0);
    assert_eq!(revision(&mut c), settled);

    // Detection by process name, no title needed.
    c.host.terminals.get_mut(&term).unwrap().title = None;
    c.host.terminals.get_mut(&term).unwrap().descendants =
        Some(vec!["cmd".into(), "codex".into()]);
    c.tick_agents();
    let agents = c.handle("agent.list", &json!({})).unwrap();
    assert_eq!(agents["agents"][0]["agent"], "codex");
}

#[test]
fn agent_exit_waits_for_the_grace_period() {
    let mut c = core();
    let term = term_of(&c, "w1:p1");
    c.host.terminals.get_mut(&term).unwrap().descendants = Some(vec!["claude".into()]);
    c.tick_agents();
    // Gone within the default grace (1s): still counted as starting up, the
    // way `agent.start` looks before the shell has spawned the CLI.
    c.host.terminals.get_mut(&term).unwrap().descendants = Some(vec![]);
    c.tick_agents();
    let agents = c.handle("agent.list", &json!({})).unwrap();
    assert_eq!(agents["agents"].as_array().unwrap().len(), 1);
    assert_eq!(agents["agents"][0]["agent"], "claude");
}

#[test]
fn shell_idle_follows_the_process_tree_and_travels_as_a_plain_event() {
    let mut c = core();
    let term = term_of(&c, "w1:p1");
    let snapshot_idle = |c: &mut ServerCore<FakeHost>| {
        c.handle("session.snapshot", &json!({})).unwrap()["panes"][0]["shell_idle"].clone()
    };

    // Blind host: nothing to say.
    c.tick_agents();
    assert!(snapshot_idle(&mut c).is_null());

    // A program runs under the shell.
    c.host.terminals.get_mut(&term).unwrap().descendants = Some(vec!["python".into()]);
    c.pending_events.clear();
    c.tick_agents();
    assert_eq!(snapshot_idle(&mut c), json!(false));
    assert!(c.pending_events.iter().any(|(name, data)| {
        name == "pane.shell_idle_changed" && data["idle"] == json!(false)
    }));
    // Not an agent: `python` is no whole-word match for any kind.
    assert!(c.handle("agent.list", &json!({})).unwrap()["agents"]
        .as_array()
        .unwrap()
        .is_empty());

    // Back at the prompt; a steady state emits nothing more.
    c.host.terminals.get_mut(&term).unwrap().descendants = Some(vec![]);
    c.pending_events.clear();
    c.tick_agents();
    assert_eq!(snapshot_idle(&mut c), json!(true));
    assert_eq!(c.pending_events.len(), 1);
    c.pending_events.clear();
    c.tick_agents();
    assert!(c.pending_events.is_empty());
}

#[test]
fn pane_cwd_follows_the_shell_and_travels_as_an_event() {
    let mut c = core();
    let term = term_of(&c, "w1:p1");
    let snapshot_cwd = |c: &mut ServerCore<FakeHost>| {
        c.handle("session.snapshot", &json!({})).unwrap()["panes"][0]["cwd"].clone()
    };

    // Blind host: the pane keeps the cwd it was spawned with.
    c.tick_agents();
    assert_eq!(snapshot_cwd(&mut c), json!("C:/dev/proj"));

    // The user typed `cd`: the snapshot follows, the revision moves, and the
    // change travels as a plain event (no snapshot refetch needed).
    let before = c.model.revision;
    c.host.terminals.get_mut(&term).unwrap().cwd = Some("C:/dev/proj/tests".into());
    c.pending_events.clear();
    c.tick_agents();
    assert_eq!(snapshot_cwd(&mut c), json!("C:/dev/proj/tests"));
    assert!(c.model.revision > before);
    assert!(c.pending_events.iter().any(|(name, data)| {
        name == "pane.cwd_changed"
            && data["pane_id"] == json!("w1:p1")
            && data["cwd"] == json!("C:/dev/proj/tests")
    }));

    // Steady state: nothing more, and the revision stays put.
    let settled = c.model.revision;
    c.pending_events.clear();
    c.tick_agents();
    assert!(c.pending_events.is_empty());
    assert_eq!(c.model.revision, settled);

    // A split inherits the live cwd, not the stale spawn one.
    c.handle("pane.split", &json!({"pane_id": "w1:p1", "direction": "right"})).unwrap();
    assert_eq!(c.model.pane("w1:p2".parse().unwrap()).unwrap().cwd, "C:/dev/proj/tests");
}
