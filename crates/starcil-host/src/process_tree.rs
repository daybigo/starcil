//! Cheap process-tree lookup for the agent tick: which programs run under a
//! pane's shell right now.
//!
//! `pane.process_info` shells out to PowerShell (hundreds of ms) — fine for a
//! one-off query, useless every 300ms. This module reads the process table
//! natively (Windows: one Toolhelp32 snapshot, Linux: `/proc`), caches it for
//! a tick, and walks parent links from the shell pid. Platforms without an
//! implementation answer `None` ("unknown") so callers never mistake a blind
//! host for an idle shell.

/// Lowercase program names under `shell_pid` (children, grandchildren, …),
/// without the `.exe` suffix. `None` when the platform cannot tell.
pub fn descendant_names(shell_pid: u32) -> Option<Vec<String>> {
    table::with_table(|table| collect_descendants(shell_pid, table))
}

/// Current directory of `pid` as the OS records it (Windows: the process
/// parameters block; Linux: `/proc/<pid>/cwd`). `None` when the platform
/// cannot tell or the process is gone. Trailing separators are dropped except
/// on a bare root (`C:\`, `/`).
pub fn process_cwd(pid: u32) -> Option<String> {
    cwd::read(pid).map(|path| normalize_cwd(&path))
}

pub(crate) fn normalize_cwd(path: &str) -> String {
    let trimmed = path.trim_end_matches(['\\', '/']);
    if trimmed.is_empty() {
        // "/" on unix.
        return path.chars().take(1).collect();
    }
    if trimmed.len() == 2 && trimmed.as_bytes()[1] == b':' {
        // A drive root keeps its separator: `C:` alone means "the current
        // directory on C:" to every Windows shell.
        return format!("{trimmed}\\");
    }
    trimmed.to_owned()
}

/// One row of the process table: (pid, parent pid, executable name).
pub(crate) type ProcessRow = (u32, u32, String);

/// Breadth-first walk over parent links. Pid reuse can make the table look
/// cyclic, so every pid is visited once.
pub(crate) fn collect_descendants(root: u32, table: &[ProcessRow]) -> Vec<String> {
    let mut names = Vec::new();
    let mut frontier = vec![root];
    let mut visited = std::collections::BTreeSet::new();
    visited.insert(root);
    while let Some(parent) = frontier.pop() {
        for (pid, ppid, name) in table {
            if *ppid == parent && visited.insert(*pid) {
                names.push(normalize_name(name));
                frontier.push(*pid);
            }
        }
    }
    names
}

fn normalize_name(name: &str) -> String {
    let lower = name.trim().to_ascii_lowercase();
    lower
        .strip_suffix(".exe")
        .map(str::to_owned)
        .unwrap_or(lower)
}

#[cfg(any(windows, target_os = "linux"))]
mod table {
    use std::sync::Mutex;
    use std::time::{Duration, Instant};

    use super::ProcessRow;

    /// One snapshot serves every pane of a tick.
    const TTL: Duration = Duration::from_millis(150);
    static CACHE: Mutex<Option<(Instant, Vec<ProcessRow>)>> = Mutex::new(None);

    pub(super) fn with_table<T>(f: impl FnOnce(&[ProcessRow]) -> T) -> Option<T> {
        let mut guard = CACHE.lock().ok()?;
        let fresh = guard
            .as_ref()
            .is_some_and(|(taken, _)| taken.elapsed() < TTL);
        if !fresh {
            *guard = Some((Instant::now(), super::platform::read_table()?));
        }
        guard.as_ref().map(|(_, table)| f(table))
    }
}

#[cfg(not(any(windows, target_os = "linux")))]
mod table {
    use super::ProcessRow;

    pub(super) fn with_table<T>(_f: impl FnOnce(&[ProcessRow]) -> T) -> Option<T> {
        None
    }
}

#[cfg(all(windows, target_pointer_width = "64"))]
mod cwd {
    use std::ffi::c_void;

    const PROCESS_QUERY_INFORMATION: u32 = 0x0400;
    const PROCESS_VM_READ: u32 = 0x0010;
    const PROCESS_BASIC_INFORMATION_CLASS: u32 = 0;
    /// `PEB.ProcessParameters` on x64.
    const PEB_PROCESS_PARAMETERS: usize = 0x20;
    /// `RTL_USER_PROCESS_PARAMETERS.CurrentDirectory.DosPath` on x64: a
    /// `UNICODE_STRING` (length in bytes, max length, padding, buffer).
    const PARAMS_CURRENT_DIRECTORY: usize = 0x38;

    /// `PROCESS_BASIC_INFORMATION` (winternl.h) on x64.
    #[repr(C)]
    #[allow(non_snake_case)]
    struct ProcessBasicInformation {
        ExitStatus: i32,
        PebBaseAddress: usize,
        AffinityMask: usize,
        BasePriority: i32,
        UniqueProcessId: usize,
        InheritedFromUniqueProcessId: usize,
    }

    #[link(name = "kernel32")]
    extern "system" {
        fn OpenProcess(access: u32, inherit: i32, pid: u32) -> isize;
        fn ReadProcessMemory(
            process: isize,
            base: *const c_void,
            buffer: *mut c_void,
            size: usize,
            read: *mut usize,
        ) -> i32;
        fn CloseHandle(handle: isize) -> i32;
    }

    #[link(name = "ntdll")]
    extern "system" {
        fn NtQueryInformationProcess(
            process: isize,
            class: u32,
            info: *mut c_void,
            length: u32,
            returned: *mut u32,
        ) -> i32;
    }

    /// The shell's current directory straight from its process parameters
    /// (what `Get-Location` / `cd` would print), no shell round-trip. Works
    /// for same-user processes without elevation.
    pub(super) fn read(pid: u32) -> Option<String> {
        // SAFETY: plain Win32/NT calls on a handle we open and close here;
        // every read goes through ReadProcessMemory into local buffers whose
        // sizes are passed alongside.
        unsafe {
            let process = OpenProcess(PROCESS_QUERY_INFORMATION | PROCESS_VM_READ, 0, pid);
            if process == 0 {
                return None;
            }
            let result = read_from(process);
            CloseHandle(process);
            result
        }
    }

    unsafe fn read_from(process: isize) -> Option<String> {
        let mut info: ProcessBasicInformation = std::mem::zeroed();
        let mut returned = 0u32;
        let status = NtQueryInformationProcess(
            process,
            PROCESS_BASIC_INFORMATION_CLASS,
            (&mut info as *mut ProcessBasicInformation).cast(),
            std::mem::size_of::<ProcessBasicInformation>() as u32,
            &mut returned,
        );
        if status != 0 || info.PebBaseAddress == 0 {
            return None;
        }
        let params = read_usize(process, info.PebBaseAddress + PEB_PROCESS_PARAMETERS)?;
        if params == 0 {
            return None;
        }
        let dos_path = params + PARAMS_CURRENT_DIRECTORY;
        let length = usize::from(read_u16(process, dos_path)?);
        let buffer = read_usize(process, dos_path + 8)?;
        if length == 0 || buffer == 0 {
            return None;
        }
        let mut units = vec![0u16; length / 2];
        let mut read = 0usize;
        let ok = ReadProcessMemory(
            process,
            buffer as *const c_void,
            units.as_mut_ptr().cast(),
            length,
            &mut read,
        );
        if ok == 0 || read != length {
            return None;
        }
        Some(String::from_utf16_lossy(&units))
    }

    unsafe fn read_usize(process: isize, address: usize) -> Option<usize> {
        let mut value = 0usize;
        let mut read = 0usize;
        let ok = ReadProcessMemory(
            process,
            address as *const c_void,
            (&mut value as *mut usize).cast(),
            std::mem::size_of::<usize>(),
            &mut read,
        );
        (ok != 0 && read == std::mem::size_of::<usize>()).then_some(value)
    }

    unsafe fn read_u16(process: isize, address: usize) -> Option<u16> {
        let mut value = 0u16;
        let mut read = 0usize;
        let ok = ReadProcessMemory(
            process,
            address as *const c_void,
            (&mut value as *mut u16).cast(),
            2,
            &mut read,
        );
        (ok != 0 && read == 2).then_some(value)
    }
}

#[cfg(target_os = "linux")]
mod cwd {
    pub(super) fn read(pid: u32) -> Option<String> {
        std::fs::read_link(format!("/proc/{pid}/cwd"))
            .ok()
            .map(|path| path.to_string_lossy().into_owned())
    }
}

#[cfg(not(any(all(windows, target_pointer_width = "64"), target_os = "linux")))]
mod cwd {
    pub(super) fn read(_pid: u32) -> Option<String> {
        None
    }
}

#[cfg(windows)]
mod platform {
    use super::ProcessRow;

    const TH32CS_SNAPPROCESS: u32 = 0x0000_0002;
    const INVALID_HANDLE_VALUE: isize = -1;
    const MAX_PATH: usize = 260;

    /// `PROCESSENTRY32W` (tlhelp32.h), field for field.
    #[repr(C)]
    #[allow(non_snake_case)]
    struct ProcessEntry32W {
        dwSize: u32,
        cntUsage: u32,
        th32ProcessID: u32,
        th32DefaultHeapID: usize,
        th32ModuleID: u32,
        cntThreads: u32,
        th32ParentProcessID: u32,
        pcPriClassBase: i32,
        dwFlags: u32,
        szExeFile: [u16; MAX_PATH],
    }

    #[link(name = "kernel32")]
    extern "system" {
        fn CreateToolhelp32Snapshot(flags: u32, process_id: u32) -> isize;
        fn Process32FirstW(snapshot: isize, entry: *mut ProcessEntry32W) -> i32;
        fn Process32NextW(snapshot: isize, entry: *mut ProcessEntry32W) -> i32;
        fn CloseHandle(handle: isize) -> i32;
    }

    pub(super) fn read_table() -> Option<Vec<ProcessRow>> {
        // SAFETY: plain Win32 calls; the entry is zeroed and carries its own
        // size as the API requires, the handle is closed on every path.
        unsafe {
            let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
            if snapshot == INVALID_HANDLE_VALUE {
                return None;
            }
            let mut entry: ProcessEntry32W = std::mem::zeroed();
            entry.dwSize = std::mem::size_of::<ProcessEntry32W>() as u32;
            let mut table = Vec::with_capacity(256);
            let mut more = Process32FirstW(snapshot, &mut entry);
            while more != 0 {
                let len = entry
                    .szExeFile
                    .iter()
                    .position(|&unit| unit == 0)
                    .unwrap_or(MAX_PATH);
                table.push((
                    entry.th32ProcessID,
                    entry.th32ParentProcessID,
                    String::from_utf16_lossy(&entry.szExeFile[..len]),
                ));
                more = Process32NextW(snapshot, &mut entry);
            }
            CloseHandle(snapshot);
            Some(table)
        }
    }
}

#[cfg(target_os = "linux")]
mod platform {
    use super::ProcessRow;

    /// `/proc/<pid>/stat`: `pid (comm) state ppid …`; `comm` may contain
    /// spaces and parentheses, so split at the LAST `)`.
    pub(super) fn read_table() -> Option<Vec<ProcessRow>> {
        let entries = std::fs::read_dir("/proc").ok()?;
        let mut table = Vec::with_capacity(256);
        for entry in entries.flatten() {
            let Ok(pid) = entry.file_name().to_string_lossy().parse::<u32>() else {
                continue;
            };
            let Ok(stat) = std::fs::read_to_string(entry.path().join("stat")) else {
                continue;
            };
            let Some(open) = stat.find('(') else { continue };
            let Some(close) = stat.rfind(')') else { continue };
            let name = stat[open + 1..close].to_owned();
            let ppid = stat[close + 1..]
                .split_whitespace()
                .nth(1)
                .and_then(|value| value.parse::<u32>().ok())
                .unwrap_or(0);
            table.push((pid, ppid, name));
        }
        Some(table)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn walks_children_and_grandchildren_and_normalizes_names() {
        let table: Vec<ProcessRow> = vec![
            (1, 0, "wininit.exe".into()),
            (100, 1, "cmd.exe".into()),
            (200, 100, "Claude.EXE".into()),
            (300, 200, "node.exe".into()),
            (400, 1, "other.exe".into()),
        ];
        assert_eq!(collect_descendants(100, &table), vec!["claude", "node"]);
        assert!(collect_descendants(300, &table).is_empty(), "leaf has no children");
        assert!(collect_descendants(999, &table).is_empty(), "unknown pid");
    }

    #[test]
    fn pid_reuse_cycles_terminate() {
        let table: Vec<ProcessRow> = vec![
            (100, 200, "cmd.exe".into()),
            (200, 100, "loop.exe".into()),
        ];
        assert_eq!(collect_descendants(100, &table), vec!["loop"]);
    }

    #[test]
    fn cwd_normalization_drops_trailing_separators_but_keeps_roots() {
        assert_eq!(normalize_cwd("C:\\Users\\cesar\\"), "C:\\Users\\cesar");
        assert_eq!(normalize_cwd("C:\\"), "C:\\");
        assert_eq!(normalize_cwd("C:"), "C:\\");
        assert_eq!(normalize_cwd("/home/cesar/"), "/home/cesar");
        assert_eq!(normalize_cwd("/"), "/");
        assert_eq!(normalize_cwd("D:\\dev"), "D:\\dev");
    }

    #[cfg(any(all(windows, target_pointer_width = "64"), target_os = "linux"))]
    #[test]
    fn the_live_cwd_of_this_process_is_its_current_dir() {
        let expected = std::env::current_dir().unwrap();
        let read = process_cwd(std::process::id()).expect("own cwd readable");
        assert_eq!(
            std::path::Path::new(&read).canonicalize().ok(),
            expected.canonicalize().ok(),
            "{read} vs {}",
            expected.display()
        );
        assert!(process_cwd(u32::MAX - 1).is_none(), "a missing pid is None, not a panic");
    }

    #[cfg(any(windows, target_os = "linux"))]
    #[test]
    fn the_live_table_lists_this_process_tree() {
        // The test binary's own pid is in the table; its descendants (none)
        // resolve to an empty list rather than "unknown".
        let names = descendant_names(std::process::id());
        assert!(names.is_some(), "the platform can enumerate processes");
    }
}
