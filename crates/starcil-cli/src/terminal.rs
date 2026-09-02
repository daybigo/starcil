use crate::connection::{transport_endpoint_for, Connection, EndpointSelection};
use crate::TerminalAction;
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, size};
use serde_json::{json, Value};
use starcil_platform::{
    Transport, TransportError, TransportFrame, TransportHandle, DEFAULT_MAX_FRAME_SIZE,
};
use starcil_protocol::attach::{ClientMode, Hello, HelloBody};
use starcil_protocol::{Incoming, Request, PROTOCOL_MAJOR, PROTOCOL_MINOR};
use std::future::Future;
use std::io::{self, BufRead, Read, Write};
use std::pin::Pin;
use std::thread;
use tokio::sync::mpsc;

const DETACH_PREFIX: u8 = 0x02;

pub(crate) fn run_terminal<F>(
    session: Option<String>,
    action: TerminalAction,
    connector: &mut F,
) -> io::Result<()>
where
    F: FnMut(&EndpointSelection) -> io::Result<Box<dyn Connection>>,
{
    let selection = EndpointSelection { session };
    let action = match action {
        TerminalAction::AgentAttach { target, takeover } => TerminalAction::Attach {
            terminal_id: resolve_agent_terminal(&selection, &target, connector)?,
            takeover,
        },
        action => action,
    };
    let endpoint = transport_endpoint_for(&selection)?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;

    runtime.block_on(async move {
        let mut transport = connect_stream(&endpoint).await?;
        match action {
            TerminalAction::Observe { target, cols, rows } => {
                let hello = hello_frame(ClientMode::TerminalObserve, target, cols, rows, None)?;
                let stdout = io::stdout();
                let mut output = stdout.lock();
                pump_observe(&mut transport, hello, &mut output).await
            }
            TerminalAction::Control { target, takeover, cols, rows } => {
                let hello = hello_frame(
                    ClientMode::TerminalControl,
                    target,
                    cols,
                    rows,
                    Some(takeover),
                )?;
                let input = spawn_control_input();
                let stdout = io::stdout();
                let mut output = stdout.lock();
                pump_control(&mut transport, hello, input, &mut output).await
            }
            TerminalAction::Attach { terminal_id, takeover } => {
                let terminal_size = size().ok();
                let hello = hello_frame(
                    ClientMode::TerminalAttach,
                    terminal_id,
                    terminal_size.map(|(cols, _)| cols),
                    terminal_size.map(|(_, rows)| rows),
                    Some(takeover),
                )?;
                let _raw_mode = RawModeGuard::enter()?;
                let input = spawn_raw_input();
                let stdout = io::stdout();
                let mut output = stdout.lock();
                pump_raw(&mut transport, hello, input, &mut output).await
            }
            TerminalAction::AgentAttach { .. } => unreachable!("agent attach is resolved above"),
        }
    })
}

fn resolve_agent_terminal<F>(
    selection: &EndpointSelection,
    target: &str,
    connector: &mut F,
) -> io::Result<String>
where
    F: FnMut(&EndpointSelection) -> io::Result<Box<dyn Connection>>,
{
    let mut connection = connector(selection)?;
    let request = Request::new(
        "cli:agent:get-for-attach",
        "agent.get",
        json!({"target": target}),
    );
    match connection.call(&request)? {
        Incoming::Success(response) => response
            .result
            .get("agent")
            .and_then(|agent| agent.get("terminal_id"))
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "agent.get response is missing agent.terminal_id",
                )
            }),
        Incoming::Error(response) => Err(io::Error::new(
            io::ErrorKind::NotFound,
            response.error.to_string(),
        )),
        Incoming::Event(_) => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "agent.get returned an event instead of a response",
        )),
    }
}

#[cfg(windows)]
async fn connect_stream(
    endpoint: &starcil_platform::TransportEndpoint,
) -> io::Result<TransportHandle> {
    starcil_platform::connect_named_pipe(endpoint, DEFAULT_MAX_FRAME_SIZE)
        .await
        .map_err(transport_io_error)
}

#[cfg(unix)]
async fn connect_stream(
    endpoint: &starcil_platform::TransportEndpoint,
) -> io::Result<TransportHandle> {
    let path = match endpoint {
        starcil_platform::TransportEndpoint::UnixSocket { path, .. } => path,
        other => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("endpoint is not a Unix socket: {other}"),
            ));
        }
    };
    let stream = tokio::net::UnixStream::connect(path).await?;
    Ok(starcil_platform::spawn_stream_transport(
        stream,
        DEFAULT_MAX_FRAME_SIZE,
    ))
}

#[cfg(not(any(windows, unix)))]
async fn connect_stream(
    _endpoint: &starcil_platform::TransportEndpoint,
) -> io::Result<TransportHandle> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "terminal streams are unsupported on this platform",
    ))
}

fn hello_frame(
    mode: ClientMode,
    target: String,
    cols: Option<u16>,
    rows: Option<u16>,
    takeover: Option<bool>,
) -> io::Result<Value> {
    serde_json::to_value(Hello {
        hello: HelloBody {
            protocol_major: PROTOCOL_MAJOR,
            protocol_minor: PROTOCOL_MINOR,
            version: env!("CARGO_PKG_VERSION").to_owned(),
            mode,
            capabilities: Vec::new(),
            cols,
            rows,
            takeover,
            target: Some(target),
        },
    })
    .map_err(invalid_data)
}

trait ClientStreamTransport: Transport {
    fn enable_client_raw_framing(&self);

    fn send_client_raw<'a>(
        &'a mut self,
        payload: &'a [u8],
    ) -> Pin<Box<dyn Future<Output = Result<(), TransportError>> + Send + 'a>>;

    fn recv_client_frame<'a>(
        &'a mut self,
    ) -> Pin<
        Box<
            dyn Future<Output = Result<Option<TransportFrame>, TransportError>> + Send + 'a,
        >,
    >;
}

impl ClientStreamTransport for TransportHandle {
    fn enable_client_raw_framing(&self) {
        self.enable_direct_raw_framing();
    }

    fn send_client_raw<'a>(
        &'a mut self,
        payload: &'a [u8],
    ) -> Pin<Box<dyn Future<Output = Result<(), TransportError>> + Send + 'a>> {
        Box::pin(async move { self.send_raw(payload).await })
    }

    fn recv_client_frame<'a>(
        &'a mut self,
    ) -> Pin<
        Box<
            dyn Future<Output = Result<Option<TransportFrame>, TransportError>> + Send + 'a,
        >,
    > {
        Box::pin(async move { self.recv_frame().await })
    }
}

async fn pump_observe<T, W>(
    transport: &mut T,
    hello: Value,
    output: &mut W,
) -> io::Result<()>
where
    T: ClientStreamTransport,
    W: Write,
{
    transport.send(hello).await.map_err(transport_io_error)?;
    while let Some(frame) = transport.recv().await.map_err(transport_io_error)? {
        emit_ndjson(output, &frame)?;
    }
    Ok(())
}

#[derive(Debug)]
enum InputMessage {
    Frame(Value),
    Bytes(Vec<u8>),
    Error(String),
    Eof,
}

async fn pump_control<T, W>(
    transport: &mut T,
    hello: Value,
    mut input: mpsc::UnboundedReceiver<InputMessage>,
    output: &mut W,
) -> io::Result<()>
where
    T: ClientStreamTransport,
    W: Write,
{
    transport.send(hello).await.map_err(transport_io_error)?;
    let mut input_open = true;
    loop {
        tokio::select! {
            incoming = transport.recv() => {
                match incoming.map_err(transport_io_error)? {
                    Some(frame) => emit_ndjson(output, &frame)?,
                    None => return Ok(()),
                }
            }
            message = input.recv(), if input_open => {
                match message {
                    Some(InputMessage::Frame(frame)) => {
                        transport.send(frame).await.map_err(transport_io_error)?;
                    }
                    Some(InputMessage::Error(message)) => {
                        return Err(io::Error::new(io::ErrorKind::InvalidInput, message));
                    }
                    Some(InputMessage::Eof) | None => {
                        transport
                            .send(json!({"release": true}))
                            .await
                            .map_err(transport_io_error)?;
                        input_open = false;
                    }
                    Some(InputMessage::Bytes(_)) => unreachable!("raw input in control pump"),
                }
            }
        }
    }
}

async fn pump_raw<T, W>(
    transport: &mut T,
    hello: Value,
    mut input: mpsc::UnboundedReceiver<InputMessage>,
    output: &mut W,
) -> io::Result<()>
where
    T: ClientStreamTransport,
    W: Write,
{
    // This only switches the local reader. Enabling it before Hello removes the
    // race where the server can answer with raw bytes immediately after Hello.
    transport.enable_client_raw_framing();
    transport.send(hello).await.map_err(transport_io_error)?;

    let mut detach_keys = DetachKeys::default();
    loop {
        tokio::select! {
            incoming = transport.recv_client_frame() => {
                match incoming.map_err(transport_io_error)? {
                    Some(TransportFrame::Raw(bytes)) => {
                        output.write_all(&bytes)?;
                        output.flush()?;
                    }
                    Some(TransportFrame::Json(frame)) => emit_ndjson(output, &frame)?,
                    None => return Ok(()),
                }
            }
            message = input.recv() => {
                match message {
                    Some(InputMessage::Bytes(bytes)) => {
                        let decoded = detach_keys.feed(&bytes);
                        if !decoded.bytes.is_empty() {
                            transport
                                .send_client_raw(&decoded.bytes)
                                .await
                                .map_err(transport_io_error)?;
                        }
                        if decoded.detach {
                            return Ok(());
                        }
                    }
                    Some(InputMessage::Error(message)) => {
                        return Err(io::Error::new(io::ErrorKind::Other, message));
                    }
                    Some(InputMessage::Eof) | None => {
                        if let Some(prefix) = detach_keys.finish() {
                            transport
                                .send_client_raw(&[prefix])
                                .await
                                .map_err(transport_io_error)?;
                        }
                        return Ok(());
                    }
                    Some(InputMessage::Frame(_)) => unreachable!("NDJSON input in raw pump"),
                }
            }
        }
    }
}

fn spawn_control_input() -> mpsc::UnboundedReceiver<InputMessage> {
    let (sender, receiver) = mpsc::unbounded_channel();
    thread::spawn(move || {
        let stdin = io::stdin();
        read_control_input(stdin.lock(), &sender);
    });
    receiver
}

fn read_control_input<R: BufRead>(
    mut input: R,
    sender: &mpsc::UnboundedSender<InputMessage>,
) {
    let mut line = String::new();
    loop {
        line.clear();
        match input.read_line(&mut line) {
            Ok(0) => {
                let _ = sender.send(InputMessage::Eof);
                return;
            }
            Ok(_) => match decode_ndjson_line(&line) {
                Ok(Some(frame)) => {
                    if sender.send(InputMessage::Frame(frame)).is_err() {
                        return;
                    }
                }
                Ok(None) => {}
                Err(error) => {
                    let _ = sender.send(InputMessage::Error(error.to_string()));
                    return;
                }
            },
            Err(error) => {
                let _ = sender.send(InputMessage::Error(error.to_string()));
                return;
            }
        }
    }
}

fn spawn_raw_input() -> mpsc::UnboundedReceiver<InputMessage> {
    let (sender, receiver) = mpsc::unbounded_channel();
    thread::spawn(move || {
        let stdin = io::stdin();
        let mut input = stdin.lock();
        let mut buffer = [0_u8; 4096];
        loop {
            match input.read(&mut buffer) {
                Ok(0) => {
                    let _ = sender.send(InputMessage::Eof);
                    return;
                }
                Ok(length) => {
                    if sender
                        .send(InputMessage::Bytes(buffer[..length].to_vec()))
                        .is_err()
                    {
                        return;
                    }
                }
                Err(error) => {
                    let _ = sender.send(InputMessage::Error(error.to_string()));
                    return;
                }
            }
        }
    });
    receiver
}

fn emit_ndjson<W: Write>(output: &mut W, frame: &Value) -> io::Result<()> {
    let encoded = encode_ndjson_frame(frame)?;
    output.write_all(&encoded)?;
    output.flush()
}

fn encode_ndjson_frame(frame: &Value) -> io::Result<Vec<u8>> {
    let mut encoded = serde_json::to_vec(frame).map_err(invalid_data)?;
    encoded.push(b'\n');
    Ok(encoded)
}

fn decode_ndjson_line(line: &str) -> io::Result<Option<Value>> {
    let line = line.trim();
    if line.is_empty() {
        return Ok(None);
    }
    serde_json::from_str(line).map(Some).map_err(invalid_data)
}

fn invalid_data(error: serde_json::Error) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, error)
}

fn transport_io_error(error: TransportError) -> io::Error {
    io::Error::new(io::ErrorKind::Other, error)
}

struct RawModeGuard;

impl RawModeGuard {
    fn enter() -> io::Result<Self> {
        enable_raw_mode()?;
        Ok(Self)
    }
}

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
    }
}

#[derive(Debug, Default)]
struct DetachKeys {
    prefix_pending: bool,
}

#[derive(Debug, PartialEq, Eq)]
struct DetachResult {
    bytes: Vec<u8>,
    detach: bool,
}

impl DetachKeys {
    fn feed(&mut self, input: &[u8]) -> DetachResult {
        let mut bytes = Vec::with_capacity(input.len());
        let mut detach = false;
        for &byte in input {
            if self.prefix_pending {
                self.prefix_pending = false;
                match byte {
                    b'q' => {
                        detach = true;
                        break;
                    }
                    DETACH_PREFIX => bytes.push(DETACH_PREFIX),
                    other => {
                        bytes.push(DETACH_PREFIX);
                        bytes.push(other);
                    }
                }
            } else if byte == DETACH_PREFIX {
                self.prefix_pending = true;
            } else {
                bytes.push(byte);
            }
        }
        DetachResult { bytes, detach }
    }

    fn finish(&mut self) -> Option<u8> {
        std::mem::take(&mut self.prefix_pending).then_some(DETACH_PREFIX)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use starcil_platform::InMemoryTransport;

    impl ClientStreamTransport for InMemoryTransport {
        fn enable_client_raw_framing(&self) {
            self.enable_direct_raw_framing();
        }

        fn send_client_raw<'a>(
            &'a mut self,
            payload: &'a [u8],
        ) -> Pin<Box<dyn Future<Output = Result<(), TransportError>> + Send + 'a>> {
            Box::pin(async move { self.send_raw(payload).await })
        }

        fn recv_client_frame<'a>(
            &'a mut self,
        ) -> Pin<
            Box<
                dyn Future<Output = Result<Option<TransportFrame>, TransportError>> + Send + 'a,
            >,
        > {
            Box::pin(async move { self.recv_frame().await })
        }
    }

    #[test]
    fn ndjson_and_hello_frames_encode_and_decode() {
        let hello = hello_frame(
            ClientMode::TerminalControl,
            "w1:p2".to_owned(),
            Some(120),
            Some(40),
            Some(true),
        )
        .expect("hello frame");
        assert_eq!(
            hello,
            json!({
                "hello": {
                    "protocol_major": 1,
                    "protocol_minor": 0,
                    "version": env!("CARGO_PKG_VERSION"),
                    "mode": "terminal-control",
                    "cols": 120,
                    "rows": 40,
                    "takeover": true,
                    "target": "w1:p2"
                }
            })
        );

        let frame = json!({"input": {"data_base64": "YQ=="}});
        let encoded = encode_ndjson_frame(&frame).expect("encode frame");
        assert_eq!(encoded.last(), Some(&b'\n'));
        let decoded = decode_ndjson_line(std::str::from_utf8(&encoded).unwrap())
            .expect("decode frame")
            .expect("non-empty frame");
        assert_eq!(decoded, frame);
        assert_eq!(decode_ndjson_line(" \r\n").unwrap(), None);
    }

    #[test]
    fn detach_keys_work_across_chunks_and_escape_the_prefix() {
        let mut keys = DetachKeys::default();
        assert_eq!(
            keys.feed(b"abc\x02"),
            DetachResult {
                bytes: b"abc".to_vec(),
                detach: false
            }
        );
        assert_eq!(
            keys.feed(b"\x02q"),
            DetachResult {
                bytes: vec![DETACH_PREFIX, b'q'],
                detach: false
            }
        );
        assert_eq!(
            keys.feed(b"\x02"),
            DetachResult {
                bytes: Vec::new(),
                detach: false
            }
        );
        assert_eq!(
            keys.feed(b"qignored"),
            DetachResult {
                bytes: Vec::new(),
                detach: true
            }
        );
    }

    #[test]
    fn pending_detach_prefix_is_forwarded_at_eof() {
        let mut keys = DetachKeys::default();
        assert!(!keys.feed(&[DETACH_PREFIX]).detach);
        assert_eq!(keys.finish(), Some(DETACH_PREFIX));
        assert_eq!(keys.finish(), None);
    }

    #[tokio::test]
    async fn observe_pump_streams_ndjson_over_in_memory_transport() {
        let (mut client, mut server) = InMemoryTransport::pair(DEFAULT_MAX_FRAME_SIZE);
        let server_task = tokio::spawn(async move {
            let hello = server.recv().await.unwrap().unwrap();
            assert_eq!(hello["hello"]["mode"], "terminal-observe");
            server
                .send(json!({"observe": {"terminal_id": "term_1", "cols": 80, "rows": 24}}))
                .await
                .unwrap();
            server.send(json!({"terminal": "closed"})).await.unwrap();
        });

        let hello = hello_frame(
            ClientMode::TerminalObserve,
            "w1:p1".to_owned(),
            Some(80),
            Some(24),
            None,
        )
        .unwrap();
        let mut output = Vec::new();
        pump_observe(&mut client, hello, &mut output).await.unwrap();
        server_task.await.unwrap();

        let lines = String::from_utf8(output).unwrap();
        assert_eq!(lines.lines().count(), 2);
        assert!(lines.contains(r#""terminal_id":"term_1""#));
        assert!(lines.contains(r#""terminal":"closed""#));
    }

    #[tokio::test]
    async fn control_pump_forwards_frames_and_releases_on_stdin_eof() {
        let (mut client, mut server) = InMemoryTransport::pair(DEFAULT_MAX_FRAME_SIZE);
        let (sender, input) = mpsc::unbounded_channel();
        sender
            .send(InputMessage::Frame(json!({"input": {"data_base64": "YQ=="}})))
            .unwrap();
        sender.send(InputMessage::Eof).unwrap();
        drop(sender);

        let server_task = tokio::spawn(async move {
            let hello = server.recv().await.unwrap().unwrap();
            assert_eq!(hello["hello"]["mode"], "terminal-control");
            assert_eq!(
                server.recv().await.unwrap().unwrap(),
                json!({"input": {"data_base64": "YQ=="}})
            );
            assert_eq!(server.recv().await.unwrap().unwrap(), json!({"release": true}));
            server.send(json!({"released": true})).await.unwrap();
        });

        let hello = hello_frame(
            ClientMode::TerminalControl,
            "term_1".to_owned(),
            None,
            None,
            Some(false),
        )
        .unwrap();
        let mut output = Vec::new();
        pump_control(&mut client, hello, input, &mut output)
            .await
            .unwrap();
        server_task.await.unwrap();
        assert_eq!(
            decode_ndjson_line(std::str::from_utf8(&output).unwrap()).unwrap(),
            Some(json!({"released": true}))
        );
    }

    #[tokio::test]
    async fn raw_pump_uses_in_memory_transport_and_detaches_locally() {
        let (mut client, mut server) = InMemoryTransport::pair(DEFAULT_MAX_FRAME_SIZE);
        let (sender, input) = mpsc::unbounded_channel();
        sender
            .send(InputMessage::Bytes(b"abc\x02\x02".to_vec()))
            .unwrap();
        sender
            .send(InputMessage::Bytes(b"\x02q".to_vec()))
            .unwrap();
        drop(sender);

        let server_task = tokio::spawn(async move {
            let hello = server.recv().await.unwrap().unwrap();
            assert_eq!(hello["hello"]["mode"], "terminal-attach");
            assert_eq!(
                server.recv_frame().await.unwrap(),
                Some(TransportFrame::Raw(b"abc\x02".to_vec()))
            );
        });

        let hello = hello_frame(
            ClientMode::TerminalAttach,
            "term_1".to_owned(),
            None,
            None,
            Some(false),
        )
        .unwrap();
        let mut output = Vec::new();
        pump_raw(&mut client, hello, input, &mut output)
            .await
            .unwrap();
        server_task.await.unwrap();
        assert!(output.is_empty());
    }
}
