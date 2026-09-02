//! OS-facing building blocks for Starcil.
//!
//! This crate deliberately keeps platform details out of the server and TUI crates.

pub mod clipboard;
pub mod detach;
pub mod endpoint;
pub mod paths;
pub mod transport;

pub use clipboard::{ArboardClipboard, Clipboard, ClipboardError};
pub use detach::{spawn_detached, DetachError};
pub use endpoint::{EndpointError, TransportEndpoint};
pub use paths::{PathError, PlatformPaths};
pub use transport::{
    spawn_stream_transport, spawn_stream_transport_with_limits, InMemoryTransport, Transport,
    TransportError, TransportFrame, TransportHandle, DEFAULT_MAX_FRAME_SIZE,
    DEFAULT_MAX_RAW_PAYLOAD_SIZE,
};

#[cfg(windows)]
pub use transport::{connect_named_pipe, NamedPipeListener};
