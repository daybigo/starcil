"""Composer E2E (no human): the in-pane input, launcher buttons, and folder pick.

Nested-starcil harness: the inner TUI runs inside a pane of an outer starcil
(temp HOME, sessions `cbouter`/`cbinner`) and is driven with SGR mouse bytes and
keystrokes through `terminal session control`. The native folder dialog cannot
open headless, so the inner TUI runs with STARCIL_FOLDER_PICKER=fake:<path> —
the documented automation seam; the real dialog is the same code path minus the
override. The dock is configured to ["cmd"] so clicking it opens a harmless
shell pane on any Windows machine.

Usage (after `build.ps1 build --release -p starcil`):
    python tests/e2e/command-bar.py [path/to/starcil.exe]
Prints PASS/FAIL per scenario; exits 1 on any FAIL.
"""
import base64
import json
import os
import subprocess
import sys
import tempfile
import time

EXE = sys.argv[1] if len(sys.argv) > 1 else r"C:\dev\Starcil\target\release\starcil.exe"
OUTER, INNER = "cbouter", "cbinner"
COLS, ROWS = 120, 36
SIDEBAR = 26
CNW = 0x08000000

home = tempfile.mkdtemp(prefix="starcil-cmdbar-")
picked_dir = os.path.join(home, "picked folder")
os.makedirs(picked_dir, exist_ok=True)
# Fixtures for Tab completion inside the picked folder (scenario H).
os.makedirs(os.path.join(picked_dir, "alpha_dir"), exist_ok=True)
with open(os.path.join(picked_dir, "alphabet.txt"), "w", encoding="utf-8") as f:
    f.write("x")
appdata = os.path.join(home, "AppData", "Roaming")
localappdata = os.path.join(home, "AppData", "Local")
os.makedirs(os.path.join(appdata, "starcil"), exist_ok=True)
os.makedirs(localappdata, exist_ok=True)
with open(os.path.join(appdata, "starcil", "config.toml"), "w", encoding="utf-8") as f:
    f.write(
        "onboarding = false\n\n[terminal]\n# Pinned: the scripts type PowerShell syntax (& \"exe\" args, cls), whatever pwsh/powershell the host has.\ndefault_shell = \"powershell.exe\"\n\n[experimental]\nallow_nested = true\n\n"
        "[ui.dock]\nagents = [\"cmd\"]\n"
    )

ENV = dict(os.environ)
ENV.update({
    "APPDATA": appdata,
    "LOCALAPPDATA": localappdata,
    "STARCIL_FOLDER_PICKER": f"fake:{picked_dir}",
})
ENV.pop("STARCIL_ENV", None)
ENV.pop("STARCIL_SESSION", None)


def cli(session, *args, check=True):
    p = subprocess.run([EXE, "--session", session, *args], env=ENV, capture_output=True,
                       text=True, encoding="utf-8", errors="replace", timeout=10,
                       creationflags=CNW)
    if check and p.returncode != 0:
        raise SystemExit(f"cli failed {args}: {p.stdout} {p.stderr}")
    return p.stdout


def cli_json(session, *args):
    envelope = json.loads(cli(session, *args).strip().splitlines()[-1])
    return envelope.get("result", envelope)


def wait_server(session):
    for _ in range(50):
        if subprocess.run([EXE, "--session", session, "status"], env=ENV,
                          capture_output=True, creationflags=CNW).returncode == 0:
            return
        time.sleep(0.2)
    raise SystemExit("server " + session)


server = subprocess.Popen([EXE, "--session", OUTER, "server"], env=ENV, creationflags=CNW,
                          stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
wait_server(OUTER)
outer_pane = cli_json(OUTER, "pane", "list")["panes"][0]["pane_id"]
control = subprocess.Popen(
    [EXE, "--session", OUTER, "terminal", "session", "control", outer_pane,
     "--cols", str(COLS), "--rows", str(ROWS)],
    env=ENV, stdin=subprocess.PIPE, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
    creationflags=CNW)


def raw(data: bytes):
    control.stdin.write((json.dumps({"input": {"data_base64": base64.b64encode(data).decode()}}) + "\n").encode())
    control.stdin.flush()


def sgr(button, x, y, pressed=True):
    return f"\x1b[<{button};{x + 1};{y + 1}{'M' if pressed else 'm'}".encode()


def click(x, y):
    raw(sgr(0, x, y, True))
    time.sleep(0.1)
    raw(sgr(0, x, y, False))
    time.sleep(0.5)


def screen():
    return cli(OUTER, "pane", "read", outer_pane, "--source", "visible")


def wait_for(needle, timeout=20):
    deadline = time.time() + timeout
    while time.time() < deadline:
        s = screen()
        if needle in s:
            return s
        time.sleep(0.25)
    print(screen())
    raise SystemExit("timeout " + needle)


time.sleep(1.0)
wait_for(">")
raw(f'& "{EXE}" --session {INNER}\r'.encode())
wait_for("spaces")
time.sleep(1.5)
wait_server(INNER)

results = {}


def row_with(needle, rows=None):
    for index, row in enumerate(rows or screen().splitlines()):
        if needle in row:
            return index
    return -1

# --- scenario A: the composer renders INSIDE the shell pane -------------------
s = screen()
rows = s.splitlines()
dock_row = row_with(" 1 cmd")  # dock rows carry no icon except claude/codex
folder_row = row_with("open folder")
input_row = row_with("❯")
results["composer_renders_inside_the_pane"] = (
    dock_row > 0
    and folder_row > dock_row
    and input_row == folder_row + 3  # panel bottom border, then the line
    and input_row < ROWS - 1
    # double line around the input, cwd status row under it
    and "─" in rows[input_row - 1]
    and "─" in rows[input_row + 1]
    # one-row header: the text is level with the tab labels
    and "Workspaces" in rows[0]
)
print(rows[0][:SIDEBAR])
print(rows[dock_row])
print(rows[folder_row])
print(rows[input_row])

# --- scenario B: click the input, type, Enter sends to the focused pane ------
click(SIDEBAR + 10, input_row)
raw(b"echo BARTEST-42")
time.sleep(0.8)
s = screen()
rows = s.splitlines()
typed_in_composer = "echo BARTEST-42" in rows[input_row]
pane_area = "\n".join(rows[:dock_row])
results["typing_lands_in_composer_not_pane"] = typed_in_composer and "BARTEST-42" not in pane_area
print(rows[input_row])
raw(b"\r")
wait_for("BARTEST-42")
time.sleep(0.8)
s = screen()
rows = s.splitlines()
results["enter_runs_it_in_the_focused_pane"] = "BARTEST-42" in "\n".join(rows[:dock_row])
results["composer_clears_after_send"] = "echo BARTEST-42" not in rows[input_row]

# Esc hands the keyboard back to the pane.
raw(b"\x1b")
time.sleep(0.3)

# --- scenario C: dock click runs the agent in the CURRENT pane ---------------
panes_before = len(cli_json(INNER, "pane", "list")["panes"])
# The dock item is `cmd`: its banner ("Microsoft Windows [...]") must appear
# ONE more time in the same pane after the click, whatever shell the pane runs
# (cmd prints it at startup too; PowerShell does not).
banner = "Microsoft Windows"
banners_before = cli(INNER, "pane", "read", "w1:p1", "--source", "recent").count(banner)
click(SIDEBAR + 3, dock_row)
time.sleep(2.0)
panes_after = len(cli_json(INNER, "pane", "list")["panes"])
inner_text = cli(INNER, "pane", "read", "w1:p1", "--source", "recent")
print("panes before/after dock click:", panes_before, panes_after)
print("cmd banners before/after dock click:", banners_before, inner_text.count(banner))
results["dock_click_runs_in_current_pane"] = (
    panes_after == panes_before and inner_text.count(banner) > banners_before
)

# --- scenario D: the folder button does a visual cd in the focused pane ------
folder_row = row_with("open folder")
click(SIDEBAR + 5, folder_row)
wait_for("pushd")
time.sleep(1.5)
s = screen()
rows = s.splitlines()
# the cwd label is the status row under the bottom line, left-aligned
input_row = row_with("❯", rows)
status_row = rows[input_row + 2] if 0 <= input_row < len(rows) - 2 else ""
results["folder_pick_pushd_in_active_pane"] = ("pushd" in s) and ("picked folder" in s)
# left-aligned: the label starts within the first cells of the pane column
pane_part = status_row[SIDEBAR:]
leading = len(pane_part) - len(pane_part.lstrip())
results["cwd_label_shows_choice_on_the_left"] = ("picked folder" in status_row) and leading <= 3
print(status_row[:120] if status_row else "(no cwd label)")

# --- scenario E: alt+digit runs dock item 1 in the current pane --------------
# ConPTY translates ESC+digit into Alt+digit console records.
before_text = cli(INNER, "pane", "read", "w1:p1", "--source", "recent")
raw(b"\x1b1")
time.sleep(2.0)
after_text = cli(INNER, "pane", "read", "w1:p1", "--source", "recent")
panes_after = len(cli_json(INNER, "pane", "list")["panes"])
results["alt_digit_runs_in_current_pane"] = (
    panes_after == panes_before
    and after_text.count("Microsoft Windows") > before_text.count("Microsoft Windows")
)

# --- scenario F: once the pane's CLI is recognized, the composer hides -------
# The server detects agents by OSC title; a nested `cmd /k "title ..."` sets it
# through ConPTY AND keeps a process running under the shell, so it stands in
# for Claude Code announcing itself. The snapshot with
# `pane.agent` must reach the TUI on its own (no click needed).
# First a structural change (tab rename) so the TUI stream's snapshot revision
# is in sync: only then does a detection WITHOUT a model bump go unnoticed —
# the state Cesar's session is always in after a few clicks.
cli(INNER, "tab", "rename", "w1:t1", "shell")
time.sleep(0.8)
input_row = row_with("❯")
click(SIDEBAR + 10, input_row)
raw(b'cmd /k "title Claude Code"')
raw(b"\r")
deadline = time.time() + 10
hidden = False
while time.time() < deadline:
    s = screen()
    if "open folder" not in s and "❯" not in s and "claude" in s:
        hidden = True
        break
    time.sleep(0.25)
results["composer_hides_by_itself_when_the_agent_is_detected"] = hidden
if not hidden:
    print(s)

# --- scenario G: the CLI exits -> the pane is a shell again -> composer back --
# Leave the three nested cmds (dock click, alt+1, the titled one) so the shell
# sits idle: the server sees nothing running under it and ends the agent.
for _ in range(3):
    raw(b"exit\r")
    time.sleep(0.6)
deadline = time.time() + 10
back = False
while time.time() < deadline:
    s = screen()
    if "open folder" in s and "❯" in s and "claude" not in s:
        back = True
        break
    time.sleep(0.25)
results["composer_returns_when_the_agent_exits"] = back
if not back:
    print(s)

# --- scenario H: the cwd label follows a cd typed by the user ---------------
# The pane's PowerShell never left the repo: scenario D's pushd ran inside the
# nested cmd from the dock click. `cd ..` moves the shell itself; its prompt
# hook announces the new location and the label under the input follows —
# and the picked-folder preview from D goes away with it.
initial_cwd = cli_json(INNER, "pane", "list")["panes"][0]["cwd"]
parent_name = os.path.basename(os.path.dirname(initial_cwd))
raw(b"cd ..")
raw(b"\r")
deadline = time.time() + 10
followed = False
status_row = ""
while time.time() < deadline:
    rows = screen().splitlines()
    input_row = row_with("❯", rows)
    status_row = rows[input_row + 2] if 0 <= input_row < len(rows) - 2 else ""
    if "picked folder" not in status_row and parent_name in status_row:
        followed = True
        break
    time.sleep(0.3)
live_cwd = cli_json(INNER, "pane", "list")["panes"][0]["cwd"]
results["cwd_label_follows_cd"] = followed and live_cwd == os.path.dirname(initial_cwd)
print(status_row[:120] if status_row else "(no status row)", "| pane cwd:", live_cwd)
if not results["cwd_label_follows_cd"]:
    print(screen())

# --- scenario I: Tab completes paths against the pane's LIVE cwd -------------
# Move the shell into the picked folder for real, wait for the label to say
# so, then complete: `cd` offers directories only, so `al` resolves to
# alpha_dir\ (not alphabet.txt).
raw(b'cd "' + picked_dir.encode() + b'"')
raw(b"\r")
deadline = time.time() + 10
while time.time() < deadline:
    if cli_json(INNER, "pane", "list")["panes"][0]["cwd"] == picked_dir:
        break
    time.sleep(0.3)
time.sleep(0.5)
raw(b"cd al")
time.sleep(0.4)
raw(b"\t")
time.sleep(0.8)
rows = screen().splitlines()
input_row = row_with("❯", rows)
completed = input_row >= 0 and "cd alpha_dir\\" in rows[input_row]
results["tab_completes_against_the_live_cwd"] = completed
print(rows[input_row] if input_row >= 0 else "(no input row)")
raw(b"\x1b")
time.sleep(0.3)

# --- scenario J: up arrow recalls the last command --------------------------
raw(b"echo HIST-7")
raw(b"\r")
wait_for("HIST-7")
time.sleep(0.6)
raw(b"\x1b[A")
time.sleep(0.6)
rows = screen().splitlines()
input_row = row_with("❯", rows)
results["up_arrow_recalls_history"] = input_row >= 0 and "echo HIST-7" in rows[input_row]
print(rows[input_row] if input_row >= 0 else "(no input row)")
raw(b"\x1b")
time.sleep(0.3)

# --- scenario K: a click on the pane content keeps the typing below ---------
click(SIDEBAR + 10, 3)
raw(b"zzz-below")
time.sleep(0.8)
rows = screen().splitlines()
input_row = row_with("❯", rows)
above = "\n".join(rows[:input_row]) if input_row > 0 else ""
results["content_click_keeps_typing_in_the_composer"] = (
    input_row >= 0 and "zzz-below" in rows[input_row] and "zzz-below" not in above
)
print(rows[input_row] if input_row >= 0 else "(no input row)")
raw(b"\x1b")
time.sleep(0.3)

print("\n=== RESULTS ===")
failed = 0
for name, passed in results.items():
    print(f"{name}: {'PASS' if passed else 'FAIL'}")
    failed += 0 if passed else 1

cli(INNER, "server", "stop", check=False)
cli(OUTER, "server", "stop", check=False)
try:
    control.stdin.close()
except Exception:
    pass
time.sleep(0.5)
for process in (control, server):
    try:
        process.kill()
    except Exception:
        pass
sys.exit(1 if failed else 0)
