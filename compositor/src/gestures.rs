#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[repr(u32)]
pub(crate) enum SwipeDirection {
    Up,
    Down,
    Left,
    Right,
}

#[derive(Default)]
pub(crate) struct Swipe {
    delta: Option<(f64, f64)>,
    tracker_x: ferese_animation::gesture::VelocityTracker,
    tracker_y: ferese_animation::gesture::VelocityTracker,
    pub release_velocity: f64,
    pub unbounded_release_velocity: f64,

    threshold: f64,
    fingers: u32,
    direction: Option<SwipeDirection>,
    preview_started: bool,
    momentum_navigation: bool,
}

impl Swipe {
    pub fn begin(&mut self, fingers: u32, blocked: bool, threshold: u16) {
        self.delta = ((3..=5).contains(&fingers) && !blocked).then_some((0.0, 0.0));
        self.threshold = f64::from(threshold);
        self.fingers = fingers;
        self.direction = None;
        self.preview_started = false;
        self.momentum_navigation = false;
        self.tracker_x = Default::default();
        self.tracker_y = Default::default();
        self.release_velocity = 0.0;
        self.unbounded_release_velocity = 0.0;
    }

    pub fn start_time(&mut self, time: std::time::Duration) {
        self.tracker_x.push(time, 0.0);
        self.tracker_y.push(time, 0.0);
    }

    pub fn update_at(&mut self, dx: f64, dy: f64, time: std::time::Duration) {
        self.update(dx, dy);
        if let Some((x, y)) = self.delta {
            self.tracker_x.push(time, x);
            self.tracker_y.push(time, y);
        }
    }

    pub fn finish_at(
        &mut self,
        cancelled: bool,
        time: std::time::Duration,
    ) -> Option<SwipeDirection> {
        let Some(direction) = self.direction else {
            return self.finish(cancelled);
        };
        let v = match direction {
            SwipeDirection::Left => -self.tracker_x.velocity(time),
            SwipeDirection::Right => self.tracker_x.velocity(time),
            SwipeDirection::Up => -self.tracker_y.velocity(time),
            SwipeDirection::Down => self.tracker_y.velocity(time),
        };
        self.release_velocity = if cancelled {
            0.0
        } else {
            v / (2.0 * self.threshold.max(1.0))
        };
        self.unbounded_release_velocity = self.release_velocity;
        if !self.preview_started && !self.momentum_navigation {
            return self.finish(cancelled);
        }
        let Some((_, progress)) = self.preview() else {
            self.release_velocity = 0.0;
            self.unbounded_release_velocity = 0.0;
            self.delta = None;
            return None;
        };
        // The preview is bounded. Its visible velocity is zero when pushing
        // farther beyond either endpoint, even if the fingers keep moving.
        if (progress == 0.0 && self.release_velocity < 0.0)
            || (progress == 1.0 && self.release_velocity > 0.0)
        {
            self.release_velocity = 0.0;
        }
        self.delta = None;
        (!cancelled
            && ferese_animation::gesture::project(progress, self.release_velocity, 0.997) >= 0.5)
            .then_some(direction)
    }

    pub fn fingers(&self) -> u32 {
        self.fingers
    }

    pub fn active(&self) -> bool {
        self.delta.is_some()
    }

    pub fn update(&mut self, dx: f64, dy: f64) {
        if let Some((x, y)) = &mut self.delta {
            *x += dx;
            *y += dy;
            if self.direction.is_none() {
                self.direction = dominant_direction(*x, *y, 6.0);
            }
        }
    }

    pub fn mark_preview_started(&mut self) {
        self.preview_started = true;
    }

    pub fn mark_momentum_navigation(&mut self) {
        self.momentum_navigation = true;
    }

    pub fn preview_started(&self) -> bool {
        self.preview_started
    }

    pub fn preview(&self) -> Option<(SwipeDirection, f64)> {
        self.unbounded_preview()
            .map(|(direction, progress)| (direction, progress.clamp(0.0, 1.0)))
    }

    pub fn unbounded_preview(&self) -> Option<(SwipeDirection, f64)> {
        let (x, y) = self.delta?;
        if !x.is_finite() || !y.is_finite() {
            return None;
        }
        let direction = self.direction?;
        let distance = match direction {
            SwipeDirection::Left => -x,
            SwipeDirection::Right => x,
            SwipeDirection::Up => -y,
            SwipeDirection::Down => y,
        };
        Some((direction, distance / (self.threshold.max(1.0) * 2.0)))
    }

    pub fn finish(&mut self, cancelled: bool) -> Option<SwipeDirection> {
        let (x, y) = self.delta.take()?;
        if cancelled {
            return None;
        }
        dominant_direction(x, y, self.threshold)
            .filter(|direction| Some(*direction) == self.direction)
    }
}

fn dominant_direction(x: f64, y: f64, threshold: f64) -> Option<SwipeDirection> {
    if !x.is_finite() || !y.is_finite() {
        return None;
    }
    const DOMINANCE: f64 = 1.25;
    if x.abs() >= threshold && x.abs() > y.abs() * DOMINANCE {
        Some(if x < 0.0 {
            SwipeDirection::Left
        } else {
            SwipeDirection::Right
        })
    } else if y.abs() >= threshold && y.abs() > x.abs() * DOMINANCE {
        Some(if y < 0.0 {
            SwipeDirection::Up
        } else {
            SwipeDirection::Down
        })
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn release_uses_momentum_and_a_pause_removes_it() {
        use std::time::Duration;
        let swipe = || {
            let mut swipe = Swipe::default();
            swipe.begin(3, false, 80);
            swipe.start_time(Duration::ZERO);
            swipe.update_at(0.0, -20.0, Duration::from_millis(10));
            swipe.update_at(0.0, -20.0, Duration::from_millis(20));
            swipe.mark_preview_started();
            swipe
        };
        let mut flick = swipe();
        assert_eq!(flick.preview(), Some((SwipeDirection::Up, 0.25)));
        assert_eq!(
            flick.finish_at(false, Duration::from_millis(20)),
            Some(SwipeDirection::Up)
        );
        assert!((flick.release_velocity - 12.5).abs() < 1e-9);
        let mut paused = swipe();
        assert_eq!(paused.finish_at(false, Duration::from_millis(80)), None);
        assert_eq!(paused.release_velocity, 0.0);
        let mut cancelled = swipe();
        assert_eq!(cancelled.finish_at(true, Duration::from_millis(20)), None);
        assert_eq!(cancelled.release_velocity, 0.0);
    }

    #[test]
    fn reversing_momentum_can_cancel_past_halfway() {
        use std::time::Duration;
        let mut swipe = Swipe::default();
        swipe.begin(3, false, 80);
        swipe.start_time(Duration::ZERO);
        swipe.update_at(0.0, -120.0, Duration::from_millis(200));
        swipe.update_at(0.0, 20.0, Duration::from_millis(210));
        swipe.mark_preview_started();
        assert_eq!(swipe.preview(), Some((SwipeDirection::Up, 0.625)));
        assert_eq!(swipe.finish_at(false, Duration::from_millis(210)), None);
        assert!(swipe.release_velocity < 0.0);
    }

    #[test]
    fn cancellation_and_new_gestures_reset_preview_state() {
        let mut swipe = Swipe::default();
        swipe.begin(3, false, 80);
        swipe.update(0.0, -200.0);
        swipe.mark_preview_started();
        assert_eq!(swipe.preview(), Some((SwipeDirection::Up, 1.0)));
        assert_eq!(swipe.finish(true), None);
        assert!(
            swipe.preview_started(),
            "release must suppress a second action after a preview"
        );
        assert_eq!(swipe.preview(), None);
        swipe.begin(4, false, 80);
        assert!(!swipe.preview_started());
        swipe.update(-100.0, 0.0);
        assert_eq!(swipe.finish(false), Some(SwipeDirection::Left));
        swipe.begin(3, true, 80);
        swipe.update(0.0, -200.0);
        assert!(!swipe.active());
        assert_eq!(swipe.finish(false), None);
    }

    #[test]
    fn preview_tracks_motion_and_reversal_before_release() {
        let mut swipe = Swipe::default();
        swipe.begin(3, false, 80);
        swipe.update(0.0, -40.0);
        assert_eq!(swipe.preview(), Some((SwipeDirection::Up, 0.25)));
        swipe.update(0.0, -40.0);
        assert_eq!(swipe.preview(), Some((SwipeDirection::Up, 0.5)));
        swipe.update(0.0, 60.0);
        assert_eq!(swipe.preview(), Some((SwipeDirection::Up, 0.125)));
        assert_eq!(swipe.finish(false), None);
        assert_eq!(swipe.preview(), None);
    }

    #[test]
    fn reversal_across_the_origin_cancels_instead_of_committing_another_axis() {
        let mut swipe = Swipe::default();
        swipe.begin(3, false, 80);
        swipe.update(0.0, -40.0);
        swipe.update(0.0, 160.0);
        assert_eq!(swipe.preview(), Some((SwipeDirection::Up, 0.0)));
        assert_eq!(swipe.finish(false), None);
    }

    #[test]
    fn a_swipe_accumulates_motion_and_commits_only_once_on_release() {
        let mut swipe = Swipe::default();
        swipe.begin(3, false, 80);
        for _ in 0..10 {
            swipe.update(-10.0, 1.0);
        }
        assert!(swipe.active());
        assert_eq!(swipe.finish(false), Some(SwipeDirection::Left));
        assert!(!swipe.active());
        assert_eq!(swipe.finish(false), None);
    }

    #[test]
    fn vertical_and_horizontal_swipes_are_distinct() {
        for (delta, expected) in [
            ((0.0, -100.0), SwipeDirection::Up),
            ((0.0, 100.0), SwipeDirection::Down),
            ((-100.0, 0.0), SwipeDirection::Left),
            ((100.0, 0.0), SwipeDirection::Right),
        ] {
            let mut swipe = Swipe::default();
            swipe.begin(3, false, 80);
            swipe.update(delta.0, delta.1);
            assert_eq!(swipe.finish(false), Some(expected));
        }
    }

    #[test]
    fn cancellation_jitter_diagonals_and_nonfinite_motion_do_not_navigate() {
        for (x, y, cancelled) in [
            (200.0, 0.0, true),
            (79.0, 0.0, false),
            (100.0, 100.0, false),
            (f64::NAN, 100.0, false),
        ] {
            let mut swipe = Swipe::default();
            swipe.begin(3, false, 80);
            swipe.update(x, y);
            assert_eq!(swipe.finish(cancelled), None);
            assert!(!swipe.active());
        }
    }

    #[test]
    fn other_finger_counts_and_blocked_gestures_are_not_intercepted() {
        for (fingers, blocked) in [(2, false), (6, false), (3, true)] {
            let mut swipe = Swipe::default();
            swipe.begin(fingers, blocked, 80);
            assert!(!swipe.active());
            swipe.update(200.0, 0.0);
            assert_eq!(swipe.finish(false), None);
        }
    }

    #[test]
    fn configured_distance_changes_when_a_swipe_commits() {
        let mut swipe = Swipe::default();
        swipe.begin(3, false, 120);
        swipe.update(100.0, 0.0);
        assert_eq!(swipe.finish(false), None);
        swipe.begin(3, false, 40);
        swipe.update(100.0, 0.0);
        assert_eq!(swipe.finish(false), Some(SwipeDirection::Right));
    }
}
