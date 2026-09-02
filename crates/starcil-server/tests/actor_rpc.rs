//! Async RPC layer over an in-memory transport: request/response, waits,
//! event subscription push — no pipes, no PTYs.

use serde_json::{json, Value};
use starcil_platform::{InMemoryTransport, Transport, DEFAULT_MAX_FRAME_SIZE};
use starcil_server::actor::{handle_connection, SharedServer};
use starcil_server::ServerCore;
use starcil_testkit::FakeHost;

fn boot() -> SharedServer<FakeHost> {
    SharedServer::new(ServerCore::new("default", "C:/dev/proj", FakeHost::new()).unwrap())
}

async fn rpc(client: &mut impl Transport, id: &str, method: &str, params: Value) -> Value {
    client
        .send(json!({"id": id, "method": method, "params": params}))
        .await
        .unwrap();
    loop {
        let line = client.recv().await.unwrap().expect("connection open");
        if line.get("id").and_then(Value::as_str) == Some(id) {
            return line;
        }
    }
}

#[tokio::test]
async fn ping_split_and_agent_flow_over_transport() {
    let server = boot();
    let (mut client, server_side) = InMemoryTransport::pair(DEFAULT_MAX_FRAME_SIZE);
    let conn_server = server.clone();
    let task = tokio::spawn(async move { handle_connection(conn_server, server_side).await });

    let r = rpc(&mut client, "t1", "ping", json!({})).await;
    assert_eq!(r["result"]["type"], "pong");

    let r = rpc(&mut client, "t2", "pane.split", json!({"pane_id": "w1:p1", "direction": "right"})).await;
    assert_eq!(r["result"]["pane"]["pane_id"], "w1:p2");

    let r = rpc(
        &mut client,
        "t3",
        "pane.report_agent",
        json!({"pane_id": "w1:p2", "source": "starcil:claude", "agent": "claude", "state": "working"}),
    )
    .await;
    assert_eq!(r["result"]["accepted"], true);

    // agent.wait resolves when a later report settles the agent.
    let waiter_server = server.clone();
    let waiter = tokio::spawn(async move {
        let (mut wclient, wserver) = InMemoryTransport::pair(DEFAULT_MAX_FRAME_SIZE);
        let conn = waiter_server.clone();
        tokio::spawn(async move { handle_connection(conn, wserver).await });
        rpc(
            &mut wclient,
            "w1",
            "agent.wait",
            json!({"target": "w1:p2", "timeout_ms": 10000}),
        )
        .await
    });
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    let _ = rpc(
        &mut client,
        "t4",
        "pane.report_agent",
        json!({"pane_id": "w1:p2", "source": "starcil:claude", "agent": "claude", "state": "blocked"}),
    )
    .await;
    let waited = waiter.await.unwrap();
    assert_eq!(waited["result"]["outcome"], "reached");
    assert_eq!(waited["result"]["state"], "blocked");

    drop(client);
    let _ = tokio::time::timeout(std::time::Duration::from_secs(2), task).await;
}

/// `agent.wait` is a standalone wait: an agent that already sits in a target
/// state settles at once. Only `agent.prompt --wait` carries the 5s stall rule
/// (a prompt that changes nothing was not processed).
#[tokio::test]
async fn standalone_wait_settles_on_an_already_idle_agent_but_prompt_stalls() {
    let server = boot();
    let (mut client, server_side) = InMemoryTransport::pair(DEFAULT_MAX_FRAME_SIZE);
    let conn = server.clone();
    tokio::spawn(async move { handle_connection(conn, server_side).await });

    let _ = rpc(&mut client, "s1", "pane.split", json!({"pane_id": "w1:p1", "direction": "right"})).await;
    let r = rpc(
        &mut client,
        "s2",
        "pane.report_agent",
        json!({"pane_id": "w1:p2", "source": "starcil:claude", "agent": "claude", "state": "idle"}),
    )
    .await;
    assert_eq!(r["result"]["accepted"], true);

    let started = std::time::Instant::now();
    let waited = rpc(&mut client, "s3", "agent.wait", json!({"target": "w1:p2", "timeout_ms": 10000})).await;
    assert_eq!(waited["result"]["outcome"], "reached", "{waited}");
    assert_eq!(waited["result"]["state"], "idle");
    assert!(started.elapsed() < std::time::Duration::from_secs(4), "must not wait for the stall window");

    let prompted = rpc(
        &mut client,
        "s4",
        "agent.prompt",
        json!({"target": "w1:p2", "text": "hello", "wait": {"timeout_ms": 8000}}),
    )
    .await;
    assert_eq!(prompted["error"]["code"], "agent_prompt_stalled", "{prompted}");
}

/// `agent.start` types the command and then waits for the agent to settle,
/// reporting the startup outcome instead of failing when it never does.
#[tokio::test]
async fn agent_start_reports_its_startup_outcome() {
    let server = boot();
    let (mut client, server_side) = InMemoryTransport::pair(DEFAULT_MAX_FRAME_SIZE);
    let conn = server.clone();
    tokio::spawn(async move { handle_connection(conn, server_side).await });

    let _ = rpc(&mut client, "a1", "pane.split", json!({"pane_id": "w1:p1", "direction": "right"})).await;
    // Nothing ever draws on the fake host: the wait times out, the start still succeeds.
    let quiet = rpc(
        &mut client,
        "a2",
        "agent.start",
        json!({"name": "quiet", "kind": "codex", "pane_id": "w1:p2", "timeout_ms": 400}),
    )
    .await;
    assert_eq!(quiet["result"]["type"], "agent_started", "{quiet}");
    assert_eq!(quiet["result"]["startup"]["outcome"], "timeout");
    assert_eq!(quiet["result"]["agent"]["name"], "quiet");

    let _ = rpc(&mut client, "a3", "pane.split", json!({"pane_id": "w1:p1", "direction": "down"})).await;
    let reporter = server.clone();
    tokio::spawn(async move {
        let (mut rclient, rserver) = InMemoryTransport::pair(DEFAULT_MAX_FRAME_SIZE);
        let conn = reporter.clone();
        tokio::spawn(async move { handle_connection(conn, rserver).await });
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        rpc(
            &mut rclient,
            "r1",
            "pane.report_agent",
            json!({"pane_id": "w1:p3", "source": "starcil:claude", "agent": "claude", "state": "idle"}),
        )
        .await
    });
    let ready = rpc(
        &mut client,
        "a4",
        "agent.start",
        json!({"name": "ready", "kind": "claude", "pane_id": "w1:p3", "timeout_ms": 10000}),
    )
    .await;
    assert_eq!(ready["result"]["type"], "agent_started", "{ready}");
    assert_eq!(ready["result"]["startup"]["outcome"], "reached");
    assert_eq!(ready["result"]["startup"]["state"], "idle");
    assert_eq!(ready["result"]["agent"]["agent_status"], "idle");

    // An idle lifecycle with the shell back at its prompt is NOT a running
    // agent (the program was not installed, or died): the startup keeps waiting.
    let split = rpc(&mut client, "a5", "pane.split", json!({"pane_id": "w1:p1", "direction": "right"})).await;
    let bare_pane = split["result"]["pane"]["pane_id"].as_str().unwrap().to_string();
    let bare_term = split["result"]["pane"]["terminal_id"].as_str().unwrap().to_string();
    {
        let mut core = server.core.lock().unwrap();
        core.host.terminals.get_mut(&bare_term).unwrap().descendants = Some(vec![]);
    }
    let reporter = server.clone();
    let bare_for_report = bare_pane.clone();
    tokio::spawn(async move {
        let (mut rclient, rserver) = InMemoryTransport::pair(DEFAULT_MAX_FRAME_SIZE);
        let conn = reporter.clone();
        tokio::spawn(async move { handle_connection(conn, rserver).await });
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        rpc(
            &mut rclient,
            "r2",
            "pane.report_agent",
            json!({"pane_id": bare_for_report, "source": "starcil:claude", "agent": "claude", "state": "idle"}),
        )
        .await
    });
    let bare = rpc(
        &mut client,
        "a6",
        "agent.start",
        json!({"name": "bare", "kind": "claude", "pane_id": bare_pane, "timeout_ms": 900}),
    )
    .await;
    assert_eq!(bare["result"]["type"], "agent_started", "{bare}");
    assert_eq!(bare["result"]["startup"]["outcome"], "timeout", "{bare}");
    assert_eq!(bare["result"]["startup"]["state"], "idle");

    // The pane disappearing under the agent is reported as `exited`.
    let split = rpc(&mut client, "a7", "pane.split", json!({"pane_id": "w1:p1", "direction": "right"})).await;
    let doomed = split["result"]["pane"]["pane_id"].as_str().unwrap().to_string();
    let closer = server.clone();
    let doomed_for_close = doomed.clone();
    tokio::spawn(async move {
        let (mut cclient, cserver) = InMemoryTransport::pair(DEFAULT_MAX_FRAME_SIZE);
        let conn = closer.clone();
        tokio::spawn(async move { handle_connection(conn, cserver).await });
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        rpc(&mut cclient, "c1", "pane.close", json!({"pane_id": doomed_for_close})).await
    });
    let gone = rpc(
        &mut client,
        "a8",
        "agent.start",
        json!({"name": "gone", "kind": "codex", "pane_id": doomed, "timeout_ms": 10000}),
    )
    .await;
    assert_eq!(gone["result"]["type"], "agent_started", "{gone}");
    assert_eq!(gone["result"]["startup"]["outcome"], "exited", "{gone}");

    // A kind WITH screen rules is up only once a rule or a report recognized
    // its UI. The stability fallback calling a quiet screen `idle` does not
    // count, even with a process running under the shell. Reports on the root
    // pane stand in for the periodic tick (this harness runs no timer).
    let split = rpc(&mut client, "a9", "pane.split", json!({"pane_id": "w1:p1", "direction": "right"})).await;
    let quiet_pane = split["result"]["pane"]["pane_id"].as_str().unwrap().to_string();
    let quiet_term = split["result"]["pane"]["terminal_id"].as_str().unwrap().to_string();
    {
        let mut core = server.core.lock().unwrap();
        core.host.terminals.get_mut(&quiet_term).unwrap().descendants = Some(vec!["node".to_string()]);
    }
    let ticker = server.clone();
    tokio::spawn(async move {
        let (mut tclient, tserver) = InMemoryTransport::pair(DEFAULT_MAX_FRAME_SIZE);
        let conn = ticker.clone();
        tokio::spawn(async move { handle_connection(conn, tserver).await });
        for i in 0..5 {
            tokio::time::sleep(std::time::Duration::from_millis(150)).await;
            rpc(
                &mut tclient,
                &format!("k{i}"),
                "pane.report_agent",
                json!({"pane_id": "w1:p1", "source": "starcil:claude", "agent": "claude", "state": "idle"}),
            )
            .await;
        }
    });
    let unrecognized = rpc(
        &mut client,
        "a10",
        "agent.start",
        json!({"name": "unrec", "kind": "claude", "pane_id": quiet_pane, "timeout_ms": 900}),
    )
    .await;
    assert_eq!(unrecognized["result"]["startup"]["outcome"], "timeout", "{unrecognized}");
    assert_eq!(
        unrecognized["result"]["startup"]["state"], "idle",
        "the fallback did call the quiet screen idle: {unrecognized}"
    );

    // A kind WITHOUT screen rules cannot do better than the fallback: a
    // process under the shell plus a settled screen is accepted.
    let split = rpc(&mut client, "a11", "pane.split", json!({"pane_id": "w1:p1", "direction": "right"})).await;
    let plain_pane = split["result"]["pane"]["pane_id"].as_str().unwrap().to_string();
    let plain_term = split["result"]["pane"]["terminal_id"].as_str().unwrap().to_string();
    {
        let mut core = server.core.lock().unwrap();
        core.host.terminals.get_mut(&plain_term).unwrap().descendants = Some(vec!["pi".to_string()]);
    }
    let ticker = server.clone();
    tokio::spawn(async move {
        let (mut tclient, tserver) = InMemoryTransport::pair(DEFAULT_MAX_FRAME_SIZE);
        let conn = ticker.clone();
        tokio::spawn(async move { handle_connection(conn, tserver).await });
        for i in 0..5 {
            tokio::time::sleep(std::time::Duration::from_millis(150)).await;
            rpc(
                &mut tclient,
                &format!("m{i}"),
                "pane.report_agent",
                json!({"pane_id": "w1:p1", "source": "starcil:claude", "agent": "claude", "state": "idle"}),
            )
            .await;
        }
    });
    let plain = rpc(
        &mut client,
        "a12",
        "agent.start",
        json!({"name": "plain", "kind": "pi", "pane_id": plain_pane, "timeout_ms": 5000}),
    )
    .await;
    assert_eq!(plain["result"]["startup"]["outcome"], "reached", "{plain}");
    assert_eq!(plain["result"]["startup"]["state"], "idle");
}

#[tokio::test]
async fn unknown_method_returns_error_envelope() {
    let server = boot();
    let (mut client, server_side) = InMemoryTransport::pair(DEFAULT_MAX_FRAME_SIZE);
    let conn = server.clone();
    tokio::spawn(async move { handle_connection(conn, server_side).await });
    let r = rpc(&mut client, "e1", "pane.explode", json!({})).await;
    assert_eq!(r["error"]["code"], "unknown_method");
}

#[tokio::test]
async fn events_subscription_pushes_matching_events() {
    let server = boot();

    // Subscriber connection.
    let (mut sub, sub_side) = InMemoryTransport::pair(DEFAULT_MAX_FRAME_SIZE);
    let conn = server.clone();
    tokio::spawn(async move { handle_connection(conn, sub_side).await });
    sub.send(json!({
        "id": "sub1",
        "method": "events.subscribe",
        "params": {"subscriptions": [{"type": "pane.created"}]}
    }))
    .await
    .unwrap();
    let ack = sub.recv().await.unwrap().unwrap();
    assert_eq!(ack["result"]["type"], "subscribed");

    // Actor connection triggers a split.
    let (mut client, server_side) = InMemoryTransport::pair(DEFAULT_MAX_FRAME_SIZE);
    let conn = server.clone();
    tokio::spawn(async move { handle_connection(conn, server_side).await });
    let _ = rpc(&mut client, "s1", "pane.split", json!({"pane_id": "w1:p1", "direction": "down"})).await;

    let pushed = tokio::time::timeout(std::time::Duration::from_secs(2), sub.recv())
        .await
        .expect("event within 2s")
        .unwrap()
        .unwrap();
    assert_eq!(pushed["event"], "pane.created");
    assert_eq!(pushed["data"]["pane_id"], "w1:p2");
}

#[tokio::test]
async fn wait_for_output_matches_and_times_out() {
    let server = boot();
    {
        let mut core = server.core.lock().unwrap();
        let term = core.model.pane("w1:p1".parse().unwrap()).unwrap().terminal_id.clone();
        core.host.set_screen(&term, "building...\ntests passed: 42");
    }
    let (mut client, server_side) = InMemoryTransport::pair(DEFAULT_MAX_FRAME_SIZE);
    let conn = server.clone();
    tokio::spawn(async move { handle_connection(conn, server_side).await });

    let r = rpc(
        &mut client,
        "m1",
        "pane.wait_for_output",
        json!({"pane_id": "w1:p1", "match": "tests passed", "timeout_ms": 3000}),
    )
    .await;
    assert_eq!(r["result"]["matched"], "tests passed");

    let r = rpc(
        &mut client,
        "m2",
        "pane.wait_for_output",
        json!({"pane_id": "w1:p1", "regex": "never [a-z]+ appears", "timeout_ms": 300}),
    )
    .await;
    assert_eq!(r["error"]["code"], "timeout");

    let r = rpc(
        &mut client,
        "m3",
        "pane.wait_for_output",
        json!({"pane_id": "w1:p1", "match": "x", "regex": "y"}),
    )
    .await;
    assert_eq!(r["error"]["code"], "invalid_params");
}
