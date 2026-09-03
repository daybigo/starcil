<h1 align="center">Starcil</h1>
<p align="center"><em>The terminal workspace for AI coding agents.</em></p>
<p align="center">
  <a href="https://daybigo.github.io/starcil/">Website</a> ·
  <a href="https://github.com/daybigo/starcil/releases/latest">Download</a> ·
  <a href="https://github.com/daybigo/starcil/actions/workflows/ci.yml"><img alt="CI" src="https://github.com/daybigo/starcil/actions/workflows/ci.yml/badge.svg"></a>
  <a href="https://github.com/daybigo/starcil/releases/latest"><img alt="Release" src="https://img.shields.io/github/v/release/daybigo/starcil"></a>
</p>

Starcil is a terminal multiplexer built around coding agents. It arranges terminals into
**workspaces, tabs and panes**, recognizes the CLI agents running inside them (Claude Code,
Codex, Gemini, OpenCode, Copilot and more), shows who is working, idle or waiting on a
question, and exposes the whole session through a `starcil` command line and a socket API,
so an agent inside one pane can spawn, brief and wait for agents in the others.

```
 ◫  Workspaces        │ starcil  api   +
 1 starcil        ●   │╭ claude ────────────────────────────╮
   api                ││ ✴ Read src/composer.rs              │
                      ││ ✴ Edit composer.rs                  │
                      ││                                     │
                      ││ ❯ run the e2e, fix what fails       │
 new           menu   ││                                     │
──────────────────────│╰─────────────────────────────────────╯
        agents        │╭ codex ─────────────────────────────╮
 ⠹ reviewer·claude    ││ • Working (2m 14s · esc to stop)    │
 ✓ worker·codex       ││                                     │
                      │╰─────────────────────────────────────╯
```

## Install

Windows 10/11, x86_64. One command downloads the latest release, verifies its SHA-256
checksum, puts `starcil` on your `PATH` and leaves your config alone. Run it again to upgrade.

```powershell
irm https://daybigo.github.io/starcil/install.ps1 | iex
```

Prefer a file? Grab `starcil-x86_64-pc-windows-gnu.zip` from the
[latest release](https://github.com/daybigo/starcil/releases/latest), unpack `starcil.exe`
anywhere on your `PATH`. `SHA256SUMS` next to it covers every asset.

Linux and macOS are not there yet: the server and client talk over Windows named pipes
today, and the Unix socket transport is next on the list.

## Quick start

```powershell
starcil                  # open the default session (starts its server if needed)
starcil --session work   # an independent server and workspace tree
```

- A shell pane has **one place to type**: the `❯` composer at its bottom. Tab completes
  paths against the pane's live directory and commands from `PATH`; `↑`/`↓` walk a history
  seeded from your shell's own; `Ctrl+R` searches it; `Ctrl+V` pastes into the draft.
  The prompt above only shows what ran. While a program (vim, a script asking a question)
  runs in the pane, keys go to it until the prompt returns.
- The **dock** in the composer launches the agents found on your `PATH` (`Alt+1`…`9`).
  Once a CLI agent is recognized in a pane the composer hides: the agent has its own.
- The **prefix** is `Ctrl+B`: then `v` splits right, `-` splits down, `h/j/k/l` move,
  `z` zooms, `c` opens a tab, `b` toggles the sidebar, `q` detaches, `?` lists everything.
  Tabs can be dragged to reorder; panes resize with the mouse.
- Sessions **persist**: close the terminal and the server keeps the panes; the next
  `starcil` restores them, each in the directory it was in.

## For agents

An agent running inside a Starcil pane can drive the session with the same CLI you use:

```powershell
starcil pane split --current --direction right --no-focus
starcil agent start reviewer --kind codex --pane w1:p2
starcil agent prompt reviewer "review the diff in this repo, write findings to REVIEW.txt" --wait
starcil pane read w1:p2 --source recent --lines 40
starcil agent wait reviewer --until idle --timeout 600000
```

`starcil --skill` prints the skill an agent needs to work this way: the mechanics, how to
run a small fleet (roles, briefs, waiting on files instead of screens) and the platform
gotchas. Install it once for Claude Code with the `skills` CLI, or paste the output into
any agent's instructions:

```powershell
npx skills add daybigo/starcil --skill starcil -g
```

Claude Code and Codex can report their state precisely through hooks instead of screen
detection: `starcil integration install claude` / `codex`. Every other command group
(`workspace`, `tab`, `pane`, `agent`, `worktree`, `terminal`, `session`, `api`, `config`,
`notification`, `plugin`…) is listed by `starcil --help`; `starcil api` speaks the socket
protocol directly as NDJSON.

## Configuration

`%APPDATA%\starcil\config.toml` (`starcil --default-config` prints every key with its
default; `starcil config check` validates the file). Some keys worth knowing:

| Key | What it does |
| --- | --- |
| `terminal.default_shell` | `""` picks `pwsh.exe`, else `powershell.exe`; set `cmd.exe` explicitly if you want it |
| `ui.dock.agents` | which CLIs the dock offers, in order |
| `theme.name`, `[theme.custom]` | 12 built-in themes plus per-token overrides |
| `keys.*` | the whole keymap; `starcil config reset-keys` backs up and clears customizations |
| `update.channel` | `stable` (default) or `preview` |

Starcil checks GitHub for a new release on start and asks before installing it;
`starcil update` does it on demand.

## Building from source

Rust stable with the `x86_64-pc-windows-gnu` target (see `rust-toolchain.toml`) and a GNU
toolchain on `PATH` (GitHub's Windows runners have one; locally, w64devkit works):

```powershell
cargo build --release -p starcil
cargo test --workspace
```

The binary is `target/release/starcil.exe`. `packaging/verify-install.ps1` installs a
local build through the real installer into an isolated profile and cleans up after itself.

## License

MIT — see [LICENSE](LICENSE).
