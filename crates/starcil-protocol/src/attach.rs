//! TUI attach stream: the handshake plus the terminal snapshot/patch frames a
//! full client renders from. Separate from the request/response family; a
//! connection in `tui` mode receives these interleaved with events.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ClientMode {
    Rpc,
    Tui,
    TerminalControl,
    TerminalObserve,
    TerminalAttach,
    Plugin,
    Reporter,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Hello {
    pub hello: HelloBody,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HelloBody {
    pub protocol_major: u32,
    pub protocol_minor: u32,
    pub version: String,
    pub mode: ClientMode,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capabilities: Vec<String>,
    /// Terminal size for tui/terminal modes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cols: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rows: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub takeover: Option<bool>,
    /// Terminal target for terminal-observe/control/attach modes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Welcome {
    pub welcome: WelcomeBody,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WelcomeBody {
    pub protocol_major: u32,
    pub protocol_minor: u32,
    pub version: String,
    pub session: String,
    /// Server generation: bumps on every server restart, used to detect
    /// stale references after resume.
    pub generation: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capabilities: Vec<String>,
}

/// One styled run inside a row: text starting at `col` with style `style`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Run {
    pub col: u16,
    pub style: u32,
    pub text: String,
}

/// Packed style table entry: 32-bit fg, 32-bit bg (0x01RRGGBB truecolor,
/// 0x020000NN indexed, 0 = default) and attribute bits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct StyleDef {
    pub fg: u32,
    pub bg: u32,
    pub attrs: u16,
}

pub mod attr_bits {
    pub const BOLD: u16 = 1 << 0;
    pub const DIM: u16 = 1 << 1;
    pub const ITALIC: u16 = 1 << 2;
    pub const UNDERLINE: u16 = 1 << 3;
    pub const INVERSE: u16 = 1 << 4;
    pub const HIDDEN: u16 = 1 << 5;
    pub const STRIKETHROUGH: u16 = 1 << 6;
    pub const BLINK: u16 = 1 << 7;
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RowPatch {
    pub row: u16,
    pub runs: Vec<Run>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneMouseMode {
    pub alternate_screen: bool,
    pub tracking: PaneMouseTracking,
    pub encoding: PaneMouseEncoding,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PaneMouseTracking {
    None,
    Press,
    PressRelease,
    ButtonMotion,
    AnyMotion,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PaneMouseEncoding {
    Default,
    Utf8,
    Sgr,
}

/// A terminal frame for one pane: either a full snapshot (all rows) or an
/// incremental patch (only dirty rows). `seq` is per-pane and gapless; a gap
/// means the client must request a resync snapshot.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TerminalFrame {
    pub pane_id: String,
    pub seq: u64,
    /// Terminal generation (bumps when the pane's process restarts).
    pub generation: u64,
    pub cols: u16,
    pub rows: u16,
    pub snapshot: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub styles: Vec<StyleDef>,
    pub patches: Vec<RowPatch>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cursor: Option<CursorState>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scroll: Option<super::types::ScrollMetrics>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mouse: Option<PaneMouseMode>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CursorState {
    pub row: u16,
    pub col: u16,
    pub visible: bool,
}

/// Frames sent by a tui/terminal-control client.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "input", rename_all = "snake_case")]
pub enum InputFrame {
    Text { pane_id: String, text: String },
    Keys { pane_id: String, keys: Vec<String> },
    Bytes { pane_id: String, data_base64: String },
    Resize { pane_id: String, cols: u16, rows: u16 },
    /// Reserve the bottom `rows` of this pane's layout rect for client-drawn
    /// chrome (the in-pane composer): the server sizes the PTY that much
    /// shorter. `rows: 0` clears the reservation.
    ReserveRows { pane_id: String, rows: u16 },
    Scroll { pane_id: String, delta: i32 },
    /// Ask for a full snapshot of one pane (resync after seq gap).
    Resync { pane_id: String },
    /// Change the set of panes this client wants terminal frames for.
    Subscribe { pane_ids: Vec<String> },
    Release { pane_id: String },
    /// The client's pane-content area (drives server-side layout rects and
    /// PTY sizing).
    ClientArea { cols: u16, rows: u16 },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_roundtrip() {
        let f = TerminalFrame {
            pane_id: "w1:p1".into(),
            seq: 9,
            generation: 1,
            cols: 80,
            rows: 24,
            snapshot: false,
            styles: vec![StyleDef { fg: 0x01ff8800, bg: 0, attrs: attr_bits::BOLD }],
            patches: vec![RowPatch { row: 3, runs: vec![Run { col: 0, style: 0, text: "hello".into() }] }],
            cursor: Some(CursorState { row: 3, col: 5, visible: true }),
            scroll: None,
            mouse: None,
        };
        let s = serde_json::to_string(&f).unwrap();
        let back: TerminalFrame = serde_json::from_str(&s).unwrap();
        assert_eq!(back, f);
    }

    #[test]
    fn input_frames_tag() {
        let s = serde_json::to_string(&InputFrame::Text { pane_id: "w1:p1".into(), text: "ls".into() }).unwrap();
        assert!(s.contains(r#""input":"text""#));
        let r: InputFrame = serde_json::from_str(r#"{"input":"resize","pane_id":"w1:p1","cols":120,"rows":30}"#).unwrap();
        assert!(matches!(r, InputFrame::Resize { cols: 120, rows: 30, .. }));
    }

    #[test]
    fn hello_welcome_shape() {
        let h = Hello {
            hello: HelloBody {
                protocol_major: 1,
                protocol_minor: 0,
                version: "0.1.0".into(),
                mode: ClientMode::Tui,
                capabilities: vec![],
                cols: Some(120),
                rows: Some(40),
                takeover: None,
                target: None,
            },
        };
        let v = serde_json::to_value(&h).unwrap();
        assert_eq!(v["hello"]["mode"], "tui");
    }
}
