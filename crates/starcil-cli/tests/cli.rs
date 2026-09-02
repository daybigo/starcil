use serde_json::json;
use starcil_cli::{group_help, parse, Connection, NdjsonConnection, COMMAND_GROUPS, ROOT_HELP};
use starcil_protocol::{Incoming, Request};
use std::io::{self, Cursor, Read, Write};

fn args(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
}

#[test]
fn every_command_group_serves_its_own_help() {
    for group in COMMAND_GROUPS {
        let help = group_help(group.name).expect("every group has help text");
        assert!(
            help.starts_with(&format!("starcil {} commands:
", group.name)),
            "help for {} must open with its header",
            group.name
        );
        for subcommand in group.subcommands {
            assert!(help.contains(subcommand), "help for {} must list `{subcommand}`", group.name);
        }
        let invocation = parse(&args(&[group.name])).expect("bare group parses");
        match invocation.behavior {
            starcil_cli::Behavior::Help(actual) => {
                assert_eq!(actual, help, "bare help mismatch for {}", group.name)
            }
            behavior => panic!("expected help for {}, got {behavior:?}", group.name),
        }
    }
    assert_eq!(group_help("nope"), None);
    let listed = COMMAND_GROUPS.iter().map(|group| group.name).collect::<Vec<_>>().join(" ");
    assert!(
        ROOT_HELP.contains(&format!("groups: {listed}
")),
        "root help must list every group in registration order"
    );
}

#[test]
fn parser_accepts_the_complete_command_surface() {
    let cases: &[(&[&str], &str)] = &[
        (&["status"], "ping"),
        (&["status", "server"], "ping"),
        (&["status", "client"], "ping"),
        (&["server", "stop"], "server.stop"),
        (&["server", "reload-config"], "server.reload_config"),
        (&["agent", "list"], "agent.list"),
        (&["agent", "get", "w1:p1"], "agent.get"),
        (&["agent", "read", "w1:p1"], "agent.read"),
        (&["agent", "read", "bot", "--source", "detection", "--lines", "12", "--format", "ansi"], "agent.read"),
        (&["agent", "send-keys", "bot", "ctrl+c", "enter"], "agent.send_keys"),
        (&["agent", "prompt", "bot", "continue"], "agent.prompt"),
        (&["agent", "prompt", "bot", "continue", "--wait", "--until", "done", "--timeout", "1000"], "agent.prompt"),
        (&["agent", "rename", "bot", "builder"], "agent.rename"),
        (&["agent", "rename", "bot", "--clear"], "agent.rename"),
        (&["agent", "focus", "bot"], "agent.focus"),
        (&["agent", "wait", "bot", "--until", "idle"], "agent.wait"),
        (&["agent", "attach", "bot", "--takeover"], "agent.attach"),
        (&["agent", "start", "builder", "--kind", "codex", "--pane", "w1:p2"], "agent.start"),
        (&["agent", "start", "builder", "--kind", "codex", "--pane", "w1:p2", "--", "--profile", "lean"], "agent.start"),
        (&["agent", "explain", "bot", "--verbose"], "agent.explain"),
        (&["agent", "explain", "--file", "screen.txt", "--agent", "codex", "--json"], "agent.explain"),
        (&["pane", "list"], "pane.list"),
        (&["pane", "list", "--workspace", "w1"], "pane.list"),
        (&["pane", "current", "--pane", "w1:p1"], "pane.current"),
        (&["pane", "get", "w1:p1"], "pane.get"),
        (&["pane", "layout", "--current"], "pane.layout"),
        (&["pane", "process-info", "--pane", "w1:p1"], "pane.process_info"),
        (&["pane", "neighbor", "--direction", "right", "--current"], "pane.neighbor"),
        (&["pane", "edges"], "pane.edges"),
        (&["pane", "focus", "--direction", "left", "--pane", "w1:p2"], "pane.focus_direction"),
        (&["pane", "resize", "--direction", "down", "--amount", "0.2", "--current"], "pane.resize"),
        (&["pane", "zoom", "w1:p1", "--toggle"], "pane.zoom"),
        (&["pane", "zoom", "--current", "--on"], "pane.zoom"),
        (&["pane", "rename", "w1:p1", "logs"], "pane.rename"),
        (&["pane", "rename", "w1:p1", "--clear"], "pane.rename"),
        (&["pane", "read", "w1:p1", "--source", "recent", "--lines", "80", "--format", "text"], "pane.read"),
        (&["pane", "split", "--current", "--direction", "right"], "pane.split"),
        (&["pane", "swap", "--direction", "right", "--current"], "pane.swap"),
        (&["pane", "swap", "--source-pane", "w1:p1", "--target-pane", "w1:p2"], "pane.swap"),
        (&["pane", "move", "w1:p1", "--tab", "w1:t2", "--split", "down"], "pane.move"),
        (&["pane", "move", "w1:p1", "--new-tab", "--workspace", "w2"], "pane.move"),
        (&["pane", "move", "w1:p1", "--new-workspace", "--label", "api"], "pane.move"),
        (&["pane", "close", "w1:p1"], "pane.close"),
        (&["pane", "send-text", "w1:p1", "hello"], "pane.send_text"),
        (&["pane", "send-keys", "w1:p1", "alt+x", "f12"], "pane.send_keys"),
        (&["pane", "wait-output", "w1:p1", "--match", "done"], "pane.wait_for_output"),
        (&["pane", "wait-output", "w1:p1", "--regex", "done.*", "--raw"], "pane.wait_for_output"),
        (&["pane", "report-agent", "w1:p1", "--source", "hook:test", "--agent", "codex", "--state", "working"], "pane.report_agent"),
        (&["pane", "report-agent-session", "w1:p1", "--source", "hook:test", "--agent", "codex"], "pane.report_agent_session"),
        (&["pane", "release-agent", "w1:p1", "--source", "hook:test", "--agent", "codex"], "pane.release_agent"),
        (&["pane", "report-metadata", "w1:p1", "--source", "hook:test", "--token", "model=sol"], "pane.report_metadata"),
        (&["pane", "run", "w1:p1", "cargo test"], "pane.run"),
        (&["workspace", "list"], "workspace.list"),
        (&["workspace", "create", "--cwd", "C:/dev", "--env", "A=B", "--no-focus"], "workspace.create"),
        (&["workspace", "get", "w1"], "workspace.get"),
        (&["workspace", "focus", "w1"], "workspace.focus"),
        (&["workspace", "rename", "w1", "api"], "workspace.rename"),
        (&["workspace", "report-metadata", "w1", "--source", "hook:test", "--token", "branch=main"], "workspace.report_metadata"),
        (&["workspace", "close", "w1"], "workspace.close"),
        (&["tab", "list", "--workspace", "w1"], "tab.list"),
        (&["tab", "create", "--workspace", "w1", "--label", "logs"], "tab.create"),
        (&["tab", "get", "w1:t1"], "tab.get"),
        (&["tab", "focus", "w1:t1"], "tab.focus"),
        (&["tab", "rename", "w1:t1", "logs"], "tab.rename"),
        (&["tab", "close", "w1:t1"], "tab.close"),
        (&["worktree", "list", "--workspace", "w1"], "worktree.list"),
        (&["worktree", "create", "--cwd", "C:/dev/repo", "--branch", "feat/api", "--base", "main", "--path", "C:/dev/api", "--focus"], "worktree.create"),
        (&["worktree", "open", "--workspace", "w1", "--branch", "feat/api"], "worktree.open"),
        (&["worktree", "remove", "--workspace", "w2", "--force"], "worktree.remove"),
        (&["terminal", "title", "set", "Starcil API"], "client.window_title.set"),
        (&["terminal", "title", "clear"], "client.window_title.clear"),
        (&["notification", "show", "done", "--body", "tests passed", "--position", "top-right", "--sound", "done"], "notification.show"),
        (&["integration", "install", "codex"], "integration.install"),
        (&["integration", "uninstall", "claude"], "integration.uninstall"),
        (&["integration", "status", "--outdated-only"], "integration.status"),
        (&["plugin", "install", "owner/repo/subdir", "--ref", "v1.2.0", "--yes"], "plugin.link"),
        (&["plugin", "uninstall", "example.plugin"], "plugin.unlink"),
        (&["plugin", "uninstall", "owner/repo/subdir"], "plugin.unlink"),
        (&["plugin", "link", "C:/dev/plugin", "--disabled"], "plugin.link"),
        (&["plugin", "list", "--plugin", "example.plugin", "--json"], "plugin.list"),
        (&["plugin", "config-dir", "example.plugin"], "plugin.list"),
        (&["plugin", "unlink", "example.plugin"], "plugin.unlink"),
        (&["plugin", "enable", "example.plugin"], "plugin.enable"),
        (&["plugin", "disable", "example.plugin"], "plugin.disable"),
        (&["plugin", "action", "list", "--plugin", "example.plugin"], "plugin.action.list"),
        (&["plugin", "action", "invoke", "example.plugin.apply", "--plugin", "example.plugin"], "plugin.action.invoke"),
        (&["plugin", "log", "list", "--plugin", "example.plugin", "--limit", "20"], "plugin.log.list"),
        (&["plugin", "pane", "open", "--plugin", "example.plugin", "--entrypoint", "board"], "plugin.pane.open"),
        (&["plugin", "pane", "focus", "w1:p2"], "plugin.pane.focus"),
        (&["plugin", "pane", "close", "w1:p2"], "plugin.pane.close"),
        (&["integration", "hook", "claude-notification"], "local.integration.hook.claude-notification"),
        (&["integration", "hook", "claude-stop"], "local.integration.hook.claude-stop"),
        (&["integration", "hook", "claude-session-start"], "local.integration.hook.claude-session-start"),
        (&["integration", "hook", "codex-notify", r#"{"type":"agent-turn-complete"}"#], "local.integration.hook.codex-notify"),
        (&["session", "stop", "work"], "server.stop"),
        (&["api", "snapshot"], "session.snapshot"),
    ];

    assert!(cases.len() >= 60, "acceptance table must remain broad");
    for (input, method) in cases {
        let invocation = parse(&args(input)).unwrap_or_else(|error| panic!("{input:?}: {error}"));
        assert_eq!(&invocation.method, method, "method mismatch for {input:?}");
    }
}

#[test]
fn parser_maps_high_risk_params_exactly() {
    let split = parse(&args(&[
        "pane", "split", "w1:p1", "--direction", "right", "--ratio", "0.333", "--cwd",
        "C:/dev/api", "--env", "ROLE=test", "--env", "MODE=fast", "--no-focus",
    ])).unwrap();
    assert_eq!(split.request_id, "cli:pane:split");
    assert_eq!(split.params, json!({
        "pane_id": "w1:p1", "direction": "right", "ratio": 0.333, "cwd": "C:/dev/api",
        "env": {"ROLE": "test", "MODE": "fast"}, "focus": false
    }));

    let prompt = parse(&args(&[
        "agent", "prompt", "builder", "ship it", "--wait", "--until", "done", "--until",
        "blocked", "--timeout", "5000",
    ])).unwrap();
    assert_eq!(prompt.params, json!({
        "target": "builder", "text": "ship it",
        "wait": {"until": ["done", "blocked"], "timeout_ms": 5000}
    }));

    let wait = parse(&args(&[
        "pane", "wait-output", "w1:p2", "--regex", "FLEET_(DONE|BLOCKED)", "--source",
        "recent-unwrapped", "--lines", "120", "--timeout", "30000", "--raw",
    ])).unwrap();
    assert_eq!(wait.params, json!({
        "pane_id": "w1:p2", "regex": "FLEET_(DONE|BLOCKED)", "source": "recent-unwrapped",
        "lines": 120, "timeout_ms": 30000, "raw": true
    }));

    let report = parse(&args(&[
        "pane", "report-agent", "w1:p3", "--source", "hook:codex", "--agent", "codex",
        "--state", "working", "--message", "building", "--seq", "7", "--ttl-ms", "9000",
        "--agent-session-id", "abc",
    ])).unwrap();
    assert_eq!(report.params, json!({
        "pane_id": "w1:p3", "source": "hook:codex", "agent": "codex", "state": "working",
        "message": "building", "seq": 7, "ttl_ms": 9000, "agent_session_id": "abc"
    }));

    let moved = parse(&args(&[
        "pane", "move", "w1:p2", "--new-workspace", "--label", "api", "--tab-label", "main", "--focus",
    ])).unwrap();
    assert_eq!(moved.params, json!({
        "pane_id": "w1:p2", "destination": {"type": "new_workspace", "label": "api", "tab_label": "main"}, "focus": true
    }));

    let open = parse(&args(&[
        "worktree", "open", "--cwd", "C:/dev/repo", "--branch", "feat/api", "--label", "API", "--no-focus",
    ])).unwrap();
    assert_eq!(open.params, json!({"cwd": "C:/dev/repo", "branch": "feat/api", "label": "API", "focus": false}));

    let notification = parse(&args(&[
        "notification", "show", "Build failed", "--body", "API workspace", "--position", "bottom-left", "--sound", "request",
    ])).unwrap();
    assert_eq!(notification.params, json!({
        "title": "Build failed", "body": "API workspace", "position": "bottom-left", "sound": "request"
    }));

    let pane = parse(&args(&[
        "plugin", "pane", "open", "--plugin", "example.plugin", "--entrypoint", "board",
        "--placement", "popup", "--width", "80%", "--height", "30", "--workspace", "w1",
        "--target-pane", "w1:p1", "--direction", "right", "--cwd", "C:/dev/plugin",
        "--env", "MODE=review", "--no-focus",
    ])).unwrap();
    match pane.behavior {
        starcil_cli::Behavior::Plugin { action: starcil_cli::PluginAction::PaneOpen { params }, .. } => {
            assert_eq!(params, json!({
                "plugin_id": "example.plugin", "entrypoint": "board", "placement": "popup",
                "width": "80%", "height": 30, "workspace_id": "w1", "target_pane_id": "w1:p1",
                "direction": "right", "cwd": "C:/dev/plugin", "env": {"MODE": "review"}, "focus": false
            }));
        }
        behavior => panic!("expected plugin pane action, got {behavior:?}"),
    }
}

#[test]
fn parser_preserves_github_slugs_refs_and_hidden_hook_payloads() {
    let install = parse(&args(&[
        "plugin", "install", "acme/tools/review/helper", "--ref", "release-2", "--yes",
    ])).unwrap();
    match install.behavior {
        starcil_cli::Behavior::Plugin {
            action: starcil_cli::PluginAction::Install { source, requested_ref, yes },
            ..
        } => {
            assert_eq!(source.owner, "acme");
            assert_eq!(source.repo, "tools");
            assert_eq!(source.subdir.as_deref(), Some("review/helper"));
            assert_eq!(requested_ref.as_deref(), Some("release-2"));
            assert!(yes);
        }
        behavior => panic!("expected install, got {behavior:?}"),
    }

    let uninstall = parse(&args(&["plugin", "uninstall", "acme/tools/review/helper"])).unwrap();
    assert!(matches!(
        uninstall.behavior,
        starcil_cli::Behavior::Plugin {
            action: starcil_cli::PluginAction::Uninstall {
                target: starcil_cli::PluginTarget::Github(starcil_cli::GithubSlug { ref owner, ref repo, ref subdir })
            },
            ..
        } if owner == "acme" && repo == "tools" && subdir.as_deref() == Some("review/helper")
    ));

    let payload = r#"{"type":"agent-turn-complete","thread-id":"abc"}"#;
    let hook = parse(&args(&["integration", "hook", "codex-notify", payload])).unwrap();
    assert!(matches!(
        hook.behavior,
        starcil_cli::Behavior::IntegrationHook {
            action: starcil_cli::IntegrationHookAction::CodexNotify { payload: Some(ref actual) },
            ..
        } if actual == payload
    ));
}

#[test]
fn parser_rejects_malformed_invocations_with_group_usage() {
    let cases: &[&[&str]] = &[
        &["wat"],
        &["pane", "wat"],
        &["pane", "list", "--wat"],
        &["agent", "get"],
        &["agent", "send-keys", "bot"],
        &["agent", "send-keys", "bot", "definitely-not-a-key"],
        &["agent", "prompt", "bot", "text", "--timeout", "nan"],
        &["agent", "start", "BadName", "--kind", "codex", "--pane", "w1:p1"],
        &["agent", "start", "bot", "--kind", "unknown", "--pane", "w1:p1"],
        &["agent", "start", "bot", "--kind", "codex"],
        &["pane", "split", "--direction", "left"],
        &["pane", "split", "--direction", "right", "--focus", "--no-focus"],
        &["pane", "split", "--direction", "right", "--ratio", "2"],
        &["pane", "zoom", "w1:p1", "--pane", "w1:p2"],
        &["pane", "zoom", "--on", "--off"],
        &["pane", "swap", "--source-pane", "w1:p1"],
        &["pane", "wait-output", "w1:p1"],
        &["pane", "wait-output", "w1:p1", "--match", "x", "--regex", "y"],
        &["pane", "move", "w1:p1", "--new-tab", "--new-workspace"],
        &["pane", "report-agent", "w1:p1", "--source", "x"],
        &["workspace", "create", "--env", "BROKEN"],
        &["worktree", "list", "--workspace", "w1", "--cwd", "C:/dev"],
        &["worktree", "open", "--branch", "a", "--path", "C:/dev/a"],
        &["worktree", "open"],
        &["worktree", "remove", "--force"],
        &["terminal", "session", "observe", "w1:p1", "--takeover"],
        &["notification", "show", "done", "--sound", "loud"],
        &["api", "schema", "--json", "--output", "schema.json"],
        &["plugin", "install", "owner"],
        &["plugin", "install", "owner/../repo"],
        &["plugin", "install", "owner/repo", "--ref", "main", "--ref", "other"],
        &["plugin", "list", "--json", "--json"],
        &["plugin", "action", "invoke"],
        &["plugin", "pane", "open", "--plugin", "example.plugin"],
        &["plugin", "pane", "open", "--plugin", "example.plugin", "--entrypoint", "board", "--focus", "--no-focus"],
        &["plugin", "pane", "open", "--plugin", "example.plugin", "--entrypoint", "board", "--width", "101%"],
        &["integration", "hook", "unknown"],
        &["--no-session", "--session", "work"],
    ];

    assert!(cases.len() >= 15, "rejection table must remain broad");
    for input in cases {
        let error = parse(&args(input)).unwrap_err();
        assert!(!error.message.is_empty(), "empty error for {input:?}");
        assert!(error.usage.starts_with("starcil "), "missing group usage for {input:?}: {error:?}");
    }
}

#[test]
fn explicit_session_is_transport_metadata_not_rpc_params() {
    let invocation = parse(&args(&["--session", "work", "pane", "list"])).unwrap();
    assert_eq!(invocation.params, json!({}));
    assert_eq!(
        invocation.behavior,
        starcil_cli::Behavior::Socket { session: Some("work".into()), output: starcil_cli::OutputMode::Json }
    );
}

#[test]
fn bundled_skill_is_the_repository_skill_file() {
    let skill = starcil_cli::BUNDLED_SKILL;
    assert!(skill.starts_with("---\nname: starcil\n"), "skill must open with its frontmatter");
    assert!(skill.contains("STARCIL_ENV"), "skill must carry the in-pane guardrail");
    match parse(&args(&["--skill"])).expect("--skill parses").behavior {
        starcil_cli::Behavior::Skill => {}
        behavior => panic!("expected Behavior::Skill, got {behavior:?}"),
    }
    assert!(ROOT_HELP.contains("  starcil --skill\n"));
}

#[test]
fn parser_covers_root_and_special_flows() {
    let cases: &[&[&str]] = &[
        &[],
        &["--session", "work"],
        &["--remote", "buildbox", "--remote-keybindings", "server", "--handoff"],
        &["--no-session"],
        &["--help"],
        &["--version"],
        &["--default-config"],
        &["--skill"],
        &["update"],
        &["update", "--handoff"],
        &["server"],
        &["server", "--help"],
        &["completion", "zsh"],
        &["completion", "bash"],
        &["completion", "powershell"],
        &["terminal", "attach", "term_1", "--takeover"],
        &["terminal", "session", "control", "w1:p1", "--takeover", "--cols", "120", "--rows", "40"],
        &["terminal", "session", "observe", "w1:p1", "--cols", "80", "--rows", "24"],
        &["session", "list", "--json"],
        &["session", "attach", "work"],
        &["session", "delete", "work", "--json"],
        &["api", "schema"],
        &["api", "schema", "--json"],
        &["api", "schema", "--output", "schema.json"],
        &["config", "check"],
        &["config", "reset-keys"],
        &["channel", "show"],
        &["channel", "set", "preview"],
        &["plugin"],
        &["plugin", "--help"],
        &["pane", "--help"],
    ];
    for input in cases {
        parse(&args(input)).unwrap_or_else(|error| panic!("special flow {input:?}: {error}"));
    }
}

struct ScriptedIo {
    reads: Cursor<Vec<u8>>,
    writes: Vec<u8>,
}

impl ScriptedIo {
    fn new(script: &str) -> Self {
        Self { reads: Cursor::new(script.as_bytes().to_vec()), writes: Vec::new() }
    }
}

impl Read for ScriptedIo {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        self.reads.read(buffer)
    }
}

impl Write for ScriptedIo {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.writes.extend_from_slice(buffer);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn ndjson_connection_ignores_events_and_other_response_ids() {
    let script = concat!(
        "{\"event\":\"pane.updated\",\"data\":{}}\n",
        "{\"id\":\"other\",\"result\":{\"type\":\"pong\"}}\n",
        "{\"id\":\"cli:root:status\",\"result\":{\"type\":\"pong\"}}\n",
    );
    let mut connection = NdjsonConnection::new(ScriptedIo::new(script));
    let request = Request::new("cli:root:status", "ping", json!({}));
    let incoming = connection.call(&request).unwrap();
    match incoming {
        Incoming::Success(response) => assert_eq!(response.id, "cli:root:status"),
        value => panic!("expected success, got {value:?}"),
    }
}
