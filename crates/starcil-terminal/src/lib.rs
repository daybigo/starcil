//! ConPTY/PTY ownership and terminal emulation for Starcil panes.

mod command;
mod interceptor;
mod pane;
mod screen;

pub use command::{prepare_environment, PaneCommand, PreparedEnvironment};
pub use pane::{
    PaneTerminal, QueryResponseCounts, ResizeOutcome, ScreenStability, TerminalError,
    TerminalSize,
};
pub use screen::{
    ReadFormat, ReadSource, TerminalCellStyle, TerminalColor, TerminalCursor,
    TerminalFrameRow, TerminalMouseEncoding, TerminalMouseMode, TerminalMouseTracking,
    TerminalRead, TerminalScreenFrame, TerminalScrollMetrics, TerminalStyledRun,
};
