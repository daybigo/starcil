//! Direct terminal observe, control, and raw-attach streams.

use crate::actor::SharedServer;
use crate::hosttraits::{HostError, ReadFormat, ReadSource, TerminalHost};
use serde_json::{json, Value};
use starcil_domain::PaneId;
use starcil_platform::{
    InMemoryTransport, Transport, TransportError, TransportFrame, TransportHandle,
};
use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio_util::sync::CancellationToken;

const STREAM_POLL_INTERVAL: Duration = Duration::from_millis(40);
const MAX_OUTPUT_CHUNKS_PER_TICK: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamRequest {
    pub target: String,
    pub cols: Option<u16>,
    pub rows: Option<u16>,
    pub takeover: bool,
}

impl StreamRequest {
    pub fn new(target: impl Into<String>) -> Self {
        Self {
            target: target.into(),
            cols: None,
            rows: None,
            takeover: false,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum StreamError {
    #[error(transparent)]
    Transport(#[from] TransportError),
    #[error("terminal target not found: {0}")]
    TargetNotFound(String),
    #[error("terminal host failed: {0}")]
    Host(String),
    #[error("invalid terminal stream frame: {0}")]
    InvalidFrame(String),
    #[error("server state lock was poisoned")]
    LockPoisoned,
}

impl From<HostError> for StreamError {
    fn from(error: HostError) -> Self {
        Self::Host(error.to_string())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OutputPoll {
    Data(Vec<u8>),
    Empty,
    Closed,
}

/// A non-blocking subscription to raw PTY output.
pub trait TerminalOutput: Send {
    fn try_next(&mut self) -> OutputPoll;
}

impl TerminalOutput for std::sync::mpsc::Receiver<Vec<u8>> {
    fn try_next(&mut self) -> OutputPoll {
        match self.try_recv() {
            Ok(bytes) => OutputPoll::Data(bytes),
            Err(std::sync::mpsc::TryRecvError::Empty) => OutputPoll::Empty,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => OutputPoll::Closed,
        }
    }
}

/// Streaming-only host operations kept separate from the frozen core host seam.
pub trait TerminalStreamHost: TerminalHost {
    fn stream_size(&self, terminal_id: &str) -> Result<(u16, u16), HostError>;
    fn write_stream_bytes(&mut self, terminal_id: &str, bytes: &[u8]) -> Result<(), HostError>;
    fn scroll_stream(&mut self, terminal_id: &str, delta: i32) -> Result<(), HostError>;
    fn subscribe_stream_output(
        &self,
        terminal_id: &str,
    ) -> Result<Option<Box<dyn TerminalOutput>>, HostError>;
}

#[derive(Default)]
pub struct LeaseRegistry {
    state: Mutex<LeaseState>,
}

#[derive(Default)]
struct LeaseState {
    next_generation: u64,
    active: BTreeMap<String, LeaseEntry>,
}

struct LeaseEntry {
    generation: u64,
    revoked: CancellationToken,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("terminal {terminal_id} already has a controller")]
pub struct LeaseConflict {
    pub terminal_id: String,
}

pub struct ControlLease {
    registry: Arc<LeaseRegistry>,
    terminal_id: String,
    generation: u64,
    revoked: CancellationToken,
}

impl LeaseRegistry {
    pub fn acquire(
        self: &Arc<Self>,
        terminal_id: &str,
        takeover: bool,
    ) -> Result<ControlLease, LeaseConflict> {
        let mut state = self.state.lock().expect("lease registry lock poisoned");
        if let Some(current) = state.active.get(terminal_id) {
            if !takeover {
                return Err(LeaseConflict {
                    terminal_id: terminal_id.to_owned(),
                });
            }
            current.revoked.cancel();
        }
        state.next_generation = state.next_generation.saturating_add(1).max(1);
        let generation = state.next_generation;
        let revoked = CancellationToken::new();
        state.active.insert(
            terminal_id.to_owned(),
            LeaseEntry {
                generation,
                revoked: revoked.clone(),
            },
        );
        Ok(ControlLease {
            registry: Arc::clone(self),
            terminal_id: terminal_id.to_owned(),
            generation,
            revoked,
        })
    }

    pub fn has_controller(&self, terminal_id: &str) -> bool {
        self.state
            .lock()
            .expect("lease registry lock poisoned")
            .active
            .contains_key(terminal_id)
    }

    fn release(&self, terminal_id: &str, generation: u64) {
        let mut state = self.state.lock().expect("lease registry lock poisoned");
        if state
            .active
            .get(terminal_id)
            .is_some_and(|entry| entry.generation == generation)
        {
            state.active.remove(terminal_id);
        }
    }

    fn is_current(&self, terminal_id: &str, generation: u64) -> bool {
        self.state
            .lock()
            .expect("lease registry lock poisoned")
            .active
            .get(terminal_id)
            .is_some_and(|entry| entry.generation == generation)
    }
}

impl ControlLease {
    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn revoked(&self) -> CancellationToken {
        self.revoked.clone()
    }

    pub fn is_current(&self) -> bool {
        self.registry
            .is_current(&self.terminal_id, self.generation)
    }
}

impl Drop for ControlLease {
    fn drop(&mut self) {
        self.registry.release(&self.terminal_id, self.generation);
    }
}

/// Read-only NDJSON terminal stream.
pub async fn serve_observe<H, T>(
    server: &SharedServer<H>,
    conn: &mut T,
    request: StreamRequest,
) -> Result<(), StreamError>
where
    H: TerminalStreamHost,
    T: Transport,
{
    let terminal_id = resolve_terminal(server, &request.target)?;
    let mut opened = open_terminal(server, &terminal_id, &request, false)?;
    send_header(conn, &opened.terminal_id, opened.cols, opened.rows).await?;
    send_data(conn, &opened.snapshot).await?;
    if !opened.alive {
        send_closed(conn).await?;
        return Ok(());
    }

    let mut poll = tokio::time::interval(STREAM_POLL_INTERVAL);
    poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            incoming = conn.recv() => {
                match incoming? {
                    Some(_) => send_error(conn, "read_only", "observe streams do not accept terminal input").await?,
                    None => return Ok(()),
                }
            }
            _ = poll.tick() => {
                let batch = take_output(server, &terminal_id, &mut opened)?;
                for bytes in batch.frames {
                    send_data(conn, &bytes).await?;
                }
                if batch.closed {
                    send_closed(conn).await?;
                    return Ok(());
                }
            }
            _ = server.shutdown.cancelled() => return Ok(()),
        }
    }
}

/// Writable NDJSON terminal stream guarded by a generation-token lease.
pub async fn serve_control<H, T>(
    server: &SharedServer<H>,
    conn: &mut T,
    leases: Arc<LeaseRegistry>,
    request: StreamRequest,
) -> Result<(), StreamError>
where
    H: TerminalStreamHost,
    T: Transport,
{
    let terminal_id = resolve_terminal(server, &request.target)?;
    let lease = match leases.acquire(&terminal_id, request.takeover) {
        Ok(lease) => lease,
        Err(error) => {
            send_error(conn, "lease_conflict", &error.to_string()).await?;
            return Ok(());
        }
    };
    let revoked = lease.revoked();
    let mut opened = open_terminal(server, &terminal_id, &request, true)?;
    send_header(conn, &opened.terminal_id, opened.cols, opened.rows).await?;
    send_data(conn, &opened.snapshot).await?;
    if !opened.alive {
        send_closed(conn).await?;
        return Ok(());
    }

    let mut poll = tokio::time::interval(STREAM_POLL_INTERVAL);
    poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            incoming = conn.recv() => {
                let Some(frame) = incoming? else { return Ok(()) };
                if !lease.is_current() {
                    send_error(conn, "lease_revoked", "terminal control lease was taken over").await?;
                    return Ok(());
                }
                match parse_control_frame(&frame) {
                    Ok(ControlAction::Input(bytes)) => with_host(server, |host| {
                        host.write_stream_bytes(&terminal_id, &bytes)
                    })?,
                    Ok(ControlAction::Resize { cols, rows }) => with_host(server, |host| {
                        host.resize(&terminal_id, cols, rows)
                    })?,
                    Ok(ControlAction::Scroll(delta)) => with_host(server, |host| {
                        host.scroll_stream(&terminal_id, delta)
                    })?,
                    Ok(ControlAction::Release) => {
                        conn.send(json!({"released": true, "terminal_id": terminal_id})).await?;
                        return Ok(());
                    }
                    Err(error) => send_error(conn, "invalid_frame", &error.to_string()).await?,
                }
            }
            _ = poll.tick() => {
                let batch = take_output(server, &terminal_id, &mut opened)?;
                for bytes in batch.frames {
                    send_data(conn, &bytes).await?;
                }
                if batch.closed {
                    send_closed(conn).await?;
                    return Ok(());
                }
            }
            _ = revoked.cancelled() => {
                send_error(conn, "lease_revoked", "terminal control lease was taken over").await?;
                return Ok(());
            }
            _ = server.shutdown.cancelled() => return Ok(()),
        }
    }
}

/// Raw human attach: rendered ANSI snapshot, then raw PTY output and raw input.
pub async fn serve_attach<H, T>(
    server: &SharedServer<H>,
    conn: &mut T,
    leases: Arc<LeaseRegistry>,
    request: StreamRequest,
) -> Result<(), StreamError>
where
    H: TerminalStreamHost,
    T: RawStreamTransport,
{
    let terminal_id = resolve_terminal(server, &request.target)?;
    let lease = match leases.acquire(&terminal_id, request.takeover) {
        Ok(lease) => lease,
        Err(error) => {
            send_error(conn, "lease_conflict", &error.to_string()).await?;
            return Ok(());
        }
    };
    let revoked = lease.revoked();
    let mut opened = open_terminal(server, &terminal_id, &request, true)?;
    conn.enable_stream_raw_framing();
    let mut snapshot = b"\x1b[2J\x1b[H".to_vec();
    snapshot.extend_from_slice(&opened.snapshot);
    conn.send_stream_raw(&snapshot).await?;
    if !opened.alive {
        return Ok(());
    }

    let mut poll = tokio::time::interval(STREAM_POLL_INTERVAL);
    poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            incoming = conn.recv_stream_frame() => {
                match incoming? {
                    Some(TransportFrame::Raw(bytes)) => {
                        if !lease.is_current() {
                            return Ok(());
                        }
                        with_host(server, |host| host.write_stream_bytes(&terminal_id, &bytes))?;
                    }
                    Some(TransportFrame::Json(frame)) => match parse_control_frame(&frame) {
                        Ok(ControlAction::Resize { cols, rows }) => with_host(server, |host| {
                            host.resize(&terminal_id, cols, rows)
                        })?,
                        Ok(ControlAction::Scroll(delta)) => with_host(server, |host| {
                            host.scroll_stream(&terminal_id, delta)
                        })?,
                        Ok(ControlAction::Release) => return Ok(()),
                        Ok(ControlAction::Input(bytes)) => with_host(server, |host| {
                            host.write_stream_bytes(&terminal_id, &bytes)
                        })?,
                        Err(_) => {}
                    },
                    None => return Ok(()),
                }
            }
            _ = poll.tick() => {
                let batch = take_output(server, &terminal_id, &mut opened)?;
                for bytes in batch.frames {
                    conn.send_stream_raw(&bytes).await?;
                }
                if batch.closed {
                    return Ok(());
                }
            }
            _ = revoked.cancelled() => {
                conn.send_stream_raw(b"\r\n[starcil: control lease revoked]\r\n").await?;
                return Ok(());
            }
            _ = server.shutdown.cancelled() => return Ok(()),
        }
    }
}

pub trait RawStreamTransport: Transport {
    fn enable_stream_raw_framing(&self);

    fn send_stream_raw<'a>(
        &'a mut self,
        payload: &'a [u8],
    ) -> Pin<Box<dyn Future<Output = Result<(), TransportError>> + Send + 'a>>;

    fn recv_stream_frame<'a>(
        &'a mut self,
    ) -> Pin<
        Box<
            dyn Future<Output = Result<Option<TransportFrame>, TransportError>> + Send + 'a,
        >,
    >;
}

impl RawStreamTransport for TransportHandle {
    fn enable_stream_raw_framing(&self) {
        self.enable_direct_raw_framing();
    }

    fn send_stream_raw<'a>(
        &'a mut self,
        payload: &'a [u8],
    ) -> Pin<Box<dyn Future<Output = Result<(), TransportError>> + Send + 'a>> {
        Box::pin(async move { self.send_raw(payload).await })
    }

    fn recv_stream_frame<'a>(
        &'a mut self,
    ) -> Pin<
        Box<
            dyn Future<Output = Result<Option<TransportFrame>, TransportError>> + Send + 'a,
        >,
    > {
        Box::pin(async move { self.recv_frame().await })
    }
}

impl RawStreamTransport for InMemoryTransport {
    fn enable_stream_raw_framing(&self) {
        self.enable_direct_raw_framing();
    }

    fn send_stream_raw<'a>(
        &'a mut self,
        payload: &'a [u8],
    ) -> Pin<Box<dyn Future<Output = Result<(), TransportError>> + Send + 'a>> {
        Box::pin(async move { self.send_raw(payload).await })
    }

    fn recv_stream_frame<'a>(
        &'a mut self,
    ) -> Pin<
        Box<
            dyn Future<Output = Result<Option<TransportFrame>, TransportError>> + Send + 'a,
        >,
    > {
        Box::pin(async move { self.recv_frame().await })
    }
}

struct OpenedTerminal {
    terminal_id: String,
    cols: u16,
    rows: u16,
    snapshot: Vec<u8>,
    last_change_seq: u64,
    output: Option<Box<dyn TerminalOutput>>,
    alive: bool,
}

struct OutputBatch {
    frames: Vec<Vec<u8>>,
    closed: bool,
}

fn resolve_terminal<H: TerminalHost>(
    server: &SharedServer<H>,
    target: &str,
) -> Result<String, StreamError> {
    let core = server.core.lock().map_err(|_| StreamError::LockPoisoned)?;
    if let Ok(pane_id) = target.parse::<PaneId>() {
        return core
            .model
            .pane(pane_id)
            .map(|pane| pane.terminal_id.clone())
            .map_err(|_| StreamError::TargetNotFound(target.to_owned()));
    }
    if let Some(pane) = core
        .model
        .panes
        .values()
        .find(|pane| pane.terminal_id == target)
    {
        return Ok(pane.terminal_id.clone());
    }
    if let Some(pane_id) = core.model.resolve_agent_name(target) {
        return core
            .model
            .pane(pane_id)
            .map(|pane| pane.terminal_id.clone())
            .map_err(|_| StreamError::TargetNotFound(target.to_owned()));
    }
    Err(StreamError::TargetNotFound(target.to_owned()))
}

fn open_terminal<H: TerminalStreamHost>(
    server: &SharedServer<H>,
    terminal_id: &str,
    request: &StreamRequest,
    allow_resize: bool,
) -> Result<OpenedTerminal, StreamError> {
    let mut core = server.core.lock().map_err(|_| StreamError::LockPoisoned)?;
    if allow_resize {
        if let (Some(cols), Some(rows)) = (request.cols, request.rows) {
            if cols == 0 || rows == 0 {
                return Err(StreamError::InvalidFrame(
                    "terminal dimensions must be positive".to_owned(),
                ));
            }
            core.host.resize(terminal_id, cols, rows)?;
        }
    }
    let output = core.host.subscribe_stream_output(terminal_id)?;
    let (cols, rows) = core.host.stream_size(terminal_id)?;
    let snapshot = core
        .host
        .read(terminal_id, ReadSource::Visible, 0, ReadFormat::Ansi)?
        .text
        .into_bytes();
    let last_change_seq = core.host.change_seq(terminal_id);
    let alive = core.host.is_alive(terminal_id);
    Ok(OpenedTerminal {
        terminal_id: terminal_id.to_owned(),
        cols,
        rows,
        snapshot,
        last_change_seq,
        output,
        alive,
    })
}

fn take_output<H: TerminalStreamHost>(
    server: &SharedServer<H>,
    terminal_id: &str,
    opened: &mut OpenedTerminal,
) -> Result<OutputBatch, StreamError> {
    let mut frames = Vec::new();
    let has_raw_subscription = opened.output.is_some();
    let mut output_closed = false;
    if let Some(output) = opened.output.as_mut() {
        for _ in 0..MAX_OUTPUT_CHUNKS_PER_TICK {
            match output.try_next() {
                OutputPoll::Data(bytes) => frames.push(bytes),
                OutputPoll::Empty => break,
                OutputPoll::Closed => {
                    output_closed = true;
                    break;
                }
            }
        }
    }
    if output_closed {
        opened.output = None;
    }

    let core = server.core.lock().map_err(|_| StreamError::LockPoisoned)?;
    let alive = core.host.is_alive(terminal_id);
    if !has_raw_subscription {
        let change_seq = core.host.change_seq(terminal_id);
        if change_seq != opened.last_change_seq {
            let rendered = core
                .host
                .read(terminal_id, ReadSource::Visible, 0, ReadFormat::Ansi)?
                .text
                .into_bytes();
            opened.last_change_seq = change_seq;
            frames.push(rendered);
        }
    }
    Ok(OutputBatch {
        frames,
        closed: !alive,
    })
}

fn with_host<H, R>(
    server: &SharedServer<H>,
    operation: impl FnOnce(&mut H) -> Result<R, HostError>,
) -> Result<R, StreamError>
where
    H: TerminalStreamHost,
{
    let mut core = server.core.lock().map_err(|_| StreamError::LockPoisoned)?;
    operation(&mut core.host).map_err(StreamError::from)
}

async fn send_header<T: Transport>(
    conn: &mut T,
    terminal_id: &str,
    cols: u16,
    rows: u16,
) -> Result<(), TransportError> {
    conn.send(json!({
        "observe": {
            "terminal_id": terminal_id,
            "cols": cols,
            "rows": rows,
        }
    }))
    .await
}

async fn send_data<T: Transport>(conn: &mut T, bytes: &[u8]) -> Result<(), TransportError> {
    conn.send(json!({"data_base64": encode_base64(bytes)})).await
}

async fn send_closed<T: Transport>(conn: &mut T) -> Result<(), TransportError> {
    conn.send(json!({"terminal": "closed"})).await
}

async fn send_error<T: Transport>(
    conn: &mut T,
    code: &str,
    message: &str,
) -> Result<(), TransportError> {
    conn.send(json!({"error": {"code": code, "message": message}}))
        .await
}

enum ControlAction {
    Input(Vec<u8>),
    Resize { cols: u16, rows: u16 },
    Scroll(i32),
    Release,
}

fn parse_control_frame(frame: &Value) -> Result<ControlAction, StreamError> {
    if frame.get("release").and_then(Value::as_bool) == Some(true)
        || frame.get("input").and_then(Value::as_str) == Some("release")
    {
        return Ok(ControlAction::Release);
    }
    if let Some(input) = frame.get("input").and_then(Value::as_object) {
        let encoded = input
            .get("data_base64")
            .and_then(Value::as_str)
            .ok_or_else(|| StreamError::InvalidFrame("input.data_base64 is required".to_owned()))?;
        return decode_base64(encoded).map(ControlAction::Input);
    }
    if frame.get("input").and_then(Value::as_str) == Some("bytes") {
        let encoded = frame
            .get("data_base64")
            .and_then(Value::as_str)
            .ok_or_else(|| StreamError::InvalidFrame("data_base64 is required".to_owned()))?;
        return decode_base64(encoded).map(ControlAction::Input);
    }
    if let Some(resize) = frame.get("resize").and_then(Value::as_object) {
        return parse_resize(resize.get("cols"), resize.get("rows"));
    }
    if frame.get("input").and_then(Value::as_str) == Some("resize") {
        return parse_resize(frame.get("cols"), frame.get("rows"));
    }
    if let Some(scroll) = frame.get("scroll").and_then(Value::as_object) {
        return parse_scroll(scroll.get("delta"));
    }
    if frame.get("input").and_then(Value::as_str) == Some("scroll") {
        return parse_scroll(frame.get("delta"));
    }
    Err(StreamError::InvalidFrame(
        "expected input, resize, scroll, or release".to_owned(),
    ))
}

fn parse_resize(cols: Option<&Value>, rows: Option<&Value>) -> Result<ControlAction, StreamError> {
    let cols = cols
        .and_then(Value::as_u64)
        .and_then(|value| u16::try_from(value).ok())
        .filter(|value| *value > 0)
        .ok_or_else(|| StreamError::InvalidFrame("resize.cols must be a positive u16".to_owned()))?;
    let rows = rows
        .and_then(Value::as_u64)
        .and_then(|value| u16::try_from(value).ok())
        .filter(|value| *value > 0)
        .ok_or_else(|| StreamError::InvalidFrame("resize.rows must be a positive u16".to_owned()))?;
    Ok(ControlAction::Resize { cols, rows })
}

fn parse_scroll(delta: Option<&Value>) -> Result<ControlAction, StreamError> {
    let delta = delta
        .and_then(Value::as_i64)
        .and_then(|value| i32::try_from(value).ok())
        .ok_or_else(|| StreamError::InvalidFrame("scroll.delta must be an i32".to_owned()))?;
    Ok(ControlAction::Scroll(delta))
}

pub fn encode_base64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] =
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut encoded = String::with_capacity(bytes.len().saturating_add(2) / 3 * 4);
    for chunk in bytes.chunks(3) {
        let first = chunk[0];
        let second = chunk.get(1).copied().unwrap_or(0);
        let third = chunk.get(2).copied().unwrap_or(0);
        encoded.push(TABLE[(first >> 2) as usize] as char);
        encoded.push(TABLE[(((first & 0x03) << 4) | (second >> 4)) as usize] as char);
        if chunk.len() > 1 {
            encoded.push(TABLE[(((second & 0x0f) << 2) | (third >> 6)) as usize] as char);
        } else {
            encoded.push('=');
        }
        if chunk.len() > 2 {
            encoded.push(TABLE[(third & 0x3f) as usize] as char);
        } else {
            encoded.push('=');
        }
    }
    encoded
}

pub fn decode_base64(encoded: &str) -> Result<Vec<u8>, StreamError> {
    if encoded.len() % 4 != 0 {
        return Err(StreamError::InvalidFrame(
            "data_base64 has invalid length".to_owned(),
        ));
    }
    let bytes = encoded.as_bytes();
    let mut decoded = Vec::with_capacity(encoded.len() / 4 * 3);
    for (chunk_index, chunk) in bytes.chunks_exact(4).enumerate() {
        let last = chunk_index + 1 == bytes.len() / 4;
        let a = base64_value(chunk[0])?;
        let b = base64_value(chunk[1])?;
        let c_padding = chunk[2] == b'=';
        let d_padding = chunk[3] == b'=';
        if (!last && (c_padding || d_padding)) || (c_padding && !d_padding) {
            return Err(StreamError::InvalidFrame(
                "data_base64 has invalid padding".to_owned(),
            ));
        }
        let c = if c_padding { 0 } else { base64_value(chunk[2])? };
        let d = if d_padding { 0 } else { base64_value(chunk[3])? };
        decoded.push((a << 2) | (b >> 4));
        if !c_padding {
            decoded.push((b << 4) | (c >> 2));
        }
        if !d_padding {
            decoded.push((c << 6) | d);
        }
    }
    Ok(decoded)
}

fn base64_value(byte: u8) -> Result<u8, StreamError> {
    match byte {
        b'A'..=b'Z' => Ok(byte - b'A'),
        b'a'..=b'z' => Ok(byte - b'a' + 26),
        b'0'..=b'9' => Ok(byte - b'0' + 52),
        b'+' => Ok(62),
        b'/' => Ok(63),
        _ => Err(StreamError::InvalidFrame(
            "data_base64 contains an invalid character".to_owned(),
        )),
    }
}
