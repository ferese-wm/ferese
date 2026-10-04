//! Redraw/fallback measurements and the orphaned-animation watchdog policy.
use std::collections::HashMap;
use std::panic::Location;
use std::time::{Duration, Instant};

#[derive(Default)]
pub(super) struct AnimationFallback {
    orphaned_since: Option<Instant>,
}

impl AnimationFallback {
    pub fn waiting_for_frame(&self) -> bool {
        self.orphaned_since.is_some()
    }

    pub fn deadline(&mut self, now: Instant, interval: Duration, active: bool, chained: bool) -> Option<Instant> {
        if !active || chained {
            self.orphaned_since = None;
            return None;
        }

        Some(*self.orphaned_since.get_or_insert(now) + interval.saturating_mul(2))
    }
}

#[derive(Debug, Default)]
struct RedrawCount {
    calls: u64,
    requested_outputs: u64,
    connected_outputs: u64,
}

pub(super) struct SchedulingMetrics {
    enabled: bool,
    started: Instant,
    redraws: HashMap<(&'static str, u32, bool), RedrawCount>,
    pub fallback_arms: u64,
    pub fallback_cancels: u64,
    pub fallback_wakes: u64,
    pub fallback_recoveries: u64,
    pub chained_animation_observations: u64,
}

impl SchedulingMetrics {
    pub fn new() -> Self {
        Self {
            enabled: std::env::var_os("FERESE_TRACE_PERFORMANCE").is_some(),
            started: Instant::now(),
            redraws: HashMap::new(),
            fallback_arms: 0,
            fallback_cancels: 0,
            fallback_wakes: 0,
            fallback_recoveries: 0,
            chained_animation_observations: 0,
        }
    }

    pub fn redraw(&mut self, source: &'static Location<'static>, global: bool, requested: usize, connected: usize) {
        if !self.enabled {
            return;
        }

        let count = self.redraws.entry((source.file(), source.line(), global)).or_default();
        count.calls += 1;
        count.requested_outputs += requested as u64;
        count.connected_outputs += connected as u64;
        if self.started.elapsed() >= Duration::from_secs(5) {
            self.dump();
            self.started = Instant::now();
            self.redraws.clear();
            self.fallback_arms = 0;
            self.fallback_cancels = 0;
            self.fallback_wakes = 0;
            self.fallback_recoveries = 0;
            self.chained_animation_observations = 0;
        }
    }

    pub fn dump(&self) {
        if !self.enabled {
            return;
        }

        for ((file, line, global), count) in &self.redraws {
            tracing::info!(target: "ferese::render", file, line, global,
                calls = count.calls, requested_outputs = count.requested_outputs,
                connected_outputs = count.connected_outputs, "redraw requests");
        }

        tracing::info!(target: "ferese::render", fallback_arms = self.fallback_arms,
            fallback_cancels = self.fallback_cancels, fallback_wakes = self.fallback_wakes,
            fallback_recoveries = self.fallback_recoveries,
            chained_animation_observations = self.chained_animation_observations,
            "animation fallback scheduling");
    }
}

pub(super) fn cursor_on_output(state: &crate::Ferese, output: &smithay::output::Output) -> bool {
    use smithay::input::pointer::{CursorImageStatus, CursorImageSurfaceData};
    use smithay::utils::{Logical, Rectangle};
    use smithay::wayland::compositor::with_states;
    if state.input_capture.active() {
        return false;
    }

    let Some(pointer) = state.seat.get_pointer() else {
        return false;
    };
    let Some(bounds) = state.space.output_geometry(output) else {
        return false;
    };
    let location = pointer.current_location();
    let rect = match &state.cursor_status {
        CursorImageStatus::Surface(surface) => {
            let hotspot = with_states(surface, |states| {
                states
                    .data_map
                    .get::<CursorImageSurfaceData>()
                    .map(|attributes| attributes.lock().unwrap().hotspot)
                    .unwrap_or_default()
            });
            smithay::desktop::utils::bbox_from_surface_tree(surface, (location - hotspot.to_f64()).to_i32_round())
                .to_f64()
        }

        CursorImageStatus::Named(_) => {
            let Some(cursor) = state.named_cursor_frame() else {
                return false;
            };
            cursor.logical_rect(location)
        }

        CursorImageStatus::Hidden => return false,
    };
    // One logical pixel covers physical rounding at fractional scales.
    let bounds = Rectangle::<f64, Logical>::new(
        (f64::from(bounds.loc.x) - 1.0, f64::from(bounds.loc.y) - 1.0).into(),
        (f64::from(bounds.size.w) + 2.0, f64::from(bounds.size.h) + 2.0).into(),
    );
    rect.overlaps(bounds)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mixed_refresh_only_orphaned_animations_need_a_watchdog() {
        let now = Instant::now();
        let fast = Duration::from_nanos(1_000_000_000 / 240);
        let slow = Duration::from_nanos(1_000_000_000 / 60);
        let mut idle_fast = AnimationFallback::default();
        let mut moving_slow = AnimationFallback::default();
        assert_eq!(idle_fast.deadline(now, fast, false, false), None);
        assert_eq!(moving_slow.deadline(now, slow, true, false), Some(now + slow * 2));
        assert_eq!(
            moving_slow.deadline(now + slow, slow, true, false),
            Some(now + slow * 2)
        );
        assert_eq!(moving_slow.deadline(now + slow, slow, true, true), None);
    }

    #[test]
    fn disconnect_and_reconnect_do_not_keep_an_old_deadline() {
        let now = Instant::now();
        let interval = Duration::from_millis(16);
        let mut output = AnimationFallback::default();
        output.deadline(now, interval, true, false);
        assert_eq!(output.deadline(now + interval, interval, false, false), None);
        let reconnected = now + Duration::from_secs(1);
        assert_eq!(
            output.deadline(reconnected, interval, true, false),
            Some(reconnected + interval * 2)
        );
    }

    #[test]
    fn measured_legacy_fast_monitor_watchdog_wakes_with_an_intact_slow_chain() {
        // The old watchdog wakes at last_animation_tick + 2*fast_interval,
        // then advances the shared clock, even with a slow render in flight.
        let mut last_tick_us = 0;
        let mut old_wakes = 0;
        let mut policy = AnimationFallback::default();
        let start = Instant::now();
        for now_us in 0..1_000_000 {
            if now_us % 16_667 == 0 {
                last_tick_us = now_us;
            }

            if now_us >= last_tick_us + 8_334 {
                old_wakes += 1;
                last_tick_us = now_us;
            }

            assert_eq!(policy.deadline(start, Duration::from_micros(16_667), true, true), None);
        }

        assert_eq!(old_wakes, 60);
        eprintln!("watchdog replay: legacy={old_wakes} unnecessary wakes/s, intact-chain policy=0");
    }
    #[test]
    fn scheduled_and_in_flight_frames_suppress_fallback_until_the_chain_is_lost() {
        use crate::frame_scheduler::FrameScheduler;
        let start = Instant::now();
        let interval = Duration::from_millis(16);
        let mut scheduler = FrameScheduler::new(interval);
        let mut fallback = AnimationFallback::default();
        scheduler.request(Duration::ZERO);
        let scheduled = scheduler.plan(Duration::ZERO).unwrap();
        assert_eq!(fallback.deadline(start, interval, true, true), None);
        let plan = scheduler.begin().unwrap();
        assert_eq!(plan, scheduled);
        scheduler.rendered(plan, Duration::from_millis(2), Duration::from_millis(2), true);
        assert_eq!(scheduler.plan(interval), None);
        assert_eq!(fallback.deadline(start + interval, interval, true, true), None);
        scheduler.presented(interval);
        assert_eq!(
            fallback.deadline(start + interval, interval, true, false),
            Some(start + interval * 3)
        );
        scheduler.request(interval);
        assert!(scheduler.plan(interval).is_some());
        assert_eq!(fallback.deadline(start + interval, interval, true, true), None);
    }

    #[test]
    fn settled_animation_still_recovers_its_requested_final_frame_after_scheduling_failure() {
        use crate::frame_scheduler::FrameScheduler;
        let start = Instant::now();
        let interval = Duration::from_millis(16);
        let mut scheduler = FrameScheduler::new(interval);
        let mut fallback = AnimationFallback::default();
        scheduler.request(Duration::ZERO);
        scheduler.plan(Duration::ZERO).unwrap();
        // Timer insertion failed: a scheduler plan without an event-loop
        // source is not a live chain. Another output can settle the spring.
        assert!(scheduler.has_pending_request());
        let deadline = fallback.deadline(start, interval, true, false).unwrap();
        let animation_active = false;
        let needs_final_frame = animation_active || (fallback.waiting_for_frame() && scheduler.has_pending_request());
        assert_eq!(
            fallback.deadline(deadline, interval, needs_final_frame, false),
            Some(deadline)
        );
        // A successful retry installs the normal timer and cancels fallback.
        assert_eq!(fallback.deadline(deadline, interval, needs_final_frame, true), None);
        scheduler.begin().unwrap();
        assert!(!scheduler.has_pending_request());
    }

    #[test]
    #[ignore = "requires private sockets; subprocess supplies its own runtime directory"]
    fn cursor_footprints_limit_output_local_redraws_and_clear_both_sides_of_a_crossing() {
        if std::env::var_os("FERESE_CURSOR_REDRAW_TEST_CHILD").is_none() {
            let runtime = tempfile::tempdir().unwrap();
            let result = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "backends::direct::redraw::tests::cursor_footprints_limit_output_local_redraws_and_clear_both_sides_of_a_crossing", "--include-ignored", "--nocapture"])
                .env("XDG_RUNTIME_DIR", runtime.path())
                .env("FERESE_CURSOR_REDRAW_TEST_CHILD", "1")
                .output().unwrap();
            assert!(
                result.status.success(),
                "{}{}",
                String::from_utf8_lossy(&result.stdout),
                String::from_utf8_lossy(&result.stderr)
            );
            return;
        }

        use smithay::input::pointer::{CursorImageStatus, MotionEvent};
        use smithay::output::{Mode, Output, PhysicalProperties, Subpixel};
        use smithay::utils::SERIAL_COUNTER;
        let mut event_loop = smithay::reexports::calloop::EventLoop::try_new().unwrap();
        let display = smithay::reexports::wayland_server::Display::new().unwrap();
        let config = crate::config::Config::default().runtime_config().unwrap();
        let mut state = crate::Ferese::new(&mut event_loop, display, config).unwrap();
        let mut outputs = Vec::new();
        for (index, refresh) in [60_000, 240_000].into_iter().enumerate() {
            let output = Output::new(
                format!("cursor-test-{index}"),
                PhysicalProperties {
                    size: (0, 0).into(),
                    subpixel: Subpixel::Unknown,
                    make: "test".into(),
                    model: "test".into(),
                },
            );
            output.change_current_state(
                Some(Mode {
                    size: if index == 0 {
                        (3840, 2160).into()
                    } else {
                        (1920, 1080).into()
                    },
                    refresh,
                }),
                None,
                Some(smithay::output::Scale::Fractional(if index == 0 { 2.0 } else { 1.0 })),
                Some(((index as i32) * 1920, 0).into()),
            );
            state.space.map_output(&output, ((index as i32) * 1920, 0));
            state.register_output(&output, output.name());
            outputs.push(output);
        }

        let pointer = state.seat.get_pointer().unwrap();
        let mut old = [false; 2];
        let mut local_requests = 0;
        for x in [500.0, 501.0, 502.0] {
            pointer.motion(
                &mut state,
                None,
                &MotionEvent {
                    location: (x, 500.0).into(),
                    serial: SERIAL_COUNTER.next_serial(),
                    time: 0,
                },
            );
            let current = [
                cursor_on_output(&state, &outputs[0]),
                cursor_on_output(&state, &outputs[1]),
            ];
            assert_eq!(current, [true, false]);
            local_requests += old
                .into_iter()
                .zip(current)
                .filter(|(before, after)| *before || *after)
                .count();
            old = current;
        }

        assert_eq!(local_requests, 3);
        eprintln!("cursor replay: legacy=6 output requests, footprint policy={local_requests}");
        pointer.motion(
            &mut state,
            None,
            &MotionEvent {
                location: (1919.0, 500.0).into(),
                serial: SERIAL_COUNTER.next_serial(),
                time: 0,
            },
        );
        assert!(cursor_on_output(&state, &outputs[0]));
        assert!(cursor_on_output(&state, &outputs[1]), "cursor image straddles the seam");
        if let Some(cursor) = state.named_cursor_frame() {
            let x = 1920.0 - f64::from(cursor.size.w) / f64::from(cursor.buffer_scale) * 0.75
                + f64::from(cursor.hotspot.x) / f64::from(cursor.buffer_scale);
            pointer.motion(
                &mut state,
                None,
                &MotionEvent {
                    location: (x, 500.0).into(),
                    serial: SERIAL_COUNTER.next_serial(),
                    time: 0,
                },
            );
            assert!(
                cursor_on_output(&state, &outputs[1]),
                "fractional-scale footprint matches rendered cursor size"
            );
        }

        old = [true, true];
        state.cursor_status = CursorImageStatus::Hidden;
        let current = [
            cursor_on_output(&state, &outputs[0]),
            cursor_on_output(&state, &outputs[1]),
        ];
        assert_eq!(current, [false, false]);
        assert_eq!(
            old.into_iter()
                .zip(current)
                .filter(|(before, after)| *before || *after)
                .count(),
            2
        );
        // Grabs and held gestures defer window/output work independently of
        // the cursor, which may be on another display or hidden entirely.
        state.defer_output_redraw(outputs[0].clone());
        state.defer_output_redraw(outputs[1].clone());
        state.defer_output_redraw(outputs[1].clone());
        assert_eq!(state.output_redraw_pending, outputs);
        state.unregister_output(&outputs[1]);
        assert_eq!(state.output_redraw_pending, vec![outputs[0].clone()]);
        assert!(!cursor_on_output(&state, &outputs[1]));
    }
}
