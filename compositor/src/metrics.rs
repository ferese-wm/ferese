use std::collections::VecDeque;
use std::time::{Duration, Instant};

use smithay::utils::{Physical, Rectangle};

const REPORT_INTERVAL: Duration = Duration::from_secs(5);
const MAX_FRAME_SAMPLES: usize = 512;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct FrameEffectMetrics;

#[derive(Debug)]
pub(crate) struct RenderMetrics {
    enabled: bool,
    output: String,
    interval_started: Instant,
    rendered_frames: u64,
    redraw_requests: u64,
    requests_scheduled: u64,
    requests_in_flight: u64,
    no_damage_frames: u64,
    no_damage_render_time: Duration,
    damaged_pixels: u64,
    total_render_time: Duration,
    longest_render_time: Duration,
    frame_times: VecDeque<Duration>,
}

impl RenderMetrics {
    pub(crate) fn from_environment(output: impl Into<String>) -> Self {
        Self::new(
            output,
            std::env::var_os("FERESE_TRACE_PERFORMANCE").is_some(),
            Instant::now(),
        )
    }

    fn new(output: impl Into<String>, enabled: bool, now: Instant) -> Self {
        Self {
            enabled,
            output: output.into(),
            interval_started: now,
            rendered_frames: 0,
            redraw_requests: 0,
            requests_scheduled: 0,
            requests_in_flight: 0,
            no_damage_frames: 0,
            no_damage_render_time: Duration::ZERO,
            damaged_pixels: 0,
            total_render_time: Duration::ZERO,
            longest_render_time: Duration::ZERO,
            frame_times: VecDeque::new(),
        }
    }

    pub(crate) fn record_request(&mut self, scheduled: bool, in_flight: bool) {
        if !self.enabled {
            return;
        }

        self.redraw_requests += 1;
        self.requests_scheduled += u64::from(scheduled);
        self.requests_in_flight += u64::from(in_flight);
    }

    pub(crate) fn record_frame(
        &mut self,
        render_time: Duration,
        damage: &[Rectangle<i32, Physical>],
        missed_deadlines: u64,
        _effects: FrameEffectMetrics,
    ) {
        if !self.enabled {
            return;
        }

        self.rendered_frames += 1;
        self.damaged_pixels += damage.iter().map(rectangle_area).sum::<u64>();
        self.total_render_time += render_time;
        self.longest_render_time = self.longest_render_time.max(render_time);
        if self.frame_times.len() == MAX_FRAME_SAMPLES {
            self.frame_times.pop_front();
        }
        self.frame_times.push_back(render_time);

        self.report_if_due(missed_deadlines);
    }

    pub(crate) fn record_no_damage(&mut self, render_time: Duration, missed_deadlines: u64) {
        if !self.enabled {
            return;
        }
        self.no_damage_frames += 1;
        self.no_damage_render_time += render_time;
        self.report_if_due(missed_deadlines);
    }

    pub(crate) fn enabled(&self) -> bool {
        self.enabled
    }

    pub(crate) fn report_if_due(&mut self, missed_deadlines: u64) {
        if !self.enabled {
            return;
        }
        let now = Instant::now();
        let elapsed = now.saturating_duration_since(self.interval_started);
        if elapsed < REPORT_INTERVAL {
            return;
        }

        let average_render_us = self
            .total_render_time
            .as_micros()
            .checked_div(u128::from(self.rendered_frames))
            .unwrap_or(0);
        self.frame_times.make_contiguous().sort_unstable();
        let percentile = |percent: usize| {
            let index = (self.frame_times.len() * percent)
                .div_ceil(100)
                .saturating_sub(1);
            self.frame_times
                .get(index)
                .copied()
                .unwrap_or_default()
                .as_micros()
        };
        tracing::info!(
            target: "ferese::render",
            output = %self.output,
            interval_ms = elapsed.as_millis(),
            frames = self.rendered_frames,
            redraw_requests = self.redraw_requests,
            requests_scheduled = self.requests_scheduled,
            requests_in_flight = self.requests_in_flight,
            no_damage_frames = self.no_damage_frames,
            no_damage_render_us = self.no_damage_render_time.as_micros(),
            damaged_pixels = self.damaged_pixels,
            average_render_us,
            longest_render_us = self.longest_render_time.as_micros(),
            percentile_samples = self.frame_times.len(),
            percentile_sample_limit = MAX_FRAME_SAMPLES,
            p95_render_us = percentile(95),
            p99_render_us = percentile(99),
            missed_deadlines,
            "render performance"
        );

        self.interval_started = now;
        self.rendered_frames = 0;
        self.redraw_requests = 0;
        self.requests_scheduled = 0;
        self.requests_in_flight = 0;
        self.no_damage_frames = 0;
        self.no_damage_render_time = Duration::ZERO;
        self.damaged_pixels = 0;
        self.total_render_time = Duration::ZERO;
        self.longest_render_time = Duration::ZERO;
        self.frame_times.clear();
    }
}

fn rectangle_area(rectangle: &Rectangle<i32, Physical>) -> u64 {
    u64::try_from(rectangle.size.w.max(0)).unwrap_or(0)
        * u64::try_from(rectangle.size.h.max(0)).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_metrics_do_not_accumulate() {
        let mut metrics = RenderMetrics::new("test", false, Instant::now());
        metrics.record_no_damage(Duration::from_millis(1), 0);

        metrics.record_frame(
            Duration::from_millis(2),
            &[Rectangle::new((0, 0).into(), (100, 50).into())],
            0,
            FrameEffectMetrics,
        );

        assert_eq!(metrics.rendered_frames, 0);
        assert_eq!(metrics.damaged_pixels, 0);
        assert_eq!(metrics.no_damage_frames, 0);
        assert_eq!(metrics.no_damage_render_time, Duration::ZERO);
    }

    #[test]
    fn redraw_requests_distinguish_scheduled_and_in_flight_coalescing() {
        let mut metrics = RenderMetrics::new("test", true, Instant::now());
        metrics.record_request(false, false);
        metrics.record_request(true, false);
        metrics.record_request(false, true);
        assert_eq!(metrics.redraw_requests, 3);
        assert_eq!(metrics.requests_scheduled, 1);
        assert_eq!(metrics.requests_in_flight, 1);
        assert_eq!(metrics.rendered_frames, 0);

        let mut disabled = RenderMetrics::new("test", false, Instant::now());
        disabled.record_request(true, true);
        assert_eq!(disabled.redraw_requests, 0);
    }

    #[test]
    fn no_damage_attempts_are_separate_from_presented_frame_statistics() {
        let mut metrics = RenderMetrics::new("test", true, Instant::now());
        metrics.record_no_damage(Duration::from_millis(2), 0);
        metrics.record_no_damage(Duration::from_millis(3), 0);
        assert_eq!(metrics.no_damage_frames, 2);
        assert_eq!(metrics.no_damage_render_time, Duration::from_millis(5));
        assert_eq!(metrics.rendered_frames, 0);
        assert!(metrics.frame_times.is_empty());

        // An entirely idle interval must report and reset without waiting for
        // a damaged frame or dividing by a zero presented-frame count.
        metrics.interval_started = Instant::now() - REPORT_INTERVAL;
        metrics.record_no_damage(Duration::from_millis(1), 0);
        assert_eq!(metrics.no_damage_frames, 0);
        assert_eq!(metrics.no_damage_render_time, Duration::ZERO);
    }

    #[test]
    fn enabled_metrics_count_frames_pixels_and_render_time() {
        let mut metrics = RenderMetrics::new("test", true, Instant::now());

        metrics.record_frame(
            Duration::from_millis(2),
            &[
                Rectangle::new((0, 0).into(), (100, 50).into()),
                Rectangle::new((10, 10).into(), (20, 10).into()),
            ],
            0,
            FrameEffectMetrics,
        );

        assert_eq!(metrics.rendered_frames, 1);
        assert_eq!(metrics.damaged_pixels, 5_200);
        assert_eq!(metrics.total_render_time, Duration::from_millis(2));
        assert_eq!(metrics.longest_render_time, Duration::from_millis(2));
        assert_eq!(metrics.frame_times, vec![Duration::from_millis(2)]);
    }

    #[test]
    fn profiling_samples_are_bounded() {
        let mut metrics = RenderMetrics::new("test", true, Instant::now());
        for _ in 0..2000 {
            metrics.record_frame(Duration::from_millis(1), &[], 0, FrameEffectMetrics);
        }
        assert_eq!(metrics.frame_times.len(), 512);
    }

    #[test]
    fn profiling_retains_the_most_recent_samples_in_order() {
        let mut metrics = RenderMetrics::new("test", true, Instant::now());
        for micros in 0..600 {
            metrics.record_frame(Duration::from_micros(micros), &[], 0, FrameEffectMetrics);
        }
        assert_eq!(
            metrics.frame_times.front(),
            Some(&Duration::from_micros(88))
        );
        assert_eq!(
            metrics.frame_times.back(),
            Some(&Duration::from_micros(599))
        );
        assert!(
            metrics
                .frame_times
                .iter()
                .copied()
                .eq((88..600).map(Duration::from_micros))
        );
    }
}
