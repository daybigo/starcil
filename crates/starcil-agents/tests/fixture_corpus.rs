use starcil_agents::{
    Clock, CompiledManifest, DecisionAuthority, DetectionSnapshot, EvaluationInput,
    LifecycleEngine, LifecycleState, ProcessInfo,
};
use std::time::Duration;

struct FixedClock(Duration);

impl Clock for FixedClock {
    fn now(&self) -> Duration {
        self.0
    }
}

struct Fixture {
    name: &'static str,
    agent_id: Option<&'static str>,
    text: &'static str,
    expected: LifecycleState,
}

const FIXTURES: &[Fixture] = &[
    Fixture {
        name: "claude-working",
        agent_id: Some("claude"),
        text: include_str!("../fixtures/claude-working.txt"),
        expected: LifecycleState::Working,
    },
    Fixture {
        // The turn summary reuses the spinner glyph (✻); a draft sits in the
        // input box. Both must read idle (C18: panes stuck "working").
        name: "claude-done-idle",
        agent_id: Some("claude"),
        text: include_str!("../fixtures/claude-done-idle.txt"),
        expected: LifecycleState::Idle,
    },
    Fixture {
        name: "claude-blocked",
        agent_id: Some("claude"),
        text: include_str!("../fixtures/claude-blocked.txt"),
        expected: LifecycleState::Blocked,
    },
    Fixture {
        name: "claude-idle",
        agent_id: Some("claude"),
        text: include_str!("../fixtures/claude-idle.txt"),
        expected: LifecycleState::Idle,
    },
    Fixture {
        name: "codex-working",
        agent_id: Some("codex"),
        text: include_str!("../fixtures/codex-working.txt"),
        expected: LifecycleState::Working,
    },
    Fixture {
        name: "codex-trust-blocked",
        agent_id: Some("codex"),
        text: include_str!("../fixtures/codex-trust-blocked.txt"),
        expected: LifecycleState::Blocked,
    },
    Fixture {
        name: "codex-update-blocked",
        agent_id: Some("codex"),
        text: include_str!("../fixtures/codex-update-blocked.txt"),
        expected: LifecycleState::Blocked,
    },
    Fixture {
        name: "codex-idle",
        agent_id: Some("codex"),
        text: include_str!("../fixtures/codex-idle.txt"),
        expected: LifecycleState::Idle,
    },
    Fixture {
        name: "gemini-working",
        agent_id: Some("gemini"),
        text: include_str!("../fixtures/gemini-working.txt"),
        expected: LifecycleState::Working,
    },
    Fixture {
        name: "gemini-blocked",
        agent_id: Some("gemini"),
        text: include_str!("../fixtures/gemini-blocked.txt"),
        expected: LifecycleState::Blocked,
    },
    Fixture {
        name: "gemini-idle",
        agent_id: Some("gemini"),
        text: include_str!("../fixtures/gemini-idle.txt"),
        expected: LifecycleState::Idle,
    },
    Fixture {
        name: "generic-yn-blocked",
        agent_id: None,
        text: include_str!("../fixtures/generic-yn-blocked.txt"),
        expected: LifecycleState::Blocked,
    },
    Fixture {
        name: "generic-password-blocked",
        agent_id: None,
        text: include_str!("../fixtures/generic-password-blocked.txt"),
        expected: LifecycleState::Blocked,
    },
    Fixture {
        name: "generic-press-key-blocked",
        agent_id: None,
        text: include_str!("../fixtures/generic-press-key-blocked.txt"),
        expected: LifecycleState::Blocked,
    },
    Fixture {
        name: "generic-selection-blocked",
        agent_id: None,
        text: include_str!("../fixtures/generic-selection-blocked.txt"),
        expected: LifecycleState::Blocked,
    },
    Fixture {
        name: "generic-shell-unknown",
        agent_id: None,
        text: include_str!("../fixtures/generic-shell-unknown.txt"),
        expected: LifecycleState::Unknown,
    },
];

fn classify(fixture: &Fixture) -> starcil_agents::LifecycleDecision {
    let mut engine = LifecycleEngine::new(
        FixedClock(Duration::from_secs(10)),
        CompiledManifest::bundled().unwrap(),
    );
    engine.evaluate(EvaluationInput {
        process: ProcessInfo::foreground_agent(),
        agent_id: fixture.agent_id,
        screen: DetectionSnapshot {
            text: fixture.text,
            change_seq: 42,
            last_change_at: Duration::ZERO,
        },
        seen: true,
    })
}

#[test]
fn every_authored_fixture_classifies_as_labeled() {
    for fixture in FIXTURES {
        let decision = classify(fixture);
        assert_eq!(decision.state, fixture.expected, "fixture {}", fixture.name);
    }
}

#[test]
fn explain_exposes_rule_and_matched_region_for_three_agent_families() {
    for name in ["claude-blocked", "codex-working", "gemini-idle"] {
        let fixture = FIXTURES.iter().find(|fixture| fixture.name == name).unwrap();
        let decision = classify(fixture);
        assert_eq!(
            decision.explanation.authority,
            DecisionAuthority::ManifestScreenRule,
            "fixture {name}"
        );
        let rule = decision.explanation.rule.expect("screen rule evidence");
        assert!(!rule.rule_id.is_empty(), "fixture {name}");
        assert!(!rule.matched_region.is_empty(), "fixture {name}");
        assert_eq!(decision.explanation.screen_change_seq, 42);
    }
}
