"""Real mouse e2e for the starcil TUI (no human, no outer multiplexer).

Runs an OUTER starcil server (temp HOME, own sessions `mtouter`/`mtinner`), puts
the INNER starcil TUI inside one of its panes (a real ConPTY), and injects SGR
mouse sequences (exactly what a host terminal emits) into that pane through the
`terminal session control` stream. ConPTY translates them into MOUSE_EVENT
records, so this covers conhost + the Windows input reader + starcil routing +
render end to end. Scenario 4 mimics Warp, which forwards the right-button
PRESS but never its RELEASE (2026-08-20 root cause of "anticlick works once"
and "cannot resize panes").

Usage (after `build.ps1 build --release -p starcil`):
    python tests/e2e/mouse-harness.py [path/to/starcil.exe]
Prints PASS/FAIL per scenario; exits 1 on any FAIL.
"""
import base64
import json
import os
import re
import subprocess
import sys
import tempfile
import time

EXE = sys.argv[1] if len(sys.argv) > 1 else r"C:\dev\Starcil\target\release\starcil.exe"
OUTER = "mtouter"
INNER = "mtinner"
COLS, ROWS = 120, 36

home = tempfile.mkdtemp(prefix="starcil-mouse-")
appdata = os.path.join(home, "AppData", "Roaming")
localappdata = os.path.join(home, "AppData", "Local")
os.makedirs(os.path.join(appdata, "starcil"), exist_ok=True)
os.makedirs(localappdata, exist_ok=True)
with open(os.path.join(appdata, "starcil", "config.toml"), "w", encoding="utf-8") as f:
    f.write("onboarding = false\n\n[terminal]\n# Pinned: the scripts type PowerShell syntax (& \"exe\" args, cls), whatever pwsh/powershell the host has.\ndefault_shell = \"powershell.exe\"\n\n[experimental]\nallow_nested = true\n")

ENV = dict(os.environ)
ENV.update({"APPDATA": appdata, "LOCALAPPDATA": localappdata, "STARCIL_MOUSE_DEBUG": "1"})
ENV.pop("STARCIL_ENV", None)
ENV.pop("STARCIL_SESSION", None)

CREATE_NO_WINDOW = 0x08000000


def cli(session, *args, timeout=10, check=True):
    p = subprocess.run([EXE, "--session", session, *args], env=ENV, capture_output=True,
                       text=True, encoding="utf-8", errors="replace", timeout=timeout,
                       creationflags=CREATE_NO_WINDOW)
    if check and p.returncode != 0:
        raise SystemExit(f"cli failed: {args}\n{p.stdout}\n{p.stderr}")
    return p.stdout


def cli_json(session, *args):
    out = cli(session, *args)
    env = json.loads(out.strip().splitlines()[-1])
    return env.get("result", env)


def wait_server(session, tries=50):
    for _ in range(tries):
        p = subprocess.run([EXE, "--session", session, "status"], env=ENV, capture_output=True,
                           text=True, timeout=5, creationflags=CREATE_NO_WINDOW)
        if p.returncode == 0:
            return
        time.sleep(0.2)
    raise SystemExit(f"server {session} did not come up")


def read_pane(session, pane_id):
    return cli(session, "pane", "read", pane_id, "--source", "visible")


def wait_for(session, pane_id, needle, timeout=15):
    deadline = time.time() + timeout
    last = ""
    while time.time() < deadline:
        last = read_pane(session, pane_id)
        if needle in last:
            return last
        time.sleep(0.25)
    print(last)
    raise SystemExit(f"timeout waiting for {needle!r}")


# --- boot outer --------------------------------------------------------------
server = subprocess.Popen([EXE, "--session", OUTER, "server"], env=ENV,
                          creationflags=CREATE_NO_WINDOW,
                          stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
wait_server(OUTER)
panes = cli_json(OUTER, "pane", "list")
outer_pane = panes["panes"][0]["pane_id"]
print("outer pane", outer_pane)

# --- control stream: fixes the PTY size and lets us inject raw bytes ----------
control = subprocess.Popen(
    [EXE, "--session", OUTER, "terminal", "session", "control", outer_pane,
     "--cols", str(COLS), "--rows", str(ROWS)],
    env=ENV, stdin=subprocess.PIPE, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
    creationflags=CREATE_NO_WINDOW)


def raw(data: bytes):
    frame = json.dumps({"input": {"data_base64": base64.b64encode(data).decode()}}) + "\n"
    control.stdin.write(frame.encode())
    control.stdin.flush()


def sgr(button, x, y, pressed):
    # x,y are 0-based screen cells; SGR is 1-based
    return f"\x1b[<{button};{x + 1};{y + 1}{'M' if pressed else 'm'}".encode()


def click(button, x, y, moves=(), release=True, gap=0.05):
    raw(sgr(button, x, y, True))
    time.sleep(gap)
    for (mx, my) in moves:
        raw(sgr(button + 32, mx, my, True))
        time.sleep(gap)
    if release:
        lx, ly = (moves[-1] if moves else (x, y))
        raw(sgr(button, lx, ly, False))
    time.sleep(0.4)


def move(x, y):
    raw(sgr(35, x, y, True))
    time.sleep(0.05)


time.sleep(1.0)
wait_for(OUTER, outer_pane, ">", 20)  # shell prompt
# launch the inner TUI in that shell
raw(f'& "{EXE}" --session {INNER}\r'.encode())
wait_for(OUTER, outer_pane, "spaces", 20)
time.sleep(2.0)
wait_server(INNER)
print("inner up; screen:")
print(read_pane(OUTER, outer_pane))

results = {}


def screen():
    return read_pane(OUTER, outer_pane)


def overlay():
    s = screen()
    for line in s.splitlines():
        if "mouse#" in line or "Mouse" in line or "mouse:" in line:
            return line.strip()
    return "(no overlay)"


# --- scenario 0: PTYs are sized to the visible pane area, not the terminal ---
# Sidebar (26 cells incl. divider) + tab bar (1 row)
# must be excluded from the layout area the server uses, or the inner app
# wraps past the pane frame.
SIDEBAR = 26
BAR_ROWS = 3
TAB_ROWS = 1
inner_layout = cli_json(INNER, "pane", "layout")["layout"]
print("inner layout area", inner_layout["area"])
# The composer lives inside the pane now, so only the tab bar leaves the area.
results["layout_area_excludes_sidebar_and_tabbar"] = (
    inner_layout["area"]["width"] == COLS - SIDEBAR
    and inner_layout["area"]["height"] == ROWS - TAB_ROWS
)
raw(b"mode con\r")
time.sleep(1.5)
inner_text = cli(INNER, "pane", "read", "w1:p1", "--source", "visible")
m = re.search(r"Columns:\s+(\d+)", inner_text) or re.search(r"Columnas:\s+(\d+)", inner_text)
pty_cols = int(m.group(1)) if m else -1
print("inner pty cols (mode con):", pty_cols)
results["lone_pane_pty_width_is_visible_width"] = pty_cols == COLS - SIDEBAR
raw(b"cls\r")
time.sleep(0.5)

# --- scenario 1: right-click twice in the same pane --------------------------
inner_panes = cli_json(INNER, "pane", "list")
print("inner panes", inner_panes)
# geometry: find the content cell of the first pane via `pane layout`
x, y = 60, 15
click(2, x, y)
s1 = screen()
print("--- after right-click #1 ---")
print(s1)
results["rc1_menu"] = ("Copy selection" in s1) or ("Paste" in s1 and "Close" in s1)
print("overlay:", overlay())

# close it with Esc
raw(b"\x1b")
time.sleep(0.4)
s1b = screen()
results["rc1_closed_esc"] = "Copy selection" not in s1b

click(2, x + 5, y + 2)
s2 = screen()
print("--- after right-click #2 (after Esc) ---")
print(s2)
results["rc2_menu"] = "Copy selection" in s2
print("overlay:", overlay())

# left-click an item (first item = Copy selection) to close via activation
raw(b"\x1b")
time.sleep(0.3)

# right-click #3, then left click outside to close, then right-click #4
click(2, x, y)
s3 = screen()
results["rc3_menu"] = "Copy selection" in s3
click(0, 30, 30)  # left click outside the menu
s3b = screen()
results["rc3_closed_leftclick"] = "Copy selection" not in s3b
click(2, x + 2, y + 1)
s4 = screen()
print("--- after right-click #4 (after left-click close) ---")
print(s4)
results["rc4_menu"] = "Copy selection" in s4

# right-click #5 while menu open but elsewhere: must re-anchor (menu visible)
click(2, 45, 8)
s5 = screen()
results["rc5_reanchor"] = "Copy selection" in s5
# left click an item: select "Paste" row? Just close with Esc
raw(b"\x1b")
time.sleep(0.3)

# --- scenario 2: WT-style with motion events between press and release -------
click(2, x, y, moves=[(x + 1, y), (x + 1, y + 1)])
s6 = screen()
results["rc6_with_drag_motion"] = "Copy selection" in s6
raw(b"\x1b")
time.sleep(0.3)
move(40, 20)
move(41, 20)
click(2, x, y)
s7 = screen()
results["rc7_after_hover_motion"] = "Copy selection" in s7
raw(b"\x1b")
time.sleep(0.3)

# --- scenario 3: resize via divider drag ------------------------------------
cli(INNER, "pane", "split", "--direction", "right")
time.sleep(1.0)
layout_before = cli(INNER, "pane", "layout")
print("layout before:", layout_before)
print(screen())
results["_layout_before"] = layout_before
raw(b"mode con\r")
time.sleep(1.5)
focused = cli_json(INNER, "pane", "layout")["layout"]
focused_rect = next(p["rect"] for p in focused["panes"] if p["focused"])
focused_text = cli(INNER, "pane", "read", focused["focused_pane_id"], "--source", "visible")
m = re.search(r"Column(?:s|as):\s+(\d+)", focused_text)
split_cols = int(m.group(1)) if m else -1
print("split pane rect", focused_rect, "pty cols", split_cols)
results["split_pane_pty_width_is_rect_minus_frame"] = split_cols == focused_rect["width"] - 2
raw(b"cls\r")
time.sleep(0.5)

# find divider: use the outer screen: look for a vertical border column.
scr = screen().splitlines()
# pick a middle row and find '│' characters after the sidebar
row = scr[ROWS // 2] if len(scr) > ROWS // 2 else ""
cols_with_border = [i for i, ch in enumerate(row) if ch in "│┃|"]
print("border cols on mid row:", cols_with_border)
results["_border_cols"] = cols_with_border
# the divider between two panes: a border column that's not the first/last
mid_candidates = [c for c in cols_with_border if 20 < c < COLS - 2]
results["_divider_candidates"] = mid_candidates
if mid_candidates:
    # pick the one nearest to the center of the main area
    main_center = (COLS + 26) // 2
    dx = min(mid_candidates, key=lambda c: abs(c - main_center))
    dy = ROWS // 2
    print("dragging divider at", dx, dy)
    click(0, dx, dy, moves=[(dx + 3, dy), (dx + 6, dy), (dx + 10, dy)])
    layout_after = cli(INNER, "pane", "layout")
    print("layout after:", layout_after)
    results["_layout_after"] = layout_after
    results["resize_changed_layout"] = layout_after != layout_before
    print(screen())

# --- scenario 4: WARP MODE — the host never forwards the right RELEASE -------
# Reset: a release for every button so conhost's mask is clean, then mimic Warp.
raw(sgr(0, x, y, False)); raw(sgr(2, x, y, False)); time.sleep(0.3)
raw(b"\x1b"); time.sleep(0.3)
layout_w0 = cli(INNER, "pane", "layout")
scr = screen().splitlines()
row = scr[ROWS // 2]
cols_with_border = [i for i, ch in enumerate(row) if ch in "│┃|"]
mid_candidates = [c for c in cols_with_border if 20 < c < COLS - 2]
main_center = (COLS + 26) // 2
dx = min(mid_candidates, key=lambda c: abs(c - main_center)) if mid_candidates else 73
dy = ROWS // 2
print("warp-mode divider at", dx, dy, "candidates", mid_candidates)

def warp_right_press(px, py):
    raw(sgr(2, px, py, True)); time.sleep(0.4)   # press only — Warp eats the release

warp_right_press(40, 10)
w1 = screen(); results["warp_rc1_menu"] = "Copy selection" in w1
raw(b"\x1b"); time.sleep(0.3)
warp_right_press(44, 12)
w2 = screen(); results["warp_rc2_menu_after_stuck_bit"] = "Copy selection" in w2
raw(b"\x1b"); time.sleep(0.3)
warp_right_press(90, 20)
w3 = screen(); results["warp_rc3_menu_other_pane"] = "Copy selection" in w3
print(w3)
raw(b"\x1b"); time.sleep(0.3)
# left drag on the divider while the right bit is stuck
click(0, dx, dy, moves=[(dx - 3, dy), (dx - 6, dy), (dx - 10, dy)])
layout_w1 = cli(INNER, "pane", "layout")
results["warp_resize_with_stuck_right"] = layout_w1 != layout_w0
print("warp layout before:", layout_w0.strip()[-200:])
print("warp layout after: ", layout_w1.strip()[-200:])
print(screen())
# selection drag while stuck: drag over the shell prompt text, expect a copy toast
click(0, 30, 3, moves=[(36, 3), (42, 3), (50, 3)])
w4 = screen(); results["warp_drag_select_copies"] = "Copied" in w4
print(w4)
# and the right click still works after all that
warp_right_press(40, 10)
w5 = screen(); results["warp_rc4_menu_after_drags"] = "Copy selection" in w5
raw(b"\x1b"); time.sleep(0.3)

# --- scenario 5: right-click on a tab and on a workspace (sidebar) -----------
raw(sgr(0, x, y, False)); raw(sgr(2, x, y, False)); time.sleep(0.3)
raw(b"\x1b"); time.sleep(0.3)
# tab bar is row 0 of the main area: first tab block starts at x=SIDEBAR
click(2, SIDEBAR + 3, 0)
t1 = screen()
results["tab_menu_opens"] = " Tab " in t1 and "New tab" in t1
print(t1)
raw(b"\x1b"); time.sleep(0.3)
# workspace rows start under the one-row Workspaces header
click(2, 4, 1)
t2 = screen()
results["workspace_menu_opens"] = " Workspace " in t2 and "New workspace" in t2
print(t2)
# activate "New workspace" (first item) with a left click on it: the menu is
# anchored at the click row (1), so its first item sits on row 2
click(0, 6, 2)
time.sleep(1.0)
ws = cli_json(INNER, "workspace", "list")
ws_count = len(ws.get("workspaces", []))
print("workspaces after New workspace:", ws_count)
results["workspace_menu_new_creates_workspace"] = ws_count == 2
# tab menu → Close on a second tab
cli(INNER, "tab", "create")
time.sleep(0.8)
tabs_before = len(cli_json(INNER, "tab", "list").get("tabs", []))
click(2, SIDEBAR + 14, 0)   # second tab block
t3 = screen()
print(t3)
click(0, SIDEBAR + 16, 3)   # third item = Close (rows: 1 New tab, 2 Rename, 3 Close)
time.sleep(0.8)
tabs_after = len(cli_json(INNER, "tab", "list").get("tabs", []))
print("tabs before/after close:", tabs_before, tabs_after)
results["tab_menu_close_closes_tab"] = tabs_after == tabs_before - 1

# --- scenario 6: drag a tab along the bar to reorder it ----------------------
cli(INNER, "tab", "create")
time.sleep(0.8)
tabs = cli_json(INNER, "tab", "list").get("tabs", [])
focused_ws = next((t["workspace_id"] for t in tabs if t.get("focused")), None)
ids_before = [t["tab_id"] for t in tabs if t["workspace_id"] == focused_ws]
print("tab order before drag:", ids_before)
# Tab blocks are 10 cells wide: first [SIDEBAR, SIDEBAR+10), second
# [SIDEBAR+10, SIDEBAR+20) with its midpoint at SIDEBAR+15. Grab the first
# and carry it past that midpoint: they swap while the button is still held.
click(0, SIDEBAR + 3, 0, moves=[(SIDEBAR + 8, 0), (SIDEBAR + 12, 0), (SIDEBAR + 17, 0)])
time.sleep(0.8)
tabs = cli_json(INNER, "tab", "list").get("tabs", [])
ids_after = [t["tab_id"] for t in tabs if t["workspace_id"] == focused_ws]
print("tab order after drag:", ids_after)
print(screen())
results["tab_drag_reorders_tabs"] = (
    len(ids_before) >= 2 and ids_after[:2] == [ids_before[1], ids_before[0]]
)
# Dragging it back (now the second block) restores the original order.
click(0, SIDEBAR + 13, 0, moves=[(SIDEBAR + 8, 0), (SIDEBAR + 3, 0)])
time.sleep(0.8)
tabs = cli_json(INNER, "tab", "list").get("tabs", [])
ids_back = [t["tab_id"] for t in tabs if t["workspace_id"] == focused_ws]
print("tab order after dragging back:", ids_back)
results["tab_drag_back_restores_order"] = ids_back == ids_before

print("overlay:", overlay())
print("\n=== RESULTS ===")
failed = 0
for k, v in results.items():
    if not k.startswith("_"):
        print(f"{k}: {'PASS' if v else 'FAIL'}")
        failed += 0 if v else 1

# --- teardown ----------------------------------------------------------------
try:
    cli(INNER, "server", "stop", check=False)
    cli(OUTER, "server", "stop", check=False)
except Exception:
    pass
try:
    control.stdin.close()
except Exception:
    pass
time.sleep(1)
for p in (control, server):
    try:
        p.kill()
    except Exception:
        pass
print("home:", home)
sys.exit(1 if failed else 0)
