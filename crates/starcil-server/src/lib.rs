//! starcil-server — the session daemon: single-authority state, socket API
//! dispatch, terminal host wiring, persistence, subscriptions.
//! `core`+`dispatch` are the sync heart; the async actor/transport layer
//! (lane B) wraps them.

pub mod actor;
pub mod agents_glue;
pub mod core;
pub mod metadata;
pub mod persistence;
pub mod plugins_glue;
pub mod streams;
pub mod dispatch;
pub mod dispatch_ext;
pub mod hosttraits;

pub use core::ServerCore;
pub use hosttraits::{HostError, ReadFormat, ReadSource, TerminalHost, TerminalReadout, TerminalSpawn};
