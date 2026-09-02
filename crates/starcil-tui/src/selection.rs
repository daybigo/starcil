//! Cell-based terminal selection and clipboard extraction.

use starcil_platform::{Clipboard, ClipboardError};

use crate::mirror::PaneMirror;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct CellPoint {
    pub row: u16,
    pub col: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selection {
    pub pane_id: String,
    pub anchor: CellPoint,
    pub head: CellPoint,
}

impl Selection {
    pub fn normalized(&self) -> (CellPoint, CellPoint) {
        if self.anchor <= self.head {
            (self.anchor, self.head)
        } else {
            (self.head, self.anchor)
        }
    }

    pub fn contains(&self, pane_id: &str, row: u16, col: u16) -> bool {
        if self.pane_id != pane_id {
            return false;
        }
        let point = CellPoint { row, col };
        let (start, end) = self.normalized();
        point >= start && point <= end
    }
}

#[derive(Debug, Default)]
pub struct SelectionController {
    selection: Option<Selection>,
    dragging: bool,
    moved: bool,
    last_click: Option<(String, CellPoint)>,
}

impl SelectionController {
    pub fn selection(&self) -> Option<&Selection> {
        self.selection.as_ref()
    }

    pub fn is_dragging(&self) -> bool {
        self.dragging
    }

    pub fn begin(&mut self, pane_id: impl Into<String>, row: u16, col: u16) -> bool {
        let pane_id = pane_id.into();
        let point = CellPoint { row, col };
        let double_click = self.last_click.as_ref() == Some(&(pane_id.clone(), point));
        self.last_click = Some((pane_id.clone(), point));
        self.selection = Some(Selection {
            pane_id,
            anchor: point,
            head: point,
        });
        self.dragging = true;
        self.moved = false;
        double_click
    }

    pub fn update(&mut self, pane_id: &str, row: u16, col: u16) {
        if !self.dragging {
            return;
        }
        if let Some(selection) = self
            .selection
            .as_mut()
            .filter(|selection| selection.pane_id == pane_id)
        {
            selection.head = CellPoint { row, col };
            if selection.head != selection.anchor {
                self.moved = true;
            }
        }
    }

    pub fn finish(&mut self) -> bool {
        let was_dragging = self.dragging;
        self.dragging = false;
        if was_dragging && !self.moved {
            // A press-release without movement is a click (focus), not a
            // selection: keep nothing so it can never reach the clipboard.
            self.selection = None;
            return false;
        }
        was_dragging && self.selection.is_some()
    }

    pub fn set_range(
        &mut self,
        pane_id: impl Into<String>,
        anchor: CellPoint,
        head: CellPoint,
    ) {
        self.selection = Some(Selection {
            pane_id: pane_id.into(),
            anchor,
            head,
        });
        self.dragging = false;
        self.moved = true;
    }

    pub fn select_word(&mut self, pane_id: &str, row: u16, col: u16, mirror: &PaneMirror) {
        let chars = mirror.line_text(row).chars().collect::<Vec<_>>();
        if chars.is_empty() {
            return;
        }
        let col = usize::from(col).min(chars.len().saturating_sub(1));
        let class = word_class(chars[col]);
        let mut start = col;
        while start > 0 && word_class(chars[start - 1]) == class {
            start -= 1;
        }
        let mut end = col;
        while end + 1 < chars.len() && word_class(chars[end + 1]) == class {
            end += 1;
        }
        self.set_range(
            pane_id,
            CellPoint {
                row,
                col: start as u16,
            },
            CellPoint {
                row,
                col: end as u16,
            },
        );
    }

    pub fn clear(&mut self) {
        self.selection = None;
        self.dragging = false;
        self.moved = false;
    }

    pub fn is_selected(&self, pane_id: &str, row: u16, col: u16) -> bool {
        self.selection
            .as_ref()
            .is_some_and(|selection| selection.contains(pane_id, row, col))
    }

    pub fn selected_text(&self, mirror: &PaneMirror) -> Option<String> {
        let selection = self
            .selection
            .as_ref()
            .filter(|selection| selection.pane_id == mirror.pane_id())?;
        let (start, end) = selection.normalized();
        let mut lines = Vec::new();
        for row in start.row..=end.row.min(mirror.rows().saturating_sub(1)) {
            let first_col = if row == start.row { start.col } else { 0 };
            let last_col = if row == end.row {
                end.col
            } else {
                mirror.cols().saturating_sub(1)
            };
            let mut line = String::new();
            for col in first_col..=last_col.min(mirror.cols().saturating_sub(1)) {
                if let Some(cell) = mirror.cell(row, col) {
                    line.push(cell.ch);
                }
            }
            lines.push(line.trim_end_matches(' ').to_owned());
        }
        Some(lines.join("\n"))
    }

    pub fn copy_to<C: Clipboard>(
        &self,
        mirror: &PaneMirror,
        clipboard: &mut C,
    ) -> Result<Option<String>, ClipboardError> {
        let Some(text) = self.selected_text(mirror) else {
            return Ok(None);
        };
        clipboard.set_text(&text)?;
        Ok(Some(text))
    }
}

fn word_class(character: char) -> u8 {
    if character.is_alphanumeric() || character == '_' || character == '-' {
        0
    } else if character.is_whitespace() {
        1
    } else {
        2
    }
}
