//! Async server: owns the shared ServerCore, accepts socket clients, services
//! RPC requests, implements the wait family (agent.wait, agent.prompt --wait,
//! pane.wait_for_output, events.subscribe/wait), runs the periodic agent tick,
//! and fans events out to subscribers.

use crate::core::ServerCore;
use crate::hosttraits::{ReadFormat, ReadSource, TerminalHost};
use crate::persistence::PersistenceState;
use crate::streams::{LeaseRegistry, RawStreamTransport, StreamRequest, TerminalStreamHost};
use serde_json::{json, Value};
use starcil_agents::{AgentWait, LifecycleState, PromptWait, SystemClock, WaitConfig, WaitOutcome};
use starcil_domain::{AgentStatus, PaneId};
use starcil_platform::{NamedPipeListener, TransportEndpoint, DEFAULT_MAX_FRAME_SIZE};
#[allow(unused_imports)]
use starcil_platform::Transport;
use starcil_protocol::error::{ApiError, ErrorCode};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

const TICK_INTERVAL: Duration = Duration::from_millis(300);
const WAIT_POLL: Duration = Duration::from_millis(100);

pub struct SharedServer<H: TerminalHost> {
    pub core: Arc<Mutex<ServerCore<H>>>,
    pub events: broadcast::Sender<Value>,
    pub shutdown: CancellationToken,
    /// Session persistence (None in unit tests without a state file).
    pub persistence: Arc<Mutex<Option<PersistenceState>>>,
    /// Revision at the last dirty-mark, to detect structural changes.
    pub last_seen_revision: Arc<std::sync::atomic::AtomicU64>,
    /// Terminal stream control leases.
    pub leases: Arc<LeaseRegistry>,
}

impl<H: TerminalHost> Clone for SharedServer<H> {
    fn clone(&self) -> Self {
        SharedServer {
            core: self.core.clone(),
            events: self.events.clone(),
            shutdown: self.shutdown.clone(),
            persistence: self.persistence.clone(),
            last_seen_revision: self.last_seen_revision.clone(),
            leases: self.leases.clone(),
        }
    }
}

impl<H: TerminalHost> SharedServer<H> {
    pub fn new(core: ServerCore<H>) -> Self {
        let (events, _) = broadcast::channel(1024);
        SharedServer {
            core: Arc::new(Mutex::new(core)),
            events,
            shutdown: CancellationToken::new(),
            persistence: Arc::new(Mutex::new(None)),
            last_seen_revision: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            leases: Arc::new(LeaseRegistry::default()),
        }
    }

    /// Lock, run, drain pending events into the broadcast bus.
    pub fn call(&self, method: &str, params: &Value) -> Result<Value, ApiError> {
        let mut core = self.core.lock().expect("core lock poisoned");
        let out = core.handle(method, params);
        let revision = core.model.revision;
        let drained: Vec<_> = core.pending_events.drain(..).collect();
        for (event, data) in &drained {
            core.fan_out_plugin_event(event, data);
        }
        drop(core);
        for (event, data) in drained {
            let _ = self.events.send(json!({"event": event, "data": data, "revision": revision}));
        }
        out
    }

    pub fn agent_status(&self, pane: PaneId) -> Option<AgentStatus> {
        let core = self.core.lock().expect("core lock poisoned");
        core.agents.panes.get(&pane).map(|a| a.status)
    }

    fn tick(&self) {
        let mut core = self.core.lock().expect("core lock poisoned");
        core.tick_agents();
        // Persistence: mark dirty on revision change, save after the debounce.
        {
            use std::sync::atomic::Ordering;
            let rev = core.model.revision;
            let last = self.last_seen_revision.swap(rev, Ordering::Relaxed);
            if let Ok(mut guard) = self.persistence.lock() {
                if let Some(state) = guard.as_mut() {
                    if rev != last {
                        state.mark_dirty();
                    }
                    if let Err(e) = crate::persistence::save_if_dirty(state, &core) {
                        tracing::warn!(error = %e, "session state save failed");
                    }
                }
            }
        }
        let now = std::time::Instant::now();
        let mut expired = false;
        for m in core.pane_metadata.values_mut() {
            expired |= m.expire(now);
        }
        for m in core.workspace_metadata.values_mut() {
            expired |= m.expire(now);
        }
        let _ = expired;
        let revision = core.model.revision;
        let drained: Vec<_> = core.pending_events.drain(..).collect();
        for (event, data) in &drained {
            core.fan_out_plugin_event(event, data);
        }
        drop(core);
        for (event, data) in drained {
            let _ = self.events.send(json!({"event": event, "data": data, "revision": revision}));
        }
    }
}

fn status_from_str(s: &str) -> Option<LifecycleState> {
    Some(match s {
        "idle" => LifecycleState::Idle,
        "working" => LifecycleState::Working,
        "blocked" => LifecycleState::Blocked,
        "done" => LifecycleState::Done,
        "unknown" => LifecycleState::Unknown,
        _ => return None,
    })
}

fn to_lifecycle(s: AgentStatus) -> LifecycleState {
    match s {
        AgentStatus::Idle => LifecycleState::Idle,
        AgentStatus::Working => LifecycleState::Working,
        AgentStatus::Blocked => LifecycleState::Blocked,
        AgentStatus::Done => LifecycleState::Done,
        AgentStatus::Unknown => LifecycleState::Unknown,
    }
}

fn wait_config_from(params: &Value) -> Result<WaitConfig, ApiError> {
    let mut config = WaitConfig::default();
    if let Some(untils) = params.get("until").and_then(Value::as_array) {
        if !untils.is_empty() {
            let mut targets = Vec::new();
            for u in untils {
                let s = u
                    .as_str()
                    .ok_or_else(|| ApiError::invalid_params("until entries must be strings"))?;
                targets.push(
                    status_from_str(s)
                        .ok_or_else(|| ApiError::invalid_params(format!("invalid status `{s}`")))?,
                );
            }
            config.targets = targets;
        }
    }
    if let Some(ms) = params.get("timeout_ms").and_then(Value::as_u64) {
        config.timeout = Some(Duration::from_millis(ms));
    }
    Ok(config)
}

/// The two wait state machines behind the RPC wait family.
enum LifecycleWait {
    /// `agent.prompt --wait`: a prompt sent from a non-working state must
    /// produce a lifecycle change within 5s, otherwise it is reported stalled.
    AfterPrompt(PromptWait),
    /// `agent.wait` and `agent.start`: settles as soon as the state matches
    /// (immediately if it already does) and never stalls.
    Standalone(AgentWait),
}

impl LifecycleWait {
    fn poll(&mut self, clock: &SystemClock, state: LifecycleState) -> WaitOutcome {
        match self {
            LifecycleWait::AfterPrompt(wait) => wait.poll(clock, state),
            LifecycleWait::Standalone(wait) => wait.poll(clock, state),
        }
    }
}

/// Poll one pane's lifecycle until the wait settles (async, never holds the
/// core lock while sleeping). Never returns `Pending`.
async fn wait_lifecycle<H: TerminalHost>(
    server: &SharedServer<H>,
    pane: PaneId,
    config: WaitConfig,
    after_prompt: bool,
) -> WaitOutcome {
    let clock = SystemClock::default();
    let current = |server: &SharedServer<H>| {
        server
            .agent_status(pane)
            .map(to_lifecycle)
            .unwrap_or(LifecycleState::Unknown)
    };
    let mut wait = if after_prompt {
        LifecycleWait::AfterPrompt(PromptWait::start(&clock, current(server), config))
    } else {
        LifecycleWait::Standalone(AgentWait::start(&clock, config))
    };
    loop {
        match wait.poll(&clock, current(server)) {
            WaitOutcome::Pending => tokio::time::sleep(WAIT_POLL).await,
            settled => return settled,
        }
    }
}

/// Service one wait to completion. Returns the final wait payload or an
/// ApiError (`agent_prompt_stalled` only exists on the after-prompt path).
async fn run_agent_wait<H: TerminalHost>(
    server: &SharedServer<H>,
    pane: PaneId,
    config: WaitConfig,
    after_prompt: bool,
) -> Result<Value, ApiError> {
    match wait_lifecycle(server, pane, config, after_prompt).await {
        WaitOutcome::Pending => unreachable!("wait_lifecycle only returns settled outcomes"),
        WaitOutcome::Reached { state, elapsed_ms } => Ok(json!({
            "type": "agent_wait",
            "outcome": "reached",
            "pane_id": pane.to_string(),
            "state": state,
            "elapsed_ms": elapsed_ms,
        })),
        WaitOutcome::Stalled { state, elapsed_ms } => Err(ApiError {
            code: ErrorCode::AgentPromptStalled,
            message: format!(
                "no lifecycle change observed within 5s (state {state:?}, {elapsed_ms}ms)"
            ),
            details: None,
        }),
        WaitOutcome::Timeout { state, elapsed_ms } => Err(ApiError {
            code: ErrorCode::Timeout,
            message: format!("wait timed out in state {state:?} after {elapsed_ms}ms"),
            details: None,
        }),
    }
}

/// Startup wait behind `agent.start`. The agent counts as up when its
/// lifecycle settled (idle, done, or blocked on a startup gate), something
/// runs under the pane's shell (when the host can tell), and the state came
/// from positive recognition — a screen rule or a hook report — unless the
/// kind has no screen rules at all. A shell back at its prompt means the
/// program never launched or already died. Outcomes: `reached`, `exited`
/// (the agent left the pane, or the pane closed), `timeout`.
async fn wait_startup<H: TerminalHost>(
    server: &SharedServer<H>,
    pane: PaneId,
    timeout: Duration,
) -> Value {
    let started = std::time::Instant::now();
    loop {
        let probe = {
            let core = server.core.lock().expect("core lock poisoned");
            core.startup_probe(pane)
        };
        let state = serde_json::to_value(probe.status).unwrap_or(Value::Null);
        let elapsed_ms = started.elapsed().as_millis() as u64;
        if !probe.present {
            return json!({"outcome": "exited", "state": state, "elapsed_ms": elapsed_ms});
        }
        let settled = matches!(
            probe.status,
            AgentStatus::Idle | AgentStatus::Done | AgentStatus::Blocked
        );
        if settled && probe.shell_idle != Some(true) && probe.recognized {
            return json!({"outcome": "reached", "state": state, "elapsed_ms": elapsed_ms});
        }
        if started.elapsed() >= timeout {
            return json!({"outcome": "timeout", "state": state, "elapsed_ms": elapsed_ms});
        }
        tokio::time::sleep(WAIT_POLL).await;
    }
}

async fn run_wait_for_output<H: TerminalHost>(
    server: &SharedServer<H>,
    params: &Value,
) -> Result<Value, ApiError> {
    let (pane, source, lines) = {
        let core = server.core.lock().expect("core lock poisoned");
        let pane = core.resolve_pane(params)?;
        let source = match params.get("source").and_then(Value::as_str).unwrap_or("recent-unwrapped") {
            "visible" => ReadSource::Visible,
            "recent" => ReadSource::Recent,
            "recent-unwrapped" => ReadSource::RecentUnwrapped,
            other => return Err(ApiError::invalid_params(format!("invalid source `{other}`"))),
        };
        let lines = params.get("lines").and_then(Value::as_u64).unwrap_or(0) as usize;
        (pane, source, lines)
    };
    let match_text = params.get("match").and_then(Value::as_str).map(str::to_string);
    let regex_pat = params.get("regex").and_then(Value::as_str).map(str::to_string);
    if match_text.is_some() == regex_pat.is_some() {
        return Err(ApiError::invalid_params("provide exactly one of `match` or `regex`"));
    }
    let regex = match &regex_pat {
        Some(p) => Some(
            regex_automata::meta::Regex::new(p)
                .map_err(|e| ApiError::invalid_params(format!("invalid regex: {e}")))?,
        ),
        None => None,
    };
    let timeout = params.get("timeout_ms").and_then(Value::as_u64).map(Duration::from_millis);
    let started = std::time::Instant::now();
    loop {
        let text = {
            let core = server.core.lock().expect("core lock poisoned");
            let term = core.model.pane(pane)?.terminal_id.clone();
            core.host
                .read(&term, source, lines, ReadFormat::Text)
                .map(|r| r.text)
                .unwrap_or_default()
        };
        let matched: Option<String> = if let Some(m) = &match_text {
            text.contains(m.as_str()).then(|| m.clone())
        } else if let Some(r) = &regex {
            r.find(text.as_bytes())
                .map(|m| String::from_utf8_lossy(&text.as_bytes()[m.range()]).to_string())
        } else {
            None
        };
        if let Some(matched) = matched {
            return Ok(json!({
                "type": "pane_output_matched",
                "pane_id": pane.to_string(),
                "matched": matched,
                "elapsed_ms": started.elapsed().as_millis() as u64,
            }));
        }
        if let Some(t) = timeout {
            if started.elapsed() >= t {
                return Err(ApiError::new(
                    ErrorCode::Timeout,
                    format!("no output match after {}ms", t.as_millis()),
                ));
            }
        }
        tokio::time::sleep(WAIT_POLL).await;
    }
}

/// Handle one request, including the async wait family. Returns the response
/// line to send (already JSON-encoded).
pub async fn service_request<H: TerminalHost>(server: &SharedServer<H>, line: Value) -> Option<String> {
    let req: starcil_protocol::Request = match serde_json::from_value(line) {
        Ok(r) => r,
        Err(e) => {
            return Some(starcil_protocol::failure(
                "unknown",
                ApiError::invalid_params(format!("malformed request: {e}")),
            ));
        }
    };
    let id = req.id.clone();
    let result: Result<Value, ApiError> = match req.method.as_str() {
        "server.stop" => {
            // Cancel AFTER the response has time to flush: an immediate cancel
            // tears the connection down before the ok reaches the client.
            let shutdown = server.shutdown.clone();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(200)).await;
                shutdown.cancel();
            });
            Ok(json!({"type": "ok"}))
        }
        "agent.wait" => {
            let target = req.params.get("target").and_then(Value::as_str).map(str::to_string);
            let pane = {
                let core = server.core.lock().expect("core lock poisoned");
                match target {
                    Some(t) => core.resolve_agent(&t),
                    None => Err(ApiError::invalid_params("missing `target`")),
                }
            };
            match pane {
                Ok(p) => match wait_config_from(&req.params) {
                    Ok(cfg) => run_agent_wait(server, p, cfg, false).await,
                    Err(e) => Err(e),
                },
                Err(e) => Err(e),
            }
        }
        "agent.start" => {
            // Type the launch command synchronously, then wait for the CLI to
            // come up (see `wait_startup`) so the caller knows whether the
            // agent is ready before its first prompt. `timeout_ms` bounds the
            // wait (0 = do not wait); timeout and exit are reported in
            // `startup`, not as errors: the command was typed and retrying
            // would launch the agent twice.
            match server.call("agent.start", &req.params) {
                Ok(mut started) => {
                    let timeout = req
                        .params
                        .get("timeout_ms")
                        .and_then(Value::as_u64)
                        .unwrap_or(30_000);
                    let name = req
                        .params
                        .get("name")
                        .and_then(Value::as_str)
                        .expect("validated by handler")
                        .to_string();
                    let pane = {
                        let core = server.core.lock().expect("core lock poisoned");
                        core.resolve_agent(&name)
                    };
                    if let Ok(pane) = pane {
                        started["startup"] =
                            wait_startup(server, pane, Duration::from_millis(timeout)).await;
                        let core = server.core.lock().expect("core lock poisoned");
                        if let Ok(info) = core.agent_info(pane) {
                            started["agent"] = info;
                        }
                    }
                    Ok(started)
                }
                Err(e) => Err(e),
            }
        }
        "agent.prompt" => {
            // Submit synchronously; honor the optional wait object.
            let wait_params = req.params.get("wait").cloned();
            let submitted = server.call("agent.prompt", &req.params);
            match (submitted, wait_params) {
                (Ok(v), None) => Ok(v),
                (Ok(_), Some(wp)) => {
                    let target = req
                        .params
                        .get("target")
                        .and_then(Value::as_str)
                        .expect("validated by handler")
                        .to_string();
                    let pane = {
                        let core = server.core.lock().expect("core lock poisoned");
                        core.resolve_agent(&target)
                    };
                    match pane {
                        Ok(p) => match wait_config_from(&wp) {
                            Ok(cfg) => run_agent_wait(server, p, cfg, true).await,
                            Err(e) => Err(e),
                        },
                        Err(e) => Err(e),
                    }
                }
                (Err(e), _) => Err(e),
            }
        }
        "pane.wait_for_output" => run_wait_for_output(server, &req.params).await,
        "events.wait" => {
            // One-shot: first event matching the subscriptions.
            let mut rx = server.events.subscribe();
            let subs = req.params.get("subscriptions").cloned().unwrap_or(json!([]));
            let timeout = req
                .params
                .get("timeout_ms")
                .and_then(Value::as_u64)
                .map(Duration::from_millis)
                .unwrap_or(Duration::from_secs(3600));
            let deadline = tokio::time::Instant::now() + timeout;
            loop {
                let ev = tokio::select! {
                    e = rx.recv() => e.ok(),
                    _ = tokio::time::sleep_until(deadline) => None,
                };
                match ev {
                    Some(ev) if event_matches(&subs, &ev) => {
                        break Ok(json!({"type": "event", "matched": ev}));
                    }
                    Some(_) => continue,
                    None => {
                        break Err(ApiError::new(ErrorCode::Timeout, "no matching event"));
                    }
                }
            }
        }
        _ => server.call(&req.method, &req.params),
    };
    Some(match result {
        Ok(v) => starcil_protocol::success(&id, v),
        Err(e) => starcil_protocol::failure(&id, e),
    })
}


/// Structural events that invalidate a TUI client's cached snapshot.
pub fn is_structural_event(name: &str) -> bool {
    matches!(
        name,
        "workspace.created"
            | "workspace.closed"
            | "workspace.focused"
            | "workspace.renamed"
            | "workspace.moved"
            | "workspace.reordered"
            | "tab.created"
            | "tab.closed"
            | "tab.focused"
            | "tab.renamed"
            | "tab.moved"
            | "pane.created"
            | "pane.closed"
            | "pane.focused"
            | "pane.moved"
            | "pane.updated"
            | "pane.agent_detected"
            | "pane.agent_exited"
            | "pane.agent_status_changed"
            | "layout.updated"
            | "worktree.created"
            | "worktree.opened"
            | "worktree.removed"
    )
}

pub fn event_matches(subs: &Value, event: &Value) -> bool {
    let Some(list) = subs.as_array() else { return true };
    if list.is_empty() {
        return true;
    }
    let name = event.get("event").and_then(Value::as_str).unwrap_or("");
    let data = event.get("data").cloned().unwrap_or(Value::Null);
    for sub in list {
        let wanted = sub.get("type").and_then(Value::as_str).unwrap_or("");
        if wanted != name {
            continue;
        }
        let mut ok = true;
        for key in ["pane_id", "workspace_id", "tab_id", "agent_status"] {
            if let Some(want) = sub.get(key).and_then(Value::as_str) {
                let got = data.get(key).and_then(Value::as_str).unwrap_or("");
                if want != got {
                    ok = false;
                    break;
                }
            }
        }
        if ok {
            return true;
        }
    }
    false
}

/// Full server run: bind the session endpoint, accept clients, tick agents.
pub async fn run_server<H: TerminalStreamHost + Send + 'static>(
    server: SharedServer<H>,
    session: &str,
) -> Result<(), String> {
    let endpoint = TransportEndpoint::for_session(session).map_err(|e| e.to_string())?;
    let mut listener =
        NamedPipeListener::bind(&endpoint, DEFAULT_MAX_FRAME_SIZE).map_err(|e| e.to_string())?;
    tracing::info!(session, address = %endpoint.as_address(), "starcil server listening");

    // Periodic agent tick.
    let ticker = server.clone();
    tokio::spawn(async move {
        loop {
            if ticker.shutdown.is_cancelled() {
                break;
            }
            ticker.tick();
            tokio::time::sleep(TICK_INTERVAL).await;
        }
    });

    loop {
        let conn = tokio::select! {
            c = listener.accept() => c.map_err(|e| e.to_string())?,
            _ = server.shutdown.cancelled() => break,
        };
        let server = server.clone();
        tokio::spawn(async move {
            handle_connection(server, conn).await;
        });
    }
    Ok(())
}

pub async fn handle_connection<H, T>(server: SharedServer<H>, mut conn: T)
where
    H: TerminalStreamHost,
    T: RawStreamTransport,
{
    let mut subscriptions: Option<Value> = None;
    let mut events_rx = server.events.subscribe();
    // TUI attach state.
    let mut is_tui = false;
    let mut last_snapshot_revision: u64 = 0;
    let mut frame_panes: Vec<String> = Vec::new();
    let mut need_snapshot: std::collections::BTreeSet<String> = Default::default();
    let mut pump = tokio::time::interval(Duration::from_millis(50));
    pump.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            incoming = conn.recv() => {
                match incoming {
                    Ok(Some(line)) => {
                        // TUI handshake: {"hello":{...}} upgrades the connection.
                        if line.get("hello").is_some() {
                            let hello: Result<starcil_protocol::attach::Hello, _> =
                                serde_json::from_value(line.clone());
                            match hello {
                                Ok(h) if h.hello.protocol_major == starcil_protocol::PROTOCOL_MAJOR => {
                                    use starcil_protocol::attach::ClientMode as CM;
                                    // Terminal stream modes take over the connection entirely.
                                    if matches!(h.hello.mode, CM::TerminalObserve | CM::TerminalControl | CM::TerminalAttach) {
                                        let Some(target) = h.hello.target.clone() else {
                                            let err = starcil_protocol::failure(
                                                "hello",
                                                ApiError::invalid_params("terminal modes require `target`"),
                                            );
                                            let _ = conn.send(serde_json::from_str(&err).unwrap()).await;
                                            break;
                                        };
                                        let request = StreamRequest {
                                            target,
                                            cols: h.hello.cols,
                                            rows: h.hello.rows,
                                            takeover: h.hello.takeover.unwrap_or(false),
                                        };
                                        let leases = server.leases.clone();
                                        let outcome = match h.hello.mode {
                                            CM::TerminalObserve => crate::streams::serve_observe(&server, &mut conn, request).await,
                                            CM::TerminalControl => crate::streams::serve_control(&server, &mut conn, leases, request).await,
                                            _ => crate::streams::serve_attach(&server, &mut conn, leases, request).await,
                                        };
                                        if let Err(e) = outcome {
                                            tracing::debug!(error = %e, "terminal stream ended");
                                        }
                                        break;
                                    }
                                    is_tui = matches!(h.hello.mode, CM::Tui);
                                    let (welcome, snapshot) = {
                                        let mut core = server.core.lock().expect("core lock poisoned");
                                        if is_tui {
                                            core.has_foreground_client = true;
                                            if let (Some(c), Some(r)) = (h.hello.cols, h.hello.rows) {
                                                core.client_area = starcil_domain::Rect { x: 0, y: 0, width: c, height: r };
                                                core.sync_pty_sizes();
                                            }
                                        }
                                        let snapshot = core.handle("session.snapshot", &json!({})).ok();
                                        let welcome = starcil_protocol::attach::Welcome {
                                            welcome: starcil_protocol::attach::WelcomeBody {
                                                protocol_major: starcil_protocol::PROTOCOL_MAJOR,
                                                protocol_minor: starcil_protocol::PROTOCOL_MINOR,
                                                version: core.version.clone(),
                                                session: core.session_name.clone(),
                                                generation: 1,
                                                capabilities: vec![],
                                            },
                                        };
                                        (welcome, snapshot)
                                    };
                                    if conn.send(serde_json::to_value(&welcome).unwrap()).await.is_err() {
                                        break;
                                    }
                                    if let Some(snap) = snapshot {
                                        let ev = json!({"event": "session.snapshot", "data": snap});
                                        if conn.send(ev).await.is_err() {
                                            break;
                                        }
                                    }
                                    // TUI clients get all lifecycle events pushed.
                                    if is_tui {
                                        subscriptions = Some(json!([]));
                                    }
                                }
                                _ => {
                                    let err = starcil_protocol::failure(
                                        "hello",
                                        ApiError::new(ErrorCode::ProtocolMismatch, "unsupported protocol version"),
                                    );
                                    let _ = conn.send(serde_json::from_str(&err).unwrap()).await;
                                    break;
                                }
                            }
                            continue;
                        }
                        // Raw input frames from tui/terminal-control clients.
                        if line.get("input").is_some() {
                            let frame: Result<starcil_protocol::attach::InputFrame, _> =
                                serde_json::from_value(line.clone());
                            if let Ok(frame) = frame {
                                apply_input_frame(&server, frame, &mut frame_panes, &mut need_snapshot);
                            }
                            continue;
                        }
                        // events.subscribe upgrades this connection to push mode.
                        if line.get("method").and_then(Value::as_str) == Some("events.subscribe") {
                            let id = line.get("id").and_then(Value::as_str).unwrap_or("sub").to_string();
                            subscriptions = line.get("params").and_then(|p| p.get("subscriptions")).cloned();
                            let ack = starcil_protocol::success(&id, json!({"type": "subscribed"}));
                            if conn.send(serde_json::from_str(&ack).unwrap()).await.is_err() {
                                break;
                            }
                            continue;
                        }
                        if let Some(resp) = service_request(&server, line).await {
                            let value: Value = serde_json::from_str(&resp).unwrap();
                            if conn.send(value).await.is_err() {
                                break;
                            }
                        }
                        if server.shutdown.is_cancelled() {
                            break;
                        }
                    }
                    Ok(None) | Err(_) => break,
                }
            }
            _ = pump.tick(), if is_tui && !frame_panes.is_empty() => {
                let frames = {
                    let mut core = server.core.lock().expect("core lock poisoned");
                    let mut out = Vec::new();
                    for pane_id in &frame_panes {
                        let Ok(pane) = pane_id.parse::<starcil_domain::PaneId>() else { continue };
                        let Ok(meta) = core.model.pane(pane) else { continue };
                        let term = meta.terminal_id.clone();
                        let snap = need_snapshot.remove(pane_id);
                        if let Some(mut frame) = core.host.take_frame(&term, snap) {
                            frame["pane_id"] = json!(pane_id);
                            out.push(frame);
                        }
                    }
                    out
                };
                let mut broken = false;
                for f in frames {
                    if conn.send(f).await.is_err() {
                        broken = true;
                        break;
                    }
                }
                if broken {
                    break;
                }
            }
            ev = events_rx.recv(), if subscriptions.is_some() => {
                match ev {
                    Ok(ev) => {
                        let subs = subscriptions.clone().unwrap_or(json!([]));
                        let name = ev.get("event").and_then(Value::as_str).unwrap_or("").to_string();
                        if event_matches(&subs, &ev) && conn.send(ev).await.is_err() {
                            break;
                        }
                        // TUI clients keep a local structure cache: push a fresh
                        // snapshot after structural changes so layout stays live.
                        if is_tui && is_structural_event(&name) {
                            let snapshot = {
                                let mut core = server.core.lock().expect("core lock poisoned");
                                let rev = core.model.revision;
                                if rev == last_snapshot_revision {
                                    None
                                } else {
                                    last_snapshot_revision = rev;
                                    core.handle("session.snapshot", &json!({})).ok()
                                }
                            };
                            if let Some(snap) = snapshot {
                                let frame = json!({"event": "session.snapshot", "data": snap});
                                if conn.send(frame).await.is_err() {
                                    break;
                                }
                            }
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
            _ = server.shutdown.cancelled() => break,
        }
    }
    if is_tui {
        let mut core = server.core.lock().expect("core lock poisoned");
        core.has_foreground_client = false;
    }
}

/// Apply one raw input frame from a tui client.
fn apply_input_frame<H: TerminalHost>(
    server: &SharedServer<H>,
    frame: starcil_protocol::attach::InputFrame,
    frame_panes: &mut Vec<String>,
    need_snapshot: &mut std::collections::BTreeSet<String>,
) {
    use starcil_protocol::attach::InputFrame as F;
    let mut core = server.core.lock().expect("core lock poisoned");
    match frame {
        F::Text { pane_id, text } => {
            if let Ok(p) = pane_id.parse::<starcil_domain::PaneId>() {
                if let Ok(meta) = core.model.pane(p) {
                    let term = meta.terminal_id.clone();
                    let _ = core.host.write_text(&term, &text);
                }
            }
        }
        F::Keys { pane_id, keys } => {
            if let Ok(p) = pane_id.parse::<starcil_domain::PaneId>() {
                if let Ok(meta) = core.model.pane(p) {
                    let term = meta.terminal_id.clone();
                    let _ = core.host.write_keys(&term, &keys);
                }
            }
        }
        F::ReserveRows { pane_id, rows } => {
            // The handler already holds the core lock.
            if let Ok(p) = pane_id.parse::<starcil_domain::PaneId>() {
                if rows == 0 {
                    core.reserved_rows.remove(&p);
                } else {
                    core.reserved_rows.insert(p, rows);
                }
                core.sync_pty_sizes();
            }
        }
        F::Scroll { pane_id, delta } => {
            if let Ok(p) = pane_id.parse::<starcil_domain::PaneId>() {
                if let Ok(meta) = core.model.pane(p) {
                    let term = meta.terminal_id.clone();
                    core.host.scroll_view(&term, delta);
                }
            }
        }
        F::Bytes { pane_id, data_base64 } => {
            if let Ok(p) = pane_id.parse::<starcil_domain::PaneId>() {
                if let Ok(meta) = core.model.pane(p) {
                    if let Ok(bytes) = crate::streams::decode_base64(&data_base64) {
                        let term = meta.terminal_id.clone();
                        let _ = core.host.write_text(&term, &String::from_utf8_lossy(&bytes));
                    }
                }
            }
        }
        F::Resize { pane_id, cols, rows } => {
            if let Ok(p) = pane_id.parse::<starcil_domain::PaneId>() {
                if let Ok(meta) = core.model.pane(p) {
                    let term = meta.terminal_id.clone();
                    let _ = core.host.resize(&term, cols, rows);
                }
            }
        }
        F::Resync { pane_id } => {
            need_snapshot.insert(pane_id);
        }
        F::Subscribe { pane_ids } => {
            for p in &pane_ids {
                if !frame_panes.contains(p) {
                    need_snapshot.insert(p.clone());
                }
            }
            *frame_panes = pane_ids;
        }
        F::Release { pane_id } => {
            frame_panes.retain(|p| p != &pane_id);
            need_snapshot.remove(&pane_id);
        }
        F::ClientArea { cols, rows } => {
            core.client_area = starcil_domain::Rect { x: 0, y: 0, width: cols, height: rows };
            core.sync_pty_sizes();
        }
    }
}
