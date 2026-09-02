use serde_json::json;
use starcil_domain::{Node, PaneId};
use starcil_server::hosttraits::{
    HostError, ReadFormat, ReadSource, ScrollInfo, TerminalHost, TerminalReadout, TerminalSpawn,
};
use starcil_server::persistence::{
    capture, clear_recipes, remember_recipe, restore_at_boot, save_if_dirty, PersistenceState,
};
use starcil_server::ServerCore;
use starcil_testkit::FakeHost;

struct RecordingHost {
    inner: FakeHost,
    attempts: Vec<TerminalSpawn>,
}

impl RecordingHost {
    fn new() -> Self {
        Self {
            inner: FakeHost::new(),
            attempts: Vec::new(),
        }
    }
}

impl TerminalHost for RecordingHost {
    fn spawn(&mut self, spec: TerminalSpawn) -> Result<String, HostError> {
        self.attempts.push(spec.clone());
        if spec
            .command
            .as_ref()
            .and_then(|argv| argv.first())
            .is_some_and(|program| program == "dead-recipe")
        {
            return Err(HostError::SpawnFailed("recipe executable not found".to_owned()));
        }
        self.inner.spawn(spec)
    }

    fn kill(&mut self, terminal_id: &str) -> Result<(), HostError> {
        self.inner.kill(terminal_id)
    }

    fn is_alive(&self, terminal_id: &str) -> bool {
        self.inner.is_alive(terminal_id)
    }

    fn write_text(&mut self, terminal_id: &str, text: &str) -> Result<(), HostError> {
        self.inner.write_text(terminal_id, text)
    }

    fn write_enter(&mut self, terminal_id: &str) -> Result<(), HostError> {
        self.inner.write_enter(terminal_id)
    }

    fn write_keys(&mut self, terminal_id: &str, keys: &[String]) -> Result<(), HostError> {
        self.inner.write_keys(terminal_id, keys)
    }

    fn paste_text(&mut self, terminal_id: &str, text: &str) -> Result<(), HostError> {
        self.inner.paste_text(terminal_id, text)
    }

    fn resize(&mut self, terminal_id: &str, cols: u16, rows: u16) -> Result<(), HostError> {
        self.inner.resize(terminal_id, cols, rows)
    }

    fn read(
        &self,
        terminal_id: &str,
        source: ReadSource,
        lines: usize,
        format: ReadFormat,
    ) -> Result<TerminalReadout, HostError> {
        self.inner.read(terminal_id, source, lines, format)
    }

    fn scroll_info(&self, terminal_id: &str) -> Option<ScrollInfo> {
        self.inner.scroll_info(terminal_id)
    }

    fn terminal_title(&self, terminal_id: &str) -> Option<String> {
        self.inner.terminal_title(terminal_id)
    }

    fn change_seq(&self, terminal_id: &str) -> u64 {
        self.inner.change_seq(terminal_id)
    }

    fn process_info(&self, terminal_id: &str) -> Result<serde_json::Value, HostError> {
        self.inner.process_info(terminal_id)
    }

    fn take_frame(&mut self, terminal_id: &str, snapshot: bool) -> Option<serde_json::Value> {
        self.inner.take_frame(terminal_id, snapshot)
    }
}

#[test]
fn capture_save_load_and_restore_preserves_layout_and_launch_contracts() {
    let session = "b3-restore";
    clear_recipes(session);
    let mut original =
        ServerCore::new(session, "C:/workspace/root", FakeHost::new()).expect("original core");
    original
        .handle(
            "pane.split",
            &json!({
                "pane_id": "w1:p1",
                "direction": "right",
                "ratio": 0.37,
                "cwd": "C:/workspace/recipe"
            }),
        )
        .expect("split pane");
    let first: PaneId = "w1:p1".parse().unwrap();
    let recipe: PaneId = "w1:p2".parse().unwrap();
    original.model.workspaces[0].label = "Product".to_owned();
    original.model.workspaces[0].tabs[0].label = "Main".to_owned();
    original.model.workspaces[0].tabs[0].focused_pane = recipe;
    original.model.workspaces[0].tabs[0].zoomed = Some(recipe);
    original.model.pane_mut(first).unwrap().label = Some("Claude".to_owned());
    original.model.pane_mut(recipe).unwrap().label = Some("Recipe".to_owned());
    original
        .report_agent_session_handler(&json!({
            "pane_id": first.to_string(),
            "source": "starcil:integration",
            "agent": "claude",
            "agent_session_id": "session-live-42"
        }))
        .expect("agent session report");
    remember_recipe(
        session,
        recipe,
        vec!["dead-recipe".to_owned(), "--restore".to_owned()],
    );

    let captured = capture(&original);
    assert_eq!(captured.pane_extras[&first].agent_kind.as_deref(), Some("claude"));
    assert_eq!(
        captured.pane_extras[&recipe].recipe_argv.as_deref(),
        Some(["dead-recipe".to_owned(), "--restore".to_owned()].as_slice())
    );

    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("state-b3-restore.json");
    let mut persistence = PersistenceState::new(path.clone());
    persistence.mark_dirty();
    assert!(save_if_dirty(&mut persistence, &original).expect("state save"));
    assert!(!persistence.dirty);
    let loaded = starcil_persist::load(&path).expect("state load").doc;

    let mut restored = ServerCore::new(
        session,
        "C:/discarded",
        RecordingHost::new(),
    )
    .expect("fresh core");
    let report = restore_at_boot(&mut restored, loaded, true);
    assert!(report.success, "restore failed: {:?}", report.fatal_error);
    assert_eq!((report.restored_workspaces, report.restored_tabs, report.restored_panes), (1, 1, 2));
    assert_eq!(report.rebound_sessions, 1);
    assert_eq!(report.launch_failures.len(), 1);
    assert!(report.launch_failures[0].fell_back_to_shell);
    assert_eq!(report.launch_failures[0].command[0], "dead-recipe");

    let new_first = report.bindings.panes[&first];
    let new_recipe = report.bindings.panes[&recipe];
    let workspace = &restored.model.workspaces[0];
    let tab = &workspace.tabs[0];
    assert_eq!(workspace.label, "Product");
    assert_eq!(workspace.cwd, "C:/workspace/root");
    assert_eq!(tab.label, "Main");
    assert_eq!(tab.focused_pane, new_recipe);
    assert_eq!(tab.zoomed, Some(new_recipe));
    assert!(matches!(&tab.tree, Node::Split { ratio, .. } if (*ratio - 0.37).abs() < f32::EPSILON));
    assert_eq!(restored.model.pane(new_first).unwrap().label.as_deref(), Some("Claude"));
    assert_eq!(restored.model.pane(new_first).unwrap().cwd, "C:/workspace/root");
    assert_eq!(restored.model.pane(new_recipe).unwrap().label.as_deref(), Some("Recipe"));
    assert_eq!(restored.model.pane(new_recipe).unwrap().cwd, "C:/workspace/recipe");

    let resume_spawn = restored.host.attempts.iter().find(|attempt| {
        attempt.command.as_deref()
            == Some(["claude".to_owned(), "--resume".to_owned(), "session-live-42".to_owned()].as_slice())
    });
    assert!(resume_spawn.is_some(), "native Claude resume argv was not spawned");
    let new_recipe_text = new_recipe.to_string();
    let recipe_attempts: Vec<_> = restored
        .host
        .attempts
        .iter()
        .filter(|attempt| {
            attempt.env.get("STARCIL_PANE_ID").map(String::as_str)
                == Some(new_recipe_text.as_str())
        })
        .collect();
    assert!(recipe_attempts.iter().any(|attempt| {
        attempt.command.as_ref().is_some_and(|argv| argv.first().is_some_and(|program| program == "dead-recipe"))
    }));
    assert!(recipe_attempts.iter().any(|attempt| attempt.command.is_none()));
    let restored_session = restored.agents.panes[&new_first]
        .session_ref
        .as_ref()
        .expect("restored session ref");
    assert_eq!(restored_session["value"], "session-live-42");

    clear_recipes(session);
}
