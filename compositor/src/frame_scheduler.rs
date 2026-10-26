//! Presentation scheduling policy, independent of DRM and the event loop.
use std::time::Duration;

const MIN_RENDER: Duration = Duration::from_millis(2);
const TIMER_MARGIN: Duration = Duration::from_millis(1);
const WAKEUP_DECAY: Duration = Duration::from_secs(1);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct FramePlan {
    pub render_at: Duration,
    pub present_at: Duration,
    pub requested_at: Duration,
    pub phase_known: bool,
}

#[derive(Debug)]
pub(crate) struct FrameScheduler {
    interval: Duration,
    variable_refresh: bool,
    tearing: bool,
    last_presented: Option<Duration>,
    last_target: Option<Duration>,
    requested_at: Option<Duration>,
    scheduled: Option<FramePlan>,
    submitted: Option<FramePlan>,
    last_callback: Option<Duration>,
    render_estimate: Duration,
    render_variance: Duration,
    last_sample: Option<Duration>,
    extra_margin: Duration,
    wakeup_delay: Duration,
    last_wakeup: Option<Duration>,
    pub missed_deadlines: u64,
}

impl FrameScheduler {
    pub fn new(interval: Duration) -> Self {
        Self {
            interval: interval.max(Duration::from_micros(1)),
            variable_refresh: false,
            tearing: false,
            last_presented: None,
            last_target: None,
            requested_at: None,
            scheduled: None,
            submitted: None,
            last_callback: None,
            render_estimate: MIN_RENDER,
            render_variance: Duration::ZERO,
            last_sample: None,
            extra_margin: Duration::ZERO,
            wakeup_delay: Duration::ZERO,
            last_wakeup: None,
            missed_deadlines: 0,
        }
    }

    pub fn reset(&mut self, interval: Duration) {
        let missed = self.missed_deadlines;
        let variable_refresh = self.variable_refresh;
        let tearing = self.tearing;
        *self = Self::new(interval);
        self.variable_refresh = variable_refresh;
        self.tearing = tearing;
        self.missed_deadlines = missed;
    }

    /// The interval is the maximum refresh rate's minimum spacing in variable mode.
    /// Keep in-flight ownership while discarding predictions from the old clock policy.
    pub fn set_variable_refresh(&mut self, enabled: bool) {
        if self.variable_refresh == enabled {
            return;
        }
        self.variable_refresh = enabled;
        if enabled {
            self.tearing = false;
        }
        self.last_presented = None;
        self.last_target = None;
        self.last_callback = None;
        self.scheduled = None;
        self.extra_margin = Duration::ZERO;
        self.wakeup_delay = Duration::ZERO;
        self.last_wakeup = None;
    }

    /// Return whether an obsolete render timer must be removed by the caller.
    pub fn set_tearing(&mut self, enabled: bool) -> bool {
        let enabled = enabled && !self.variable_refresh;
        if self.tearing == enabled {
            return false;
        }
        self.tearing = enabled;
        self.last_presented = None;
        self.last_target = None;
        self.last_callback = None;
        self.scheduled = None;
        self.extra_margin = Duration::ZERO;
        self.wakeup_delay = Duration::ZERO;
        self.last_wakeup = None;
        true
    }

    pub fn request(&mut self, now: Duration) {
        self.requested_at.get_or_insert(now);
    }

    pub fn has_pending_request(&self) -> bool {
        self.requested_at.is_some() || self.scheduled.is_some()
    }

    fn budget(&self, now: Duration) -> Duration {
        let idle = self
            .last_presented
            .is_none_or(|last| now.saturating_sub(last) >= self.interval * 100);
        if idle {
            // A cold GPU is less predictable. Start immediately on idle wakeup.
            self.interval
        } else {
            (self.render_estimate
                + self.render_variance * 2
                + TIMER_MARGIN
                + self.extra_margin
                + self.wakeup_margin(now))
            .min(self.interval)
        }
    }

    fn wakeup_margin(&self, now: Duration) -> Duration {
        if self.wakeup_delay.is_zero() {
            return Duration::ZERO;
        }
        let elapsed = self
            .last_wakeup
            .map_or(Duration::ZERO, |last| now.saturating_sub(last));
        // Forget transient contention by elapsed time, independent of refresh rate.
        self.wakeup_delay
            .mul_f64((-elapsed.as_secs_f64() / WAKEUP_DECAY.as_secs_f64()).exp())
    }

    fn phase_at_or_after(&self, time: Duration) -> Duration {
        let Some(last) = self.last_presented else {
            return time;
        };

        let count = time
            .saturating_sub(last)
            .as_nanos()
            .div_ceil(self.interval.as_nanos())
            .max(1);
        last + self.interval.mul_f64(count as f64)
    }

    pub fn plan(&mut self, now: Duration) -> Option<FramePlan> {
        if self.submitted.is_some() {
            return None;
        }

        if let Some(plan) = self.scheduled {
            return Some(plan);
        }

        let requested_at = self.requested_at?;
        if self
            .last_presented
            .is_some_and(|last| last > now + self.interval)
        {
            self.last_presented = None;
            self.last_target = None;
        }

        let budget = self.budget(now);
        if self.tearing {
            let plan = FramePlan {
                render_at: now,
                present_at: now + self.render_estimate,
                requested_at,
                phase_known: false,
            };
            self.scheduled = Some(plan);
            return Some(plan);
        }
        if self.variable_refresh {
            // There is no fixed vblank phase to round to. A late frame can start
            // a refresh as soon as it is ready, subject to the panel's maximum rate.
            let present_at = (now + budget)
                .max(self.last_presented.map_or(now, |last| last + self.interval))
                .max(self.last_target.map_or(now, |last| last + self.interval));
            let plan = FramePlan {
                render_at: present_at.saturating_sub(budget).max(now),
                present_at,
                requested_at,
                phase_known: false,
            };
            self.scheduled = Some(plan);
            return Some(plan);
        }
        let idle = self
            .last_presented
            .is_none_or(|last| now.saturating_sub(last) >= self.interval * 100);
        // With one in-flight GBM frame, submitting immediately can hit the
        // nearest vblank even when the conservative budget is larger than
        // the remaining time. Do not deliberately skip that presentation.
        let next = if self.last_presented.is_some() {
            now + Duration::from_nanos(1)
        } else {
            now + self.interval
        };

        let earliest = next.max(self.last_target.map_or(next, |last| last + self.interval));
        let present_at = self.phase_at_or_after(earliest);
        let plan = FramePlan {
            render_at: if idle {
                now
            } else {
                present_at.saturating_sub(budget).max(now)
            },
            present_at,
            requested_at,
            phase_known: self.last_presented.is_some(),
        };

        self.scheduled = Some(plan);
        Some(plan)
    }

    pub fn begin(&mut self, now: Duration) -> Option<FramePlan> {
        let plan = self.scheduled.take()?;
        if plan.phase_known {
            // Rendering cost starts after dispatch. Account separately for time
            // spent waiting for a CPU or for earlier event-loop callbacks.
            let late = now.saturating_sub(plan.render_at).min(self.interval);
            self.wakeup_delay = self.wakeup_margin(now).max(late);
            self.last_wakeup = Some(now);
        }
        self.requested_at = None;
        self.last_target = Some(plan.present_at);
        Some(plan)
    }

    pub fn rendered(&mut self, plan: FramePlan, now: Duration, cost: Duration, submitted: bool) {
        if submitted {
            self.submitted = Some(plan);
            let elapsed = self
                .last_sample
                .map_or(Duration::from_millis(500), |last| now.saturating_sub(last));
            self.last_sample = Some(now);
            let sample = cost.max(MIN_RENDER);
            let ratio = (elapsed.as_secs_f64() / 0.5).clamp(0.01, 1.0);
            let deviation = sample.saturating_sub(self.render_estimate);
            let variance_ratio = (elapsed.as_secs_f64() / 6.0).clamp(0.001, 0.1);
            self.render_variance = self
                .render_variance
                .mul_f64(1.0 - variance_ratio)
                .max(deviation);
            self.render_estimate =
                self.render_estimate.mul_f64(1.0 - ratio) + sample.mul_f64(ratio);
        }
    }

    pub fn presented(&mut self, timestamp: Duration) -> Option<FramePlan> {
        // Regressive or implausibly early events must not shift the phase backward.
        if self.last_presented.is_some_and(|last| timestamp < last) {
            self.last_presented = None;
            self.last_target = None;
            return self.submitted.take();
        }

        self.last_presented = Some(timestamp);
        let plan = self.submitted.take()?;
        // The first submission has only an estimated phase. Retiring it on
        // the actual refresh must not carry that estimate into the next frame.
        self.last_target = Some(timestamp);
        if self.variable_refresh || self.tearing {
            // Variable refresh timestamps cannot measure missed fixed refresh slots.
            self.extra_margin = Duration::ZERO;
            return Some(plan);
        }
        let late = timestamp.saturating_sub(plan.present_at);
        if plan.phase_known && late > self.interval / 2 {
            let missed =
                (late.as_nanos() + self.interval.as_nanos() / 2) / self.interval.as_nanos();
            self.missed_deadlines = self.missed_deadlines.saturating_add(missed as u64);
            // CPU submission timings may omit asynchronous GPU work. Increase
            // the margin promptly after a real miss; relax it slowly afterward.
            self.extra_margin = (self.extra_margin + TIMER_MARGIN).min(self.interval / 2);
        } else {
            self.extra_margin = self.extra_margin.mul_f64(0.98);
        }

        Some(plan)
    }

    pub fn retire_unknown(&mut self) {
        self.submitted = None;
        self.last_presented = None;
        self.last_target = None;
    }

    pub fn presented_with_phase(
        &mut self,
        timestamp: Duration,
        fixed_phase: bool,
    ) -> Option<FramePlan> {
        let plan = self.presented(timestamp);
        if !fixed_phase && !self.variable_refresh && !self.tearing {
            // An older variable/async frame may retire after policy changed back
            // to fixed refresh. Its timestamp cannot establish a vblank phase.
            self.last_presented = None;
            self.last_target = None;
        }
        plan
    }

    pub fn forecast_time(&self, now: Duration, plan: FramePlan) -> Option<Duration> {
        // Before the first real page flip there is no reliable refresh phase.
        // A late timer still samples the next physical presentation, while
        // deadline metrics retain the original scheduled target.
        (!self.variable_refresh && !self.tearing && plan.phase_known).then(|| {
            plan.present_at
                .max(self.phase_at_or_after(now + Duration::from_nanos(1)))
        })
    }

    pub fn callback_deadline(&self, now: Duration, target: Duration) -> Option<Duration> {
        // A timer from a callback-only update must yield to a later submission.
        // Its page flip will release callbacks using the observed refresh phase.
        if self.submitted.is_some() {
            return None;
        }

        if self.tearing {
            let observed_progress = self
                .last_presented
                .is_some_and(|presented| self.last_callback.is_none_or(|last| presented > last));
            let earliest = if observed_progress {
                self.last_presented.map_or(now, |last| last.max(now))
            } else {
                self.last_callback
                    .map_or(now, |last| last + self.interval)
                    .max(now)
            };
            return Some(earliest.max(target));
        }

        let tolerance = Duration::from_micros(500).min(self.interval / 8);
        if target <= now
            && self
                .last_callback
                .is_none_or(|last| last + self.interval <= now + tolerance)
        {
            return Some(now);
        }

        let earliest = self
            .last_callback
            .map_or(now, |last| last + self.interval)
            .max(target)
            .max(now);
        if self.variable_refresh {
            return Some(earliest);
        }
        // DRM may report a completed flip slightly before its presentation
        // timestamp. Wait for that refresh, rather than advancing a full slot.
        if let Some(presented) = self.last_presented
            && earliest <= presented + tolerance
        {
            return Some(earliest.max(presented));
        }
        Some(self.phase_at_or_after(earliest))
    }

    pub fn callback_sent(&mut self, now: Duration) {
        if self.variable_refresh || self.tearing {
            self.last_callback = Some(self.last_presented.map_or(now, |last| last.max(now)));
            return;
        }
        // Record the refresh slot, not delayed event-loop dispatch time.
        // Otherwise a callback delivered a few microseconds late can make
        // the next refresh look too early and halve a client's frame rate.
        self.last_callback = Some(self.last_presented.map_or(now, |last| {
            let tolerance = Duration::from_micros(500).min(self.interval / 8);
            let slots =
                (now.saturating_sub(last) + tolerance).as_nanos() / self.interval.as_nanos();
            last + self.interval.mul_f64(slots as f64)
        }));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(value: u64) -> Duration {
        Duration::from_millis(value)
    }

    #[test]
    fn requests_coalesce_without_postponing_the_frame_and_wait_for_retirement() {
        let mut scheduler = FrameScheduler::new(ms(10));
        scheduler.presented(ms(100));
        scheduler.request(ms(101));
        let plan = scheduler.plan(ms(101)).unwrap();
        assert_eq!((plan.render_at, plan.present_at), (ms(107), ms(110)));
        scheduler.request(ms(102));
        assert_eq!(scheduler.plan(ms(102)), Some(plan));
        assert_eq!(scheduler.begin(plan.render_at), Some(plan));
        scheduler.rendered(plan, ms(108), ms(1), true);
        scheduler.request(ms(109));
        assert_eq!(scheduler.plan(ms(109)), None);
        scheduler.presented(ms(110));
        assert_eq!(scheduler.plan(ms(110)).unwrap().present_at, ms(120));
    }

    #[test]
    fn idle_wakeup_starts_early_and_is_not_a_missed_deadline() {
        let mut scheduler = FrameScheduler::new(ms(10));
        scheduler.presented(ms(100));
        scheduler.request(ms(10_100));
        let plan = scheduler.plan(ms(10_100)).unwrap();
        assert_eq!(plan.render_at, ms(10_100));
        scheduler.begin(plan.render_at);
        scheduler.rendered(plan, ms(10_101), ms(1), true);
        scheduler.presented(ms(10_110));
        assert_eq!(scheduler.missed_deadlines, 0);
    }

    #[test]
    fn misses_measure_the_submitted_target_and_expand_the_budget() {
        let mut scheduler = FrameScheduler::new(ms(10));
        scheduler.presented(ms(100));
        scheduler.request(ms(101));
        let plan = scheduler.plan(ms(101)).unwrap();
        scheduler.begin(plan.render_at);
        scheduler.rendered(plan, ms(108), ms(1), true);
        assert_eq!(scheduler.presented(ms(130)), Some(plan));
        assert_eq!(scheduler.missed_deadlines, 2);
        assert_eq!(scheduler.budget(ms(131)), ms(4));
        assert_eq!(scheduler.presented(ms(129)), None);
    }

    #[test]
    fn late_timer_dispatch_moves_the_next_render_earlier_without_skipping_a_refresh() {
        let mut scheduler = FrameScheduler::new(ms(10));
        scheduler.presented(ms(100));
        scheduler.request(ms(101));
        let first = scheduler.plan(ms(101)).unwrap();
        assert_eq!(first.render_at, ms(107));
        let dispatch = first.render_at + ms(3);
        assert_eq!(scheduler.begin(dispatch), Some(first));
        scheduler.rendered(first, dispatch + ms(2), ms(2), true);
        scheduler.presented(ms(120));
        assert_eq!(scheduler.missed_deadlines, 1);

        scheduler.request(ms(120));
        let next = scheduler.plan(ms(120)).unwrap();
        assert_eq!(next.present_at, ms(130), "retain the next physical refresh");
        assert!(
            next.render_at + ms(3) + ms(2) <= next.present_at - TIMER_MARGIN,
            "the next frame must budget both the observed wakeup delay and rendering"
        );
        assert_eq!(
            scheduler.plan(ms(121)),
            Some(next),
            "new input must not postpone the plan"
        );
    }

    #[test]
    fn wakeup_history_decays_by_time_is_bounded_and_resets_with_the_output() {
        let mut fast = FrameScheduler::new(ms(5));
        let mut slow = FrameScheduler::new(ms(20));
        let mut margins = Vec::new();
        for scheduler in [&mut fast, &mut slow] {
            scheduler.presented(ms(100));
            scheduler.request(ms(101));
            let plan = scheduler.plan(ms(101)).unwrap();
            let now = plan.render_at + ms(3);
            scheduler.begin(now);
            assert_eq!(scheduler.wakeup_margin(now), ms(3));
            margins.push(scheduler.wakeup_margin(now + ms(1000)));
            assert!(scheduler.wakeup_margin(now + ms(5000)) < Duration::from_micros(21));
            scheduler.reset(ms(10));
            assert_eq!(scheduler.wakeup_margin(now), Duration::ZERO);
        }
        assert_eq!(
            margins[0], margins[1],
            "decay must not depend on display refresh rate"
        );
        let mut scheduler = FrameScheduler::new(ms(10));
        scheduler.presented(ms(100));
        scheduler.request(ms(101));
        let plan = scheduler.plan(ms(101)).unwrap();
        let now = plan.render_at + ms(200);
        scheduler.begin(now);
        assert_eq!(scheduler.wakeup_margin(now), ms(10));
        assert!(scheduler.budget(now) <= ms(10));
    }

    #[test]
    fn unknown_phase_does_not_train_wakeup_history_or_move_a_pending_plan() {
        let mut scheduler = FrameScheduler::new(ms(10));
        scheduler.request(ms(100));
        let plan = scheduler.plan(ms(100)).unwrap();
        assert!(!plan.phase_known);
        assert_eq!(scheduler.plan(ms(120)), Some(plan));
        scheduler.begin(ms(130));
        assert_eq!(scheduler.wakeup_margin(ms(130)), Duration::ZERO);
    }

    #[test]
    fn first_retirement_aligns_the_next_frame_to_the_observed_refresh() {
        let mut scheduler = FrameScheduler::new(ms(10));
        scheduler.request(ms(100));
        let plan = scheduler.plan(ms(100)).unwrap();
        scheduler.begin(plan.render_at);
        scheduler.rendered(plan, ms(102), ms(2), true);
        scheduler.presented(ms(103));

        scheduler.request(ms(104));
        assert_eq!(scheduler.plan(ms(104)).unwrap().present_at, ms(113));
    }

    #[test]
    fn no_damage_attempts_and_callbacks_do_not_spin_within_a_refresh() {
        let mut scheduler = FrameScheduler::new(ms(10));
        scheduler.presented(ms(100));
        scheduler.request(ms(101));
        let plan = scheduler.plan(ms(101)).unwrap();
        scheduler.begin(plan.render_at);
        scheduler.rendered(plan, ms(108), ms(1), false);
        scheduler.callback_sent(ms(110));
        scheduler.request(ms(108));
        assert_eq!(scheduler.plan(ms(108)).unwrap().present_at, ms(120));
        assert_eq!(
            scheduler.callback_deadline(ms(111), ms(110)).unwrap(),
            ms(120)
        );
    }

    #[test]
    fn callback_only_timer_waits_for_a_later_submission_even_when_it_misses_its_target() {
        let mut scheduler = FrameScheduler::new(ms(10));
        scheduler.presented(ms(100));
        scheduler.request(ms(101));
        let unchanged = scheduler.plan(ms(101)).unwrap();
        scheduler.begin(unchanged.render_at);
        scheduler.rendered(unchanged, ms(108), ms(1), false);
        let callback_at = scheduler
            .callback_deadline(ms(108), unchanged.present_at)
            .unwrap();
        assert_eq!(callback_at, ms(110));

        scheduler.request(ms(109));
        let changed = scheduler.plan(ms(109)).unwrap();
        scheduler.begin(changed.render_at);
        scheduler.rendered(changed, ms(118), ms(1), true);

        // The old timer is dispatched late, after a new frame was queued.
        // Neither that timer nor the predicted refresh can release callbacks.
        assert_eq!(scheduler.callback_deadline(ms(118), callback_at), None);
        assert_eq!(
            scheduler.callback_deadline(ms(120), changed.present_at),
            None
        );
        assert_eq!(scheduler.callback_deadline(ms(129), callback_at), None);

        scheduler.presented(ms(130));
        assert_eq!(scheduler.callback_deadline(ms(130), ms(130)), Some(ms(130)));
        scheduler.callback_sent(ms(130));
        assert_eq!(
            scheduler.callback_deadline(ms(130), callback_at),
            Some(ms(140))
        );
    }

    #[test]
    fn outputs_keep_independent_phases_and_reset_cancels_obsolete_work() {
        let mut fast = FrameScheduler::new(ms(4));
        let mut slow = FrameScheduler::new(ms(16));
        for scheduler in [&mut fast, &mut slow] {
            scheduler.presented(ms(100));
            scheduler.request(ms(101));
        }

        assert_eq!(fast.plan(ms(101)).unwrap().present_at, ms(104));
        assert_eq!(slow.plan(ms(101)).unwrap().present_at, ms(116));
        fast.reset(ms(8));
        assert_eq!(fast.plan(ms(102)), None);
    }

    #[test]
    fn callback_dispatch_jitter_does_not_skip_a_refresh_or_duplicate_a_callback() {
        let mut scheduler = FrameScheduler::new(ms(10));
        scheduler.presented(ms(100));
        scheduler.callback_sent(ms(100) + Duration::from_micros(80));
        scheduler.presented(ms(110));
        let now = ms(110) + Duration::from_micros(10);
        assert_eq!(scheduler.callback_deadline(now, now).unwrap(), now);
        scheduler.callback_sent(now);
        assert_eq!(scheduler.callback_deadline(now, now).unwrap(), ms(120));
    }

    #[test]
    fn early_page_flip_notification_releases_callbacks_in_the_current_refresh() {
        let interval = Duration::from_nanos(1_000_000_000 / 60);
        for early_us in [80, 499, 501, 700, 1_000] {
            let mut scheduler = FrameScheduler::new(interval);
            let mut presented = Duration::from_secs(1);
            scheduler.presented(presented);
            scheduler.callback_sent(presented + Duration::from_micros(80));
            for _ in 0..120 {
                presented += interval;
                scheduler.presented(presented);
                let dispatch = presented - Duration::from_micros(early_us);
                let deadline = scheduler.callback_deadline(dispatch, dispatch).unwrap();
                assert!(
                    deadline <= presented,
                    "{early_us} us early: callback delayed by {:?}",
                    deadline.saturating_sub(presented),
                );
                assert_eq!(
                    scheduler.callback_deadline(deadline, deadline),
                    Some(deadline)
                );
                scheduler.callback_sent(deadline);
                assert!(
                    scheduler.callback_deadline(deadline, deadline).unwrap()
                        >= presented + interval,
                    "one presentation must not release callbacks twice",
                );
            }
        }
    }

    #[test]
    fn unknown_retirement_and_regressive_timestamps_do_not_strand_pending_work() {
        let mut scheduler = FrameScheduler::new(ms(10));
        scheduler.presented(ms(100));
        scheduler.request(ms(101));
        let plan = scheduler.plan(ms(101)).unwrap();
        scheduler.begin(plan.render_at);
        scheduler.rendered(plan, ms(108), ms(1), true);
        scheduler.request(ms(109));
        assert_eq!(scheduler.presented(ms(90)), Some(plan));
        assert!(scheduler.plan(ms(110)).is_some());
        scheduler.retire_unknown();
        assert!(scheduler.plan(ms(111)).is_some());
    }

    #[test]
    fn cold_gpu_and_late_dispatch_do_not_predict_a_past_or_deliberately_skipped_frame() {
        let mut scheduler = FrameScheduler::new(ms(10));
        scheduler.presented(ms(100));
        scheduler.request(ms(10_101));
        let plan = scheduler.plan(ms(10_101)).unwrap();
        assert_eq!(plan.render_at, ms(10_101));
        assert_eq!(plan.present_at, ms(10_110));
        assert_eq!(scheduler.forecast_time(ms(10_111), plan), Some(ms(10_120)));

        let mut unknown = FrameScheduler::new(ms(10));
        unknown.request(ms(100));
        let plan = unknown.plan(ms(100)).unwrap();
        assert_eq!(unknown.forecast_time(ms(101), plan), None);
        unknown.begin(plan.render_at);
        unknown.rendered(plan, ms(102), ms(2), true);
        unknown.presented(ms(130));
        assert_eq!(
            unknown.missed_deadlines, 0,
            "unknown phase is not a measurable missed deadline"
        );
    }

    #[test]
    fn variable_refresh_uses_minimum_spacing_without_rounding_late_work_to_a_phase() {
        let mut scheduler = FrameScheduler::new(ms(10));
        scheduler.set_variable_refresh(true);
        scheduler.presented(ms(100));
        scheduler.request(ms(101));
        let first = scheduler.plan(ms(101)).unwrap();
        assert_eq!(first.present_at, ms(110));
        assert_eq!(first.render_at, ms(107));
        assert_eq!(scheduler.forecast_time(ms(108), first), None);
        scheduler.begin(first.render_at);
        scheduler.rendered(first, ms(109), ms(2), true);
        scheduler.presented(ms(114));
        scheduler.request(ms(135));
        let late = scheduler.plan(ms(135)).unwrap();
        assert_eq!(late.render_at, ms(135));
        assert!(
            late.present_at < ms(144),
            "do not wait for an obsolete phase"
        );
        scheduler.begin(late.render_at);
        scheduler.rendered(late, ms(137), ms(2), true);
        scheduler.presented(ms(190));
        assert_eq!(scheduler.missed_deadlines, 0);
    }

    #[test]
    fn refresh_policy_transition_preserves_in_flight_ownership_and_resets_phase() {
        let mut scheduler = FrameScheduler::new(ms(10));
        scheduler.presented(ms(100));
        scheduler.request(ms(101));
        let plan = scheduler.plan(ms(101)).unwrap();
        scheduler.begin(plan.render_at);
        scheduler.rendered(plan, ms(109), ms(2), true);
        scheduler.set_variable_refresh(true);
        scheduler.request(ms(112));
        assert_eq!(scheduler.plan(ms(112)), None);
        assert_eq!(scheduler.presented(ms(115)), Some(plan));
        assert!(scheduler.plan(ms(116)).is_some());
        scheduler.reset(ms(8));
        scheduler.request(ms(130));
        let plan = scheduler.plan(ms(130)).unwrap();
        assert_eq!(plan.render_at, ms(130));
        assert!(!plan.phase_known);
        scheduler.set_variable_refresh(false);
        let plan = scheduler.plan(ms(131)).unwrap();
        assert!(!plan.phase_known);
    }

    #[test]
    fn variable_callback_only_work_is_rate_limited_without_fixed_slot_quantization() {
        let mut scheduler = FrameScheduler::new(ms(10));
        scheduler.set_variable_refresh(true);
        scheduler.presented(ms(100));
        scheduler.callback_sent(ms(102));
        assert_eq!(scheduler.callback_deadline(ms(103), ms(103)), Some(ms(112)));
        assert_eq!(scheduler.callback_deadline(ms(135), ms(135)), Some(ms(135)));
        scheduler.callback_sent(ms(135));
        assert_eq!(scheduler.callback_deadline(ms(135), ms(135)), Some(ms(145)));
    }

    #[test]
    fn tearing_starts_immediately_and_retires_before_scheduling_another_frame() {
        let mut scheduler = FrameScheduler::new(ms(10));
        scheduler.presented(ms(100));
        scheduler.request(ms(101));
        let fixed = scheduler.plan(ms(101)).unwrap();
        assert!(fixed.render_at > ms(101));
        assert!(scheduler.set_tearing(true));
        assert!(!scheduler.set_tearing(true));
        let plan = scheduler.plan(ms(102)).unwrap();
        assert_eq!(plan.render_at, ms(102));
        assert_eq!(scheduler.forecast_time(ms(102), plan), None);
        scheduler.begin(plan.render_at);
        scheduler.rendered(plan, ms(104), ms(2), true);
        scheduler.request(ms(105));
        assert_eq!(scheduler.plan(ms(105)), None);
        scheduler.presented(ms(106));
        assert_eq!(scheduler.plan(ms(106)).unwrap().render_at, ms(106));
        assert_eq!(scheduler.missed_deadlines, 0);
    }

    #[test]
    fn asynchronous_retirements_release_callbacks_but_no_damage_updates_cannot_spin() {
        let mut scheduler = FrameScheduler::new(ms(10));
        scheduler.set_tearing(true);
        scheduler.presented(ms(100));
        scheduler.callback_sent(ms(100));
        assert_eq!(scheduler.callback_deadline(ms(101), ms(101)), Some(ms(110)));
        scheduler.presented(ms(103));
        assert_eq!(scheduler.callback_deadline(ms(103), ms(103)), Some(ms(103)));
        scheduler.callback_sent(ms(103));
        assert_eq!(scheduler.callback_deadline(ms(104), ms(104)), Some(ms(113)));
        scheduler.presented(ms(106));
        assert_eq!(scheduler.callback_deadline(ms(105), ms(105)), Some(ms(106)));
        scheduler.callback_sent(ms(106));
        assert_eq!(scheduler.callback_deadline(ms(106), ms(106)), Some(ms(116)));
    }

    #[test]
    fn variable_refresh_and_tearing_are_mutually_exclusive_across_resets() {
        let mut scheduler = FrameScheduler::new(ms(10));
        scheduler.set_tearing(true);
        scheduler.set_variable_refresh(true);
        assert!(!scheduler.set_tearing(true));
        scheduler.reset(ms(8));
        scheduler.request(ms(100));
        let plan = scheduler.plan(ms(100)).unwrap();
        assert_eq!(plan.present_at, ms(108));
        assert!(!scheduler.tearing);
        scheduler.set_variable_refresh(false);
        assert!(scheduler.set_tearing(true));
        assert_eq!(scheduler.plan(ms(101)).unwrap().render_at, ms(101));
    }

    #[test]
    fn older_async_or_variable_retirements_cannot_seed_a_new_fixed_refresh_phase() {
        for variable in [false, true] {
            let mut scheduler = FrameScheduler::new(ms(10));
            if variable {
                scheduler.set_variable_refresh(true);
            } else {
                scheduler.set_tearing(true);
            }
            scheduler.request(ms(100));
            let plan = scheduler.plan(ms(100)).unwrap();
            scheduler.begin(plan.render_at);
            scheduler.rendered(plan, ms(102), ms(2), true);
            scheduler.set_variable_refresh(false);
            scheduler.set_tearing(false);
            scheduler.request(ms(103));
            assert_eq!(scheduler.presented_with_phase(ms(105), false), Some(plan));
            let fixed = scheduler.plan(ms(106)).unwrap();
            assert!(!fixed.phase_known);
            scheduler.begin(fixed.render_at);
            scheduler.rendered(fixed, ms(108), ms(2), true);
            scheduler.presented_with_phase(ms(110), true);
            scheduler.request(ms(111));
            assert!(scheduler.plan(ms(111)).unwrap().phase_known);
        }
    }
}
