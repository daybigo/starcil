use starcil_platform::{EndpointError, TransportEndpoint};
use starcil_protocol::MAX_FRAME_BYTES;
use thiserror::Error;
use tokio::io::{
    AsyncBufRead, AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader,
};

#[cfg(windows)]
const LOCAL_CONNECT_ATTEMPTS: usize = 100;
#[cfg(windows)]
const LOCAL_CONNECT_RETRY_MS: u64 = 20;

/// Remote-side entry point wired by `starcil bridge --stdio [--session NAME]`.
pub async fn bridge_stdio_pump(session: Option<&str>) -> Result<(), BridgeError> {
    let endpoint = TransportEndpoint::for_session(session.unwrap_or("default"))?;
    let stream = connect_local_endpoint(&endpoint).await?;
    bridge_stream_pump(
        tokio::io::stdin(),
        tokio::io::stdout(),
        stream,
        MAX_FRAME_BYTES,
    )
    .await
}

/// Pumps complete bounded NDJSON frames in both directions until either side closes.
/// Each direction owns exactly one writer and forwards bytes without UTF-8 conversion.
pub async fn bridge_stream_pump<I, O, S>(
    stdin: I,
    stdout: O,
    socket: S,
    max_frame_size: usize,
) -> Result<(), BridgeError>
where
    I: AsyncRead + Unpin,
    O: AsyncWrite + Unpin,
    S: AsyncRead + AsyncWrite + Unpin,
{
    let (socket_reader, socket_writer) = tokio::io::split(socket);
    let to_socket = copy_ndjson(
        BufReader::new(stdin),
        socket_writer,
        max_frame_size,
        "stdio to local socket",
    );
    let to_stdout = copy_ndjson(
        BufReader::new(socket_reader),
        stdout,
        max_frame_size,
        "local socket to stdio",
    );
    tokio::pin!(to_socket);
    tokio::pin!(to_stdout);
    tokio::select! {
        result = &mut to_socket => result,
        result = &mut to_stdout => result,
    }
}

async fn copy_ndjson<R, W>(
    mut reader: R,
    mut writer: W,
    max_frame_size: usize,
    direction: &'static str,
) -> Result<(), BridgeError>
where
    R: AsyncBufRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut frame = Vec::with_capacity(max_frame_size.min(8 * 1024));
    loop {
        if !read_bounded_frame(&mut reader, &mut frame, max_frame_size, direction).await? {
            writer
                .shutdown()
                .await
                .map_err(|source| BridgeError::Io { direction, source })?;
            return Ok(());
        }
        writer
            .write_all(&frame)
            .await
            .map_err(|source| BridgeError::Io { direction, source })?;
        writer
            .flush()
            .await
            .map_err(|source| BridgeError::Io { direction, source })?;
    }
}

async fn read_bounded_frame<R>(
    reader: &mut R,
    frame: &mut Vec<u8>,
    max_frame_size: usize,
    direction: &'static str,
) -> Result<bool, BridgeError>
where
    R: AsyncBufRead + Unpin,
{
    frame.clear();
    let max_frame_size = max_frame_size.max(1);
    loop {
        let available = reader
            .fill_buf()
            .await
            .map_err(|source| BridgeError::Io { direction, source })?;
        if available.is_empty() {
            return if frame.is_empty() {
                Ok(false)
            } else {
                Err(BridgeError::TruncatedFrame { direction })
            };
        }

        if let Some(newline) = available.iter().position(|byte| *byte == b'\n') {
            let payload_bytes = frame.len().saturating_add(newline);
            if payload_bytes > max_frame_size {
                return Err(BridgeError::FrameTooLarge {
                    direction,
                    max_bytes: max_frame_size,
                });
            }
            let consumed = newline + 1;
            frame.extend_from_slice(&available[..consumed]);
            reader.consume(consumed);
            return Ok(true);
        }

        if frame.len().saturating_add(available.len()) > max_frame_size {
            return Err(BridgeError::FrameTooLarge {
                direction,
                max_bytes: max_frame_size,
            });
        }
        let consumed = available.len();
        frame.extend_from_slice(available);
        reader.consume(consumed);
    }
}

#[cfg(windows)]
type LocalStream = tokio::net::windows::named_pipe::NamedPipeClient;

#[cfg(unix)]
type LocalStream = tokio::net::UnixStream;

#[cfg(windows)]
async fn connect_local_endpoint(
    endpoint: &TransportEndpoint,
) -> Result<LocalStream, BridgeError> {
    use tokio::net::windows::named_pipe::ClientOptions;

    let address = match endpoint {
        TransportEndpoint::WindowsPipe { .. } => endpoint.as_address(),
        other => return Err(BridgeError::UnsupportedEndpoint(other.as_address())),
    };
    for attempt in 0..LOCAL_CONNECT_ATTEMPTS {
        match ClientOptions::new().open(&address) {
            Ok(client) => return Ok(client),
            Err(_error) if attempt + 1 < LOCAL_CONNECT_ATTEMPTS => {
                tokio::time::sleep(std::time::Duration::from_millis(
                    LOCAL_CONNECT_RETRY_MS,
                ))
                .await;
            }
            Err(source) => {
                return Err(BridgeError::Connect {
                    endpoint: address,
                    source,
                });
            }
        }
    }
    unreachable!("the bounded local connection loop always returns")
}

#[cfg(unix)]
async fn connect_local_endpoint(
    endpoint: &TransportEndpoint,
) -> Result<LocalStream, BridgeError> {
    let path = match endpoint {
        TransportEndpoint::UnixSocket { path, .. } => path,
        other => return Err(BridgeError::UnsupportedEndpoint(other.as_address())),
    };
    tokio::net::UnixStream::connect(path)
        .await
        .map_err(|source| BridgeError::Connect {
            endpoint: path.display().to_string(),
            source,
        })
}

#[cfg(not(any(windows, unix)))]
struct LocalStream;

#[cfg(not(any(windows, unix)))]
async fn connect_local_endpoint(
    endpoint: &TransportEndpoint,
) -> Result<LocalStream, BridgeError> {
    Err(BridgeError::UnsupportedEndpoint(endpoint.as_address()))
}

#[derive(Debug, Error)]
pub enum BridgeError {
    #[error(transparent)]
    Endpoint(#[from] EndpointError),
    #[error("local endpoint `{0}` is unsupported on this platform")]
    UnsupportedEndpoint(String),
    #[error("could not connect to local Starcil endpoint `{endpoint}`: {source}")]
    Connect {
        endpoint: String,
        #[source]
        source: std::io::Error,
    },
    #[error("{direction} NDJSON frame exceeds the {max_bytes} byte limit")]
    FrameTooLarge {
        direction: &'static str,
        max_bytes: usize,
    },
    #[error("{direction} closed in the middle of an NDJSON frame")]
    TruncatedFrame { direction: &'static str },
    #[error("{direction} I/O failed: {source}")]
    Io {
        direction: &'static str,
        #[source]
        source: std::io::Error,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test]
    async fn pump_preserves_complete_ndjson_bytes_in_both_directions() {
        let (mut ssh_writer, bridge_stdin) = tokio::io::duplex(1024);
        let (bridge_stdout, mut ssh_reader) = tokio::io::duplex(1024);
        let (bridge_socket, mut server_socket) = tokio::io::duplex(1024);
        let pump = tokio::spawn(bridge_stream_pump(
            bridge_stdin,
            bridge_stdout,
            bridge_socket,
            128,
        ));

        let request = b"{\"request\":\"caf\\u00e9\"}\n";
        ssh_writer.write_all(request).await.unwrap();
        let mut observed_request = vec![0; request.len()];
        server_socket.read_exact(&mut observed_request).await.unwrap();
        assert_eq!(observed_request, request);

        let response = b"{\"result\":[0,255]}\n";
        server_socket.write_all(response).await.unwrap();
        let mut observed_response = vec![0; response.len()];
        ssh_reader.read_exact(&mut observed_response).await.unwrap();
        assert_eq!(observed_response, response);

        drop(ssh_writer);
        pump.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn pump_rejects_oversized_and_truncated_frames() {
        let (mut writer, input) = tokio::io::duplex(1024);
        let (output, _reader) = tokio::io::duplex(1024);
        let (socket, _peer) = tokio::io::duplex(1024);
        let pump = tokio::spawn(bridge_stream_pump(input, output, socket, 8));
        writer.write_all(b"123456789\n").await.unwrap();
        let error = pump.await.unwrap().unwrap_err();
        assert!(matches!(error, BridgeError::FrameTooLarge { max_bytes: 8, .. }));

        let (mut writer, input) = tokio::io::duplex(1024);
        let (output, _reader) = tokio::io::duplex(1024);
        let (socket, _peer) = tokio::io::duplex(1024);
        let pump = tokio::spawn(bridge_stream_pump(input, output, socket, 32));
        writer.write_all(b"{\"partial\":true}").await.unwrap();
        drop(writer);
        let error = pump.await.unwrap().unwrap_err();
        assert!(matches!(error, BridgeError::TruncatedFrame { .. }));
    }
}
