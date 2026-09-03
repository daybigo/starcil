use std::future::Future;
use std::pin::Pin;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

use serde_json::{json, Value};
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::mpsc;

#[cfg(windows)]
use tokio::net::windows::named_pipe::{
    ClientOptions, NamedPipeClient, NamedPipeServer, ServerOptions,
};

#[cfg(any(windows, unix))]
use crate::endpoint::TransportEndpoint;

pub const DEFAULT_MAX_FRAME_SIZE: usize = 8 * 1024 * 1024;
pub const DEFAULT_MAX_RAW_PAYLOAD_SIZE: usize = 64 * 1024 * 1024;
const CHANNEL_CAPACITY: usize = 128;

#[derive(Debug, Clone, PartialEq)]
pub enum TransportFrame {
    Json(Value),
    Raw(Vec<u8>),
}

enum OutgoingFrame {
    Json(Vec<u8>),
    Raw(Vec<u8>),
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum TransportError {
    #[error("transport I/O error: {0}")]
    Io(String),
    #[error("invalid Unix socket endpoint: {0}")]
    InvalidUnixSocketEndpoint(String),
    #[error("invalid JSON frame: {0}")]
    InvalidJson(String),
    #[error("NDJSON frame exceeds the {max_bytes} byte limit")]
    FrameTooLarge { max_bytes: usize },
    #[error("raw payload exceeds the {max_bytes} byte limit")]
    RawPayloadTooLarge { max_bytes: usize },
    #[error("graphics stream header requires an unsigned integer data_length")]
    InvalidRawPayloadLength,
    #[error("raw payload ended early: expected {expected} bytes, received {received}")]
    TruncatedRawPayload { expected: usize, received: usize },
    #[error("raw payload framing is not enabled for this connection")]
    RawFramingDisabled,
    #[error("received a raw payload through JSON-only recv; use recv_frame")]
    UnexpectedRawPayload,
    #[error("transport connection closed")]
    Closed,
    #[error("endpoint is not a Windows named pipe: {0}")]
    InvalidNamedPipeEndpoint(String),
}

pub trait Transport: Send {
    fn send<'a>(
        &'a mut self,
        frame: Value,
    ) -> Pin<Box<dyn Future<Output = Result<(), TransportError>> + Send + 'a>>;

    fn recv<'a>(
        &'a mut self,
    ) -> Pin<Box<dyn Future<Output = Result<Option<Value>, TransportError>> + Send + 'a>>;
}

pub struct TransportHandle {
    incoming: mpsc::Receiver<Result<TransportFrame, TransportError>>,
    outgoing: mpsc::Sender<OutgoingFrame>,
    max_frame_size: usize,
    max_raw_payload_size: usize,
    raw_framing: Arc<AtomicBool>,
    direct_raw_framing: Arc<AtomicBool>,
}

impl TransportHandle {
    /// Permanently switches this dedicated connection to graphics stream framing.
    ///
    /// Call this before acknowledging `pane.graphics.stream`. Subsequent inbound
    /// JSON headers must carry `data_length` and are followed by exactly that many
    /// raw bytes. The mode intentionally cannot be disabled because the protocol
    /// assigns the connection to the graphics stream until it closes.
    pub fn enable_raw_framing(&self) {
        self.raw_framing.store(true, Ordering::Release);
    }

    pub fn raw_framing_enabled(&self) -> bool {
        self.raw_framing.load(Ordering::Acquire)
    }

    /// Switch a dedicated connection to unframed terminal bytes after its
    /// NDJSON handshake. Unlike graphics framing, inbound bytes are delivered
    /// directly and do not require a `data_length` header.
    pub fn enable_direct_raw_framing(&self) {
        self.direct_raw_framing.store(true, Ordering::Release);
    }

    pub fn direct_raw_framing_enabled(&self) -> bool {
        self.direct_raw_framing.load(Ordering::Acquire)
    }

    pub async fn send_raw(&mut self, payload: &[u8]) -> Result<(), TransportError> {
        if !self.raw_framing_enabled() && !self.direct_raw_framing_enabled() {
            return Err(TransportError::RawFramingDisabled);
        }
        if payload.len() > self.max_raw_payload_size {
            return Err(TransportError::RawPayloadTooLarge {
                max_bytes: self.max_raw_payload_size,
            });
        }
        self.outgoing
            .send(OutgoingFrame::Raw(payload.to_vec()))
            .await
            .map_err(|_| TransportError::Closed)
    }

    pub async fn recv_frame(&mut self) -> Result<Option<TransportFrame>, TransportError> {
        match self.incoming.recv().await {
            Some(Ok(frame)) => Ok(Some(frame)),
            Some(Err(error)) => Err(error),
            None => Ok(None),
        }
    }
}

impl Transport for TransportHandle {
    fn send<'a>(
        &'a mut self,
        frame: Value,
    ) -> Pin<Box<dyn Future<Output = Result<(), TransportError>> + Send + 'a>> {
        Box::pin(async move {
            let encoded = encode_frame(&frame, self.max_frame_size)?;
            self.outgoing
                .send(OutgoingFrame::Json(encoded))
                .await
                .map_err(|_| TransportError::Closed)
        })
    }

    fn recv<'a>(
        &'a mut self,
    ) -> Pin<Box<dyn Future<Output = Result<Option<Value>, TransportError>> + Send + 'a>> {
        Box::pin(async move {
            match self.recv_frame().await? {
                Some(TransportFrame::Json(frame)) => Ok(Some(frame)),
                Some(TransportFrame::Raw(_)) => Err(TransportError::UnexpectedRawPayload),
                None => Ok(None),
            }
        })
    }
}

pub fn spawn_stream_transport<S>(stream: S, max_frame_size: usize) -> TransportHandle
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    spawn_stream_transport_with_limits(
        stream,
        max_frame_size,
        DEFAULT_MAX_RAW_PAYLOAD_SIZE,
    )
}

pub fn spawn_stream_transport_with_limits<S>(
    stream: S,
    max_frame_size: usize,
    max_raw_payload_size: usize,
) -> TransportHandle
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let max_frame_size = max_frame_size.max(1);
    let max_raw_payload_size = max_raw_payload_size.max(1);
    let (reader, mut writer) = tokio::io::split(stream);
    let (incoming_tx, incoming) = mpsc::channel(CHANNEL_CAPACITY);
    let (outgoing, mut outgoing_rx) = mpsc::channel::<OutgoingFrame>(CHANNEL_CAPACITY);
    let reader_error_outgoing = outgoing.clone();
    let raw_framing = Arc::new(AtomicBool::new(false));
    let reader_raw_framing = Arc::clone(&raw_framing);
    let direct_raw_framing = Arc::new(AtomicBool::new(false));
    let reader_direct_raw_framing = Arc::clone(&direct_raw_framing);

    tokio::spawn(async move {
        while let Some(frame) = outgoing_rx.recv().await {
            let encoded = match frame {
                OutgoingFrame::Json(mut encoded) => {
                    encoded.push(b'\n');
                    encoded
                }
                OutgoingFrame::Raw(encoded) => encoded,
            };
            if writer.write_all(&encoded).await.is_err() {
                break;
            }
        }
        let _ = writer.shutdown().await;
    });

    tokio::spawn(async move {
        run_reader(
            reader,
            incoming_tx,
            reader_error_outgoing,
            max_frame_size,
            max_raw_payload_size,
            reader_raw_framing,
            reader_direct_raw_framing,
        )
        .await;
    });

    TransportHandle {
        incoming,
        outgoing,
        max_frame_size,
        max_raw_payload_size,
        raw_framing,
        direct_raw_framing,
    }
}

fn encode_frame(frame: &Value, max_frame_size: usize) -> Result<Vec<u8>, TransportError> {
    let encoded = serde_json::to_vec(frame)
        .map_err(|error| TransportError::InvalidJson(error.to_string()))?;
    if encoded.len() > max_frame_size {
        return Err(TransportError::FrameTooLarge {
            max_bytes: max_frame_size,
        });
    }
    Ok(encoded)
}

async fn run_reader<R>(
    mut reader: R,
    incoming: mpsc::Sender<Result<TransportFrame, TransportError>>,
    outgoing: mpsc::Sender<OutgoingFrame>,
    max_frame_size: usize,
    max_raw_payload_size: usize,
    raw_framing: Arc<AtomicBool>,
    direct_raw_framing: Arc<AtomicBool>,
) where
    R: AsyncRead + Unpin,
{
    let mut read_buffer = [0_u8; 8 * 1024];
    let mut frame = Vec::with_capacity(max_frame_size.min(8 * 1024));
    let mut discarding_oversized = false;
    let mut raw_payload = Vec::new();
    let mut raw_payload_length = None;

    loop {
        let length = match reader.read(&mut read_buffer).await {
            Ok(0) => {
                if let Some(expected) = raw_payload_length {
                    let _ = incoming
                        .send(Err(TransportError::TruncatedRawPayload {
                            expected,
                            received: raw_payload.len(),
                        }))
                        .await;
                    return;
                }
                if !frame.is_empty() && !discarding_oversized {
                    match deliver_frame(
                        &frame,
                        &incoming,
                        &outgoing,
                        raw_framing.load(Ordering::Acquire),
                        max_raw_payload_size,
                    )
                    .await
                    {
                        Delivery::ExpectRaw(expected) => {
                            let _ = incoming
                                .send(Err(TransportError::TruncatedRawPayload {
                                    expected,
                                    received: 0,
                                }))
                                .await;
                        }
                        Delivery::Continue | Delivery::Stop => {}
                    }
                }
                return;
            }
            Ok(length) => length,
            Err(error) => {
                let _ = incoming
                    .send(Err(TransportError::Io(error.to_string())))
                    .await;
                return;
            }
        };

        let mut cursor = 0;
        if direct_raw_framing.load(Ordering::Acquire) {
            if incoming
                .send(Ok(TransportFrame::Raw(read_buffer[..length].to_vec())))
                .await
                .is_err()
            {
                return;
            }
            continue;
        }
        while cursor < length {
            if let Some(expected) = raw_payload_length {
                let remaining = expected - raw_payload.len();
                let available = length - cursor;
                let take = remaining.min(available);
                raw_payload.extend_from_slice(&read_buffer[cursor..cursor + take]);
                cursor += take;

                if raw_payload.len() == expected {
                    let payload = std::mem::take(&mut raw_payload);
                    raw_payload_length = None;
                    if incoming
                        .send(Ok(TransportFrame::Raw(payload)))
                        .await
                        .is_err()
                    {
                        return;
                    }
                }
                continue;
            }

            let byte = read_buffer[cursor];
            cursor += 1;
            if discarding_oversized {
                if byte == b'\n' {
                    discarding_oversized = false;
                }
                continue;
            }

            if byte == b'\n' {
                if !frame.is_empty() {
                    match deliver_frame(
                        &frame,
                        &incoming,
                        &outgoing,
                        raw_framing.load(Ordering::Acquire),
                        max_raw_payload_size,
                    )
                    .await
                    {
                        Delivery::Continue => {}
                        Delivery::ExpectRaw(0) => {
                            if incoming
                                .send(Ok(TransportFrame::Raw(Vec::new())))
                                .await
                                .is_err()
                            {
                                return;
                            }
                        }
                        Delivery::ExpectRaw(expected) => {
                            raw_payload = Vec::with_capacity(expected);
                            raw_payload_length = Some(expected);
                        }
                        Delivery::Stop => return,
                    }
                }
                frame.clear();
            } else if frame.len() == max_frame_size {
                frame.clear();
                discarding_oversized = true;
                let error = TransportError::FrameTooLarge {
                    max_bytes: max_frame_size,
                };
                send_error_frame(&outgoing, &error).await;
                if incoming.send(Err(error)).await.is_err() {
                    return;
                }
            } else if byte != b'\r' {
                frame.push(byte);
            }
        }
    }
}

enum Delivery {
    Continue,
    ExpectRaw(usize),
    Stop,
}

async fn deliver_frame(
    encoded: &[u8],
    incoming: &mpsc::Sender<Result<TransportFrame, TransportError>>,
    outgoing: &mpsc::Sender<OutgoingFrame>,
    raw_framing: bool,
    max_raw_payload_size: usize,
) -> Delivery {
    match serde_json::from_slice::<Value>(encoded) {
        Ok(frame) => {
            let raw_length = if raw_framing {
                match graphics_payload_length(&frame, max_raw_payload_size) {
                    Ok(length) => Some(length),
                    Err(error) => {
                        send_error_frame(outgoing, &error).await;
                        let _ = incoming.send(Err(error)).await;
                        return Delivery::Stop;
                    }
                }
            } else {
                None
            };

            if incoming
                .send(Ok(TransportFrame::Json(frame)))
                .await
                .is_err()
            {
                return Delivery::Stop;
            }
            raw_length.map_or(Delivery::Continue, Delivery::ExpectRaw)
        }
        Err(error) => {
            let error = TransportError::InvalidJson(error.to_string());
            send_error_frame(outgoing, &error).await;
            if incoming.send(Err(error)).await.is_err() || raw_framing {
                Delivery::Stop
            } else {
                Delivery::Continue
            }
        }
    }
}

fn graphics_payload_length(
    frame: &Value,
    max_raw_payload_size: usize,
) -> Result<usize, TransportError> {
    let length = frame
        .as_object()
        .and_then(|object| object.get("data_length"))
        .and_then(Value::as_u64)
        .and_then(|length| usize::try_from(length).ok())
        .ok_or(TransportError::InvalidRawPayloadLength)?;
    if length > max_raw_payload_size {
        return Err(TransportError::RawPayloadTooLarge {
            max_bytes: max_raw_payload_size,
        });
    }
    Ok(length)
}

async fn send_error_frame(
    outgoing: &mpsc::Sender<OutgoingFrame>,
    error: &TransportError,
) {
    let (code, details) = match error {
        TransportError::FrameTooLarge { max_bytes } => {
            ("frame_too_large", json!({ "max_bytes": max_bytes }))
        }
        TransportError::RawPayloadTooLarge { max_bytes } => {
            ("raw_payload_too_large", json!({ "max_bytes": max_bytes }))
        }
        TransportError::InvalidRawPayloadLength => {
            ("invalid_raw_payload_length", Value::Null)
        }
        TransportError::InvalidJson(_) => ("invalid_json", Value::Null),
        _ => ("transport_error", Value::Null),
    };
    let frame = json!({
        "id": "transport:error",
        "error": {
            "code": code,
            "message": error.to_string(),
            "details": details,
        }
    });
    if let Ok(encoded) = serde_json::to_vec(&frame) {
        let _ = outgoing.send(OutgoingFrame::Json(encoded)).await;
    }
}

pub struct InMemoryTransport {
    incoming: mpsc::Receiver<TransportFrame>,
    outgoing: mpsc::Sender<TransportFrame>,
    max_frame_size: usize,
    max_raw_payload_size: usize,
    raw_framing: AtomicBool,
    direct_raw_framing: AtomicBool,
}

impl InMemoryTransport {
    pub fn pair(max_frame_size: usize) -> (Self, Self) {
        Self::pair_with_limits(max_frame_size, DEFAULT_MAX_RAW_PAYLOAD_SIZE)
    }

    pub fn pair_with_limits(
        max_frame_size: usize,
        max_raw_payload_size: usize,
    ) -> (Self, Self) {
        let (left_tx, left_rx) = mpsc::channel(CHANNEL_CAPACITY);
        let (right_tx, right_rx) = mpsc::channel(CHANNEL_CAPACITY);
        let max_frame_size = max_frame_size.max(1);
        let max_raw_payload_size = max_raw_payload_size.max(1);

        (
            Self {
                incoming: left_rx,
                outgoing: right_tx,
                max_frame_size,
                max_raw_payload_size,
                raw_framing: AtomicBool::new(false),
                direct_raw_framing: AtomicBool::new(false),
            },
            Self {
                incoming: right_rx,
                outgoing: left_tx,
                max_frame_size,
                max_raw_payload_size,
                raw_framing: AtomicBool::new(false),
                direct_raw_framing: AtomicBool::new(false),
            },
        )
    }

    pub fn enable_raw_framing(&self) {
        self.raw_framing.store(true, Ordering::Release);
    }

    pub fn raw_framing_enabled(&self) -> bool {
        self.raw_framing.load(Ordering::Acquire)
    }

    pub fn enable_direct_raw_framing(&self) {
        self.direct_raw_framing.store(true, Ordering::Release);
    }

    pub fn direct_raw_framing_enabled(&self) -> bool {
        self.direct_raw_framing.load(Ordering::Acquire)
    }

    pub async fn send_raw(&mut self, payload: &[u8]) -> Result<(), TransportError> {
        if !self.raw_framing_enabled() && !self.direct_raw_framing_enabled() {
            return Err(TransportError::RawFramingDisabled);
        }
        if payload.len() > self.max_raw_payload_size {
            return Err(TransportError::RawPayloadTooLarge {
                max_bytes: self.max_raw_payload_size,
            });
        }
        self.outgoing
            .send(TransportFrame::Raw(payload.to_vec()))
            .await
            .map_err(|_| TransportError::Closed)
    }

    pub async fn recv_frame(&mut self) -> Result<Option<TransportFrame>, TransportError> {
        Ok(self.incoming.recv().await)
    }
}

impl Transport for InMemoryTransport {
    fn send<'a>(
        &'a mut self,
        frame: Value,
    ) -> Pin<Box<dyn Future<Output = Result<(), TransportError>> + Send + 'a>> {
        Box::pin(async move {
            encode_frame(&frame, self.max_frame_size)?;
            self.outgoing
                .send(TransportFrame::Json(frame))
                .await
                .map_err(|_| TransportError::Closed)
        })
    }

    fn recv<'a>(
        &'a mut self,
    ) -> Pin<Box<dyn Future<Output = Result<Option<Value>, TransportError>> + Send + 'a>> {
        Box::pin(async move {
            match self.recv_frame().await? {
                Some(TransportFrame::Json(frame)) => Ok(Some(frame)),
                Some(TransportFrame::Raw(_)) => Err(TransportError::UnexpectedRawPayload),
                None => Ok(None),
            }
        })
    }
}

#[cfg(windows)]
pub struct NamedPipeListener {
    address: String,
    next_server: Option<NamedPipeServer>,
    max_frame_size: usize,
}

#[cfg(windows)]
impl NamedPipeListener {
    pub fn bind(
        endpoint: &TransportEndpoint,
        max_frame_size: usize,
    ) -> Result<Self, TransportError> {
        let address = match endpoint {
            TransportEndpoint::WindowsPipe { .. } => endpoint.as_address(),
            other => {
                return Err(TransportError::InvalidNamedPipeEndpoint(
                    other.as_address(),
                ));
            }
        };
        let next_server = create_pipe_server(&address, true)?;
        Ok(Self {
            address,
            next_server: Some(next_server),
            max_frame_size: max_frame_size.max(1),
        })
    }

    pub async fn accept(&mut self) -> Result<TransportHandle, TransportError> {
        let server = self.next_server.take().ok_or(TransportError::Closed)?;
        server
            .connect()
            .await
            .map_err(|error| TransportError::Io(error.to_string()))?;
        self.next_server = Some(create_pipe_server(&self.address, false)?);
        Ok(spawn_stream_transport(server, self.max_frame_size))
    }
}

#[cfg(windows)]
fn create_pipe_server(address: &str, first: bool) -> Result<NamedPipeServer, TransportError> {
    ServerOptions::new()
        .first_pipe_instance(first)
        .create(address)
        .map_err(|error| TransportError::Io(error.to_string()))
}

#[cfg(windows)]
pub async fn connect_named_pipe(
    endpoint: &TransportEndpoint,
    max_frame_size: usize,
) -> Result<TransportHandle, TransportError> {
    let address = match endpoint {
        TransportEndpoint::WindowsPipe { .. } => endpoint.as_address(),
        other => {
            return Err(TransportError::InvalidNamedPipeEndpoint(
                other.as_address(),
            ));
        }
    };

    let client = connect_pipe_with_retry(&address).await?;
    Ok(spawn_stream_transport(client, max_frame_size))
}

#[cfg(windows)]
async fn connect_pipe_with_retry(address: &str) -> Result<NamedPipeClient, TransportError> {
    const ATTEMPTS: usize = 100;
    for attempt in 0..ATTEMPTS {
        match ClientOptions::new().open(address) {
            Ok(client) => return Ok(client),
            Err(error) if attempt + 1 < ATTEMPTS => {
                tracing::trace!(%error, attempt, "named pipe is not ready");
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
            Err(error) => return Err(TransportError::Io(error.to_string())),
        }
    }
    Err(TransportError::Closed)
}

/// Session endpoint on Unix: a socket file under the runtime dir
/// (`$XDG_RUNTIME_DIR/starcil/<session>.sock`, or `~/.starcil/`), owner-only.
/// Same shape as [`NamedPipeListener`] so the server is platform-agnostic.
#[cfg(unix)]
pub struct UnixSocketListener {
    path: std::path::PathBuf,
    listener: tokio::net::UnixListener,
    max_frame_size: usize,
}

#[cfg(unix)]
impl UnixSocketListener {
    /// Bind the session socket. Must run inside a tokio runtime. A socket
    /// file left behind by a dead server is removed; one that still answers
    /// means another server owns the session.
    pub fn bind(
        endpoint: &TransportEndpoint,
        max_frame_size: usize,
    ) -> Result<Self, TransportError> {
        use std::os::unix::fs::PermissionsExt;

        let path = match endpoint {
            TransportEndpoint::UnixSocket { path, .. } => path.clone(),
            other => {
                return Err(TransportError::InvalidUnixSocketEndpoint(
                    other.as_address(),
                ));
            }
        };
        let io = |error: std::io::Error| TransportError::Io(error.to_string());
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(io)?;
            let _ = std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700));
        }
        if path.exists() {
            match std::os::unix::net::UnixStream::connect(&path) {
                Ok(_) => {
                    return Err(TransportError::Io(format!(
                        "another Starcil server already listens on {}",
                        path.display()
                    )));
                }
                Err(_) => std::fs::remove_file(&path).map_err(io)?,
            }
        }
        let listener = tokio::net::UnixListener::bind(&path).map_err(io)?;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
        Ok(Self {
            path,
            listener,
            max_frame_size: max_frame_size.max(1),
        })
    }

    pub async fn accept(&mut self) -> Result<TransportHandle, TransportError> {
        let (stream, _) = self
            .listener
            .accept()
            .await
            .map_err(|error| TransportError::Io(error.to_string()))?;
        Ok(spawn_stream_transport(stream, self.max_frame_size))
    }

    pub fn path(&self) -> &std::path::Path {
        &self.path
    }
}

#[cfg(unix)]
impl Drop for UnixSocketListener {
    fn drop(&mut self) {
        // A clean stop leaves no socket file behind; a crash is handled at
        // the next bind.
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Connect to a session's Unix socket, retrying briefly while a freshly
/// spawned server is still binding (the pipe connector does the same).
#[cfg(unix)]
pub async fn connect_unix_socket(
    endpoint: &TransportEndpoint,
    max_frame_size: usize,
) -> Result<TransportHandle, TransportError> {
    let path = match endpoint {
        TransportEndpoint::UnixSocket { path, .. } => path.clone(),
        other => {
            return Err(TransportError::InvalidUnixSocketEndpoint(
                other.as_address(),
            ));
        }
    };
    const ATTEMPTS: usize = 100;
    for attempt in 0..ATTEMPTS {
        match tokio::net::UnixStream::connect(&path).await {
            Ok(stream) => return Ok(spawn_stream_transport(stream, max_frame_size)),
            Err(error) if attempt + 1 < ATTEMPTS => {
                tracing::trace!(%error, attempt, "unix socket is not ready");
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
            Err(error) => return Err(TransportError::Io(error.to_string())),
        }
    }
    Err(TransportError::Closed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test]
    async fn ndjson_handles_split_and_joined_frames() {
        let (mut raw, stream) = tokio::io::duplex(4096);
        let mut transport = spawn_stream_transport(stream, 1024);

        raw.write_all(b"{\"one\":1}\n{\"two\":\"")
            .await
            .expect("first write");
        raw.write_all(b"split\"}\n{\"three\":3}\n")
            .await
            .expect("second write");

        assert_eq!(transport.recv().await.unwrap().unwrap(), json!({"one": 1}));
        assert_eq!(
            transport.recv().await.unwrap().unwrap(),
            json!({"two": "split"})
        );
        assert_eq!(
            transport.recv().await.unwrap().unwrap(),
            json!({"three": 3})
        );
    }

    #[tokio::test]
    async fn graphics_stream_preserves_repeated_exact_raw_payloads() {
        let (mut raw, stream) = tokio::io::duplex(4096);
        let mut transport = spawn_stream_transport_with_limits(stream, 1024, 1024);
        transport.enable_raw_framing();

        let first_header = json!({
            "format": "png",
            "image_width": 2,
            "image_height": 2,
            "data_length": 7,
            "placement": {"viewport_col": 0, "viewport_row": 0}
        });
        let second_header = json!({
            "format": "rgba",
            "image_width": 1,
            "image_height": 1,
            "data_length": 4,
            "placement": {"viewport_col": 1, "viewport_row": 1}
        });
        let first_payload = [0x89, b'P', b'\n', b'{', 0, 0xff, b'}'];
        let second_payload = [1, 2, 3, 4];

        let mut wire = serde_json::to_vec(&first_header).unwrap();
        wire.push(b'\n');
        wire.extend_from_slice(&first_payload);
        wire.extend_from_slice(&serde_json::to_vec(&second_header).unwrap());
        wire.push(b'\n');
        wire.extend_from_slice(&second_payload);
        raw.write_all(&wire).await.expect("mixed framing write");

        assert_eq!(
            transport.recv_frame().await.unwrap(),
            Some(TransportFrame::Json(first_header))
        );
        assert_eq!(
            transport.recv_frame().await.unwrap(),
            Some(TransportFrame::Raw(first_payload.to_vec()))
        );
        assert_eq!(
            transport.recv_frame().await.unwrap(),
            Some(TransportFrame::Json(second_header))
        );
        assert_eq!(
            transport.recv_frame().await.unwrap(),
            Some(TransportFrame::Raw(second_payload.to_vec()))
        );
    }

    #[tokio::test]
    async fn graphics_stream_writer_adds_no_delimiter_after_raw_payload() {
        let (mut raw, stream) = tokio::io::duplex(4096);
        let mut transport = spawn_stream_transport_with_limits(stream, 1024, 1024);
        transport.enable_raw_framing();

        let header = json!({
            "format": "png",
            "image_width": 2,
            "image_height": 2,
            "data_length": 6,
            "placement": {}
        });
        let payload = [0, b'\n', b'{', b'}', 0xfe, 0xff];
        transport.send(header.clone()).await.unwrap();
        transport.send_raw(&payload).await.unwrap();

        let mut expected = serde_json::to_vec(&header).unwrap();
        expected.push(b'\n');
        expected.extend_from_slice(&payload);
        let mut received = vec![0; expected.len()];
        raw.read_exact(&mut received).await.expect("wire output");
        assert_eq!(received, expected);
    }

    #[tokio::test]
    async fn direct_terminal_mode_switches_from_ndjson_to_bidirectional_raw_bytes() {
        let (mut raw, stream) = tokio::io::duplex(4096);
        let mut transport = spawn_stream_transport_with_limits(stream, 1024, 1024);
        raw.write_all(b"{\"hello\":true}\n").await.unwrap();
        assert_eq!(
            transport.recv_frame().await.unwrap(),
            Some(TransportFrame::Json(json!({"hello": true})))
        );

        transport.enable_direct_raw_framing();
        let inbound = [0, b'\n', b'{', 0xff, 0x1b];
        raw.write_all(&inbound).await.unwrap();
        assert_eq!(
            transport.recv_frame().await.unwrap(),
            Some(TransportFrame::Raw(inbound.to_vec()))
        );

        let outbound = [0x1b, b'[', b'H', 0, 0xfe];
        transport.send_raw(&outbound).await.unwrap();
        let mut received = [0; 5];
        raw.read_exact(&mut received).await.unwrap();
        assert_eq!(received, outbound);
    }

    #[tokio::test]
    async fn graphics_stream_rejects_oversized_raw_payload_before_allocating() {
        let (mut raw, stream) = tokio::io::duplex(4096);
        let mut transport = spawn_stream_transport_with_limits(stream, 1024, 4);
        transport.enable_raw_framing();
        let header = json!({
            "format": "png",
            "image_width": 2,
            "image_height": 2,
            "data_length": 5,
            "placement": {}
        });
        let mut encoded = serde_json::to_vec(&header).unwrap();
        encoded.push(b'\n');
        raw.write_all(&encoded).await.expect("oversized header write");

        let error = transport
            .recv_frame()
            .await
            .expect_err("oversized raw declaration");
        assert_eq!(
            error,
            TransportError::RawPayloadTooLarge { max_bytes: 4 }
        );

        let mut raw_reader = tokio::io::BufReader::new(raw);
        let mut response = String::new();
        tokio::io::AsyncBufReadExt::read_line(&mut raw_reader, &mut response)
            .await
            .expect("error response");
        let response: Value = serde_json::from_str(response.trim()).expect("error JSON");
        assert_eq!(response["error"]["code"], "raw_payload_too_large");
    }

    #[tokio::test]
    async fn oversized_frame_returns_error_frame_without_panicking() {
        let (mut raw, stream) = tokio::io::duplex(4096);
        let mut transport = spawn_stream_transport(stream, 64);

        raw.write_all(format!("{{\"payload\":\"{}\"}}\n", "x".repeat(128)).as_bytes())
            .await
            .expect("oversized write");

        let error = transport.recv().await.expect_err("oversized error");
        assert_eq!(error, TransportError::FrameTooLarge { max_bytes: 64 });

        let mut raw_reader = tokio::io::BufReader::new(raw);
        let mut response = String::new();
        tokio::io::AsyncBufReadExt::read_line(&mut raw_reader, &mut response)
            .await
            .expect("error response");
        let response: Value = serde_json::from_str(response.trim()).expect("error JSON");
        assert_eq!(response["error"]["code"], "frame_too_large");
    }

    #[tokio::test]
    async fn three_concurrent_in_memory_clients_echo() {
        let mut clients = Vec::new();
        let mut servers = Vec::new();
        for _ in 0..3 {
            let (client, server) = InMemoryTransport::pair(1024);
            clients.push(client);
            servers.push(server);
        }

        let server_tasks: Vec<_> = servers
            .into_iter()
            .map(|mut server| {
                tokio::spawn(async move {
                    let frame = server.recv().await.unwrap().unwrap();
                    server.send(frame).await.unwrap();
                })
            })
            .collect();

        let client_tasks: Vec<_> = clients
            .into_iter()
            .enumerate()
            .map(|(index, mut client)| {
                tokio::spawn(async move {
                    let expected = json!({"client": index});
                    client.send(expected.clone()).await.unwrap();
                    assert_eq!(client.recv().await.unwrap().unwrap(), expected);
                })
            })
            .collect();

        for task in client_tasks {
            task.await.unwrap();
        }
        for task in server_tasks {
            task.await.unwrap();
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn unix_socket_listener_round_trips_and_cleans_up() {
        let temp = std::env::temp_dir().join(format!("starcil-sock-{}", std::process::id()));
        std::fs::create_dir_all(&temp).unwrap();
        let endpoint = TransportEndpoint::UnixSocket {
            path: temp.join("t.sock"),
            session: "t".to_owned(),
        };
        // A stale file from a dead server must not block the bind.
        std::fs::write(temp.join("t.sock"), b"").unwrap();
        let mut listener = UnixSocketListener::bind(&endpoint, 1024).expect("bind socket");
        let server = tokio::spawn(async move {
            let mut conn = listener.accept().await.expect("accept");
            let frame = conn.recv().await.expect("recv").expect("frame");
            conn.send(frame).await.expect("echo");
            listener
        });
        let mut client = connect_unix_socket(&endpoint, 1024).await.expect("connect");
        client.send(json!({"hello": "unix"})).await.unwrap();
        let echoed = client.recv().await.unwrap().unwrap();
        assert_eq!(echoed, json!({"hello": "unix"}));
        let listener = server.await.unwrap();
        assert!(temp.join("t.sock").exists());
        drop(listener);
        assert!(!temp.join("t.sock").exists(), "a clean stop unlinks the socket");
        // A second server on the same session is refused while the first answers.
        let mut first = UnixSocketListener::bind(&endpoint, 1024).unwrap();
        let busy = tokio::spawn(async move { first.accept().await.map(|_| ()) });
        let second = UnixSocketListener::bind(&endpoint, 1024);
        assert!(second.is_err(), "the live socket answers, so the bind must refuse");
        busy.abort();
        let _ = std::fs::remove_dir_all(&temp);
    }

    #[cfg(windows)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn named_pipe_supports_two_concurrent_clients() {
        use std::sync::atomic::{AtomicU64, Ordering};

        static NEXT_ENDPOINT: AtomicU64 = AtomicU64::new(1);
        let session = format!(
            "test-{}-{}",
            std::process::id(),
            NEXT_ENDPOINT.fetch_add(1, Ordering::Relaxed)
        );
        let endpoint = TransportEndpoint::for_session(&session).expect("endpoint");
        let mut listener = NamedPipeListener::bind(&endpoint, 1024).expect("bind pipe");

        let server = tokio::spawn(async move {
            let mut handlers = Vec::new();
            for _ in 0..2 {
                let mut connection = listener.accept().await.expect("accept client");
                handlers.push(tokio::spawn(async move {
                    let frame = connection.recv().await.unwrap().unwrap();
                    connection.send(frame).await.unwrap();
                }));
            }
            for handler in handlers {
                handler.await.unwrap();
            }
        });

        let client_one = connect_named_pipe(&endpoint, 1024);
        let client_two = connect_named_pipe(&endpoint, 1024);
        let (mut client_one, mut client_two) = tokio::try_join!(client_one, client_two).unwrap();

        let first = async {
            client_one.send(json!({"client": 1})).await.unwrap();
            client_one.recv().await.unwrap().unwrap()
        };
        let second = async {
            client_two.send(json!({"client": 2})).await.unwrap();
            client_two.recv().await.unwrap().unwrap()
        };
        let (first, second) = tokio::join!(first, second);

        assert_eq!(first, json!({"client": 1}));
        assert_eq!(second, json!({"client": 2}));
        server.await.unwrap();
    }
}
