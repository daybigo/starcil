//! Seams the server core depends on. Lane B implements `TerminalHost` over
//! starcil-terminal; tests use the fake in starcil-testkit. The server core
//! never touches a PTY directly.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ReadSource {
    Visible,
    Recent,
    RecentUnwrapped,
    Detection,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReadFormat {
    Text,
    Ansi,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TerminalSpawn {
    pub cwd: String,
    /// None = interactive default shell; Some = argv command.
    pub command: Option<Vec<String>>,
    pub env: BTreeMap<String, String>,
    pub rows: u16,
    pub cols: u16,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TerminalReadout {
    pub text: String,
    pub lines: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScrollInfo {
    pub offset_from_bottom: u64,
    pub max_offset_from_bottom: u64,
    pub viewport_rows: u16,
}

#[derive(Debug, thiserror::Error)]
pub enum HostError {
    #[error("terminal {0} not found")]
    NotFound(String),
    #[error("spawn failed: {0}")]
    SpawnFailed(String),
    #[error("io: {0}")]
    Io(String),
    #[error("invalid key: {0}")]
    InvalidKey(String),
}

/// Everything the dispatcher needs from the terminal layer.
pub trait TerminalHost: Send {
    /// Create a terminal and return its opaque id (`term_<hex>`).
    fn spawn(&mut self, spec: TerminalSpawn) -> Result<String, HostError>;
    fn kill(&mut self, terminal_id: &str) -> Result<(), HostError>;
    fn is_alive(&self, terminal_id: &str) -> bool;
    /// Send literal text (no newline appended).
    fn write_text(&mut self, terminal_id: &str, text: &str) -> Result<(), HostError>;
    /// Send Enter as its own PTY write (never merged with text).
    fn write_enter(&mut self, terminal_id: &str) -> Result<(), HostError>;
    /// Send a validated logical key ("esc", "ctrl+c", "shift+tab", "f5"...).
    /// Implementations validate ALL keys before writing any bytes.
    fn write_keys(&mut self, terminal_id: &str, keys: &[String]) -> Result<(), HostError>;
    /// Paste text honoring the pane's live bracketed-paste mode.
    fn paste_text(&mut self, terminal_id: &str, text: &str) -> Result<(), HostError>;
    fn resize(&mut self, terminal_id: &str, cols: u16, rows: u16) -> Result<(), HostError>;
    fn read(
        &self,
        terminal_id: &str,
        source: ReadSource,
        lines: usize,
        format: ReadFormat,
    ) -> Result<TerminalReadout, HostError>;
    fn scroll_info(&self, terminal_id: &str) -> Option<ScrollInfo>;
    /// Latest OSC 0/2 title, normalized, if any.
    fn terminal_title(&self, terminal_id: &str) -> Option<String>;
    /// Monotonic per-terminal output change counter.
    fn change_seq(&self, terminal_id: &str) -> u64;
    /// Shell pid + foreground process info when the platform exposes it.
    fn process_info(&self, terminal_id: &str) -> Result<serde_json::Value, HostError>;
    /// Names of every process running under the pane's shell (its whole
    /// descendant tree; lowercase, no `.exe`), when the platform can
    /// enumerate them cheaply enough for the agent tick. `None` = the host
    /// cannot tell; `Some(vec![])` = the shell sits idle with nothing running
    /// in it. Callers must never read a blind host as an idle shell.
    fn descendant_process_names(&self, _terminal_id: &str) -> Option<Vec<String>> {
        None
    }

    /// The shell process's current directory, read from the OS on every
    /// agent tick, so `pane.cwd` follows every `cd` the user types. `None`
    /// when the platform cannot tell (the pane keeps the cwd it was spawned
    /// with).
    fn shell_cwd(&self, _terminal_id: &str) -> Option<String> {
        None
    }

    /// Produce a protocol TerminalFrame (as JSON) for streaming clients:
    /// a full snapshot when `snapshot` is true, else only dirty rows since the
    /// last call (None = nothing changed). Hosts without frame support keep
    /// the default; the TUI then falls back to full reads.
    fn take_frame(&mut self, _terminal_id: &str, _snapshot: bool) -> Option<serde_json::Value> {
        None
    }

    /// Move the terminal's scrollback view (positive = up into history).
    /// The next frames carry the scrolled rows; hosts without scrollback
    /// keep the default no-op.
    fn scroll_view(&mut self, _terminal_id: &str, _delta: i32) {}
}
