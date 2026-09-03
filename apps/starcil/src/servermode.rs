//! Server and client launch modes for the starcil binary.

use starcil_cli::LaunchClient;
use starcil_server::actor::{run_server, SharedServer};
use starcil_server::ServerCore;

/// Run the headless session server in the foreground.
pub fn run(session: Option<String>) -> i32 {
    let session = session.unwrap_or_else(|| "default".to_string());
    let cwd = std::env::current_dir()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| ".".to_string());

    init_logging(&session);

    // Load config first: it decides the default shell and scrollback budget.
    let path = starcil_config::config_path()
        .or_else(starcil_config::default_config_path)
        .unwrap_or_default();
    let report = starcil_config::load(&path);
    let default_shell = {
        let s = report.config.terminal.default_shell.trim();
        if s.is_empty() { None } else { Some(s.to_string()) }
    };
    let scrollback = report.config.advanced.scrollback_limit_bytes as usize;

    let host = starcil_host::RealHost::new(default_shell, scrollback);
    let mut core = match ServerCore::new(&session, &cwd, host) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{}", serde_json::json!({"error": e}));
            return 1;
        }
    };
    core.apply_config(&report.config);
    // Register the session on disk so `session list` can discover it.
    if let Ok(paths) = starcil_platform::PlatformPaths::discover() {
        if let Ok(dir) = paths.session_runtime_dir(&session) {
            let _ = std::fs::create_dir_all(&dir);
        }
    }
    // Plugin host: registry next to per-session state, data under the data dir.
    if let Ok(paths) = starcil_platform::PlatformPaths::discover() {
        let data = paths.data_dir().to_path_buf();
        let _ = std::fs::create_dir_all(&data);
        let registry_file = data.join(format!("plugins-{session}.json"));
        let socket = starcil_cli::endpoint_for(&starcil_cli::EndpointSelection { session: Some(session.clone()) });
        let bin = std::env::current_exe().unwrap_or_default();
        if let Err(e) = core.init_plugins(registry_file, data.join("plugins"), socket.display().to_string(), bin) {
            tracing::warn!(error = %e, "plugin host init failed");
        }
        core.run_plugin_startup_hooks();
    }

    // Restore the previous session state (native agent resume honored).
    let mut persistence_state: Option<starcil_server::persistence::PersistenceState> = None;
    if let Ok(paths) = starcil_platform::PlatformPaths::discover() {
        if let Ok(state) = starcil_server::persistence::PersistenceState::for_session(&paths, &session) {
            if state.path.exists() {
                match starcil_persist::load(&state.path) {
                    Ok(outcome) => {
                        let resume = report.config.session.resume_agents_on_restore;
                        let restore = starcil_server::persistence::restore_at_boot(&mut core, outcome.doc, resume);
                        tracing::info!(
                            workspaces = restore.restored_workspaces,
                            panes = restore.restored_panes,
                            rebound_sessions = restore.rebound_sessions,
                            could_not_resume = restore.could_not_resume.len(),
                            launch_failures = restore.launch_failures.len(),
                            "session state restored"
                        );
                    }
                    Err(e) => tracing::warn!(error = %e, "session state load failed; starting fresh"),
                }
            }
            persistence_state = Some(state);
        }
    }

    let shared = SharedServer::new(core);
    if let Some(state) = persistence_state {
        *shared.persistence.lock().unwrap() = Some(state);
    }
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    match runtime.block_on(run_server(shared, &session)) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("{{\"error\":{{\"code\":\"internal\",\"message\":\"{e}\"}}}}");
            1
        }
    }
}

/// Bare launch / --session / --remote / --no-session: attach the full TUI,
/// autostarting the server when needed.
pub fn launch_client(launch: &LaunchClient) -> i32 {
    // Nested clients corrupt the outer workspace (the inner multiplexer
    // fights the outer one for the same PTY). Refused by default;
    // `experimental.allow_nested = true` overrides.
    if nested_launch_refused(
        std::env::var("STARCIL_ENV").ok().as_deref(),
        nested_launch_allowed_by_config(),
    ) {
        eprintln!(
            "starcil: refusing to launch inside a starcil-managed pane — nested sessions corrupt the outer workspace."
        );
        eprintln!(
            "starcil: set `experimental.allow_nested = true` in your config to override."
        );
        return 1;
    }
    if let Some(target) = &launch.remote {
        return crate::clientloop::run_remote(target, launch.session.as_deref());
    }
    let session = launch.session.clone().unwrap_or_else(|| "default".to_string());
    if !launch.no_session {
        if let Err(e) = ensure_server(&session) {
            eprintln!("starcil: could not start the session server: {e}");
            return 1;
        }
    }
    starcil_tui_entry(&session, launch)
}

/// A client launch is refused when the process runs inside a starcil pane
/// (`STARCIL_ENV=1`) and the config does not opt into nesting.
fn nested_launch_refused(starcil_env: Option<&str>, allow_nested: bool) -> bool {
    starcil_env == Some("1") && !allow_nested
}

fn nested_launch_allowed_by_config() -> bool {
    let path = starcil_config::config_path().or_else(starcil_config::default_config_path);
    let report = path
        .as_deref()
        .map(starcil_config::load)
        .unwrap_or_else(|| starcil_config::parse_config(""));
    report.config.experimental.allow_nested
}

/// Spawn `starcil server --session <s>` detached if the endpoint is not
/// answering, then wait for it to become reachable.
fn ensure_server(session: &str) -> Result<(), String> {
    use starcil_cli::{endpoint_for, EndpointSelection};
    let selection = EndpointSelection { session: Some(session.to_string()) };
    let endpoint = endpoint_for(&selection);
    if endpoint_reachable(&endpoint) {
        return Ok(());
    }
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let mut cmd = std::process::Command::new(exe);
    // Global --session comes before the subcommand in this grammar.
    cmd.arg("--session").arg(session).arg("server");
    starcil_platform::spawn_detached(&mut cmd).map_err(|e| e.to_string())?;
    // Wait up to 5s for the socket.
    for _ in 0..50 {
        std::thread::sleep(std::time::Duration::from_millis(100));
        if endpoint_reachable(&endpoint) {
            return Ok(());
        }
    }
    Err("server did not become reachable within 5s".into())
}

/// A named pipe accepts an open when a server owns it. "All pipe instances
/// busy" (os error 231) also proves a live listener.
#[cfg(windows)]
fn endpoint_reachable(endpoint: &std::path::Path) -> bool {
    match std::fs::OpenOptions::new().read(true).write(true).open(endpoint) {
        Ok(_) => true,
        Err(e) => e.raw_os_error() == Some(231),
    }
}

/// A Unix socket cannot be `open()`ed like a file (ENXIO): only a connect
/// proves a live listener. A stale socket file refuses the connection.
#[cfg(unix)]
fn endpoint_reachable(endpoint: &std::path::Path) -> bool {
    std::os::unix::net::UnixStream::connect(endpoint).is_ok()
}

fn init_logging(session: &str) {
    if let Ok(paths) = starcil_platform::PlatformPaths::discover() {
        let dir = paths.log_dir().to_path_buf();
        let _ = std::fs::create_dir_all(&dir);
        let file = tracing_appender::rolling::never(dir, format!("starcil-server-{session}.log"));
        let _ = tracing_subscriber::fmt().with_writer(file).with_ansi(false).try_init();
    }
}

// ---- seams filled as fleet deliveries land ----

/// The full TUI client loop (real link over the socket).
fn starcil_tui_entry(session: &str, _launch: &LaunchClient) -> i32 {
    crate::clientloop::run(session)
}

#[cfg(test)]
mod tests {
    use super::nested_launch_refused;

    #[test]
    fn nested_launches_are_refused_unless_opted_in() {
        assert!(nested_launch_refused(Some("1"), false));
        assert!(!nested_launch_refused(Some("1"), true));
        assert!(!nested_launch_refused(None, false));
        assert!(!nested_launch_refused(Some("0"), false));
    }
}
