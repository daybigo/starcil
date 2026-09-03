//! OS-facing building blocks for Starcil.
//!
//! This crate deliberately keeps platform details out of the server and TUI crates.

pub mod clipboard;
pub mod detach;
pub mod endpoint;
pub mod notify;
pub mod paths;
pub mod transport;

pub use clipboard::{ArboardClipboard, Clipboard, ClipboardError};
pub use detach::{spawn_detached, DetachError};
pub use endpoint::{EndpointError, TransportEndpoint};
pub use notify::show_desktop_notification;
pub use paths::{PathError, PlatformPaths};
pub use transport::{
    spawn_stream_transport, spawn_stream_transport_with_limits, InMemoryTransport, Transport,
    TransportError, TransportFrame, TransportHandle, DEFAULT_MAX_FRAME_SIZE,
    DEFAULT_MAX_RAW_PAYLOAD_SIZE,
};

#[cfg(windows)]
pub use transport::{connect_named_pipe, NamedPipeListener};
#[cfg(unix)]
pub use transport::{connect_unix_socket, UnixSocketListener};

/// This platform's session transport: named pipes on Windows, Unix sockets
/// elsewhere. Server and clients use these names and never spell the OS.
#[cfg(windows)]
pub use transport::{connect_named_pipe as connect_session, NamedPipeListener as SessionListener};
#[cfg(unix)]
pub use transport::{connect_unix_socket as connect_session, UnixSocketListener as SessionListener};
