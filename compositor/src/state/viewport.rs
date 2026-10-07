use super::*;

#[derive(Clone, PartialEq)]
enum ViewportOwner {
    Spring,
    Gesture(Box<FocusSwipe>),
}

#[derive(Clone, PartialEq)]
enum ViewportMode {
    Running(ViewportOwner),
    // The paused owner keeps its position and velocity. Releasing consumes one
    // tick before that same owner resumes, excluding time spent waiting.
    Held(ViewportOwner),
    // The release frame excludes held wall time and presents zero velocity.
    Resuming(ViewportOwner),
}

#[derive(Clone, PartialEq)]
pub(super) struct ViewportPresentation {
    motion: AnimatedValue,
    mode: ViewportMode,
}

impl ViewportPresentation {
    pub(super) fn new(target: f64) -> Self {
        Self {
            motion: AnimatedValue::new(target),
            mode: ViewportMode::Running(ViewportOwner::Spring),
        }
    }

    #[cfg(test)]
    pub(super) fn from_motion(motion: AnimatedValue) -> Self {
        Self {
            motion,
            ..Self::new(motion.target)
        }
    }

    pub(super) fn motion(&self) -> &AnimatedValue {
        &self.motion
    }

    pub(super) fn retarget(&mut self, target: f64, animations: bool) -> bool {
        let changed = (self.motion.target - target).abs() > 0.001;
        self.motion.retarget_preserving_motion(target);
        if !animations {
            self.motion.snap();
        }
        changed
    }

    pub(super) fn gesture(&self) -> Option<&FocusSwipe> {
        match &self.mode {
            ViewportMode::Running(ViewportOwner::Gesture(swipe))
            | ViewportMode::Held(ViewportOwner::Gesture(swipe))
            | ViewportMode::Resuming(ViewportOwner::Gesture(swipe)) => Some(swipe),
            _ => None,
        }
    }

    pub(super) fn update_gesture(&mut self, progress: f64) {
        match &mut self.mode {
            ViewportMode::Running(ViewportOwner::Gesture(swipe))
            | ViewportMode::Held(ViewportOwner::Gesture(swipe))
            | ViewportMode::Resuming(ViewportOwner::Gesture(swipe)) => swipe.progress = progress,
            _ => {}
        }
    }

    pub(super) fn begin_gesture(&mut self, swipe: FocusSwipe) {
        debug_assert!(self.gesture().is_none());
        let owner = ViewportOwner::Gesture(Box::new(swipe));
        self.mode = if self.is_held() {
            ViewportMode::Held(owner)
        } else {
            ViewportMode::Running(owner)
        };
    }

    pub(super) fn end_gesture(&mut self, release_velocity: Option<f64>) -> Option<FocusSwipe> {
        self.gesture()?;
        let mode = std::mem::replace(&mut self.mode, ViewportMode::Running(ViewportOwner::Spring));
        let (swipe, held) = match mode {
            ViewportMode::Running(ViewportOwner::Gesture(swipe)) => (swipe, false),
            ViewportMode::Held(ViewportOwner::Gesture(swipe)) => (swipe, true),
            ViewportMode::Resuming(ViewportOwner::Gesture(swipe)) => (swipe, false),
            _ => unreachable!(),
        };
        if let Some(velocity) = release_velocity {
            self.motion.current = swipe.position();
            self.motion.velocity = velocity;
        }
        if held {
            self.mode = ViewportMode::Held(ViewportOwner::Spring);
        }
        Some(*swipe)
    }

    pub(super) fn is_held(&self) -> bool {
        matches!(self.mode, ViewportMode::Held(_))
    }

    // Commits/deadlines retain a release tick. An independent retarget can
    // discard the old dependency hold immediately instead.
    pub(super) fn reconcile_hold(&mut self, blocked: bool, release_tick: bool) {
        let mode = std::mem::replace(&mut self.mode, ViewportMode::Running(ViewportOwner::Spring));
        self.mode = match mode {
            ViewportMode::Running(owner) | ViewportMode::Resuming(owner) if blocked => ViewportMode::Held(owner),
            ViewportMode::Held(owner) if !blocked && !release_tick => ViewportMode::Running(owner),
            mode => mode,
        };
    }

    pub(super) fn presented_motion(&self) -> AnimatedValue {
        let mut motion = self.motion;
        if matches!(self.mode, ViewportMode::Held(_) | ViewportMode::Resuming(_)) {
            motion.velocity = 0.0;
        }
        motion
    }

    pub(super) fn needs_tick(&self) -> bool {
        self.is_held()
            || self.gesture().is_some()
            || self.motion.current != self.motion.target
            || self.motion.velocity != 0.0
    }

    #[cfg(feature = "resize-metrics")]
    pub(super) fn spring_is_moving(&self) -> bool {
        self.gesture().is_none() && (self.motion.current != self.motion.target || self.motion.velocity != 0.0)
    }

    // Prediction and authoritative ticks evaluate the same motion step. The
    // sampler borrows gesture/dependency metadata instead of cloning layouts.
    fn step_motion(
        &self,
        delta: Duration,
        spring: SpringConfig,
        animations: bool,
        blocked: bool,
    ) -> (AnimatedValue, bool) {
        let mut motion = self.motion;
        if blocked || self.is_held() {
            return (motion, blocked || self.needs_tick());
        }
        let active = match &self.mode {
            ViewportMode::Running(ViewportOwner::Gesture(swipe))
            | ViewportMode::Resuming(ViewportOwner::Gesture(swipe)) => {
                motion.current = swipe.position();
                motion.velocity = 0.0;
                true
            }
            ViewportMode::Running(ViewportOwner::Spring) | ViewportMode::Resuming(ViewportOwner::Spring)
                if animations =>
            {
                motion.advance(delta, spring)
            }
            ViewportMode::Running(ViewportOwner::Spring) | ViewportMode::Resuming(ViewportOwner::Spring) => {
                motion.snap();
                false
            }
            ViewportMode::Held(_) => unreachable!(),
        };
        (motion, active)
    }

    pub(super) fn advance(&mut self, delta: Duration, spring: SpringConfig, animations: bool, blocked: bool) -> bool {
        let (motion, active) = self.step_motion(delta, spring, animations, blocked);
        self.motion = motion;
        let mode = std::mem::replace(&mut self.mode, ViewportMode::Running(ViewportOwner::Spring));
        self.mode = match mode {
            ViewportMode::Running(owner) | ViewportMode::Held(owner) | ViewportMode::Resuming(owner) if blocked => {
                ViewportMode::Held(owner)
            }
            ViewportMode::Held(owner) => ViewportMode::Resuming(owner),
            ViewportMode::Running(owner) | ViewportMode::Resuming(owner) => ViewportMode::Running(owner),
        };
        active
    }

    pub(super) fn sample(
        &self,
        delta: Duration,
        spring: SpringConfig,
        animations: bool,
        blocked: bool,
    ) -> AnimatedValue {
        let (mut motion, _) = self.step_motion(delta, spring, animations, blocked);
        if blocked || self.is_held() {
            motion.velocity = 0.0;
        }
        motion
    }
}

impl Ferese {
    pub(super) fn focus_swipe(&self) -> Option<&FocusSwipe> {
        self.viewports.values().find_map(ViewportPresentation::gesture)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TICK: Duration = Duration::from_millis(16);

    fn moving() -> ViewportPresentation {
        let mut viewport = ViewportPresentation::new(0.0);
        viewport.retarget(500.0, true);
        viewport.advance(TICK, SpringConfig::default(), true, false);
        viewport
    }

    fn swipe(viewport: &ViewportPresentation) -> FocusSwipe {
        FocusSwipe {
            workspace: WorkspaceId(1),
            from: WindowId(1),
            to: WindowId(2),
            direction: Direction::Right,
            gesture_direction: SwipeDirection::Left,
            start: viewport.motion.current,
            destination: 700.0,
            progress: 0.0,
            layout: None,
            dependencies: None,
        }
    }

    #[test]
    fn focus_retarget_preserves_moving_spring_position_and_velocity() {
        let mut viewport = moving();
        let before = viewport.motion;
        viewport.retarget(-200.0, true);
        assert_eq!(viewport.motion.current, before.current);
        assert_eq!(viewport.motion.velocity, before.velocity);
        assert_eq!(viewport.motion.target, -200.0);
        let mut expected = before;
        expected.retarget_preserving_motion(-200.0);
        expected.advance(TICK, SpringConfig::default());
        viewport.advance(TICK, SpringConfig::default(), true, false);
        assert_eq!(viewport.motion, expected);
    }

    #[test]
    fn gesture_interrupts_spring_and_releases_from_latest_input() {
        let mut viewport = moving();
        let before = viewport.motion.current;
        viewport.begin_gesture(swipe(&viewport));
        viewport.advance(Duration::from_secs(5), SpringConfig::default(), true, false);
        assert_eq!(viewport.motion.current, before);
        assert_eq!(viewport.motion.velocity, 0.0);
        viewport.update_gesture(0.5);
        // No frame is necessary between the last input and release.
        let position = viewport.gesture().unwrap().position();
        let velocity = viewport.gesture().unwrap().release_velocity(0.4, 99.0);
        viewport.end_gesture(Some(velocity)).unwrap();
        viewport.retarget(700.0, true);
        assert_eq!(viewport.motion.current, position);
        assert_eq!(viewport.motion.velocity, velocity);
        assert!(matches!(viewport.mode, ViewportMode::Running(ViewportOwner::Spring)));
        let mut expected = viewport.motion;
        expected.advance(TICK, SpringConfig::default());
        viewport.advance(TICK, SpringConfig::default(), true, false);
        assert_eq!(viewport.motion, expected);
    }

    #[test]
    fn edge_gesture_keeps_rubber_band_and_release_derivative() {
        let mut viewport = moving();
        let mut gesture = swipe(&viewport);
        gesture.to = gesture.from;
        gesture.destination = gesture.start;
        gesture.progress = 2.0;
        let (position, derivative) = ferese_animation::gesture::rubber_band(2.0, 1.0, 0.55);
        let expected = gesture.start + 64.0 * position;
        assert_eq!(gesture.release_velocity(99.0, 0.4), 64.0 * 0.4 * derivative);
        viewport.begin_gesture(gesture);
        viewport.advance(TICK, SpringConfig::default(), true, false);
        assert_eq!(viewport.motion.current, expected);
    }

    #[test]
    fn hold_release_retains_motion_and_excludes_waiting_wall_time() {
        let mut viewport = moving();
        let frozen = viewport.motion;
        viewport.reconcile_hold(true, false);
        viewport.advance(Duration::from_secs(3), SpringConfig::default(), true, true);
        assert_eq!(viewport.motion, frozen);
        assert_eq!(viewport.presented_motion().velocity, 0.0);
        viewport.reconcile_hold(false, true);
        viewport.advance(Duration::from_secs(2), SpringConfig::default(), true, false);
        assert_eq!(viewport.motion, frozen);
        assert_eq!(viewport.presented_motion().velocity, 0.0);
        let mut expected = frozen;
        expected.advance(TICK, SpringConfig::default());
        viewport.advance(TICK, SpringConfig::default(), true, false);
        assert_eq!(viewport.motion, expected);
        assert_eq!(viewport.presented_motion(), expected);
    }

    #[test]
    fn held_retarget_preserves_motion_until_its_dependencies_clear() {
        let mut viewport = moving();
        viewport.reconcile_hold(true, false);
        let frozen = viewport.motion;
        viewport.retarget(900.0, true);
        viewport.reconcile_hold(true, false);
        viewport.advance(TICK, SpringConfig::default(), true, true);
        assert_eq!(viewport.motion.current, frozen.current);
        assert_eq!(viewport.motion.velocity, frozen.velocity);
        assert_eq!(viewport.motion.target, 900.0);
        // An independent policy request retires the hold immediately, even if
        // other widths on this workspace are still awaiting client commits.
        viewport.retarget(-100.0, true);
        viewport.reconcile_hold(false, false);
        assert!(!viewport.is_held());
        viewport.advance(TICK, SpringConfig::default(), true, false);
        assert_ne!(viewport.motion.current, frozen.current);
    }

    #[test]
    fn hold_pauses_gesture_owner_and_release_resumes_that_gesture() {
        let mut viewport = moving();
        let frozen = viewport.motion;
        viewport.reconcile_hold(true, false);
        viewport.begin_gesture(swipe(&viewport));
        viewport.update_gesture(0.75);
        let position = viewport.gesture().unwrap().position();
        viewport.advance(TICK, SpringConfig::default(), true, true);
        assert_eq!(viewport.motion, frozen);
        viewport.advance(Duration::from_secs(3), SpringConfig::default(), true, false);
        assert_eq!(viewport.motion, frozen);
        viewport.advance(TICK, SpringConfig::default(), true, false);
        assert_eq!(viewport.motion.current, position);
        assert_eq!(viewport.motion.velocity, 0.0);
        assert!(viewport.gesture().is_some());
    }

    #[test]
    fn blocked_gesture_release_does_not_publish_uncommitted_position() {
        let mut viewport = moving();
        let frozen = viewport.motion;
        viewport.begin_gesture(swipe(&viewport));
        viewport.update_gesture(0.8);
        viewport.reconcile_hold(true, false);
        viewport.end_gesture(None).unwrap();
        viewport.retarget(700.0, true);
        assert!(matches!(viewport.mode, ViewportMode::Held(ViewportOwner::Spring)));
        assert_eq!(viewport.motion.current, frozen.current);
        assert_eq!(viewport.motion.velocity, frozen.velocity);
    }

    #[test]
    fn prediction_steps_a_copy_of_every_owner_and_hold_phase() {
        let running = moving();
        let mut gesture = running.clone();
        gesture.begin_gesture(swipe(&gesture));
        gesture.update_gesture(0.3);
        let mut held_spring = running.clone();
        held_spring.reconcile_hold(true, false);
        let mut held_gesture = gesture.clone();
        held_gesture.reconcile_hold(true, false);
        let mut resuming = held_spring.clone();
        resuming.advance(TICK, SpringConfig::default(), true, false);
        for state in [running, gesture, held_spring, held_gesture, resuming] {
            for blocked in [false, true] {
                for horizon in [Duration::ZERO, TICK, Duration::from_millis(40)] {
                    let original = state.clone();
                    let sample = state.sample(horizon, SpringConfig::default(), true, blocked);
                    let mut advanced = state.clone();
                    advanced.advance(horizon, SpringConfig::default(), true, blocked);
                    assert_eq!(sample, advanced.presented_motion());
                    assert!(state == original, "prediction changed authoritative mode or motion");
                }
            }
        }
    }

    #[test]
    fn layout_handoff_preserves_screen_position_for_each_viewport_owner() {
        let spring = moving();
        let mut gesture = spring.clone();
        gesture.begin_gesture(swipe(&gesture));
        gesture.update_gesture(0.6);
        gesture.advance(TICK, SpringConfig::default(), true, false);
        let mut held = spring.clone();
        held.reconcile_hold(true, false);
        for viewport in [spring, gesture, held] {
            let mut geometry = WindowGeometry::new(Rect::new(123.0, 0.0, 400.0, 300.0), None);
            geometry.visual.velocity.x = 42.0;
            let before = geometry.visual;
            let motion = viewport.presented_motion();
            let mut world = AnimatedValue::new(restored_scrolling_world_x(
                true,
                before.current.x,
                motion.current,
                900.0,
            ));
            world.velocity = before.velocity.x + motion.velocity;
            world.set_target(geometry.logical.x + motion.target);
            super::super::animation::sync_scrolling_coordinates(&mut geometry, &mut world, &motion, false);
            assert!((geometry.visual.current.x - before.current.x).abs() < 1e-6);
            assert!((geometry.visual.velocity.x - before.velocity.x).abs() < 1e-6);
            assert_eq!(geometry.logical.x, world.target - motion.target);
        }
    }

    #[test]
    fn reduced_motion_snaps_policy_target_after_hold_release() {
        let mut viewport = moving();
        viewport.reconcile_hold(true, false);
        let frozen = viewport.motion;
        viewport.advance(TICK, SpringConfig::default(), false, true);
        assert_eq!(viewport.motion, frozen);
        viewport.advance(TICK, SpringConfig::default(), false, false);
        assert_eq!(viewport.motion, frozen);
        viewport.advance(TICK, SpringConfig::default(), false, false);
        assert_eq!(viewport.motion.current, viewport.motion.target);
        assert_eq!(viewport.motion.velocity, 0.0);
        assert!(!viewport.needs_tick());
        viewport.retarget(300.0, false);
        assert_eq!(viewport.motion.current, 300.0);
    }
}
