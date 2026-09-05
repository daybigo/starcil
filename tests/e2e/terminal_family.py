"""Real ConPTY host with controlled terminal replies, using only ctypes.

An outer Starcil pane always advertises kitty. This host advertises it only
for the kitty family, so each test measures a distinct outer terminal path.
"""
import ctypes as C
from ctypes import wintypes as W
import re
import subprocess
import threading
import time

K = C.WinDLL("kernel32", use_last_error=True)


class COORD(C.Structure):
    _fields_ = [("X", W.SHORT), ("Y", W.SHORT)]


class STARTUPINFO(C.Structure):
    _fields_ = [("cb", W.DWORD), ("lpReserved", W.LPWSTR), ("lpDesktop", W.LPWSTR),
                ("lpTitle", W.LPWSTR), ("dwX", W.DWORD), ("dwY", W.DWORD),
                ("dwXSize", W.DWORD), ("dwYSize", W.DWORD), ("dwXCountChars", W.DWORD),
                ("dwYCountChars", W.DWORD), ("dwFillAttribute", W.DWORD),
                ("dwFlags", W.DWORD), ("wShowWindow", W.WORD), ("cbReserved2", W.WORD),
                ("lpReserved2", C.c_void_p), ("hStdInput", W.HANDLE),
                ("hStdOutput", W.HANDLE), ("hStdError", W.HANDLE)]


class STARTUPINFOEX(C.Structure):
    _fields_ = [("StartupInfo", STARTUPINFO), ("lpAttributeList", C.c_void_p)]


class PROCESS_INFORMATION(C.Structure):
    _fields_ = [("hProcess", W.HANDLE), ("hThread", W.HANDLE),
                ("dwProcessId", W.DWORD), ("dwThreadId", W.DWORD)]


K.CreatePipe.argtypes = [C.POINTER(W.HANDLE), C.POINTER(W.HANDLE), C.c_void_p, W.DWORD]
K.CreatePseudoConsole.argtypes = [COORD, W.HANDLE, W.HANDLE, W.DWORD, C.POINTER(W.HANDLE)]
K.CreatePseudoConsole.restype = C.c_long
K.ClosePseudoConsole.argtypes = [W.HANDLE]
K.InitializeProcThreadAttributeList.argtypes = [C.c_void_p, W.DWORD, W.DWORD, C.POINTER(C.c_size_t)]
K.UpdateProcThreadAttribute.argtypes = [C.c_void_p, W.DWORD, C.c_size_t, C.c_void_p, C.c_size_t, C.c_void_p, C.c_void_p]
K.DeleteProcThreadAttributeList.argtypes = [C.c_void_p]
K.CreateProcessW.argtypes = [W.LPCWSTR, W.LPWSTR, C.c_void_p, C.c_void_p, W.BOOL,
                            W.DWORD, C.c_void_p, W.LPCWSTR, C.POINTER(STARTUPINFOEX), C.POINTER(PROCESS_INFORMATION)]
K.ReadFile.argtypes = [W.HANDLE, C.c_void_p, W.DWORD, C.POINTER(W.DWORD), C.c_void_p]
K.WriteFile.argtypes = K.ReadFile.argtypes
K.CloseHandle.argtypes = [W.HANDLE]
K.WaitForSingleObject.argtypes = [W.HANDLE, W.DWORD]
K.TerminateProcess.argtypes = [W.HANDLE, W.UINT]


def win32_key(vk, scan, char, state):
    return (f"\x1b[{vk};{scan};{char};1;{state};1_"
            f"\x1b[{vk};{scan};{char};0;{state};1_").encode()


def passthrough(data):
    return b"".join(f"\x1b[0;0;{value};1;0;1_".encode() for value in data)


FAMILIES = {
    "win32": {"shift+enter": win32_key(13, 28, 13, 16),
              "alt+enter": win32_key(13, 28, 13, 2),
              "ctrl+j": win32_key(74, 36, 10, 8),
              "ctrl+enter": win32_key(13, 28, 10, 8)},
    "vt": {"shift+enter": b"\r", "alt+enter": b"\x1b\r", "ctrl+j": b"\n"},
    "kitty": {"shift+enter": b"\x1b[13;2u", "alt+enter": b"\x1b[13;3u", "ctrl+j": b"\x1b[106;5u"},
}


class Terminal:
    def __init__(self, command, env, cwd, family, cols=140, rows=45):
        self.family = family
        self.output = bytearray()
        self.replies = []
        self.error = None
        self.lock = threading.Lock()
        self.input = W.HANDLE()
        self.output_handle = W.HANDLE()
        input_read, output_write = W.HANDLE(), W.HANDLE()
        self.console = W.HANDLE()
        self.process = PROCESS_INFORMATION()
        self.closed = False
        self.thread = None
        attributes = None
        try:
            for read, write in [(input_read, self.input), (self.output_handle, output_write)]:
                if not K.CreatePipe(C.byref(read), C.byref(write), None, 0):
                    raise C.WinError(C.get_last_error())
            hr = K.CreatePseudoConsole(COORD(cols, rows), input_read, output_write, 0, C.byref(self.console))
            if hr != 0:
                raise OSError(f"CreatePseudoConsole HRESULT={hr:#x}")
            size = C.c_size_t()
            K.InitializeProcThreadAttributeList(None, 1, 0, C.byref(size))
            attributes = C.create_string_buffer(size.value)
            if not K.InitializeProcThreadAttributeList(attributes, 1, 0, C.byref(size)):
                raise C.WinError(C.get_last_error())
            if not K.UpdateProcThreadAttribute(attributes, 0, 0x00020016, self.console,
                                               C.sizeof(W.HANDLE), None, None):
                raise C.WinError(C.get_last_error())
            startup = STARTUPINFOEX()
            startup.StartupInfo.cb = C.sizeof(startup)
            # Match portable-pty: invalid explicit std handles make Windows
            # assign the pseudoconsole instead of inheriting this log pipe.
            startup.StartupInfo.dwFlags = 0x00000100
            startup.StartupInfo.hStdInput = W.HANDLE(-1)
            startup.StartupInfo.hStdOutput = W.HANDLE(-1)
            startup.StartupInfo.hStdError = W.HANDLE(-1)
            startup.lpAttributeList = C.cast(attributes, C.c_void_p)
            block = C.create_unicode_buffer("\0".join(f"{k}={v}" for k, v in sorted(env.items(), key=lambda kv: kv[0].upper())) + "\0\0")
            cmdline = C.create_unicode_buffer(subprocess.list2cmdline(command))
            if not K.CreateProcessW(None, cmdline, None, None, False, 0x00080000 | 0x00000400,
                                    block, cwd, C.byref(startup), C.byref(self.process)):
                raise C.WinError(C.get_last_error())
            self.thread = threading.Thread(target=self._drain, daemon=True)
            self.thread.start()
        except BaseException:
            self.close()
            raise
        finally:
            for handle in [input_read, output_write]:
                if handle: K.CloseHandle(handle)
            if attributes: K.DeleteProcThreadAttributeList(attributes)

    def raw(self, data):
        with self.lock:
            count = W.DWORD()
            if not K.WriteFile(self.input, data, len(data), C.byref(count), None) or count.value != len(data):
                raise C.WinError(C.get_last_error())

    def _drain(self):
        pending = b""
        pattern = re.compile(rb"\x1b\[(\?u|0?c|6n|>1u|<1u)")
        try:
            while True:
                buf = C.create_string_buffer(16384)
                count = W.DWORD()
                if not K.ReadFile(self.output_handle, buf, len(buf), C.byref(count), None) or not count.value:
                    return
                data = buf.raw[:count.value]
                self.output.extend(data)
                pending += data
                matches = list(pattern.finditer(pending))
                for match in matches:
                    query = match.group(1)
                    self.replies.append(query.decode())
                    response = None
                    if query == b"?u" and self.family == "kitty": response = b"\x1b[?0u"
                    elif query in (b"c", b"0c"): response = b"\x1b[?1;2c"
                    elif query == b"6n": response = b"\x1b[1;1R"
                    if response:
                        self.raw(passthrough(response) if self.family == "win32" else response)
                pending = pending[matches[-1].end():] if matches else pending
                pending = pending[-32:]
        except BaseException as error:
            if not self.closed: self.error = str(error)

    def text(self):
        return bytes(self.output).decode("utf-8", "replace")

    def wait_text(self, needle, timeout=20):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if needle in self.text(): return
            if self.error: raise RuntimeError(self.error)
            time.sleep(0.05)
        raise TimeoutError(f"terminal did not show {needle!r}: {self.text()[-2000:]!r}")

    def wait(self, timeout=5):
        return K.WaitForSingleObject(self.process.hProcess, int(timeout * 1000)) == 0

    def close(self):
        if self.closed: return
        self.closed = True
        if self.process.hProcess and not self.wait(2):
            K.TerminateProcess(self.process.hProcess, 1)
            self.wait(3)
        if self.console: K.ClosePseudoConsole(self.console)
        if self.thread: self.thread.join(timeout=3)
        for handle in [self.input, self.output_handle, self.process.hThread, self.process.hProcess]:
            if handle: K.CloseHandle(handle)
