//! Production bridge between the server's frozen host seam and real PTYs.

use std::collections::BTreeMap;
#[cfg(windows)]
use std::process::Command;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use starcil_protocol::attach::{
    attr_bits, CursorState, PaneMouseEncoding, PaneMouseMode, PaneMouseTracking, RowPatch, Run,
    StyleDef, TerminalFrame,
};
use starcil_protocol::types::{ForegroundProcess, ProcessInfo, ScrollMetrics};
use starcil_server::hosttraits::{
    HostError, ReadFormat, ReadSource, ScrollInfo, TerminalHost, TerminalReadout,
    TerminalSpawn,
};
use starcil_server::streams::{TerminalOutput, TerminalStreamHost};
use starcil_terminal::{
    PaneCommand, PaneTerminal, ReadFormat as TerminalReadFormat,
    ReadSource as TerminalReadSource, TerminalCellStyle, TerminalColor,
    TerminalCursor, TerminalScreenFrame, TerminalSize,
};

pub const DEFAULT_SCROLLBACK_LIMIT_BYTES: usize = 10 * 1024 * 1024;

struct TerminalEntry {
    pane_id: String,
    terminal: PaneTerminal,
    frame_seq: u64,
    generation: u64,
    last_sent_cursor: Option<TerminalCursor>,
}

mod default_shell;
mod process_tree;

pub use default_shell::{
    resolve_default_shell, resolve_with as resolve_default_shell_with, LoginShell, ShellOs,
};

pub struct RealHost {
    default_shell: Option<String>,
    login_shell: LoginShell,
    scrollback_limit_bytes: usize,
    next_terminal: u64,
    terminals: BTreeMap<String, TerminalEntry>,
}

impl RealHost {
    pub fn new(default_shell: Option<String>, scrollback_limit_bytes: usize) -> Self {
        Self {
            default_shell: default_shell.filter(|shell| !shell.trim().is_empty()),
            login_shell: LoginShell::Auto,
            scrollback_limit_bytes: scrollback_limit_bytes.max(1),
            next_terminal: 0,
            terminals: BTreeMap::new(),
        }
    }

    /// `terminal.shell_mode`: login shells on macOS by default, everywhere
    /// with `Always`, nowhere with `Never`.
    pub fn with_login_shell(mut self, login_shell: LoginShell) -> Self {
        self.login_shell = login_shell;
        self
    }

    pub fn write_bytes(&mut self, terminal_id: &str, bytes: &[u8]) -> Result<(), HostError> {
        self.terminal(terminal_id)?
            .write_bytes(bytes)
            .map_err(host_io_error)
    }

    pub fn terminal_count(&self) -> usize {
        self.terminals.len()
    }

    fn terminal(&self, terminal_id: &str) -> Result<&PaneTerminal, HostError> {
        self.terminals
            .get(terminal_id)
            .map(|entry| &entry.terminal)
            .ok_or_else(|| HostError::NotFound(terminal_id.to_owned()))
    }

    fn allocate_terminal_id(&mut self) -> Result<String, HostError> {
        self.next_terminal = self
            .next_terminal
            .checked_add(1)
            .ok_or_else(|| HostError::SpawnFailed("terminal id space exhausted".to_owned()))?;
        Ok(format!("term_{:016x}", self.next_terminal))
    }

    /// `terminal.default_shell` wins; otherwise the per-OS policy in
    /// [`default_shell`] (PowerShell on Windows, `$SHELL` then the best shell on
    /// disk elsewhere).
    fn default_shell(&self) -> String {
        if let Some(shell) = &self.default_shell {
            return shell.clone();
        }
        default_shell::resolve_default_shell()
    }

    fn capture_frame(&mut self, terminal_id: &str, snapshot: bool) -> Option<FrameData> {
        let entry = self.terminals.get_mut(terminal_id)?;
        let terminal_frame = entry.terminal.take_screen_frame(snapshot).ok()??;
        entry.frame_seq = entry.frame_seq.checked_add(1)?;
        let cursor = cursor_update(&mut entry.last_sent_cursor, terminal_frame.cursor, snapshot);
        Some(build_frame_data(
            terminal_id,
            &entry.pane_id,
            entry.frame_seq,
            entry.generation,
            terminal_frame,
            snapshot,
            cursor,
        ))
    }
}

impl Default for RealHost {
    fn default() -> Self {
        Self::new(None, DEFAULT_SCROLLBACK_LIMIT_BYTES)
    }
}

impl TerminalHost for RealHost {
    fn spawn(&mut self, spec: TerminalSpawn) -> Result<String, HostError> {
        let TerminalSpawn {
            cwd,
            command,
            env,
            rows,
            cols,
        } = spec;
        let terminal_id = self.allocate_terminal_id()?;
        let pane_id = find_environment_value(&env, "STARCIL_PANE_ID")
            .unwrap_or_else(|| terminal_id.clone());

        let (program, args) = match command {
            Some(argv) => {
                let mut argv = argv.into_iter();
                let program = argv.next().ok_or_else(|| {
                    HostError::SpawnFailed("explicit command argv cannot be empty".to_owned())
                })?;
                if program.trim().is_empty() {
                    return Err(HostError::SpawnFailed(
                        "explicit command program cannot be empty".to_owned(),
                    ));
                }
                (program, argv.collect())
            }
            None => {
                let shell = self.default_shell();
                let args =
                    default_shell::startup_args_for(&shell, ShellOs::current(), self.login_shell);
                (shell, args)
            }
        };

        let mut pane_command = PaneCommand::new(program).args(args).cwd(cwd);
        for (key, value) in env {
            if is_managed_environment_key(&key) {
                pane_command = pane_command.starcil_env(key.to_ascii_uppercase(), value);
            } else {
                pane_command = pane_command.env(key, value);
            }
        }

        let size = TerminalSize::new(rows, cols)
            .map_err(|error| HostError::SpawnFailed(error.to_string()))?;
        let terminal = PaneTerminal::spawn(
            pane_command,
            size,
            self.scrollback_limit_bytes,
        )
        .map_err(|error| HostError::SpawnFailed(error.to_string()))?;
        self.terminals.insert(
            terminal_id.clone(),
            TerminalEntry {
                pane_id,
                terminal,
                frame_seq: 0,
                generation: 1,
                last_sent_cursor: None,
            },
        );
        Ok(terminal_id)
    }

    fn kill(&mut self, terminal_id: &str) -> Result<(), HostError> {
        self.terminal(terminal_id)?.kill().map_err(host_io_error)
    }

    fn is_alive(&self, terminal_id: &str) -> bool {
        self.terminal(terminal_id)
            .and_then(|terminal| terminal.is_alive().map_err(host_io_error))
            .unwrap_or(false)
    }

    fn write_text(&mut self, terminal_id: &str, text: &str) -> Result<(), HostError> {
        self.terminal(terminal_id)?
            .write_text(text)
            .map_err(host_io_error)
    }

    fn write_enter(&mut self, terminal_id: &str) -> Result<(), HostError> {
        self.terminal(terminal_id)?
            .write_enter()
            .map_err(host_io_error)
    }

    fn write_keys(&mut self, terminal_id: &str, keys: &[String]) -> Result<(), HostError> {
        let encoded: Result<Vec<Vec<u8>>, HostError> =
            keys.iter().map(|key| encode_key(key)).collect();
        let encoded = encoded?;
        let mut payload = Vec::with_capacity(encoded.iter().map(Vec::len).sum());
        for key in encoded {
            payload.extend_from_slice(&key);
        }
        self.write_bytes(terminal_id, &payload)
    }

    fn paste_text(&mut self, terminal_id: &str, text: &str) -> Result<(), HostError> {
        self.terminal(terminal_id)?
            .paste_text(text)
            .map_err(host_io_error)
    }

    fn resize(&mut self, terminal_id: &str, cols: u16, rows: u16) -> Result<(), HostError> {
        self.terminal(terminal_id)?
            .resize(rows, cols)
            .map(|_| ())
            .map_err(host_io_error)
    }

    fn read(
        &self,
        terminal_id: &str,
        source: ReadSource,
        lines: usize,
        format: ReadFormat,
    ) -> Result<TerminalReadout, HostError> {
        let source = match source {
            ReadSource::Visible => TerminalReadSource::Visible,
            ReadSource::Recent => TerminalReadSource::Recent,
            ReadSource::RecentUnwrapped => TerminalReadSource::RecentUnwrapped,
            ReadSource::Detection => TerminalReadSource::Detection,
        };
        let format = match format {
            ReadFormat::Text => TerminalReadFormat::Text,
            ReadFormat::Ansi => TerminalReadFormat::Ansi,
        };
        let lines = (lines != 0).then_some(lines);
        let read = self
            .terminal(terminal_id)?
            .read(source, lines, format)
            .map_err(host_io_error)?;
        let line_count = read.content.lines().count();
        Ok(TerminalReadout {
            text: read.content,
            lines: line_count,
        })
    }

    fn scroll_info(&self, terminal_id: &str) -> Option<ScrollInfo> {
        let metrics = self.terminal(terminal_id).ok()?.scroll_metrics().ok()?;
        Some(ScrollInfo {
            offset_from_bottom: metrics.offset_from_bottom,
            max_offset_from_bottom: metrics.max_offset_from_bottom,
            viewport_rows: metrics.viewport_rows,
        })
    }

    fn terminal_title(&self, terminal_id: &str) -> Option<String> {
        self.terminal(terminal_id)
            .ok()?
            .terminal_title()
            .ok()?
    }

    fn change_seq(&self, terminal_id: &str) -> u64 {
        self.terminal(terminal_id)
            .and_then(|terminal| terminal.change_seq().map_err(host_io_error))
            .unwrap_or(0)
    }

    fn process_info(&self, terminal_id: &str) -> Result<Value, HostError> {
        let shell_pid = self
            .terminal(terminal_id)?
            .process_id()
            .map_err(host_io_error)?
            .ok_or_else(|| HostError::Io("PTY child did not expose a process id".to_owned()))?;
        let info = ProcessInfo {
            shell_pid,
            foreground_pgid: None,
            foreground: foreground_processes(shell_pid),
        };
        serde_json::to_value(info).map_err(|error| HostError::Io(error.to_string()))
    }

    fn descendant_process_names(&self, terminal_id: &str) -> Option<Vec<String>> {
        let shell_pid = self.terminal(terminal_id).ok()?.process_id().ok()??;
        process_tree::descendant_names(shell_pid)
    }

    fn shell_cwd(&self, terminal_id: &str) -> Option<String> {
        let terminal = self.terminal(terminal_id).ok()?;
        // What the shell itself announced wins: PowerShell's `cd` never
        // moves the process's directory (it keeps its own location), so
        // only its prompt hook knows; cmd, bash and zsh do move it.
        if let Ok(Some(cwd)) = terminal.shell_cwd() {
            return Some(process_tree::normalize_cwd(&cwd));
        }
        let shell_pid = terminal.process_id().ok()??;
        process_tree::process_cwd(shell_pid)
    }

    fn take_frame(&mut self, terminal_id: &str, snapshot: bool) -> Option<Value> {
        let frame = self.capture_frame(terminal_id, snapshot)?;
        serde_json::to_value(frame.into_terminal_frame()).ok()
    }

    fn scroll_view(&mut self, terminal_id: &str, delta: i32) {
        if let Ok(terminal) = self.terminal(terminal_id) {
            let _ = terminal.scroll(delta);
        }
    }
}

impl TerminalStreamHost for RealHost {
    fn stream_size(&self, terminal_id: &str) -> Result<(u16, u16), HostError> {
        let (rows, cols) = self.terminal(terminal_id)?.size().map_err(host_io_error)?;
        Ok((cols, rows))
    }

    fn write_stream_bytes(&mut self, terminal_id: &str, bytes: &[u8]) -> Result<(), HostError> {
        self.write_bytes(terminal_id, bytes)
    }

    fn scroll_stream(&mut self, terminal_id: &str, delta: i32) -> Result<(), HostError> {
        self.terminal(terminal_id)?
            .scroll(delta)
            .map_err(host_io_error)
    }

    fn subscribe_stream_output(
        &self,
        terminal_id: &str,
    ) -> Result<Option<Box<dyn TerminalOutput>>, HostError> {
        self.terminal(terminal_id)?
            .subscribe_output()
            .map(|receiver| Some(Box::new(receiver) as Box<dyn TerminalOutput>))
            .map_err(host_io_error)
    }
}

impl Drop for RealHost {
    fn drop(&mut self) {
        for entry in self.terminals.values() {
            let _ = entry.terminal.kill();
        }
    }
}

pub trait TerminalFrameHost {
    fn dirty_frame(&mut self, terminal_id: &str) -> Option<FrameData>;
    fn full_frame(&mut self, terminal_id: &str) -> Option<FrameData>;
}

impl TerminalFrameHost for RealHost {
    fn dirty_frame(&mut self, terminal_id: &str) -> Option<FrameData> {
        self.capture_frame(terminal_id, false)
    }

    fn full_frame(&mut self, terminal_id: &str) -> Option<FrameData> {
        self.capture_frame(terminal_id, true)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FrameData {
    pub terminal_id: String,
    pub pane_id: String,
    pub seq: u64,
    pub generation: u64,
    pub cols: u16,
    pub rows: u16,
    pub snapshot: bool,
    pub styles: Vec<StyleDef>,
    pub dirty_rows: Vec<FrameRowData>,
    pub cursor: Option<CursorState>,
    pub scroll: Option<ScrollMetrics>,
    pub mouse: Option<PaneMouseMode>,
    pub terminal_change_seq: u64,
}

impl FrameData {
    pub fn into_terminal_frame(self) -> TerminalFrame {
        TerminalFrame {
            pane_id: self.pane_id,
            seq: self.seq,
            generation: self.generation,
            cols: self.cols,
            rows: self.rows,
            snapshot: self.snapshot,
            styles: self.styles,
            patches: self
                .dirty_rows
                .into_iter()
                .map(|row| RowPatch {
                    row: row.row,
                    runs: row.runs,
                })
                .collect(),
            cursor: self.cursor,
            scroll: self.scroll,
            mouse: self.mouse,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FrameRowData {
    pub row: u16,
    pub text: String,
    pub runs: Vec<Run>,
}

fn build_frame_data(
    terminal_id: &str,
    pane_id: &str,
    frame_seq: u64,
    generation: u64,
    terminal_frame: TerminalScreenFrame,
    snapshot: bool,
    cursor: Option<CursorState>,
) -> FrameData {
    let mut style_indexes = BTreeMap::<TerminalCellStyle, u32>::new();
    let mut styles = Vec::new();
    let dirty_rows = terminal_frame
        .row_data
        .into_iter()
        .map(|row| {
            let runs = row
                .runs
                .into_iter()
                .map(|run| {
                    let style = *style_indexes.entry(run.style).or_insert_with(|| {
                        let index = styles.len() as u32;
                        styles.push(pack_style(run.style));
                        index
                    });
                    Run {
                        col: run.col,
                        style,
                        text: run.text,
                    }
                })
                .collect();
            FrameRowData {
                row: row.row,
                text: row.text,
                runs,
            }
        })
        .collect();

    FrameData {
        terminal_id: terminal_id.to_owned(),
        pane_id: pane_id.to_owned(),
        seq: frame_seq,
        generation,
        cols: terminal_frame.cols,
        rows: terminal_frame.rows,
        snapshot,
        styles,
        dirty_rows,
        cursor,
        scroll: Some(ScrollMetrics {
            offset_from_bottom: terminal_frame.scroll.offset_from_bottom,
            max_offset_from_bottom: terminal_frame.scroll.max_offset_from_bottom,
            viewport_rows: terminal_frame.scroll.viewport_rows,
        }),
        mouse: Some(pane_mouse_mode(terminal_frame.mouse)),
        terminal_change_seq: terminal_frame.change_seq,
    }
}

fn pane_mouse_mode(mouse: starcil_terminal::TerminalMouseMode) -> PaneMouseMode {
    let tracking = match mouse.tracking {
        starcil_terminal::TerminalMouseTracking::None => PaneMouseTracking::None,
        starcil_terminal::TerminalMouseTracking::Press => PaneMouseTracking::Press,
        starcil_terminal::TerminalMouseTracking::PressRelease => PaneMouseTracking::PressRelease,
        starcil_terminal::TerminalMouseTracking::ButtonMotion => {
            PaneMouseTracking::ButtonMotion
        }
        starcil_terminal::TerminalMouseTracking::AnyMotion => PaneMouseTracking::AnyMotion,
    };
    let encoding = match mouse.encoding {
        starcil_terminal::TerminalMouseEncoding::Default => PaneMouseEncoding::Default,
        starcil_terminal::TerminalMouseEncoding::Utf8 => PaneMouseEncoding::Utf8,
        starcil_terminal::TerminalMouseEncoding::Sgr => PaneMouseEncoding::Sgr,
    };
    PaneMouseMode {
        alternate_screen: mouse.alternate_screen,
        tracking,
        encoding,
    }
}

fn cursor_update(
    last_sent: &mut Option<TerminalCursor>,
    current: TerminalCursor,
    snapshot: bool,
) -> Option<CursorState> {
    let changed = snapshot || *last_sent != Some(current);
    *last_sent = Some(current);
    changed.then_some(CursorState {
        row: current.row,
        col: current.col,
        visible: current.visible,
    })
}

fn pack_style(style: TerminalCellStyle) -> StyleDef {
    let mut attrs = 0;
    if style.bold {
        attrs |= attr_bits::BOLD;
    }
    if style.dim {
        attrs |= attr_bits::DIM;
    }
    if style.italic {
        attrs |= attr_bits::ITALIC;
    }
    if style.underline {
        attrs |= attr_bits::UNDERLINE;
    }
    if style.inverse {
        attrs |= attr_bits::INVERSE;
    }
    StyleDef {
        fg: pack_color(style.foreground),
        bg: pack_color(style.background),
        attrs,
    }
}

fn pack_color(color: TerminalColor) -> u32 {
    match color {
        TerminalColor::Default => 0,
        TerminalColor::Indexed(index) => 0x0200_0000 | u32::from(index),
        TerminalColor::Rgb { red, green, blue } => {
            0x0100_0000
                | (u32::from(red) << 16)
                | (u32::from(green) << 8)
                | u32::from(blue)
        }
    }
}

fn find_environment_value(env: &BTreeMap<String, String>, expected: &str) -> Option<String> {
    env.iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(expected))
        .map(|(_, value)| value.clone())
}

fn is_managed_environment_key(key: &str) -> bool {
    key.to_ascii_uppercase().starts_with("STARCIL_")
}

fn host_io_error(error: starcil_terminal::TerminalError) -> HostError {
    HostError::Io(error.to_string())
}

fn encode_key(key: &str) -> Result<Vec<u8>, HostError> {
    let normalized = key.to_ascii_lowercase();
    let bytes = match normalized.as_str() {
        "esc" | "escape" => b"\x1b".to_vec(),
        "enter" | "return" => b"\r".to_vec(),
        "tab" => b"\t".to_vec(),
        "shift+tab" => b"\x1b[Z".to_vec(),
        "backspace" => vec![0x7f],
        "space" => b" ".to_vec(),
        "up" => b"\x1b[A".to_vec(),
        "down" => b"\x1b[B".to_vec(),
        "right" => b"\x1b[C".to_vec(),
        "left" => b"\x1b[D".to_vec(),
        "home" => b"\x1b[H".to_vec(),
        "end" => b"\x1b[F".to_vec(),
        "insert" => b"\x1b[2~".to_vec(),
        "delete" => b"\x1b[3~".to_vec(),
        "pageup" | "page_up" => b"\x1b[5~".to_vec(),
        "pagedown" | "page_down" => b"\x1b[6~".to_vec(),
        "f1" => b"\x1bOP".to_vec(),
        "f2" => b"\x1bOQ".to_vec(),
        "f3" => b"\x1bOR".to_vec(),
        "f4" => b"\x1bOS".to_vec(),
        "f5" => b"\x1b[15~".to_vec(),
        "f6" => b"\x1b[17~".to_vec(),
        "f7" => b"\x1b[18~".to_vec(),
        "f8" => b"\x1b[19~".to_vec(),
        "f9" => b"\x1b[20~".to_vec(),
        "f10" => b"\x1b[21~".to_vec(),
        "f11" => b"\x1b[23~".to_vec(),
        "f12" => b"\x1b[24~".to_vec(),
        "ctrl+space" => vec![0],
        _ if normalized.starts_with("ctrl+") => {
            let suffix = &normalized[5..];
            let mut characters = suffix.chars();
            let character = characters
                .next()
                .filter(|_| characters.next().is_none())
                .ok_or_else(|| HostError::InvalidKey(key.to_owned()))?;
            match character {
                'a'..='z' => vec![(character as u8) - b'a' + 1],
                '[' => vec![0x1b],
                '\\' => vec![0x1c],
                ']' => vec![0x1d],
                '^' => vec![0x1e],
                '_' => vec![0x1f],
                _ => return Err(HostError::InvalidKey(key.to_owned())),
            }
        }
        _ if normalized.starts_with("alt+") => {
            let suffix = key.get(4..).unwrap_or_default();
            if suffix.chars().count() != 1 {
                return Err(HostError::InvalidKey(key.to_owned()));
            }
            let mut encoded = vec![0x1b];
            encoded.extend_from_slice(suffix.as_bytes());
            encoded
        }
        _ if key.chars().count() == 1 => key.as_bytes().to_vec(),
        _ => return Err(HostError::InvalidKey(key.to_owned())),
    };
    Ok(bytes)
}

#[cfg(windows)]
fn foreground_processes(shell_pid: u32) -> Vec<ForegroundProcess> {
    let script = format!(
        "$items = @(Get-CimInstance Win32_Process -Filter \"ParentProcessId = {shell_pid}\" -ErrorAction Stop | Select-Object ProcessId,Name,CommandLine); ConvertTo-Json -Compress -InputObject $items"
    );
    let output = match Command::new("powershell.exe")
        .args(["-NoLogo", "-NoProfile", "-NonInteractive", "-Command", &script])
        .output()
    {
        Ok(output) if output.status.success() => output,
        _ => return Vec::new(),
    };
    let value: Value = match serde_json::from_slice(&output.stdout) {
        Ok(value) => value,
        Err(_) => return Vec::new(),
    };
    let values: Vec<&Value> = match &value {
        Value::Array(items) => items.iter().collect(),
        Value::Object(_) => vec![&value],
        _ => Vec::new(),
    };
    values
        .into_iter()
        .filter_map(|item| {
            let pid = item.get("ProcessId")?.as_u64()?.try_into().ok()?;
            let name = item.get("Name")?.as_str()?.to_owned();
            let command_line = item
                .get("CommandLine")
                .and_then(Value::as_str)
                .filter(|line| !line.is_empty())
                .map(|line| vec![line.to_owned()])
                .unwrap_or_default();
            Some(ForegroundProcess {
                pid,
                name,
                argv: command_line,
                cwd: None,
            })
        })
        .collect()
}

#[cfg(not(windows))]
fn foreground_processes(_shell_pid: u32) -> Vec<ForegroundProcess> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(windows)]
    use std::thread;
    #[cfg(windows)]
    use std::time::{Duration, Instant};

    #[test]
    fn key_encoding_covers_required_logical_keys() {
        assert_eq!(encode_key("esc").unwrap(), b"\x1b");
        assert_eq!(encode_key("ctrl+c").unwrap(), &[3]);
        assert_eq!(encode_key("shift+tab").unwrap(), b"\x1b[Z");
        assert_eq!(encode_key("f5").unwrap(), b"\x1b[15~");
        assert!(matches!(
            encode_key("ctrl+not-a-key"),
            Err(HostError::InvalidKey(_))
        ));
    }

    #[test]
    fn each_incremental_frame_owns_valid_style_indexes() {
        let orange_style = TerminalCellStyle {
            foreground: TerminalColor::Rgb {
                red: 215,
                green: 119,
                blue: 87,
            },
            background: TerminalColor::Default,
            bold: false,
            dim: false,
            italic: false,
            underline: false,
            inverse: false,
        };
        let default_style = TerminalCellStyle {
            foreground: TerminalColor::Default,
            ..orange_style
        };
        let cursor = TerminalCursor {
            row: 0,
            col: 6,
            visible: true,
        };

        let orange = build_frame_data(
            "term_fake",
            "w1:p1",
            1,
            1,
            fake_screen_frame(
                vec![starcil_terminal::TerminalFrameRow {
                    row: 0,
                    text: "ORANGE".to_owned(),
                    runs: vec![starcil_terminal::TerminalStyledRun {
                        col: 0,
                        text: "ORANGE".to_owned(),
                        style: orange_style,
                    }],
                }],
                cursor,
            ),
            false,
            Some(CursorState {
                row: cursor.row,
                col: cursor.col,
                visible: cursor.visible,
            }),
        );
        let partial = build_frame_data(
            "term_fake",
            "w1:p1",
            2,
            1,
            fake_screen_frame(
                vec![starcil_terminal::TerminalFrameRow {
                    row: 1,
                    text: "working".to_owned(),
                    runs: vec![starcil_terminal::TerminalStyledRun {
                        col: 0,
                        text: "working".to_owned(),
                        style: default_style,
                    }],
                }],
                cursor,
            ),
            false,
            None,
        );

        assert_eq!(orange.dirty_rows[0].runs[0].style, 0);
        assert_eq!(partial.dirty_rows[0].runs[0].style, 0);
        assert_eq!(orange.styles[0].fg, 0x01d7_7757);
        assert_eq!(partial.styles[0].fg, 0);
        assert_ne!(orange.styles[0], partial.styles[0]);
        assert!(partial.dirty_rows.iter().all(|row| row.row != 0));

        for frame in [&orange, &partial] {
            for row in &frame.dirty_rows {
                for run in &row.runs {
                    let style = frame
                        .styles
                        .get(run.style as usize)
                        .expect("run style index must belong to its own frame");
                    if row.row == 0 && run.text.contains("ORANGE") {
                        assert_eq!(style.fg, 0x01d7_7757);
                    }
                }
            }
        }
    }

    #[test]
    fn cursor_is_sent_only_on_first_frame_move_or_snapshot() {
        let mut last_sent = None;
        let initial = TerminalCursor {
            row: 2,
            col: 4,
            visible: true,
        };

        assert!(cursor_update(&mut last_sent, initial, false).is_some());
        assert!(cursor_update(&mut last_sent, initial, false).is_none());

        let moved = TerminalCursor { col: 5, ..initial };
        assert_eq!(
            cursor_update(&mut last_sent, moved, false),
            Some(CursorState {
                row: 2,
                col: 5,
                visible: true,
            })
        );
        assert!(cursor_update(&mut last_sent, moved, false).is_none());
        assert!(cursor_update(&mut last_sent, moved, true).is_some());
    }

    fn fake_screen_frame(
        row_data: Vec<starcil_terminal::TerminalFrameRow>,
        cursor: TerminalCursor,
    ) -> TerminalScreenFrame {
        TerminalScreenFrame {
            cols: 16,
            rows: 3,
            snapshot: false,
            row_data,
            cursor,
            scroll: starcil_terminal::TerminalScrollMetrics {
                offset_from_bottom: 0,
                max_offset_from_bottom: 0,
                viewport_rows: 3,
            },
            mouse: starcil_terminal::TerminalMouseMode {
                alternate_screen: false,
                tracking: starcil_terminal::TerminalMouseTracking::None,
                encoding: starcil_terminal::TerminalMouseEncoding::Default,
            },
            change_seq: 1,
        }
    }

    #[cfg(windows)]
    #[test]
    fn live_real_host_powershell_echo_dirty_frame_and_resize() {
        let mut host = RealHost::new(Some("powershell.exe".to_owned()), 1024 * 1024);
        let terminal_id = host
            .spawn(TerminalSpawn {
                cwd: std::env::current_dir().unwrap().to_string_lossy().into_owned(),
                command: None,
                env: managed_test_environment(),
                rows: 30,
                cols: 100,
            })
            .expect("spawn real host terminal");

        // A cold Windows PowerShell on a CI runner can take well over 10s to
        // reach its prompt (module autoload, Defender): wait generously.
        wait_for_screen(&host, &terminal_id, Duration::from_secs(45), |screen| {
            screen.contains("PS ") || screen.contains('>')
        });
        let snapshot = host.full_frame(&terminal_id).expect("prompt snapshot");
        assert!(snapshot.snapshot);
        assert_eq!(snapshot.pane_id, "w1:p1");

        host.write_text(&terminal_id, "echo STARCIL_HOST_OK")
            .expect("write text");
        host.write_enter(&terminal_id).expect("write enter");
        wait_for_screen(&host, &terminal_id, Duration::from_secs(30), |screen| {
            screen.contains("STARCIL_HOST_OK")
        });

        let dirty = host.dirty_frame(&terminal_id).expect("echo dirty frame");
        assert!(dirty
            .dirty_rows
            .iter()
            .any(|row| row.text.contains("STARCIL_HOST_OK")));
        assert_eq!(dirty.seq, snapshot.seq + 1);

        host.resize(&terminal_id, 120, 32).expect("resize");
        let resized = host.full_frame(&terminal_id).expect("resized snapshot");
        assert_eq!((resized.cols, resized.rows), (120, 32));
        assert!(host.process_info(&terminal_id).unwrap()["shell_pid"]
            .as_u64()
            .is_some_and(|pid| pid > 0));
        host.kill(&terminal_id).expect("kill");
        assert!(!host.is_alive(&terminal_id));
    }

    #[cfg(windows)]
    fn managed_test_environment() -> BTreeMap<String, String> {
        BTreeMap::from([
            ("STARCIL_ENV".to_owned(), "1".to_owned()),
            ("STARCIL_SESSION".to_owned(), "host-live".to_owned()),
            ("STARCIL_WORKSPACE_ID".to_owned(), "w1".to_owned()),
            ("STARCIL_TAB_ID".to_owned(), "w1:t1".to_owned()),
            ("STARCIL_PANE_ID".to_owned(), "w1:p1".to_owned()),
        ])
    }

    #[cfg(windows)]
    fn wait_for_screen(
        host: &RealHost,
        terminal_id: &str,
        timeout: Duration,
        predicate: impl Fn(&str) -> bool,
    ) {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            let screen = host
                .read(
                    terminal_id,
                    ReadSource::RecentUnwrapped,
                    80,
                    ReadFormat::Text,
                )
                .expect("read screen")
                .text;
            if predicate(&screen) {
                return;
            }
            thread::sleep(Duration::from_millis(25));
        }
        panic!("timed out waiting for screen");
    }
}
