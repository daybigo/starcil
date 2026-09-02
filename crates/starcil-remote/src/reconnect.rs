use std::{
    fmt,
    future::Future,
    pin::Pin,
    time::Duration,
};

use starcil_platform::{Transport, TransportError};
use starcil_protocol::{attach::{Hello, Welcome}, PROTOCOL_MAJOR};
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReconnectPhase {
    Connected,
    Waiting { attempt: u32, delay: Duration },
    Connecting { attempt: u32 },
    Handshaking { attempt: u32 },
    SnapshotRequired,
    Fatal { message: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReconnectAction {
    Wait(Duration),
    SpawnTransport,
    SendHello,
    RequestFreshSnapshot,
    Fatal(String),
}

#[derive(Debug, Clone)]
pub struct ReconnectStateMachine {
    phase: ReconnectPhase,
    attempt: u32,
}

impl Default for ReconnectStateMachine {
    fn default() -> Self {
        Self::new()
    }
}

impl ReconnectStateMachine {
    pub fn new() -> Self {
        Self {
            phase: ReconnectPhase::Connected,
            attempt: 0,
        }
    }

    pub fn phase(&self) -> &ReconnectPhase {
        &self.phase
    }

    pub fn transport_lost(&mut self) -> Result<ReconnectAction, ReconnectError> {
        match &self.phase {
            ReconnectPhase::Connected | ReconnectPhase::SnapshotRequired => {
                self.attempt = 0;
                self.wait_action()
            }
            _ => Err(self.invalid("transport_lost")),
        }
    }

    pub fn backoff_elapsed(&mut self) -> Result<ReconnectAction, ReconnectError> {
        let attempt = match &self.phase {
            ReconnectPhase::Waiting { attempt, .. } => *attempt,
            _ => return Err(self.invalid("backoff_elapsed")),
        };
        self.phase = ReconnectPhase::Connecting { attempt };
        Ok(ReconnectAction::SpawnTransport)
    }

    pub fn transport_spawned(&mut self) -> Result<ReconnectAction, ReconnectError> {
        let attempt = match &self.phase {
            ReconnectPhase::Connecting { attempt } => *attempt,
            _ => return Err(self.invalid("transport_spawned")),
        };
        self.phase = ReconnectPhase::Handshaking { attempt };
        Ok(ReconnectAction::SendHello)
    }

    pub fn retryable_failure(&mut self) -> Result<ReconnectAction, ReconnectError> {
        match &self.phase {
            ReconnectPhase::Connecting { .. } | ReconnectPhase::Handshaking { .. } => {
                self.attempt = self.attempt.saturating_add(1);
                self.wait_action()
            }
            _ => Err(self.invalid("retryable_failure")),
        }
    }

    pub fn handshake_accepted(
        &mut self,
        remote_protocol_major: u32,
    ) -> Result<ReconnectAction, ReconnectError> {
        if !matches!(&self.phase, ReconnectPhase::Handshaking { .. }) {
            return Err(self.invalid("handshake_accepted"));
        }
        if remote_protocol_major != PROTOCOL_MAJOR {
            let message = protocol_mismatch_message(PROTOCOL_MAJOR, Some(remote_protocol_major));
            self.phase = ReconnectPhase::Fatal {
                message: message.clone(),
            };
            return Ok(ReconnectAction::Fatal(message));
        }
        self.phase = ReconnectPhase::SnapshotRequired;
        Ok(ReconnectAction::RequestFreshSnapshot)
    }

    pub fn protocol_rejected(&mut self) -> Result<ReconnectAction, ReconnectError> {
        if !matches!(&self.phase, ReconnectPhase::Handshaking { .. }) {
            return Err(self.invalid("protocol_rejected"));
        }
        let message = protocol_mismatch_message(PROTOCOL_MAJOR, None);
        self.phase = ReconnectPhase::Fatal {
            message: message.clone(),
        };
        Ok(ReconnectAction::Fatal(message))
    }

    pub fn snapshot_requested(&mut self) -> Result<(), ReconnectError> {
        if !matches!(&self.phase, ReconnectPhase::SnapshotRequired) {
            return Err(self.invalid("snapshot_requested"));
        }
        self.phase = ReconnectPhase::Connected;
        self.attempt = 0;
        Ok(())
    }

    fn wait_action(&mut self) -> Result<ReconnectAction, ReconnectError> {
        let delay = retry_delay(self.attempt);
        self.phase = ReconnectPhase::Waiting {
            attempt: self.attempt,
            delay,
        };
        Ok(ReconnectAction::Wait(delay))
    }

    fn invalid(&self, event: &'static str) -> ReconnectError {
        ReconnectError::InvalidTransition {
            phase: self.phase.clone(),
            event,
        }
    }
}

pub fn retry_delay(attempt: u32) -> Duration {
    match attempt {
        0 => Duration::from_millis(500),
        1 => Duration::from_secs(2),
        2 => Duration::from_secs(5),
        later => Duration::from_secs(5_u64.saturating_mul(1_u64 << (later - 2).min(3)).min(30)),
    }
}

pub trait ReconnectSleeper {
    fn sleep<'a>(
        &'a mut self,
        duration: Duration,
    ) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>>;
}

#[derive(Debug, Default)]
pub struct TokioReconnectSleeper;

impl ReconnectSleeper for TokioReconnectSleeper {
    fn sleep<'a>(
        &'a mut self,
        duration: Duration,
    ) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>> {
        Box::pin(tokio::time::sleep(duration))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReconnectSignal {
    RequestFreshSnapshot,
}

pub struct ReconnectOutcome<T> {
    pub transport: T,
    pub signal: ReconnectSignal,
}

/// Reconnects after a broken transport, redoes only the Hello/Welcome handshake,
/// and returns a mandatory fresh-snapshot signal. Unacknowledged input is never
/// accepted by this API and therefore is never replayed across the reconnect.
pub async fn reconnect_after_loss<T, C, F, E, S>(
    machine: &mut ReconnectStateMachine,
    sleeper: &mut S,
    hello: &Hello,
    mut connect: C,
) -> Result<ReconnectOutcome<T>, ReconnectError>
where
    T: Transport,
    C: FnMut() -> F,
    F: Future<Output = Result<T, E>>,
    E: fmt::Display,
    S: ReconnectSleeper,
{
    if hello.hello.protocol_major != PROTOCOL_MAJOR {
        return Err(ReconnectError::InvalidLocalProtocol {
            expected: PROTOCOL_MAJOR,
            actual: hello.hello.protocol_major,
        });
    }

    let mut action = machine.transport_lost()?;
    loop {
        match action {
            ReconnectAction::Wait(delay) => {
                sleeper.sleep(delay).await;
                action = machine.backoff_elapsed()?;
            }
            ReconnectAction::SpawnTransport => match connect().await {
                Ok(mut transport) => {
                    action = machine.transport_spawned()?;
                    debug_assert_eq!(action, ReconnectAction::SendHello);
                    match handshake(&mut transport, hello).await? {
                        HandshakeResult::Accepted(remote_major) => {
                            action = machine.handshake_accepted(remote_major)?;
                            match action {
                                ReconnectAction::RequestFreshSnapshot => {
                                    return Ok(ReconnectOutcome {
                                        transport,
                                        signal: ReconnectSignal::RequestFreshSnapshot,
                                    });
                                }
                                ReconnectAction::Fatal(message) => {
                                    return Err(ReconnectError::ProtocolMismatch(message));
                                }
                                _ => unreachable!("handshake acceptance has a terminal action"),
                            }
                        }
                        HandshakeResult::ProtocolRejected => {
                            let action = machine.protocol_rejected()?;
                            let ReconnectAction::Fatal(message) = action else {
                                unreachable!("protocol rejection is fatal")
                            };
                            return Err(ReconnectError::ProtocolMismatch(message));
                        }
                        HandshakeResult::Retryable(message) => {
                            tracing::debug!(%message, "remote handshake failed; retrying");
                            action = machine.retryable_failure()?;
                        }
                    }
                }
                Err(error) => {
                    tracing::debug!(error = %error, "remote transport respawn failed; retrying");
                    action = machine.retryable_failure()?;
                }
            },
            ReconnectAction::SendHello => {
                unreachable!("the async driver sends Hello immediately after spawning")
            }
            ReconnectAction::RequestFreshSnapshot => {
                unreachable!("the async driver returns the snapshot signal immediately")
            }
            ReconnectAction::Fatal(message) => {
                return Err(ReconnectError::ProtocolMismatch(message));
            }
        }
    }
}

enum HandshakeResult {
    Accepted(u32),
    ProtocolRejected,
    Retryable(String),
}

async fn handshake<T>(
    transport: &mut T,
    hello: &Hello,
) -> Result<HandshakeResult, ReconnectError>
where
    T: Transport,
{
    let frame = serde_json::to_value(hello)?;
    if let Err(error) = transport.send(frame).await {
        return Ok(HandshakeResult::Retryable(error.to_string()));
    }
    let frame = match transport.recv().await {
        Ok(Some(frame)) => frame,
        Ok(None) => return Ok(HandshakeResult::Retryable("remote closed during handshake".into())),
        Err(error) => return Ok(HandshakeResult::Retryable(error.to_string())),
    };
    if frame
        .get("error")
        .and_then(|error| error.get("code"))
        .and_then(serde_json::Value::as_str)
        == Some("protocol_mismatch")
    {
        return Ok(HandshakeResult::ProtocolRejected);
    }
    match serde_json::from_value::<Welcome>(frame) {
        Ok(welcome) => Ok(HandshakeResult::Accepted(
            welcome.welcome.protocol_major,
        )),
        Err(error) => Ok(HandshakeResult::Retryable(format!(
            "remote returned an invalid Welcome frame: {error}"
        ))),
    }
}

fn protocol_mismatch_message(local: u32, remote: Option<u32>) -> String {
    match remote {
        Some(remote) => format!(
            "remote Starcil protocol major mismatch: local {local}, remote {remote}; update one side before reconnecting"
        ),
        None => format!(
            "remote Starcil rejected protocol major {local}; update one side before reconnecting"
        ),
    }
}

#[derive(Debug, Error)]
pub enum ReconnectError {
    #[error("invalid reconnect transition `{event}` while in {phase:?}")]
    InvalidTransition {
        phase: ReconnectPhase,
        event: &'static str,
    },
    #[error("local Hello protocol major is {actual}, but this build requires {expected}")]
    InvalidLocalProtocol { expected: u32, actual: u32 },
    #[error("{0}")]
    ProtocolMismatch(String),
    #[error("could not serialize the remote Hello frame: {0}")]
    Json(#[from] serde_json::Error),
    #[error("remote handshake transport failed: {0}")]
    Transport(#[from] TransportError),
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{collections::VecDeque, sync::{Arc, Mutex}};
    use starcil_platform::InMemoryTransport;
    use starcil_protocol::{attach::{ClientMode, HelloBody, WelcomeBody}, PROTOCOL_MINOR};

    fn hello() -> Hello {
        Hello {
            hello: HelloBody {
                protocol_major: PROTOCOL_MAJOR,
                protocol_minor: PROTOCOL_MINOR,
                version: "0.1.0".into(),
                mode: ClientMode::Tui,
                capabilities: Vec::new(),
                cols: Some(120),
                rows: Some(40),
                takeover: None,
                target: None,
            },
        }
    }

    #[test]
    fn pure_machine_uses_the_bounded_backoff_schedule() {
        let mut machine = ReconnectStateMachine::new();
        let mut observed = Vec::new();
        let ReconnectAction::Wait(delay) = machine.transport_lost().unwrap() else {
            panic!("loss must wait")
        };
        observed.push(delay);
        for _ in 0..6 {
            assert_eq!(machine.backoff_elapsed().unwrap(), ReconnectAction::SpawnTransport);
            let ReconnectAction::Wait(delay) = machine.retryable_failure().unwrap() else {
                panic!("failure must wait")
            };
            observed.push(delay);
        }
        assert_eq!(
            observed,
            [
                Duration::from_millis(500),
                Duration::from_secs(2),
                Duration::from_secs(5),
                Duration::from_secs(10),
                Duration::from_secs(20),
                Duration::from_secs(30),
                Duration::from_secs(30),
            ]
        );
    }

    #[test]
    fn protocol_major_mismatch_is_fatal_and_clear() {
        let mut machine = ReconnectStateMachine::new();
        machine.transport_lost().unwrap();
        machine.backoff_elapsed().unwrap();
        machine.transport_spawned().unwrap();
        let ReconnectAction::Fatal(message) = machine
            .handshake_accepted(PROTOCOL_MAJOR + 1)
            .unwrap()
        else {
            panic!("mismatch must be fatal")
        };
        assert!(message.contains("local 1, remote 2"));
        assert!(matches!(machine.phase(), ReconnectPhase::Fatal { .. }));
    }

    struct RecordingSleeper(Arc<Mutex<Vec<Duration>>>);

    impl ReconnectSleeper for RecordingSleeper {
        fn sleep<'a>(
            &'a mut self,
            duration: Duration,
        ) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>> {
            self.0.lock().unwrap().push(duration);
            Box::pin(std::future::ready(()))
        }
    }

    #[tokio::test]
    async fn async_driver_uses_injected_outcomes_and_requests_snapshot() {
        let (client, mut server) = InMemoryTransport::pair(1024 * 1024);
        let server_task = tokio::spawn(async move {
            let frame = server.recv().await.unwrap().unwrap();
            let received: Hello = serde_json::from_value(frame).unwrap();
            assert_eq!(received.hello.protocol_major, PROTOCOL_MAJOR);
            server
                .send(
                    serde_json::to_value(Welcome {
                        welcome: WelcomeBody {
                            protocol_major: PROTOCOL_MAJOR,
                            protocol_minor: PROTOCOL_MINOR,
                            version: "0.1.0".into(),
                            session: "default".into(),
                            generation: 2,
                            capabilities: Vec::new(),
                        },
                    })
                    .unwrap(),
                )
                .await
                .unwrap();
        });
        let mut outcomes = VecDeque::from([Err("offline"), Ok(client)]);
        let sleeps = Arc::new(Mutex::new(Vec::new()));
        let mut sleeper = RecordingSleeper(Arc::clone(&sleeps));
        let mut machine = ReconnectStateMachine::new();
        let outcome = reconnect_after_loss(
            &mut machine,
            &mut sleeper,
            &hello(),
            || std::future::ready(outcomes.pop_front().expect("connector outcome")),
        )
        .await
        .unwrap();
        assert_eq!(outcome.signal, ReconnectSignal::RequestFreshSnapshot);
        assert!(matches!(machine.phase(), ReconnectPhase::SnapshotRequired));
        assert_eq!(
            *sleeps.lock().unwrap(),
            [Duration::from_millis(500), Duration::from_secs(2)]
        );
        server_task.await.unwrap();
    }
}
