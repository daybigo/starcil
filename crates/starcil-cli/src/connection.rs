use starcil_protocol::{Incoming, Request, MAX_FRAME_BYTES};
use std::env;
use std::fs::OpenOptions;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::PathBuf;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EndpointSelection {
    pub session: Option<String>,
}

/// Resolve the endpoint in the required order: explicit session, socket
/// override, session environment variable, then the default session.
pub fn endpoint_for(selection: &EndpointSelection) -> PathBuf {
    resolve_endpoint(selection, non_empty_env).unwrap_or_default()
}

pub(crate) fn transport_endpoint_for(
    selection: &EndpointSelection,
) -> io::Result<starcil_platform::TransportEndpoint> {
    let endpoint = resolve_endpoint(selection, non_empty_env)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
    endpoint
        .to_string_lossy()
        .parse()
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))
}

fn resolve_endpoint<F>(selection: &EndpointSelection, read_env: F) -> Result<PathBuf, starcil_platform::EndpointError>
where
    F: Fn(&str) -> Option<String>,
{
    if let Some(session) = selection.session.as_deref() {
        return platform_session_endpoint(session);
    }
    if let Some(path) = read_env("STARCIL_SOCKET_PATH") {
        return Ok(PathBuf::from(path));
    }
    if let Some(session) = read_env("STARCIL_SESSION") {
        return platform_session_endpoint(&session);
    }
    platform_session_endpoint("default")
}

fn non_empty_env(name: &str) -> Option<String> {
    env::var(name).ok().filter(|value| !value.trim().is_empty())
}

fn platform_session_endpoint(session: &str) -> Result<PathBuf, starcil_platform::EndpointError> {
    starcil_platform::TransportEndpoint::for_session(session)
        .map(|endpoint| PathBuf::from(endpoint.as_address()))
}

/// Request-level transport seam. A future platform transport only needs to
/// implement this trait; the parser and dispatcher remain unchanged.
pub trait Connection {
    fn call(&mut self, request: &Request) -> io::Result<Incoming>;
}

pub struct NdjsonConnection<T> {
    io: BufReader<T>,
}

impl<T: Read + Write> NdjsonConnection<T> {
    pub fn new(io: T) -> Self {
        Self { io: BufReader::new(io) }
    }

    fn write_request(&mut self, request: &Request) -> io::Result<()> {
        let mut frame = serde_json::to_vec(request)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        frame.push(b'\n');
        if frame.len() > MAX_FRAME_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("request frame exceeds {MAX_FRAME_BYTES} bytes"),
            ));
        }
        let writer = self.io.get_mut();
        writer.write_all(&frame)?;
        writer.flush()
    }

    fn read_frame(&mut self) -> io::Result<Vec<u8>> {
        let mut frame = Vec::new();
        let bytes = self.io.read_until(b'\n', &mut frame)?;
        if bytes == 0 {
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "server closed the connection"));
        }
        if frame.len() > MAX_FRAME_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("response frame exceeds {MAX_FRAME_BYTES} bytes"),
            ));
        }
        while matches!(frame.last(), Some(b'\n' | b'\r')) {
            frame.pop();
        }
        Ok(frame)
    }
}

impl<T: Read + Write> Connection for NdjsonConnection<T> {
    fn call(&mut self, request: &Request) -> io::Result<Incoming> {
        self.write_request(request)?;
        loop {
            let frame = self.read_frame()?;
            let incoming: Incoming = serde_json::from_slice(&frame)
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
            match &incoming {
                Incoming::Success(response) if response.id == request.id => return Ok(incoming),
                Incoming::Error(response) if response.id == request.id => return Ok(incoming),
                Incoming::Success(_) | Incoming::Error(_) | Incoming::Event(_) => continue,
            }
        }
    }
}

pub(crate) fn connect(selection: &EndpointSelection) -> io::Result<Box<dyn Connection>> {
    let endpoint = resolve_endpoint(selection, non_empty_env)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
    connect_endpoint(endpoint)
}

#[cfg(windows)]
fn connect_endpoint(endpoint: PathBuf) -> io::Result<Box<dyn Connection>> {
    let pipe = OpenOptions::new().read(true).write(true).open(endpoint)?;
    Ok(Box::new(NdjsonConnection::new(pipe)))
}

#[cfg(unix)]
fn connect_endpoint(endpoint: PathBuf) -> io::Result<Box<dyn Connection>> {
    use std::os::unix::net::UnixStream;
    let socket = UnixStream::connect(endpoint)?;
    Ok(Box::new(NdjsonConnection::new(socket)))
}

#[cfg(not(any(windows, unix)))]
fn connect_endpoint(_endpoint: PathBuf) -> io::Result<Box<dyn Connection>> {
    Err(io::Error::new(io::ErrorKind::Unsupported, "local sockets are unsupported on this platform"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn platform_endpoint(session: &str) -> PathBuf {
        PathBuf::from(
            starcil_platform::TransportEndpoint::for_session(session)
                .expect("valid session")
                .as_address(),
        )
    }

    fn resolve_with(selection: &EndpointSelection, values: &[(&str, &str)]) -> PathBuf {
        let values = values
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect::<BTreeMap<_, _>>();
        resolve_endpoint(selection, |name| {
            values.get(name).cloned().filter(|value| !value.trim().is_empty())
        })
        .expect("endpoint resolves")
    }

    #[test]
    fn named_session_endpoint_is_exactly_the_platform_endpoint() {
        let actual = resolve_with(
            &EndpointSelection { session: Some("e2etest".to_owned()) },
            &[],
        );
        assert_eq!(actual, platform_endpoint("e2etest"));
    }

    #[test]
    fn endpoint_precedence_is_explicit_socket_env_session_env_default() {
        let explicit = resolve_with(
            &EndpointSelection { session: Some("explicit".to_owned()) },
            &[("STARCIL_SOCKET_PATH", "override.sock"), ("STARCIL_SESSION", "environment")],
        );
        assert_eq!(explicit, platform_endpoint("explicit"));

        let socket = resolve_with(
            &EndpointSelection::default(),
            &[("STARCIL_SOCKET_PATH", "override.sock"), ("STARCIL_SESSION", "environment")],
        );
        assert_eq!(socket, PathBuf::from("override.sock"));

        let environment = resolve_with(
            &EndpointSelection::default(),
            &[("STARCIL_SOCKET_PATH", "  "), ("STARCIL_SESSION", "environment")],
        );
        assert_eq!(environment, platform_endpoint("environment"));

        assert_eq!(resolve_with(&EndpointSelection::default(), &[]), platform_endpoint("default"));
    }

    #[test]
    fn invalid_session_never_falls_back_to_a_cli_derived_endpoint() {
        let error = resolve_endpoint(
            &EndpointSelection { session: Some("../escape".to_owned()) },
            |_| None,
        )
        .unwrap_err();
        assert!(matches!(error, starcil_platform::EndpointError::InvalidSession(_)));
    }
}
