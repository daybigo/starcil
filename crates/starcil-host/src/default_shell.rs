//! Which shell a new interactive pane gets when `terminal.default_shell` is empty.
//!
//! Windows: PowerShell 7 (`pwsh.exe`) when it is on PATH, otherwise Windows
//! PowerShell (`powershell.exe`). `COMSPEC` is deliberately ignored: Windows always
//! sets it and it always points at cmd.exe, so consulting it first made the
//! PowerShell fallback unreachable and every pane opened cmd (no `ls`, no aliases,
//! no tab completion worth the name) — the opposite of what the docs promised.
//! Unix: `$SHELL` (the user's login shell), then the first shell that exists in
//! a per-OS preference list (zsh first on macOS, bash first on Linux), then `/bin/sh`.
//!
//! The resolver takes the environment and the filesystem as closures so the
//! policy is unit-tested without spawning anything.

use std::path::{Path, PathBuf};

/// Operating-system family the policy is being resolved for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShellOs {
    Windows,
    MacOs,
    Linux,
}

impl ShellOs {
    pub fn current() -> Self {
        if cfg!(windows) {
            Self::Windows
        } else if cfg!(target_os = "macos") {
            Self::MacOs
        } else {
            Self::Linux
        }
    }
}

/// Resolve the default shell using the process environment and the real filesystem.
pub fn resolve_default_shell() -> String {
    resolve_with(
        ShellOs::current(),
        &|key| std::env::var(key).ok(),
        &|path| path.is_file(),
    )
}

/// Testable core: `env` reads an environment variable, `is_file` probes the disk.
pub fn resolve_with(
    os: ShellOs,
    env: &dyn Fn(&str) -> Option<String>,
    is_file: &dyn Fn(&Path) -> bool,
) -> String {
    match os {
        ShellOs::Windows => resolve_windows(env, is_file),
        ShellOs::MacOs => resolve_unix(&["/bin/zsh", "/bin/bash", "/usr/bin/bash"], env, is_file),
        ShellOs::Linux => resolve_unix(
            &["/bin/bash", "/usr/bin/bash", "/bin/zsh", "/usr/bin/zsh"],
            env,
            is_file,
        ),
    }
}

const WINDOWS_POWERSHELL: &str = "powershell.exe";
const POWERSHELL_7: &str = "pwsh.exe";

fn resolve_windows(env: &dyn Fn(&str) -> Option<String>, is_file: &dyn Fn(&Path) -> bool) -> String {
    let path = env("PATH").unwrap_or_default();
    // Joined by hand with a backslash: this policy describes Windows even
    // when it is unit-tested on Linux, where `Path::join` would use `/`.
    path.split(';')
        .map(str::trim)
        .filter(|directory| !directory.is_empty())
        .map(|directory| {
            let directory = directory.trim_end_matches(['/', '\\']);
            PathBuf::from(format!("{directory}\\{POWERSHELL_7}"))
        })
        .find(|candidate| is_file(candidate))
        .map(|found| found.to_string_lossy().into_owned())
        .unwrap_or_else(|| WINDOWS_POWERSHELL.to_owned())
}

fn resolve_unix(
    preferred: &[&str],
    env: &dyn Fn(&str) -> Option<String>,
    is_file: &dyn Fn(&Path) -> bool,
) -> String {
    if let Some(shell) = env("SHELL") {
        let shell = shell.trim();
        if !shell.is_empty() {
            return shell.to_owned();
        }
    }
    preferred
        .iter()
        .find(|candidate| is_file(Path::new(candidate)))
        .map(|candidate| (*candidate).to_owned())
        .unwrap_or_else(|| "/bin/sh".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn env_of(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: BTreeMap<String, String> = pairs
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect();
        move |key| map.get(key).cloned()
    }

    fn files_of(paths: &[&str]) -> impl Fn(&Path) -> bool {
        let set: Vec<PathBuf> = paths.iter().map(PathBuf::from).collect();
        move |path| set.iter().any(|known| known == path)
    }

    #[test]
    fn windows_prefers_powershell_7_when_it_is_on_path() {
        let env = env_of(&[
            ("COMSPEC", r"C:\WINDOWS\system32\cmd.exe"),
            (
                "PATH",
                r"C:\WINDOWS\system32; C:\Program Files\PowerShell\7 ;C:\tools",
            ),
        ]);
        let files = files_of(&[r"C:\Program Files\PowerShell\7\pwsh.exe"]);
        assert_eq!(
            resolve_with(ShellOs::Windows, &env, &files),
            r"C:\Program Files\PowerShell\7\pwsh.exe"
        );
    }

    #[test]
    fn windows_falls_back_to_windows_powershell_and_never_to_comspec() {
        let env = env_of(&[
            ("COMSPEC", r"C:\WINDOWS\system32\cmd.exe"),
            ("PATH", r"C:\WINDOWS\system32;C:\WINDOWS"),
        ]);
        let files = files_of(&[r"C:\WINDOWS\system32\cmd.exe"]);
        assert_eq!(resolve_with(ShellOs::Windows, &env, &files), "powershell.exe");
    }

    #[test]
    fn windows_without_path_still_resolves_windows_powershell() {
        let env = env_of(&[("COMSPEC", r"C:\WINDOWS\system32\cmd.exe")]);
        let files = files_of(&[]);
        assert_eq!(resolve_with(ShellOs::Windows, &env, &files), "powershell.exe");
    }

    #[test]
    fn unix_honors_shell_env_before_probing_disk() {
        let env = env_of(&[("SHELL", "/usr/bin/fish")]);
        let files = files_of(&["/bin/bash", "/bin/zsh"]);
        assert_eq!(resolve_with(ShellOs::Linux, &env, &files), "/usr/bin/fish");
        assert_eq!(resolve_with(ShellOs::MacOs, &env, &files), "/usr/bin/fish");
    }

    #[test]
    fn unix_ignores_blank_shell_env() {
        let env = env_of(&[("SHELL", "   ")]);
        let files = files_of(&["/bin/bash"]);
        assert_eq!(resolve_with(ShellOs::Linux, &env, &files), "/bin/bash");
    }

    #[test]
    fn linux_without_shell_prefers_bash_then_zsh_then_sh() {
        let env = env_of(&[]);
        assert_eq!(
            resolve_with(ShellOs::Linux, &env, &files_of(&["/bin/zsh", "/usr/bin/bash"])),
            "/usr/bin/bash"
        );
        assert_eq!(
            resolve_with(ShellOs::Linux, &env, &files_of(&["/usr/bin/zsh"])),
            "/usr/bin/zsh"
        );
        assert_eq!(resolve_with(ShellOs::Linux, &env, &files_of(&[])), "/bin/sh");
    }

    #[test]
    fn macos_without_shell_prefers_zsh() {
        let env = env_of(&[]);
        let files = files_of(&["/bin/zsh", "/bin/bash"]);
        assert_eq!(resolve_with(ShellOs::MacOs, &env, &files), "/bin/zsh");
    }
}

/// PowerShell's prompt hook: the process's own directory never follows `cd`
/// (PowerShell keeps a per-runspace location), so the default shell is
/// started with a `prompt` wrapper that announces the location through OSC
/// `9;9` — the sequence Windows Terminal and ConEmu use — before whatever
/// the user's profile prints. Single quotes only: the script travels as one
/// command-line argument.
pub const POWERSHELL_CWD_HOOK: &str = "$global:__StarcilPrompt = $function:prompt; function global:prompt { [string]::Concat([char]27, ']9;9;', $ExecutionContext.SessionState.Path.CurrentLocation.ProviderPath, [char]7, (& $global:__StarcilPrompt)) }";

/// Extra arguments for the default interactive shell. Only PowerShell
/// (`powershell.exe`, `pwsh`) gets any: the cwd hook above, kept interactive
/// with `-NoExit`. Explicit `terminal.default_shell` values still count —
/// the user picked the shell, not its plumbing.
pub fn startup_args(program: &str) -> Vec<String> {
    // Split on both separators by hand: a Windows path stays recognizable
    // when this runs (or is tested) on Linux.
    let file_name = program
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(program)
        .to_ascii_lowercase();
    let stem = file_name
        .rsplit_once('.')
        .map(|(stem, _)| stem.to_owned())
        .unwrap_or(file_name);
    if stem == "powershell" || stem == "pwsh" {
        vec![
            "-NoExit".to_owned(),
            "-Command".to_owned(),
            POWERSHELL_CWD_HOOK.to_owned(),
        ]
    } else {
        Vec::new()
    }
}

#[cfg(test)]
mod startup_tests {
    use super::*;

    #[test]
    fn only_powershell_gets_the_cwd_hook() {
        for program in ["powershell.exe", "pwsh", r"C:\Program Files\PowerShell\7\pwsh.exe", "PowerShell.EXE"] {
            let args = startup_args(program);
            assert_eq!(args.len(), 3, "{program}");
            assert_eq!(args[0], "-NoExit");
            assert_eq!(args[1], "-Command");
            assert!(args[2].contains("]9;9;"), "{program}");
            assert!(!args[2].contains('"'), "the hook must survive argv quoting");
        }
        for program in ["cmd.exe", "/bin/bash", "/usr/bin/zsh", "nu.exe", ""] {
            assert!(startup_args(program).is_empty(), "{program}");
        }
    }
}
