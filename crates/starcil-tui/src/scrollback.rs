//! Local scroll offsets, keyboard copy mode, and external-editor handoff.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use thiserror::Error;

use crate::selection::{CellPoint, SelectionController};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CopyState {
    pub pane_id: String,
    pub cursor: CellPoint,
    pub anchor: Option<CellPoint>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditorDocument {
    pub pane_id: String,
    pub path: PathBuf,
}

#[derive(Debug, Error)]
pub enum EditorError {
    #[error("could not write scrollback file {path}: {source}")]
    Write {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("could not launch editor `{editor}`: {source}")]
    Launch {
        editor: String,
        #[source]
        source: std::io::Error,
    },
}

pub trait EditorLauncher {
    fn open(&mut self, path: &Path) -> Result<(), EditorError>;
}

#[derive(Debug, Default)]
pub struct ProcessEditor;

impl EditorLauncher for ProcessEditor {
    fn open(&mut self, path: &Path) -> Result<(), EditorError> {
        let editor = std::env::var("EDITOR").unwrap_or_else(|_| {
            if cfg!(windows) {
                "notepad.exe".to_owned()
            } else {
                "vi".to_owned()
            }
        });
        Command::new(&editor)
            .arg(path)
            .status()
            .map_err(|source| EditorError::Launch { editor, source })?;
        Ok(())
    }
}

#[derive(Debug, Default)]
pub struct ScrollbackController {
    offsets: BTreeMap<String, u64>,
    copy: Option<CopyState>,
    pending_reads: BTreeMap<String, String>,
}

impl ScrollbackController {
    pub fn offset(&self, pane_id: &str) -> u64 {
        self.offsets.get(pane_id).copied().unwrap_or_default()
    }

    pub fn set_offset(&mut self, pane_id: impl Into<String>, offset: u64, maximum: u64) -> u64 {
        let pane_id = pane_id.into();
        let offset = offset.min(maximum);
        if offset == 0 {
            self.offsets.remove(&pane_id);
        } else {
            self.offsets.insert(pane_id, offset);
        }
        offset
    }

    pub fn scroll_by(
        &mut self,
        pane_id: impl Into<String>,
        lines: i64,
        maximum: u64,
    ) -> u64 {
        let pane_id = pane_id.into();
        let current = self.offset(&pane_id);
        let next = if lines >= 0 {
            current.saturating_add(lines as u64)
        } else {
            current.saturating_sub(lines.unsigned_abs())
        };
        self.set_offset(pane_id, next, maximum)
    }

    pub fn return_live(&mut self, pane_id: &str) -> bool {
        self.offsets.remove(pane_id).is_some()
    }

    pub fn copy_state(&self) -> Option<&CopyState> {
        self.copy.as_ref()
    }

    pub fn enter_copy(&mut self, pane_id: impl Into<String>, rows: u16) {
        let pane_id = pane_id.into();
        self.copy = Some(CopyState {
            pane_id: pane_id.clone(),
            cursor: CellPoint {
                row: rows.saturating_sub(1),
                col: 0,
            },
            anchor: None,
        });
        self.offsets.entry(pane_id).or_default();
    }

    pub fn exit_copy(&mut self) {
        self.copy = None;
    }

    pub fn move_copy_cursor(
        &mut self,
        row_delta: i32,
        col_delta: i32,
        rows: u16,
        cols: u16,
        selection: &mut SelectionController,
    ) {
        let Some(copy) = self.copy.as_mut() else {
            return;
        };
        copy.cursor.row = clamp_delta(copy.cursor.row, row_delta, rows.saturating_sub(1));
        copy.cursor.col = clamp_delta(copy.cursor.col, col_delta, cols.saturating_sub(1));
        if let Some(anchor) = copy.anchor {
            selection.set_range(copy.pane_id.clone(), anchor, copy.cursor);
        }
    }

    pub fn toggle_copy_selection(&mut self, selection: &mut SelectionController) {
        let Some(copy) = self.copy.as_mut() else {
            return;
        };
        if copy.anchor.is_some() {
            copy.anchor = None;
            selection.clear();
        } else {
            copy.anchor = Some(copy.cursor);
            selection.set_range(copy.pane_id.clone(), copy.cursor, copy.cursor);
        }
    }

    pub fn register_read(&mut self, request_id: impl Into<String>, pane_id: impl Into<String>) {
        self.pending_reads.insert(request_id.into(), pane_id.into());
    }

    pub fn complete_read(
        &mut self,
        request_id: &str,
        text: &str,
    ) -> Result<Option<EditorDocument>, EditorError> {
        let Some(pane_id) = self.pending_reads.remove(request_id) else {
            return Ok(None);
        };
        let safe_id = pane_id
            .chars()
            .map(|character| {
                if character.is_ascii_alphanumeric() || character == '-' || character == '_' {
                    character
                } else {
                    '_'
                }
            })
            .collect::<String>();
        let path = std::env::temp_dir().join(format!(
            "starcil-scrollback-{}-{safe_id}.txt",
            std::process::id()
        ));
        fs::write(&path, text).map_err(|source| EditorError::Write {
            path: path.clone(),
            source,
        })?;
        Ok(Some(EditorDocument { pane_id, path }))
    }
}

fn clamp_delta(value: u16, delta: i32, maximum: u16) -> u16 {
    if delta >= 0 {
        value.saturating_add(delta as u16).min(maximum)
    } else {
        value.saturating_sub(delta.unsigned_abs() as u16)
    }
}
