"""Real agent newline matrix: terminal family -> ConPTY -> TUI -> server -> agent.

    python tests/e2e/agent-newline-e2e.py [starcil.exe] [claude codex]

FAMILIES=win32,vt,kitty selects outer emulations. Probe each chord first with the
same binary/input mode, then test the real agent composer. Never submit a draft.
A VT-only Shift+Enter is a bare CR: verify that on an EMPTY composer and report
SKIP (no distinguishable Shift bit), never send it after text. All other FAILs
make the script exit 1. Evidence remains in the printed temp directory.
"""
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile
import time
import uuid

from terminal_family import FAMILIES, Terminal, win32_key

exe_args = [a for a in sys.argv[1:] if a.lower().endswith(".exe")]
AGENTS = [a for a in sys.argv[1:] if not a.lower().endswith(".exe")] or ["claude", "codex"]
EXE = exe_args[0] if exe_args else r"C:\dev\Starcil\target\release\starcil.exe"
PROJECT = str(Path(__file__).resolve().parents[2])
CNW = 0x08000000
SELECTED = os.environ.get("FAMILIES", "win32,vt,kitty").split(",")
ROOT = Path(tempfile.mkdtemp(prefix="starcil-c24b-"))
RESULTS = []


def make_env(name):
    home = ROOT / name
    appdata, local = home / "Roaming", home / "Local"
    (appdata / "starcil").mkdir(parents=True)
    local.mkdir()
    (appdata / "starcil" / "config.toml").write_text(
        'onboarding = false\n\n[terminal]\ndefault_shell = "powershell.exe"\n\n'
        '[update]\nversion_check = false\n\n[experimental]\nallow_nested = true\n', encoding="utf-8")
    env = dict(os.environ)
    env.update(APPDATA=str(appdata), LOCALAPPDATA=str(local), STARCIL_KEY_TRACE=str(home / "keys.log"))
    for key in ["STARCIL_ENV", "STARCIL_SESSION", "STARCIL_SOCKET_PATH", "STARCIL_CLIENT_DEBUG"]:
        env.pop(key, None)
    return env, home


def cli(env, session, *args, check=True):
    proc = subprocess.run([EXE, "--session", session, *args], env=env, capture_output=True,
                          text=True, encoding="utf-8", errors="replace", timeout=15, creationflags=CNW)
    if check and proc.returncode:
        raise RuntimeError(f"CLI {args}: {proc.stderr or proc.stdout}")
    return proc


def panes(env, session):
    envelope = json.loads(cli(env, session, "pane", "list").stdout.strip().splitlines()[-1])
    return envelope.get("result", envelope)["panes"]


def wait_until(action, predicate, label, timeout=40):
    deadline = time.monotonic() + timeout
    last = None
    while time.monotonic() < deadline:
        last = action()
        if predicate(last): return last
        time.sleep(0.2)
    raise TimeoutError(f"{label}: {str(last)[-1500:]}")


def trace(path, offset=0):
    if not path.exists(): return ""
    with path.open("rb") as file:
        file.seek(offset)
        return file.read().decode("utf-8", "replace")


def verdict(label, status, details):
    RESULTS.append((label, status, details))
    print(f"{status} {label}: {details}", flush=True)


def probe(family):
    env, home = make_env(f"probe-{family}")
    path = home / "probe.txt"
    terminal = Terminal([EXE, "__probe-keys", str(path), "--seconds", "5"], env, PROJECT, family)
    try:
        terminal.wait_text("READY kitty=")
        observed = {}
        cases = dict(FAMILIES[family])
        cases["esc"] = b"\x1b[27u" if family == "kitty" else win32_key(27, 1, 27, 0)
        for chord, data in cases.items():
            keylog = home / "keys.log"
            start = keylog.stat().st_size
            terminal.raw(data)
            time.sleep(0.22)
            output = trace(keylog, start)
            matches = re.findall(r"pane_chord=([^\s\x1b]+)", output)
            observed[chord] = next((name for name in matches if name != "-"), None)
            raw_lines = re.findall(r"RAW KEY [^\r\n\x1b]+", output)
            expected = "enter" if family == "vt" and chord == "shift+enter" else "ctrl+j" if chord == "ctrl+enter" else chord
            verdict(f"probe/{family}/{chord}", "PASS" if observed[chord] == expected else "FAIL",
                    f"bytes={data.hex()} chord={observed[chord]} raw={raw_lines[:2]}")
        if family != "kitty":
            start = keylog.stat().st_size
            terminal.raw(b"\x1b[13;2u")
            time.sleep(0.22)
            dropped = not trace(keylog, start).strip()
            verdict(f"probe/{family}/unnegotiated-kitty", "PASS" if dropped else "FAIL",
                    f"raw CSI 13;2u with VT input off: no input records={dropped}")
        if not terminal.wait(8): raise TimeoutError("probe did not finish")
        saved = path.read_text(encoding="utf-8")
        original = re.search(r"original=(0x[0-9a-f]+)", saved).group(1)
        restored = re.search(r"RESTORED input=(0x[0-9a-f]+)", saved).group(1)
        expected_mode = f"READY kitty={'true' if family == 'kitty' else 'false'}"
        if family == "kitty":
            wait_until(lambda: terminal.replies, lambda replies: ">1u" in replies and "<1u" in replies,
                       "kitty push/pop reached outer terminal", 2)
        good = original == restored and expected_mode in saved
        verdict(f"probe/{family}/mode-restoration", "PASS" if good else "FAIL",
                f"{saved.splitlines()[0]} restored={restored} handshake={terminal.replies} output={path}")
        return observed
    finally:
        (home / "outer.vt").write_bytes(bytes(terminal.output))
        terminal.close()


def agent_scenario(family, agent, observed):
    env, home = make_env(f"{family}-{agent}")
    session = f"c24b-{uuid.uuid4().hex[:12]}"
    log = home / "keys.log"
    terminal = None
    server = subprocess.Popen([EXE, "--session", session, "server"], env=env,
                              stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, creationflags=CNW)
    try:
        wait_until(lambda: cli(env, session, "status", check=False).returncode, lambda code: code == 0, "server startup")
        terminal = Terminal([EXE, "--session", session], env, PROJECT, family)
        wait_until(lambda: trace(log), lambda text: "MODE kitty=" in text, "TUI negotiation")
        info = wait_until(lambda: panes(env, session)[0],
                          lambda p: p.get("shell_idle") is True and not (p.get("agent") or p.get("agent_name")), "shell prompt")
        pane = info["pane_id"]
        time.sleep(0.5)
        # Shell launch only after verifying this pane is an idle shell.
        terminal.raw(f'cd "{PROJECT}"'.encode())
        time.sleep(0.2)
        terminal.raw(b"\r")
        time.sleep(0.8)
        terminal.raw(agent.encode())
        time.sleep(0.2)
        terminal.raw(b"\r")
        info = wait_until(lambda: panes(env, session)[0],
                          lambda p: agent in str(p.get("agent_name", "")).lower() or agent in str(p.get("agent", "")).lower(),
                          f"{agent} detected", 60)
        read = lambda: cli(env, session, "pane", "read", pane, "--source", "visible").stdout
        markers = ["? for shortcuts", "❯", "auto mode on", "Try \""] if agent == "claude" else ["OpenAI Codex", "›", "Ask Codex"]
        wait_until(read, lambda screen: any(marker in screen for marker in markers), "agent composer", 60)
        time.sleep(3)
        print(f"AGENT family={family} agent={agent} session={session} detected={info.get('agent_name')} mode={trace(log).split('MODE ')[-1].splitlines()[0]}", flush=True)
        for index, (chord, data) in enumerate(FAMILIES[family].items()):
            label = f"agent/{family}/{agent}/{chord}"
            expected = "ctrl+j" if chord == "ctrl+enter" else chord
            unsupported = family == "vt" and chord == "shift+enter"
            if observed.get(chord) != ("enter" if unsupported else expected):
                verdict(label, "FAIL", "probe did not produce a safe newline chord; no draft typed")
                continue
            # Gate with an empty composer: unexpected Enter cannot submit text.
            offset = log.stat().st_size
            terminal.raw(data)
            forwarded = wait_until(lambda: trace(log, offset), lambda text: "FORWARD " in text, "forwarded chord", 5)
            actual = re.findall(r'FORWARD .* keys=\["([^"]+)"\]', forwarded)
            if actual != (["enter"] if unsupported else [expected]):
                verdict(label, "FAIL", f"forwarded={actual}; no draft typed")
                continue
            if unsupported:
                verdict(label, "SKIP", "VT-only Shift+Enter is bare CR -> enter; tested empty composer, no message. Use Alt+Enter or Ctrl+J.")
                continue
            left, right = f"c24b_{index}_first", f"c24b_{index}_second"
            terminal.raw(left.encode())
            time.sleep(0.25)
            terminal.raw(data)
            time.sleep(0.25)
            terminal.raw(right.encode())
            screen = wait_until(read, lambda text: left in text and right in text, "draft visible", 8)
            lines = screen.splitlines()
            first = next(i for i, line in enumerate(lines) if left in line)
            second = next(i for i, line in enumerate(lines) if right in line)
            (home / f"{index}-composer.txt").write_text(screen, encoding="utf-8")
            evidence = [line for line in trace(log, offset).splitlines() if "EVENT Key" in line or "FORWARD " in line]
            (home / f"{index}-keys.txt").write_text("\n".join(evidence), encoding="utf-8")
            verdict(label, "PASS" if first != second else "FAIL",
                    f"forwarded={expected} rows={first},{second} composer={[lines[first].strip(), lines[second].strip()]}")
            terminal.raw(b"\x03")
            wait_until(read, lambda text: left not in text and right not in text, "draft cleared", 8)
            time.sleep(0.7)
    finally:
        # Stop only this unique session and exact owned process handles.
        try:
            cli(env, session, "server", "stop", check=False)
        finally:
            if terminal:
                (home / "outer.vt").write_bytes(bytes(terminal.output))
                terminal.close()
            try: server.wait(timeout=8)
            except subprocess.TimeoutExpired:
                server.kill()
                server.wait(timeout=5)


print(f"EVIDENCE {ROOT}", flush=True)
for family in SELECTED:
    if family not in FAMILIES: raise SystemExit(f"unknown family {family}")
    try:
        observed = probe(family)
    except Exception as error:
        verdict(f"probe/{family}", "FAIL", str(error))
        continue
    for agent in AGENTS:
        if agent not in ("claude", "codex"): raise SystemExit(f"unsupported agent {agent}")
        try:
            agent_scenario(family, agent, observed)
        except Exception as error:
            verdict(f"agent/{family}/{agent}", "FAIL", str(error))

counts = {status: sum(row[1] == status for row in RESULTS) for status in ("PASS", "FAIL", "SKIP")}
summary = "AGENT_NEWLINE " + " ".join(f"{key}={value}" for key, value in counts.items())
(ROOT / "results.json").write_text(json.dumps({"results": RESULTS, "counts": counts}, ensure_ascii=False, indent=2), encoding="utf-8")
print(summary, flush=True)
raise SystemExit(1 if counts["FAIL"] else 0)
