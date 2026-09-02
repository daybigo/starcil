"""What does ConPTY (conhost) hand to a console app for SGR mouse input when the
host terminal loses releases / sends bare presses? Runs rawprobe.py inside a
starcil pane (real ConPTY) and injects SGR bytes."""
import base64
import json
import os
import subprocess
import sys
import tempfile
import time

EXE = r"C:\dev\Starcil\target\release\starcil.exe"
OUTER = "mtprobe"
HERE = os.path.dirname(os.path.abspath(__file__))
LOG = os.path.join(tempfile.gettempdir(), "starcil-rawprobe.log")
if os.path.exists(LOG):
    os.remove(LOG)

home = tempfile.mkdtemp(prefix="starcil-probe-")
appdata = os.path.join(home, "AppData", "Roaming")
localappdata = os.path.join(home, "AppData", "Local")
os.makedirs(os.path.join(appdata, "starcil"), exist_ok=True)
os.makedirs(localappdata, exist_ok=True)
ENV = dict(os.environ)
ENV.update({"APPDATA": appdata, "LOCALAPPDATA": localappdata})
ENV.pop("STARCIL_ENV", None)
CREATE_NO_WINDOW = 0x08000000


def cli(*args, timeout=10):
    p = subprocess.run([EXE, "--session", OUTER, *args], env=ENV, capture_output=True,
                       text=True, encoding="utf-8", errors="replace", timeout=timeout,
                       creationflags=CREATE_NO_WINDOW)
    return p.returncode, p.stdout


server = subprocess.Popen([EXE, "--session", OUTER, "server"], env=ENV,
                          creationflags=CREATE_NO_WINDOW,
                          stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
for _ in range(50):
    if cli("status")[0] == 0:
        break
    time.sleep(0.2)
pane = json.loads(cli("pane", "list")[1].strip().splitlines()[-1])["result"]["panes"][0]["pane_id"]
control = subprocess.Popen(
    [EXE, "--session", OUTER, "terminal", "session", "control", pane, "--cols", "120", "--rows", "36"],
    env=ENV, stdin=subprocess.PIPE, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
    creationflags=CREATE_NO_WINDOW)


def raw(data: bytes):
    control.stdin.write((json.dumps({"input": {"data_base64": base64.b64encode(data).decode()}}) + "\n").encode())
    control.stdin.flush()


def sgr(button, x, y, pressed):
    return f"\x1b[<{button};{x};{y}{'M' if pressed else 'm'}".encode()


time.sleep(1.5)
raw(f'python "{os.path.join(HERE, "rawprobe.py")}" "{LOG}" 12\r'.encode())
time.sleep(2.5)

steps = [
    ("R press #1", sgr(2, 40, 10, True)),
    ("R release #1", sgr(2, 40, 10, False)),
    ("R press #2 (no release follows)", sgr(2, 42, 11, True)),
    ("R press #3 (stuck state)", sgr(2, 44, 12, True)),
    ("R press #4 (stuck state)", sgr(2, 46, 13, True)),
    ("R release (late)", sgr(2, 46, 13, False)),
    ("R press #5 (after late release)", sgr(2, 48, 14, True)),
    ("R release #5", sgr(2, 48, 14, False)),
    ("L press", sgr(0, 60, 20, True)),
    ("L motion 1", sgr(32, 61, 20, True)),
    ("L motion 2", sgr(32, 63, 20, True)),
    ("L release", sgr(0, 63, 20, False)),
    ("hover motion", sgr(35, 70, 22, True)),
]
for label, data in steps:
    raw(data)
    time.sleep(0.15)
time.sleep(0.5)
raw(b"q")
time.sleep(1.0)

print("=== injected steps ===")
for label, _ in steps:
    print(" ", label)
print("=== conhost -> INPUT_RECORDs ===")
print(open(LOG, encoding="utf-8").read() if os.path.exists(LOG) else "(no log)")

cli("server", "stop")
try:
    control.stdin.close()
except Exception:
    pass
time.sleep(0.5)
for p in (control, server):
    try:
        p.kill()
    except Exception:
        pass
