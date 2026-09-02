"""Real mouse passthrough e2e for a tracking app inside a nested Starcil TUI.

Usage (after `build.ps1 build --release -p starcil`):
    python tests/e2e/mouse-passthrough.py [path/to/starcil.exe]
"""

import base64
import json
import os
import subprocess
import sys
import tempfile
import time
from pathlib import Path


EXE = sys.argv[1] if len(sys.argv) > 1 else r"C:\dev\Starcil\target\release\starcil.exe"
OUTER = "mtpassouter"
INNER = "mtpassinner"
COLS, ROWS = 120, 36
MOUSETRAP = Path(__file__).with_name("mousetrap.py")
CREATE_NO_WINDOW = 0x08000000

home = tempfile.mkdtemp(prefix="starcil-mouse-passthrough-")
appdata = os.path.join(home, "AppData", "Roaming")
localappdata = os.path.join(home, "AppData", "Local")
os.makedirs(os.path.join(appdata, "starcil"), exist_ok=True)
os.makedirs(localappdata, exist_ok=True)
with open(os.path.join(appdata, "starcil", "config.toml"), "w", encoding="utf-8") as config:
    config.write("onboarding = false\n\n[terminal]\n# Pinned: the scripts type PowerShell syntax (& \"exe\" args, cls), whatever pwsh/powershell the host has.\ndefault_shell = \"powershell.exe\"\n\n[experimental]\nallow_nested = true\n")

ENV = dict(os.environ)
ENV.update({"APPDATA": appdata, "LOCALAPPDATA": localappdata})
ENV.pop("STARCIL_ENV", None)
ENV.pop("STARCIL_SESSION", None)


def cli(session, *args, timeout=15, check=True):
    process = subprocess.run(
        [EXE, "--session", session, *args],
        env=ENV,
        capture_output=True,
        text=True,
        encoding="utf-8",
        errors="replace",
        timeout=timeout,
        creationflags=CREATE_NO_WINDOW,
    )
    if check and process.returncode != 0:
        raise RuntimeError(f"cli failed {args}: {process.stdout}\n{process.stderr}")
    return process.stdout


def cli_json(session, *args):
    output = cli(session, *args)
    envelope = json.loads(output.strip().splitlines()[-1])
    return envelope.get("result", envelope)


def wait_server(session, tries=50):
    for _ in range(tries):
        process = subprocess.run(
            [EXE, "--session", session, "status"],
            env=ENV,
            capture_output=True,
            text=True,
            timeout=5,
            creationflags=CREATE_NO_WINDOW,
        )
        if process.returncode == 0:
            return
        time.sleep(0.2)
    raise RuntimeError(f"server {session} did not start")


def read_pane(session, pane_id):
    return cli(session, "pane", "read", pane_id, "--source", "visible")


def wait_for(session, pane_id, needle, timeout=15):
    deadline = time.time() + timeout
    last = ""
    while time.time() < deadline:
        last = read_pane(session, pane_id)
        if needle in last:
            return last
        time.sleep(0.2)
    raise RuntimeError(f"timeout waiting for {needle!r}; last screen:\n{last}")


def scroll_offset(pane_id):
    panes = cli_json(INNER, "pane", "list")["panes"]
    pane = next(pane for pane in panes if pane["pane_id"] == pane_id)
    return pane.get("scroll", {}).get("offset_from_bottom", 0)


results = {
    "wheel_reaches_tracking_app": False,
    "wheel_scrolls_scrollback_after_mode_off": False,
}
server = None
control = None

try:
    server = subprocess.Popen(
        [EXE, "--session", OUTER, "server"],
        env=ENV,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
        creationflags=CREATE_NO_WINDOW,
    )
    wait_server(OUTER)
    outer_pane = cli_json(OUTER, "pane", "list")["panes"][0]["pane_id"]
    control = subprocess.Popen(
        [
            EXE,
            "--session",
            OUTER,
            "terminal",
            "session",
            "control",
            outer_pane,
            "--cols",
            str(COLS),
            "--rows",
            str(ROWS),
        ],
        env=ENV,
        stdin=subprocess.PIPE,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
        creationflags=CREATE_NO_WINDOW,
    )

    def raw(data):
        frame = json.dumps(
            {"input": {"data_base64": base64.b64encode(data).decode("ascii")}}
        ) + "\n"
        control.stdin.write(frame.encode("utf-8"))
        control.stdin.flush()

    def sgr(button, x, y):
        return f"\x1b[<{button};{x + 1};{y + 1}M".encode("ascii")

    wait_for(OUTER, outer_pane, ">", 20)
    raw(f'& "{EXE}" --session {INNER}\r'.encode("utf-8"))
    wait_for(OUTER, outer_pane, "spaces", 20)
    wait_server(INNER)
    inner_pane = cli_json(INNER, "pane", "list")["panes"][0]["pane_id"]

    raw(f'& python -u "{MOUSETRAP}"\r'.encode("utf-8"))
    wait_for(OUTER, outer_pane, "MOUSETRAP_READY", 20)

    x, y = 60, 15
    raw(sgr(64, x, y))
    wait_for(OUTER, outer_pane, "[<64;", 20)
    results["wheel_reaches_tracking_app"] = True

    raw(b"q")
    wait_for(OUTER, outer_pane, "MOUSETRAP_DONE", 20)
    raw(sgr(64, x, y))
    deadline = time.time() + 10
    while time.time() < deadline:
        if scroll_offset(inner_pane) > 0:
            results["wheel_scrolls_scrollback_after_mode_off"] = True
            break
        time.sleep(0.2)
except Exception as error:
    print(f"harness_error: {error}")
finally:
    for session in (INNER, OUTER):
        try:
            cli(session, "server", "stop", check=False)
        except Exception:
            pass
    if control is not None and control.stdin is not None:
        try:
            control.stdin.close()
        except Exception:
            pass
    for process in (control, server):
        if process is not None:
            try:
                process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                print("owned_process_did_not_exit: FAIL")

failed = 0
for name, passed in results.items():
    print(f"{name}: {'PASS' if passed else 'FAIL'}")
    failed += 0 if passed else 1

sys.exit(1 if failed else 0)
