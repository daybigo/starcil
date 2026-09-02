//! Hand-rolled command-line parser and NDJSON dispatcher for Starcil.

mod connection;
mod flows;
mod help;
mod hooks;
mod parser;
mod plugin_runtime;
mod runtime;
mod schema;
mod terminal;

pub use connection::{endpoint_for, Connection, EndpointSelection, NdjsonConnection};
pub use help::{completion_script, group_help, CommandGroup, COMMAND_GROUPS, ROOT_HELP};

/// The agent skill shipped with this binary (`skills/starcil/SKILL.md`),
/// printed by `starcil --skill` so agents can be taught without a network.
pub const BUNDLED_SKILL: &str = include_str!("../../../skills/starcil/SKILL.md");
pub use parser::{
    parse, Behavior, ChannelAction, CliError, CompletionShell, ConfigAction, Invocation,
    GithubSlug, IntegrationHookAction, LaunchClient, OutputMode, PluginAction, PluginTarget,
    SchemaOutput, SessionAction, TerminalAction, UpdateAction,
};
pub use flows::{
    configured_channel, delete_session_directories, discover_sessions, perform_update,
    session_state_root, set_channel_at, SessionInfo, UpdateFlowOutcome,
};
pub use hooks::run_hook_with;
pub use runtime::{dispatch, dispatch_with};
pub use schema::{api_schema, method_groups};
