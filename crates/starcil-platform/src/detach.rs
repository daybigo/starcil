use std::process::{Child, Command, Stdio};

use thiserror::Error;

#[derive(Debug, Error)]
pub enum DetachError {
    #[error("failed to spawn detached process: {0}")]
    Spawn(#[from] std::io::Error),
}

pub fn spawn_detached(command: &mut Command) -> Result<Child, DetachError> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;

        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW | DETACHED_PROCESS);
    }

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // A session of its own, with no controlling terminal: closing the
        // terminal that launched the TUI then hangs up nobody, and the server
        // outlives it like a daemon should. `setsid` refuses a process-group
        // leader, so the child must not be moved into a group first.
        // SAFETY: `setsid` is async-signal-safe and touches nothing shared with
        // the parent; the closure runs in the forked child right before exec.
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }

    Ok(command.spawn()?)
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    #[ignore = "libtest cannot prove survival after its own parent process exits; run scripts/live-detach.ps1"]
    fn detached_cmd_can_outlive_its_spawner() {
        let mut command = Command::new("cmd.exe");
        command.args(["/d", "/c", "ping 127.0.0.1 -n 3 >nul"]);
        let child = spawn_detached(&mut command).expect("detached child");
        assert!(child.id() > 0);
    }
}
