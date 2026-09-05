"""End-to-end check of the key encoder through a real ConPTY.

For each scenario: boot an isolated Starcil server with the given binary, run
keyprobe.py in its pane (optionally acting as a kitty-protocol client), drive
`starcil pane send-keys` and print the KEY_EVENT_RECORDs the child received.

    python sendkeys-harness.py [path/to/starcil.exe]
"""
import json
import os
import re
import subprocess
import sys
import tempfile
import time

HERE = os.path.dirname(os.path.abspath(__file__))
EXE = sys.argv[1] if len(sys.argv) > 1 else r"C:\dev\Starcil\target\release\starcil.exe"
PROBE = os.path.join(HERE, "keyprobe.py")
SESSION = "skouter"
CREATE_NO_WINDOW = 0x08000000
KEYS = os.environ.get("KEYS", "enter,shift+enter,alt+enter,ctrl+enter,ctrl+j,esc,ctrl+left,shift+tab,ctrl+shift+tab,ctrl+c").split(",")
WAIT = float(os.environ.get("WAIT", "0.5"))
SCENARIOS = os.environ.get("SCENARIOS", "legacy,kitty,vt").split(",")


def make_env():
    home = tempfile.mkdtemp(prefix="starcil-sendkeys-")
    appdata = os.path.join(home, "AppData", "Roaming")
    localappdata = os.path.join(home, "AppData", "Local")
    os.makedirs(os.path.join(appdata, "starcil"), exist_ok=True)
    os.makedirs(localappdata, exist_ok=True)
    with open(os.path.join(appdata, "starcil", "config.toml"), "w", encoding="utf-8") as f:
        f.write('onboarding = false\n\n[terminal]\ndefault_shell = "powershell.exe"\n\n[experimental]\nallow_nested = true\n')
    env = dict(os.environ)
    env.update({"APPDATA": appdata, "LOCALAPPDATA": localappdata})
    env.pop("STARCIL_ENV", None)
    env.pop("STARCIL_SESSION", None)
    return env


def scenario(name, probe_flags):
    env = make_env()

    def cli(*a, timeout=15):
        p = subprocess.run([EXE, "--session", SESSION, *a], env=env, capture_output=True, text=True,
                           encoding="utf-8", errors="replace", timeout=timeout, creationflags=CREATE_NO_WINDOW)
        if p.returncode != 0:
            raise SystemExit(f"cli failed: {a}\n{p.stdout}\n{p.stderr}")
        return p.stdout

    def cli_json(*a):
        env_ = json.loads(cli(*a).strip().splitlines()[-1])
        return env_.get("result", env_)

    def read_pane(pane):
        return cli("pane", "read", pane, "--source", "recent", "--lines", "400")

    def key_records(text):
        out = {}
        for line in text.splitlines():
            m = re.match(r"\s*K(\d+) (.*)", line)
            if m:
                out[int(m.group(1))] = m.group(2).strip()
        return out

    server = subprocess.Popen([EXE, "--session", SESSION, "server"], env=env, creationflags=CREATE_NO_WINDOW,
                              stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    try:
        for _ in range(50):
            p = subprocess.run([EXE, "--session", SESSION, "status"], env=env, capture_output=True, text=True,
                               timeout=5, creationflags=CREATE_NO_WINDOW)
            if p.returncode == 0:
                break
            time.sleep(0.2)
        else:
            raise SystemExit("server did not come up")
        pane = cli_json("pane", "list")["panes"][0]["pane_id"]
        deadline = time.time() + 20
        while ">" not in read_pane(pane):
            if time.time() > deadline:
                raise SystemExit("no prompt")
            time.sleep(0.25)
        time.sleep(0.5)
        cli("pane", "send-text", pane, f'python "{PROBE}" {probe_flags}'.strip())
        cli("pane", "send-keys", pane, "enter")
        deadline = time.time() + 20
        while "READY" not in read_pane(pane):
            if time.time() > deadline:
                print(read_pane(pane))
                raise SystemExit("probe did not start")
            time.sleep(0.25)
        time.sleep(0.5)
        print(f"===== {name} (probe flags: {probe_flags or 'none'})")
        seen = key_records(read_pane(pane))
        if seen:
            print("  at startup:")
            for index in sorted(seen):
                print("     ", seen[index])
        last = max(seen) if seen else 0
        for key in KEYS:
            cli("pane", "send-keys", pane, key)
            time.sleep(WAIT)
            now = key_records(read_pane(pane))
            new = [now[i] for i in sorted(now) if i > last]
            last = max(now) if now else last
            print(f"  -- send-keys {key}")
            for line in new:
                print("     ", line)
            if not new:
                print("      (nothing)")
        cli("pane", "send-keys", pane, "q")
        time.sleep(0.3)
    finally:
        try:
            subprocess.run([EXE, "--session", SESSION, "server", "stop"], env=env, capture_output=True, timeout=10,
                           creationflags=CREATE_NO_WINDOW)
        except Exception:
            pass
        try:
            server.wait(timeout=5)
        except Exception:
            server.kill()


if "legacy" in SCENARIOS:
    scenario("legacy child (console reader, no kitty)", "")
if "kitty" in SCENARIOS:
    scenario("kitty client (pushes flags 1, then queries)", "--kitty --query")
if "vt" in SCENARIOS:
    scenario("VT-input child that queries without pushing", "--vt --query")
