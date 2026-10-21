//! Opt-in accounting of observable DRM results, independent of scanout policy.
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum PrimaryOutcome {
    DirectScanout,
    Composed,
    ReusedSwapchain,
}

impl PrimaryOutcome {
    pub fn classify(direct: bool, composition_damage: bool) -> Self {
        if direct {
            Self::DirectScanout
        } else if composition_damage {
            Self::Composed
        } else {
            Self::ReusedSwapchain
        }
    }
}

#[derive(Debug, Default, Eq, PartialEq)]
struct Counters {
    render_attempts: u64,
    primary_allowed: u64,
    blocked_locked: u64,
    blocked_animation: u64,
    blocked_not_fullscreen: u64,
    render_failures: u64,
    sync_failures: u64,
    no_damage_results: u64,
    queue_failures: u64,
    submissions: u64,
    scanout_submissions: u64,
    composed_submissions: u64,
    reused_swapchain_submissions: u64,
    allowed_composition_submissions: u64,
    cursor_plane_submissions: u64,
    no_cursor_plane_submissions: u64,
    retired_frames: u64,
    retirement_failures: u64,
    capture_batches: u64,
    capture_time: Duration,
}

#[derive(Debug)]
pub(super) struct Diagnostics {
    enabled: bool,
    output: String,
    started: Instant,
    counters: Counters,
}

impl Diagnostics {
    pub fn from_environment(output: String) -> Self {
        Self::new(
            output,
            std::env::var_os("FERESE_TRACE_PERFORMANCE").is_some(),
        )
    }

    fn new(output: String, enabled: bool) -> Self {
        Self {
            enabled,
            output,
            started: Instant::now(),
            counters: Counters::default(),
        }
    }

    pub fn attempt(&mut self, fullscreen: bool, animating: bool, locked: bool) {
        if !self.enabled {
            return;
        }
        self.counters.render_attempts += 1;
        // Exclusive diagnostic precedence only; the policy itself is unchanged.
        if locked {
            self.counters.blocked_locked += 1;
        } else if animating {
            self.counters.blocked_animation += 1;
        } else if !fullscreen {
            self.counters.blocked_not_fullscreen += 1;
        } else {
            self.counters.primary_allowed += 1;
        }
    }

    pub fn render_failed(&mut self) {
        if self.enabled {
            self.counters.render_failures += 1;
        }
    }

    pub fn sync_failed(&mut self) {
        if self.enabled {
            self.counters.sync_failures += 1;
        }
    }

    pub fn no_damage(&mut self) {
        if self.enabled {
            self.counters.no_damage_results += 1;
        }
    }

    pub fn queue_result<T, E>(
        &mut self,
        result: &Result<T, E>,
        primary: PrimaryOutcome,
        cursor_plane: bool,
        primary_allowed: bool,
    ) {
        if !self.enabled {
            return;
        }
        if result.is_err() {
            self.counters.queue_failures += 1;
            return;
        }
        self.counters.submissions += 1;
        match primary {
            PrimaryOutcome::DirectScanout => self.counters.scanout_submissions += 1,
            PrimaryOutcome::Composed => self.counters.composed_submissions += 1,
            PrimaryOutcome::ReusedSwapchain => self.counters.reused_swapchain_submissions += 1,
        }
        if primary_allowed && primary != PrimaryOutcome::DirectScanout {
            // Smithay selected a swapchain despite permission. This API does not
            // expose whether a candidate was tested, or why it was rejected.
            self.counters.allowed_composition_submissions += 1;
        }
        if cursor_plane {
            self.counters.cursor_plane_submissions += 1;
        } else {
            self.counters.no_cursor_plane_submissions += 1;
        }
    }

    pub fn retirement(&mut self, completed: bool, failed: bool) {
        if !self.enabled {
            return;
        }
        self.counters.retired_frames += u64::from(completed && !failed);
        self.counters.retirement_failures += u64::from(failed);
    }

    pub fn capture_start(&self, pending: impl FnOnce() -> bool) -> Option<Instant> {
        (self.enabled && pending()).then(Instant::now)
    }

    pub fn capture_finished(&mut self, start: Option<Instant>) {
        if let Some(start) = start {
            self.record_capture(start.elapsed());
        }
    }

    fn record_capture(&mut self, elapsed: Duration) {
        if !self.enabled {
            return;
        }
        self.counters.capture_batches += 1;
        self.counters.capture_time += elapsed;
    }

    pub fn report_if_due(&mut self) {
        if !self.enabled || self.started.elapsed() < Duration::from_secs(5) {
            return;
        }
        let c = &self.counters;
        tracing::info!(target: "ferese::render", output = %self.output,
            interval_ms = self.started.elapsed().as_millis(),
            render_attempts = c.render_attempts, primary_allowed = c.primary_allowed,
            blocked_locked = c.blocked_locked, blocked_animation = c.blocked_animation,
            blocked_not_fullscreen = c.blocked_not_fullscreen,
            render_failures = c.render_failures, sync_failures = c.sync_failures,
            no_damage_results = c.no_damage_results, queue_failures = c.queue_failures,
            submissions = c.submissions, scanout_submissions = c.scanout_submissions,
            composed_submissions = c.composed_submissions,
            reused_swapchain_submissions = c.reused_swapchain_submissions,
            allowed_composition_submissions = c.allowed_composition_submissions,
            cursor_plane_submissions = c.cursor_plane_submissions,
            no_cursor_plane_submissions = c.no_cursor_plane_submissions,
            retired_frames = c.retired_frames, retirement_failures = c.retirement_failures,
            capture_batches = c.capture_batches, capture_us = c.capture_time.as_micros(),
            "DRM frame outcomes");
        self.counters = Counters::default();
        self.started = Instant::now();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn primary_classification_does_not_infer_scanout_from_zero_damage() {
        assert_eq!(
            PrimaryOutcome::classify(true, false),
            PrimaryOutcome::DirectScanout
        );
        assert_eq!(
            PrimaryOutcome::classify(false, true),
            PrimaryOutcome::Composed
        );
        assert_eq!(
            PrimaryOutcome::classify(false, false),
            PrimaryOutcome::ReusedSwapchain
        );
    }

    #[test]
    fn policy_reasons_partition_attempts_with_documented_precedence() {
        let mut d = Diagnostics::new("test".into(), true);
        for fullscreen in [false, true] {
            for animating in [false, true] {
                for locked in [false, true] {
                    let allowed_before = d.counters.primary_allowed;
                    d.attempt(fullscreen, animating, locked);
                    assert_eq!(
                        d.counters.primary_allowed > allowed_before,
                        super::super::planes::frame_flags(fullscreen, animating, locked)
                            .contains(smithay::backend::drm::compositor::FrameFlags::ALLOW_PRIMARY_PLANE_SCANOUT),
                    );
                }
            }
        }
        assert_eq!(d.counters.render_attempts, 8);
        assert_eq!(d.counters.primary_allowed, 1);
        assert_eq!(d.counters.blocked_locked, 4);
        assert_eq!(d.counters.blocked_animation, 2);
        assert_eq!(d.counters.blocked_not_fullscreen, 1);
    }

    #[test]
    fn only_successful_queues_count_outcomes_and_retirement_is_separate() {
        let mut d = Diagnostics::new("test".into(), true);
        let queued = Err::<(), _>("queue failed");
        d.queue_result(&queued, PrimaryOutcome::DirectScanout, true, true);
        assert!(
            super::super::capture::after_queue(queued, || panic!("failed queues must not capture"))
                .is_err()
        );
        assert_eq!(d.counters.submissions, 0);
        assert_eq!(d.counters.scanout_submissions, 0);
        assert_eq!(d.counters.cursor_plane_submissions, 0);
        assert_eq!(d.counters.queue_failures, 1);
        for (primary, cursor, allowed) in [
            (PrimaryOutcome::DirectScanout, true, true),
            (PrimaryOutcome::Composed, false, true),
            (PrimaryOutcome::ReusedSwapchain, true, false),
        ] {
            d.queue_result(&Ok::<(), ()>(()), primary, cursor, allowed);
        }
        assert_eq!(d.counters.submissions, 3);
        assert_eq!(d.counters.scanout_submissions, 1);
        assert_eq!(d.counters.composed_submissions, 1);
        assert_eq!(d.counters.reused_swapchain_submissions, 1);
        assert_eq!(d.counters.allowed_composition_submissions, 1);
        assert_eq!(d.counters.cursor_plane_submissions, 2);
        assert_eq!(d.counters.no_cursor_plane_submissions, 1);
        assert_eq!(d.counters.retired_frames, 0);
        d.retirement(false, false);
        d.retirement(false, true);
        assert_eq!(d.counters.retired_frames, 0);
        d.retirement(true, false);
        assert_eq!(d.counters.retired_frames, 1);
        assert_eq!(d.counters.retirement_failures, 1);
    }

    #[test]
    fn no_damage_and_failures_never_count_as_submissions() {
        let mut d = Diagnostics::new("test".into(), true);
        d.no_damage();
        d.render_failed();
        d.sync_failed();
        d.record_capture(Duration::from_micros(123));
        assert_eq!(d.counters.no_damage_results, 1);
        assert_eq!(d.counters.render_failures, 1);
        assert_eq!(d.counters.sync_failures, 1);
        assert_eq!(d.counters.submissions, 0);
        assert_eq!(d.counters.capture_time, Duration::from_micros(123));
        assert_eq!(d.counters.capture_batches, 1);
    }

    #[test]
    fn disabled_accounting_is_inert_and_outputs_are_independent() {
        let mut d = Diagnostics::new("disabled".into(), false);
        d.attempt(true, false, false);
        d.render_failed();
        d.sync_failed();
        d.no_damage();
        d.queue_result(&Ok::<(), ()>(()), PrimaryOutcome::DirectScanout, true, true);
        d.retirement(true, false);
        d.record_capture(Duration::from_secs(1));
        assert_eq!(d.counters, Counters::default());
        assert!(
            d.capture_start(|| panic!("disabled diagnostics must not inspect capture requests"))
                .is_none()
        );
        let mut other = Diagnostics::new("other".into(), true);
        other.no_damage();
        assert!(other.capture_start(|| false).is_none());
        assert_eq!(d.counters.no_damage_results, 0);
        assert_eq!(other.counters.no_damage_results, 1);
        other.started = Instant::now() - Duration::from_secs(6);
        other.report_if_due();
        assert_eq!(other.counters, Counters::default());
    }
}
