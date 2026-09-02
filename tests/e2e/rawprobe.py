"""Raw console INPUT_RECORD probe: mirrors what crossterm's EnableMouseCapture
does (SetConsoleMode(ENABLE_MOUSE_INPUT|ENABLE_WINDOW_INPUT|ENABLE_EXTENDED_FLAGS))
and logs every MOUSE_EVENT_RECORD it reads, untouched, to a file.
Usage: python rawprobe.py <logfile> [seconds]
"""
import ctypes
import sys
import time
from ctypes import wintypes

log_path = sys.argv[1]
seconds = float(sys.argv[2]) if len(sys.argv) > 2 else 20.0

k32 = ctypes.windll.kernel32
STD_INPUT_HANDLE = -10
STD_OUTPUT_HANDLE = -11
ENABLE_MOUSE_INPUT = 0x0010
ENABLE_WINDOW_INPUT = 0x0008
ENABLE_EXTENDED_FLAGS = 0x0080
ENABLE_VIRTUAL_TERMINAL_PROCESSING = 0x0004


class COORD(ctypes.Structure):
    _fields_ = [("X", ctypes.c_short), ("Y", ctypes.c_short)]


class MOUSE_EVENT_RECORD(ctypes.Structure):
    _fields_ = [("dwMousePosition", COORD), ("dwButtonState", wintypes.DWORD),
                ("dwControlKeyState", wintypes.DWORD), ("dwEventFlags", wintypes.DWORD)]


class KEY_EVENT_RECORD(ctypes.Structure):
    _fields_ = [("bKeyDown", wintypes.BOOL), ("wRepeatCount", wintypes.WORD),
                ("wVirtualKeyCode", wintypes.WORD), ("wVirtualScanCode", wintypes.WORD),
                ("UnicodeChar", wintypes.WCHAR), ("dwControlKeyState", wintypes.DWORD)]


class EVENT_UNION(ctypes.Union):
    _fields_ = [("KeyEvent", KEY_EVENT_RECORD), ("MouseEvent", MOUSE_EVENT_RECORD),
                ("pad", ctypes.c_byte * 16)]


class INPUT_RECORD(ctypes.Structure):
    _fields_ = [("EventType", wintypes.WORD), ("Event", EVENT_UNION)]


hin = k32.GetStdHandle(STD_INPUT_HANDLE)
hout = k32.GetStdHandle(STD_OUTPUT_HANDLE)
old_mode = wintypes.DWORD()
k32.GetConsoleMode(hin, ctypes.byref(old_mode))
out_mode = wintypes.DWORD()
k32.GetConsoleMode(hout, ctypes.byref(out_mode))
k32.SetConsoleMode(hout, out_mode.value | ENABLE_VIRTUAL_TERMINAL_PROCESSING)
# alt screen like the TUI, so the host treats us as a full-screen app
sys.stdout.write("\x1b[?1049h\x1b[2J\x1b[H")
sys.stdout.write("RAW MOUSE PROBE — right-click twice, then left-drag. %.0fs\r\n" % seconds)
sys.stdout.flush()
k32.SetConsoleMode(hin, ENABLE_MOUSE_INPUT | ENABLE_WINDOW_INPUT | ENABLE_EXTENDED_FLAGS)

log = open(log_path, "w", encoding="utf-8", buffering=1)
log.write("start mode=0x%x\n" % old_mode.value)
deadline = time.time() + seconds
rec = INPUT_RECORD()
n = wintypes.DWORD()
count = 0
while time.time() < deadline:
    avail = wintypes.DWORD()
    k32.GetNumberOfConsoleInputEvents(hin, ctypes.byref(avail))
    if avail.value == 0:
        time.sleep(0.01)
        continue
    if not k32.ReadConsoleInputW(hin, ctypes.byref(rec), 1, ctypes.byref(n)):
        break
    count += 1
    if rec.EventType == 2:  # MOUSE_EVENT
        m = rec.Event.MouseEvent
        line = "MOUSE pos=(%d,%d) buttons=0x%x flags=0x%x ctrl=0x%x" % (
            m.dwMousePosition.X, m.dwMousePosition.Y, m.dwButtonState, m.dwEventFlags,
            m.dwControlKeyState)
    elif rec.EventType == 1:
        k = rec.Event.KeyEvent
        line = "KEY down=%d vk=0x%x ch=%r" % (k.bKeyDown, k.wVirtualKeyCode, k.UnicodeChar)
        if k.bKeyDown and k.wVirtualKeyCode == 0x51:  # 'q' quits
            log.write(line + "\n")
            break
    else:
        line = "EVENT type=%d" % rec.EventType
    log.write(line + "\n")
    sys.stdout.write(line + "\r\n")
    sys.stdout.flush()
log.write("end count=%d\n" % count)
log.close()
k32.SetConsoleMode(hin, old_mode.value)
sys.stdout.write("\x1b[?1049l")
sys.stdout.flush()
