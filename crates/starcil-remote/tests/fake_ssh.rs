use std::env;

#[cfg(unix)]
use std::path::PathBuf;

use serde_json::json;
use starcil_platform::{Transport, TransportEndpoint};
use starcil_protocol::MAX_FRAME_BYTES;
use starcil_remote::{
    bridge_stdio_pump, RemoteTarget, SshConnectOptions, SshTransport,
};

const CHILD_ENV: &str = "STARCIL_FAKE_SSH_CHILD";

fn main() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("test runtime");
    if env::var_os(CHILD_ENV).is_some() {
        runtime.block_on(run_fake_ssh_child());
    } else {
        runtime.block_on(run_end_to_end_test());
    }
}

async fn run_fake_ssh_child() {
    let arguments = env::args().collect::<Vec<_>>();
    let session = arguments
        .windows(2)
        .find(|pair| pair[0] == "--session")
        .map(|pair| pair[1].as_str());
    bridge_stdio_pump(session)
        .await
        .expect("fake ssh bridge pump");
}

async fn run_end_to_end_test() {
    let temp = tempfile::tempdir().expect("temporary remote test directory");
    #[cfg(unix)]
    env::set_var("XDG_RUNTIME_DIR", temp.path());
    let session = format!("remote-fake-{}", std::process::id());
    let endpoint = TransportEndpoint::for_session(&session).expect("fake server endpoint");
    let server = spawn_echo_server(&endpoint).await;

    let current_exe = env::current_exe().expect("current fake ssh test executable");
    let options = SshConnectOptions::new(
        RemoteTarget::parse("fake-buildbox").expect("fake target"),
        temp.path().join("ssh-runtime"),
    )
    .with_manage_ssh_config(false)
    .with_ssh_program(current_exe)
    .with_session(&session)
    .with_command_env(CHILD_ENV, "1");
    let mut transport = SshTransport::connect(options)
        .await
        .expect("spawn fake ssh transport");
    let expected = json!({"fake_ssh": "bridge echo", "sequence": 1});
    transport.send(expected.clone()).await.expect("send echo");
    let received = transport
        .recv()
        .await
        .expect("receive echo")
        .expect("echo frame");
    assert_eq!(received, expected);

    server.await.expect("fake echo server task");
    let _ = transport.terminate().await.expect("stop fake ssh child");
}

#[cfg(windows)]
async fn spawn_echo_server(endpoint: &TransportEndpoint) -> tokio::task::JoinHandle<()> {
    let mut listener =
        starcil_platform::NamedPipeListener::bind(endpoint, MAX_FRAME_BYTES)
            .expect("bind fake named pipe");
    tokio::spawn(async move {
        let mut connection = listener.accept().await.expect("accept fake bridge");
        let frame = connection
            .recv()
            .await
            .expect("read bridge frame")
            .expect("bridge frame");
        connection.send(frame).await.expect("echo bridge frame");
    })
}

#[cfg(unix)]
async fn spawn_echo_server(endpoint: &TransportEndpoint) -> tokio::task::JoinHandle<()> {
    let path = PathBuf::from(endpoint.as_address());
    std::fs::create_dir_all(path.parent().expect("socket parent"))
        .expect("create fake socket parent");
    let listener = tokio::net::UnixListener::bind(&path).expect("bind fake Unix socket");
    tokio::spawn(async move {
        let (stream, _) = listener.accept().await.expect("accept fake bridge");
        let mut connection = starcil_platform::spawn_stream_transport(stream, MAX_FRAME_BYTES);
        let frame = connection
            .recv()
            .await
            .expect("read bridge frame")
            .expect("bridge frame");
        connection.send(frame).await.expect("echo bridge frame");
    })
}

#[cfg(not(any(windows, unix)))]
async fn spawn_echo_server(_endpoint: &TransportEndpoint) -> tokio::task::JoinHandle<()> {
    panic!("fake SSH integration test requires a local socket platform")
}
