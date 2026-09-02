//! SSH thin-client transport, bridge, reconnect, and clipboard-image protocol.

mod bridge;
mod image_bridge;
mod reconnect;
mod ssh_config;
mod ssh_transport;
mod target;

pub use bridge::{bridge_stdio_pump, bridge_stream_pump, BridgeError};
pub use image_bridge::{
    send_image, send_image_file, ImageBridgeError, ImageBridgeFrame, ImageChunk, ImageEnd,
    ImageFormat, ImagePaste, ImageReceiver, IMAGE_CHUNK_BYTES, MAX_IMAGE_BYTES,
};
pub use reconnect::{
    reconnect_after_loss, retry_delay, ReconnectAction, ReconnectError, ReconnectOutcome,
    ReconnectPhase, ReconnectSignal, ReconnectSleeper, ReconnectStateMachine,
    TokioReconnectSleeper,
};
pub use ssh_config::{SshConfigError, SshConfigManager};
pub use ssh_transport::{
    SshConnectOptions, SshTransport, SshTransportError, REMOTE_BINARY_ENV,
};
pub use target::{RemoteTarget, RemoteTargetError};
