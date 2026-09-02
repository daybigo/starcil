use crate::{Connection, IntegrationHookAction};
use serde_json::{json, Value};
use starcil_protocol::Request;

pub fn run_hook_with(
    action: &IntegrationHookAction,
    pane_id: Option<&str>,
    stdin_payload: &str,
    connection: &mut dyn Connection,
) {
    let Some(pane_id) = pane_id.filter(|value| !value.trim().is_empty()) else { return };
    match action {
        IntegrationHookAction::ClaudeNotification => {
            let Some(payload) = parse_object(stdin_payload) else { return };
            let message = payload
                .get("message")
                .and_then(Value::as_str)
                .filter(|value| !value.trim().is_empty())
                .unwrap_or("Claude needs attention");
            call_safely(
                connection,
                Request::new(
                    "hook:claude-notification",
                    "pane.report_agent",
                    json!({
                        "pane_id": pane_id,
                        "source": "starcil:claude",
                        "agent": "claude",
                        "state": "blocked",
                        "message": message,
                    }),
                ),
            );
        }
        IntegrationHookAction::ClaudeStop => {
            if parse_object(stdin_payload).is_none() { return }
            call_safely(
                connection,
                Request::new(
                    "hook:claude-stop",
                    "pane.report_agent",
                    json!({
                        "pane_id": pane_id,
                        "source": "starcil:claude",
                        "agent": "claude",
                        "state": "idle",
                    }),
                ),
            );
        }
        IntegrationHookAction::ClaudeSessionStart => {
            let Some(session_id) = parse_object(stdin_payload)
                .and_then(|payload| payload.get("session_id").and_then(Value::as_str).map(str::to_owned))
                .filter(|value| !value.trim().is_empty())
            else {
                return;
            };
            call_safely(
                connection,
                Request::new(
                    "hook:claude-session-start",
                    "pane.report_agent_session",
                    json!({
                        "pane_id": pane_id,
                        "source": "starcil:claude",
                        "agent": "claude",
                        "agent_session_id": session_id,
                    }),
                ),
            );
        }
        IntegrationHookAction::CodexNotify { payload } => {
            let Some(payload) = payload.as_deref().and_then(parse_object) else { return };
            if payload.get("type").and_then(Value::as_str) != Some("agent-turn-complete") {
                return;
            }
            let Some(thread_id) = payload
                .get("thread-id")
                .and_then(Value::as_str)
                .filter(|value| !value.trim().is_empty())
            else {
                return;
            };
            call_safely(
                connection,
                Request::new(
                    "hook:codex-notify:session",
                    "pane.report_agent_session",
                    json!({
                        "pane_id": pane_id,
                        "source": "starcil:codex",
                        "agent": "codex",
                        "agent_session_id": thread_id,
                    }),
                ),
            );
            call_safely(
                connection,
                Request::new(
                    "hook:codex-notify:idle",
                    "pane.report_agent",
                    json!({
                        "pane_id": pane_id,
                        "source": "starcil:codex",
                        "agent": "codex",
                        "state": "idle",
                    }),
                ),
            );
        }
    }
}

fn parse_object(source: &str) -> Option<Value> {
    serde_json::from_str::<Value>(source).ok().filter(Value::is_object)
}

fn call_safely(connection: &mut dyn Connection, request: Request) {
    let _ = connection.call(&request);
}

#[cfg(test)]
mod tests {
    use super::*;
    use starcil_protocol::{success, Incoming, SuccessResponse};
    use std::io;

    #[derive(Default)]
    struct RecordingConnection {
        requests: Vec<Request>,
    }

    impl Connection for RecordingConnection {
        fn call(&mut self, request: &Request) -> io::Result<Incoming> {
            self.requests.push(request.clone());
            let frame = success(&request.id, json!({"type": "ok"}));
            let response: SuccessResponse = serde_json::from_str(&frame).unwrap();
            Ok(Incoming::Success(response))
        }
    }

    #[test]
    fn all_four_helpers_emit_the_contracted_requests() {
        let mut connection = RecordingConnection::default();
        run_hook_with(
            &IntegrationHookAction::ClaudeNotification,
            Some("w1:p1"),
            r#"{"message":"Permission required"}"#,
            &mut connection,
        );
        run_hook_with(
            &IntegrationHookAction::ClaudeStop,
            Some("w1:p1"),
            "{}",
            &mut connection,
        );
        run_hook_with(
            &IntegrationHookAction::ClaudeSessionStart,
            Some("w1:p1"),
            r#"{"session_id":"claude-7"}"#,
            &mut connection,
        );
        run_hook_with(
            &IntegrationHookAction::CodexNotify {
                payload: Some(r#"{"type":"agent-turn-complete","thread-id":"thread-9","future":true}"#.to_owned()),
            },
            Some("w1:p1"),
            "",
            &mut connection,
        );

        assert_eq!(
            connection.requests.iter().map(|request| request.method.as_str()).collect::<Vec<_>>(),
            [
                "pane.report_agent",
                "pane.report_agent",
                "pane.report_agent_session",
                "pane.report_agent_session",
                "pane.report_agent",
            ]
        );
        assert_eq!(connection.requests[0].params["state"], "blocked");
        assert_eq!(connection.requests[2].params["agent_session_id"], "claude-7");
        assert_eq!(connection.requests[3].params["agent_session_id"], "thread-9");
        assert_eq!(connection.requests[4].params["state"], "idle");
    }

    #[test]
    fn malformed_or_out_of_pane_hooks_are_silent_no_ops() {
        let mut connection = RecordingConnection::default();
        run_hook_with(&IntegrationHookAction::ClaudeStop, None, "{}", &mut connection);
        run_hook_with(&IntegrationHookAction::ClaudeNotification, Some("w1:p1"), "not json", &mut connection);
        run_hook_with(
            &IntegrationHookAction::CodexNotify { payload: Some(r#"{"type":"future"}"#.to_owned()) },
            Some("w1:p1"),
            "",
            &mut connection,
        );
        assert!(connection.requests.is_empty());
    }
}
