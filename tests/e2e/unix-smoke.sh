#!/usr/bin/env bash
# Unix smoke test (no human), Linux and macOS: a real server on a Unix
# socket, a real shell pane (bash on Linux, zsh on macOS: each OS's default),
# the CLI round-trip, the live cwd and shell_idle read from the process table
# (/proc on Linux, libproc on macOS), and the TUI running nested inside a
# pane of a second server, driven through the outer pane's PTY (the same
# trick as the Windows harnesses).
#
# Usage: bash tests/e2e/unix-smoke.sh [path/to/starcil]
# Prints PASS/FAIL per check; exits 1 on any FAIL. Runs under a throwaway
# HOME so nothing of the real user is touched.
set -uo pipefail

EXE=$(cd "$(dirname "${1:-target/release/starcil}")" && pwd -P)/$(basename "${1:-target/release/starcil}")
OUTER=lsouter
INNER=lsinner
OS=$(uname -s)

# Always /tmp: a Unix socket path is capped at 104 bytes on macOS and the
# per-user $TMPDIR there is already 50 of them.
WORK=$(mktemp -d /tmp/starcil-smoke.XXXXXX)
export HOME="$WORK/home"
export XDG_CONFIG_HOME="$HOME/.config"
export XDG_DATA_HOME="$HOME/.local/share"
mkdir -p "$XDG_CONFIG_HOME/starcil" "$HOME/proj"
if [ "$OS" = Darwin ]; then
    # A Mac has no XDG_RUNTIME_DIR: the socket goes under ~/.starcil, and the
    # default shell is zsh. An empty .zshrc keeps zsh-newuser-install quiet.
    unset XDG_RUNTIME_DIR
    SOCK_DIR="$HOME/.starcil"
    SHELL_BIN=/bin/zsh
    : > "$HOME/.zshrc"
    perm_of() { stat -f %Lp "$1"; }
else
    export XDG_RUNTIME_DIR="$WORK/run"
    mkdir -p "$XDG_RUNTIME_DIR"
    SOCK_DIR="$XDG_RUNTIME_DIR/starcil"
    SHELL_BIN=/bin/bash
    perm_of() { stat -c %a "$1"; }
fi
cat > "$XDG_CONFIG_HOME/starcil/config.toml" <<EOF
onboarding = false

[terminal]
default_shell = "$SHELL_BIN"

[experimental]
allow_nested = true

[ui.dock]
agents = ["bash"]
EOF
unset STARCIL_ENV STARCIL_SESSION
export TERM=xterm-256color

pass=0
fail=0
check() {
    if [ "$2" = 1 ]; then echo "PASS $1"; pass=$((pass + 1)); else echo "FAIL $1"; fail=$((fail + 1)); fi
}
cli() { local session=$1; shift; "$EXE" --session "$session" "$@" 2>/dev/null; }
field() { grep -o "\"$1\":[^,}]*" | head -1 | sed 's/^"[^"]*"://; s/^"//; s/"$//'; }
wait_for() { # session pane needle [seconds]
    local deadline=$((SECONDS + ${4:-15}))
    while [ $SECONDS -lt $deadline ]; do
        if cli "$1" pane read "$2" --source recent | grep -q "$3"; then return 0; fi
        sleep 0.25
    done
    return 1
}
wait_server() {
    for _ in $(seq 1 60); do
        if cli "$1" status >/dev/null 2>&1; then return 0; fi
        sleep 0.25
    done
    return 1
}

cleanup() {
    cli "$INNER" server stop >/dev/null 2>&1 || true
    cli "$OUTER" server stop >/dev/null 2>&1 || true
    sleep 0.5
    kill "$SERVER_PID" 2>/dev/null || true
    rm -rf "$WORK"
}
trap cleanup EXIT

# --- 1. server on a Unix socket --------------------------------------------
cd "$HOME"
"$EXE" --session "$OUTER" server >"$WORK/server.log" 2>&1 &
SERVER_PID=$!
wait_server "$OUTER"; check "server answers over the unix socket" $([ $? = 0 ] && echo 1 || echo 0)
sock="$SOCK_DIR/$OUTER.sock"
[ -S "$sock" ]; check "socket file lives in the runtime dir ($sock)" $([ $? = 0 ] && echo 1 || echo 0)
perm=$(perm_of "$sock" 2>/dev/null || echo "?")
[ "$perm" = 600 ]; check "socket is owner-only (0600, got $perm)" $([ $? = 0 ] && echo 1 || echo 0)

# --- 2. a real shell pane, CLI round-trip ------------------------------------
pane=$(cli "$OUTER" pane list | field pane_id)
[ -n "$pane" ]; check "pane list names a pane ($pane)" $([ $? = 0 ] && echo 1 || echo 0)
cli "$OUTER" pane send-text "$pane" 'echo UNIX-OK-$((6*7))' >/dev/null
cli "$OUTER" pane send-keys "$pane" enter >/dev/null
wait_for "$OUTER" "$pane" "UNIX-OK-42"; check "$(basename "$SHELL_BIN") runs a command typed through the CLI" $([ $? = 0 ] && echo 1 || echo 0)
if [ "$OS" = Darwin ]; then
    # shell_mode = auto starts a LOGIN shell on macOS (`zsh -l`; $0 does not
    # change, the `login` option does). The echoed command line must not
    # contain the needle itself, hence the substitution.
    cli "$OUTER" pane send-text "$pane" 'echo LOGIN-SHELL=$([[ -o login ]] && echo yes || echo no)' >/dev/null
    cli "$OUTER" pane send-keys "$pane" enter >/dev/null
    wait_for "$OUTER" "$pane" "LOGIN-SHELL=yes"; check "the macOS pane runs a login shell" $([ $? = 0 ] && echo 1 || echo 0)
fi

# --- 3. live cwd + shell_idle from the process table -------------------------
cli "$OUTER" pane send-text "$pane" 'cd proj' >/dev/null
cli "$OUTER" pane send-keys "$pane" enter >/dev/null
ok=0
for _ in $(seq 1 30); do
    case "$(cli "$OUTER" pane list | field cwd)" in */proj) ok=1; break ;; esac
    sleep 0.25
done
check "pane cwd follows cd (read from the shell process)" $ok
cli "$OUTER" pane send-text "$pane" 'sleep 3' >/dev/null
cli "$OUTER" pane send-keys "$pane" enter >/dev/null
sleep 1.2
busy=$(cli "$OUTER" pane list | field shell_idle)
sleep 3.5
idle=$(cli "$OUTER" pane list | field shell_idle)
[ "$busy" = false ] && [ "$idle" = true ]; check "shell_idle follows the process tree (busy=$busy idle=$idle)" $([ $? = 0 ] && echo 1 || echo 0)

# --- 4. the TUI, nested in the pane, driven through the outer PTY ------------
cli "$OUTER" pane send-text "$pane" "$EXE --session $INNER" >/dev/null
cli "$OUTER" pane send-keys "$pane" enter >/dev/null
wait_server "$INNER"; check "the TUI autostarts its own server" $([ $? = 0 ] && echo 1 || echo 0)
ok=0
for _ in $(seq 1 60); do
    screen=$(cli "$OUTER" pane read "$pane" --source visible)
    if grep -q "Workspaces" <<<"$screen" && grep -q "❯" <<<"$screen"; then ok=1; break; fi
    sleep 0.25
done
check "the TUI renders the sidebar and the composer" $ok
[ $ok = 1 ] || cli "$OUTER" pane read "$pane" --source visible | head -30
# Keys typed into the outer PTY reach the TUI's composer; Enter runs the line
# in the inner shell and its output shows inside the inner pane.
cli "$OUTER" pane send-text "$pane" 'echo NESTED-$((5*5))' >/dev/null
sleep 0.5
screen=$(cli "$OUTER" pane read "$pane" --source visible)
grep -q "❯ echo NESTED" <<<"$screen"; check "typing lands in the composer, not in the inner prompt" $([ $? = 0 ] && echo 1 || echo 0)
cli "$OUTER" pane send-keys "$pane" enter >/dev/null
ok=0
for _ in $(seq 1 40); do
    if cli "$OUTER" pane read "$pane" --source visible | grep -q "NESTED-25"; then ok=1; break; fi
    sleep 0.25
done
check "Enter runs the composer line in the inner shell" $ok
inner_pane=$(cli "$INNER" pane list | field pane_id)
cli "$INNER" pane read "$inner_pane" --source recent | grep -q "NESTED-25"; check "the inner server saw the command" $([ $? = 0 ] && echo 1 || echo 0)
# Tab completion against the inner shell's cwd (~/proj, where the TUI was
# launched): `cd pru<Tab>` -> `cd prueba/`.
mkdir -p "$HOME/proj/prueba"
cli "$OUTER" pane send-text "$pane" 'cd pru' >/dev/null
sleep 0.3
cli "$OUTER" pane send-keys "$pane" tab >/dev/null
sleep 0.6
cli "$OUTER" pane read "$pane" --source visible | grep -q "❯ cd prueba/"; check "Tab completes a path against the live cwd" $([ $? = 0 ] && echo 1 || echo 0)
cli "$OUTER" pane read "$pane" --source visible | grep "❯" | head -2
cli "$OUTER" pane send-keys "$pane" esc >/dev/null

# --- 5. clean stop ------------------------------------------------------------
cli "$INNER" server stop >/dev/null 2>&1
sleep 1
[ ! -e "$SOCK_DIR/$INNER.sock" ]; check "server stop removes the socket file" $([ $? = 0 ] && echo 1 || echo 0)

echo
echo "=== RESULTS: $pass passed, $fail failed ==="
[ $fail = 0 ]
