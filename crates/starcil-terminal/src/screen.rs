use std::collections::BTreeSet;
use std::time::Instant;

use serde::{Deserialize, Serialize};

use crate::interceptor::{EscapeInterceptor, InterceptEvent, QueryKind};

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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalRead {
    pub source: ReadSource,
    pub format: ReadFormat,
    pub content: String,
    pub change_seq: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TerminalColor {
    Default,
    Indexed(u8),
    Rgb { red: u8, green: u8, blue: u8 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct TerminalCellStyle {
    pub foreground: TerminalColor,
    pub background: TerminalColor,
    pub bold: bool,
    pub dim: bool,
    pub italic: bool,
    pub underline: bool,
    pub inverse: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalStyledRun {
    pub col: u16,
    pub text: String,
    pub style: TerminalCellStyle,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalFrameRow {
    pub row: u16,
    pub text: String,
    pub runs: Vec<TerminalStyledRun>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalCursor {
    pub row: u16,
    pub col: u16,
    pub visible: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalScrollMetrics {
    pub offset_from_bottom: u64,
    pub max_offset_from_bottom: u64,
    pub viewport_rows: u16,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalScreenFrame {
    pub cols: u16,
    pub rows: u16,
    pub snapshot: bool,
    pub row_data: Vec<TerminalFrameRow>,
    pub cursor: TerminalCursor,
    pub scroll: TerminalScrollMetrics,
    pub mouse: TerminalMouseMode,
    pub change_seq: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalMouseMode {
    pub alternate_screen: bool,
    pub tracking: TerminalMouseTracking,
    pub encoding: TerminalMouseEncoding,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TerminalMouseTracking {
    None,
    Press,
    PressRelease,
    ButtonMotion,
    AnyMotion,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TerminalMouseEncoding {
    Default,
    Utf8,
    Sgr,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CapturedRow {
    text: String,
    ansi: String,
    soft_wrapped: bool,
}

pub(crate) struct ScreenState {
    parser: vt100::Parser,
    interceptor: EscapeInterceptor,
    visible_rows: Vec<String>,
    visible_ansi: String,
    visible_styles: Vec<Vec<TerminalCellStyle>>,
    dirty_rows: BTreeSet<u16>,
    change_seq: u64,
    last_change: Instant,
    bracketed_paste: bool,
    terminal_title: Option<String>,
    /// Working directory the shell last announced (OSC 9;9 / OSC 7).
    shell_cwd: Option<String>,
    mouse_mode: TerminalMouseMode,
    mouse_mode_dirty: bool,
    /// Last cursor seen by `refresh_cache`; a cursor move with no cell change
    /// (a typed trailing space, arrow keys in a line editor) must still reach
    /// the client, so it gets its own frame.
    cursor: TerminalCursor,
    cursor_dirty: bool,
}

impl ScreenState {
    pub(crate) fn new(rows: u16, cols: u16, scrollback_limit_bytes: usize) -> Self {
        let scrollback_rows = scrollback_rows_from_budget(scrollback_limit_bytes, cols);
        let parser = vt100::Parser::new(rows, cols, scrollback_rows);
        let visible_rows = visible_row_texts(parser.screen(), rows, cols);
        let visible_ansi =
            String::from_utf8_lossy(&parser.screen().contents_formatted()).into_owned();
        let visible_styles = capture_visible_styles(parser.screen(), rows, cols);
        let mouse_mode = capture_mouse_mode(parser.screen());
        let cursor = capture_cursor(parser.screen());
        let dirty_rows = (0..rows).collect();
        Self {
            parser,
            interceptor: EscapeInterceptor::default(),
            visible_rows,
            visible_ansi,
            visible_styles,
            dirty_rows,
            change_seq: 0,
            last_change: Instant::now(),
            bracketed_paste: false,
            terminal_title: None,
            shell_cwd: None,
            mouse_mode,
            mouse_mode_dirty: false,
            cursor,
            cursor_dirty: false,
        }
    }

    #[cfg(test)]
    pub(crate) fn process<F>(&mut self, bytes: &[u8], mut respond: F)
    where
        F: FnMut(QueryKind, Vec<u8>),
    {
        self.process_chunks(std::iter::once(bytes), &mut respond);
    }

    pub(crate) fn process_chunks<'a, I, F>(&mut self, chunks: I, mut respond: F)
    where
        I: IntoIterator<Item = &'a [u8]>,
        F: FnMut(QueryKind, Vec<u8>),
    {
        for bytes in chunks {
            let events = self.interceptor.scan(bytes);
            let mut start = 0;
            for (end, event) in events {
                self.parser.process(&bytes[start..end]);
                self.handle_event(event, &mut respond);
                start = end;
            }
            self.parser.process(&bytes[start..]);
        }
        self.refresh_cache();
    }

    pub(crate) fn resize(&mut self, rows: u16, cols: u16) {
        self.parser.screen_mut().set_size(rows, cols);
        let previous_seq = self.change_seq;
        self.refresh_cache();
        self.dirty_rows.extend(0..rows);
        if self.change_seq == previous_seq {
            self.change_seq = self.change_seq.saturating_add(1);
            self.last_change = Instant::now();
        }
    }

    pub(crate) fn size(&self) -> (u16, u16) {
        self.parser.screen().size()
    }

    pub(crate) fn scroll(&mut self, delta: i32) {
        if delta == 0 {
            return;
        }
        let screen = self.parser.screen_mut();
        let current = screen.scrollback();
        screen.set_scrollback(usize::MAX);
        let maximum = screen.scrollback();
        let requested = if delta.is_positive() {
            current.saturating_add(delta as usize)
        } else {
            current.saturating_sub(delta.unsigned_abs() as usize)
        };
        screen.set_scrollback(requested.min(maximum));
        let previous_seq = self.change_seq;
        self.refresh_cache();
        let (rows, _) = self.parser.screen().size();
        self.dirty_rows.extend(0..rows);
        if self.change_seq == previous_seq {
            self.change_seq = self.change_seq.saturating_add(1);
            self.last_change = Instant::now();
        }
    }

    fn handle_event<F>(&mut self, event: InterceptEvent, respond: &mut F)
    where
        F: FnMut(QueryKind, Vec<u8>),
    {
        match event {
            InterceptEvent::BracketedPaste(enabled) => self.bracketed_paste = enabled,
            InterceptEvent::Title(title) => {
                if self.terminal_title != title {
                    self.terminal_title = title;
                    self.change_seq = self.change_seq.saturating_add(1);
                    self.last_change = Instant::now();
                }
            }
            // Not a screen change: the agent tick polls it on its own.
            InterceptEvent::Cwd(cwd) => self.shell_cwd = Some(cwd),
            InterceptEvent::Query(kind) => {
                let response = match kind {
                    QueryKind::CursorPosition => {
                        let (row, col) = self.parser.screen().cursor_position();
                        format!("\x1b[{};{}R", row + 1, col + 1).into_bytes()
                    }
                    QueryKind::PrimaryDeviceAttributes => b"\x1b[?1;2c".to_vec(),
                    QueryKind::SecondaryDeviceAttributes => b"\x1b[>0;100;0c".to_vec(),
                    QueryKind::DeviceStatus => b"\x1b[0n".to_vec(),
                };
                respond(kind, response);
            }
        }
    }

    fn refresh_cache(&mut self) {
        let (rows, cols) = self.parser.screen().size();
        // Row texts MUST come from the per-row iterator: `contents()` joins
        // soft-wrapped rows without a newline and trims trailing blanks, so
        // splitting it shifts every index below a wrapped row and the dirty
        // comparison then diffs the wrong pairs (missed keystroke echoes,
        // spurious blank rows — the invisible-composer bug).
        let next_rows = visible_row_texts(self.parser.screen(), rows, cols);
        let next_ansi =
            String::from_utf8_lossy(&self.parser.screen().contents_formatted()).into_owned();
        let next_styles = capture_visible_styles(self.parser.screen(), rows, cols);
        let next_mouse_mode = capture_mouse_mode(self.parser.screen());
        let next_cursor = capture_cursor(self.parser.screen());

        let mut changed_rows = BTreeSet::new();
        for index in 0..usize::from(rows) {
            if self.visible_rows.get(index) != next_rows.get(index)
                || self.visible_styles.get(index) != next_styles.get(index)
            {
                changed_rows.insert(index as u16);
            }
        }

        let mouse_mode_changed = self.mouse_mode != next_mouse_mode;
        let cursor_changed = self.cursor != next_cursor;
        if !changed_rows.is_empty() || mouse_mode_changed || cursor_changed {
            self.change_seq = self.change_seq.saturating_add(1);
            self.last_change = Instant::now();
        }
        self.dirty_rows.extend(changed_rows);
        if mouse_mode_changed {
            self.mouse_mode = next_mouse_mode;
            self.mouse_mode_dirty = true;
        }
        if cursor_changed {
            self.cursor = next_cursor;
            self.cursor_dirty = true;
        }
        self.visible_rows = next_rows;
        self.visible_ansi = next_ansi;
        self.visible_styles = next_styles;
    }

    pub(crate) fn read(
        &mut self,
        source: ReadSource,
        lines: Option<usize>,
        format: ReadFormat,
    ) -> TerminalRead {
        let default_lines = match source {
            ReadSource::Detection => 16,
            _ => usize::MAX,
        };
        let line_limit = lines.unwrap_or(default_lines);
        let captured = match source {
            ReadSource::Visible => self.capture_recent_rows(usize::from(self.parser.screen().size().0)),
            ReadSource::Recent | ReadSource::RecentUnwrapped => {
                self.capture_recent_rows(line_limit)
            }
            ReadSource::Detection => Vec::new(),
        };
        let text = match source {
            ReadSource::Visible => take_last_captured(&captured, line_limit, false, false),
            ReadSource::Recent => {
                take_last_captured(&captured, line_limit, false, false)
            }
            ReadSource::RecentUnwrapped => {
                take_last_captured(&captured, line_limit, true, false)
            }
            ReadSource::Detection => {
                take_last_text_lines(&self.parser.screen().contents(), line_limit.min(16))
            }
        };

        let content = match (source, format, lines) {
            (ReadSource::Visible, ReadFormat::Ansi, None) => self.visible_ansi.clone(),
            (ReadSource::Detection, ReadFormat::Ansi, _) => {
                format!("\x1b[0m{text}\x1b[0m")
            }
            (_, ReadFormat::Ansi, _) => {
                take_last_captured(&captured, line_limit, source == ReadSource::RecentUnwrapped, true)
            }
            (_, ReadFormat::Text, _) => text,
        };
        TerminalRead {
            source,
            format,
            content,
            change_seq: self.change_seq,
        }
    }

    fn capture_recent_rows(&mut self, limit: usize) -> Vec<CapturedRow> {
        let screen = self.parser.screen_mut();
        let (visible_rows, cols) = screen.size();
        let visible_rows = usize::from(visible_rows);
        let original_scrollback = screen.scrollback();
        screen.set_scrollback(usize::MAX);
        let available_scrollback = screen.scrollback();
        screen.set_scrollback(0);

        let history_needed = available_scrollback.min(limit.saturating_sub(visible_rows));
        let mut rows = Vec::with_capacity(visible_rows.saturating_add(history_needed));
        let mut remaining = history_needed;
        while remaining > 0 {
            screen.set_scrollback(remaining);
            let take = remaining.min(visible_rows);
            rows.extend(capture_visible_rows(screen, cols).into_iter().take(take));
            remaining -= take;
        }

        screen.set_scrollback(0);
        rows.extend(capture_visible_rows(screen, cols));
        screen.set_scrollback(original_scrollback);
        let start = rows.len().saturating_sub(limit);
        rows.drain(..start);
        while rows.last().is_some_and(|row| row.text.is_empty()) {
            rows.pop();
        }
        rows
    }

    pub(crate) fn take_dirty_rows(&mut self) -> Vec<u16> {
        let rows = self.dirty_rows.iter().copied().collect();
        self.dirty_rows.clear();
        rows
    }

    pub(crate) fn change_seq(&self) -> u64 {
        self.change_seq
    }

    pub(crate) fn last_change(&self) -> Instant {
        self.last_change
    }

    pub(crate) fn bracketed_paste(&self) -> bool {
        self.bracketed_paste
    }

    pub(crate) fn terminal_title(&self) -> Option<String> {
        self.terminal_title.clone()
    }

    pub(crate) fn shell_cwd(&self) -> Option<String> {
        self.shell_cwd.clone()
    }

    pub(crate) fn scroll_metrics(&mut self) -> TerminalScrollMetrics {
        capture_scroll_metrics(self.parser.screen_mut())
    }

    pub(crate) fn take_frame(&mut self, snapshot: bool) -> Option<TerminalScreenFrame> {
        let (rows, cols) = self.parser.screen().size();
        let mouse_mode_dirty = std::mem::take(&mut self.mouse_mode_dirty);
        let cursor_dirty = std::mem::take(&mut self.cursor_dirty);
        let selected_rows: Vec<u16> = if snapshot {
            self.dirty_rows.clear();
            (0..rows).collect()
        } else {
            let rows = self.take_dirty_rows();
            if rows.is_empty() && !mouse_mode_dirty && !cursor_dirty {
                return None;
            }
            rows
        };

        let screen = self.parser.screen();
        let row_data = selected_rows
            .into_iter()
            .filter(|row| *row < rows)
            .map(|row| capture_styled_row(screen, row, cols))
            .collect();
        let cursor = capture_cursor(screen);
        let scroll = capture_scroll_metrics(self.parser.screen_mut());

        Some(TerminalScreenFrame {
            cols,
            rows,
            snapshot,
            row_data,
            cursor,
            scroll,
            mouse: self.mouse_mode,
            change_seq: self.change_seq,
        })
    }
}

fn capture_cursor(screen: &vt100::Screen) -> TerminalCursor {
    let (row, col) = screen.cursor_position();
    TerminalCursor {
        row,
        col,
        visible: !screen.hide_cursor(),
    }
}

fn capture_mouse_mode(screen: &vt100::Screen) -> TerminalMouseMode {
    let tracking = match screen.mouse_protocol_mode() {
        vt100::MouseProtocolMode::None => TerminalMouseTracking::None,
        vt100::MouseProtocolMode::Press => TerminalMouseTracking::Press,
        vt100::MouseProtocolMode::PressRelease => TerminalMouseTracking::PressRelease,
        vt100::MouseProtocolMode::ButtonMotion => TerminalMouseTracking::ButtonMotion,
        vt100::MouseProtocolMode::AnyMotion => TerminalMouseTracking::AnyMotion,
    };
    let encoding = match screen.mouse_protocol_encoding() {
        vt100::MouseProtocolEncoding::Default => TerminalMouseEncoding::Default,
        vt100::MouseProtocolEncoding::Utf8 => TerminalMouseEncoding::Utf8,
        vt100::MouseProtocolEncoding::Sgr => TerminalMouseEncoding::Sgr,
    };
    TerminalMouseMode {
        alternate_screen: screen.alternate_screen(),
        tracking,
        encoding,
    }
}

fn capture_scroll_metrics(screen: &mut vt100::Screen) -> TerminalScrollMetrics {
    let (viewport_rows, _) = screen.size();
    let offset = screen.scrollback();
    screen.set_scrollback(usize::MAX);
    let maximum = screen.scrollback();
    screen.set_scrollback(offset);
    TerminalScrollMetrics {
        offset_from_bottom: offset as u64,
        max_offset_from_bottom: maximum as u64,
        viewport_rows,
    }
}

fn capture_visible_styles(
    screen: &vt100::Screen,
    rows: u16,
    cols: u16,
) -> Vec<Vec<TerminalCellStyle>> {
    (0..rows)
        .map(|row| {
            (0..cols)
                .filter_map(|col| screen.cell(row, col).map(cell_style))
                .collect()
        })
        .collect()
}

fn capture_styled_row(screen: &vt100::Screen, row: u16, cols: u16) -> TerminalFrameRow {
    let text = screen
        .rows(0, cols)
        .nth(usize::from(row))
        .unwrap_or_default();
    let mut runs = Vec::new();
    let mut current: Option<(u16, u16, String, TerminalCellStyle)> = None;

    for col in 0..cols {
        let Some(cell) = screen.cell(row, col) else {
            continue;
        };
        if cell.is_wide_continuation() {
            continue;
        }
        let style = cell_style(cell);
        let contents = if cell.has_contents() {
            cell.contents()
        } else {
            " "
        };
        let width = if cell.is_wide() { 2 } else { 1 };

        match &mut current {
            Some((_, next_col, run_text, run_style))
                if *next_col == col && *run_style == style =>
            {
                run_text.push_str(contents);
                *next_col = next_col.saturating_add(width);
            }
            Some(_) => {
                let (start, _, run_text, run_style) = current.take().expect("run exists");
                runs.push(TerminalStyledRun {
                    col: start,
                    text: run_text,
                    style: run_style,
                });
                current = Some((col, col.saturating_add(width), contents.to_owned(), style));
            }
            None => {
                current = Some((col, col.saturating_add(width), contents.to_owned(), style));
            }
        }
    }

    if let Some((start, _, run_text, run_style)) = current {
        runs.push(TerminalStyledRun {
            col: start,
            text: run_text,
            style: run_style,
        });
    }

    TerminalFrameRow { row, text, runs }
}

fn cell_style(cell: &vt100::Cell) -> TerminalCellStyle {
    TerminalCellStyle {
        foreground: terminal_color(cell.fgcolor()),
        background: terminal_color(cell.bgcolor()),
        bold: cell.bold(),
        dim: cell.dim(),
        italic: cell.italic(),
        underline: cell.underline(),
        inverse: cell.inverse(),
    }
}

fn terminal_color(color: vt100::Color) -> TerminalColor {
    match color {
        vt100::Color::Default => TerminalColor::Default,
        vt100::Color::Idx(index) => TerminalColor::Indexed(index),
        vt100::Color::Rgb(red, green, blue) => TerminalColor::Rgb { red, green, blue },
    }
}

fn scrollback_rows_from_budget(byte_budget: usize, cols: u16) -> usize {
    let estimated_row_bytes = usize::from(cols).max(1).saturating_mul(4);
    (byte_budget / estimated_row_bytes).max(1)
}

/// Per-row visible texts with TRUE row indices (one entry per grid row).
/// Trailing blanks are kept on purpose: vt100 already stops at the last cell
/// with contents, so a typed space at the end of a line is a real change and
/// trimming it hid the keystroke (the row never went dirty).
fn visible_row_texts(screen: &vt100::Screen, rows: u16, cols: u16) -> Vec<String> {
    let mut result: Vec<String> = screen.rows(0, cols).collect();
    result.resize(usize::from(rows), String::new());
    result.truncate(usize::from(rows));
    result
}

fn capture_visible_rows(screen: &vt100::Screen, cols: u16) -> Vec<CapturedRow> {
    let plain: Vec<_> = screen.rows(0, cols).collect();
    let formatted: Vec<_> = screen.rows_formatted(0, cols).collect();
    plain
        .into_iter()
        .zip(formatted)
        .map(|(text, ansi)| CapturedRow {
            soft_wrapped: text.chars().count() >= usize::from(cols),
            text,
            ansi: String::from_utf8_lossy(&ansi).into_owned(),
        })
        .collect()
}

fn take_last_captured(
    rows: &[CapturedRow],
    limit: usize,
    unwrap: bool,
    ansi: bool,
) -> String {
    let start = rows.len().saturating_sub(limit);
    let rows = &rows[start..];
    let mut result = String::new();
    for (index, row) in rows.iter().enumerate() {
        result.push_str(if ansi { &row.ansi } else { &row.text });
        if index + 1 < rows.len() && !(unwrap && row.soft_wrapped) {
            result.push('\n');
        }
    }
    result.trim_matches('\n').to_owned()
}

fn take_last_text_lines(text: &str, limit: usize) -> String {
    let lines: Vec<_> = text.lines().collect();
    let start = lines.len().saturating_sub(limit);
    lines[start..].join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_updates_dirty_rows_and_sequence() {
        let mut state = ScreenState::new(4, 20, 4096);
        state.take_dirty_rows();
        state.process(b"hello", |_, _| {});
        assert_eq!(state.change_seq(), 1);
        assert_eq!(state.take_dirty_rows(), vec![0]);
        assert!(state
            .read(ReadSource::Visible, None, ReadFormat::Text)
            .content
            .contains("hello"));
    }

    #[test]
    fn keystroke_echo_below_a_soft_wrapped_row_is_dirty_and_emitted() {
        // 20 cols: a 25-char line soft-wraps across rows 0-1. vt100's
        // contents() joins wrapped rows without a newline, which used to
        // shift every cached row index below the wrap and lose the echo.
        let mut state = ScreenState::new(6, 20, 4096);
        state.process(b"AAAAAAAAAAAAAAAAAAAAAAAAA", |_, _| {});
        state.process(b"\x1b[4;1H> ", |_, _| {});
        state.take_frame(true).expect("baseline snapshot");

        state.process(b"x", |_, _| {});
        let frame = state.take_frame(false).expect("echo frame");
        let echo_row = frame
            .row_data
            .iter()
            .find(|row| row.row == 3)
            .expect("the prompt row below the wrap must be dirty");
        assert!(
            echo_row.text.contains("> x"),
            "echo text missing: {:?}",
            echo_row.text
        );

        // A second keystroke stays visible too (the cache must track true
        // row indices, not contents() line positions).
        state.process(b"y", |_, _| {});
        let frame = state.take_frame(false).expect("second echo frame");
        let echo_row = frame
            .row_data
            .iter()
            .find(|row| row.row == 3)
            .expect("second echo dirty");
        assert!(echo_row.text.contains("> xy"));
    }

    #[test]
    fn cursor_query_uses_current_screen_position() {
        let mut state = ScreenState::new(4, 20, 4096);
        let mut responses = Vec::new();
        state.process(b"abc\x1b[6n", |kind, response| {
            responses.push((kind, response))
        });
        assert_eq!(
            responses,
            vec![(QueryKind::CursorPosition, b"\x1b[1;4R".to_vec())]
        );
    }

    #[test]
    fn bracketed_paste_mode_is_intercepted() {
        let mut state = ScreenState::new(4, 20, 4096);
        state.process(b"\x1b[?2004h", |_, _| {});
        assert!(state.bracketed_paste());
        state.process(b"\x1b[?2004l", |_, _| {});
        assert!(!state.bracketed_paste());
    }

    #[test]
    fn trailing_space_and_cursor_only_moves_emit_frames() {
        let mut state = ScreenState::new(4, 20, 4096);
        state.process(b"hello", |_, _| {});
        state.take_frame(true).expect("initial frame");
        assert!(state.take_frame(false).is_none(), "nothing pending");

        // A typed space at the end of the line: the row content changes and
        // the cursor advances, both must reach the client now, not with the
        // next letter.
        state.process(b" ", |_, _| {});
        let frame = state.take_frame(false).expect("space frame");
        assert_eq!(frame.cursor.col, 6);
        assert_eq!(frame.row_data.len(), 1);
        assert_eq!(frame.row_data[0].text, "hello ");

        // Cursor movement without any cell change (arrow keys in a line
        // editor) gets a cursor-only frame.
        state.process(b"\x1b[D", |_, _| {});
        let frame = state.take_frame(false).expect("cursor-only frame");
        assert!(frame.row_data.is_empty());
        assert_eq!(frame.cursor.col, 5);
        assert!(state.take_frame(false).is_none(), "cursor dirty flag is consumed");
    }

    #[test]
    fn mouse_mode_changes_emit_frames_without_cell_changes() {
        let mut state = ScreenState::new(4, 20, 4096);
        state.take_frame(true).expect("initial frame");

        state.process(b"\x1b[?1049h\x1b[?1000h\x1b[?1006h", |_, _| {});
        let enabled = state.take_frame(false).expect("mouse mode frame");
        assert!(enabled.row_data.is_empty());
        assert_eq!(
            enabled.mouse,
            TerminalMouseMode {
                alternate_screen: true,
                tracking: TerminalMouseTracking::PressRelease,
                encoding: TerminalMouseEncoding::Sgr,
            }
        );

        state.process(b"\x1b[?1000l", |_, _| {});
        let disabled = state.take_frame(false).expect("mouse mode disable frame");
        assert!(disabled.row_data.is_empty());
        assert_eq!(disabled.mouse.tracking, TerminalMouseTracking::None);
    }

    #[test]
    fn ansi_reads_preserve_sgr_formatting() {
        let mut state = ScreenState::new(4, 20, 4096);
        state.process(b"\x1b[31mred\x1b[0m", |_, _| {});
        let ansi = state
            .read(ReadSource::Recent, Some(4), ReadFormat::Ansi)
            .content;
        assert!(ansi.contains("\x1b[31m"));
        assert!(ansi.contains("red"));
    }

    #[test]
    fn detection_reads_bottom_meaningful_rows_not_trailing_blanks() {
        let mut state = ScreenState::new(30, 80, 4096);
        state.process(b"agent blocked", |_, _| {});
        assert_eq!(
            state
                .read(ReadSource::Detection, None, ReadFormat::Text)
                .content,
            "agent blocked"
        );
    }

    #[test]
    fn recent_unwrapped_joins_full_width_rows() {
        let rows = vec![
            CapturedRow {
                text: "1234".to_owned(),
                ansi: "1234".to_owned(),
                soft_wrapped: true,
            },
            CapturedRow {
                text: "next".to_owned(),
                ansi: "next".to_owned(),
                soft_wrapped: false,
            },
        ];
        assert_eq!(take_last_captured(&rows, 2, false, false), "1234\nnext");
        assert_eq!(take_last_captured(&rows, 2, true, false), "1234next");
    }

    #[test]
    fn title_and_styled_snapshot_are_exposed_atomically() {
        let mut state = ScreenState::new(3, 12, 4096);
        state.take_dirty_rows();
        state.process(b"\x1b]2; Build pane \x07\x1b[31;1mred\x1b[0m", |_, _| {});

        assert_eq!(state.terminal_title().as_deref(), Some("Build pane"));
        let frame = state.take_frame(false).expect("dirty frame");
        assert!(!frame.snapshot);
        assert_eq!((frame.rows, frame.cols), (3, 12));
        assert_eq!(frame.row_data.len(), 1);
        assert!(frame.row_data[0].text.contains("red"));
        assert!(frame.row_data[0].runs.iter().any(|run| {
            run.text.contains("red")
                && run.style.bold
                && run.style.foreground == TerminalColor::Indexed(1)
        }));
        assert!(state.take_frame(false).is_none());

        let snapshot = state.take_frame(true).expect("full snapshot");
        assert!(snapshot.snapshot);
        assert_eq!(snapshot.row_data.len(), 3);
    }

    #[test]
    fn partial_update_does_not_reemit_unchanged_truecolor_row() {
        let mut state = ScreenState::new(3, 16, 4096);
        state.take_frame(true).expect("initial snapshot");
        state.process(
            b"\x1b[1;1H\x1b[38;2;215;119;87mORANGE\x1b[0m",
            |_, _| {},
        );

        let orange = TerminalColor::Rgb {
            red: 215,
            green: 119,
            blue: 87,
        };
        let painted = state.take_frame(false).expect("orange frame");
        assert_eq!(
            painted
                .row_data
                .iter()
                .filter(|row| row.row == 0)
                .flat_map(|row| &row.runs)
                .find(|run| run.text.contains("ORANGE"))
                .map(|run| run.style.foreground),
            Some(orange)
        );

        state.process(b"\x1b[2;1Hworking", |_, _| {});
        let partial = state.take_frame(false).expect("other-row frame");
        assert_eq!(
            partial
                .row_data
                .iter()
                .map(|row| row.row)
                .collect::<Vec<_>>(),
            vec![1]
        );

        let snapshot = state.take_frame(true).expect("final snapshot");
        assert_eq!(
            snapshot
                .row_data
                .iter()
                .filter(|row| row.row == 0)
                .flat_map(|row| &row.runs)
                .find(|run| run.text.contains("ORANGE"))
                .map(|run| run.style.foreground),
            Some(orange)
        );
    }

    #[test]
    fn style_only_row_change_is_dirty_alongside_text_change() {
        let mut state = ScreenState::new(3, 16, 4096);
        state.process(b"\x1b[1;1Hlabel\x1b[2;1Hold", |_, _| {});
        state.take_frame(true).expect("baseline snapshot");

        state.process(
            b"\x1b[1;1H\x1b[38;2;215;119;87mlabel\x1b[0m\x1b[2;1Hnew",
            |_, _| {},
        );
        let frame = state.take_frame(false).expect("combined update");

        assert_eq!(
            frame
                .row_data
                .iter()
                .map(|row| row.row)
                .collect::<Vec<_>>(),
            vec![0, 1]
        );
        assert!(frame.row_data[0].runs.iter().any(|run| {
            run.text.contains("label")
                && run.style.foreground
                    == TerminalColor::Rgb {
                        red: 215,
                        green: 119,
                        blue: 87,
                    }
        }));
    }

    #[test]
    fn clear_and_repaint_chunks_are_committed_as_one_screen_change() {
        let mut state = ScreenState::new(2, 16, 4096);
        state.process(
            b"\x1b[1;1H\x1b[38;2;215;119;87mORANGE\x1b[0m",
            |_, _| {},
        );
        state.take_frame(true).expect("painted snapshot");

        let chunks: [&[u8]; 2] = [
            b"\x1b[1;1H\x1b[2K",
            b"\x1b[1;1H\x1b[38;2;215;119;87mORANGE\x1b[0m",
        ];
        state.process_chunks(chunks, |_, _| {});

        assert!(state.take_frame(false).is_none());
        let snapshot = state.take_frame(true).expect("settled snapshot");
        assert!(snapshot.row_data[0].runs.iter().any(|run| {
            run.text.contains("ORANGE")
                && run.style.foreground
                    == TerminalColor::Rgb {
                        red: 215,
                        green: 119,
                        blue: 87,
                    }
        }));
    }

    #[test]
    fn scroll_metrics_report_retained_history() {
        let mut state = ScreenState::new(2, 8, 4096);
        state.process(b"one\r\ntwo\r\nthree", |_, _| {});
        let metrics = state.scroll_metrics();
        assert_eq!(metrics.offset_from_bottom, 0);
        assert!(metrics.max_offset_from_bottom >= 1);
        assert_eq!(metrics.viewport_rows, 2);
    }
}
