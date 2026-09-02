use std::collections::VecDeque;

use starcil_protocol::attach::{InputFrame, TerminalFrame};
use starcil_protocol::types::SessionSnapshot;
use starcil_protocol::{Incoming, Request};

#[derive(Debug, Clone)]
pub enum ClientMsg {
    Request(Request),
    Input(InputFrame),
}

#[derive(Debug, Clone)]
pub enum ServerMsg {
    Incoming(Incoming),
    TerminalFrame(TerminalFrame),
    SessionSnapshot(SessionSnapshot),
}

/// Transport seam used by the app core. A socket implementation can plug in
/// later without changing input, state, or rendering logic.
pub trait ServerLink {
    fn send(&mut self, msg: ClientMsg);
    fn drain(&mut self) -> Vec<ServerMsg>;
}

#[derive(Debug, Default)]
pub struct FakeLink {
    scripted: VecDeque<ServerMsg>,
    sent: Vec<ClientMsg>,
}

impl FakeLink {
    pub fn new(messages: impl IntoIterator<Item = ServerMsg>) -> Self {
        Self {
            scripted: messages.into_iter().collect(),
            sent: Vec::new(),
        }
    }

    pub fn push(&mut self, message: ServerMsg) {
        self.scripted.push_back(message);
    }

    pub fn sent(&self) -> &[ClientMsg] {
        &self.sent
    }

    pub fn take_sent(&mut self) -> Vec<ClientMsg> {
        std::mem::take(&mut self.sent)
    }
}

impl ServerLink for FakeLink {
    fn send(&mut self, msg: ClientMsg) {
        self.sent.push(msg);
    }

    fn drain(&mut self) -> Vec<ServerMsg> {
        self.scripted.drain(..).collect()
    }
}
