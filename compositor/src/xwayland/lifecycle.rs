use std::collections::VecDeque;
use std::time::{Duration, Instant};

const FAILURE_WINDOW: Duration = Duration::from_secs(30);

const MAX_FAILURES: usize = 3;

/// What the service must do once a generation's resources are actually gone.
///
/// Chosen while cleanup starts, published only after it finishes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AfterStop {
    /// Schedule a bounded retry.
    Backoff,
    /// Open the circuit: fail fast until an explicit retry.
    Failed,
    /// The session is shutting down.
    Stopped,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Phase {
    Idle,

    Starting(u64),

    Spawned(u64),
    Running(u64),
    /// Cleanup in progress. This phase still owns the previous process and its
    /// monitoring sources, and no new generation may start until
    /// [`Lifecycle::finish_stop`] runs.
    Stopping {
        generation: u64,
        after_stop: AfterStop,
    },
    Backoff {
        generation: u64,
        until: Instant,
    },

    Failed,
    Stopped,
}

impl Phase {
    pub fn generation(self) -> Option<u64> {
        match self {
            Phase::Starting(generation)
            | Phase::Spawned(generation)
            | Phase::Running(generation)
            | Phase::Stopping { generation, .. }
            | Phase::Backoff { generation, .. } => Some(generation),
            Phase::Idle | Phase::Failed | Phase::Stopped => None,
        }
    }

    pub fn is_running(self) -> bool {
        matches!(self, Phase::Running(_))
    }

    ///
    /// Only `Idle` qualifies. In particular `Stopping` does not, which is what
    /// keeps a new generation from starting while a previous process may still
    /// hold the X11 listeners.
    pub fn can_start(self) -> bool {
        matches!(self, Phase::Idle)
    }

    /// True while a generation's process and cleanup sources are still owned.
    #[cfg(test)]
    pub fn is_stopping(self) -> bool {
        matches!(self, Phase::Stopping { .. })
    }
}

// Process resources belong to the manager, not this state model.
pub(crate) struct Lifecycle {
    pub phase: Phase,
    next_generation: u64,
    failures: VecDeque<Instant>,
}

impl Default for Lifecycle {
    fn default() -> Self {
        Self {
            phase: Phase::Idle,
            next_generation: 0,
            failures: VecDeque::new(),
        }
    }
}

impl Lifecycle {
    pub fn request_start(&mut self) -> Option<u64> {
        if !self.phase.can_start() {
            return None;
        }
        let Some(generation) = self.next_generation.checked_add(1) else {
            self.phase = Phase::Failed;
            return None;
        };
        self.next_generation = generation;
        self.phase = Phase::Starting(generation);

        Some(generation)
    }

    pub fn spawned(&mut self, generation: u64) -> bool {
        if self.phase != Phase::Starting(generation) {
            return false;
        }
        self.phase = Phase::Spawned(generation);

        true
    }

    pub fn ready(&mut self, generation: u64) -> bool {
        if self.phase != Phase::Spawned(generation) {
            return false;
        }
        self.phase = Phase::Running(generation);

        true
    }

    /// Parks in `Stopping` so no connection can start a replacement generation
    /// while the old service still holds the listeners.
    pub fn begin_stop_after_failure(&mut self, generation: u64, now: Instant, retryable: bool) -> Option<AfterStop> {
        let current = matches!(
            self.phase,
            Phase::Starting(current)
                | Phase::Spawned(current)
                | Phase::Running(current)
                if current == generation
        );
        if !current {
            return None;
        }

        while self
            .failures
            .front()
            .is_some_and(|at| now.saturating_duration_since(*at) > FAILURE_WINDOW)
        {
            self.failures.pop_front();
        }
        self.failures.push_back(now);

        let after_stop = if !retryable || self.failures.len() >= MAX_FAILURES {
            AfterStop::Failed
        } else {
            AfterStop::Backoff
        };
        self.phase = Phase::Stopping { generation, after_stop };

        Some(after_stop)
    }

    /// How long a retryable failure should wait before re-enabling activation.
    pub fn backoff_delay(&self) -> Duration {
        if self.failures.len() <= 1 {
            Duration::from_millis(250)
        } else {
            Duration::from_secs(1)
        }
    }

    /// Publish the result of a completed stop.
    ///
    /// Returns `None` unless cleanup is finishing for `generation`, so a stale
    /// cleanup callback cannot publish a newer generation's state.
    pub fn finish_stop(&mut self, generation: u64, now: Instant) -> Option<AfterStop> {
        let Phase::Stopping {
            generation: current,
            after_stop,
        } = self.phase
        else {
            return None;
        };
        if current != generation {
            return None;
        }

        self.phase = match after_stop {
            AfterStop::Backoff => Phase::Backoff {
                generation,
                until: now + self.backoff_delay(),
            },
            AfterStop::Failed => Phase::Failed,
            AfterStop::Stopped => Phase::Stopped,
        };

        Some(after_stop)
    }

    /// Strengthen an in-flight stop to `Failed` because cleanup could not be
    /// confirmed.
    ///
    /// The scheduled outcome is only a hint: a service whose group still holds
    /// the listeners must open the circuit whatever the stop was going to do,
    /// otherwise a retryable `Backoff` would hand the display to a
    /// replacement generation that cannot bind it.
    pub fn escalate_stop_to_failed(&mut self, generation: u64) {
        let Phase::Stopping {
            generation: current,
            after_stop,
        } = &mut self.phase
        else {
            return;
        };
        if *current == generation {
            *after_stop = AfterStop::Failed;
        }
    }

    pub fn finish_backoff(&mut self, generation: u64, now: Instant) -> bool {
        match self.phase {
            Phase::Backoff {
                generation: current,
                until,
            } if current == generation && now >= until => {
                self.phase = Phase::Idle;
                true
            }
            _ => false,
        }
    }

    /// Whether an explicit user-requested retry is currently allowed.
    ///
    /// Deliberately a check, not a transition: the caller must prepare
    /// resources first and only then call [`Lifecycle::commit_explicit_retry`],
    /// so a failed setup cannot leave the service in a state that rejects its
    /// own retry.
    pub fn can_explicit_retry(&self) -> bool {
        self.phase == Phase::Failed
    }

    /// Adopt the prepared state for an explicit retry.
    ///
    /// This clears the retry budget. It is a separate operation from an
    /// incoming client connection, so a failing service can never reopen its
    /// own circuit by receiving connections.
    pub fn commit_explicit_retry(&mut self) -> bool {
        if !self.can_explicit_retry() {
            return false;
        }
        self.failures.clear();
        self.phase = Phase::Idle;

        true
    }

    /// Invalidate any scheduled start for the shutdown path.
    ///
    /// Uses the same `Stopping` phase as a failure so there is one stop
    /// transition rather than two that can disagree.
    pub fn begin_shutdown_stop(&mut self) -> Option<u64> {
        if self.phase == Phase::Stopped {
            return None;
        }
        let generation = self.phase.generation().unwrap_or(0);
        self.phase = Phase::Stopping {
            generation,
            after_stop: AfterStop::Stopped,
        };

        Some(generation)
    }

    pub fn recent_failures(&self) -> usize {
        self.failures.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn start(lifecycle: &mut Lifecycle) -> u64 {
        lifecycle.request_start().expect("Idle must yield a generation ticket")
    }

    /// Fail a generation the way the manager does, then complete the stop.
    ///
    /// Cleanup completing is what publishes the next phase, so a test that
    /// needs the next generation must say so explicitly.
    fn fail_and_stop(lifecycle: &mut Lifecycle, generation: u64, now: Instant, retryable: bool) -> AfterStop {
        let after_stop = lifecycle
            .begin_stop_after_failure(generation, now, retryable)
            .expect("the current generation must accept a failure");
        assert_eq!(
            lifecycle.finish_stop(generation, now),
            Some(after_stop),
            "cleanup completion publishes the chosen outcome"
        );
        after_stop
    }

    fn start_after_backoff(lifecycle: &mut Lifecycle) -> u64 {
        let now = Instant::now();
        let Phase::Backoff { generation, until } = lifecycle.phase else {
            panic!("expected a backoff, found {:?}", lifecycle.phase);
        };
        assert!(lifecycle.finish_backoff(generation, until.max(now)));
        start(lifecycle)
    }

    #[test]
    fn starts_from_idle_and_claims_one_generation_at_a_time() {
        let mut lifecycle = Lifecycle::default();
        assert_eq!(lifecycle.phase, Phase::Idle);

        let generation = start(&mut lifecycle);
        assert_eq!(generation, 1);
        assert_eq!(lifecycle.phase, Phase::Starting(1));

        assert_eq!(lifecycle.request_start(), None);
        assert_eq!(lifecycle.phase, Phase::Starting(1));
    }

    #[test]
    fn generations_are_monotonic() {
        let mut lifecycle = Lifecycle::default();
        let first = start(&mut lifecycle);
        lifecycle.ready(first);
        fail_and_stop(&mut lifecycle, first, Instant::now(), true);
        let second = start_after_backoff(&mut lifecycle);

        assert!(second > first);
    }

    #[test]
    fn spawn_then_readiness_reaches_running() {
        let mut lifecycle = Lifecycle::default();
        let generation = start(&mut lifecycle);

        assert!(lifecycle.spawned(generation));
        assert_eq!(lifecycle.phase, Phase::Spawned(generation));
        assert!(lifecycle.ready(generation));
        assert_eq!(lifecycle.phase, Phase::Running(generation));
        assert!(lifecycle.phase.is_running());
    }

    #[test]
    fn stale_generations_cannot_advance_the_phase() {
        let mut lifecycle = Lifecycle::default();
        let stale = start(&mut lifecycle);
        fail_and_stop(&mut lifecycle, stale, Instant::now(), true);
        let current = start_after_backoff(&mut lifecycle);

        assert!(!lifecycle.spawned(stale));
        assert!(!lifecycle.ready(stale));
        assert_eq!(lifecycle.begin_stop_after_failure(stale, Instant::now(), true), None);
        assert_eq!(lifecycle.phase, Phase::Starting(current));
    }

    #[test]
    fn readiness_cannot_be_claimed_before_exec() {
        let mut lifecycle = Lifecycle::default();
        let generation = start(&mut lifecycle);

        assert!(!lifecycle.ready(generation));
        assert_eq!(lifecycle.phase, Phase::Starting(generation));
    }

    #[test]
    fn a_non_retryable_failure_opens_the_circuit_immediately() {
        let mut lifecycle = Lifecycle::default();
        let generation = start(&mut lifecycle);
        lifecycle.spawned(generation);

        assert_eq!(
            fail_and_stop(&mut lifecycle, generation, Instant::now(), false),
            AfterStop::Failed
        );
        assert_eq!(lifecycle.phase, Phase::Failed);
        assert!(!lifecycle.phase.can_start());
    }

    #[test]
    fn backoff_is_bounded_and_then_opens_the_circuit() {
        let mut lifecycle = Lifecycle::default();

        let now = Instant::now();
        let first = start(&mut lifecycle);
        assert_eq!(fail_and_stop(&mut lifecycle, first, now, true), AfterStop::Backoff);
        let Phase::Backoff { until, .. } = lifecycle.phase else {
            panic!("first failure must back off");
        };
        assert_eq!(until, now + Duration::from_millis(250));

        let second = start_after_backoff(&mut lifecycle);
        let now = Instant::now();
        assert_eq!(fail_and_stop(&mut lifecycle, second, now, true), AfterStop::Backoff);
        let Phase::Backoff { until, .. } = lifecycle.phase else {
            panic!("second failure must back off");
        };
        assert_eq!(until, now + Duration::from_secs(1));

        let third = start_after_backoff(&mut lifecycle);
        assert_eq!(
            fail_and_stop(&mut lifecycle, third, Instant::now(), true),
            AfterStop::Failed,
            "reaching the failure limit must open the circuit instead of backing off"
        );
        assert_eq!(lifecycle.phase, Phase::Failed);
        assert_eq!(lifecycle.recent_failures(), MAX_FAILURES);
    }

    #[test]
    fn backoff_releases_to_idle_only_after_the_window() {
        let mut lifecycle = Lifecycle::default();
        let generation = start(&mut lifecycle);
        let now = Instant::now();
        fail_and_stop(&mut lifecycle, generation, now, true);

        let Phase::Backoff { until, .. } = lifecycle.phase else {
            panic!("must be backing off");
        };

        assert!(!lifecycle.finish_backoff(generation, until - Duration::from_millis(1)));
        assert!(lifecycle.finish_backoff(generation, until));
        assert_eq!(lifecycle.phase, Phase::Idle);
    }

    #[test]
    fn a_successful_spawn_does_not_reset_the_failure_window() {
        let mut lifecycle = Lifecycle::default();
        let first = start(&mut lifecycle);
        fail_and_stop(&mut lifecycle, first, Instant::now(), true);
        let second = start_after_backoff(&mut lifecycle);

        lifecycle.spawned(second);
        lifecycle.ready(second);

        assert_eq!(lifecycle.recent_failures(), 1);
    }

    #[test]
    fn failures_outside_the_window_are_discarded() {
        let mut lifecycle = Lifecycle::default();
        let start_time = Instant::now();
        let generation = start(&mut lifecycle);
        fail_and_stop(&mut lifecycle, generation, Instant::now(), true);
        assert_eq!(lifecycle.recent_failures(), 1);

        let second = start_after_backoff(&mut lifecycle);
        let later = start_time + FAILURE_WINDOW + Duration::from_secs(1);
        fail_and_stop(&mut lifecycle, second, later, true);

        assert_eq!(lifecycle.recent_failures(), 1);
    }

    #[test]
    fn explicit_retry_clears_the_circuit_but_a_connection_cannot() {
        let mut lifecycle = Lifecycle::default();
        let generation = start(&mut lifecycle);
        lifecycle.spawned(generation);
        fail_and_stop(&mut lifecycle, generation, Instant::now(), false);
        assert_eq!(lifecycle.phase, Phase::Failed);

        assert_eq!(lifecycle.request_start(), None);

        assert!(lifecycle.can_explicit_retry());
        assert!(lifecycle.commit_explicit_retry());
        assert_eq!(lifecycle.phase, Phase::Idle);
        assert_eq!(lifecycle.recent_failures(), 0);
        assert!(lifecycle.request_start().is_some());
    }

    #[test]
    fn explicit_retry_is_refused_outside_the_failed_phase() {
        let mut lifecycle = Lifecycle::default();
        assert!(!lifecycle.can_explicit_retry());
        assert!(!lifecycle.commit_explicit_retry());

        let generation = start(&mut lifecycle);
        lifecycle.spawned(generation);
        assert!(!lifecycle.can_explicit_retry());
        assert!(!lifecycle.commit_explicit_retry());
    }

    #[test]
    fn a_failed_generation_is_not_startable_until_cleanup_completes() {
        let mut lifecycle = Lifecycle::default();
        let generation = start(&mut lifecycle);
        lifecycle.spawned(generation);

        let now = Instant::now();
        let after_stop = lifecycle
            .begin_stop_after_failure(generation, now, true)
            .expect("the current generation accepts a failure");

        // The process is still alive and still holds the listeners, so the
        // lifecycle must not be startable and must not look like a failure
        // that a queued connection could act on.
        assert!(lifecycle.phase.is_stopping());
        assert!(!lifecycle.phase.can_start());
        assert_eq!(lifecycle.request_start(), None);
        assert!(!lifecycle.can_explicit_retry());

        // Only cleanup finishing publishes the retryable outcome.
        assert_eq!(
            lifecycle.finish_stop(generation, now),
            Some(after_stop),
            "cleanup completion publishes the outcome"
        );
        assert!(matches!(lifecycle.phase, Phase::Backoff { .. }));
        assert!(!lifecycle.phase.can_start(), "backoff still blocks a start");
    }

    #[test]
    fn an_abandoned_cleanup_cannot_publish_a_newer_generation() {
        let mut lifecycle = Lifecycle::default();
        let generation = start(&mut lifecycle);
        lifecycle.spawned(generation);

        lifecycle
            .begin_stop_after_failure(generation, Instant::now(), false)
            .expect("the current generation accepts a failure");
        assert_eq!(lifecycle.finish_stop(generation.wrapping_add(1), Instant::now()), None);

        // A stale completion leaves the real transition in flight.
        assert!(lifecycle.phase.is_stopping());
        assert!(lifecycle.finish_stop(generation, Instant::now()).is_some());
        assert_eq!(lifecycle.phase, Phase::Failed);
    }

    #[test]
    fn stopping_invalidates_scheduled_starts() {
        let mut lifecycle = Lifecycle::default();
        let _generation = start(&mut lifecycle);

        let generation = lifecycle.begin_shutdown_stop().expect("a stop is required");
        assert!(lifecycle.phase.is_stopping());
        assert_eq!(
            lifecycle.finish_stop(generation, Instant::now()),
            Some(AfterStop::Stopped)
        );
        assert_eq!(lifecycle.phase, Phase::Stopped);
        assert_eq!(lifecycle.request_start(), None);
        assert!(!lifecycle.spawned(generation));
        assert!(!lifecycle.ready(generation));

        // Shutdown is idempotent once stopped.
        assert_eq!(lifecycle.begin_shutdown_stop(), None);
        assert_eq!(lifecycle.phase, Phase::Stopped);
    }

    #[test]
    fn phase_reports_its_generation() {
        assert_eq!(Phase::Starting(4).generation(), Some(4));
        assert_eq!(
            Phase::Backoff {
                generation: 4,
                until: Instant::now()
            }
            .generation(),
            Some(4)
        );
        assert_eq!(Phase::Idle.generation(), None);
        assert_eq!(Phase::Failed.generation(), None);
    }
}
