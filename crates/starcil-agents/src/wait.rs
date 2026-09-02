use crate::{Clock, LifecycleState};
use serde::Serialize;
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WaitConfig {
    pub targets: Vec<LifecycleState>,
    pub timeout: Option<Duration>,
    pub stall_after: Duration,
}

impl Default for WaitConfig {
    fn default() -> Self {
        Self {
            targets: vec![
                LifecycleState::Idle,
                LifecycleState::Done,
                LifecycleState::Blocked,
            ],
            timeout: None,
            stall_after: Duration::from_secs(5),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum WaitOutcome {
    Pending,
    Reached {
        state: LifecycleState,
        elapsed_ms: u64,
    },
    Stalled {
        state: LifecycleState,
        elapsed_ms: u64,
    },
    Timeout {
        state: LifecycleState,
        elapsed_ms: u64,
    },
}

/// Pure prompt-wait state machine. A non-working initial state must change before it settles.
pub struct PromptWait {
    config: WaitConfig,
    started_at: Duration,
    initial_state: LifecycleState,
    last_state: LifecycleState,
    lifecycle_changed: bool,
    finished: Option<WaitOutcome>,
}

impl PromptWait {
    pub fn start<C: Clock>(clock: &C, initial_state: LifecycleState, config: WaitConfig) -> Self {
        Self::at(clock.now(), initial_state, config)
    }

    pub fn at(started_at: Duration, initial_state: LifecycleState, config: WaitConfig) -> Self {
        Self {
            config,
            started_at,
            initial_state,
            last_state: initial_state,
            lifecycle_changed: false,
            finished: None,
        }
    }

    pub fn poll<C: Clock>(&mut self, clock: &C, state: LifecycleState) -> WaitOutcome {
        self.poll_at(clock.now(), state)
    }

    pub fn poll_at(&mut self, now: Duration, state: LifecycleState) -> WaitOutcome {
        if let Some(outcome) = &self.finished {
            return outcome.clone();
        }

        if state != self.last_state {
            self.lifecycle_changed = true;
            self.last_state = state;
        }
        let elapsed = now.saturating_sub(self.started_at);

        let can_settle = self.initial_state == LifecycleState::Working || self.lifecycle_changed;
        if can_settle && self.config.targets.contains(&state) {
            return self.finish(WaitOutcome::Reached {
                state,
                elapsed_ms: millis(elapsed),
            });
        }

        let stall_due = self.initial_state != LifecycleState::Working
            && !self.lifecycle_changed
            && elapsed >= self.config.stall_after;
        let timeout_due = self
            .config
            .timeout
            .map(|timeout| elapsed >= timeout)
            .unwrap_or(false);
        let stall_precedes_timeout = self
            .config
            .timeout
            .map(|timeout| self.config.stall_after < timeout)
            .unwrap_or(true);

        if stall_due && stall_precedes_timeout {
            return self.finish(WaitOutcome::Stalled {
                state,
                elapsed_ms: millis(elapsed),
            });
        }
        if timeout_due {
            return self.finish(WaitOutcome::Timeout {
                state,
                elapsed_ms: millis(elapsed),
            });
        }
        WaitOutcome::Pending
    }

    pub fn lifecycle_changed(&self) -> bool {
        self.lifecycle_changed
    }

    fn finish(&mut self, outcome: WaitOutcome) -> WaitOutcome {
        self.finished = Some(outcome.clone());
        outcome
    }
}

/// Standalone lifecycle wait. Unlike prompt wait, an already-matching state settles immediately.
pub struct AgentWait {
    config: WaitConfig,
    started_at: Duration,
    finished: Option<WaitOutcome>,
}

impl AgentWait {
    pub fn start<C: Clock>(clock: &C, config: WaitConfig) -> Self {
        Self::at(clock.now(), config)
    }

    pub fn at(started_at: Duration, config: WaitConfig) -> Self {
        Self {
            config,
            started_at,
            finished: None,
        }
    }

    pub fn poll<C: Clock>(&mut self, clock: &C, state: LifecycleState) -> WaitOutcome {
        self.poll_at(clock.now(), state)
    }

    pub fn poll_at(&mut self, now: Duration, state: LifecycleState) -> WaitOutcome {
        if let Some(outcome) = &self.finished {
            return outcome.clone();
        }
        let elapsed = now.saturating_sub(self.started_at);
        if self.config.targets.contains(&state) {
            let outcome = WaitOutcome::Reached {
                state,
                elapsed_ms: millis(elapsed),
            };
            self.finished = Some(outcome.clone());
            return outcome;
        }
        if self
            .config
            .timeout
            .map(|timeout| elapsed >= timeout)
            .unwrap_or(false)
        {
            let outcome = WaitOutcome::Timeout {
                state,
                elapsed_ms: millis(elapsed),
            };
            self.finished = Some(outcome.clone());
            return outcome;
        }
        WaitOutcome::Pending
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

    #[test]
    fn non_working_start_without_change_stalls_at_five_seconds() {
        let clock = FakeClock::default();
        let mut wait = PromptWait::start(&clock, LifecycleState::Idle, WaitConfig::default());
        clock.set(Duration::from_millis(4_999));
        assert_eq!(wait.poll(&clock, LifecycleState::Idle), WaitOutcome::Pending);
        clock.set(Duration::from_secs(5));
        assert_eq!(
            wait.poll(&clock, LifecycleState::Idle),
            WaitOutcome::Stalled {
                state: LifecycleState::Idle,
                elapsed_ms: 5_000
            }
        );
    }

    #[test]
    fn timeout_before_stall_is_reported_as_timeout() {
        let clock = FakeClock::default();
        let mut wait = PromptWait::start(
            &clock,
            LifecycleState::Unknown,
            WaitConfig {
                timeout: Some(Duration::from_secs(2)),
                ..WaitConfig::default()
            },
        );
        clock.set(Duration::from_secs(2));
        assert_eq!(
            wait.poll(&clock, LifecycleState::Unknown),
            WaitOutcome::Timeout {
                state: LifecycleState::Unknown,
                elapsed_ms: 2_000
            }
        );
    }

    #[test]
    fn timeout_equal_to_stall_threshold_is_normal_timeout() {
        let clock = FakeClock::default();
        let mut wait = PromptWait::start(
            &clock,
            LifecycleState::Idle,
            WaitConfig {
                timeout: Some(Duration::from_secs(5)),
                ..WaitConfig::default()
            },
        );
        clock.set(Duration::from_secs(5));
        assert_eq!(
            wait.poll(&clock, LifecycleState::Idle),
            WaitOutcome::Timeout {
                state: LifecycleState::Idle,
                elapsed_ms: 5_000
            }
        );
    }

    #[test]
    fn working_then_idle_reaches_default_settled_target() {
        let clock = FakeClock::default();
        let mut wait = PromptWait::start(&clock, LifecycleState::Idle, WaitConfig::default());
        clock.set(Duration::from_millis(100));
        assert_eq!(wait.poll(&clock, LifecycleState::Working), WaitOutcome::Pending);
        clock.set(Duration::from_millis(500));
        assert_eq!(
            wait.poll(&clock, LifecycleState::Idle),
            WaitOutcome::Reached {
                state: LifecycleState::Idle,
                elapsed_ms: 500
            }
        );
    }

    #[test]
    fn until_target_can_be_working() {
        let clock = FakeClock::default();
        let mut wait = PromptWait::start(
            &clock,
            LifecycleState::Idle,
            WaitConfig {
                targets: vec![LifecycleState::Working],
                ..WaitConfig::default()
            },
        );
        clock.set(Duration::from_millis(20));
        assert_eq!(
            wait.poll(&clock, LifecycleState::Working),
            WaitOutcome::Reached {
                state: LifecycleState::Working,
                elapsed_ms: 20
            }
        );
    }

    #[test]
    fn a_non_target_change_disables_stall_but_not_timeout() {
        let clock = FakeClock::default();
        let mut wait = PromptWait::start(
            &clock,
            LifecycleState::Idle,
            WaitConfig {
                targets: vec![LifecycleState::Done],
                timeout: Some(Duration::from_secs(8)),
                ..WaitConfig::default()
            },
        );
        clock.set(Duration::from_secs(1));
        assert_eq!(wait.poll(&clock, LifecycleState::Blocked), WaitOutcome::Pending);
        clock.set(Duration::from_secs(5));
        assert_eq!(wait.poll(&clock, LifecycleState::Blocked), WaitOutcome::Pending);
        clock.set(Duration::from_secs(8));
        assert_eq!(
            wait.poll(&clock, LifecycleState::Blocked),
            WaitOutcome::Timeout {
                state: LifecycleState::Blocked,
                elapsed_ms: 8_000
            }
        );
    }

    #[test]
    fn standalone_wait_returns_immediately_for_current_target() {
        let clock = FakeClock::default();
        let mut wait = AgentWait::start(&clock, WaitConfig::default());
        assert_eq!(
            wait.poll(&clock, LifecycleState::Blocked),
            WaitOutcome::Reached {
                state: LifecycleState::Blocked,
                elapsed_ms: 0
            }
        );
    }
}
