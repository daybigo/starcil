use serde_json::{json, Value};
use starcil_platform::{InMemoryTransport, Transport, TransportFrame};
use starcil_server::actor::SharedServer;
use starcil_server::hosttraits::{
    HostError, ReadFormat, ReadSource, ScrollInfo, TerminalHost, TerminalReadout, TerminalSpawn,
};
use starcil_server::streams::{
    decode_base64, encode_base64, serve_attach, serve_control, serve_observe, LeaseRegistry,
    StreamRequest, TerminalOutput, TerminalStreamHost,
};
use starcil_server::ServerCore;
use starcil_testkit::FakeHost;
use std::sync::Arc;
use std::time::Duration;

struct StreamFakeHost {
    inner: FakeHost,
    raw_writes: Vec<(String, Vec<u8>)>,
    scrolls: Vec<(String, i32)>,
}

impl StreamFakeHost {
    fn new() -> Self {
        Self {
            inner: FakeHost::new(),
            raw_writes: Vec::new(),
            scrolls: Vec::new(),
        }
    }
}

impl TerminalHost for StreamFakeHost {
    fn spawn(&mut self, spec: TerminalSpawn) -> Result<String, HostError> {
        self.inner.spawn(spec)
    }

    fn kill(&mut self, terminal_id: &str) -> Result<(), HostError> {
        self.inner.kill(terminal_id)
    }

    fn is_alive(&self, terminal_id: &str) -> bool {
        self.inner.is_alive(terminal_id)
    }

    fn write_text(&mut self, terminal_id: &str, text: &str) -> Result<(), HostError> {
        self.inner.write_text(terminal_id, text)
    }

    fn write_enter(&mut self, terminal_id: &str) -> Result<(), HostError> {
        self.inner.write_enter(terminal_id)
    }

    fn write_keys(&mut self, terminal_id: &str, keys: &[String]) -> Result<(), HostError> {
        self.inner.write_keys(terminal_id, keys)
    }

    fn paste_text(&mut self, terminal_id: &str, text: &str) -> Result<(), HostError> {
        self.inner.paste_text(terminal_id, text)
    }

    fn resize(&mut self, terminal_id: &str, cols: u16, rows: u16) -> Result<(), HostError> {
        self.inner.resize(terminal_id, cols, rows)
    }

    fn read(
        &self,
        terminal_id: &str,
        source: ReadSource,
        lines: usize,
        format: ReadFormat,
    ) -> Result<TerminalReadout, HostError> {
        self.inner.read(terminal_id, source, lines, format)
    }

    fn scroll_info(&self, terminal_id: &str) -> Option<ScrollInfo> {
        self.inner.scroll_info(terminal_id)
    }

    fn terminal_title(&self, terminal_id: &str) -> Option<String> {
        self.inner.terminal_title(terminal_id)
    }

    fn change_seq(&self, terminal_id: &str) -> u64 {
        self.inner.change_seq(terminal_id)
    }

    fn process_info(&self, terminal_id: &str) -> Result<Value, HostError> {
        self.inner.process_info(terminal_id)
    }

    fn take_frame(&mut self, terminal_id: &str, snapshot: bool) -> Option<Value> {
        self.inner.take_frame(terminal_id, snapshot)
    }
}

impl TerminalStreamHost for StreamFakeHost {
    fn stream_size(&self, terminal_id: &str) -> Result<(u16, u16), HostError> {
        self.inner
            .terminals
            .get(terminal_id)
            .map(|terminal| terminal.size)
            .ok_or_else(|| HostError::NotFound(terminal_id.to_owned()))
    }

    fn write_stream_bytes(&mut self, terminal_id: &str, bytes: &[u8]) -> Result<(), HostError> {
        if !self.inner.terminals.contains_key(terminal_id) {
            return Err(HostError::NotFound(terminal_id.to_owned()));
        }
        self.raw_writes
            .push((terminal_id.to_owned(), bytes.to_vec()));
        Ok(())
    }

    fn scroll_stream(&mut self, terminal_id: &str, delta: i32) -> Result<(), HostError> {
        if !self.inner.terminals.contains_key(terminal_id) {
            return Err(HostError::NotFound(terminal_id.to_owned()));
        }
        self.scrolls.push((terminal_id.to_owned(), delta));
        Ok(())
    }

    fn subscribe_stream_output(
        &self,
        terminal_id: &str,
    ) -> Result<Option<Box<dyn TerminalOutput>>, HostError> {
        if !self.inner.terminals.contains_key(terminal_id) {
            return Err(HostError::NotFound(terminal_id.to_owned()));
        }
        Ok(None)
    }
}

fn stream_server() -> (SharedServer<StreamFakeHost>, String) {
    let core = ServerCore::new("streams", "C:/workspace", StreamFakeHost::new())
        .expect("stream core");
    let terminal_id = core
        .model
        .pane("w1:p1".parse().unwrap())
        .unwrap()
        .terminal_id
        .clone();
    (SharedServer::new(core), terminal_id)
}

async fn recv_with_timeout(conn: &mut InMemoryTransport) -> Value {
    tokio::time::timeout(Duration::from_secs(1), conn.recv())
        .await
        .expect("stream frame timeout")
        .expect("transport")
        .expect("stream closed")
}

#[tokio::test]
async fn observe_emits_base64_screen_changes_and_closed_record() {
    let (server, terminal_id) = stream_server();
    {
        let mut core = server.core.lock().unwrap();
        core.host.inner.set_screen(&terminal_id, "first screen");
    }
    let (mut client, mut server_conn) = InMemoryTransport::pair(1024 * 1024);
    let task_server = server.clone();
    let task = tokio::spawn(async move {
        serve_observe(
            &task_server,
            &mut server_conn,
            StreamRequest::new("w1:p1"),
        )
        .await
    });

    let header = recv_with_timeout(&mut client).await;
    assert_eq!(header["observe"]["terminal_id"], terminal_id);
    assert_eq!(header["observe"]["cols"], 120);
    assert_eq!(header["observe"]["rows"], 40);
    let initial = recv_with_timeout(&mut client).await;
    assert_eq!(
        decode_base64(initial["data_base64"].as_str().unwrap()).unwrap(),
        b"first screen"
    );

    {
        let mut core = server.core.lock().unwrap();
        core.host.inner.set_screen(&terminal_id, "second screen");
    }
    let changed = recv_with_timeout(&mut client).await;
    assert_eq!(
        decode_base64(changed["data_base64"].as_str().unwrap()).unwrap(),
        b"second screen"
    );
    {
        let mut core = server.core.lock().unwrap();
        core.host.kill(&terminal_id).unwrap();
    }
    assert_eq!(recv_with_timeout(&mut client).await, json!({"terminal": "closed"}));
    task.await.unwrap().unwrap();
}

#[tokio::test]
async fn control_conflict_takeover_and_release_follow_generation_lease() {
    let (server, terminal_id) = stream_server();
    let leases = Arc::new(LeaseRegistry::default());

    let (mut client_one, mut server_one) = InMemoryTransport::pair(1024 * 1024);
    let first_server = server.clone();
    let first_leases = Arc::clone(&leases);
    let first = tokio::spawn(async move {
        serve_control(
            &first_server,
            &mut server_one,
            first_leases,
            StreamRequest::new("w1:p1"),
        )
        .await
    });
    let _ = recv_with_timeout(&mut client_one).await;
    let _ = recv_with_timeout(&mut client_one).await;
    assert!(leases.has_controller(&terminal_id));

    let (mut conflict_client, mut conflict_server_conn) = InMemoryTransport::pair(1024 * 1024);
    let conflict_task_server = server.clone();
    let conflict_leases = Arc::clone(&leases);
    let conflict = tokio::spawn(async move {
        serve_control(
            &conflict_task_server,
            &mut conflict_server_conn,
            conflict_leases,
            StreamRequest::new("w1:p1"),
        )
        .await
    });
    let conflict_frame = recv_with_timeout(&mut conflict_client).await;
    assert_eq!(conflict_frame["error"]["code"], "lease_conflict");
    conflict.await.unwrap().unwrap();

    let (mut takeover_client, mut takeover_server_conn) = InMemoryTransport::pair(1024 * 1024);
    let takeover_task_server = server.clone();
    let takeover_leases = Arc::clone(&leases);
    let mut takeover_request = StreamRequest::new("w1:p1");
    takeover_request.takeover = true;
    let takeover = tokio::spawn(async move {
        serve_control(
            &takeover_task_server,
            &mut takeover_server_conn,
            takeover_leases,
            takeover_request,
        )
        .await
    });
    let _ = recv_with_timeout(&mut takeover_client).await;
    let _ = recv_with_timeout(&mut takeover_client).await;
    let revoked = recv_with_timeout(&mut client_one).await;
    assert_eq!(revoked["error"]["code"], "lease_revoked");
    first.await.unwrap().unwrap();

    takeover_client
        .send(json!({"input": {"data_base64": encode_base64(b"raw input")}}))
        .await
        .unwrap();
    takeover_client
        .send(json!({"resize": {"cols": 90, "rows": 30}}))
        .await
        .unwrap();
    takeover_client
        .send(json!({"scroll": {"delta": 4}}))
        .await
        .unwrap();
    takeover_client
        .send(json!({"release": true}))
        .await
        .unwrap();
    let released = recv_with_timeout(&mut takeover_client).await;
    assert_eq!(released["released"], true);
    takeover.await.unwrap().unwrap();
    assert!(!leases.has_controller(&terminal_id));
    {
        let core = server.core.lock().unwrap();
        assert_eq!(core.host.raw_writes, vec![(terminal_id.clone(), b"raw input".to_vec())]);
        assert_eq!(core.host.inner.terminal(&terminal_id).size, (90, 30));
        assert_eq!(core.host.scrolls, vec![(terminal_id.clone(), 4)]);
    }

    let (mut final_client, mut final_server_conn) = InMemoryTransport::pair(1024 * 1024);
    let final_task_server = server.clone();
    let final_leases = Arc::clone(&leases);
    let final_task = tokio::spawn(async move {
        serve_control(
            &final_task_server,
            &mut final_server_conn,
            final_leases,
            StreamRequest::new("w1:p1"),
        )
        .await
    });
    let header = recv_with_timeout(&mut final_client).await;
    assert_eq!(header["observe"]["terminal_id"], terminal_id);
    let _ = recv_with_timeout(&mut final_client).await;
    final_client.send(json!({"release": true})).await.unwrap();
    let _ = recv_with_timeout(&mut final_client).await;
    final_task.await.unwrap().unwrap();
}

#[tokio::test]
async fn attach_sends_rendered_snapshot_and_forwards_raw_input() {
    let (server, terminal_id) = stream_server();
    {
        let mut core = server.core.lock().unwrap();
        core.host.inner.set_screen(&terminal_id, "attach snapshot");
    }
    let leases = Arc::new(LeaseRegistry::default());
    let (mut client, mut server_conn) = InMemoryTransport::pair(1024 * 1024);
    client.enable_direct_raw_framing();
    let task_server = server.clone();
    let task_leases = Arc::clone(&leases);
    let task = tokio::spawn(async move {
        serve_attach(
            &task_server,
            &mut server_conn,
            task_leases,
            StreamRequest::new("w1:p1"),
        )
        .await
    });

    let initial = tokio::time::timeout(Duration::from_secs(1), client.recv_frame())
        .await
        .expect("attach snapshot timeout")
        .expect("transport")
        .expect("attach closed");
    let TransportFrame::Raw(initial) = initial else {
        panic!("attach snapshot must use raw framing");
    };
    assert!(initial.starts_with(b"\x1b[2J\x1b[H"));
    assert!(initial.ends_with(b"attach snapshot"));
    client.send_raw(b"\0raw\x1binput").await.unwrap();
    drop(client);
    task.await.unwrap().unwrap();

    let core = server.core.lock().unwrap();
    assert_eq!(
        core.host.raw_writes,
        vec![(terminal_id.clone(), b"\0raw\x1binput".to_vec())]
    );
    assert!(!leases.has_controller(&terminal_id));
}
