use crate::{
    integration_for_kind, AgentKind, CompiledManifest, ManifestMetadata, MatcherKind, ScreenMatch,
    ScreenState,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    time::{Duration, Instant},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LifecycleState {
    Idle,
    Working,
    Blocked,
    Done,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReportedState {
    Idle,
    Working,
    Blocked,
}

impl From<ReportedState> for LifecycleState {
    fn from(value: ReportedState) -> Self {
        match value {
            ReportedState::Idle => Self::Idle,
            ReportedState::Working => Self::Working,
            ReportedState::Blocked => Self::Blocked,
        }
    }
}

impl From<ScreenState> for LifecycleState {
    fn from(value: ScreenState) -> Self {
        match value {
            ScreenState::Idle => Self::Idle,
            ScreenState::Working => Self::Working,
            ScreenState::Blocked => Self::Blocked,
        }
    }
}

pub trait Clock {
    fn now(&self) -> Duration;
}

#[derive(Debug)]
pub struct SystemClock {
    origin: Instant,
}

impl Default for SystemClock {
    fn default() -> Self {
        Self {
            origin: Instant::now(),
        }
    }
}

impl Clock for SystemClock {
    fn now(&self) -> Duration {
        self.origin.elapsed()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ProcessInfo {
    pub alive: bool,
    pub foreground: bool,
}

impl ProcessInfo {
    pub const fn foreground_agent() -> Self {
        Self {
            alive: true,
            foreground: true,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct DetectionSnapshot<'a> {
    pub text: &'a str,
    pub change_seq: u64,
    pub last_change_at: Duration,
}

#[derive(Debug, Clone, Copy)]
pub struct EvaluationInput<'a> {
    pub process: ProcessInfo,
    pub agent_id: Option<&'a str>,
    pub screen: DetectionSnapshot<'a>,
    /// True when the pane has been focused since its latest lifecycle transition.
    pub seen: bool,
}

#[derive(Debug, Clone)]
pub struct IntegrationReport {
    pub source: String,
    pub state: ReportedState,
    pub seq: u64,
    pub ttl: Duration,
    pub message: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReportAcceptance {
    Accepted,
    IgnoredStale { current_seq: u64 },
    IgnoredSessionOnlyIntegration,
    RejectedEmptySource,
}

#[derive(Debug, Clone, Copy)]
pub struct LifecycleConfig {
    pub screen_stability_window: Duration,
    pub integration_repaint_grace: Duration,
}

impl Default for LifecycleConfig {
    fn default() -> Self {
        Self {
            screen_stability_window: Duration::ZERO,
            integration_repaint_grace: Duration::from_secs(2),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionAuthority {
    ProcessExit,
    IntegrationReport,
    ManifestScreenRule,
    ConservativeFallback,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RuleEvidence {
    pub agent_id: String,
    pub rule_id: String,
    pub matcher: MatcherKind,
    pub pattern: String,
    pub matched_region: String,
    pub row_from_tail: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct IntegrationEvidence {
    pub source: String,
    pub state: ReportedState,
    pub seq: u64,
    pub ttl_ms: u64,
    pub received_at_ms: u64,
    pub screen_change_seq_at_report: u64,
    pub message: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FallbackEvidence {
    pub authority: DecisionAuthority,
    pub outcome: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LifecycleExplanation {
    pub state: LifecycleState,
    pub base_state: LifecycleState,
    pub authority: DecisionAuthority,
    pub evaluation_seq: u64,
    pub screen_change_seq: u64,
    pub process: ProcessInfo,
    pub seen: bool,
    pub previous_state: Option<LifecycleState>,
    pub derived_done: bool,
    pub screen_detection_skipped_by_lifecycle_authority: bool,
    pub manifest: Option<ManifestMetadata>,
    pub idle_fallback_reason: Option<String>,
    pub rule: Option<RuleEvidence>,
    pub integration: Option<IntegrationEvidence>,
    pub fallbacks_considered: Vec<FallbackEvidence>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LifecycleDecision {
    pub state: LifecycleState,
    pub explanation: LifecycleExplanation,
}

struct StoredReport {
    report: IntegrationReport,
    received_at: Duration,
    screen_change_seq_at_report: u64,
    accepted_order: u64,
}

pub struct LifecycleEngine<C> {
    clock: C,
    manifest: CompiledManifest,
    config: LifecycleConfig,
    reports: BTreeMap<String, StoredReport>,
    accepted_order: u64,
    evaluation_seq: u64,
    previous_base_state: Option<LifecycleState>,
    previous_state: Option<LifecycleState>,
    worked_while_unseen: bool,
    done_latched: bool,
    last_explanation: Option<LifecycleExplanation>,
}

impl<C: Clock> LifecycleEngine<C> {
    pub fn new(clock: C, manifest: CompiledManifest) -> Self {
        Self::with_config(clock, manifest, LifecycleConfig::default())
    }

    pub fn with_config(clock: C, manifest: CompiledManifest, config: LifecycleConfig) -> Self {
        Self {
            clock,
            manifest,
            config,
            reports: BTreeMap::new(),
            accepted_order: 0,
            evaluation_seq: 0,
            previous_base_state: None,
            previous_state: None,
            worked_while_unseen: false,
            done_latched: false,
            last_explanation: None,
        }
    }

    pub fn accept_report(
        &mut self,
        report: IntegrationReport,
        screen_change_seq: u64,
    ) -> ReportAcceptance {
        if report.source.trim().is_empty() {
            return ReportAcceptance::RejectedEmptySource;
        }
        if let Some(current) = self.reports.get(&report.source) {
            if report.seq <= current.report.seq {
                return ReportAcceptance::IgnoredStale {
                    current_seq: current.report.seq,
                };
            }
        }
        self.accepted_order = self.accepted_order.saturating_add(1);
        self.reports.insert(
            report.source.clone(),
            StoredReport {
                report,
                received_at: self.clock.now(),
                screen_change_seq_at_report: screen_change_seq,
                accepted_order: self.accepted_order,
            },
        );
        ReportAcceptance::Accepted
    }

    /// Accepts state only from official integrations with complete lifecycle coverage.
    pub fn accept_official_integration_report(
        &mut self,
        agent_kind: AgentKind,
        report: IntegrationReport,
        screen_change_seq: u64,
    ) -> ReportAcceptance {
        if !integration_for_kind(agent_kind)
            .map(|integration| integration.authors_lifecycle())
            .unwrap_or(false)
        {
            return ReportAcceptance::IgnoredSessionOnlyIntegration;
        }
        self.accept_report(report, screen_change_seq)
    }

    pub fn release_source(&mut self, source: &str) -> bool {
        self.reports.remove(source).is_some()
    }

    pub fn explain(&self) -> Option<&LifecycleExplanation> {
        self.last_explanation.as_ref()
    }

    pub fn evaluate(&mut self, input: EvaluationInput<'_>) -> LifecycleDecision {
        let now = self.clock.now();
        self.evaluation_seq = self.evaluation_seq.saturating_add(1);
        let mut fallbacks = Vec::new();
        let mut rule_evidence = None;
        let mut integration_evidence = None;
        let manifest_metadata = input
            .agent_id
            .and_then(|agent_id| self.manifest.manifest_metadata(agent_id))
            .cloned();
        let mut idle_fallback_reason = None;

        let (base_state, authority) = if !input.process.alive {
            fallbacks.push(FallbackEvidence {
                authority: DecisionAuthority::IntegrationReport,
                outcome: "not considered because the agent process exited".to_owned(),
            });
            fallbacks.push(FallbackEvidence {
                authority: DecisionAuthority::ManifestScreenRule,
                outcome: "not considered because the agent process exited".to_owned(),
            });
            (LifecycleState::Unknown, DecisionAuthority::ProcessExit)
        } else {
            fallbacks.push(FallbackEvidence {
                authority: DecisionAuthority::ProcessExit,
                outcome: format!(
                    "process is alive (foreground={})",
                    input.process.foreground
                ),
            });
            let (live_report, mut report_fallbacks) = self.select_live_report(now, input.screen);
            fallbacks.append(&mut report_fallbacks);
            if let Some((state, evidence)) = live_report {
                integration_evidence = Some(evidence);
                fallbacks.push(FallbackEvidence {
                    authority: DecisionAuthority::ManifestScreenRule,
                    outcome: "not considered because a live integration report won".to_owned(),
                });
                (state.into(), DecisionAuthority::IntegrationReport)
            } else if let Some(found) = self
                .manifest
                .match_screen(input.agent_id, input.screen.text)
            {
                let state = found.state.into();
                rule_evidence = Some(rule_evidence_from_match(found));
                (state, DecisionAuthority::ManifestScreenRule)
            } else {
                fallbacks.push(FallbackEvidence {
                    authority: DecisionAuthority::ManifestScreenRule,
                    outcome: "no agent-specific or generic screen rule matched".to_owned(),
                });
                let known_agent = input
                    .agent_id
                    .map(|agent_id| self.manifest.is_known_agent(agent_id))
                    .unwrap_or(false);
                if known_agent {
                    let stable_for = now.saturating_sub(input.screen.last_change_at);
                    if stable_for >= self.config.screen_stability_window {
                        idle_fallback_reason =
                            Some("default_known_agent_idle_fallback".to_owned());
                        fallbacks.push(FallbackEvidence {
                            authority: DecisionAuthority::ConservativeFallback,
                            outcome: "default_known_agent_idle_fallback".to_owned(),
                        });
                        (LifecycleState::Idle, DecisionAuthority::ConservativeFallback)
                    } else {
                        fallbacks.push(FallbackEvidence {
                            authority: DecisionAuthority::ConservativeFallback,
                            outcome: format!(
                                "known agent screen changed {} ms ago; conservatively working",
                                millis(stable_for)
                            ),
                        });
                        (LifecycleState::Working, DecisionAuthority::ConservativeFallback)
                    }
                } else {
                    fallbacks.push(FallbackEvidence {
                        authority: DecisionAuthority::ConservativeFallback,
                        outcome: "unknown agent without a matching generic rule".to_owned(),
                    });
                    (LifecycleState::Unknown, DecisionAuthority::ConservativeFallback)
                }
            }
        };

        let previous_state = self.previous_state;
        let (state, derived_done) = self.apply_done_semantics(base_state, input.seen);
        let explanation = LifecycleExplanation {
            state,
            base_state,
            authority,
            evaluation_seq: self.evaluation_seq,
            screen_change_seq: input.screen.change_seq,
            process: input.process,
            seen: input.seen,
            previous_state,
            derived_done,
            screen_detection_skipped_by_lifecycle_authority:
                authority == DecisionAuthority::IntegrationReport,
            manifest: manifest_metadata,
            idle_fallback_reason,
            rule: rule_evidence,
            integration: integration_evidence,
            fallbacks_considered: fallbacks,
        };
        self.previous_base_state = Some(base_state);
        self.previous_state = Some(state);
        self.last_explanation = Some(explanation.clone());
        LifecycleDecision { state, explanation }
    }

    fn select_live_report(
        &self,
        now: Duration,
        screen: DetectionSnapshot<'_>,
    ) -> (
        Option<(ReportedState, IntegrationEvidence)>,
        Vec<FallbackEvidence>,
    ) {
        let mut fallbacks = Vec::new();
        let mut selected: Option<&StoredReport> = None;
        for stored in self.reports.values() {
            let expires_at = stored
                .received_at
                .checked_add(stored.report.ttl)
                .unwrap_or(Duration::MAX);
            if now >= expires_at {
                fallbacks.push(FallbackEvidence {
                    authority: DecisionAuthority::IntegrationReport,
                    outcome: format!(
                        "source `{}` seq {} expired after {} ms",
                        stored.report.source,
                        stored.report.seq,
                        millis(stored.report.ttl)
                    ),
                });
                continue;
            }

            let grace_ends_at = stored
                .received_at
                .checked_add(self.config.integration_repaint_grace)
                .unwrap_or(Duration::MAX);
            let invalidated_by_activity =
                screen.change_seq > stored.screen_change_seq_at_report
                    && screen.last_change_at >= grace_ends_at;
            if invalidated_by_activity {
                fallbacks.push(FallbackEvidence {
                    authority: DecisionAuthority::IntegrationReport,
                    outcome: format!(
                        "source `{}` seq {} invalidated by screen change_seq {} after repaint grace",
                        stored.report.source, stored.report.seq, screen.change_seq
                    ),
                });
                continue;
            }

            if selected
                .map(|current| stored.accepted_order > current.accepted_order)
                .unwrap_or(true)
            {
                selected = Some(stored);
            }
        }

        let Some(selected) = selected else {
            if self.reports.is_empty() {
                fallbacks.push(FallbackEvidence {
                    authority: DecisionAuthority::IntegrationReport,
                    outcome: "no integration reports are registered".to_owned(),
                });
            }
            return (None, fallbacks);
        };

        for stored in self.reports.values() {
            if stored.accepted_order != selected.accepted_order {
                let expires_at = stored
                    .received_at
                    .checked_add(stored.report.ttl)
                    .unwrap_or(Duration::MAX);
                let grace_ends_at = stored
                    .received_at
                    .checked_add(self.config.integration_repaint_grace)
                    .unwrap_or(Duration::MAX);
                if now < expires_at
                    && !(screen.change_seq > stored.screen_change_seq_at_report
                        && screen.last_change_at >= grace_ends_at)
                {
                    fallbacks.push(FallbackEvidence {
                        authority: DecisionAuthority::IntegrationReport,
                        outcome: format!(
                            "source `{}` seq {} superseded by newer live source `{}` seq {}",
                            stored.report.source,
                            stored.report.seq,
                            selected.report.source,
                            selected.report.seq
                        ),
                    });
                }
            }
        }

        let evidence = IntegrationEvidence {
            source: selected.report.source.clone(),
            state: selected.report.state,
            seq: selected.report.seq,
            ttl_ms: millis(selected.report.ttl),
            received_at_ms: millis(selected.received_at),
            screen_change_seq_at_report: selected.screen_change_seq_at_report,
            message: selected.report.message.clone(),
        };
        (Some((selected.report.state, evidence)), fallbacks)
    }

    fn apply_done_semantics(
        &mut self,
        base_state: LifecycleState,
        seen: bool,
    ) -> (LifecycleState, bool) {
        if seen {
            self.done_latched = false;
            self.worked_while_unseen = false;
        }

        match base_state {
            LifecycleState::Working => {
                self.done_latched = false;
                self.worked_while_unseen = !seen;
                (LifecycleState::Working, false)
            }
            LifecycleState::Idle => {
                if !seen
                    && self.previous_base_state == Some(LifecycleState::Working)
                    && self.worked_while_unseen
                {
                    self.done_latched = true;
                }
                if !seen && self.done_latched {
                    (LifecycleState::Done, true)
                } else {
                    (LifecycleState::Idle, false)
                }
            }
            state => (state, false),
        }
    }
}

fn rule_evidence_from_match(found: ScreenMatch) -> RuleEvidence {
    RuleEvidence {
        agent_id: found.agent_id,
        rule_id: found.rule_id,
        matcher: found.matcher,
        pattern: found.pattern,
        matched_region: found.matched_region,
        row_from_tail: found.row_from_tail,
    }
}

fn millis(duration: Duration) -> u64 {
    duration.as_millis().min(u64::MAX as u128) as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::Cell, rc::Rc};

    #[derive(Clone, Default)]
    struct FakeClock(Rc<Cell<Duration>>);

    impl FakeClock {
        fn set(&self, now: Duration) {
            self.0.set(now);
        }
    }

    impl Clock for FakeClock {
        fn now(&self) -> Duration {
            self.0.get()
        }
    }

    fn engine() -> (FakeClock, LifecycleEngine<FakeClock>) {
        let clock = FakeClock::default();
        let engine = LifecycleEngine::new(clock.clone(), CompiledManifest::bundled().unwrap());
        (clock, engine)
    }

    fn input<'a>(
        text: &'a str,
        agent_id: Option<&'a str>,
        change_seq: u64,
        last_change_at: Duration,
        seen: bool,
    ) -> EvaluationInput<'a> {
        EvaluationInput {
            process: ProcessInfo::foreground_agent(),
            agent_id,
            screen: DetectionSnapshot {
                text,
                change_seq,
                last_change_at,
            },
            seen,
        }
    }

    #[test]
    fn integration_report_beats_screen_rules() {
        let (_, mut engine) = engine();
        assert_eq!(
            engine.accept_report(
                IntegrationReport {
                    source: "hook:claude".to_owned(),
                    state: ReportedState::Working,
                    seq: 7,
                    ttl: Duration::from_secs(30),
                    message: None,
                },
                4,
            ),
            ReportAcceptance::Accepted
        );
        let decision = engine.evaluate(input("❯", Some("claude"), 4, Duration::ZERO, true));
        assert_eq!(decision.state, LifecycleState::Working);
        assert_eq!(decision.explanation.authority, DecisionAuthority::IntegrationReport);
        assert_eq!(decision.explanation.integration.unwrap().seq, 7);
    }

    #[test]
    fn process_exit_beats_reports_and_screen_rules() {
        let (_, mut engine) = engine();
        engine.accept_report(
            IntegrationReport {
                source: "hook".to_owned(),
                state: ReportedState::Blocked,
                seq: 1,
                ttl: Duration::from_secs(30),
                message: Some("approval".to_owned()),
            },
            1,
        );
        let mut exited = input(
            "Do you want to continue?",
            Some("claude"),
            1,
            Duration::ZERO,
            false,
        );
        exited.process.alive = false;
        let decision = engine.evaluate(exited);
        assert_eq!(decision.state, LifecycleState::Unknown);
        assert_eq!(decision.explanation.authority, DecisionAuthority::ProcessExit);
        assert!(decision.explanation.integration.is_none());
    }

    #[test]
    fn ttl_expiry_falls_back_to_manifest_screen_rule() {
        let (clock, mut engine) = engine();
        engine.accept_report(
            IntegrationReport {
                source: "hook".to_owned(),
                state: ReportedState::Blocked,
                seq: 1,
                ttl: Duration::from_secs(1),
                message: None,
            },
            1,
        );
        clock.set(Duration::from_secs(2));
        let decision = engine.evaluate(input(
            "✻ Thinking… (esc to interrupt)",
            Some("claude"),
            1,
            Duration::ZERO,
            true,
        ));
        assert_eq!(decision.state, LifecycleState::Working);
        assert_eq!(decision.explanation.authority, DecisionAuthority::ManifestScreenRule);
        assert!(decision
            .explanation
            .fallbacks_considered
            .iter()
            .any(|item| item.outcome.contains("expired")));
    }

    #[test]
    fn repaint_within_grace_does_not_invalidate_but_later_activity_does() {
        let (clock, mut engine) = engine();
        engine.accept_report(
            IntegrationReport {
                source: "hook".to_owned(),
                state: ReportedState::Blocked,
                seq: 9,
                ttl: Duration::from_secs(30),
                message: Some("permission dialog".to_owned()),
            },
            10,
        );

        clock.set(Duration::from_secs(1));
        let repaint = engine.evaluate(input(
            "›",
            Some("codex"),
            11,
            Duration::from_secs(1),
            true,
        ));
        assert_eq!(repaint.state, LifecycleState::Blocked);
        assert_eq!(repaint.explanation.authority, DecisionAuthority::IntegrationReport);

        clock.set(Duration::from_secs(3));
        let quiet_after_repaint = engine.evaluate(input(
            "›",
            Some("codex"),
            11,
            Duration::from_secs(1),
            true,
        ));
        assert_eq!(quiet_after_repaint.state, LifecycleState::Blocked);

        let fresh_activity = engine.evaluate(input(
            "›",
            Some("codex"),
            12,
            Duration::from_secs(3),
            true,
        ));
        assert_eq!(fresh_activity.state, LifecycleState::Idle);
        assert_eq!(
            fresh_activity.explanation.authority,
            DecisionAuthority::ManifestScreenRule
        );
    }

    #[test]
    fn done_is_only_derived_from_unseen_working_to_idle_and_seen_clears_it() {
        let (clock, mut engine) = engine();
        let working = engine.evaluate(input(
            "Esc to interrupt",
            Some("codex"),
            1,
            Duration::ZERO,
            false,
        ));
        assert_eq!(working.state, LifecycleState::Working);

        clock.set(Duration::from_secs(1));
        let done = engine.evaluate(input(
            "›",
            Some("codex"),
            2,
            Duration::from_secs(1),
            false,
        ));
        assert_eq!(done.state, LifecycleState::Done);
        assert!(done.explanation.derived_done);

        let still_done = engine.evaluate(input(
            "›",
            Some("codex"),
            2,
            Duration::from_secs(1),
            false,
        ));
        assert_eq!(still_done.state, LifecycleState::Done);

        let seen = engine.evaluate(input(
            "›",
            Some("codex"),
            2,
            Duration::from_secs(1),
            true,
        ));
        assert_eq!(seen.state, LifecycleState::Idle);
        assert!(!seen.explanation.derived_done);
    }

    #[test]
    fn stale_sequence_from_same_source_is_ignored() {
        let (_, mut engine) = engine();
        engine.accept_report(
            IntegrationReport {
                source: "plugin:kimi".to_owned(),
                state: ReportedState::Working,
                seq: 4,
                ttl: Duration::from_secs(10),
                message: None,
            },
            0,
        );
        assert_eq!(
            engine.accept_report(
                IntegrationReport {
                    source: "plugin:kimi".to_owned(),
                    state: ReportedState::Idle,
                    seq: 3,
                    ttl: Duration::from_secs(10),
                    message: None,
                },
                0,
            ),
            ReportAcceptance::IgnoredStale { current_seq: 4 }
        );
    }

    #[test]
    fn session_only_official_integration_cannot_override_screen_lifecycle() {
        let (_, mut engine) = engine();
        assert_eq!(
            engine.accept_official_integration_report(
                AgentKind::Claude,
                IntegrationReport {
                    source: "official:claude".to_owned(),
                    state: ReportedState::Blocked,
                    seq: 1,
                    ttl: Duration::from_secs(30),
                    message: None,
                },
                1,
            ),
            ReportAcceptance::IgnoredSessionOnlyIntegration
        );
        let decision = engine.evaluate(input(
            "✻ Thinking… (esc to interrupt)",
            Some("claude"),
            1,
            Duration::ZERO,
            true,
        ));
        assert_eq!(decision.state, LifecycleState::Working);
        assert_eq!(decision.explanation.authority, DecisionAuthority::ManifestScreenRule);
        assert!(!decision
            .explanation
            .screen_detection_skipped_by_lifecycle_authority);
    }

    #[test]
    fn complete_official_integration_skips_screen_detection() {
        let (_, mut engine) = engine();
        assert_eq!(
            engine.accept_official_integration_report(
                AgentKind::Kimi,
                IntegrationReport {
                    source: "official:kimi".to_owned(),
                    state: ReportedState::Blocked,
                    seq: 1,
                    ttl: Duration::from_secs(30),
                    message: Some("approval".to_owned()),
                },
                1,
            ),
            ReportAcceptance::Accepted
        );
        let decision = engine.evaluate(input(
            "ordinary text",
            Some("kimi"),
            1,
            Duration::ZERO,
            true,
        ));
        assert_eq!(decision.state, LifecycleState::Blocked);
        assert!(decision
            .explanation
            .screen_detection_skipped_by_lifecycle_authority);
    }

    #[test]
    fn known_agent_without_rule_uses_documented_idle_fallback_reason() {
        let (_, mut engine) = engine();
        let decision = engine.evaluate(input(
            "ordinary output",
            Some("amp"),
            1,
            Duration::ZERO,
            true,
        ));
        assert_eq!(decision.state, LifecycleState::Idle);
        assert_eq!(
            decision.explanation.idle_fallback_reason.as_deref(),
            Some("default_known_agent_idle_fallback")
        );
        assert_eq!(
            decision.explanation.manifest.unwrap().source,
            crate::ManifestSource::Bundled
        );
    }
}
