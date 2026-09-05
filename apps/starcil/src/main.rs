//! starcil — terminal workspace manager for AI coding agents.
//! Single binary: TUI client (bare), headless server (`starcil server`),
//! automation CLI (`starcil <group> <cmd>`), bridge/helper modes.

mod clientloop;
mod keytrace;
mod servermode;
#[cfg(windows)]
mod wininput;
#[cfg(any(windows, test))]
mod vtinput;

use starcil_cli::{parse, Behavior};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("__probe-keys") {
        if args.iter().any(|a| a == "--help" || a == "-h") {
            println!("starcil __probe-keys [output-file] [--seconds N]\nRecord raw Windows KEY_EVENTs, decoded events and pane chords for 20 seconds using the TUI input mode; restores the console on exit. Example: starcil __probe-keys keys.txt");
            return;
        }
        #[cfg(windows)]
        if let Err(error) = wininput::probe_keys(&args[1..]) {
            eprintln!("starcil key probe: {error}");
            std::process::exit(1);
        }
        #[cfg(not(windows))]
        {
            eprintln!("starcil __probe-keys requires a Windows console");
            std::process::exit(1);
        }
        return;
    }
    // Hidden diagnostic: probe crossterm input under the current terminal,
    // mouse included — shows exactly which events the host terminal forwards
    // (e.g. whether right clicks ever reach the application).
    if args.first().map(String::as_str) == Some("__probe-input") {
        let out = args.get(1).cloned().unwrap_or_else(|| "probe.txt".into());
        let mut log = String::new();
        let _ = crossterm::terminal::enable_raw_mode();
        let mut stdout = std::io::stdout();
        let _ = crossterm::execute!(stdout, crossterm::event::EnableMouseCapture);
        println!("starcil input probe: type and click (all buttons) for 10 seconds…\r");
        let start = std::time::Instant::now();
        #[cfg(windows)]
        {
            // Same reader as the TUI, so the log shows raw console records
            // (including the ones discarded on purpose) and what they became.
            let (tx, rx) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                let mut reader = match wininput::ConsoleReader::new() {
                    Ok(reader) => reader,
                    Err(e) => {
                        let _ = tx.send(format!("ERR console reader: {e}\n"));
                        return;
                    }
                };
                loop {
                    let line = match reader.step() {
                        Ok(wininput::Step::Events(events)) => events
                            .iter()
                            .map(|e| format!("{e:?}\n"))
                            .collect::<String>(),
                        Ok(wininput::Step::Dropped(raw)) => format!("dropped: {raw}\n"),
                        Err(e) => format!("ERR {e}\n"),
                    };
                    if tx.send(line).is_err() {
                        return;
                    }
                }
            });
            while start.elapsed().as_secs() < 10 {
                if let Ok(line) = rx.recv_timeout(std::time::Duration::from_millis(100)) {
                    log.push_str(&line);
                }
            }
        }
        #[cfg(not(windows))]
        while start.elapsed().as_secs() < 10 {
            if crossterm::event::poll(std::time::Duration::from_millis(100)).unwrap_or(false) {
                match crossterm::event::read() {
                    Ok(e) => log.push_str(&format!("{e:?}\n")),
                    Err(e) => log.push_str(&format!("ERR {e}\n")),
                }
            }
        }
        let _ = crossterm::execute!(stdout, crossterm::event::DisableMouseCapture);
        let _ = crossterm::terminal::disable_raw_mode();
        if log.is_empty() {
            log.push_str("NO EVENTS\n");
        }
        let _ = std::fs::write(&out, log);
        println!("starcil input probe: wrote {out}");
        std::process::exit(0);
    }
    // Hidden internal mode: the remote side of `starcil --remote` runs
    // `starcil bridge --stdio [--session <name>]` over the SSH channel.
    if args.first().map(String::as_str) == Some("bridge") {
        let session = args
            .iter()
            .position(|a| a == "--session")
            .and_then(|i| args.get(i + 1))
            .cloned();
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("tokio runtime");
        let code = match runtime.block_on(starcil_remote::bridge_stdio_pump(session.as_deref())) {
            Ok(()) => 0,
            Err(e) => {
                eprintln!("starcil bridge: {e}");
                1
            }
        };
        std::process::exit(code);
    }
    // Intercept the application-level behaviors; everything else (the whole
    // automation CLI, help, completion, schema, config, channel...) is served
    // by starcil-cli's dispatcher.
    if let Ok(invocation) = parse(&args) {
        match &invocation.behavior {
            Behavior::LaunchServer { session } => {
                let code = servermode::run(session.clone());
                std::process::exit(code);
            }
            Behavior::LaunchClient(launch) => {
                let code = servermode::launch_client(launch);
                std::process::exit(code);
            }
            _ => {}
        }
    }
    std::process::exit(starcil_cli::dispatch(&args));
}
