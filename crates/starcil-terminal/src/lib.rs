//! ConPTY/PTY ownership and terminal emulation for Starcil panes.

mod command;
mod interceptor;
mod keyboard;
mod pane;
mod screen;

pub use command::{prepare_environment, PaneCommand, PreparedEnvironment};
pub use keyboard::{
    encode_key, kitty_flags_response, win32_passthrough, InvalidKey, TerminalKeyboardMode,
    KITTY_DISAMBIGUATE, KITTY_REPORT_ALL_KEYS,
};
pub use pane::{
    PaneTerminal, QueryResponseCounts, ResizeOutcome, ScreenStability, TerminalError,
    TerminalSize,
};
pub use screen::{
    ReadFormat, ReadSource, TerminalCellStyle, TerminalColor, TerminalCursor,
    TerminalFrameRow, TerminalMouseEncoding, TerminalMouseMode, TerminalMouseTracking,
    TerminalRead, TerminalScreenFrame, TerminalScrollMetrics, TerminalStyledRun,
};
