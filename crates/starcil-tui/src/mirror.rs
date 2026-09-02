use ratatui::style::Style;
use starcil_protocol::attach::{CursorState, PaneMouseMode, StyleDef, TerminalFrame};
use starcil_protocol::types::ScrollMetrics;

use crate::render::protocol_style;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MirrorCell {
    pub ch: char,
    pub style: Style,
}

impl Default for MirrorCell {
    fn default() -> Self {
        Self {
            ch: ' ',
            style: Style::default(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplyOutcome {
    Applied,
    ResyncRequired,
    AwaitingSnapshot,
    WrongPane,
}

#[derive(Debug, Clone)]
pub struct PaneMirror {
    pane_id: String,
    generation: Option<u64>,
    last_seq: Option<u64>,
    cols: u16,
    rows: u16,
    cells: Vec<MirrorCell>,
    styles: Vec<StyleDef>,
    cursor: Option<CursorState>,
    scroll: Option<ScrollMetrics>,
    mouse: Option<PaneMouseMode>,
    awaiting_resync: bool,
}

impl PaneMirror {
    pub fn new(pane_id: impl Into<String>) -> Self {
        Self {
            pane_id: pane_id.into(),
            generation: None,
            last_seq: None,
            cols: 0,
            rows: 0,
            cells: Vec::new(),
            styles: Vec::new(),
            cursor: None,
            scroll: None,
            mouse: None,
            awaiting_resync: false,
        }
    }

    pub fn pane_id(&self) -> &str {
        &self.pane_id
    }

    pub fn cols(&self) -> u16 {
        self.cols
    }

    pub fn rows(&self) -> u16 {
        self.rows
    }

    pub fn last_seq(&self) -> Option<u64> {
        self.last_seq
    }

    pub fn styles(&self) -> &[StyleDef] {
        &self.styles
    }

    pub fn style(&self, id: u32) -> Option<StyleDef> {
        self.styles.get(id as usize).copied()
    }

    pub fn cursor(&self) -> Option<CursorState> {
        self.cursor
    }

    pub fn scroll(&self) -> Option<ScrollMetrics> {
        self.scroll
    }

    pub fn mouse_mode(&self) -> Option<&PaneMouseMode> {
        self.mouse.as_ref()
    }

    pub fn cell(&self, row: u16, col: u16) -> Option<MirrorCell> {
        if row >= self.rows || col >= self.cols {
            return None;
        }
        self.cells
            .get(row as usize * self.cols as usize + col as usize)
            .copied()
    }

    pub fn line_text(&self, row: u16) -> String {
        if row >= self.rows {
            return String::new();
        }
        (0..self.cols)
            .filter_map(|col| self.cell(row, col).map(|cell| cell.ch))
            .collect()
    }

    pub fn screen_text(&self) -> String {
        let mut lines = (0..self.rows)
            .map(|row| self.line_text(row).trim_end_matches(' ').to_owned())
            .collect::<Vec<_>>();
        while lines.last().is_some_and(String::is_empty) {
            lines.pop();
        }
        lines.join("\n")
    }

    pub fn apply(&mut self, frame: &TerminalFrame) -> ApplyOutcome {
        if frame.pane_id != self.pane_id {
            return ApplyOutcome::WrongPane;
        }

        if frame.snapshot {
            self.generation = Some(frame.generation);
            self.last_seq = Some(frame.seq);
            self.cols = frame.cols;
            self.rows = frame.rows;
            self.cells = vec![MirrorCell::default(); frame.cols as usize * frame.rows as usize];
            self.styles = frame.styles.clone();
            self.cursor = frame.cursor;
            self.scroll = frame.scroll;
            if let Some(mouse) = &frame.mouse {
                self.mouse = Some(mouse.clone());
            }
            self.awaiting_resync = false;
            self.apply_rows(frame);
            return ApplyOutcome::Applied;
        }

        if self.awaiting_resync {
            return ApplyOutcome::AwaitingSnapshot;
        }
        let expected = self.last_seq.map(|seq| seq.saturating_add(1));
        if self.generation != Some(frame.generation) || expected != Some(frame.seq) {
            self.awaiting_resync = true;
            return ApplyOutcome::ResyncRequired;
        }

        if frame.cols != self.cols || frame.rows != self.rows {
            self.resize(frame.cols, frame.rows);
        }
        if !frame.styles.is_empty() {
            self.styles = frame.styles.clone();
        }
        self.apply_rows(frame);
        // Incremental frames omit the cursor when it did not move; None
        // means "unchanged", never "hidden" (hidden arrives as visible=false).
        if frame.cursor.is_some() {
            self.cursor = frame.cursor;
        }
        self.scroll = frame.scroll;
        if let Some(mouse) = &frame.mouse {
            self.mouse = Some(mouse.clone());
        }
        self.last_seq = Some(frame.seq);
        ApplyOutcome::Applied
    }

    fn resize(&mut self, cols: u16, rows: u16) {
        let mut resized = vec![MirrorCell::default(); cols as usize * rows as usize];
        for row in 0..rows.min(self.rows) {
            for col in 0..cols.min(self.cols) {
                let old = row as usize * self.cols as usize + col as usize;
                let new = row as usize * cols as usize + col as usize;
                resized[new] = self.cells[old];
            }
        }
        self.cols = cols;
        self.rows = rows;
        self.cells = resized;
    }

    fn apply_rows(&mut self, frame: &TerminalFrame) {
        for patch in &frame.patches {
            if patch.row >= self.rows {
                continue;
            }
            let start = patch.row as usize * self.cols as usize;
            let end = start + self.cols as usize;
            self.cells[start..end].fill(MirrorCell::default());
            for run in &patch.runs {
                let style = self
                    .styles
                    .get(run.style as usize)
                    .copied()
                    .map(protocol_style)
                    .unwrap_or_default();
                let mut col = run.col;
                for ch in run.text.chars() {
                    if col >= self.cols {
                        break;
                    }
                    let index = patch.row as usize * self.cols as usize + col as usize;
                    self.cells[index] = MirrorCell { ch, style };
                    col = col.saturating_add(1);
                }
            }
        }
    }
}
