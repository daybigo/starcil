#[cfg(windows)]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use std::time::Duration;

    use starcil_terminal::{PaneCommand, PaneTerminal, TerminalSize};

    let command = PaneCommand::new("powershell.exe")
        .arg("-NoLogo")
        .starcil_env("STARCIL_ENV", "1")
        .starcil_env("STARCIL_SESSION", "live-pwsh")
        .starcil_env("STARCIL_WORKSPACE_ID", "w1")
        .starcil_env("STARCIL_TAB_ID", "w1:t1")
        .starcil_env("STARCIL_PANE_ID", "w1:p1");
    let terminal = PaneTerminal::spawn(
        command,
        TerminalSize::new(30, 100)?,
        1024 * 1024,
    )?;

    wait_for(&terminal, Duration::from_secs(10), |screen| {
        screen.contains("PS ") || screen.contains('>')
    })?;
    terminal.write_text("echo STARCIL_OK")?;
    terminal.write_enter()?;
    wait_for(&terminal, Duration::from_secs(10), |screen| {
        screen.contains("STARCIL_OK")
    })?;

    let counts = terminal.query_response_counts();
    if counts.cursor_position == 0 {
        return Err("DSR cursor-position responder did not fire".into());
    }
    println!(
        "LIVE_PWSH_OK dsr={} total_queries={} change_seq={}",
        counts.cursor_position,
        counts.total,
        terminal.change_seq()?
    );
    terminal.kill()?;
    Ok(())
}

#[cfg(windows)]
fn wait_for(
    terminal: &starcil_terminal::PaneTerminal,
    timeout: std::time::Duration,
    predicate: impl Fn(&str) -> bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let deadline = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < deadline {
        let screen = terminal
            .read(
                starcil_terminal::ReadSource::RecentUnwrapped,
                Some(40),
                starcil_terminal::ReadFormat::Text,
            )?
            .content;
        if predicate(&screen) {
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    Err("timed out waiting for PowerShell screen".into())
}

#[cfg(not(windows))]
fn main() {
    eprintln!("live_pwsh is Windows-only");
}
