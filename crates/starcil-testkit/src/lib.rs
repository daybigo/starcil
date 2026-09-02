//! starcil-testkit — fakes and fixtures shared by crate tests: a fake
//! TerminalHost with scripted screens, plus helpers to drive ServerCore in
//! unit tests without any PTY.

use starcil_server::hosttraits::{
    HostError, ReadFormat, ReadSource, ScrollInfo, TerminalHost, TerminalReadout, TerminalSpawn,
};
use std::collections::BTreeMap;

#[derive(Debug, Default, Clone)]
pub struct FakeTerminal {
    pub spec_cwd: String,
    pub env: BTreeMap<String, String>,
    pub alive: bool,
    pub screen: String,
    pub title: Option<String>,
    pub writes: Vec<FakeWrite>,
    pub size: (u16, u16),
    pub change_seq: u64,
    /// Process names under the shell as the host would report them:
    /// `None` = the host cannot tell, `Some(vec![])` = idle shell.
    pub descendants: Option<Vec<String>>,
    /// What `shell_cwd` answers (None = the host cannot tell).
    pub cwd: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FakeWrite {
    Text(String),
    Enter,
    Keys(Vec<String>),
    Paste(String),
}

#[derive(Debug, Default)]
pub struct FakeHost {
    pub terminals: BTreeMap<String, FakeTerminal>,
    next: u64,
}

impl FakeHost {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn terminal(&self, id: &str) -> &FakeTerminal {
        &self.terminals[id]
    }

    pub fn set_screen(&mut self, id: &str, screen: &str) {
        let t = self.terminals.get_mut(id).expect("fake terminal exists");
        t.screen = screen.to_string();
        t.change_seq += 1;
    }
}

impl TerminalHost for FakeHost {
    fn spawn(&mut self, spec: TerminalSpawn) -> Result<String, HostError> {
        self.next += 1;
        let id = format!("term_fake{}", self.next);
        self.terminals.insert(
            id.clone(),
            FakeTerminal {
                spec_cwd: spec.cwd,
                env: spec.env,
                alive: true,
                screen: String::new(),
                title: None,
                writes: Vec::new(),
                size: (spec.cols, spec.rows),
                change_seq: 0,
                descendants: None,
                cwd: None,
            },
        );
        Ok(id)
    }

    fn kill(&mut self, terminal_id: &str) -> Result<(), HostError> {
        match self.terminals.get_mut(terminal_id) {
            Some(t) => {
                t.alive = false;
                Ok(())
            }
            None => Err(HostError::NotFound(terminal_id.into())),
        }
    }

    fn is_alive(&self, terminal_id: &str) -> bool {
        self.terminals.get(terminal_id).map(|t| t.alive).unwrap_or(false)
    }

    fn write_text(&mut self, terminal_id: &str, text: &str) -> Result<(), HostError> {
        self.terminals
            .get_mut(terminal_id)
            .ok_or_else(|| HostError::NotFound(terminal_id.into()))?
            .writes
            .push(FakeWrite::Text(text.to_string()));
        Ok(())
    }

    fn write_enter(&mut self, terminal_id: &str) -> Result<(), HostError> {
        self.terminals
            .get_mut(terminal_id)
            .ok_or_else(|| HostError::NotFound(terminal_id.into()))?
            .writes
            .push(FakeWrite::Enter);
        Ok(())
    }

    fn write_keys(&mut self, terminal_id: &str, keys: &[String]) -> Result<(), HostError> {
        for k in keys {
            if k.is_empty() || k.contains(' ') {
                return Err(HostError::InvalidKey(k.clone()));
            }
        }
        self.terminals
            .get_mut(terminal_id)
            .ok_or_else(|| HostError::NotFound(terminal_id.into()))?
            .writes
            .push(FakeWrite::Keys(keys.to_vec()));
        Ok(())
    }

    fn paste_text(&mut self, terminal_id: &str, text: &str) -> Result<(), HostError> {
        self.terminals
            .get_mut(terminal_id)
            .ok_or_else(|| HostError::NotFound(terminal_id.into()))?
            .writes
            .push(FakeWrite::Paste(text.to_string()));
        Ok(())
    }

    fn resize(&mut self, terminal_id: &str, cols: u16, rows: u16) -> Result<(), HostError> {
        self.terminals
            .get_mut(terminal_id)
            .ok_or_else(|| HostError::NotFound(terminal_id.into()))?
            .size = (cols, rows);
        Ok(())
    }

    fn read(
        &self,
        terminal_id: &str,
        _source: ReadSource,
        lines: usize,
        _format: ReadFormat,
    ) -> Result<TerminalReadout, HostError> {
        let t = self
            .terminals
            .get(terminal_id)
            .ok_or_else(|| HostError::NotFound(terminal_id.into()))?;
        let all: Vec<&str> = t.screen.lines().collect();
        let take = if lines == 0 { all.len() } else { lines.min(all.len()) };
        let text = all[all.len() - take..].join("\n");
        Ok(TerminalReadout { lines: take, text })
    }

    fn scroll_info(&self, terminal_id: &str) -> Option<ScrollInfo> {
        self.terminals.get(terminal_id).map(|t| ScrollInfo {
            offset_from_bottom: 0,
            max_offset_from_bottom: 0,
            viewport_rows: t.size.1,
        })
    }

    fn terminal_title(&self, terminal_id: &str) -> Option<String> {
        self.terminals.get(terminal_id).and_then(|t| t.title.clone())
    }

    fn change_seq(&self, terminal_id: &str) -> u64 {
        self.terminals.get(terminal_id).map(|t| t.change_seq).unwrap_or(0)
    }

    fn process_info(&self, terminal_id: &str) -> Result<serde_json::Value, HostError> {
        if !self.terminals.contains_key(terminal_id) {
            return Err(HostError::NotFound(terminal_id.into()));
        }
        Ok(serde_json::json!({"shell_pid": 4242, "foreground": []}))
    }

    fn descendant_process_names(&self, terminal_id: &str) -> Option<Vec<String>> {
        self.terminals
            .get(terminal_id)
            .and_then(|t| t.descendants.clone())
    }

    fn shell_cwd(&self, terminal_id: &str) -> Option<String> {
        self.terminals.get(terminal_id).and_then(|t| t.cwd.clone())
    }
}

impl starcil_server::streams::TerminalStreamHost for FakeHost {
    fn stream_size(&self, terminal_id: &str) -> Result<(u16, u16), HostError> {
        self.terminals
            .get(terminal_id)
            .map(|t| t.size)
            .ok_or_else(|| HostError::NotFound(terminal_id.into()))
    }

    fn write_stream_bytes(&mut self, terminal_id: &str, bytes: &[u8]) -> Result<(), HostError> {
        self.write_text(terminal_id, &String::from_utf8_lossy(bytes))
    }

    fn scroll_stream(&mut self, _terminal_id: &str, _delta: i32) -> Result<(), HostError> {
        Ok(())
    }

    fn subscribe_stream_output(
        &self,
        terminal_id: &str,
    ) -> Result<Option<Box<dyn starcil_server::streams::TerminalOutput>>, HostError> {
        if !self.terminals.contains_key(terminal_id) {
            return Err(HostError::NotFound(terminal_id.into()));
        }
        Ok(None)
    }
}
