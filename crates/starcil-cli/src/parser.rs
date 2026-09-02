use crate::help::{
    group_help, AGENT_HELP, API_HELP, CHANNEL_HELP, CONFIG_HELP, INTEGRATION_HELP,
    NOTIFICATION_HELP, PANE_HELP, PLUGIN_HELP, ROOT_HELP, SERVER_HELP, SESSION_HELP, TAB_HELP,
    TERMINAL_HELP, WORKSPACE_HELP, WORKTREE_HELP,
};
use serde_json::{Map, Number, Value};
use std::fmt;
use std::path::Path;

#[derive(Debug, Clone, PartialEq)]
pub struct Invocation {
    pub request_id: String,
    pub method: String,
    pub params: Value,
    pub behavior: Behavior,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OutputMode {
    Json,
    RawText,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompletionShell {
    Zsh,
    Bash,
    PowerShell,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LaunchClient {
    pub session: Option<String>,
    pub remote: Option<String>,
    pub remote_keybindings: Option<String>,
    pub handoff: bool,
    pub no_session: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SchemaOutput {
    Text,
    Json,
    File(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateAction {
    pub handoff: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigAction {
    Check,
    ResetKeys,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChannelAction {
    Show,
    Set(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionAction {
    List { json: bool },
    Delete { name: String, json: bool },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GithubSlug {
    pub owner: String,
    pub repo: String,
    pub subdir: Option<String>,
}

impl GithubSlug {
    pub fn as_str(&self) -> String {
        match &self.subdir {
            Some(subdir) => format!("{}/{}/{}", self.owner, self.repo, subdir),
            None => format!("{}/{}", self.owner, self.repo),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PluginTarget {
    PluginId(String),
    Github(GithubSlug),
}

#[derive(Debug, Clone, PartialEq)]
pub enum PluginAction {
    Install { source: GithubSlug, requested_ref: Option<String>, yes: bool },
    Uninstall { target: PluginTarget },
    Link { path: String, disabled: bool },
    List { plugin_id: Option<String>, json: bool },
    ConfigDir { plugin_id: String },
    Unlink { plugin_id: String },
    Enable { plugin_id: String },
    Disable { plugin_id: String },
    ActionList { plugin_id: Option<String> },
    ActionInvoke { action_id: String, plugin_id: Option<String> },
    LogList { plugin_id: Option<String>, limit: Option<u64> },
    PaneOpen { params: Value },
    PaneFocus { pane_id: String },
    PaneClose { pane_id: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IntegrationHookAction {
    ClaudeNotification,
    ClaudeStop,
    ClaudeSessionStart,
    CodexNotify { payload: Option<String> },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TerminalAction {
    Attach { terminal_id: String, takeover: bool },
    AgentAttach { target: String, takeover: bool },
    Control { target: String, takeover: bool, cols: Option<u16>, rows: Option<u16> },
    Observe { target: String, cols: Option<u16>, rows: Option<u16> },
}

#[derive(Debug, Clone, PartialEq)]
pub enum Behavior {
    Help(&'static str),
    Version,
    Completion(CompletionShell),
    DefaultConfig,
    /// Print the bundled agent skill (`skills/starcil/SKILL.md`).
    Skill,
    Socket { session: Option<String>, output: OutputMode },
    LaunchClient(LaunchClient),
    LaunchServer { session: Option<String> },
    ApiSchema(SchemaOutput),
    Update(UpdateAction),
    Config(ConfigAction),
    Channel(ChannelAction),
    Session(SessionAction),
    Plugin { session: Option<String>, action: PluginAction },
    IntegrationHook { session: Option<String>, action: IntegrationHookAction },
    Terminal { session: Option<String>, action: TerminalAction },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CliError {
    pub message: String,
    pub usage: &'static str,
}

impl CliError {
    fn new(message: impl Into<String>, usage: &'static str) -> Self {
        Self { message: message.into(), usage }
    }
}

impl fmt::Display for CliError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "starcil: {}\n{}", self.message, self.usage)
    }
}

impl std::error::Error for CliError {}

#[derive(Debug, Default)]
struct Globals {
    session: Option<String>,
    remote: Option<String>,
    remote_keybindings: Option<String>,
    handoff: bool,
    no_session: bool,
}

pub fn parse(args: &[String]) -> Result<Invocation, CliError> {
    let args = strip_binary_name(args);
    let (globals, rest) = parse_globals(args)?;

    if rest.is_empty() {
        validate_launch_globals(&globals)?;
        return Ok(local_invocation(
            "root",
            "launch",
            "local.launch_client",
            Behavior::LaunchClient(LaunchClient {
                session: globals.session,
                remote: globals.remote,
                remote_keybindings: globals.remote_keybindings,
                handoff: globals.handoff,
                no_session: globals.no_session,
            }),
        ));
    }

    if globals.remote.is_some() || globals.remote_keybindings.is_some() || globals.no_session || globals.handoff {
        return Err(CliError::new("launch-only options cannot be combined with a command", ROOT_HELP));
    }

    let command = rest[0].as_str();
    let tail = &rest[1..];
    if tail.len() == 1 && matches!(tail[0].as_str(), "--help" | "-h") {
        if let Some(help) = group_help(command) {
            return Ok(help_invocation(command, help));
        }
        if command == "server" {
            return Ok(help_invocation("server", SERVER_HELP));
        }
    }
    match command {
        "--help" | "-h" => {
            no_args(tail, ROOT_HELP)?;
            Ok(help_invocation("root", ROOT_HELP))
        }
        "--version" | "-V" => {
            no_args(tail, ROOT_HELP)?;
            Ok(local_invocation("root", "version", "local.version", Behavior::Version))
        }
        "--default-config" => {
            no_args(tail, ROOT_HELP)?;
            Ok(local_invocation("root", "default-config", "local.default_config", Behavior::DefaultConfig))
        }
        "--skill" => {
            no_args(tail, ROOT_HELP)?;
            Ok(local_invocation("root", "skill", "local.skill", Behavior::Skill))
        }
        "status" => parse_status(tail, globals.session),
        "update" => parse_update(tail),
        "server" => parse_server(tail, globals.session),
        "completion" | "completions" => parse_completion(tail),
        "agent" => parse_agent(tail, globals.session),
        "pane" => parse_pane(tail, globals.session),
        "workspace" => parse_workspace(tail, globals.session),
        "tab" => parse_tab(tail, globals.session),
        "worktree" => parse_worktree(tail, globals.session),
        "terminal" => parse_terminal(tail, globals.session),
        "notification" => parse_notification(tail, globals.session),
        "integration" => parse_integration(tail, globals.session),
        "session" => parse_session(tail),
        "api" => parse_api(tail, globals.session),
        "config" => parse_config(tail),
        "channel" => parse_channel(tail),
        "plugin" => parse_plugin(tail, globals.session),
        value if value.starts_with('-') => Err(CliError::new(format!("unknown flag '{value}'"), ROOT_HELP)),
        value => Err(CliError::new(format!("unknown command '{value}'"), ROOT_HELP)),
    }
}

fn strip_binary_name(args: &[String]) -> &[String] {
    let Some(first) = args.first() else { return args };
    let stem = Path::new(first).file_stem().and_then(|value| value.to_str());
    if stem.is_some_and(|value| value.eq_ignore_ascii_case("starcil")) { &args[1..] } else { args }
}

fn parse_globals(args: &[String]) -> Result<(Globals, &[String]), CliError> {
    let mut globals = Globals::default();
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--session" => {
                set_once(&mut globals.session, flag_value(args, &mut index, "--session", ROOT_HELP)?, "--session", ROOT_HELP)?;
            }
            "--remote" => {
                set_once(&mut globals.remote, flag_value(args, &mut index, "--remote", ROOT_HELP)?, "--remote", ROOT_HELP)?;
            }
            "--remote-keybindings" => {
                let value = flag_value(args, &mut index, "--remote-keybindings", ROOT_HELP)?;
                choice(&value, &["local", "server"], "--remote-keybindings", ROOT_HELP)?;
                set_once(&mut globals.remote_keybindings, value, "--remote-keybindings", ROOT_HELP)?;
            }
            "--handoff" => {
                duplicate_bool(globals.handoff, "--handoff", ROOT_HELP)?;
                globals.handoff = true;
                index += 1;
            }
            "--no-session" => {
                duplicate_bool(globals.no_session, "--no-session", ROOT_HELP)?;
                globals.no_session = true;
                index += 1;
            }
            _ => break,
        }
    }
    Ok((globals, &args[index..]))
}

fn validate_launch_globals(globals: &Globals) -> Result<(), CliError> {
    if globals.no_session && (globals.session.is_some() || globals.remote.is_some()) {
        return Err(CliError::new("--no-session conflicts with --session and --remote", ROOT_HELP));
    }
    if globals.remote_keybindings.is_some() && globals.remote.is_none() {
        return Err(CliError::new("--remote-keybindings requires --remote", ROOT_HELP));
    }
    Ok(())
}

fn parse_status(args: &[String], session: Option<String>) -> Result<Invocation, CliError> {
    if args.len() > 1 {
        return Err(CliError::new("status accepts at most one of server|client", ROOT_HELP));
    }
    let mut params = Map::new();
    if let Some(component) = args.first() {
        choice(component, &["server", "client"], "status target", ROOT_HELP)?;
        params.insert("component".into(), Value::String(component.clone()));
    }
    Ok(socket_invocation("root", "status", "ping", params, session, OutputMode::Json))
}

fn parse_update(args: &[String]) -> Result<Invocation, CliError> {
    let handoff = match args {
        [] => false,
        [flag] if flag == "--handoff" => true,
        [flag] if flag.starts_with('-') => return Err(CliError::new(format!("unknown flag '{flag}'"), ROOT_HELP)),
        _ => return Err(CliError::new("update accepts only --handoff", ROOT_HELP)),
    };
    Ok(local_invocation("root", "update", "local.update", Behavior::Update(UpdateAction { handoff })))
}

fn parse_server(args: &[String], session: Option<String>) -> Result<Invocation, CliError> {
    let Some(command) = args.first() else {
        return Ok(local_invocation("server", "launch", "local.launch_server", Behavior::LaunchServer { session }));
    };
    no_args(&args[1..], SERVER_HELP)?;
    match command.as_str() {
        "stop" => Ok(socket_invocation("server", "stop", "server.stop", Map::new(), session, OutputMode::Json)),
        "reload-config" => Ok(socket_invocation("server", "reload-config", "server.reload_config", Map::new(), session, OutputMode::Json)),
        value => Err(CliError::new(format!("unknown server command '{value}'"), SERVER_HELP)),
    }
}

fn parse_completion(args: &[String]) -> Result<Invocation, CliError> {
    if args.len() != 1 {
        return Err(CliError::new("completion requires one shell: zsh, bash, or powershell", ROOT_HELP));
    }
    let shell = match args[0].as_str() {
        "zsh" => CompletionShell::Zsh,
        "bash" => CompletionShell::Bash,
        "powershell" => CompletionShell::PowerShell,
        value => return Err(CliError::new(format!("unsupported completion shell '{value}'"), ROOT_HELP)),
    };
    Ok(local_invocation("root", "completion", "local.completion", Behavior::Completion(shell)))
}

fn parse_agent(args: &[String], session: Option<String>) -> Result<Invocation, CliError> {
    let Some(command) = args.first() else { return Ok(help_invocation("agent", AGENT_HELP)) };
    let tail = &args[1..];
    match command.as_str() {
        "list" => {
            no_args(tail, AGENT_HELP)?;
            Ok(socket_invocation("agent", "list", "agent.list", Map::new(), session, OutputMode::Json))
        }
        "get" => one_positional("agent", "get", "agent.get", "target", tail, session, AGENT_HELP),
        "read" => parse_read("agent", tail, session),
        "send-keys" => parse_send_keys("agent", tail, session),
        "prompt" => parse_agent_prompt(tail, session),
        "rename" => parse_agent_rename(tail, session),
        "focus" => one_positional("agent", "focus", "agent.focus", "target", tail, session, AGENT_HELP),
        "wait" => parse_agent_wait(tail, session),
        "attach" => parse_agent_attach(tail, session),
        "start" => parse_agent_start(tail, session),
        "explain" => parse_agent_explain(tail, session),
        value => Err(CliError::new(format!("unknown agent command '{value}'"), AGENT_HELP)),
    }
}

fn parse_read(group: &'static str, args: &[String], session: Option<String>) -> Result<Invocation, CliError> {
    let usage = if group == "agent" { AGENT_HELP } else { PANE_HELP };
    let id_key = if group == "agent" { "target" } else { "pane_id" };
    let method = if group == "agent" { "agent.read" } else { "pane.read" };
    let target = required_positional(args.first(), id_key, usage)?;
    let mut params = Map::new();
    params.insert(id_key.into(), Value::String(target));
    let mut format = None;
    let mut index = 1;
    while index < args.len() {
        match args[index].as_str() {
            "--source" => {
                let value = flag_value(args, &mut index, "--source", usage)?;
                let sources: &[&str] = if group == "agent" { &["visible", "recent", "recent-unwrapped", "detection"] } else { &["visible", "recent", "recent-unwrapped"] };
                choice(&value, sources, "--source", usage)?;
                insert_once(&mut params, "source", Value::String(value), usage)?;
            }
            "--lines" => {
                let value = positive_u64(&flag_value(args, &mut index, "--lines", usage)?, "--lines", usage)?;
                insert_once(&mut params, "lines", Value::Number(value.into()), usage)?;
            }
            "--format" => {
                let value = flag_value(args, &mut index, "--format", usage)?;
                choice(&value, &["text", "ansi"], "--format", usage)?;
                if format.replace(value.clone()).is_some() {
                    return Err(CliError::new("--format may only be specified once", usage));
                }
                params.insert("format".into(), Value::String(value));
            }
            "--ansi" => {
                insert_once(&mut params, "ansi", Value::Bool(true), usage)?;
                index += 1;
            }
            "--raw" if group == "pane" => {
                insert_once(&mut params, "raw", Value::Bool(true), usage)?;
                index += 1;
            }
            flag if flag.starts_with('-') => return Err(CliError::new(format!("unknown flag '{flag}'"), usage)),
            value => return Err(CliError::new(format!("unexpected argument '{value}'"), usage)),
        }
    }
    let output = if format.as_deref() == Some("ansi") { OutputMode::Json } else { OutputMode::RawText };
    Ok(socket_invocation(group, "read", method, params, session, output))
}

fn parse_send_keys(group: &'static str, args: &[String], session: Option<String>) -> Result<Invocation, CliError> {
    let usage = if group == "agent" { AGENT_HELP } else { PANE_HELP };
    if args.len() < 2 {
        return Err(CliError::new(format!("{group} send-keys requires a target and at least one key"), usage));
    }
    let key_name = if group == "agent" { "target" } else { "pane_id" };
    let method = if group == "agent" { "agent.send_keys" } else { "pane.send_keys" };
    for key in &args[1..] {
        validate_key(key, usage)?;
    }
    let mut params = Map::new();
    params.insert(key_name.into(), Value::String(args[0].clone()));
    params.insert("keys".into(), Value::Array(args[1..].iter().cloned().map(Value::String).collect()));
    Ok(socket_invocation(group, "send-keys", method, params, session, OutputMode::Json))
}

fn parse_agent_prompt(args: &[String], session: Option<String>) -> Result<Invocation, CliError> {
    if args.len() < 2 {
        return Err(CliError::new("agent prompt requires <target> <text>", AGENT_HELP));
    }
    let mut params = Map::new();
    params.insert("target".into(), Value::String(args[0].clone()));
    params.insert("text".into(), Value::String(args[1].clone()));
    let mut wait = false;
    let mut until = Vec::new();
    let mut timeout = None;
    let mut index = 2;
    while index < args.len() {
        match args[index].as_str() {
            "--wait" => {
                duplicate_bool(wait, "--wait", AGENT_HELP)?;
                wait = true;
                index += 1;
            }
            "--until" => {
                let value = flag_value(args, &mut index, "--until", AGENT_HELP)?;
                agent_status(&value, AGENT_HELP)?;
                until.push(Value::String(value));
            }
            "--timeout" => {
                let value = positive_u64(&flag_value(args, &mut index, "--timeout", AGENT_HELP)?, "--timeout", AGENT_HELP)?;
                if timeout.replace(value).is_some() {
                    return Err(CliError::new("--timeout may only be specified once", AGENT_HELP));
                }
            }
            flag if flag.starts_with('-') => return Err(CliError::new(format!("unknown flag '{flag}'"), AGENT_HELP)),
            value => return Err(CliError::new(format!("unexpected argument '{value}'"), AGENT_HELP)),
        }
    }
    if wait || !until.is_empty() || timeout.is_some() {
        let mut wait_params = Map::new();
        if !until.is_empty() {
            wait_params.insert("until".into(), Value::Array(until));
        }
        if let Some(value) = timeout {
            wait_params.insert("timeout_ms".into(), Value::Number(value.into()));
        }
        params.insert("wait".into(), Value::Object(wait_params));
    }
    Ok(socket_invocation("agent", "prompt", "agent.prompt", params, session, OutputMode::Json))
}

fn parse_agent_rename(args: &[String], session: Option<String>) -> Result<Invocation, CliError> {
    let target = required_positional(args.first(), "target", AGENT_HELP)?;
    if args.len() != 2 {
        return Err(CliError::new("agent rename requires <target> followed by <name> or --clear", AGENT_HELP));
    }
    let name = if args[1] == "--clear" { Value::Null } else if args[1].starts_with('-') {
        return Err(CliError::new(format!("unknown flag '{}'", args[1]), AGENT_HELP));
    } else {
        validate_agent_name(&args[1], AGENT_HELP)?;
        Value::String(args[1].clone())
    };
    let mut params = Map::new();
    params.insert("target".into(), Value::String(target));
    params.insert("name".into(), name);
    Ok(socket_invocation("agent", "rename", "agent.rename", params, session, OutputMode::Json))
}

fn parse_agent_wait(args: &[String], session: Option<String>) -> Result<Invocation, CliError> {
    let target = required_positional(args.first(), "target", AGENT_HELP)?;
    let mut params = Map::new();
    params.insert("target".into(), Value::String(target));
    let mut until = Vec::new();
    let mut index = 1;
    while index < args.len() {
        match args[index].as_str() {
            "--until" => {
                let value = flag_value(args, &mut index, "--until", AGENT_HELP)?;
                agent_status(&value, AGENT_HELP)?;
                until.push(Value::String(value));
            }
            "--timeout" => {
                let value = positive_u64(&flag_value(args, &mut index, "--timeout", AGENT_HELP)?, "--timeout", AGENT_HELP)?;
                insert_once(&mut params, "timeout_ms", Value::Number(value.into()), AGENT_HELP)?;
            }
            flag if flag.starts_with('-') => return Err(CliError::new(format!("unknown flag '{flag}'"), AGENT_HELP)),
            value => return Err(CliError::new(format!("unexpected argument '{value}'"), AGENT_HELP)),
        }
    }
    if !until.is_empty() {
        params.insert("until".into(), Value::Array(until));
    }
    Ok(socket_invocation("agent", "wait", "agent.wait", params, session, OutputMode::Json))
}

fn parse_agent_attach(args: &[String], session: Option<String>) -> Result<Invocation, CliError> {
    let target = required_positional(args.first(), "target", AGENT_HELP)?;
    let takeover = match &args[1..] {
        [] => false,
        [flag] if flag == "--takeover" => true,
        [flag] if flag.starts_with('-') => return Err(CliError::new(format!("unknown flag '{flag}'"), AGENT_HELP)),
        _ => return Err(CliError::new("agent attach accepts only --takeover", AGENT_HELP)),
    };
    Ok(local_invocation(
        "agent",
        "attach",
        "agent.attach",
        Behavior::Terminal {
            session,
            action: TerminalAction::AgentAttach { target, takeover },
        },
    ))
}

fn parse_agent_start(args: &[String], session: Option<String>) -> Result<Invocation, CliError> {
    let name = required_positional(args.first(), "name", AGENT_HELP)?;
    validate_agent_name(&name, AGENT_HELP)?;
    let mut params = Map::new();
    params.insert("name".into(), Value::String(name));
    let mut native_args = Vec::new();
    let mut index = 1;
    while index < args.len() {
        match args[index].as_str() {
            "--kind" => {
                let value = flag_value(args, &mut index, "--kind", AGENT_HELP)?;
                choice(&value, AGENT_KINDS, "--kind", AGENT_HELP)?;
                insert_once(&mut params, "kind", Value::String(value), AGENT_HELP)?;
            }
            "--pane" => {
                let value = flag_value(args, &mut index, "--pane", AGENT_HELP)?;
                insert_once(&mut params, "pane_id", Value::String(value), AGENT_HELP)?;
            }
            "--timeout" => {
                let value = positive_u64(&flag_value(args, &mut index, "--timeout", AGENT_HELP)?, "--timeout", AGENT_HELP)?;
                insert_once(&mut params, "timeout_ms", Value::Number(value.into()), AGENT_HELP)?;
            }
            "--" => {
                native_args.extend(args[index + 1..].iter().cloned().map(Value::String));
                index = args.len();
            }
            flag if flag.starts_with('-') => return Err(CliError::new(format!("unknown flag '{flag}'"), AGENT_HELP)),
            value => return Err(CliError::new(format!("unexpected argument '{value}'"), AGENT_HELP)),
        }
    }
    require_key(&params, "kind", "agent start requires --kind KIND", AGENT_HELP)?;
    require_key(&params, "pane_id", "agent start requires --pane ID", AGENT_HELP)?;
    params.entry("timeout_ms").or_insert_with(|| Value::Number(30_000_u64.into()));
    if !native_args.is_empty() {
        params.insert("args".into(), Value::Array(native_args));
    }
    Ok(socket_invocation("agent", "start", "agent.start", params, session, OutputMode::Json))
}

fn parse_agent_explain(args: &[String], session: Option<String>) -> Result<Invocation, CliError> {
    let mut params = Map::new();
    let mut positional = None;
    let mut format = None;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--file" => {
                let value = flag_value(args, &mut index, "--file", AGENT_HELP)?;
                insert_once(&mut params, "file", Value::String(value), AGENT_HELP)?;
            }
            "--agent" => {
                let value = flag_value(args, &mut index, "--agent", AGENT_HELP)?;
                insert_once(&mut params, "agent", Value::String(value), AGENT_HELP)?;
            }
            "--json" => {
                if format.replace("json".to_owned()).is_some() {
                    return Err(CliError::new("--json conflicts with --format", AGENT_HELP));
                }
                index += 1;
            }
            "--format" => {
                let value = flag_value(args, &mut index, "--format", AGENT_HELP)?;
                choice(&value, &["text", "json"], "--format", AGENT_HELP)?;
                if format.replace(value).is_some() {
                    return Err(CliError::new("--format/--json may only be specified once", AGENT_HELP));
                }
            }
            "--verbose" => {
                insert_once(&mut params, "verbose", Value::Bool(true), AGENT_HELP)?;
                index += 1;
            }
            flag if flag.starts_with('-') => return Err(CliError::new(format!("unknown flag '{flag}'"), AGENT_HELP)),
            value => {
                if positional.replace(value.to_owned()).is_some() {
                    return Err(CliError::new("agent explain accepts only one target", AGENT_HELP));
                }
                index += 1;
            }
        }
    }
    match (positional, params.contains_key("file"), params.contains_key("agent")) {
        (Some(target), false, false) => { params.insert("target".into(), Value::String(target)); }
        (None, true, true) => {}
        (Some(_), true, _) | (Some(_), _, true) => return Err(CliError::new("target conflicts with --file/--agent", AGENT_HELP)),
        _ => return Err(CliError::new("agent explain requires <target> or both --file PATH --agent LABEL", AGENT_HELP)),
    }
    if let Some(value) = &format {
        params.insert("format".into(), Value::String(value.clone()));
    }
    let output = if format.as_deref() == Some("text") { OutputMode::RawText } else { OutputMode::Json };
    Ok(socket_invocation("agent", "explain", "agent.explain", params, session, output))
}

const AGENT_KINDS: &[&str] = &[
    "pi", "claude", "codex", "gemini", "cursor", "devin", "agy", "cline", "omp",
    "mastracode", "opencode", "copilot", "kimi", "kiro", "droid", "amp", "grok",
    "hermes", "kilo", "qodercli", "maki",
];

fn parse_pane(args: &[String], session: Option<String>) -> Result<Invocation, CliError> {
    let Some(command) = args.first() else { return Ok(help_invocation("pane", PANE_HELP)) };
    let tail = &args[1..];
    match command.as_str() {
        "list" => parse_optional_value_command("pane", "list", "pane.list", "--workspace", "workspace_id", tail, session, PANE_HELP),
        "current" => parse_pane_selector_only("current", "pane.current", "caller_pane_id", tail, session),
        "get" => one_positional("pane", "get", "pane.get", "pane_id", tail, session, PANE_HELP),
        "layout" => parse_pane_selector_only("layout", "pane.layout", "pane_id", tail, session),
        "process-info" => parse_pane_selector_only("process-info", "pane.process_info", "pane_id", tail, session),
        "neighbor" => parse_directional_pane("neighbor", "pane.neighbor", tail, session, false),
        "edges" => parse_pane_selector_only("edges", "pane.edges", "pane_id", tail, session),
        "focus" => parse_directional_pane("focus", "pane.focus_direction", tail, session, false),
        "resize" => parse_directional_pane("resize", "pane.resize", tail, session, true),
        "zoom" => parse_pane_zoom(tail, session),
        "rename" => parse_pane_rename(tail, session),
        "read" => parse_read("pane", tail, session),
        "split" => parse_pane_split(tail, session),
        "swap" => parse_pane_swap(tail, session),
        "move" => parse_pane_move(tail, session),
        "close" => one_positional("pane", "close", "pane.close", "pane_id", tail, session, PANE_HELP),
        "send-text" => two_positionals("pane", "send-text", "pane.send_text", "pane_id", "text", tail, session, PANE_HELP),
        "send-keys" => parse_send_keys("pane", tail, session),
        "wait-output" => parse_pane_wait_output(tail, session),
        "report-agent" => parse_pane_report_agent(tail, session),
        "report-agent-session" => parse_pane_report_agent_session(tail, session),
        "release-agent" => parse_pane_release_agent(tail, session),
        "report-metadata" => parse_pane_report_metadata(tail, session),
        "run" => two_positionals("pane", "run", "pane.run", "pane_id", "command", tail, session, PANE_HELP),
        value => Err(CliError::new(format!("unknown pane command '{value}'"), PANE_HELP)),
    }
}

fn parse_pane_selector_only(
    command: &'static str,
    method: &'static str,
    param_key: &'static str,
    args: &[String],
    session: Option<String>,
) -> Result<Invocation, CliError> {
    let mut pane = None;
    let mut current = false;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--pane" => {
                let value = flag_value(args, &mut index, "--pane", PANE_HELP)?;
                set_once(&mut pane, value, "--pane", PANE_HELP)?;
            }
            "--current" => {
                duplicate_bool(current, "--current", PANE_HELP)?;
                current = true;
                index += 1;
            }
            flag if flag.starts_with('-') => return Err(CliError::new(format!("unknown flag '{flag}'"), PANE_HELP)),
            value => return Err(CliError::new(format!("unexpected argument '{value}'"), PANE_HELP)),
        }
    }
    if pane.is_some() && current {
        return Err(CliError::new("--pane and --current are mutually exclusive", PANE_HELP));
    }
    let mut params = Map::new();
    if let Some(value) = pane {
        params.insert(param_key.into(), Value::String(value));
    }
    Ok(socket_invocation("pane", command, method, params, session, OutputMode::Json))
}

fn parse_directional_pane(
    command: &'static str,
    method: &'static str,
    args: &[String],
    session: Option<String>,
    allow_amount: bool,
) -> Result<Invocation, CliError> {
    let mut params = Map::new();
    let mut current = false;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--direction" => {
                let value = flag_value(args, &mut index, "--direction", PANE_HELP)?;
                choice(&value, &["left", "right", "up", "down"], "--direction", PANE_HELP)?;
                insert_once(&mut params, "direction", Value::String(value), PANE_HELP)?;
            }
            "--pane" => {
                let value = flag_value(args, &mut index, "--pane", PANE_HELP)?;
                insert_once(&mut params, "pane_id", Value::String(value), PANE_HELP)?;
            }
            "--current" => {
                duplicate_bool(current, "--current", PANE_HELP)?;
                current = true;
                index += 1;
            }
            "--amount" if allow_amount => {
                let value = positive_f64(&flag_value(args, &mut index, "--amount", PANE_HELP)?, "--amount", PANE_HELP)?;
                insert_once(&mut params, "amount", json_number(value, "--amount", PANE_HELP)?, PANE_HELP)?;
            }
            flag if flag.starts_with('-') => return Err(CliError::new(format!("unknown flag '{flag}'"), PANE_HELP)),
            value => return Err(CliError::new(format!("unexpected argument '{value}'"), PANE_HELP)),
        }
    }
    require_key(&params, "direction", &format!("pane {command} requires --direction"), PANE_HELP)?;
    if current && params.contains_key("pane_id") {
        return Err(CliError::new("--pane and --current are mutually exclusive", PANE_HELP));
    }
    Ok(socket_invocation("pane", command, method, params, session, OutputMode::Json))
}

fn parse_pane_zoom(args: &[String], session: Option<String>) -> Result<Invocation, CliError> {
    let mut params = Map::new();
    let mut positional = None;
    let mut current = false;
    let mut mode = None;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--pane" => {
                let value = flag_value(args, &mut index, "--pane", PANE_HELP)?;
                insert_once(&mut params, "pane_id", Value::String(value), PANE_HELP)?;
            }
            "--current" => {
                duplicate_bool(current, "--current", PANE_HELP)?;
                current = true;
                index += 1;
            }
            "--toggle" | "--on" | "--off" => {
                let value = args[index].trim_start_matches("--").to_owned();
                if mode.replace(value).is_some() {
                    return Err(CliError::new("--toggle, --on, and --off are mutually exclusive", PANE_HELP));
                }
                index += 1;
            }
            flag if flag.starts_with('-') => return Err(CliError::new(format!("unknown flag '{flag}'"), PANE_HELP)),
            value => {
                if positional.replace(value.to_owned()).is_some() {
                    return Err(CliError::new("pane zoom accepts at most one pane id", PANE_HELP));
                }
                index += 1;
            }
        }
    }
    let selector_count = usize::from(positional.is_some()) + usize::from(params.contains_key("pane_id")) + usize::from(current);
    if selector_count > 1 {
        return Err(CliError::new("pane id, --pane, and --current are mutually exclusive", PANE_HELP));
    }
    if let Some(value) = positional {
        params.insert("pane_id".into(), Value::String(value));
    }
    if let Some(value) = mode {
        params.insert("mode".into(), Value::String(value));
    }
    Ok(socket_invocation("pane", "zoom", "pane.zoom", params, session, OutputMode::Json))
}

fn parse_pane_rename(args: &[String], session: Option<String>) -> Result<Invocation, CliError> {
    let pane_id = required_positional(args.first(), "pane_id", PANE_HELP)?;
    if args.len() != 2 {
        return Err(CliError::new("pane rename requires <pane_id> followed by <label> or --clear", PANE_HELP));
    }
    let label = if args[1] == "--clear" { Value::Null } else if args[1].starts_with('-') {
        return Err(CliError::new(format!("unknown flag '{}'", args[1]), PANE_HELP));
    } else { Value::String(args[1].clone()) };
    let mut params = Map::new();
    params.insert("pane_id".into(), Value::String(pane_id));
    params.insert("label".into(), label);
    Ok(socket_invocation("pane", "rename", "pane.rename", params, session, OutputMode::Json))
}

fn parse_pane_split(args: &[String], session: Option<String>) -> Result<Invocation, CliError> {
    let mut params = Map::new();
    let mut positional = None;
    let mut current = false;
    let mut focus = None;
    let mut env = Map::new();
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--pane" => {
                let value = flag_value(args, &mut index, "--pane", PANE_HELP)?;
                insert_once(&mut params, "pane_id", Value::String(value), PANE_HELP)?;
            }
            "--current" => {
                duplicate_bool(current, "--current", PANE_HELP)?;
                current = true;
                index += 1;
            }
            "--direction" => {
                let value = flag_value(args, &mut index, "--direction", PANE_HELP)?;
                choice(&value, &["right", "down"], "--direction", PANE_HELP)?;
                insert_once(&mut params, "direction", Value::String(value), PANE_HELP)?;
            }
            "--ratio" => {
                let value = unit_f64(&flag_value(args, &mut index, "--ratio", PANE_HELP)?, "--ratio", PANE_HELP)?;
                insert_once(&mut params, "ratio", json_number(value, "--ratio", PANE_HELP)?, PANE_HELP)?;
            }
            "--cwd" => {
                let value = flag_value(args, &mut index, "--cwd", PANE_HELP)?;
                insert_once(&mut params, "cwd", Value::String(value), PANE_HELP)?;
            }
            "--env" => {
                let value = flag_value(args, &mut index, "--env", PANE_HELP)?;
                let (key, value) = key_value(&value, "--env", PANE_HELP)?;
                env.insert(key, Value::String(value));
            }
            "--focus" => set_exclusive_bool(&mut focus, true, "--focus", "--no-focus", PANE_HELP, &mut index)?,
            "--no-focus" => set_exclusive_bool(&mut focus, false, "--no-focus", "--focus", PANE_HELP, &mut index)?,
            flag if flag.starts_with('-') => return Err(CliError::new(format!("unknown flag '{flag}'"), PANE_HELP)),
            value => {
                if positional.replace(value.to_owned()).is_some() {
                    return Err(CliError::new("pane split accepts at most one pane id", PANE_HELP));
                }
                index += 1;
            }
        }
    }
    let selector_count = usize::from(positional.is_some()) + usize::from(params.contains_key("pane_id")) + usize::from(current);
    if selector_count > 1 {
        return Err(CliError::new("pane id, --pane, and --current are mutually exclusive", PANE_HELP));
    }
    require_key(&params, "direction", "pane split requires --direction", PANE_HELP)?;
    if let Some(value) = positional { params.insert("pane_id".into(), Value::String(value)); }
    if !env.is_empty() { params.insert("env".into(), Value::Object(env)); }
    if let Some(value) = focus { params.insert("focus".into(), Value::Bool(value)); }
    Ok(socket_invocation("pane", "split", "pane.split", params, session, OutputMode::Json))
}

fn parse_pane_swap(args: &[String], session: Option<String>) -> Result<Invocation, CliError> {
    let mut params = Map::new();
    let mut current = false;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--direction" => {
                let value = flag_value(args, &mut index, "--direction", PANE_HELP)?;
                choice(&value, &["left", "right", "up", "down"], "--direction", PANE_HELP)?;
                insert_once(&mut params, "direction", Value::String(value), PANE_HELP)?;
            }
            "--pane" => {
                let value = flag_value(args, &mut index, "--pane", PANE_HELP)?;
                insert_once(&mut params, "pane_id", Value::String(value), PANE_HELP)?;
            }
            "--current" => {
                duplicate_bool(current, "--current", PANE_HELP)?;
                current = true;
                index += 1;
            }
            "--source-pane" => {
                let value = flag_value(args, &mut index, "--source-pane", PANE_HELP)?;
                insert_once(&mut params, "source_pane_id", Value::String(value), PANE_HELP)?;
            }
            "--target-pane" => {
                let value = flag_value(args, &mut index, "--target-pane", PANE_HELP)?;
                insert_once(&mut params, "target_pane_id", Value::String(value), PANE_HELP)?;
            }
            flag if flag.starts_with('-') => return Err(CliError::new(format!("unknown flag '{flag}'"), PANE_HELP)),
            value => return Err(CliError::new(format!("unexpected argument '{value}'"), PANE_HELP)),
        }
    }
    let directional = params.contains_key("direction");
    let explicit = params.contains_key("source_pane_id") || params.contains_key("target_pane_id");
    if directional == explicit {
        return Err(CliError::new("pane swap requires either --direction or --source-pane with --target-pane", PANE_HELP));
    }
    if explicit && !(params.contains_key("source_pane_id") && params.contains_key("target_pane_id")) {
        return Err(CliError::new("explicit pane swap requires both --source-pane and --target-pane", PANE_HELP));
    }
    if explicit && (params.contains_key("pane_id") || current) {
        return Err(CliError::new("--pane/--current cannot be used with explicit pane swap", PANE_HELP));
    }
    if directional && current && params.contains_key("pane_id") {
        return Err(CliError::new("--pane and --current are mutually exclusive", PANE_HELP));
    }
    Ok(socket_invocation("pane", "swap", "pane.swap", params, session, OutputMode::Json))
}

fn parse_pane_move(args: &[String], session: Option<String>) -> Result<Invocation, CliError> {
    let pane_id = required_positional(args.first(), "pane_id", PANE_HELP)?;
    let mut destination = Map::new();
    let mut destination_kind = None;
    let mut focus = None;
    let mut index = 1;
    while index < args.len() {
        match args[index].as_str() {
            "--tab" => {
                set_destination(&mut destination_kind, "tab", "--tab", PANE_HELP)?;
                let value = flag_value(args, &mut index, "--tab", PANE_HELP)?;
                destination.insert("tab_id".into(), Value::String(value));
            }
            "--new-tab" => {
                set_destination(&mut destination_kind, "new_tab", "--new-tab", PANE_HELP)?;
                index += 1;
            }
            "--new-workspace" => {
                set_destination(&mut destination_kind, "new_workspace", "--new-workspace", PANE_HELP)?;
                index += 1;
            }
            "--split" => {
                let value = flag_value(args, &mut index, "--split", PANE_HELP)?;
                choice(&value, &["right", "down"], "--split", PANE_HELP)?;
                insert_once(&mut destination, "split", Value::String(value), PANE_HELP)?;
            }
            "--target-pane" => {
                let value = flag_value(args, &mut index, "--target-pane", PANE_HELP)?;
                insert_once(&mut destination, "target_pane_id", Value::String(value), PANE_HELP)?;
            }
            "--ratio" => {
                let value = unit_f64(&flag_value(args, &mut index, "--ratio", PANE_HELP)?, "--ratio", PANE_HELP)?;
                insert_once(&mut destination, "ratio", json_number(value, "--ratio", PANE_HELP)?, PANE_HELP)?;
            }
            "--workspace" => {
                let value = flag_value(args, &mut index, "--workspace", PANE_HELP)?;
                insert_once(&mut destination, "workspace_id", Value::String(value), PANE_HELP)?;
            }
            "--label" => {
                let value = flag_value(args, &mut index, "--label", PANE_HELP)?;
                insert_once(&mut destination, "label", Value::String(value), PANE_HELP)?;
            }
            "--tab-label" => {
                let value = flag_value(args, &mut index, "--tab-label", PANE_HELP)?;
                insert_once(&mut destination, "tab_label", Value::String(value), PANE_HELP)?;
            }
            "--focus" => set_exclusive_bool(&mut focus, true, "--focus", "--no-focus", PANE_HELP, &mut index)?,
            "--no-focus" => set_exclusive_bool(&mut focus, false, "--no-focus", "--focus", PANE_HELP, &mut index)?,
            flag if flag.starts_with('-') => return Err(CliError::new(format!("unknown flag '{flag}'"), PANE_HELP)),
            value => return Err(CliError::new(format!("unexpected argument '{value}'"), PANE_HELP)),
        }
    }
    let kind = destination_kind.ok_or_else(|| CliError::new("pane move requires --tab, --new-tab, or --new-workspace", PANE_HELP))?;
    destination.insert("type".into(), Value::String(kind.to_owned()));
    match kind {
        "tab" => {
            require_key(&destination, "split", "pane move --tab requires --split", PANE_HELP)?;
            if destination.contains_key("workspace_id") || destination.contains_key("label") || destination.contains_key("tab_label") {
                return Err(CliError::new("new-tab/new-workspace flags cannot be used with --tab", PANE_HELP));
            }
        }
        "new_tab" => {
            if destination.contains_key("split") || destination.contains_key("target_pane_id") || destination.contains_key("ratio") || destination.contains_key("tab_label") {
                return Err(CliError::new("--new-tab does not accept split or tab-label flags", PANE_HELP));
            }
        }
        "new_workspace" => {
            if destination.contains_key("split") || destination.contains_key("target_pane_id") || destination.contains_key("ratio") || destination.contains_key("workspace_id") {
                return Err(CliError::new("--new-workspace does not accept split or workspace flags", PANE_HELP));
            }
        }
        _ => unreachable!(),
    }
    let mut params = Map::new();
    params.insert("pane_id".into(), Value::String(pane_id));
    params.insert("destination".into(), Value::Object(destination));
    if let Some(value) = focus { params.insert("focus".into(), Value::Bool(value)); }
    Ok(socket_invocation("pane", "move", "pane.move", params, session, OutputMode::Json))
}

fn parse_pane_wait_output(args: &[String], session: Option<String>) -> Result<Invocation, CliError> {
    let pane_id = required_positional(args.first(), "pane_id", PANE_HELP)?;
    let mut params = Map::new();
    params.insert("pane_id".into(), Value::String(pane_id));
    let mut matcher = None;
    let mut index = 1;
    while index < args.len() {
        match args[index].as_str() {
            "--match" | "--regex" => {
                let flag = args[index].clone();
                let key = flag.trim_start_matches("--");
                let value = flag_value(args, &mut index, &flag, PANE_HELP)?;
                if matcher.replace(key.to_owned()).is_some() {
                    return Err(CliError::new("--match and --regex are mutually exclusive", PANE_HELP));
                }
                params.insert(key.into(), Value::String(value));
            }
            "--source" => {
                let value = flag_value(args, &mut index, "--source", PANE_HELP)?;
                choice(&value, &["visible", "recent", "recent-unwrapped"], "--source", PANE_HELP)?;
                insert_once(&mut params, "source", Value::String(value), PANE_HELP)?;
            }
            "--lines" => {
                let value = positive_u64(&flag_value(args, &mut index, "--lines", PANE_HELP)?, "--lines", PANE_HELP)?;
                insert_once(&mut params, "lines", Value::Number(value.into()), PANE_HELP)?;
            }
            "--timeout" => {
                let value = positive_u64(&flag_value(args, &mut index, "--timeout", PANE_HELP)?, "--timeout", PANE_HELP)?;
                insert_once(&mut params, "timeout_ms", Value::Number(value.into()), PANE_HELP)?;
            }
            "--raw" => {
                insert_once(&mut params, "raw", Value::Bool(true), PANE_HELP)?;
                index += 1;
            }
            flag if flag.starts_with('-') => return Err(CliError::new(format!("unknown flag '{flag}'"), PANE_HELP)),
            value => return Err(CliError::new(format!("unexpected argument '{value}'"), PANE_HELP)),
        }
    }
    if matcher.is_none() {
        return Err(CliError::new("pane wait-output requires exactly one of --match or --regex", PANE_HELP));
    }
    Ok(socket_invocation("pane", "wait-output", "pane.wait_for_output", params, session, OutputMode::Json))
}

fn parse_pane_report_agent(args: &[String], session: Option<String>) -> Result<Invocation, CliError> {
    let pane_id = required_positional(args.first(), "pane_id", PANE_HELP)?;
    let mut params = Map::new();
    params.insert("pane_id".into(), Value::String(pane_id));
    let mut index = 1;
    while index < args.len() {
        match args[index].as_str() {
            "--source" => string_flag(args, &mut index, "--source", "source", &mut params, PANE_HELP)?,
            "--agent" => string_flag(args, &mut index, "--agent", "agent", &mut params, PANE_HELP)?,
            "--state" => {
                let value = flag_value(args, &mut index, "--state", PANE_HELP)?;
                choice(&value, &["idle", "working", "blocked", "unknown"], "--state", PANE_HELP)?;
                insert_once(&mut params, "state", Value::String(value), PANE_HELP)?;
            }
            "--message" => string_flag(args, &mut index, "--message", "message", &mut params, PANE_HELP)?,
            "--seq" => numeric_flag(args, &mut index, "--seq", "seq", &mut params, PANE_HELP)?,
            "--ttl-ms" => numeric_flag(args, &mut index, "--ttl-ms", "ttl_ms", &mut params, PANE_HELP)?,
            "--agent-session-id" => string_flag(args, &mut index, "--agent-session-id", "agent_session_id", &mut params, PANE_HELP)?,
            "--agent-session-path" => string_flag(args, &mut index, "--agent-session-path", "agent_session_path", &mut params, PANE_HELP)?,
            flag if flag.starts_with('-') => return Err(CliError::new(format!("unknown flag '{flag}'"), PANE_HELP)),
            value => return Err(CliError::new(format!("unexpected argument '{value}'"), PANE_HELP)),
        }
    }
    for (key, message) in [("source", "report-agent requires --source"), ("agent", "report-agent requires --agent"), ("state", "report-agent requires --state")] {
        require_key(&params, key, message, PANE_HELP)?;
    }
    Ok(socket_invocation("pane", "report-agent", "pane.report_agent", params, session, OutputMode::Json))
}

fn parse_pane_report_agent_session(args: &[String], session: Option<String>) -> Result<Invocation, CliError> {
    let pane_id = required_positional(args.first(), "pane_id", PANE_HELP)?;
    let mut params = Map::new();
    params.insert("pane_id".into(), Value::String(pane_id));
    let mut index = 1;
    while index < args.len() {
        match args[index].as_str() {
            "--source" => string_flag(args, &mut index, "--source", "source", &mut params, PANE_HELP)?,
            "--agent" => string_flag(args, &mut index, "--agent", "agent", &mut params, PANE_HELP)?,
            "--seq" => numeric_flag(args, &mut index, "--seq", "seq", &mut params, PANE_HELP)?,
            "--agent-session-id" => string_flag(args, &mut index, "--agent-session-id", "agent_session_id", &mut params, PANE_HELP)?,
            "--agent-session-path" => string_flag(args, &mut index, "--agent-session-path", "agent_session_path", &mut params, PANE_HELP)?,
            "--session-start-source" => string_flag(args, &mut index, "--session-start-source", "session_start_source", &mut params, PANE_HELP)?,
            flag if flag.starts_with('-') => return Err(CliError::new(format!("unknown flag '{flag}'"), PANE_HELP)),
            value => return Err(CliError::new(format!("unexpected argument '{value}'"), PANE_HELP)),
        }
    }
    require_key(&params, "source", "report-agent-session requires --source", PANE_HELP)?;
    require_key(&params, "agent", "report-agent-session requires --agent", PANE_HELP)?;
    Ok(socket_invocation("pane", "report-agent-session", "pane.report_agent_session", params, session, OutputMode::Json))
}

fn parse_pane_release_agent(args: &[String], session: Option<String>) -> Result<Invocation, CliError> {
    let pane_id = required_positional(args.first(), "pane_id", PANE_HELP)?;
    let mut params = Map::new();
    params.insert("pane_id".into(), Value::String(pane_id));
    let mut index = 1;
    while index < args.len() {
        match args[index].as_str() {
            "--source" => string_flag(args, &mut index, "--source", "source", &mut params, PANE_HELP)?,
            "--agent" => string_flag(args, &mut index, "--agent", "agent", &mut params, PANE_HELP)?,
            "--seq" => numeric_flag(args, &mut index, "--seq", "seq", &mut params, PANE_HELP)?,
            flag if flag.starts_with('-') => return Err(CliError::new(format!("unknown flag '{flag}'"), PANE_HELP)),
            value => return Err(CliError::new(format!("unexpected argument '{value}'"), PANE_HELP)),
        }
    }
    require_key(&params, "source", "release-agent requires --source", PANE_HELP)?;
    require_key(&params, "agent", "release-agent requires --agent", PANE_HELP)?;
    Ok(socket_invocation("pane", "release-agent", "pane.release_agent", params, session, OutputMode::Json))
}

fn parse_pane_report_metadata(args: &[String], session: Option<String>) -> Result<Invocation, CliError> {
    let pane_id = required_positional(args.first(), "pane_id", PANE_HELP)?;
    let mut params = Map::new();
    params.insert("pane_id".into(), Value::String(pane_id));
    let mut state_labels = Map::new();
    let mut tokens = Map::new();
    let mut clear_state_labels = false;
    let mut index = 1;
    while index < args.len() {
        match args[index].as_str() {
            "--source" => string_flag(args, &mut index, "--source", "source", &mut params, PANE_HELP)?,
            "--agent" => string_flag(args, &mut index, "--agent", "agent", &mut params, PANE_HELP)?,
            "--applies-to-source" => string_flag(args, &mut index, "--applies-to-source", "applies_to_source", &mut params, PANE_HELP)?,
            "--title" => string_flag(args, &mut index, "--title", "title", &mut params, PANE_HELP)?,
            "--clear-title" => {
                if params.contains_key("title") { return Err(CliError::new("--title and --clear-title are mutually exclusive", PANE_HELP)); }
                params.insert("title".into(), Value::Null);
                index += 1;
            }
            "--display-agent" => string_flag(args, &mut index, "--display-agent", "display_agent", &mut params, PANE_HELP)?,
            "--clear-display-agent" => {
                if params.contains_key("display_agent") { return Err(CliError::new("--display-agent and --clear-display-agent are mutually exclusive", PANE_HELP)); }
                params.insert("display_agent".into(), Value::Null);
                index += 1;
            }
            "--state-label" => {
                let value = flag_value(args, &mut index, "--state-label", PANE_HELP)?;
                let (status, text) = key_value(&value, "--state-label", PANE_HELP)?;
                agent_status(&status, PANE_HELP)?;
                state_labels.insert(status, Value::String(text));
            }
            "--clear-state-labels" => {
                duplicate_bool(clear_state_labels, "--clear-state-labels", PANE_HELP)?;
                clear_state_labels = true;
                index += 1;
            }
            "--token" => {
                let value = flag_value(args, &mut index, "--token", PANE_HELP)?;
                let (name, value) = key_value(&value, "--token", PANE_HELP)?;
                validate_token_name(&name, PANE_HELP)?;
                tokens.insert(name, Value::String(value));
            }
            "--clear-token" => {
                let name = flag_value(args, &mut index, "--clear-token", PANE_HELP)?;
                validate_token_name(&name, PANE_HELP)?;
                tokens.insert(name, Value::Null);
            }
            "--seq" => numeric_flag(args, &mut index, "--seq", "seq", &mut params, PANE_HELP)?,
            "--ttl-ms" => numeric_flag(args, &mut index, "--ttl-ms", "ttl_ms", &mut params, PANE_HELP)?,
            flag if flag.starts_with('-') => return Err(CliError::new(format!("unknown flag '{flag}'"), PANE_HELP)),
            value => return Err(CliError::new(format!("unexpected argument '{value}'"), PANE_HELP)),
        }
    }
    require_key(&params, "source", "report-metadata requires --source", PANE_HELP)?;
    if clear_state_labels && !state_labels.is_empty() {
        return Err(CliError::new("--clear-state-labels conflicts with --state-label", PANE_HELP));
    }
    if clear_state_labels { params.insert("state_labels".into(), Value::Null); }
    else if !state_labels.is_empty() { params.insert("state_labels".into(), Value::Object(state_labels)); }
    if !tokens.is_empty() { params.insert("tokens".into(), Value::Object(tokens)); }
    Ok(socket_invocation("pane", "report-metadata", "pane.report_metadata", params, session, OutputMode::Json))
}

fn parse_workspace(args: &[String], session: Option<String>) -> Result<Invocation, CliError> {
    let Some(command) = args.first() else { return Ok(help_invocation("workspace", WORKSPACE_HELP)) };
    let tail = &args[1..];
    match command.as_str() {
        "list" => {
            no_args(tail, WORKSPACE_HELP)?;
            Ok(socket_invocation("workspace", "list", "workspace.list", Map::new(), session, OutputMode::Json))
        }
        "create" => parse_create_command("workspace", "workspace.create", tail, session),
        "get" => one_positional("workspace", "get", "workspace.get", "workspace_id", tail, session, WORKSPACE_HELP),
        "focus" => one_positional("workspace", "focus", "workspace.focus", "workspace_id", tail, session, WORKSPACE_HELP),
        "rename" => two_positionals("workspace", "rename", "workspace.rename", "workspace_id", "label", tail, session, WORKSPACE_HELP),
        "report-metadata" => parse_workspace_metadata(tail, session),
        "close" => one_positional("workspace", "close", "workspace.close", "workspace_id", tail, session, WORKSPACE_HELP),
        value => Err(CliError::new(format!("unknown workspace command '{value}'"), WORKSPACE_HELP)),
    }
}

fn parse_tab(args: &[String], session: Option<String>) -> Result<Invocation, CliError> {
    let Some(command) = args.first() else { return Ok(help_invocation("tab", TAB_HELP)) };
    let tail = &args[1..];
    match command.as_str() {
        "list" => parse_optional_value_command("tab", "list", "tab.list", "--workspace", "workspace_id", tail, session, TAB_HELP),
        "create" => parse_create_command("tab", "tab.create", tail, session),
        "get" => one_positional("tab", "get", "tab.get", "tab_id", tail, session, TAB_HELP),
        "focus" => one_positional("tab", "focus", "tab.focus", "tab_id", tail, session, TAB_HELP),
        "rename" => two_positionals("tab", "rename", "tab.rename", "tab_id", "label", tail, session, TAB_HELP),
        "close" => one_positional("tab", "close", "tab.close", "tab_id", tail, session, TAB_HELP),
        value => Err(CliError::new(format!("unknown tab command '{value}'"), TAB_HELP)),
    }
}

fn parse_create_command(
    group: &'static str,
    method: &'static str,
    args: &[String],
    session: Option<String>,
) -> Result<Invocation, CliError> {
    let usage = if group == "workspace" { WORKSPACE_HELP } else { TAB_HELP };
    let mut params = Map::new();
    let mut env = Map::new();
    let mut focus = None;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--workspace" if group == "tab" => string_flag(args, &mut index, "--workspace", "workspace_id", &mut params, usage)?,
            "--cwd" => string_flag(args, &mut index, "--cwd", "cwd", &mut params, usage)?,
            "--label" => string_flag(args, &mut index, "--label", "label", &mut params, usage)?,
            "--env" => {
                let value = flag_value(args, &mut index, "--env", usage)?;
                let (key, value) = key_value(&value, "--env", usage)?;
                env.insert(key, Value::String(value));
            }
            "--focus" => set_exclusive_bool(&mut focus, true, "--focus", "--no-focus", usage, &mut index)?,
            "--no-focus" => set_exclusive_bool(&mut focus, false, "--no-focus", "--focus", usage, &mut index)?,
            flag if flag.starts_with('-') => return Err(CliError::new(format!("unknown flag '{flag}'"), usage)),
            value => return Err(CliError::new(format!("unexpected argument '{value}'"), usage)),
        }
    }
    if !env.is_empty() { params.insert("env".into(), Value::Object(env)); }
    if let Some(value) = focus { params.insert("focus".into(), Value::Bool(value)); }
    Ok(socket_invocation(group, "create", method, params, session, OutputMode::Json))
}

fn parse_workspace_metadata(args: &[String], session: Option<String>) -> Result<Invocation, CliError> {
    let workspace_id = required_positional(args.first(), "workspace_id", WORKSPACE_HELP)?;
    let mut params = Map::new();
    params.insert("workspace_id".into(), Value::String(workspace_id));
    let mut tokens = Map::new();
    let mut index = 1;
    while index < args.len() {
        match args[index].as_str() {
            "--source" => string_flag(args, &mut index, "--source", "source", &mut params, WORKSPACE_HELP)?,
            "--token" => {
                let value = flag_value(args, &mut index, "--token", WORKSPACE_HELP)?;
                let (name, value) = key_value(&value, "--token", WORKSPACE_HELP)?;
                validate_token_name(&name, WORKSPACE_HELP)?;
                tokens.insert(name, Value::String(value));
            }
            "--clear-token" => {
                let name = flag_value(args, &mut index, "--clear-token", WORKSPACE_HELP)?;
                validate_token_name(&name, WORKSPACE_HELP)?;
                tokens.insert(name, Value::Null);
            }
            "--seq" => numeric_flag(args, &mut index, "--seq", "seq", &mut params, WORKSPACE_HELP)?,
            "--ttl-ms" => numeric_flag(args, &mut index, "--ttl-ms", "ttl_ms", &mut params, WORKSPACE_HELP)?,
            flag if flag.starts_with('-') => return Err(CliError::new(format!("unknown flag '{flag}'"), WORKSPACE_HELP)),
            value => return Err(CliError::new(format!("unexpected argument '{value}'"), WORKSPACE_HELP)),
        }
    }
    require_key(&params, "source", "workspace report-metadata requires --source", WORKSPACE_HELP)?;
    if !tokens.is_empty() { params.insert("tokens".into(), Value::Object(tokens)); }
    Ok(socket_invocation("workspace", "report-metadata", "workspace.report_metadata", params, session, OutputMode::Json))
}

fn parse_worktree(args: &[String], session: Option<String>) -> Result<Invocation, CliError> {
    let Some(command) = args.first() else { return Ok(help_invocation("worktree", WORKTREE_HELP)) };
    let tail = &args[1..];
    match command.as_str() {
        "list" => parse_worktree_options("list", tail, session),
        "create" => parse_worktree_options("create", tail, session),
        "open" => parse_worktree_options("open", tail, session),
        "remove" => parse_worktree_remove(tail, session),
        value => Err(CliError::new(format!("unknown worktree command '{value}'"), WORKTREE_HELP)),
    }
}

fn parse_worktree_options(command: &'static str, args: &[String], session: Option<String>) -> Result<Invocation, CliError> {
    let mut params = Map::new();
    let mut focus = None;
    let mut json = false;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--workspace" => string_flag(args, &mut index, "--workspace", "workspace_id", &mut params, WORKTREE_HELP)?,
            "--cwd" => string_flag(args, &mut index, "--cwd", "cwd", &mut params, WORKTREE_HELP)?,
            "--branch" if command != "list" => string_flag(args, &mut index, "--branch", "branch", &mut params, WORKTREE_HELP)?,
            "--base" if command == "create" => string_flag(args, &mut index, "--base", "base", &mut params, WORKTREE_HELP)?,
            "--path" if command != "list" => string_flag(args, &mut index, "--path", "path", &mut params, WORKTREE_HELP)?,
            "--label" if command != "list" => string_flag(args, &mut index, "--label", "label", &mut params, WORKTREE_HELP)?,
            "--focus" if command != "list" => set_exclusive_bool(&mut focus, true, "--focus", "--no-focus", WORKTREE_HELP, &mut index)?,
            "--no-focus" if command != "list" => set_exclusive_bool(&mut focus, false, "--no-focus", "--focus", WORKTREE_HELP, &mut index)?,
            "--json" => {
                duplicate_bool(json, "--json", WORKTREE_HELP)?;
                json = true;
                index += 1;
            }
            flag if flag.starts_with('-') => return Err(CliError::new(format!("unknown flag '{flag}'"), WORKTREE_HELP)),
            value => return Err(CliError::new(format!("unexpected argument '{value}'"), WORKTREE_HELP)),
        }
    }
    if params.contains_key("workspace_id") && params.contains_key("cwd") {
        return Err(CliError::new("--workspace and --cwd are mutually exclusive", WORKTREE_HELP));
    }
    if command == "open" && (params.contains_key("path") == params.contains_key("branch")) {
        return Err(CliError::new("worktree open requires exactly one of --path or --branch", WORKTREE_HELP));
    }
    if let Some(value) = focus { params.insert("focus".into(), Value::Bool(value)); }
    let _ = json;
    Ok(socket_invocation("worktree", command, match command {
        "list" => "worktree.list",
        "create" => "worktree.create",
        "open" => "worktree.open",
        _ => unreachable!(),
    }, params, session, OutputMode::Json))
}

fn parse_worktree_remove(args: &[String], session: Option<String>) -> Result<Invocation, CliError> {
    let mut params = Map::new();
    let mut force = false;
    let mut json = false;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--workspace" => string_flag(args, &mut index, "--workspace", "workspace_id", &mut params, WORKTREE_HELP)?,
            "--force" => {
                duplicate_bool(force, "--force", WORKTREE_HELP)?;
                force = true;
                index += 1;
            }
            "--json" => {
                duplicate_bool(json, "--json", WORKTREE_HELP)?;
                json = true;
                index += 1;
            }
            flag if flag.starts_with('-') => return Err(CliError::new(format!("unknown flag '{flag}'"), WORKTREE_HELP)),
            value => return Err(CliError::new(format!("unexpected argument '{value}'"), WORKTREE_HELP)),
        }
    }
    require_key(&params, "workspace_id", "worktree remove requires --workspace ID", WORKTREE_HELP)?;
    if force { params.insert("force".into(), Value::Bool(true)); }
    let _ = json;
    Ok(socket_invocation("worktree", "remove", "worktree.remove", params, session, OutputMode::Json))
}

fn parse_terminal(args: &[String], session: Option<String>) -> Result<Invocation, CliError> {
    let Some(command) = args.first() else { return Ok(help_invocation("terminal", TERMINAL_HELP)) };
    match command.as_str() {
        "attach" => {
            let terminal_id = required_positional(args.get(1), "terminal_id", TERMINAL_HELP)?;
            let takeover = parse_optional_bool_flag(&args[2..], "--takeover", TERMINAL_HELP)?;
            Ok(local_invocation(
                "terminal",
                "attach",
                "terminal.attach",
                Behavior::Terminal {
                    session,
                    action: TerminalAction::Attach { terminal_id, takeover },
                },
            ))
        }
        "session" => parse_terminal_session(&args[1..], session),
        "title" => parse_terminal_title(&args[1..], session),
        value => Err(CliError::new(format!("unknown terminal command '{value}'"), TERMINAL_HELP)),
    }
}

fn parse_terminal_session(args: &[String], session: Option<String>) -> Result<Invocation, CliError> {
    let mode = required_positional(args.first(), "control|observe", TERMINAL_HELP)?;
    choice(&mode, &["control", "observe"], "terminal session mode", TERMINAL_HELP)?;
    let target = required_positional(args.get(1), "target", TERMINAL_HELP)?;
    let mut cols = None;
    let mut rows = None;
    let mut takeover = false;
    let mut index = 2;
    while index < args.len() {
        match args[index].as_str() {
            "--takeover" if mode == "control" => {
                duplicate_bool(takeover, "--takeover", TERMINAL_HELP)?;
                takeover = true;
                index += 1;
            }
            "--cols" => {
                let value = positive_u16(&flag_value(args, &mut index, "--cols", TERMINAL_HELP)?, "--cols", TERMINAL_HELP)?;
                if cols.replace(value).is_some() { return Err(CliError::new("--cols may only be specified once", TERMINAL_HELP)); }
            }
            "--rows" => {
                let value = positive_u16(&flag_value(args, &mut index, "--rows", TERMINAL_HELP)?, "--rows", TERMINAL_HELP)?;
                if rows.replace(value).is_some() { return Err(CliError::new("--rows may only be specified once", TERMINAL_HELP)); }
            }
            flag if flag.starts_with('-') => return Err(CliError::new(format!("unknown flag '{flag}'"), TERMINAL_HELP)),
            value => return Err(CliError::new(format!("unexpected argument '{value}'"), TERMINAL_HELP)),
        }
    }
    let action = if mode == "control" {
        TerminalAction::Control { target, takeover, cols, rows }
    } else {
        TerminalAction::Observe { target, cols, rows }
    };
    Ok(local_invocation(
        "terminal",
        &mode,
        &format!("terminal.session.{mode}"),
        Behavior::Terminal { session, action },
    ))
}

fn parse_terminal_title(args: &[String], session: Option<String>) -> Result<Invocation, CliError> {
    let action = required_positional(args.first(), "set|clear", TERMINAL_HELP)?;
    match action.as_str() {
        "set" => two_level_one_positional("terminal", "title-set", "client.window_title.set", "title", &args[1..], session, TERMINAL_HELP),
        "clear" => {
            no_args(&args[1..], TERMINAL_HELP)?;
            Ok(socket_invocation("terminal", "title-clear", "client.window_title.clear", Map::new(), session, OutputMode::Json))
        }
        value => Err(CliError::new(format!("unknown terminal title command '{value}'"), TERMINAL_HELP)),
    }
}

fn parse_notification(args: &[String], session: Option<String>) -> Result<Invocation, CliError> {
    let Some(command) = args.first() else { return Ok(help_invocation("notification", NOTIFICATION_HELP)) };
    if command != "show" {
        return Err(CliError::new(format!("unknown notification command '{command}'"), NOTIFICATION_HELP));
    }
    let title = required_positional(args.get(1), "title", NOTIFICATION_HELP)?;
    let mut params = Map::new();
    params.insert("title".into(), Value::String(title));
    let mut index = 2;
    while index < args.len() {
        match args[index].as_str() {
            "--body" => string_flag(args, &mut index, "--body", "body", &mut params, NOTIFICATION_HELP)?,
            "--position" => {
                let value = flag_value(args, &mut index, "--position", NOTIFICATION_HELP)?;
                choice(&value, &["top-left", "top-right", "bottom-left", "bottom-right"], "--position", NOTIFICATION_HELP)?;
                insert_once(&mut params, "position", Value::String(value), NOTIFICATION_HELP)?;
            }
            "--sound" => {
                let value = flag_value(args, &mut index, "--sound", NOTIFICATION_HELP)?;
                choice(&value, &["none", "done", "request"], "--sound", NOTIFICATION_HELP)?;
                insert_once(&mut params, "sound", Value::String(value), NOTIFICATION_HELP)?;
            }
            flag if flag.starts_with('-') => return Err(CliError::new(format!("unknown flag '{flag}'"), NOTIFICATION_HELP)),
            value => return Err(CliError::new(format!("unexpected argument '{value}'"), NOTIFICATION_HELP)),
        }
    }
    Ok(socket_invocation("notification", "show", "notification.show", params, session, OutputMode::Json))
}

fn parse_integration(args: &[String], session: Option<String>) -> Result<Invocation, CliError> {
    let Some(command) = args.first() else { return Ok(help_invocation("integration", INTEGRATION_HELP)) };
    match command.as_str() {
        "hook" => parse_integration_hook(&args[1..], session),
        "install" | "uninstall" => {
            if args.len() != 2 {
                return Err(CliError::new(format!("integration {command} requires one integration name"), INTEGRATION_HELP));
            }
            choice(&args[1], INTEGRATION_NAMES, "integration name", INTEGRATION_HELP)?;
            let mut params = Map::new();
            params.insert("name".into(), Value::String(args[1].clone()));
            let method = if command == "install" { "integration.install" } else { "integration.uninstall" };
            Ok(socket_invocation("integration", command, method, params, session, OutputMode::Json))
        }
        "status" => {
            let outdated = parse_optional_bool_flag(&args[1..], "--outdated-only", INTEGRATION_HELP)?;
            let mut params = Map::new();
            if outdated { params.insert("outdated_only".into(), Value::Bool(true)); }
            Ok(socket_invocation("integration", "status", "integration.status", params, session, OutputMode::Json))
        }
        value => Err(CliError::new(format!("unknown integration command '{value}'"), INTEGRATION_HELP)),
    }
}

fn parse_integration_hook(args: &[String], session: Option<String>) -> Result<Invocation, CliError> {
    let Some(hook) = args.first() else {
        return Err(CliError::new("integration hook requires a helper name", INTEGRATION_HELP));
    };
    let action = match hook.as_str() {
        "claude-notification" => {
            no_args(&args[1..], INTEGRATION_HELP)?;
            IntegrationHookAction::ClaudeNotification
        }
        "claude-stop" => {
            no_args(&args[1..], INTEGRATION_HELP)?;
            IntegrationHookAction::ClaudeStop
        }
        "claude-session-start" => {
            no_args(&args[1..], INTEGRATION_HELP)?;
            IntegrationHookAction::ClaudeSessionStart
        }
        "codex-notify" => {
            if args.len() > 2 {
                return Err(CliError::new("codex-notify accepts one JSON payload", INTEGRATION_HELP));
            }
            IntegrationHookAction::CodexNotify { payload: args.get(1).cloned() }
        }
        value => return Err(CliError::new(format!("unknown integration hook '{value}'"), INTEGRATION_HELP)),
    };
    Ok(local_invocation(
        "integration",
        hook,
        &format!("local.integration.hook.{hook}"),
        Behavior::IntegrationHook { session, action },
    ))
}

const INTEGRATION_NAMES: &[&str] = &[
    "pi", "omp", "claude", "codex", "copilot", "devin", "droid", "kimi", "opencode",
    "kilo", "hermes", "qodercli", "cursor", "mastracode",
];

fn parse_session(args: &[String]) -> Result<Invocation, CliError> {
    let Some(command) = args.first() else { return Ok(help_invocation("session", SESSION_HELP)) };
    match command.as_str() {
        "list" => {
            let json = parse_optional_bool_flag(&args[1..], "--json", SESSION_HELP)?;
            Ok(local_invocation("session", "list", "local.session.list", Behavior::Session(SessionAction::List { json })))
        }
        "attach" => {
            if args.len() != 2 { return Err(CliError::new("session attach requires <name>", SESSION_HELP)); }
            Ok(local_invocation("session", "attach", "local.launch_client", Behavior::LaunchClient(LaunchClient { session: Some(args[1].clone()), ..LaunchClient::default() })))
        }
        "stop" => {
            let (name, _json) = session_name_and_json(&args[1..], "stop")?;
            Ok(socket_invocation("session", "stop", "server.stop", Map::new(), Some(name), OutputMode::Json))
        }
        "delete" => {
            let (name, json) = session_name_and_json(&args[1..], "delete")?;
            Ok(local_invocation("session", "delete", "local.session.delete", Behavior::Session(SessionAction::Delete { name, json })))
        }
        value => Err(CliError::new(format!("unknown session command '{value}'"), SESSION_HELP)),
    }
}

fn session_name_and_json(args: &[String], command: &str) -> Result<(String, bool), CliError> {
    let name = required_positional(args.first(), "name", SESSION_HELP)?;
    let json = parse_optional_bool_flag(&args[1..], "--json", SESSION_HELP)
        .map_err(|_| CliError::new(format!("session {command} accepts <name> and optional --json"), SESSION_HELP))?;
    Ok((name, json))
}

fn parse_api(args: &[String], session: Option<String>) -> Result<Invocation, CliError> {
    let Some(command) = args.first() else { return Ok(help_invocation("api", API_HELP)) };
    match command.as_str() {
        "snapshot" => {
            no_args(&args[1..], API_HELP)?;
            Ok(socket_invocation("api", "snapshot", "session.snapshot", Map::new(), session, OutputMode::Json))
        }
        "schema" => {
            let output = match &args[1..] {
                [] => SchemaOutput::Text,
                [flag] if flag == "--json" => SchemaOutput::Json,
                [flag, path] if flag == "--output" => SchemaOutput::File(path.clone()),
                values if values.iter().any(|value| value == "--json") && values.iter().any(|value| value == "--output") => {
                    return Err(CliError::new("--json and --output are mutually exclusive", API_HELP));
                }
                [flag, ..] if flag.starts_with('-') => return Err(CliError::new(format!("unknown or malformed flag '{flag}'"), API_HELP)),
                _ => return Err(CliError::new("api schema accepts --json or --output PATH", API_HELP)),
            };
            Ok(local_invocation("api", "schema", "local.api_schema", Behavior::ApiSchema(output)))
        }
        value => Err(CliError::new(format!("unknown api command '{value}'"), API_HELP)),
    }
}

fn parse_config(args: &[String]) -> Result<Invocation, CliError> {
    if args.is_empty() { return Ok(help_invocation("config", CONFIG_HELP)); }
    no_args(&args[1..], CONFIG_HELP)?;
    let action = match args[0].as_str() {
        "check" => ConfigAction::Check,
        "reset-keys" => ConfigAction::ResetKeys,
        value => return Err(CliError::new(format!("unknown config command '{value}'"), CONFIG_HELP)),
    };
    let command = args[0].as_str();
    Ok(local_invocation("config", command, &format!("local.config.{command}"), Behavior::Config(action)))
}

fn parse_channel(args: &[String]) -> Result<Invocation, CliError> {
    let Some(command) = args.first() else { return Ok(help_invocation("channel", CHANNEL_HELP)) };
    let action = match command.as_str() {
        "show" => {
            no_args(&args[1..], CHANNEL_HELP)?;
            ChannelAction::Show
        }
        "set" => {
            if args.len() != 2 { return Err(CliError::new("channel set requires stable or preview", CHANNEL_HELP)); }
            choice(&args[1], &["stable", "preview"], "channel", CHANNEL_HELP)?;
            ChannelAction::Set(args[1].clone())
        }
        value => return Err(CliError::new(format!("unknown channel command '{value}'"), CHANNEL_HELP)),
    };
    Ok(local_invocation("channel", command, &format!("local.channel.{command}"), Behavior::Channel(action)))
}

fn parse_plugin(args: &[String], session: Option<String>) -> Result<Invocation, CliError> {
    let Some(command) = args.first() else { return Ok(help_invocation("plugin", PLUGIN_HELP)) };
    match command.as_str() {
        "install" => parse_plugin_install(&args[1..], session),
        "uninstall" => {
            if args.len() != 2 {
                return Err(CliError::new("plugin uninstall requires <plugin_id|owner/repo[/subdir...]>", PLUGIN_HELP));
            }
            let target = if args[1].contains('/') {
                PluginTarget::Github(parse_github_slug(&args[1])?)
            } else {
                validate_plugin_id_argument(&args[1])?;
                PluginTarget::PluginId(args[1].clone())
            };
            Ok(plugin_invocation("uninstall", "plugin.unlink", session, PluginAction::Uninstall { target }))
        }
        "link" => {
            let path = required_positional(args.get(1), "path", PLUGIN_HELP)?;
            let disabled = parse_optional_bool_flag(&args[2..], "--disabled", PLUGIN_HELP)?;
            Ok(plugin_invocation("link", "plugin.link", session, PluginAction::Link { path, disabled }))
        }
        "list" => {
            let (plugin_id, json, limit) = parse_plugin_output_flags(&args[1..], true, false)?;
            debug_assert!(limit.is_none());
            Ok(plugin_invocation("list", "plugin.list", session, PluginAction::List { plugin_id, json }))
        }
        "config-dir" => {
            if args.len() != 2 { return Err(CliError::new("plugin config-dir requires <plugin_id>", PLUGIN_HELP)); }
            validate_plugin_id_argument(&args[1])?;
            Ok(plugin_invocation(
                "config-dir",
                "plugin.list",
                session,
                PluginAction::ConfigDir { plugin_id: args[1].clone() },
            ))
        }
        "unlink" | "enable" | "disable" => {
            if args.len() != 2 { return Err(CliError::new(format!("plugin {command} requires <plugin_id>"), PLUGIN_HELP)); }
            validate_plugin_id_argument(&args[1])?;
            let action = match command.as_str() {
                "unlink" => PluginAction::Unlink { plugin_id: args[1].clone() },
                "enable" => PluginAction::Enable { plugin_id: args[1].clone() },
                _ => PluginAction::Disable { plugin_id: args[1].clone() },
            };
            Ok(plugin_invocation(command, &format!("plugin.{command}"), session, action))
        }
        "action" => parse_plugin_action(&args[1..], session),
        "log" => parse_plugin_log(&args[1..], session),
        "pane" => parse_plugin_pane(&args[1..], session),
        value => Err(CliError::new(format!("unknown plugin command '{value}'"), PLUGIN_HELP)),
    }
}

fn parse_plugin_install(args: &[String], session: Option<String>) -> Result<Invocation, CliError> {
    let source = parse_github_slug(&required_positional(args.first(), "owner/repo[/subdir...]", PLUGIN_HELP)?)?;
    let mut requested_ref = None;
    let mut yes = false;
    let mut index = 1;
    while index < args.len() {
        match args[index].as_str() {
            "--ref" => {
                let value = flag_value(args, &mut index, "--ref", PLUGIN_HELP)?;
                if value.trim().is_empty() || value.starts_with('-') {
                    return Err(CliError::new("--ref requires a non-empty Git ref", PLUGIN_HELP));
                }
                set_once(&mut requested_ref, value, "--ref", PLUGIN_HELP)?;
            }
            "--yes" => {
                duplicate_bool(yes, "--yes", PLUGIN_HELP)?;
                yes = true;
                index += 1;
            }
            flag if flag.starts_with('-') => return Err(CliError::new(format!("unknown flag '{flag}'"), PLUGIN_HELP)),
            value => return Err(CliError::new(format!("unexpected argument '{value}'"), PLUGIN_HELP)),
        }
    }
    Ok(plugin_invocation(
        "install",
        "plugin.link",
        session,
        PluginAction::Install { source, requested_ref, yes },
    ))
}

fn parse_plugin_action(args: &[String], session: Option<String>) -> Result<Invocation, CliError> {
    let Some(command) = args.first() else {
        return Err(CliError::new("plugin action requires list or invoke", PLUGIN_HELP));
    };
    match command.as_str() {
        "list" => {
            let (plugin_id, json, limit) = parse_plugin_output_flags(&args[1..], false, false)?;
            debug_assert!(!json && limit.is_none());
            Ok(plugin_invocation("action-list", "plugin.action.list", session, PluginAction::ActionList { plugin_id }))
        }
        "invoke" => {
            let action_id = required_positional(args.get(1), "action_id", PLUGIN_HELP)?;
            let (plugin_id, json, limit) = parse_plugin_output_flags(&args[2..], false, false)?;
            debug_assert!(!json && limit.is_none());
            Ok(plugin_invocation(
                "action-invoke",
                "plugin.action.invoke",
                session,
                PluginAction::ActionInvoke { action_id, plugin_id },
            ))
        }
        value => Err(CliError::new(format!("unknown plugin action command '{value}'"), PLUGIN_HELP)),
    }
}

fn parse_plugin_log(args: &[String], session: Option<String>) -> Result<Invocation, CliError> {
    if args.first().map(String::as_str) != Some("list") {
        return Err(CliError::new("plugin log requires list", PLUGIN_HELP));
    }
    let (plugin_id, json, limit) = parse_plugin_output_flags(&args[1..], false, true)?;
    debug_assert!(!json);
    Ok(plugin_invocation("log-list", "plugin.log.list", session, PluginAction::LogList { plugin_id, limit }))
}

fn parse_plugin_pane(args: &[String], session: Option<String>) -> Result<Invocation, CliError> {
    let Some(command) = args.first() else {
        return Err(CliError::new("plugin pane requires open, focus, or close", PLUGIN_HELP));
    };
    match command.as_str() {
        "open" => parse_plugin_pane_open(&args[1..], session),
        "focus" | "close" => {
            if args.len() != 2 { return Err(CliError::new(format!("plugin pane {command} requires <pane_id>"), PLUGIN_HELP)); }
            let action = if command == "focus" {
                PluginAction::PaneFocus { pane_id: args[1].clone() }
            } else {
                PluginAction::PaneClose { pane_id: args[1].clone() }
            };
            Ok(plugin_invocation(&format!("pane-{command}"), &format!("plugin.pane.{command}"), session, action))
        }
        value => Err(CliError::new(format!("unknown plugin pane command '{value}'"), PLUGIN_HELP)),
    }
}

fn parse_plugin_pane_open(args: &[String], session: Option<String>) -> Result<Invocation, CliError> {
    let mut params = Map::new();
    let mut env = Map::new();
    let mut focus = None;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--plugin" => string_flag(args, &mut index, "--plugin", "plugin_id", &mut params, PLUGIN_HELP)?,
            "--entrypoint" => string_flag(args, &mut index, "--entrypoint", "entrypoint", &mut params, PLUGIN_HELP)?,
            "--placement" => {
                let value = flag_value(args, &mut index, "--placement", PLUGIN_HELP)?;
                choice(&value, &["overlay", "popup", "split", "tab", "zoomed"], "--placement", PLUGIN_HELP)?;
                insert_once(&mut params, "placement", Value::String(value), PLUGIN_HELP)?;
            }
            "--width" | "--height" => {
                let flag = args[index].clone();
                let key = flag.trim_start_matches("--").to_owned();
                let value = flag_value(args, &mut index, &flag, PLUGIN_HELP)?;
                insert_once(&mut params, &key, parse_pane_dimension(&value, &flag)?, PLUGIN_HELP)?;
            }
            "--workspace" => string_flag(args, &mut index, "--workspace", "workspace_id", &mut params, PLUGIN_HELP)?,
            "--target-pane" => string_flag(args, &mut index, "--target-pane", "target_pane_id", &mut params, PLUGIN_HELP)?,
            "--direction" => {
                let value = flag_value(args, &mut index, "--direction", PLUGIN_HELP)?;
                choice(&value, &["right", "down"], "--direction", PLUGIN_HELP)?;
                insert_once(&mut params, "direction", Value::String(value), PLUGIN_HELP)?;
            }
            "--cwd" => string_flag(args, &mut index, "--cwd", "cwd", &mut params, PLUGIN_HELP)?,
            "--env" => {
                let value = flag_value(args, &mut index, "--env", PLUGIN_HELP)?;
                let (key, value) = key_value(&value, "--env", PLUGIN_HELP)?;
                env.insert(key, Value::String(value));
            }
            "--focus" => set_exclusive_bool(&mut focus, true, "--focus", "--no-focus", PLUGIN_HELP, &mut index)?,
            "--no-focus" => set_exclusive_bool(&mut focus, false, "--no-focus", "--focus", PLUGIN_HELP, &mut index)?,
            flag if flag.starts_with('-') => return Err(CliError::new(format!("unknown flag '{flag}'"), PLUGIN_HELP)),
            value => return Err(CliError::new(format!("unexpected argument '{value}'"), PLUGIN_HELP)),
        }
    }
    require_key(&params, "plugin_id", "plugin pane open requires --plugin", PLUGIN_HELP)?;
    require_key(&params, "entrypoint", "plugin pane open requires --entrypoint", PLUGIN_HELP)?;
    if !env.is_empty() { params.insert("env".into(), Value::Object(env)); }
    if let Some(value) = focus { params.insert("focus".into(), Value::Bool(value)); }
    Ok(plugin_invocation(
        "pane-open",
        "plugin.pane.open",
        session,
        PluginAction::PaneOpen { params: Value::Object(params) },
    ))
}

fn parse_plugin_output_flags(
    args: &[String],
    allow_json: bool,
    allow_limit: bool,
) -> Result<(Option<String>, bool, Option<u64>), CliError> {
    let mut plugin_id = None;
    let mut json = false;
    let mut limit = None;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--plugin" => {
                let value = flag_value(args, &mut index, "--plugin", PLUGIN_HELP)?;
                validate_plugin_id_argument(&value)?;
                set_once(&mut plugin_id, value, "--plugin", PLUGIN_HELP)?;
            }
            "--json" if allow_json => {
                duplicate_bool(json, "--json", PLUGIN_HELP)?;
                json = true;
                index += 1;
            }
            "--limit" if allow_limit => {
                let value = positive_u64(&flag_value(args, &mut index, "--limit", PLUGIN_HELP)?, "--limit", PLUGIN_HELP)?;
                if limit.replace(value).is_some() {
                    return Err(CliError::new("--limit may only be specified once", PLUGIN_HELP));
                }
            }
            flag if flag.starts_with('-') => return Err(CliError::new(format!("unknown flag '{flag}'"), PLUGIN_HELP)),
            value => return Err(CliError::new(format!("unexpected argument '{value}'"), PLUGIN_HELP)),
        }
    }
    Ok((plugin_id, json, limit))
}

fn parse_github_slug(value: &str) -> Result<GithubSlug, CliError> {
    let segments = value.split('/').collect::<Vec<_>>();
    if segments.len() < 2 || segments.iter().any(|segment| {
        segment.is_empty()
            || matches!(*segment, "." | "..")
            || !segment.bytes().all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    }) {
        return Err(CliError::new(
            "GitHub plugin source must be owner/repo[/subdir...] with safe path segments",
            PLUGIN_HELP,
        ));
    }
    Ok(GithubSlug {
        owner: segments[0].to_owned(),
        repo: segments[1].to_owned(),
        subdir: (segments.len() > 2).then(|| segments[2..].join("/")),
    })
}

fn validate_plugin_id_argument(value: &str) -> Result<(), CliError> {
    if value.is_empty()
        || !value.bytes().all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b':' | b'_' | b'-'))
    {
        return Err(CliError::new(format!("invalid plugin id '{value}'"), PLUGIN_HELP));
    }
    Ok(())
}

fn parse_pane_dimension(value: &str, flag: &str) -> Result<Value, CliError> {
    if let Some(percent) = value.strip_suffix('%') {
        let percent = percent.parse::<u16>().map_err(|_| CliError::new(format!("{flag} requires cells or a percentage"), PLUGIN_HELP))?;
        if !(1..=100).contains(&percent) {
            return Err(CliError::new(format!("{flag} percentage must be between 1% and 100%"), PLUGIN_HELP));
        }
        return Ok(Value::String(format!("{percent}%")));
    }
    let cells = positive_u16(value, flag, PLUGIN_HELP)?;
    Ok(Value::Number(cells.into()))
}

fn plugin_invocation(command: &str, method: &str, session: Option<String>, action: PluginAction) -> Invocation {
    local_invocation("plugin", command, method, Behavior::Plugin { session, action })
}

fn socket_invocation(
    group: &str,
    command: &str,
    method: &str,
    params: Map<String, Value>,
    session: Option<String>,
    output: OutputMode,
) -> Invocation {
    debug_assert!(starcil_protocol::methods::is_known(method), "unknown socket method {method}");
    Invocation {
        request_id: format!("cli:{group}:{command}"),
        method: method.to_owned(),
        params: Value::Object(params),
        behavior: Behavior::Socket { session, output },
    }
}

fn local_invocation(group: &str, command: &str, method: &str, behavior: Behavior) -> Invocation {
    Invocation {
        request_id: format!("cli:{group}:{command}"),
        method: method.to_owned(),
        params: Value::Object(Map::new()),
        behavior,
    }
}

fn help_invocation(group: &str, help: &'static str) -> Invocation {
    local_invocation(group, "help", "local.help", Behavior::Help(help))
}

fn no_args(args: &[String], usage: &'static str) -> Result<(), CliError> {
    if let Some(value) = args.first() {
        let kind = if value.starts_with('-') { "unknown flag" } else { "unexpected argument" };
        return Err(CliError::new(format!("{kind} '{value}'"), usage));
    }
    Ok(())
}

fn required_positional(value: Option<&String>, name: &str, usage: &'static str) -> Result<String, CliError> {
    match value {
        Some(value) if !value.starts_with('-') => Ok(value.clone()),
        Some(value) => Err(CliError::new(format!("expected <{name}>, found flag '{value}'"), usage)),
        None => Err(CliError::new(format!("missing required <{name}>") , usage)),
    }
}

fn one_positional(
    group: &'static str,
    command: &'static str,
    method: &'static str,
    key: &'static str,
    args: &[String],
    session: Option<String>,
    usage: &'static str,
) -> Result<Invocation, CliError> {
    let value = required_positional(args.first(), key, usage)?;
    if args.len() != 1 {
        return Err(CliError::new(format!("{group} {command} accepts exactly one <{key}>") , usage));
    }
    let mut params = Map::new();
    params.insert(key.into(), Value::String(value));
    Ok(socket_invocation(group, command, method, params, session, OutputMode::Json))
}

fn two_positionals(
    group: &'static str,
    command: &'static str,
    method: &'static str,
    first_key: &'static str,
    second_key: &'static str,
    args: &[String],
    session: Option<String>,
    usage: &'static str,
) -> Result<Invocation, CliError> {
    let first = required_positional(args.first(), first_key, usage)?;
    let second = required_positional(args.get(1), second_key, usage)?;
    if args.len() != 2 {
        return Err(CliError::new(format!("{group} {command} requires <{first_key}> <{second_key}>") , usage));
    }
    let mut params = Map::new();
    params.insert(first_key.into(), Value::String(first));
    params.insert(second_key.into(), Value::String(second));
    Ok(socket_invocation(group, command, method, params, session, OutputMode::Json))
}

fn two_level_one_positional(
    group: &'static str,
    command: &'static str,
    method: &'static str,
    key: &'static str,
    args: &[String],
    session: Option<String>,
    usage: &'static str,
) -> Result<Invocation, CliError> {
    one_positional(group, command, method, key, args, session, usage)
}

fn parse_optional_value_command(
    group: &'static str,
    command: &'static str,
    method: &'static str,
    flag: &'static str,
    key: &'static str,
    args: &[String],
    session: Option<String>,
    usage: &'static str,
) -> Result<Invocation, CliError> {
    let mut params = Map::new();
    match args {
        [] => {}
        [actual, value] if actual == flag => {
            params.insert(key.into(), Value::String(value.clone()));
        }
        [actual, ..] if actual.starts_with('-') => return Err(CliError::new(format!("unknown or malformed flag '{actual}'"), usage)),
        [value, ..] => return Err(CliError::new(format!("unexpected argument '{value}'"), usage)),
    }
    Ok(socket_invocation(group, command, method, params, session, OutputMode::Json))
}

fn flag_value(args: &[String], index: &mut usize, flag: &str, usage: &'static str) -> Result<String, CliError> {
    let value = args.get(*index + 1).ok_or_else(|| CliError::new(format!("{flag} requires a value"), usage))?;
    if value.starts_with("--") {
        return Err(CliError::new(format!("{flag} requires a value"), usage));
    }
    *index += 2;
    Ok(value.clone())
}

fn set_once(slot: &mut Option<String>, value: String, flag: &str, usage: &'static str) -> Result<(), CliError> {
    if slot.replace(value).is_some() {
        return Err(CliError::new(format!("{flag} may only be specified once"), usage));
    }
    Ok(())
}

fn insert_once(params: &mut Map<String, Value>, key: &str, value: Value, usage: &'static str) -> Result<(), CliError> {
    if params.insert(key.to_owned(), value).is_some() {
        return Err(CliError::new(format!("--{} may only be specified once", key.replace('_', "-")), usage));
    }
    Ok(())
}

fn require_key(params: &Map<String, Value>, key: &str, message: impl Into<String>, usage: &'static str) -> Result<(), CliError> {
    if !params.contains_key(key) {
        return Err(CliError::new(message, usage));
    }
    Ok(())
}

fn duplicate_bool(already_set: bool, flag: &str, usage: &'static str) -> Result<(), CliError> {
    if already_set {
        return Err(CliError::new(format!("{flag} may only be specified once"), usage));
    }
    Ok(())
}

fn choice(value: &str, choices: &[&str], label: &str, usage: &'static str) -> Result<(), CliError> {
    if !choices.contains(&value) {
        return Err(CliError::new(format!("invalid {label} '{value}'; expected {}", choices.join("|")), usage));
    }
    Ok(())
}

fn positive_u64(value: &str, flag: &str, usage: &'static str) -> Result<u64, CliError> {
    let parsed = value.parse::<u64>().map_err(|_| CliError::new(format!("{flag} requires a positive integer"), usage))?;
    if parsed == 0 {
        return Err(CliError::new(format!("{flag} requires a positive integer"), usage));
    }
    Ok(parsed)
}

fn positive_u16(value: &str, flag: &str, usage: &'static str) -> Result<u16, CliError> {
    let parsed = value.parse::<u16>().map_err(|_| CliError::new(format!("{flag} requires an integer from 1 to 65535"), usage))?;
    if parsed == 0 {
        return Err(CliError::new(format!("{flag} requires an integer from 1 to 65535"), usage));
    }
    Ok(parsed)
}

fn positive_f64(value: &str, flag: &str, usage: &'static str) -> Result<f64, CliError> {
    let parsed = value.parse::<f64>().map_err(|_| CliError::new(format!("{flag} requires a positive number"), usage))?;
    if !parsed.is_finite() || parsed <= 0.0 {
        return Err(CliError::new(format!("{flag} requires a positive number"), usage));
    }
    Ok(parsed)
}

fn unit_f64(value: &str, flag: &str, usage: &'static str) -> Result<f64, CliError> {
    let parsed = positive_f64(value, flag, usage)?;
    if parsed >= 1.0 {
        return Err(CliError::new(format!("{flag} must be greater than 0 and less than 1"), usage));
    }
    Ok(parsed)
}

fn json_number(value: f64, flag: &str, usage: &'static str) -> Result<Value, CliError> {
    Number::from_f64(value)
        .map(Value::Number)
        .ok_or_else(|| CliError::new(format!("{flag} is not a finite JSON number"), usage))
}

fn key_value(value: &str, flag: &str, usage: &'static str) -> Result<(String, String), CliError> {
    let Some((key, value)) = value.split_once('=') else {
        return Err(CliError::new(format!("{flag} requires KEY=VALUE"), usage));
    };
    if key.is_empty() {
        return Err(CliError::new(format!("{flag} key cannot be empty"), usage));
    }
    Ok((key.to_owned(), value.to_owned()))
}

fn set_exclusive_bool(
    slot: &mut Option<bool>,
    value: bool,
    flag: &str,
    conflict: &str,
    usage: &'static str,
    index: &mut usize,
) -> Result<(), CliError> {
    if slot.is_some() {
        return Err(CliError::new(format!("{flag} conflicts with {conflict} or is duplicated"), usage));
    }
    *slot = Some(value);
    *index += 1;
    Ok(())
}

fn set_destination<'a>(
    slot: &mut Option<&'a str>,
    value: &'a str,
    flag: &str,
    usage: &'static str,
) -> Result<(), CliError> {
    if slot.replace(value).is_some() {
        return Err(CliError::new(format!("{flag} conflicts with another pane move destination"), usage));
    }
    Ok(())
}

fn string_flag(
    args: &[String],
    index: &mut usize,
    flag: &str,
    key: &str,
    params: &mut Map<String, Value>,
    usage: &'static str,
) -> Result<(), CliError> {
    let value = flag_value(args, index, flag, usage)?;
    insert_once(params, key, Value::String(value), usage)
}

fn numeric_flag(
    args: &[String],
    index: &mut usize,
    flag: &str,
    key: &str,
    params: &mut Map<String, Value>,
    usage: &'static str,
) -> Result<(), CliError> {
    let raw = flag_value(args, index, flag, usage)?;
    let value = if flag == "--ttl-ms" {
        let value = positive_u64(&raw, flag, usage)?;
        if value > 86_400_000 {
            return Err(CliError::new("--ttl-ms must be between 1 and 86400000", usage));
        }
        value
    } else {
        raw.parse::<u64>().map_err(|_| CliError::new(format!("{flag} requires a non-negative integer"), usage))?
    };
    insert_once(params, key, Value::Number(value.into()), usage)
}

fn parse_optional_bool_flag(args: &[String], flag: &str, usage: &'static str) -> Result<bool, CliError> {
    match args {
        [] => Ok(false),
        [value] if value == flag => Ok(true),
        [value] if value.starts_with('-') => Err(CliError::new(format!("unknown flag '{value}'"), usage)),
        [value] => Err(CliError::new(format!("unexpected argument '{value}'"), usage)),
        _ => Err(CliError::new(format!("{flag} may only be specified once"), usage)),
    }
}

fn agent_status(value: &str, usage: &'static str) -> Result<(), CliError> {
    choice(value, &["idle", "done", "working", "blocked", "unknown"], "agent status", usage)
}

fn validate_agent_name(value: &str, usage: &'static str) -> Result<(), CliError> {
    let mut chars = value.chars();
    let valid_first = chars.next().is_some_and(|ch| ch.is_ascii_lowercase());
    let valid_rest = chars.all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || matches!(ch, '_' | '-'));
    if !valid_first || !valid_rest || value.len() > 32 {
        return Err(CliError::new("agent names must match [a-z][a-z0-9_-]{0,31}", usage));
    }
    Ok(())
}

fn validate_token_name(value: &str, usage: &'static str) -> Result<(), CliError> {
    if value.is_empty()
        || value.len() > 32
        || !value.chars().all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-'))
    {
        return Err(CliError::new(format!("invalid token name '{value}'"), usage));
    }
    Ok(())
}

fn validate_key(value: &str, usage: &'static str) -> Result<(), CliError> {
    if value.chars().count() == 1 && value.chars().all(|ch| !ch.is_control()) {
        return Ok(());
    }
    let lower = value.to_ascii_lowercase();
    let parts = lower.split('+').collect::<Vec<_>>();
    if parts.iter().any(|part| part.is_empty()) {
        return Err(CliError::new(format!("invalid key name '{value}'"), usage));
    }
    let (modifiers, base) = parts.split_at(parts.len().saturating_sub(1));
    if !modifiers.iter().all(|modifier| matches!(*modifier, "ctrl" | "control" | "alt" | "shift")) {
        return Err(CliError::new(format!("invalid key modifier in '{value}'"), usage));
    }
    let base = base.first().copied().unwrap_or_default();
    let named = matches!(
        base,
        "enter" | "tab" | "esc" | "escape" | "backspace" | "left" | "right" | "up" | "down"
            | "home" | "end" | "pageup" | "pagedown" | "insert" | "delete" | "space"
            | "minus" | "plus" | "backtick"
    );
    let function = base.strip_prefix('f').and_then(|number| number.parse::<u8>().ok()).is_some_and(|number| (1..=24).contains(&number));
    let printable = base.chars().count() == 1 && base.chars().all(|ch| !ch.is_control());
    if named || function || printable {
        Ok(())
    } else {
        Err(CliError::new(format!("invalid key name '{value}'"), usage))
    }
}
