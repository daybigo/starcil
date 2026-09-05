"""Console input probe: prints every KEY_EVENT_RECORD the console delivers.

Run inside a ConPTY (a Starcil pane) and inject bytes into that PTY to see what
conhost synthesises for them.

    --vt     enable ENABLE_VIRTUAL_TERMINAL_INPUT (a VT-native child)
    --kitty  behave like a kitty-protocol client: push flags 1 at startup
    --query  send the kitty flags query (CSI ? u) at startup
"""
import ctypes
import sys
from ctypes import wintypes

k32 = ctypes.windll.kernel32
STD_INPUT_HANDLE = -10
ENABLE_PROCESSED_INPUT = 0x0001
ENABLE_LINE_INPUT = 0x0002
ENABLE_ECHO_INPUT = 0x0004
ENABLE_VIRTUAL_TERMINAL_INPUT = 0x0200


class KEY_EVENT_RECORD(ctypes.Structure):
    _fields_ = [
        ("bKeyDown", wintypes.BOOL),
        ("wRepeatCount", wintypes.WORD),
        ("wVirtualKeyCode", wintypes.WORD),
        ("wVirtualScanCode", wintypes.WORD),
        ("UnicodeChar", wintypes.WCHAR),
        ("dwControlKeyState", wintypes.DWORD),
    ]


class EVENT_UNION(ctypes.Union):
    _fields_ = [("KeyEvent", KEY_EVENT_RECORD), ("raw", ctypes.c_byte * 16)]


class INPUT_RECORD(ctypes.Structure):
    _fields_ = [("EventType", wintypes.WORD), ("Event", EVENT_UNION)]


def main():
    handle = k32.GetStdHandle(STD_INPUT_HANDLE)
    mode = wintypes.DWORD()
    k32.GetConsoleMode(handle, ctypes.byref(mode))
    new_mode = mode.value & ~(ENABLE_PROCESSED_INPUT | ENABLE_LINE_INPUT | ENABLE_ECHO_INPUT)
    if "--vt" in sys.argv:
        new_mode |= ENABLE_VIRTUAL_TERMINAL_INPUT
    k32.SetConsoleMode(handle, new_mode)
    if "--kitty" in sys.argv:
        sys.stdout.write("\x1b[>1u")
    if "--query" in sys.argv:
        sys.stdout.write("\x1b[?u")
    print("READY mode=%#x" % new_mode, flush=True)
    record = INPUT_RECORD()
    count = wintypes.DWORD()
    index = 0
    while True:
        ok = k32.ReadConsoleInputW(handle, ctypes.byref(record), 1, ctypes.byref(count))
        if not ok or count.value == 0:
            break
        if record.EventType != 1:
            continue
        key = record.Event.KeyEvent
        index += 1
        char = ord(key.UnicodeChar) if key.UnicodeChar else 0
        print(
            "K%03d down=%d vk=%#04x sc=%#04x ch=%#06x cs=%#06x"
            % (index, key.bKeyDown, key.wVirtualKeyCode, key.wVirtualScanCode, char, key.dwControlKeyState),
            flush=True,
        )
        if char == ord("q") and key.bKeyDown:
            break


if __name__ == "__main__":
    main()
