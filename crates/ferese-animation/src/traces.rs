use super::*;

#[derive(Debug, Clone, Copy)]
struct Sample {
    time: Duration,
    current: f64,
    velocity: f64,
    target: f64,
}

struct FakeClock {
    now: Duration,
    last: Duration,
    paused: bool,
    trace: Vec<Sample>,
}

impl FakeClock {
    fn new() -> Self {
        Self {
            now: Duration::ZERO,
            last: Duration::ZERO,
            paused: false,
            trace: Vec::new(),
        }
    }

    fn sample(
        &mut self,
        time: Duration,
        value: &mut AnimatedValue,
        config: SpringConfig,
    ) -> Sample {
        assert!(time >= self.now);
        self.now = time;
        let delta = time - self.last;
        self.last = time;
        if !self.paused {
            value.advance(delta, config);
        }
        let sample = Sample {
            time,
            current: value.current,
            velocity: value.velocity,
            target: value.target,
        };
        self.trace.push(sample);
        sample
    }

    fn pause(&mut self) {
        self.paused = true;
    }

    fn resume(&mut self, time: Duration) {
        self.now = time;
        self.last = time;
        self.paused = false;
    }
}

// Frozen scalar integrator, including its cap, substeps and crossing snap.
fn legacy_advance(value: &mut AnimatedValue, delta: Duration, config: SpringConfig) {
    let config = config.normalized();
    let settled = |v: &AnimatedValue| {
        (v.current - v.target).abs() <= config.position_tolerance
            && v.velocity.abs() <= config.velocity_tolerance
    };
    if settled(value) {
        value.snap();
        return;
    }

    let seconds = delta.min(Duration::from_millis(100)).as_secs_f64();
    let steps = (seconds / (1.0 / 240.0)).ceil().max(1.0) as usize;
    let dt = seconds / steps as f64;
    for _ in 0..steps {
        let old = value.current - value.target;
        value.velocity +=
            (-config.stiffness * old - config.damping * value.velocity) / config.mass * dt;
        value.current += value.velocity * dt;
        if old != 0.0 && old.signum() != (value.current - value.target).signum() {
            value.snap();
            break;
        }
    }
    if settled(value) {
        value.snap();
    }
}

fn close(a: f64, b: f64, epsilon: f64) {
    assert!((a - b).abs() <= epsilon, "{a} != {b}, tolerance {epsilon}");
}

#[test]
fn capture_old_golden_trace_and_retarget_policy() {
    let mut value = AnimatedValue::new(0.0);
    value.set_target(500.0);
    for i in 1..=18 {
        legacy_advance(
            &mut value,
            Duration::from_secs_f64(1.0 / 60.0),
            SpringConfig::default(),
        );
        if [1, 3, 6, 12, 18].contains(&i) {
            println!("old normal {i}: {}, {}", value.current, value.velocity);
        }
    }

    let mut value = AnimatedValue::new(0.0);
    value.set_target(1000.0);
    legacy_advance(
        &mut value,
        Duration::from_millis(40),
        SpringConfig::default(),
    );
    println!("old retarget before: {}, {}", value.current, value.velocity);
    let position = value.current;
    let velocity = value.velocity;
    value.retarget_preserving_motion(2000.0);
    assert_eq!(value.current, position);
    assert_eq!(value.velocity, velocity);
    value.retarget_preserving_motion(-500.0);
    println!("old reversal: {}, {}", value.current, value.velocity);
    assert_eq!(value.current, position);
    assert_eq!(value.velocity, velocity);
}

#[test]
fn long_stall_uses_the_whole_elapsed_duration() {
    let config = SpringConfig {
        mass: 1.0,
        stiffness: 100.0,
        damping: 20.0,
        ..SpringConfig::default()
    };
    let mut clock = FakeClock::new();
    let mut value = AnimatedValue::new(0.0);
    value.set_target(500.0);
    clock.sample(Duration::from_millis(16), &mut value, config);
    let sample = clock.sample(Duration::from_millis(416), &mut value, config);
    let t: f64 = 0.416;
    let decay = (-10.0 * t).exp();
    close(
        sample.current,
        500.0 - 500.0 * (1.0 + 10.0 * t) * decay,
        1e-8,
    );
    close(sample.velocity, 50000.0 * t * decay, 1e-8);
    assert_eq!(sample.time, Duration::from_millis(416));
    assert_eq!(sample.target, 500.0);
}

#[test]
fn physical_solver_crosses_equilibrium_without_zeroing_velocity() {
    let mut x = 0.0;
    let mut v = 20000.0;
    advance_value(&mut x, 100.0, &mut v, 0.01, SpringConfig::default());
    assert!(x > 100.0, "physical solution was snapped: {x}");
    assert!(v > 0.0, "velocity was erased: {v}");
}

#[test]
fn fake_clock_pause_excludes_three_hundred_milliseconds() {
    let mut clock = FakeClock::new();
    let mut value = AnimatedValue::new(0.0);
    value.set_target(500.0);
    clock.sample(
        Duration::from_millis(40),
        &mut value,
        SpringConfig::default(),
    );
    let frozen = value;
    clock.pause();
    clock.sample(
        Duration::from_millis(340),
        &mut value,
        SpringConfig::default(),
    );
    assert_eq!(value, frozen);
    clock.resume(Duration::from_millis(340));
    clock.sample(
        Duration::from_millis(356),
        &mut value,
        SpringConfig::default(),
    );
    let mut expected = frozen;
    expected.advance(Duration::from_millis(16), SpringConfig::default());
    assert_eq!(value, expected);
}

fn physical(x: f64, v: f64, target: f64, dt: f64, config: SpringConfig) -> (f64, f64) {
    let (mut x, mut v) = (x, v);
    advance_value(&mut x, target, &mut v, dt, config);
    (x, v)
}

// Independent characteristic-root reference, used away from ill-conditioned roots.
fn reference(x: f64, v: f64, target: f64, dt: f64, config: SpringConfig) -> (f64, f64) {
    let a = config.damping / (2.0 * config.mass);
    let omega2 = config.stiffness / config.mass;
    let y = x - target;
    let d = a * a - omega2;
    if d.abs() < 1e-10 {
        let b = v + a * y;
        let e = (-a * dt).exp();
        return (target + e * (y + b * dt), e * (v - a * b * dt));
    }

    if d < 0.0 {
        let w = (-d).sqrt();
        let b = (v + a * y) / w;
        let (s, c) = (w * dt).sin_cos();
        let e = (-a * dt).exp();
        (
            target + e * (y * c + b * s),
            e * (v * c - (a * v + omega2 * y) / w * s),
        )
    } else {
        let q = d.sqrt();
        let r1 = -a + q;
        let r2 = -a - q;
        let c1 = (v - r2 * y) / (r1 - r2);
        let c2 = y - c1;
        let e1 = (r1 * dt).exp();
        let e2 = (r2 * dt).exp();
        (target + c1 * e1 + c2 * e2, c1 * r1 * e1 + c2 * r2 * e2)
    }
}

#[test]
fn solver_reference_regimes_initial_states_and_times() {
    for damping in [
        0.0,
        5.0,
        20.0,
        20.0 * (1.0 - 1e-9),
        20.0 * (1.0 + 1e-9),
        53.0,
        2000.0,
    ] {
        let config = SpringConfig {
            stiffness: 100.0,
            damping,
            ..SpringConfig::default()
        };
        for (x, v, target) in [
            (0., 0., 0.),
            (0., 100., 0.),
            (500., 0., 0.),
            (-500., 1234., 200.),
            (500., -1234., -200.),
        ] {
            for dt in [0.0, 1e-12, 0.0083, 0.05, 0.4, 5.0] {
                let actual = physical(x, v, target, dt, config);
                let expected = reference(x, v, target, dt, config);
                close(actual.0, expected.0, 1e-6);
                close(actual.1, expected.1, 1e-5);
            }
        }
    }
}

#[test]
fn near_critical_sweep_has_no_regime_jump() {
    let base = SpringConfig {
        stiffness: 100.0,
        damping: 20.0,
        ..SpringConfig::default()
    };
    for dt in [1e-12, 0.01, 0.2, 2.0] {
        let critical = physical(-500., 1234., 100., dt, base);
        for offset in [-1e-9, -1e-12, 0., 1e-12, 1e-9] {
            let config = SpringConfig {
                damping: 20.0 * (1.0 + offset),
                ..base
            };
            let actual = physical(-500., 1234., 100., dt, config);
            close(actual.0, critical.0, 1e-6);
            close(actual.1, critical.1, 1e-5);
        }
    }
}

#[test]
fn normal_trace_records_the_old_numerical_error() {
    // Captured BEFORE replacing the integrator. Duration rounds to nanoseconds,
    // giving five old substeps at 60 Hz, not four exact 1/240-second steps.
    let gold = [
        (1, 45.440622413787736, 3968.266514512032),
        (3, 204.05862514520575, 4637.558952650968),
        (6, 375.42995175442246, 2376.3948049425335),
        (12, 482.2490947739653, 366.6034676365624),
        (18, 497.62691927670414, 49.586155106479524),
    ];
    let mut old = AnimatedValue::new(0.0);
    old.set_target(500.0);
    let mut max_error: f64 = 0.0;
    for i in 1..=18 {
        legacy_advance(
            &mut old,
            Duration::from_secs_f64(1.0 / 60.0),
            SpringConfig::default(),
        );
        if let Some((_, x, v)) = gold.iter().find(|(n, _, _)| *n == i) {
            close(old.current, *x, 1e-9);
            close(old.velocity, *v, 1e-9);
        }
        let dt = Duration::from_secs_f64(1.0 / 60.0).as_secs_f64() * i as f64;
        let exact = reference(0., 0., 500., dt, SpringConfig::default());
        max_error = max_error.max((old.current - exact.0).abs());
    }
    assert!(
        max_error > 13.0 && max_error < 14.0,
        "old integrator error: {max_error}"
    );
}

#[test]
fn retarget_golden_policy_values() {
    let mut old = AnimatedValue::new(0.0);
    old.set_target(1000.0);
    legacy_advance(&mut old, Duration::from_millis(40), SpringConfig::default());
    close(old.current, 318.94300938241565, 1e-9);
    close(old.velocity, 9837.123292018367, 1e-9);
    old.retarget_preserving_motion(2000.);
    close(old.velocity, 9837.123292018367, 1e-9);
    old.retarget_preserving_motion(-500.);
    close(old.velocity, 9837.123292018367, 1e-9);
}

#[test]
fn zero_elapsed_time_is_a_strict_noop_even_inside_settling_tolerance() {
    let mut value = AnimatedValue::new(0.05);
    value.set_target(0.0);
    let before = value;
    value.advance(Duration::ZERO, SpringConfig::default());
    assert_eq!(value, before);
}

#[test]
fn gesture_flick_trace_crosses_once_and_settles_while_clamp_stops_at_crossing() {
    let config = SpringConfig::default();
    let mut free = AnimatedValue::new(0.0);
    free.velocity = 20000.0;
    free.set_target(100.0);
    let mut clamped = free;
    let mut crossings = 0;
    let mut previous_error: f64 = -100.0;
    for ms in 1..=1500 {
        free.advance_with_policy(
            Duration::from_millis(1),
            config,
            CrossingPolicy::AllowOvershoot,
        );
        clamped.advance_with_policy(Duration::from_millis(1), config, CrossingPolicy::NoCrossing);
        let error = free.current - free.target;
        if error != 0.0 && previous_error.signum() != error.signum() {
            crossings += 1;
        }
        if error != 0.0 {
            previous_error = error;
        }
        assert!(clamped.current <= clamped.target);
        if clamped.current == clamped.target {
            assert_eq!(clamped.velocity, 0.0);
        }
        if ms <= 200 {
            let exact = reference(0., 20000., 100., ms as f64 / 1000.0, config);
            close(free.current, exact.0, 1e-7);
            close(free.velocity, exact.1, 1e-6);
        }
    }
    assert_eq!(crossings, 1);
    assert_eq!((free.current, free.velocity), (100., 0.));
    assert_eq!((clamped.current, clamped.velocity), (100., 0.));
}

#[test]
fn rapid_retarget_trace_preserves_position_and_the_golden_policy() {
    let mut clock = FakeClock::new();
    let mut value = AnimatedValue::new(0.0);
    value.set_target(1000.0);
    clock.sample(
        Duration::from_millis(40),
        &mut value,
        SpringConfig::default(),
    );
    let before = value;
    value.retarget_preserving_motion(2000.0);
    assert_eq!(value.current, before.current);
    assert_eq!(value.velocity, before.velocity);
    clock.sample(
        Duration::from_millis(65),
        &mut value,
        SpringConfig::default(),
    );
    let before = value;
    value.retarget_preserving_motion(-500.0);
    assert_eq!(value.current, before.current);
    assert_eq!(value.velocity, before.velocity);
    clock.sample(
        Duration::from_millis(81),
        &mut value,
        SpringConfig::default(),
    );
    assert_ne!(value.velocity, 0.0);

    // Apply the current policy to the exact old in-flight state too.
    value.current = 318.94300938241565;
    value.velocity = 9837.123292018367;
    value.retarget_preserving_motion(-500.0);
    close(value.velocity, 9837.123292018367, 1e-9);
}

#[test]
fn interrupted_resize_and_zoom_targets_preserve_position_and_velocity() {
    let initial = Rect::new(0., 0., 400., 300.);
    let mut geometry = WindowGeometry::new(initial, None);
    geometry.set_logical_target(Rect::new(100., 50., 900., 600.), Duration::ZERO);
    geometry.advance(Duration::from_millis(40), SpringConfig::default(), true);
    for mode in [
        PresentationMode::Normal,
        PresentationMode::Maximized,
        PresentationMode::Fullscreen,
        PresentationMode::Normal,
    ] {
        let before = (geometry.visual.current, geometry.decorations);
        let velocity = geometry.visual.velocity;
        geometry.set_presentation_mode(
            Rect::new(200., 100., 700., 500.),
            mode,
            Duration::from_millis(40),
        );
        assert_eq!((geometry.visual.current, geometry.decorations), before);
        assert_eq!(geometry.visual.velocity, velocity);
        geometry.advance(Duration::ZERO, SpringConfig::default(), true);
        assert_eq!((geometry.visual.current, geometry.decorations), before);
        geometry.advance(Duration::from_millis(16), SpringConfig::default(), true);
    }
}

#[test]
fn variable_cadence_traces_match_reference_at_every_absolute_timestamp() {
    let config = SpringConfig::default();
    let mut schedules = vec![vec![16600], vec![8300], vec![5000, 22000, 11000, 37000]];
    for mut seed in [1u64, 0x12345678, 0xdeadbeef] {
        let mut seeded = Vec::new();
        for _ in 0..64 {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
            seeded.push((1 + (seed >> 32) % 47) * 1000);
        }
        schedules.push(seeded);
    }

    for schedule in schedules {
        let mut clock = FakeClock::new();
        let mut value = AnimatedValue::new(-100.0);
        value.set_target(500.0);
        let mut elapsed = Duration::ZERO;
        let mut settled_at = None;
        for i in 0..500 {
            elapsed += Duration::from_micros(schedule[i % schedule.len()]);
            let sample = clock.sample(elapsed, &mut value, config);
            let exact = reference(-100., 0., 500., elapsed.as_secs_f64(), config);
            if sample.current == sample.target && sample.velocity == 0.0 {
                assert_eq!((sample.current, sample.velocity), (500., 0.));
                settled_at.get_or_insert(elapsed);
            } else {
                close(sample.current, exact.0, config.position_tolerance);
                close(sample.velocity, exact.1, config.velocity_tolerance);
            }
            if elapsed >= Duration::from_secs(2) {
                break;
            }
        }
        let settled_at = settled_at.unwrap();
        let mut reference_settle_us = 0;
        while {
            let (x, v) = reference(-100., 0., 500., reference_settle_us as f64 / 1e6, config);
            (x - 500.).abs() > config.position_tolerance || v.abs() > config.velocity_tolerance
        } {
            reference_settle_us += 100;
        }
        let exact_settle = Duration::from_micros(reference_settle_us);
        assert!(
            settled_at.abs_diff(exact_settle)
                <= Duration::from_micros(*schedule.iter().max().unwrap() + 100)
        );
    }
}

#[test]
fn scalar_and_rect_share_math_and_rect_clamps_only_the_crossing_component() {
    let mut value = AnimatedValue::new(0.0);
    value.set_target(100.0);
    value.velocity = 20000.0;
    let mut rect = AnimatedRect::new(Rect::new(0., 0., 400., 300.));
    rect.set_target(Rect::new(100., 500., 400., 300.));
    rect.velocity.x = value.velocity;
    for policy in [CrossingPolicy::NoCrossing, CrossingPolicy::AllowOvershoot] {
        let mut scalar = value;
        let mut shape = rect;
        scalar.advance_with_policy(Duration::from_millis(10), SpringConfig::default(), policy);
        shape.advance_with_policy(Duration::from_millis(10), SpringConfig::default(), policy);
        assert_eq!(scalar.current, shape.current.x);
        assert_eq!(scalar.velocity, shape.velocity.x);
        assert!(shape.current.y > 0.0 && shape.current.y < 500.0);
        if policy == CrossingPolicy::NoCrossing {
            assert_eq!(shape.current.x, 100.0);
            assert_eq!(shape.velocity.x, 0.0);
            assert_ne!(shape.velocity.y, 0.0);
        }
    }
}

#[test]
fn clamp_detects_crossing_inside_a_stall_even_when_endpoints_have_same_sign() {
    let config = SpringConfig {
        stiffness: 100.0,
        damping: 0.0,
        ..SpringConfig::default()
    };
    let mut value = AnimatedValue::new(0.0);
    value.set_target(100.0);
    value.advance_with_policy(
        Duration::from_secs_f64(std::f64::consts::TAU / 10.0),
        config,
        CrossingPolicy::NoCrossing,
    );
    assert_eq!((value.current, value.velocity), (100., 0.));
}

#[test]
fn solver_is_finite_for_realistic_ranges_and_invalid_configuration_falls_back() {
    for mass in [0.01, 1.0, 100.0] {
        for stiffness in [0.1, 700.0, 100000.0] {
            for damping in [0.0, 0.01, 53.0, 1000000.0] {
                for dt in [1e-12, 0.008, 0.4, 10.0, 3600.0] {
                    let config = SpringConfig {
                        mass,
                        stiffness,
                        damping,
                        ..SpringConfig::default()
                    };
                    let (x, v) = physical(-10000., 50000., 10000., dt, config);
                    assert!(
                        x.is_finite() && v.is_finite(),
                        "{config:?} dt {dt}: {x} {v}"
                    );
                }
            }
        }
    }
    for bad in [0.0, -1.0, f64::NAN, f64::INFINITY] {
        for config in [
            SpringConfig {
                mass: bad,
                ..SpringConfig::default()
            },
            SpringConfig {
                stiffness: bad,
                ..SpringConfig::default()
            },
        ] {
            assert_eq!(
                physical(0., 10., 100., 0.1, config),
                physical(0., 10., 100., 0.1, SpringConfig::default())
            );
        }
    }

    for bad in [-1.0, f64::NAN, f64::INFINITY] {
        let config = SpringConfig {
            damping: bad,
            ..SpringConfig::default()
        };
        assert_eq!(
            physical(0., 10., 100., 0.1, config),
            physical(0., 10., 100., 0.1, SpringConfig::default())
        );
    }
}

#[test]
fn physical_zero_and_negative_time_do_not_move_state() {
    for dt in [0.0, -0.01] {
        assert_eq!(
            physical(5.0, 100.0, 10.0, dt, SpringConfig::default()),
            (5.0, 100.0)
        );
    }
}

#[test]
fn resize_target_rebases_an_existing_zoom_without_a_zero_time_jump() {
    let mut geometry = WindowGeometry::new(Rect::new(100., 50., 400., 300.), None);
    geometry.set_presentation_mode(
        Rect::new(0., 0., 1920., 1080.),
        PresentationMode::Fullscreen,
        Duration::ZERO,
    );
    geometry.advance(Duration::from_millis(40), SpringConfig::default(), true);
    let before = (geometry.visual.current, geometry.decorations);
    geometry.set_logical_target(Rect::new(0., 0., 1600., 900.), Duration::from_millis(40));
    geometry.advance(Duration::ZERO, SpringConfig::default(), true);
    assert_eq!((geometry.visual.current, geometry.decorations), before);
    geometry.advance(Duration::from_nanos(1), SpringConfig::default(), true);
    close(geometry.visual.current.width, before.0.width, 1e-4);
}

#[test]
fn small_default_normal_motion_matches_legacy_within_half_a_pixel() {
    let mut old = AnimatedValue::new(0.0);
    old.set_target(10.0);
    let mut clock = FakeClock::new();
    let mut exact = old;
    for i in 1..=120 {
        let step = Duration::from_secs_f64(1.0 / 60.0);
        legacy_advance(&mut old, step, SpringConfig::default());
        let sample = clock.sample(step * i, &mut exact, SpringConfig::default());
        close(sample.current, old.current, 0.5);
    }
}

#[test]
fn creation_clock_excludes_earlier_idle_time_and_records_all_sample_fields() {
    let mut clock = FakeClock::new();
    let created = Duration::from_secs(10);
    clock.resume(created);
    let mut value = AnimatedValue::new(0.0);
    value.set_target(500.0);
    let first = clock.sample(
        created + Duration::from_millis(16),
        &mut value,
        SpringConfig::default(),
    );
    let exact = reference(0.0, 0.0, 500.0, 0.016, SpringConfig::default());
    close(first.current, exact.0, 1e-8);
    close(first.velocity, exact.1, 1e-8);
    assert_eq!(first.time, created + Duration::from_millis(16));
    assert_eq!(first.target, 500.0);
    assert_eq!(clock.trace.len(), 1);
    value.retarget_preserving_motion(800.0);
    let same_time = clock.sample(first.time, &mut value, SpringConfig::default());
    assert_eq!(same_time.current, first.current);
    assert_eq!(same_time.velocity, first.velocity);
    assert_eq!(same_time.target, 800.0);
}

#[test]
fn damped_physical_solutions_converge_in_all_three_regimes() {
    for damping in [5.0, 20.0, 53.0, 2000.0] {
        let config = SpringConfig {
            stiffness: 100.0,
            damping,
            ..SpringConfig::default()
        };
        let (x, v) = physical(-500.0, 1234.0, 100.0, 2000.0, config);
        close(x, 100.0, 1e-9);
        close(v, 0.0, 1e-9);
    }
}

#[test]
fn solver_is_continuous_on_both_sides_of_the_flick_crossing() {
    let config = SpringConfig::default();
    let (mut low, mut high) = (0.0, 0.02);
    for _ in 0..60 {
        let middle = (low + high) / 2.0;
        if physical(0.0, 20000.0, 100.0, middle, config).0 < 100.0 {
            low = middle;
        } else {
            high = middle;
        }
    }
    let before = physical(0.0, 20000.0, 100.0, low - 1e-9, config);
    let after = physical(0.0, 20000.0, 100.0, high + 1e-9, config);
    assert!(before.0 < 100.0 && after.0 > 100.0);
    close(before.0, after.0, 1e-4);
    close(before.1, after.1, 0.01);
    assert!(after.1 > 1000.0);
}
