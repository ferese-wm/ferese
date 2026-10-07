use super::*;

#[test]
fn dim_fade_respects_speed_without_changing_idle_deadlines() {
    let policy = IdleSettings::default();
    for (duration, half_at) in [(250, 30_125), (1000, 30_500)] {
        let fade = Duration::from_millis(duration);
        assert_eq!(policy.appearance(Duration::from_secs(29), fade), (0.0, false));
        let halfway = policy.appearance(Duration::from_millis(half_at), fade);
        assert!((halfway.0 - 0.325).abs() < 0.001 && !halfway.1);
        assert_eq!(
            policy.next_deadline(Duration::from_millis(half_at), fade),
            Some(Duration::from_millis(16))
        );
        assert_eq!(policy.appearance(Duration::from_secs(120), fade), (1.0, true));
    }
    assert_eq!(
        policy.appearance(Duration::from_secs(30), Duration::ZERO),
        (0.65, false)
    );
    assert_eq!(policy.appearance(Duration::from_secs(120), Duration::ZERO), (1.0, true));
}

#[test]
fn idle_policy_fades_then_sleeps_and_activity_resets_it() {
    let policy = IdleSettings::default();
    assert_eq!(
        policy.appearance(Duration::from_secs(29), Duration::from_millis(500)),
        (0.0, false)
    );
    let halfway = policy.appearance(Duration::from_millis(30_250), Duration::from_millis(500));
    assert!((halfway.0 - 0.325).abs() < 0.001 && !halfway.1);
    assert_eq!(
        policy.appearance(Duration::from_secs(31), Duration::from_millis(500)),
        (0.65, false)
    );
    assert_eq!(
        policy.appearance(Duration::from_secs(120), Duration::from_millis(500)),
        (1.0, true)
    );
    assert_eq!(
        policy.appearance(Duration::ZERO, Duration::from_millis(500)),
        (0.0, false)
    );
    assert_eq!(
        policy.next_deadline(Duration::from_millis(29_998), Duration::from_millis(500)),
        Some(Duration::from_millis(2))
    );
    assert_eq!(
        policy.next_deadline(Duration::from_secs(30), Duration::from_millis(500)),
        Some(Duration::from_millis(16))
    );
    assert_eq!(
        policy.next_deadline(Duration::from_secs(30), Duration::ZERO),
        Some(Duration::from_secs(90))
    );
    assert_eq!(policy.next_deadline(Duration::from_secs(120), Duration::ZERO), None);
    assert_eq!(
        IdleSettings {
            dim_after_seconds: 0,
            sleep_after_seconds: 0
        }
        .next_deadline(Duration::ZERO, Duration::ZERO),
        None
    );
}

#[test]
fn mirror_must_present_protected_frame_before_lock_is_ready() {
    let make_output = |name: &str| {
        Output::new(
            name.into(),
            smithay::output::PhysicalProperties {
                size: (0, 0).into(),
                subpixel: smithay::output::Subpixel::Unknown,
                make: "test".into(),
                model: "test".into(),
            },
        )
    };
    let source = make_output("source");
    let mirror = make_output("mirror");
    let mut lock = Lock {
        lifecycle: Lifecycle::Orphaned,
        ..Lock::default()
    };
    lock.presented.insert(source.clone());
    let displays = [&source, &mirror];
    assert!(
        !lock.ready_for_idle(displays.into_iter()),
        "failed mirror rendering must block confirmation"
    );
    lock.presented.insert(mirror.clone());
    assert!(lock.ready_for_idle(displays.into_iter()));
    lock.output_added(&mirror);
    assert!(
        !lock.ready_for_idle(displays.into_iter()),
        "reconfiguration invalidates physical protection"
    );
    lock.output_removed(&mirror);
    assert!(lock.ready_for_idle(std::iter::once(&source)));
}

#[test]
fn protected_fallback_frames_allow_idle_after_early_locker_crash() {
    let output = Output::new(
        "test".into(),
        smithay::output::PhysicalProperties {
            size: (0, 0).into(),
            subpixel: smithay::output::Subpixel::Unknown,
            make: "test".into(),
            model: "test".into(),
        },
    );
    let mut lock = Lock {
        lifecycle: Lifecycle::Orphaned,
        ..Lock::default()
    };
    assert!(!lock.ready_for_idle(std::iter::once(&output)));
    lock.presented.insert(output.clone());
    assert!(lock.ready_for_idle(std::iter::once(&output)));
    assert!(matches!(lock.lifecycle, Lifecycle::Orphaned));
    lock.output_added(&output);
    assert!(!lock.ready_for_idle(std::iter::once(&output)));
    assert!(matches!(lock.lifecycle, Lifecycle::Orphaned));
}

#[test]
fn input_wakes_a_sleeping_orphan_without_unlocking() {
    let mut lock = Lock {
        lifecycle: Lifecycle::Orphaned,
        sleeping: true,
        idle_opacity: 1.0,
        ..Lock::default()
    };
    let now = Instant::now();
    assert!(lock.activity(now));
    assert!(lock.active() && matches!(lock.lifecycle, Lifecycle::Orphaned));
    assert!(!lock.sleeping);
    assert_eq!(lock.idle_opacity, 0.0);
    assert_eq!(lock.idle_since, Some(now));
    assert!(!lock.activity(now));
    let mut unlocked = Lock::default();
    assert!(!unlocked.activity(now));
    assert!(unlocked.idle_since.is_none());
}

#[test]
fn idle_disabled_options_are_independent() {
    assert_eq!(
        IdleSettings {
            dim_after_seconds: 0,
            sleep_after_seconds: 0
        }
        .appearance(Duration::from_secs(9999), Duration::from_millis(500)),
        (0.0, false)
    );
    assert_eq!(
        IdleSettings {
            dim_after_seconds: 0,
            sleep_after_seconds: 10
        }
        .appearance(Duration::from_secs(10), Duration::from_millis(500)),
        (1.0, true)
    );
    assert_eq!(
        IdleSettings {
            dim_after_seconds: 1,
            sleep_after_seconds: 0
        }
        .appearance(Duration::from_secs(9999), Duration::from_millis(500)),
        (0.65, false)
    );
    assert!(
        IdleSettings {
            dim_after_seconds: 86401,
            sleep_after_seconds: 0
        }
        .validate()
        .is_err()
    );
    assert!(crate::config::Config::parse_source("lock-screen { dim-after-seconds -1; }").is_err());
    assert!(
        crate::config::Config::parse_source("lock-screen { sleep-after-seconds 90000; }")
            .unwrap()
            .runtime_config()
            .is_err()
    );
}

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::sync::Arc;

use calloop::EventLoop;
use smithay::reexports::wayland_server::Client;
use smithay::reexports::wayland_server::protocol::wl_compositor::WlCompositor;

struct Fixture {
    events: EventLoop<'static, Ferese>,
    state: Ferese,
}

impl Fixture {
    fn new(test: &str) -> Option<Self> {
        if !crate::startup_tests::private_runtime(test) {
            return None;
        }

        let mut events = EventLoop::try_new().unwrap();
        let state = crate::startup_tests::state(&mut events);
        Some(Self { events, state })
    }

    fn dispatch(&mut self) {
        self.events.dispatch(Duration::from_millis(5), &mut self.state).unwrap();
        crate::after_dispatch(&mut self.state);
    }

    fn output(&mut self, name: &str) -> Output {
        let output = Output::new(
            name.into(),
            smithay::output::PhysicalProperties {
                size: (0, 0).into(),
                subpixel: smithay::output::Subpixel::Unknown,
                make: "test".into(),
                model: "test".into(),
            },
        );
        output.change_current_state(
            Some(smithay::output::Mode {
                size: (800, 600).into(),
                refresh: 60_000,
            }),
            None,
            None,
            None,
        );
        self.state.space.map_output(&output, (0, 0));
        self.state.register_output(&output, name.into());
        output
    }

    fn acquire(&mut self) -> Locker {
        let (server, mut wire) = UnixStream::pair().unwrap();
        let client = self
            .state
            .display_handle
            .insert_client(server, Arc::new(crate::state::ClientState::default()))
            .unwrap();
        let manager = client
            .create_resource::<ExtSessionLockManagerV1, (), Ferese>(&self.state.display_handle, 1, ())
            .unwrap();
        request(&mut wire, manager.id().protocol_id(), 1, &[2]);
        self.dispatch();
        Locker {
            wire,
            client,
            next_id: 3,
        }
    }
}

struct Locker {
    wire: UnixStream,
    client: Client,
    next_id: u32,
}

impl Locker {
    fn id(&mut self) -> u32 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    fn expect_locked(&mut self) {
        self.wire.set_read_timeout(Some(Duration::from_secs(1))).unwrap();
        let mut event = [0u8; 8];
        self.wire.read_exact(&mut event).unwrap();
        assert_eq!(u32::from_ne_bytes(event[..4].try_into().unwrap()), 2);
        assert_eq!(u32::from_ne_bytes(event[4..].try_into().unwrap()), 8u32 << 16);
    }

    fn surface(&mut self, fixture: &mut Fixture, output: &Output) -> LockSurface {
        output.create_global::<Ferese>(&fixture.state.display_handle);
        let registry = self.id();
        request(&mut self.wire, 1, 1, &[registry]);
        fixture.dispatch();
        self.wire.set_nonblocking(true).unwrap();
        let mut events = Vec::new();
        let _ = self.wire.read_to_end(&mut events);
        self.wire.set_nonblocking(false).unwrap();
        let mut offset = 0;
        let mut output_name = None;
        while offset < events.len() {
            let object = u32::from_ne_bytes(events[offset..offset + 4].try_into().unwrap());
            let header = u32::from_ne_bytes(events[offset + 4..offset + 8].try_into().unwrap());
            if object == registry && header & 0xffff == 0 {
                let len = u32::from_ne_bytes(events[offset + 12..offset + 16].try_into().unwrap()) as usize;
                if &events[offset + 16..offset + 16 + len] == b"wl_output\0" {
                    let name = u32::from_ne_bytes(events[offset + 8..offset + 12].try_into().unwrap());
                    output_name = Some(output_name.map_or(name, |previous: u32| previous.max(name)));
                }
            }

            offset += (header >> 16) as usize;
        }

        let output_id = self.id();
        let mut args = vec![output_name.expect("output global"), 10];
        args.extend(
            b"wl_output\0\0\0"
                .chunks_exact(4)
                .map(|bytes| u32::from_ne_bytes(bytes.try_into().unwrap())),
        );
        args.extend([4, output_id]);
        request(&mut self.wire, registry, 0, &args);
        let compositor = self
            .client
            .create_resource::<WlCompositor, (), Ferese>(&fixture.state.display_handle, 6, ())
            .unwrap();
        let surface_id = self.id();
        request(&mut self.wire, compositor.id().protocol_id(), 0, &[surface_id]);
        let lock_surface = self.id();
        request(&mut self.wire, 2, 1, &[lock_surface, surface_id, output_id]);
        fixture.dispatch();
        fixture
            .state
            .session_lock
            .surfaces
            .get(output)
            .expect("lock surface on requested output")
            .clone()
    }
}

fn request(wire: &mut UnixStream, object: u32, opcode: u32, args: &[u32]) {
    for word in [object, (((args.len() + 2) * 4) as u32) << 16 | opcode]
        .into_iter()
        .chain(args.iter().copied())
    {
        wire.write_all(&word.to_ne_bytes()).unwrap();
    }
}

#[test]
fn lock_confirms_without_outputs() {
    let Some(mut fixture) = Fixture::new("session_lock::tests::lock_confirms_without_outputs") else {
        return;
    };
    let epoch = fixture.state.capture_epoch;
    let mut locker = fixture.acquire();
    assert!(matches!(fixture.state.session_lock.lifecycle, Lifecycle::Locked(_)));
    assert_ne!(fixture.state.capture_epoch, epoch, "lock revokes deferred snapshots");
    locker.expect_locked();
}

#[test]
fn lock_waits_for_each_output_before_confirmation() {
    let Some(mut fixture) = Fixture::new("session_lock::tests::lock_waits_for_each_output_before_confirmation") else {
        return;
    };
    let first = fixture.output("first");
    let second = fixture.output("second");
    let mut locker = fixture.acquire();
    assert!(matches!(fixture.state.session_lock.lifecycle, Lifecycle::Acquiring(_)));
    fixture.state.lock_frame_presented(&first);
    assert!(matches!(fixture.state.session_lock.lifecycle, Lifecycle::Acquiring(_)));
    assert!(fixture.state.session_lock.idle_timer.is_none());
    fixture.state.lock_frame_presented(&second);
    assert!(matches!(fixture.state.session_lock.lifecycle, Lifecycle::Locked(_)));
    fixture.dispatch();
    locker.expect_locked();
}

#[test]
fn live_owner_rejects_competitor_unlock() {
    let Some(mut fixture) = Fixture::new("session_lock::tests::live_owner_rejects_competitor_unlock") else {
        return;
    };
    let _owner_wire = fixture.acquire();
    let owner = fixture.state.session_lock.lifecycle.owner().cloned();
    let mut competitor = fixture.acquire();
    assert_eq!(fixture.state.session_lock.lifecycle.owner(), owner.as_ref());
    request(&mut competitor.wire, 2, 2, &[]);
    fixture.dispatch();
    assert_eq!(fixture.state.session_lock.lifecycle.owner(), owner.as_ref());
    assert!(fixture.state.session_lock.active());
}

#[test]
fn pending_owner_cannot_unlock_before_confirmation() {
    let Some(mut fixture) = Fixture::new("session_lock::tests::pending_owner_cannot_unlock_before_confirmation") else {
        return;
    };
    fixture.output("pending");
    let mut pending = fixture.acquire();
    assert!(matches!(fixture.state.session_lock.lifecycle, Lifecycle::Acquiring(_)));
    request(&mut pending.wire, 2, 2, &[]);
    fixture.dispatch();
    assert!(fixture.state.session_lock.active());
    fixture.dispatch();
    assert!(matches!(fixture.state.session_lock.lifecycle, Lifecycle::Orphaned));
}

#[test]
fn owner_disconnect_stays_locked_and_allows_replacement() {
    let Some(mut fixture) = Fixture::new("session_lock::tests::owner_disconnect_stays_locked_and_allows_replacement")
    else {
        return;
    };
    let owner_wire = fixture.acquire();
    let owner = fixture.state.session_lock.lifecycle.owner().cloned();
    drop(owner_wire);
    fixture.dispatch();
    assert!(matches!(fixture.state.session_lock.lifecycle, Lifecycle::Orphaned));
    assert!(fixture.state.session_lock.active());
    let mut replacement = fixture.acquire();
    assert!(matches!(fixture.state.session_lock.lifecycle, Lifecycle::Locked(_)));
    assert_ne!(fixture.state.session_lock.lifecycle.owner(), owner.as_ref());
    replacement.expect_locked();
}

#[test]
fn removing_last_output_confirms_pending_lock() {
    let Some(mut fixture) = Fixture::new("session_lock::tests::removing_last_output_confirms_pending_lock") else {
        return;
    };
    let output = fixture.output("last");
    let mut pending = fixture.acquire();
    assert!(matches!(fixture.state.session_lock.lifecycle, Lifecycle::Acquiring(_)));
    fixture.state.unregister_output(&output);
    assert!(matches!(fixture.state.session_lock.lifecycle, Lifecycle::Locked(_)));
    fixture.dispatch();
    pending.expect_locked();
}

#[test]
fn refresh_after_removal_confirms_and_starts_idle_timer() {
    let Some(mut fixture) = Fixture::new("session_lock::tests::refresh_after_removal_confirms_and_starts_idle_timer")
    else {
        return;
    };
    let output = fixture.output("last");
    let mut pending = fixture.acquire();
    fixture.state.space.unmap_output(&output);
    fixture.state.session_lock.output_removed(&output);
    assert!(matches!(fixture.state.session_lock.lifecycle, Lifecycle::Acquiring(_)));
    fixture.state.refresh_lock_outputs();
    assert!(matches!(fixture.state.session_lock.lifecycle, Lifecycle::Locked(_)));
    assert!(fixture.state.session_lock.idle_timer.is_some());
    fixture.dispatch();
    pending.expect_locked();
}

#[test]
fn activity_reuses_timer_and_expired_timer_rechecks_latest_activity() {
    let Some(mut fixture) =
        Fixture::new("session_lock::tests::activity_reuses_timer_and_expired_timer_rechecks_latest_activity")
    else {
        return;
    };
    fixture.state.session_lock.lifecycle = Lifecycle::Orphaned;
    fixture.state.lock_idle = IdleSettings {
        dim_after_seconds: 1,
        sleep_after_seconds: 2,
    };
    fixture.state.session_lock.idle_since = Some(Instant::now() - Duration::from_millis(999));
    fixture.state.refresh_lock_idle_policy();
    let timer = fixture.state.session_lock.idle_timer.unwrap();
    for _ in 0..1000 {
        fixture.state.lock_input_activity();
        assert_eq!(
            fixture.state.session_lock.idle_timer,
            Some(timer),
            "input replaced the source"
        );
    }

    let limit = Instant::now() + Duration::from_secs(1);
    while fixture.state.session_lock.idle_timer.unwrap().1 == timer.1 {
        assert!(Instant::now() < limit, "idle timer did not recheck");
        fixture
            .events
            .dispatch(Duration::from_millis(10), &mut fixture.state)
            .unwrap();
    }

    let rechecked = fixture.state.session_lock.idle_timer.unwrap();
    assert_eq!(timer.0, rechecked.0, "timer callback replaced the source");
    assert!(rechecked.1 > timer.1);
    assert_eq!(fixture.state.session_lock.idle_opacity, 0.0);
    assert!(!fixture.state.session_lock.sleeping);
}

#[test]
fn earlier_policy_replaces_timer_and_sleeping_activity_arms_one() {
    let Some(mut fixture) =
        Fixture::new("session_lock::tests::earlier_policy_replaces_timer_and_sleeping_activity_arms_one")
    else {
        return;
    };
    fixture.state.session_lock.lifecycle = Lifecycle::Orphaned;
    fixture.state.lock_input_activity();
    let first = fixture.state.session_lock.idle_timer.unwrap();
    fixture.state.lock_idle.dim_after_seconds = 60;
    fixture.state.refresh_lock_idle_policy();
    assert_eq!(fixture.state.session_lock.idle_timer, Some(first));
    fixture.state.lock_idle.dim_after_seconds = 1;
    fixture.state.refresh_lock_idle_policy();
    let earlier = fixture.state.session_lock.idle_timer.unwrap();
    assert_ne!(first.0, earlier.0);
    assert!(earlier.1 < first.1);
    fixture.state.session_lock.idle_since = Some(Instant::now() - Duration::from_secs(121));
    fixture.state.refresh_lock_idle_policy();
    assert!(fixture.state.session_lock.sleeping);
    assert!(fixture.state.session_lock.idle_timer.is_none());
    fixture.state.lock_input_activity();
    assert!(fixture.state.session_lock.idle_timer.is_some());
    assert!(!fixture.state.session_lock.sleeping);
    assert_eq!(fixture.state.session_lock.idle_opacity, 0.0);
    assert!(fixture.state.session_lock.active());
}

#[test]
fn dead_preferred_surface_uses_live_fallback_in_stable_output_order() {
    let Some(mut fixture) =
        Fixture::new("session_lock::tests::dead_preferred_surface_uses_live_fallback_in_stable_output_order")
    else {
        return;
    };
    let preferred = fixture.output("preferred");
    let second = fixture.output("second");
    let third = fixture.output("third");
    let mut locker = fixture.acquire();
    let dead = locker.surface(&mut fixture, &preferred);
    let fallback = locker.surface(&mut fixture, &second);
    let later = locker.surface(&mut fixture, &third);
    let preferred_id = fixture.state.output_id(&preferred).unwrap();
    fixture.state.output_workspaces.focus_output(preferred_id).unwrap();
    request(&mut locker.wire, dead.wl_surface().id().protocol_id(), 0, &[]);
    fixture.dispatch();
    assert!(!dead.alive());
    // Exercise the defensive path even though destruction normally removes it.
    fixture.state.session_lock.surfaces.insert(preferred.clone(), dead);
    for _ in 0..5 {
        fixture.state.session_lock.surfaces.remove(&second);
        fixture
            .state
            .session_lock
            .surfaces
            .insert(second.clone(), fallback.clone());
        fixture.state.session_lock.surfaces.insert(third.clone(), later.clone());
        fixture.state.focus_lock_surface();
        assert_eq!(
            fixture.state.seat.get_keyboard().unwrap().current_focus(),
            Some(fallback.wl_surface().clone())
        );
    }
}

#[test]
fn output_removed_clears_all_lock_resources() {
    let Some(mut fixture) = Fixture::new("session_lock::tests::output_removed_clears_all_lock_resources") else {
        return;
    };
    let output = fixture.output("removed");
    let mut locker = fixture.acquire();
    locker.surface(&mut fixture, &output);
    let background =
        smithay::backend::renderer::element::solid::SolidColorBuffer::new((800, 600), [0.0, 0.0, 0.0, 1.0]);
    fixture
        .state
        .session_lock
        .backgrounds
        .insert(output.clone(), background.clone());
    fixture
        .state
        .session_lock
        .idle_overlays
        .insert(output.clone(), background);
    fixture.state.session_lock.presented.insert(output.clone());
    fixture.state.session_lock.output_removed(&output);
    let lock = &fixture.state.session_lock;
    assert!(!lock.surfaces.contains_key(&output));
    assert!(!lock.backgrounds.contains_key(&output));
    assert!(!lock.idle_overlays.contains_key(&output));
    assert!(!lock.presented.contains(&output));
    assert!(lock.active(), "output cleanup must not unlock");
}

#[test]
fn confirmed_owner_can_unlock_and_cancel_idle_timer() {
    let Some(mut fixture) = Fixture::new("session_lock::tests::confirmed_owner_can_unlock_and_cancel_idle_timer")
    else {
        return;
    };
    let mut locker = fixture.acquire();
    locker.expect_locked();
    assert!(fixture.state.session_lock.idle_timer.is_some());
    request(&mut locker.wire, 2, 2, &[]);
    fixture.dispatch();
    assert!(matches!(fixture.state.session_lock.lifecycle, Lifecycle::Unlocked));
    assert!(fixture.state.session_lock.idle_timer.is_none());
}

#[test]
fn acquiring_owner_disconnect_keeps_lock_and_replacement_waits_for_frame() {
    let Some(mut fixture) =
        Fixture::new("session_lock::tests::acquiring_owner_disconnect_keeps_lock_and_replacement_waits_for_frame")
    else {
        return;
    };
    let output = fixture.output("pending");
    let pending = fixture.acquire();
    assert!(matches!(fixture.state.session_lock.lifecycle, Lifecycle::Acquiring(_)));
    drop(pending);
    fixture.dispatch();
    assert!(matches!(fixture.state.session_lock.lifecycle, Lifecycle::Orphaned));
    assert!(fixture.state.session_lock.active());
    let mut replacement = fixture.acquire();
    assert!(matches!(fixture.state.session_lock.lifecycle, Lifecycle::Acquiring(_)));
    fixture.state.lock_frame_presented(&output);
    fixture.dispatch();
    assert!(matches!(fixture.state.session_lock.lifecycle, Lifecycle::Locked(_)));
    replacement.expect_locked();
}

#[test]
fn lock_input_preserves_owner_click_grabs_and_rejects_retired_surfaces() {
    let Some(mut fixture) =
        Fixture::new("session_lock::tests::lock_input_preserves_owner_click_grabs_and_rejects_retired_surfaces")
    else {
        return;
    };
    let output = fixture.output("lock-pointer");
    let mut owner = fixture.acquire();
    let surface = owner.surface(&mut fixture, &output);
    let pointer = fixture.state.seat.get_pointer().unwrap();
    let serial = SERIAL_COUNTER.next_serial();
    pointer.motion(
        &mut fixture.state,
        Some((surface.wl_surface().clone(), (0.0, 0.0).into())),
        &smithay::input::pointer::MotionEvent {
            location: (10.0, 10.0).into(),
            serial,
            time: 1,
        },
    );
    pointer.button(
        &mut fixture.state,
        &smithay::input::pointer::ButtonEvent {
            button: 0x110,
            state: smithay::backend::input::ButtonState::Pressed,
            serial,
            time: 2,
        },
    );
    assert!(pointer.is_grabbed());
    fixture.state.prepare_lock_input();
    assert!(
        pointer.is_grabbed(),
        "the lock owner's click must survive until release"
    );
    pointer.button(
        &mut fixture.state,
        &smithay::input::pointer::ButtonEvent {
            button: 0x110,
            state: smithay::backend::input::ButtonState::Released,
            serial: SERIAL_COUNTER.next_serial(),
            time: 3,
        },
    );
    assert!(!pointer.is_grabbed());
    pointer.button(
        &mut fixture.state,
        &smithay::input::pointer::ButtonEvent {
            button: 0x110,
            state: smithay::backend::input::ButtonState::Pressed,
            serial: SERIAL_COUNTER.next_serial(),
            time: 4,
        },
    );
    assert!(pointer.is_grabbed());
    fixture.state.session_lock.surfaces.clear();
    fixture.state.prepare_lock_input();
    assert!(!pointer.is_grabbed(), "a retired surface cannot keep a lock grab");
    assert_eq!(pointer.current_focus(), None);
    assert_eq!(fixture.state.seat.get_keyboard().unwrap().current_focus(), None);
}
