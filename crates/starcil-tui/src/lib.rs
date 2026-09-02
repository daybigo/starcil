//! Headless-testable terminal UI core for Starcil.

mod app;
pub mod composer;
mod input;
mod link;
mod mirror;
mod render;

pub mod dock;
pub mod dock_icons;
pub mod mouse;
pub mod scrollback;
pub mod selection;
pub mod settings;
pub mod sound;
pub mod winmouse;

pub use app::{
    App, AppEffect, AppError, ContextMenuAction, ContextTarget, Mode, Modal, PromptKind,
    SearchState, SidebarState, ToastMessage,
};
pub use input::key_event_to_chord;
pub use link::{ClientMsg, FakeLink, ServerLink, ServerMsg};
pub use mirror::{ApplyOutcome, MirrorCell, PaneMirror};
pub use render::{protocol_style, ratatui_color, render_app};

#[cfg(test)]
mod tests;
