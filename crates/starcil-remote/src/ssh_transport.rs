use std::{
    env,
    ffi::{OsStr, OsString},
    future::Future,
    path::{Path, PathBuf},
    pin::Pin,
    process::{ExitStatus, Stdio},
    task::{Context, Poll},
};

use serde_json::Value;
use starcil_platform::{
    spawn_stream_transport, Transport, TransportError, TransportHandle,
};
use starcil_protocol::MAX_FRAME_BYTES;
use thiserror::Error;
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, ReadBuf},
    process::{Child, ChildStderr, Command},
    task::JoinHandle,
};

use crate::{RemoteTarget, SshConfigError, SshConfigManager};

pub const REMOTE_BINARY_ENV: &str = "STARCIL_REMOTE_BINARY";

#[derive(Debug, Clone)]
pub struct SshConnectOptions {
    pub target: RemoteTarget,
    pub session: Option<String>,
    pub runtime_dir: PathBuf,
    pub user_ssh_config: Option<PathBuf>,
    pub manage_ssh_config: bool,
    pub ssh_program: PathBuf,
    pub ssh_prefix_args: Vec<OsString>,
    pub remote_binary: Option<OsString>,
    pub command_env: Vec<(OsString, OsString)>,
}

impl SshConnectOptions {
    pub fn new(target: RemoteTarget, runtime_dir: impl Into<PathBuf>) -> Self {
        Self {
            target,
            session: None,
            runtime_dir: runtime_dir.into(),
            user_ssh_config: discover_user_ssh_config(),
            manage_ssh_config: true,
            ssh_program: PathBuf::from("ssh"),
            ssh_prefix_args: Vec::new(),
            remote_binary: None,
            command_env: Vec::new(),
        }
    }

    pub fn with_session(mut self, session: impl Into<String>) -> Self {
        self.session = Some(session.into());
        self
    }

    pub fn with_manage_ssh_config(mut self, managed: bool) -> Self {
        self.manage_ssh_config = managed;
        self
    }

    pub fn with_user_ssh_config(mut self, path: Option<PathBuf>) -> Self {
        self.user_ssh_config = path;
        self
    }

    pub fn with_ssh_program(mut self, program: impl Into<PathBuf>) -> Self {
        self.ssh_program = program.into();
        self
    }

    pub fn with_ssh_prefix_args<I, S>(mut self, arguments: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        self.ssh_prefix_args = arguments.into_iter().map(Into::into).collect();
        self
    }

    pub fn with_remote_binary(mut self, binary: impl Into<OsString>) -> Self {
        self.remote_binary = Some(binary.into());
        self
    }

    pub fn with_command_env(
        mut self,
        name: impl Into<OsString>,
        value: impl Into<OsString>,
    ) -> Self {
        self.command_env.push((name.into(), value.into()));
        self
    }
}

pub struct SshTransport {
    transport: TransportHandle,
    child: Child,
    stderr_task: JoinHandle<()>,
    _ssh_config: SshConfigManager,
    broken: bool,
    broken_reason: Option<String>,
}

impl SshTransport {
    pub async fn connect(options: SshConnectOptions) -> Result<Self, SshTransportError> {
        let ssh_config = SshConfigManager::create(
            &options.runtime_dir,
            options.user_ssh_config.as_deref(),
            options.manage_ssh_config,
        )?;
        let remote_binary = options
            .remote_binary
            .clone()
            .or_else(|| env::var_os(REMOTE_BINARY_ENV).filter(|value| !value.is_empty()))
            .unwrap_or_else(|| OsString::from("starcil"));
        let arguments = command_arguments(&options, ssh_config.config_path(), &remote_binary);

        let mut command = Command::new(&options.ssh_program);
        command
            .args(arguments)
            .envs(options.command_env.iter().cloned())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut child = command.spawn().map_err(|source| SshTransportError::Spawn {
            program: options.ssh_program.clone(),
            source,
        })?;
        let stdin = child.stdin.take().ok_or(SshTransportError::MissingPipe("stdin"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or(SshTransportError::MissingPipe("stdout"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or(SshTransportError::MissingPipe("stderr"))?;
        let stderr_task = tokio::spawn(drain_stderr(stderr));
        let stream = DuplexAdapter::new(stdout, stdin);
        let transport = spawn_stream_transport(stream, MAX_FRAME_BYTES);

        Ok(Self {
            transport,
            child,
            stderr_task,
            _ssh_config: ssh_config,
            broken: false,
            broken_reason: None,
        })
    }

    pub fn exit_status(&mut self) -> Result<Option<ExitStatus>, SshTransportError> {
        let status = self
            .child
            .try_wait()
            .map_err(|source| SshTransportError::ChildIo(source.to_string()))?;
        if let Some(status) = status {
            self.mark_broken(format!("ssh exited with {status}"));
        }
        Ok(status)
    }

    pub fn is_broken(&mut self) -> bool {
        if self.broken {
            return true;
        }
        match self.child.try_wait() {
            Ok(Some(status)) => self.mark_broken(format!("ssh exited with {status}")),
            Ok(None) => {}
            Err(error) => self.mark_broken(format!("could not inspect ssh child: {error}")),
        }
        self.broken
    }

    pub fn broken_reason(&self) -> Option<&str> {
        self.broken_reason.as_deref()
    }

    pub async fn terminate(&mut self) -> Result<ExitStatus, SshTransportError> {
        if self.child.try_wait().map_err(child_io)?.is_none() {
            self.child.start_kill().map_err(child_io)?;
        }
        let status = self.child.wait().await.map_err(child_io)?;
        self.mark_broken(format!("ssh exited with {status}"));
        self.stderr_task.abort();
        Ok(status)
    }

    fn mark_broken(&mut self, reason: String) {
        self.broken = true;
        if self.broken_reason.is_none() {
            self.broken_reason = Some(reason);
        }
    }
}

impl Transport for SshTransport {
    fn send<'a>(
        &'a mut self,
        frame: Value,
    ) -> Pin<Box<dyn Future<Output = Result<(), TransportError>> + Send + 'a>> {
        Box::pin(async move {
            if self.is_broken() {
                return Err(TransportError::Closed);
            }
            let result = self.transport.send(frame).await;
            if let Err(error) = &result {
                self.mark_broken(error.to_string());
            }
            result
        })
    }

    fn recv<'a>(
        &'a mut self,
    ) -> Pin<Box<dyn Future<Output = Result<Option<Value>, TransportError>> + Send + 'a>> {
        Box::pin(async move {
            let result = self.transport.recv().await;
            match &result {
                Ok(None) => self.mark_broken("ssh stdout closed".to_owned()),
                Err(error) => self.mark_broken(error.to_string()),
                Ok(Some(_)) => {}
            }
            result
        })
    }
}

impl Drop for SshTransport {
    fn drop(&mut self) {
        let _ = self.child.start_kill();
        self.stderr_task.abort();
    }
}

fn child_io(source: std::io::Error) -> SshTransportError {
    SshTransportError::ChildIo(source.to_string())
}

fn command_arguments(
    options: &SshConnectOptions,
    managed_config: Option<&Path>,
    remote_binary: &OsStr,
) -> Vec<OsString> {
    let mut arguments = options.ssh_prefix_args.clone();
    if let Some(config) = managed_config {
        arguments.push(OsString::from("-F"));
        arguments.push(config.as_os_str().to_owned());
    }
    arguments.push(OsString::from("-T"));
    arguments.push(OsString::from(options.target.as_str()));
    arguments.push(OsString::from("--"));
    arguments.push(remote_binary.to_owned());
    arguments.push(OsString::from("bridge"));
    arguments.push(OsString::from("--stdio"));
    if let Some(session) = options.session.as_deref() {
        arguments.push(OsString::from("--session"));
        arguments.push(OsString::from(session));
    }
    arguments
}

fn discover_user_ssh_config() -> Option<PathBuf> {
    env::var_os("USERPROFILE")
        .or_else(|| env::var_os("HOME"))
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .map(|home| home.join(".ssh").join("config"))
}

async fn drain_stderr(mut stderr: ChildStderr) {
    let mut buffer = [0_u8; 4096];
    loop {
        match stderr.read(&mut buffer).await {
            Ok(0) => return,
            Ok(length) => {
                let message = String::from_utf8_lossy(&buffer[..length]);
                tracing::debug!(
                    target: "starcil_remote::ssh",
                    message = %message.trim_end(),
                    "ssh stderr"
                );
            }
            Err(error) => {
                tracing::debug!(target: "starcil_remote::ssh", %error, "ssh stderr closed");
                return;
            }
        }
    }
}

struct DuplexAdapter<R, W> {
    reader: R,
    writer: W,
}

impl<R, W> DuplexAdapter<R, W> {
    fn new(reader: R, writer: W) -> Self {
        Self { reader, writer }
    }
}

impl<R, W> AsyncRead for DuplexAdapter<R, W>
where
    R: AsyncRead + Unpin,
    W: Unpin,
{
    fn poll_read(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.get_mut().reader).poll_read(context, buffer)
    }
}

impl<R, W> AsyncWrite for DuplexAdapter<R, W>
where
    R: Unpin,
    W: AsyncWrite + Unpin,
{
    fn poll_write(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &[u8],
    ) -> Poll<Result<usize, std::io::Error>> {
        Pin::new(&mut self.get_mut().writer).poll_write(context, buffer)
    }

    fn poll_flush(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
    ) -> Poll<Result<(), std::io::Error>> {
        Pin::new(&mut self.get_mut().writer).poll_flush(context)
    }

    fn poll_shutdown(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
    ) -> Poll<Result<(), std::io::Error>> {
        Pin::new(&mut self.get_mut().writer).poll_shutdown(context)
    }
}

#[derive(Debug, Error)]
pub enum SshTransportError {
    #[error(transparent)]
    Config(#[from] SshConfigError),
    #[error("could not spawn ssh program `{program}`: {source}")]
    Spawn {
        program: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("spawned ssh child did not expose piped {0}")]
    MissingPipe(&'static str),
    #[error("ssh child I/O failed: {0}")]
    ChildIo(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn managed_command_contract_includes_config_target_and_named_session() {
        let options = SshConnectOptions::new(
            RemoteTarget::parse("dev@buildbox").unwrap(),
            PathBuf::from("runtime"),
        )
        .with_ssh_prefix_args(["pre-arg"])
        .with_session("agents");
        let arguments = command_arguments(
            &options,
            Some(Path::new("runtime/managed.conf")),
            OsStr::new("custom-starcil"),
        );
        assert_eq!(
            arguments,
            [
                "pre-arg",
                "-F",
                "runtime/managed.conf",
                "-T",
                "dev@buildbox",
                "--",
                "custom-starcil",
                "bridge",
                "--stdio",
                "--session",
                "agents",
            ]
            .map(OsString::from)
        );
    }

    #[test]
    fn plain_command_omits_managed_config_without_disabling_user_ssh() {
        let options = SshConnectOptions::new(
            RemoteTarget::parse("buildbox").unwrap(),
            PathBuf::from("runtime"),
        )
        .with_manage_ssh_config(false);
        let arguments = command_arguments(&options, None, OsStr::new("starcil"));
        assert_eq!(
            arguments,
            ["-T", "buildbox", "--", "starcil", "bridge", "--stdio"]
                .map(OsString::from)
        );
    }
}
