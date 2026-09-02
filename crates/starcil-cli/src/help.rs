use crate::CompletionShell;

const fn indented_len(source: &str) -> usize {
    let bytes = source.as_bytes();
    let mut index = 0;
    let mut length = bytes.len();
    while index + 1 < bytes.len() {
        if bytes[index] == b'\n' {
            length += 2;
        }
        index += 1;
    }
    length
}

const fn indent<const N: usize>(source: &str) -> [u8; N] {
    let bytes = source.as_bytes();
    let mut output = [0; N];
    let mut source_index = 0;
    let mut output_index = 0;
    while source_index < bytes.len() {
        output[output_index] = bytes[source_index];
        output_index += 1;
        if bytes[source_index] == b'\n' && source_index + 1 < bytes.len() {
            output[output_index] = b' ';
            output[output_index + 1] = b' ';
            output_index += 2;
        }
        source_index += 1;
    }
    output
}

macro_rules! indented_help {
    ($source:expr) => {{
        const SOURCE: &str = $source;
        const LENGTH: usize = indented_len(SOURCE);
        const BYTES: [u8; LENGTH] = indent::<LENGTH>(SOURCE);
        // SAFETY: SOURCE is UTF-8 and indentation adds only ASCII spaces.
        unsafe { std::str::from_utf8_unchecked(&BYTES) }
    }};
}

pub const ROOT_HELP: &str = indented_help!("starcil commands:\n\
  starcil [--session NAME] [--remote TARGET] [--remote-keybindings local|server] [--handoff]\n\
  starcil --no-session\n\
  starcil status [server|client]\n\
  starcil update [--handoff]\n\
  starcil server [stop|reload-config]\n\
  starcil completion zsh|bash|powershell\n\
  starcil --default-config\n\
  starcil --skill\n\
  starcil --version\n\
  starcil <group>\n\
groups: agent pane workspace tab worktree terminal notification integration session api config channel plugin\n");

pub const AGENT_HELP: &str = indented_help!("starcil agent commands:\n\
  starcil agent list\n\
  starcil agent get <target>\n\
  starcil agent read <target> [--source visible|recent|recent-unwrapped|detection] [--lines N] [--format text|ansi] [--ansi]\n\
  starcil agent send-keys <target> <key> [key ...]\n\
  starcil agent prompt <target> <text> [--wait] [--until STATUS]... [--timeout MS]\n\
  starcil agent rename <target> <name>|--clear\n\
  starcil agent focus <target>\n\
  starcil agent wait <target> [--until STATUS]... [--timeout MS]\n\
  starcil agent attach <target> [--takeover]\n\
  starcil agent start <name> --kind KIND --pane ID [--timeout MS] [-- <agent-args...>]\n\
  starcil agent explain <target> [--json|--format text|json] [--verbose]\n\
  starcil agent explain --file PATH --agent LABEL [--json|--format text|json] [--verbose]\n\
  targets accept unique agent names and pane ids that currently host agents\n\
  kinds: pi|claude|codex|gemini|cursor|devin|agy|cline|omp|mastracode|opencode|copilot|kimi|kiro|droid|amp|grok|hermes|kilo|qodercli|maki\n");

pub const PANE_HELP: &str = indented_help!("starcil pane commands:\n\
  starcil pane list [--workspace <workspace_id>]\n\
  starcil pane current [--pane ID|--current]\n\
  starcil pane get <pane_id>\n\
  starcil pane layout [--pane ID|--current]\n\
  starcil pane process-info [--pane ID|--current]\n\
  starcil pane neighbor --direction left|right|up|down [--pane ID|--current]\n\
  starcil pane edges [--pane ID|--current]\n\
  starcil pane focus --direction left|right|up|down [--pane ID|--current]\n\
  starcil pane resize --direction left|right|up|down [--amount FLOAT] [--pane ID|--current]\n\
  starcil pane zoom [<pane_id>|--pane ID|--current] [--toggle|--on|--off]\n\
  starcil pane rename <pane_id> <label>|--clear\n\
  starcil pane read <pane_id> [--source visible|recent|recent-unwrapped] [--lines N] [--format text|ansi] [--ansi]\n\
  starcil pane split [<pane_id>|--pane ID|--current] --direction right|down [--ratio FLOAT] [--cwd PATH] [--env KEY=VALUE] [--focus] [--no-focus]\n\
  starcil pane swap --direction left|right|up|down [--pane ID|--current]\n\
  starcil pane swap --source-pane ID --target-pane ID\n\
  starcil pane move <pane_id> --tab <tab_id> --split right|down [--target-pane ID] [--ratio FLOAT] [--focus|--no-focus]\n\
  starcil pane move <pane_id> --new-tab [--workspace ID] [--label TEXT] [--focus|--no-focus]\n\
  starcil pane move <pane_id> --new-workspace [--label TEXT] [--tab-label TEXT] [--focus|--no-focus]\n\
  starcil pane close <pane_id>\n\
  starcil pane send-text <pane_id> <text>\n\
  starcil pane send-keys <pane_id> <key> [key ...]\n\
  starcil pane wait-output <pane_id> (--match TEXT | --regex PATTERN) [--source visible|recent|recent-unwrapped] [--lines N] [--timeout MS] [--raw]\n\
  starcil pane report-agent <pane_id> --source ID --agent LABEL --state idle|working|blocked|unknown [--message TEXT] [--seq N] [--agent-session-id ID] [--agent-session-path PATH]\n\
  starcil pane report-agent-session <pane_id> --source ID --agent LABEL [--seq N] [--agent-session-id ID] [--agent-session-path PATH]\n\
  starcil pane release-agent <pane_id> --source ID --agent LABEL [--seq N]\n\
  starcil pane report-metadata <pane_id> --source ID [--agent LABEL] [--applies-to-source ID] [--title TEXT|--clear-title] [--display-agent TEXT|--clear-display-agent] [--state-label STATUS=TEXT] [--clear-state-labels] [--token NAME=VALUE] [--clear-token NAME] [--seq N] [--ttl-ms N]\n\
  starcil pane run <pane_id> <command>\n");

pub const WORKSPACE_HELP: &str = indented_help!("starcil workspace commands:\n\
  starcil workspace list\n\
  starcil workspace create [--cwd PATH] [--label TEXT] [--env KEY=VALUE] [--focus] [--no-focus]\n\
  starcil workspace get <workspace_id>\n\
  starcil workspace focus <workspace_id>\n\
  starcil workspace rename <workspace_id> <label>\n\
  starcil workspace report-metadata <workspace_id> --source ID [--token NAME=VALUE] [--clear-token NAME] [--seq N] [--ttl-ms N]\n\
  starcil workspace close <workspace_id>\n");

pub const TAB_HELP: &str = indented_help!("starcil tab commands:\n\
  starcil tab list [--workspace <workspace_id>]\n\
  starcil tab create [--workspace <workspace_id>] [--cwd PATH] [--label TEXT] [--env KEY=VALUE] [--focus] [--no-focus]\n\
  starcil tab get <tab_id>\n\
  starcil tab focus <tab_id>\n\
  starcil tab rename <tab_id> <label>\n\
  starcil tab close <tab_id>\n");

pub const WORKTREE_HELP: &str = indented_help!("starcil worktree commands:\n\
  starcil worktree list [--workspace ID | --cwd PATH] [--json]\n\
  starcil worktree create [--workspace ID | --cwd PATH] [--branch NAME] [--base REF] [--path PATH] [--label TEXT] [--focus] [--no-focus] [--json]\n\
  starcil worktree open [--workspace ID | --cwd PATH] (--path PATH | --branch NAME) [--label TEXT] [--focus] [--no-focus] [--json]\n\
  starcil worktree remove --workspace ID [--force] [--json]\n");

pub const TERMINAL_HELP: &str = indented_help!("starcil terminal commands:\n\
  starcil terminal attach <terminal_id> [--takeover]\n\
  starcil terminal session control <target> [--takeover] [--cols N] [--rows N]\n\
  starcil terminal session observe <target> [--cols N] [--rows N]\n\
  starcil terminal title set <title>\n\
  starcil terminal title clear\n\
  detach from direct attach with ctrl+b q; send literal ctrl+b with ctrl+b ctrl+b\n");

pub const NOTIFICATION_HELP: &str = indented_help!("starcil notification commands:\n\
  starcil notification show <title> [--body TEXT] [--position top-left|top-right|bottom-left|bottom-right] [--sound none|done|request]\n");

pub const INTEGRATION_HELP: &str = indented_help!("starcil integration commands:\n\
  starcil integration install pi\n\
  starcil integration install omp\n\
  starcil integration install claude\n\
  starcil integration install codex\n\
  starcil integration install copilot\n\
  starcil integration install devin\n\
  starcil integration install droid\n\
  starcil integration install kimi\n\
  starcil integration install opencode\n\
  starcil integration install kilo\n\
  starcil integration install hermes\n\
  starcil integration install qodercli\n\
  starcil integration install cursor\n\
  starcil integration install mastracode\n\
  starcil integration uninstall pi\n\
  starcil integration uninstall omp\n\
  starcil integration uninstall claude\n\
  starcil integration uninstall codex\n\
  starcil integration uninstall copilot\n\
  starcil integration uninstall devin\n\
  starcil integration uninstall droid\n\
  starcil integration uninstall kimi\n\
  starcil integration uninstall opencode\n\
  starcil integration uninstall kilo\n\
  starcil integration uninstall hermes\n\
  starcil integration uninstall qodercli\n\
  starcil integration uninstall cursor\n\
  starcil integration uninstall mastracode\n\
  starcil integration status [--outdated-only]\n");

pub const SESSION_HELP: &str = indented_help!("starcil session commands:\n\
  starcil session list [--json]\n\
  starcil session attach <name>\n\
  starcil session stop <name> [--json]\n\
  starcil session delete <name> [--json]\n\
  use 'default' as <name> to target the default session for stop\n");

pub const API_HELP: &str = indented_help!("starcil api commands:\n\
  starcil api snapshot\n\
  starcil api schema [--json | --output PATH]\n");

pub const CONFIG_HELP: &str = indented_help!("starcil config commands:\n\
  starcil config check  validate config.toml and print diagnostics\n\
  starcil config reset-keys  back up config.toml and remove custom keybindings\n");

pub const CHANNEL_HELP: &str = indented_help!("starcil channel commands:\n\
  starcil channel show                  print the configured update channel\n\
  starcil channel set <stable|preview>  choose the update channel\n");

pub const PLUGIN_HELP: &str = indented_help!("starcil plugin commands:\n\
  starcil plugin install <owner>/<repo>[/subdir...] [--ref REF] [--yes]\n\
  starcil plugin uninstall <plugin_id|owner/repo[/subdir...]>\n\
  starcil plugin link <path> [--disabled]\n\
  starcil plugin list [--plugin ID] [--json]\n\
  starcil plugin config-dir <plugin_id>\n\
  starcil plugin unlink <plugin_id>\n\
  starcil plugin enable <plugin_id>\n\
  starcil plugin disable <plugin_id>\n\
  starcil plugin action <list|invoke>\n\
  starcil plugin log list [--plugin ID] [--limit N]\n\
  starcil plugin pane <open|focus|close>\n");

pub const SERVER_HELP: &str = indented_help!("starcil server commands:\n\
  starcil server\n\
  starcil server stop\n\
  starcil server reload-config\n");

#[derive(Debug, Clone, Copy)]
pub struct CommandGroup {
    pub name: &'static str,
    pub subcommands: &'static [&'static str],
    pub help: &'static str,
}

pub const COMMAND_GROUPS: &[CommandGroup] = &[
    CommandGroup { name: "agent", subcommands: &["list", "get", "read", "send-keys", "prompt", "rename", "focus", "wait", "attach", "start", "explain"], help: AGENT_HELP },
    CommandGroup { name: "pane", subcommands: &["list", "current", "get", "layout", "process-info", "neighbor", "edges", "focus", "resize", "zoom", "rename", "read", "split", "swap", "move", "close", "send-text", "send-keys", "wait-output", "report-agent", "report-agent-session", "release-agent", "report-metadata", "run"], help: PANE_HELP },
    CommandGroup { name: "workspace", subcommands: &["list", "create", "get", "focus", "rename", "report-metadata", "close"], help: WORKSPACE_HELP },
    CommandGroup { name: "tab", subcommands: &["list", "create", "get", "focus", "rename", "close"], help: TAB_HELP },
    CommandGroup { name: "worktree", subcommands: &["list", "create", "open", "remove"], help: WORKTREE_HELP },
    CommandGroup { name: "terminal", subcommands: &["attach", "session", "control", "observe", "title", "set", "clear"], help: TERMINAL_HELP },
    CommandGroup { name: "notification", subcommands: &["show"], help: NOTIFICATION_HELP },
    CommandGroup { name: "integration", subcommands: &["install", "uninstall", "status"], help: INTEGRATION_HELP },
    CommandGroup { name: "session", subcommands: &["list", "attach", "stop", "delete"], help: SESSION_HELP },
    CommandGroup { name: "api", subcommands: &["snapshot", "schema"], help: API_HELP },
    CommandGroup { name: "config", subcommands: &["check", "reset-keys"], help: CONFIG_HELP },
    CommandGroup { name: "channel", subcommands: &["show", "set"], help: CHANNEL_HELP },
    CommandGroup { name: "plugin", subcommands: &["install", "uninstall", "link", "list", "config-dir", "unlink", "enable", "disable", "action", "log", "pane", "invoke", "open", "focus", "close"], help: PLUGIN_HELP },
];

pub fn group_help(group: &str) -> Option<&'static str> {
    COMMAND_GROUPS.iter().find(|item| item.name == group).map(|item| item.help)
}

pub fn completion_script(shell: CompletionShell) -> String {
    let groups = COMMAND_GROUPS.iter().map(|group| group.name).collect::<Vec<_>>().join(" ");
    let mut all_words = vec![
        "status", "update", "server", "stop", "reload-config", "completion", "zsh", "bash",
        "powershell", "stable", "preview",
    ];
    for group in COMMAND_GROUPS {
        all_words.push(group.name);
        all_words.extend(group.subcommands.iter().copied());
    }
    all_words.sort_unstable();
    all_words.dedup();
    let words = all_words.join(" ");

    match shell {
        CompletionShell::Bash => format!(
            "_starcil() {{\n  local cur=\"${{COMP_WORDS[COMP_CWORD]}}\"\n  COMPREPLY=( $(compgen -W \"{words}\" -- \"$cur\") )\n}}\ncomplete -F _starcil starcil\n"
        ),
        CompletionShell::Zsh => format!(
            "#compdef starcil\n_arguments '1:group:({groups} status update server completion)' '*:command:({words})'\n"
        ),
        CompletionShell::PowerShell => format!(
            "Register-ArgumentCompleter -Native -CommandName starcil -ScriptBlock {{\n    param($wordToComplete)\n    '{words}'.Split(' ') | Where-Object {{ $_ -like \"$wordToComplete*\" }} | ForEach-Object {{ [System.Management.Automation.CompletionResult]::new($_, $_, 'ParameterValue', $_) }}\n}}\n"
        ),
    }
}
