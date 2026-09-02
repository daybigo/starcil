"""Mouse-tracking test app (stands in for Claude Code's fullscreen mode).

Enters the alternate screen, enables SGR mouse tracking, and prints every
escape sequence it reads from the console as VT input. Quits on `q`.
Run it inside a starcil pane and inject wheel events over that pane: starcil
must forward them as `ESC[<64;x;yM` / `ESC[<65;x;yM` reports.
"""
import ctypes
import msvcrt
import sys
import time

k32 = ctypes.windll.kernel32
STD_INPUT_HANDLE = -10
STD_OUTPUT_HANDLE = -11
ENABLE_VIRTUAL_TERMINAL_INPUT = 0x0200
ENABLE_VIRTUAL_TERMINAL_PROCESSING = 0x0004
ENABLE_LINE_INPUT = 0x0002
ENABLE_ECHO_INPUT = 0x0004
ENABLE_PROCESSED_INPUT = 0x0001

hin = k32.GetStdHandle(STD_INPUT_HANDLE)
hout = k32.GetStdHandle(STD_OUTPUT_HANDLE)
old_in = ctypes.c_uint32()
old_out = ctypes.c_uint32()
k32.GetConsoleMode(hin, ctypes.byref(old_in))
k32.GetConsoleMode(hout, ctypes.byref(old_out))
k32.SetConsoleMode(hout, old_out.value | ENABLE_VIRTUAL_TERMINAL_PROCESSING)
k32.SetConsoleMode(
    hin,
    (old_in.value | ENABLE_VIRTUAL_TERMINAL_INPUT)
    & ~(ENABLE_LINE_INPUT | ENABLE_ECHO_INPUT | ENABLE_PROCESSED_INPUT),
)

out = sys.stdout
out.write("\x1b[?1049h\x1b[2J\x1b[H\x1b[?1000h\x1b[?1006h")
out.write("MOUSETRAP_READY (alt screen + SGR mouse tracking). q quits.\r\n")
out.flush()

buffer = ""
last = time.time()
try:
    while True:
        if msvcrt.kbhit():
            ch = msvcrt.getwch()
            last = time.time()
            if ch == "q" and not buffer:
                break
            buffer += ch
            if buffer.startswith("\x1b") and (buffer[-1] in "Mm" and buffer.startswith("\x1b[<")):
                out.write("GOT " + repr(buffer) + "\r\n")
                out.flush()
                buffer = ""
        else:
            if buffer and time.time() - last > 0.2:
                out.write("GOT " + repr(buffer) + "\r\n")
                out.flush()
                buffer = ""
            time.sleep(0.01)
finally:
    out.write("\x1b[?1006l\x1b[?1000l\x1b[?1049l")
    # Back on the primary screen: leave real history behind so the harness
    # can prove the wheel scrolls the scrollback again once tracking is off.
    for i in range(1, 81):
        out.write(f"HISTORY-{i}\r\n")
    out.write("MOUSETRAP_DONE\r\n")
    out.flush()
    k32.SetConsoleMode(hin, old_in.value)
    k32.SetConsoleMode(hout, old_out.value)
