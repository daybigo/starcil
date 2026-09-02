#[cfg(windows)]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use std::collections::BTreeMap;
    use std::time::Duration;

    use starcil_host::{RealHost, TerminalFrameHost};
    use starcil_server::hosttraits::{TerminalHost, TerminalSpawn};

    let mut host = RealHost::new(Some("powershell.exe".to_owned()), 1024 * 1024);
    let terminal_id = host.spawn(TerminalSpawn {
        cwd: std::env::current_dir()?.to_string_lossy().into_owned(),
        command: Some(vec!["powershell.exe".to_owned(), "-NoLogo".to_owned()]),
        env: BTreeMap::from([
            ("STARCIL_ENV".to_owned(), "1".to_owned()),
            ("STARCIL_SESSION".to_owned(), "live-host".to_owned()),
            ("STARCIL_WORKSPACE_ID".to_owned(), "w1".to_owned()),
            ("STARCIL_TAB_ID".to_owned(), "w1:t1".to_owned()),
            ("STARCIL_PANE_ID".to_owned(), "w1:p1".to_owned()),
        ]),
        rows: 30,
        cols: 100,
    })?;

    wait_for(&host, &terminal_id, Duration::from_secs(10), |screen| {
        screen.contains("PS ") || screen.contains('>')
    })?;
    host.full_frame(&terminal_id)
        .ok_or("initial full frame missing")?;
    host.write_text(&terminal_id, "echo STARCIL_HOST_OK")?;
    host.write_enter(&terminal_id)?;
    wait_for(&host, &terminal_id, Duration::from_secs(10), |screen| {
        screen.contains("STARCIL_HOST_OK")
    })?;

    let dirty = host
        .dirty_frame(&terminal_id)
        .ok_or("echo dirty frame missing")?;
    if !dirty
        .dirty_rows
        .iter()
        .any(|row| row.text.contains("STARCIL_HOST_OK"))
    {
        return Err("echo was absent from dirty rows".into());
    }
    host.resize(&terminal_id, 120, 32)?;
    let resized = host
        .full_frame(&terminal_id)
        .ok_or("resized full frame missing")?;
    if (resized.cols, resized.rows) != (120, 32) {
        return Err("resized frame dimensions do not match".into());
    }
    let process = host.process_info(&terminal_id)?;
    let pid = process["shell_pid"]
        .as_u64()
        .ok_or("shell pid missing")?;

    println!(
        "LIVE_HOST_OK terminal={} pid={} dirty_rows={} frame_seq={} cols={} rows={}",
        terminal_id,
        pid,
        dirty.dirty_rows.len(),
        resized.seq,
        resized.cols,
        resized.rows
    );
    host.kill(&terminal_id)?;
    Ok(())
}

#[cfg(windows)]
fn wait_for(
    host: &starcil_host::RealHost,
    terminal_id: &str,
    timeout: std::time::Duration,
    predicate: impl Fn(&str) -> bool,
) -> Result<(), Box<dyn std::error::Error>> {
    use starcil_server::hosttraits::{ReadFormat, ReadSource, TerminalHost};

    let deadline = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < deadline {
        let screen = host
            .read(
                terminal_id,
                ReadSource::RecentUnwrapped,
                80,
                ReadFormat::Text,
            )?
            .text;
        if predicate(&screen) {
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    Err("timed out waiting for PowerShell screen".into())
}

#[cfg(not(windows))]
fn main() {
    eprintln!("live_host is Windows-only");
}
