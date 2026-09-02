use serde_json::json;
use starcil_plugins::{
    load_manifest, ActionSpec, ActiveContext, BuildSpec, CommandState, EventHookSpec,
    GithubSourceMetadata, HostEnvironment, LinkHandlerSpec, LogStore, ManifestValidator,
    PaneDimension, PaneOpenOptions, PanePlacement, PaneSpec, Platform, PluginError,
    PluginExecutor, PluginManifest, PluginRegistry, RegistryPaths, SourceMetadata, StartupSpec,
    STARCIL_PLUGIN_MANIFEST,
};
use starcil_protocol::error::{ApiError, ErrorCode};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(1);

struct TestDir {
    path: PathBuf,
}

impl TestDir {
    fn new(label: &str) -> Self {
        let nonce = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let id = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("starcil-plugins-{label}-{}-{nonce}-{id}", std::process::id()));
        fs::create_dir_all(&path).unwrap();
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let temp_root = std::env::temp_dir();
        let safe_name = self.path.file_name().and_then(|name| name.to_str()).is_some_and(|name| name.starts_with("starcil-plugins-"));
        if self.path.starts_with(&temp_root) && safe_name {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}

fn full_manifest() -> PluginManifest {
    PluginManifest {
        id: "example.full".to_owned(),
        name: "Full plugin".to_owned(),
        version: "0.2.0".to_owned(),
        min_starcil_version: "0.1.0".to_owned(),
        description: Some("Every manifest section".to_owned()),
        platforms: Some(vec![Platform::Linux, Platform::Macos, Platform::Windows]),
        build: vec![BuildSpec { command: vec!["npm".into(), "ci".into()], platforms: None }],
        startup: vec![StartupSpec { command: vec!["node".into(), "restore.js".into()], platforms: Some(vec![Platform::Windows]) }],
        actions: vec![ActionSpec {
            id: "apply".to_owned(),
            title: "Apply layout".to_owned(),
            contexts: vec!["workspace".to_owned()],
            command: vec!["node".into(), "apply.js".into()],
            platforms: None,
        }],
        events: vec![
            EventHookSpec { on: "worktree.created".to_owned(), command: vec!["node".into(), "event.js".into()], platforms: None },
            EventHookSpec { on: "future.event".to_owned(), command: vec!["node".into(), "future.js".into()], platforms: None },
        ],
        panes: vec![PaneSpec {
            id: "picker".to_owned(),
            title: "Picker".to_owned(),
            command: vec!["node".into(), "picker.js".into()],
            placement: PanePlacement::Popup,
            width: Some(PaneDimension::Percent("80%".to_owned())),
            height: Some(PaneDimension::Cells(20)),
            platforms: Some(vec![Platform::Linux, Platform::Windows]),
        }],
        link_handlers: vec![LinkHandlerSpec {
            id: "github-issue".to_owned(),
            title: "Open issue".to_owned(),
            pattern: "^https://github\\.com/".to_owned(),
            action: "apply".to_owned(),
            platforms: None,
        }],
    }
}

fn write_manifest(root: &Path, manifest: &PluginManifest) -> PathBuf {
    fs::create_dir_all(root).unwrap();
    let path = root.join(STARCIL_PLUGIN_MANIFEST);
    fs::write(&path, toml::to_string_pretty(manifest).unwrap()).unwrap();
    path
}

fn registry_paths(root: &Path) -> RegistryPaths {
    RegistryPaths::new(root.join("state").join("plugins.json"), root.join("plugin-data"))
}

#[test]
fn manifest_round_trip_validation_and_version_gate() {
    let temp = TestDir::new("manifest");
    let manifest = full_manifest();
    let manifest_path = write_manifest(&temp.path().join("plugin"), &manifest);
    let validator = ManifestValidator::new("0.2.0").unwrap();
    let loaded = load_manifest(&manifest_path, &validator).unwrap();
    assert_eq!(loaded.manifest, manifest);
    assert!(loaded.report.warnings.iter().any(|warning| warning.contains("unknown event name 'future.event'")));

    let encoded = toml::to_string_pretty(&loaded.manifest).unwrap();
    let decoded: PluginManifest = toml::from_str(&encoded).unwrap();
    assert_eq!(decoded, loaded.manifest);

    let missing: PluginManifest = toml::from_str("id='x'\nname='X'\nversion='0.1.0'\n").unwrap();
    let error = validator.validate(&missing).unwrap_err();
    assert!(error.to_string().contains("min_starcil_version"));

    let mut newer = full_manifest();
    newer.min_starcil_version = "9.0.0".to_owned();
    let error = validator.validate(&newer).unwrap_err();
    assert!(error.to_string().contains("requires Starcil 9.0.0"));
}

#[test]
fn platform_inheritance_resolves_top_level_override_and_portable_default() {
    let mut manifest = full_manifest();
    manifest.platforms = Some(vec![Platform::Windows]);
    assert_eq!(manifest.effective_platforms(None), vec![Platform::Windows]);
    assert_eq!(manifest.effective_platforms(Some(&[Platform::Linux])), vec![Platform::Linux]);
    assert!(manifest.supports(None, Platform::Windows));
    assert!(!manifest.supports(None, Platform::Linux));

    manifest.platforms = None;
    assert_eq!(manifest.effective_platforms(None), Platform::ALL.to_vec());
    let report = ManifestValidator::new("0.2.0").unwrap().validate(&manifest).unwrap();
    assert!(report.warnings.iter().any(|warning| warning.contains("top-level platforms")));
}

#[test]
fn registry_lifecycle_persists_and_missing_manifest_keeps_the_entry() {
    let temp = TestDir::new("registry");
    let plugin_root = temp.path().join("plugin");
    let manifest_path = write_manifest(&plugin_root, &full_manifest());
    let paths = registry_paths(temp.path());
    let source = GithubSourceMetadata {
        owner: "owner".into(),
        repo: "repo".into(),
        subdir: Some("plugin".into()),
        requested_ref: Some("main".into()),
        resolved_commit: "abc123".into(),
        managed_path: plugin_root.to_string_lossy().into_owned(),
        installed_unix_ms: 123,
    };

    let mut registry = PluginRegistry::open(paths.clone(), "0.2.0").unwrap();
    let linked = registry.link(&plugin_root, true, Some(SourceMetadata::from(source))).unwrap();
    assert_eq!(linked.plugin_id, "example.full");
    assert!(paths.registry_file.is_file());
    assert!(linked.config_dir.is_dir());
    assert!(linked.state_dir.is_dir());

    registry.disable("example.full").unwrap();
    let mut reloaded = PluginRegistry::open(paths.clone(), "0.2.0").unwrap();
    assert!(!reloaded.get("example.full").unwrap().enabled);
    reloaded.enable("example.full").unwrap();
    assert!(PluginRegistry::open(paths.clone(), "0.2.0").unwrap().get("example.full").unwrap().enabled);

    fs::remove_file(&manifest_path).unwrap();
    let mut missing = PluginRegistry::open(paths.clone(), "0.2.0").unwrap();
    let entry = missing.get("example.full").unwrap();
    assert!(entry.manifest.is_none());
    assert!(entry.warnings.iter().any(|warning| warning.contains("manifest could not be loaded")));

    missing.unlink("example.full").unwrap();
    assert!(PluginRegistry::open(paths, "0.2.0").unwrap().entries().is_empty());
}

fn runtime_manifest() -> PluginManifest {
    let capture_command = if cfg!(windows) {
        vec![
            "cmd.exe".to_owned(),
            "/D".to_owned(),
            "/S".to_owned(),
            "/C".to_owned(),
            "set STARCIL_>captured-env.txt & echo tail-marker 1>&2 & exit /B 0".to_owned(),
        ]
    } else {
        vec!["sh".to_owned(), "-c".to_owned(), "env | grep '^STARCIL_' > captured-env.txt; echo tail-marker >&2".to_owned()]
    };
    PluginManifest {
        id: "example.runtime".to_owned(),
        name: "Runtime".to_owned(),
        version: "0.1.0".to_owned(),
        min_starcil_version: "0.1.0".to_owned(),
        platforms: Some(Platform::ALL.to_vec()),
        actions: vec![
            ActionSpec { id: "capture".into(), title: "Capture".into(), contexts: vec![], command: capture_command, platforms: None },
            ActionSpec { id: "linux-only".into(), title: "Linux".into(), contexts: vec![], command: vec!["noop".into()], platforms: Some(vec![Platform::Linux]) },
        ],
        events: vec![EventHookSpec { on: "worktree.created".into(), command: vec!["event-helper".into()], platforms: Some(vec![Platform::Windows]) }],
        panes: vec![PaneSpec {
            id: "picker".into(),
            title: "Picker".into(),
            command: vec!["picker".into()],
            placement: PanePlacement::Overlay,
            width: None,
            height: None,
            platforms: None,
        }],
        ..PluginManifest::default()
    }
}

#[test]
fn action_invoke_injects_env_logs_exit_and_enforces_gates() {
    let temp = TestDir::new("action");
    let plugin_root = temp.path().join("plugin");
    write_manifest(&plugin_root, &runtime_manifest());
    let paths = registry_paths(temp.path());
    let mut registry = PluginRegistry::open(paths, "0.1.0").unwrap();
    registry.link(&plugin_root, true, None).unwrap();

    let logs = LogStore::new(8, 1024);
    let executor = PluginExecutor::new(
        HostEnvironment::new(r"\\.\pipe\starcil-test", temp.path().join("starcil.exe"), Platform::Windows),
        logs.clone(),
    );
    let active = ActiveContext {
        workspace_id: Some("w1".into()),
        tab_id: Some("w1:t2".into()),
        pane_id: Some("w1:p3".into()),
        worktree: Some(json!({"branch": "feat/api", "path": "C:/dev/api"})),
        request_id: Some("req-action".into()),
    };
    let invocation = executor
        .invoke_action(
            &registry,
            "example.runtime.capture",
            Some(json!({"invocation_source": "keybinding", "workspace_id": "override"})),
            &active,
        )
        .unwrap();
    assert_eq!(invocation.context["workspace_id"], "override");
    assert_eq!(invocation.context["tab_id"], "w1:t2");
    assert_eq!(invocation.context["request_id"], "req-action");
    assert_eq!(invocation.log.state, CommandState::Running);

    let env_path = plugin_root.join("captured-env.txt");
    wait_until(Duration::from_secs(5), || env_path.is_file());
    wait_until(Duration::from_secs(5), || {
        logs.list(Some("example.runtime"), Some(1)).unwrap().first().is_some_and(|log| log.state == CommandState::Exited)
    });
    let captured = fs::read_to_string(env_path).unwrap();
    for expected in [
        "STARCIL_ENV=1",
        "STARCIL_PLUGIN_ID=example.runtime",
        "STARCIL_PLUGIN_ACTION_ID=example.runtime.capture",
        "STARCIL_WORKSPACE_ID=override",
        "STARCIL_TAB_ID=w1:t2",
        "STARCIL_PANE_ID=w1:p3",
    ] {
        assert!(captured.contains(expected), "missing {expected} in {captured}");
    }
    let exited = logs.list(Some("example.runtime"), Some(1)).unwrap().remove(0);
    assert_eq!(exited.exit_code, Some(0));
    assert!(exited.stderr_tail.contains("tail-marker"));

    registry.disable("example.runtime").unwrap();
    let disabled = executor.invoke_action(&registry, "example.runtime.capture", None, &active).unwrap_err();
    assert!(matches!(disabled, PluginError::PluginDisabled(_)));
    let disabled_api: ApiError = disabled.into();
    assert_eq!(disabled_api.code, ErrorCode::PluginDisabled);

    registry.enable("example.runtime").unwrap();
    let gated = executor.invoke_action(&registry, "example.runtime.linux-only", None, &active).unwrap_err();
    assert!(matches!(gated, PluginError::PlatformUnsupported { .. }));
    let gated_api: ApiError = gated.into();
    assert_eq!(gated_api.code, ErrorCode::PlatformUnsupported);
}

#[test]
fn event_resolution_and_pane_preparation_build_authoritative_env() {
    let temp = TestDir::new("events");
    let plugin_root = temp.path().join("plugin");
    write_manifest(&plugin_root, &runtime_manifest());
    let mut registry = PluginRegistry::open(registry_paths(temp.path()), "0.1.0").unwrap();
    registry.link(&plugin_root, true, None).unwrap();
    let executor = PluginExecutor::new(
        HostEnvironment::new("socket", temp.path().join("starcil"), Platform::Windows),
        LogStore::default(),
    );
    let active = ActiveContext {
        workspace_id: Some("w1".into()),
        tab_id: Some("w1:t1".into()),
        pane_id: Some("w1:p1".into()),
        request_id: Some("req-event".into()),
        ..ActiveContext::default()
    };
    let event = json!({"workspace": {"workspace_id": "w2"}});
    let commands = executor.resolve_event_hooks(&registry, "worktree.created", &event, &active).unwrap();
    assert_eq!(commands.len(), 1);
    assert_eq!(commands[0].env["STARCIL_PLUGIN_EVENT"], "worktree.created");
    assert_eq!(commands[0].env["STARCIL_PLUGIN_EVENT_JSON"], serde_json::to_string(&event).unwrap());
    assert_eq!(commands[0].env["STARCIL_WORKSPACE_ID"], "w1");
    assert!(executor.resolve_event_hooks(&registry, "pane.created", &json!({}), &active).unwrap().is_empty());

    let pane = executor
        .prepare_pane(
            &registry,
            "example.runtime",
            "picker",
            PaneOpenOptions {
                placement: Some(PanePlacement::Popup),
                width: Some(PaneDimension::Percent("75%".into())),
                env: [("STARCIL_PLUGIN_ID".to_owned(), "attacker".to_owned())].into_iter().collect(),
                ..PaneOpenOptions::default()
            },
            &active,
        )
        .unwrap();
    assert_eq!(pane.placement, PanePlacement::Popup);
    assert_eq!(pane.width, Some(PaneDimension::Percent("75%".into())));
    assert_eq!(pane.command.env["STARCIL_PLUGIN_ID"], "example.runtime");
    assert_eq!(pane.command.env["STARCIL_PLUGIN_ENTRYPOINT_ID"], "picker");
    assert!(!pane.command.env.contains_key("STARCIL_PANE_ID"));
    assert_eq!(pane.command.context["pane_id"], "w1:p1");

    registry.disable("example.runtime").unwrap();
    assert!(executor.resolve_event_hooks(&registry, "worktree.created", &event, &active).unwrap().is_empty());
}

fn wait_until(timeout: Duration, condition: impl Fn() -> bool) {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if condition() {
            return;
        }
        thread::sleep(Duration::from_millis(20));
    }
    panic!("condition was not met within {timeout:?}");
}
