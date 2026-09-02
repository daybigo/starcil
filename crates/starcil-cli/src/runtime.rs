use crate::connection::{connect, Connection, EndpointSelection};
use crate::flows::{
    configured_channel, delete_session_directories, discover_sessions, perform_update,
    resolved_config_path, session_state_root, set_channel_at, UpdateFlowOutcome,
};
use crate::hooks::run_hook_with;
use crate::plugin_runtime::dispatch_plugin;
use crate::schema::{api_schema, method_groups};
use crate::terminal::run_terminal;
use crate::{
    completion_script, parse, Behavior, ChannelAction, ConfigAction, OutputMode, SchemaOutput,
    SessionAction,
};
use semver::Version;
use serde_json::{json, Value};
use starcil_protocol::{Incoming, Request};
use starcil_update::{Channel, Platform, UpdateConfig, Updater, UreqHttpClient};
use std::io::{self, Read};

pub fn dispatch(args: &[String]) -> i32 {
    dispatch_with(args, connect)
}

pub fn dispatch_with<F>(args: &[String], mut connector: F) -> i32
where
    F: FnMut(&EndpointSelection) -> io::Result<Box<dyn Connection>>,
{
    let invocation = match parse(args) {
        Ok(invocation) => invocation,
        Err(error) => {
            eprintln!("{error}");
            return 2;
        }
    };

    match invocation.behavior.clone() {
        Behavior::Help(help) => {
            print!("{help}");
            0
        }
        Behavior::Version => {
            println!("starcil {}", env!("CARGO_PKG_VERSION"));
            0
        }
        Behavior::Completion(shell) => {
            print!("{}", completion_script(shell));
            0
        }
        Behavior::DefaultConfig => {
            print!("{}", starcil_config::default_config_template());
            0
        }
        Behavior::Skill => {
            print!("{}", crate::BUNDLED_SKILL);
            0
        }
        Behavior::ApiSchema(output) => emit_schema(&invocation.request_id, output),
        Behavior::LaunchClient(_) => {
            eprintln!("starcil: TUI client not wired yet");
            3
        }
        Behavior::LaunchServer { .. } => not_wired("server launch"),
        Behavior::Terminal { session, action } => {
            match run_terminal(session, action, &mut connector) {
                Ok(()) => 0,
                Err(error) => local_io_error(&error),
            }
        }
        Behavior::Update(action) => run_update(action.handoff),
        Behavior::Config(action) => run_config(action),
        Behavior::Channel(action) => run_channel(action),
        Behavior::Session(action) => run_session(action, &mut connector),
        Behavior::Plugin { session, action } => {
            let selection = EndpointSelection { session: session.clone() };
            let connection = connector(&selection).ok();
            dispatch_plugin(action, session, connection)
        }
        Behavior::IntegrationHook { session, action } => {
            run_integration_hook(session, &action, &mut connector)
        }
        Behavior::Socket { session, output } => {
            let selection = EndpointSelection { session };
            let mut connection = match connector(&selection) {
                Ok(connection) => connection,
                Err(error) => return server_unavailable(&error),
            };
            let mut params = invocation.params;
            if let Err(error) = expand_worktree_paths(&invocation.method, &mut params) {
                return local_io_error(&error);
            }
            let request = Request::new(invocation.request_id, invocation.method, params);
            match connection.call(&request) {
                Ok(Incoming::Success(response)) => emit_success(response, output),
                Ok(Incoming::Error(response)) => {
                    eprintln!("{}", serde_json::to_string(&response).expect("error response serialization cannot fail"));
                    1
                }
                Ok(Incoming::Event(_)) => protocol_error("connection returned an event instead of the matching response"),
                Err(error) => server_unavailable(&error),
            }
        }
    }
}

fn run_config(action: ConfigAction) -> i32 {
    let path = match resolved_config_path() {
        Ok(path) => path,
        Err(error) => return local_io_error(&error),
    };
    match action {
        ConfigAction::Check => {
            let diagnostics = starcil_config::check(&path);
            println!("Checking {}", path.display());
            for diagnostic in &diagnostics {
                let severity = match diagnostic.severity {
                    starcil_config::Severity::Warning => "warning",
                    starcil_config::Severity::Error => "error",
                };
                println!("{severity}: {}: {}", diagnostic.toml_path(), diagnostic.message);
            }
            let errors = diagnostics
                .iter()
                .filter(|diagnostic| diagnostic.severity == starcil_config::Severity::Error)
                .count();
            if errors == 0 {
                println!("Configuration is valid.");
                0
            } else {
                println!("Configuration has {errors} error(s).");
                1
            }
        }
        ConfigAction::ResetKeys => match starcil_config::reset_keys(&path) {
            Ok(()) => {
                println!(
                    "Backed up {} to {} and removed custom keybindings.",
                    path.display(),
                    starcil_config::backup_path(&path).display()
                );
                0
            }
            Err(error) => {
                eprintln!("starcil: {error}");
                1
            }
        },
    }
}

fn run_channel(action: ChannelAction) -> i32 {
    let path = match resolved_config_path() {
        Ok(path) => path,
        Err(error) => return local_io_error(&error),
    };
    match action {
        ChannelAction::Show => match configured_channel(&path) {
            Ok(channel) => {
                println!("{channel}");
                0
            }
            Err(error) => local_io_error(&error),
        },
        ChannelAction::Set(channel) => {
            let channel = match channel.parse::<Channel>() {
                Ok(channel) => channel,
                Err(error) => {
                    eprintln!("starcil: {error}");
                    return 2;
                }
            };
            match set_channel_at(&path, channel) {
                Ok(()) => {
                    println!("Update channel set to {channel}.");
                    0
                }
                Err(error) => local_io_error(&error),
            }
        }
    }
}

fn run_session<F>(action: SessionAction, connector: &mut F) -> i32
where
    F: FnMut(&EndpointSelection) -> io::Result<Box<dyn Connection>>,
{
    let paths = match starcil_platform::PlatformPaths::discover() {
        Ok(paths) => paths,
        Err(error) => {
            eprintln!("starcil: {error}");
            return 1;
        }
    };
    let state_root = session_state_root(&paths);
    let runtime_root = paths.runtime_dir().to_path_buf();
    match action {
        SessionAction::List { json: json_output } => {
            let sessions = match discover_sessions(&state_root, &runtime_root, |name| {
                connector(&EndpointSelection { session: Some(name.to_owned()) }).is_ok()
            }) {
                Ok(sessions) => sessions,
                Err(error) => return local_io_error(&error),
            };
            if json_output {
                println!("{}", json!({"sessions": sessions}));
            } else if sessions.is_empty() {
                println!("No sessions found.");
            } else {
                println!("SESSION\tSTATE\tSTATE DIR\tRUNTIME DIR");
                for session in sessions {
                    println!(
                        "{}\t{}\t{}\t{}",
                        session.name,
                        if session.running { "running" } else { "stopped" },
                        session.state_dir.as_deref().map(|path| path.display().to_string()).unwrap_or_else(|| "-".to_owned()),
                        session.runtime_dir.as_deref().map(|path| path.display().to_string()).unwrap_or_else(|| "-".to_owned()),
                    );
                }
            }
            0
        }
        SessionAction::Delete { name, json: json_output } => {
            if let Err(error) = paths.session_runtime_dir(&name) {
                eprintln!("starcil: {error}");
                return 1;
            }
            let running = connector(&EndpointSelection { session: Some(name.clone()) }).is_ok();
            match delete_session_directories(&state_root, &runtime_root, &name, running) {
                Ok(removed) => {
                    if json_output {
                        println!("{}", json!({"type": "session_deleted", "session": name, "removed": removed}));
                    } else {
                        println!("Deleted session '{name}'.");
                    }
                    0
                }
                Err(error) => {
                    if json_output {
                        eprintln!("{}", json!({"error": {"code": "session_delete_failed", "message": error.to_string()}}));
                    } else {
                        eprintln!("starcil: {error}");
                    }
                    1
                }
            }
        }
    }
}

fn run_update(handoff: bool) -> i32 {
    if handoff {
        println!("Live update handoff is not yet supported; continuing with the normal staged update.");
    }
    let config_path = match resolved_config_path() {
        Ok(path) => path,
        Err(error) => return local_io_error(&error),
    };
    let channel = match configured_channel(&config_path) {
        Ok(channel) => channel,
        Err(error) => return local_io_error(&error),
    };
    let paths = match starcil_platform::PlatformPaths::discover() {
        Ok(paths) => paths,
        Err(error) => {
            eprintln!("starcil: {error}");
            return 1;
        }
    };
    let Some(platform) = Platform::current() else {
        eprintln!("starcil: updates are not available for this platform");
        return 1;
    };
    let current_executable = match std::env::current_exe() {
        Ok(path) => path,
        Err(error) => return local_io_error(&error),
    };
    let current_version = match Version::parse(env!("CARGO_PKG_VERSION")) {
        Ok(version) => version,
        Err(error) => {
            eprintln!("starcil: invalid current version: {error}");
            return 1;
        }
    };
    let updater = Updater::new(
        UreqHttpClient,
        UpdateConfig::new(paths.data_dir(), current_executable, platform),
    );
    match perform_update(&updater, channel, &current_version) {
        Ok(UpdateFlowOutcome::NoUpdate) => {
            println!("Starcil is already up to date, or the update service is currently offline.");
            0
        }
        Ok(UpdateFlowOutcome::NeedsRestart { version, backup_executable }) => {
            println!("Updated Starcil to {version}.");
            println!("Restart Starcil to use the new version. Previous binary: {}", backup_executable.display());
            0
        }
        Err(error) => {
            eprintln!("starcil: update failed: {error}");
            1
        }
    }
}

fn run_integration_hook<F>(
    session: Option<String>,
    action: &crate::IntegrationHookAction,
    connector: &mut F,
) -> i32
where
    F: FnMut(&EndpointSelection) -> io::Result<Box<dyn Connection>>,
{
    let pane_id = std::env::var("STARCIL_PANE_ID").ok();
    let Some(pane_id) = pane_id.as_deref().filter(|value| !value.trim().is_empty()) else { return 0 };
    let selection = EndpointSelection { session };
    let Ok(mut connection) = connector(&selection) else { return 0 };
    let mut stdin_payload = String::new();
    if !matches!(action, crate::IntegrationHookAction::CodexNotify { .. }) {
        let _ = io::stdin().read_to_string(&mut stdin_payload);
    }
    run_hook_with(action, Some(pane_id), &stdin_payload, connection.as_mut());
    0
}

fn emit_success(response: starcil_protocol::SuccessResponse, output: OutputMode) -> i32 {
    match output {
        OutputMode::Json => println!("{}", serde_json::to_string(&response).expect("success response serialization cannot fail")),
        OutputMode::RawText => {
            if let Some(text) = text_payload(&response.result) {
                print!("{text}");
            } else {
                println!("{}", serde_json::to_string(&response).expect("success response serialization cannot fail"));
            }
        }
    }
    0
}

fn text_payload(result: &Value) -> Option<&str> {
    result
        .as_str()
        .or_else(|| result.get("text").and_then(Value::as_str))
        .or_else(|| result.get("content").and_then(Value::as_str))
        .or_else(|| result.get("output").and_then(Value::as_str))
        .or_else(|| result.get("data").and_then(|data| data.get("text")).and_then(Value::as_str))
}

fn emit_schema(request_id: &str, output: SchemaOutput) -> i32 {
    let schema = api_schema();
    match output {
        SchemaOutput::Text => {
            println!(
                "Starcil socket API {}.{} methods:",
                starcil_protocol::PROTOCOL_MAJOR,
                starcil_protocol::PROTOCOL_MINOR
            );
            for (group, methods) in method_groups() {
                println!("{group}:");
                for method in methods {
                    println!("  {method}");
                }
            }
            println!("events: {}", starcil_protocol::events::ALL.len());
            0
        }
        SchemaOutput::Json => {
            println!("{}", serde_json::to_string(&schema).expect("schema serialization cannot fail"));
            0
        }
        SchemaOutput::File(path) => match serde_json::to_vec_pretty(&schema)
            .map_err(io::Error::other)
            .and_then(|contents| std::fs::write(&path, contents))
        {
            Ok(()) => {
                println!("{}", json!({"id": request_id, "result": {"type": "api_schema_written", "path": path}}));
                0
            }
            Err(error) => {
                eprintln!("{}", json!({"error": {"code": "io_error", "message": error.to_string()}}));
                1
            }
        },
    }
}

fn expand_worktree_paths(method: &str, params: &mut Value) -> io::Result<()> {
    if !matches!(method, "worktree.list" | "worktree.create" | "worktree.open") {
        return Ok(());
    }
    let Some(object) = params.as_object_mut() else { return Ok(()) };
    for key in ["cwd", "path"] {
        let Some(value) = object.get_mut(key) else { continue };
        let Some(path) = value.as_str() else { continue };
        let path = std::path::Path::new(path);
        if path.is_relative() {
            *value = Value::String(std::env::current_dir()?.join(path).to_string_lossy().into_owned());
        }
    }
    Ok(())
}

fn server_unavailable(error: &io::Error) -> i32 {
    eprintln!("{}", json!({"error": {"code": "server_unavailable", "message": error.to_string()}}));
    1
}

fn protocol_error(message: &str) -> i32 {
    eprintln!("{}", json!({"error": {"code": "protocol_error", "message": message}}));
    1
}

fn local_io_error(error: &io::Error) -> i32 {
    eprintln!("{}", json!({"error": {"code": "io_error", "message": error.to_string()}}));
    1
}

fn not_wired(flow: &str) -> i32 {
    eprintln!("starcil: {flow} not wired yet");
    3
}
