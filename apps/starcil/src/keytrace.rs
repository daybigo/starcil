//! Opt-in keyboard diagnostics. No input is logged unless a path is supplied.
use std::io::Write;
use std::sync::{Mutex, OnceLock};

pub fn record(line: &str) {
    static LOG: OnceLock<Option<Mutex<std::fs::File>>> = OnceLock::new();
    let log = LOG.get_or_init(|| {
        let path = std::env::var_os("STARCIL_KEY_TRACE")?;
        std::fs::OpenOptions::new().create(true).append(true).open(path).ok().map(Mutex::new)
    });
    if let Some(log) = log {
        if let Ok(mut file) = log.lock() {
            let _ = writeln!(file, "{line}");
        }
    }
}

#[cfg(windows)]
pub fn describe(event: &crossterm::event::Event) -> String {
    let chord = match event {
        crossterm::event::Event::Key(key) => starcil_tui::key_event_to_chord(key),
        _ => None,
    };
    let pane = chord.clone().map(starcil_tui::pane_key_chord);
    format!("EVENT {event:?} chord={} pane_chord={}",
        chord.map(|c| c.to_string()).unwrap_or_else(|| "-".into()),
        pane.map(|c| c.to_string()).unwrap_or_else(|| "-".into()))
}
