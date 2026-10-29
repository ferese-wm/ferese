use super::*;
use std::io::Write;
use std::os::fd::OwnedFd;
use std::os::unix::net::UnixStream;

use smithay::backend::allocator::dmabuf::{Dmabuf, DmabufFlags, DmabufSource};
use smithay::backend::allocator::{Fourcc, Modifier};
use smithay::backend::renderer::utils::with_renderer_surface_state;
use smithay::output::{Mode, Output, PhysicalProperties, Subpixel};
use smithay::reexports::calloop::EventLoop;

pub(super) fn fixture() -> (Ferese, EventLoop<'static, Ferese>, WlSurface, UnixStream) {
    let mut events = EventLoop::try_new().unwrap();
    let mut state = crate::startup_tests::state(&mut events);
    let output = Output::new(
        "acquire-test".into(),
        PhysicalProperties {
            serial_number: String::new(),
            size: (0, 0).into(),
            subpixel: Subpixel::Unknown,
            make: "test".into(),
            model: "test".into(),
        },
    );
    output.change_current_state(
        Some(Mode {
            size: (800, 600).into(),
            refresh: 60_000,
        }),
        None,
        None,
        None,
    );
    state.space.map_output(&output, (0, 0));
    state.register_output(&output, "acquire-test".into());
    let (window, wire) = crate::state::test_window(&mut state, &mut events, 0xff112233);
    (
        state,
        events,
        window.toplevel().unwrap().wl_surface().clone(),
        wire,
    )
}

// Socket readiness provides a deterministic stand-in for a DMA-BUF write fence.
// The source is the same Smithay DmabufSource used by real commit hooks; no GPU
// allocation/import is performed by this fixture.
fn fence() -> (DmabufSource, UnixStream) {
    let (read, write) = UnixStream::pair().unwrap();
    let mut buffer = Dmabuf::builder(
        (1, 1),
        Fourcc::Abgr8888,
        Modifier::Linear,
        DmabufFlags::empty(),
    );
    buffer.add_plane(OwnedFd::from(read), 0, 4);
    let (_, source) = buffer
        .build()
        .unwrap()
        .generate_blocker(Interest::READ)
        .unwrap();
    (source, write)
}

pub(super) fn dispatch(state: &mut Ferese, events: &mut EventLoop<'static, Ferese>) {
    events.dispatch(Duration::from_millis(1), state).unwrap();
    state.poll_buffer_sync();
    state.display_handle.flush_clients().unwrap();
}

#[test]
fn unsignalled_acquire_defers_real_commit_and_promotes_once_without_a_poll_timer() {
    if !crate::startup_tests::private_runtime(
        "buffer_sync::tests::unsignalled_acquire_defers_real_commit_and_promotes_once_without_a_poll_timer",
    ) {
        return;
    }
    let (mut state, mut events, surface, mut wire) = fixture();
    let before = with_renderer_surface_state(&surface, |data| data.current_commit()).unwrap();
    let (source, mut signal) = fence();
    state.wait_for_buffer(&surface, source, None, None, MAX_WAIT);
    let gate = state
        .buffer_sync
        .pending
        .values()
        .next()
        .unwrap()
        .gate
        .clone();
    crate::state::test_client_request(&mut wire, 2, 1, &[6, 0, 0], None);
    crate::state::test_client_request(&mut wire, 2, 2, &[0, 0, 64, 48], None);
    crate::state::test_client_request(&mut wire, 2, 6, &[], None);
    dispatch(&mut state, &mut events);
    assert_eq!(gate.state(), BlockerState::Pending);
    assert_eq!(
        with_renderer_surface_state(&surface, |data| data.current_commit()).unwrap(),
        before
    );
    signal.write_all(&[1]).unwrap();
    for _ in 0..3 {
        dispatch(&mut state, &mut events);
    }
    assert_eq!(gate.state(), BlockerState::Released);
    let committed = with_renderer_surface_state(&surface, |data| data.current_commit()).unwrap();
    assert_ne!(committed, before);
    assert!(state.buffer_sync.pending.is_empty());
    assert!(state.buffer_sync.notify.is_empty());
    dispatch(&mut state, &mut events);
    assert_eq!(
        with_renderer_surface_state(&surface, |data| data.current_commit()).unwrap(),
        committed
    );
}

#[test]
fn removal_cancels_the_transaction_and_late_readiness_cannot_release_it() {
    if !crate::startup_tests::private_runtime(
        "buffer_sync::tests::removal_cancels_the_transaction_and_late_readiness_cannot_release_it",
    ) {
        return;
    }
    let (mut state, mut events, surface, _wire) = fixture();
    let (source, mut signal) = fence();
    state.wait_for_buffer(&surface, source, None, None, MAX_WAIT);
    let gate = state
        .buffer_sync
        .pending
        .values()
        .next()
        .unwrap()
        .gate
        .clone();
    state.cancel_surface_buffer_waits(&surface);
    assert_eq!(gate.state(), BlockerState::Cancelled);
    assert!(state.buffer_sync.pending.is_empty());
    let _ = signal.write_all(&[1]);
    dispatch(&mut state, &mut events);
    assert_eq!(gate.state(), BlockerState::Cancelled);
    state.cancel_surface_buffer_waits(&surface);
}

#[test]
fn timeout_retires_all_waits_for_the_stalled_owner() {
    if !crate::startup_tests::private_runtime(
        "buffer_sync::tests::timeout_retires_all_waits_for_the_stalled_owner",
    ) {
        return;
    }
    let (mut state, mut events, surface, _wire) = fixture();
    let mut signals = Vec::new();
    let mut gates = Vec::new();
    for _ in 0..3 {
        let (source, signal) = fence();
        state.wait_for_buffer(&surface, source, None, None, Duration::from_millis(5));
        signals.push(signal);
        gates.push(
            state
                .buffer_sync
                .pending
                .get(&(state.buffer_sync.next - 1))
                .unwrap()
                .gate
                .clone(),
        );
    }
    events
        .dispatch(Duration::from_millis(20), &mut state)
        .unwrap();
    state.poll_buffer_sync();
    assert!(state.buffer_sync.pending.is_empty());
    assert!(
        gates
            .iter()
            .all(|gate| gate.state() == BlockerState::Cancelled)
    );
}

#[test]
fn per_surface_admission_is_bounded_and_does_not_affect_another_client() {
    if !crate::startup_tests::private_runtime(
        "buffer_sync::tests::per_surface_admission_is_bounded_and_does_not_affect_another_client",
    ) {
        return;
    }
    let (mut state, mut events, surface, _wire) = fixture();
    let (other_window, _other_wire) =
        crate::state::test_window(&mut state, &mut events, 0xff445566);
    let other = other_window.toplevel().unwrap().wl_surface().clone();
    let (source, mut other_signal) = fence();
    state.wait_for_buffer(&other, source, None, None, MAX_WAIT);
    let other_gate = state.buffer_sync.pending.get(&0).unwrap().gate.clone();
    let mut held = Vec::new();
    for _ in 0..MAX_PER_SURFACE + 1 {
        let (source, signal) = fence();
        state.wait_for_buffer(&surface, source, None, None, MAX_WAIT);
        held.push(signal);
    }
    assert!(state.buffer_sync.pending.len() <= MAX_PER_SURFACE + 1);
    for _ in 0..3 {
        dispatch(&mut state, &mut events);
    }
    other_signal.write_all(&[1]).unwrap();
    for _ in 0..3 {
        dispatch(&mut state, &mut events);
    }
    assert_eq!(other_gate.state(), BlockerState::Released);
    assert!(state.buffer_sync.pending.is_empty());
}

#[test]
fn future_not_before_time_extends_watchdog_without_duration_overflow() {
    let now = Duration::from_secs(100);
    assert_eq!(wait_timeout(None, now), MAX_WAIT);
    assert_eq!(wait_timeout(Some(Duration::from_secs(90)), now), MAX_WAIT);
    assert_eq!(
        wait_timeout(Some(Duration::from_secs(160)), now),
        Duration::from_secs(65)
    );
    assert_eq!(
        wait_timeout(Some(Duration::MAX), Duration::ZERO),
        Duration::MAX
    );
}

#[test]
fn an_early_timer_checkpoint_rearms_without_releasing_or_disconnecting_the_owner() {
    if !crate::startup_tests::private_runtime(
        "buffer_sync::tests::an_early_timer_checkpoint_rearms_without_releasing_or_disconnecting_the_owner",
    ) {
        return;
    }
    let (mut state, mut events, surface, _wire) = fixture();
    let (source, mut signal) = fence();
    state.wait_for_buffer(&surface, source, None, None, Duration::from_millis(1));
    let entry = state.buffer_sync.pending.values_mut().next().unwrap();
    entry.expires = monotonic_now() + Duration::from_secs(60);
    let gate = entry.gate.clone();
    events
        .dispatch(Duration::from_millis(10), &mut state)
        .unwrap();
    state.poll_buffer_sync();
    assert_eq!(gate.state(), BlockerState::Pending);
    assert!(surface.is_alive());
    assert_eq!(state.buffer_sync.pending.len(), 1);
    signal.write_all(&[1]).unwrap();
    for _ in 0..3 {
        dispatch(&mut state, &mut events);
    }
    assert_eq!(gate.state(), BlockerState::Released);
    assert!(state.buffer_sync.pending.is_empty());
}

#[test]
fn ordinary_commit_flood_behind_one_acquire_is_bounded() {
    if !crate::startup_tests::private_runtime(
        "buffer_sync::tests::ordinary_commit_flood_behind_one_acquire_is_bounded",
    ) {
        return;
    }
    let (mut state, mut events, surface, mut wire) = fixture();
    let (source, _signal) = fence();
    state.wait_for_buffer(&surface, source, None, None, MAX_WAIT);
    for _ in 0..MAX_PER_SURFACE + 2 {
        crate::state::test_client_request(&mut wire, 2, 6, &[], None);
    }
    for _ in 0..3 {
        dispatch(&mut state, &mut events);
    }
    assert!(state.buffer_sync.pending.is_empty());
    assert!(!surface.is_alive());
}

#[test]
fn synchronized_child_acquire_also_bounds_its_parent_commit_tail() {
    if !crate::startup_tests::private_runtime(
        "buffer_sync::tests::synchronized_child_acquire_also_bounds_its_parent_commit_tail",
    ) {
        return;
    }
    use smithay::reexports::wayland_server::protocol::wl_compositor::WlCompositor;
    use smithay::reexports::wayland_server::protocol::wl_subcompositor::WlSubcompositor;
    use smithay::wayland::GlobalData;
    let (mut state, mut events, surface, mut wire) = fixture();
    let client = surface.client().unwrap();
    let compositor = client
        .create_resource::<WlCompositor, GlobalData, Ferese>(&state.display_handle, 6, GlobalData)
        .unwrap();
    let subs = client
        .create_resource::<WlSubcompositor, GlobalData, Ferese>(
            &state.display_handle,
            1,
            GlobalData,
        )
        .unwrap();
    crate::state::test_client_request(&mut wire, compositor.id().protocol_id(), 0, &[7], None);
    crate::state::test_client_request(&mut wire, subs.id().protocol_id(), 1, &[8, 7, 2], None);
    dispatch(&mut state, &mut events);
    let child = client
        .object_from_protocol_id::<WlSurface>(&state.display_handle, 7)
        .unwrap();
    let (source, _signal) = fence();
    state.wait_for_buffer(&child, source, None, None, MAX_WAIT);
    crate::state::test_client_request(&mut wire, 7, 6, &[], None);
    for _ in 0..MAX_PER_SURFACE + 2 {
        crate::state::test_client_request(&mut wire, 2, 6, &[], None);
    }
    for _ in 0..3 {
        dispatch(&mut state, &mut events);
    }
    assert!(state.buffer_sync.pending.is_empty());
    assert!(!surface.is_alive());
    assert!(!child.is_alive());
}

#[test]
fn readiness_delivered_before_the_timer_cannot_extend_an_expired_deadline() {
    if !crate::startup_tests::private_runtime(
        "buffer_sync::tests::readiness_delivered_before_the_timer_cannot_extend_an_expired_deadline",
    ) {
        return;
    }
    let (mut state, mut events, surface, _wire) = fixture();
    let (source, mut signal) = fence();
    state.wait_for_buffer(&surface, source, None, None, MAX_WAIT);
    let entry = state.buffer_sync.pending.values_mut().next().unwrap();
    entry.expires = monotonic_now().saturating_sub(Duration::from_millis(1));
    let gate = entry.gate.clone();
    signal.write_all(&[1]).unwrap();
    for _ in 0..3 {
        dispatch(&mut state, &mut events);
    }
    assert_eq!(gate.state(), BlockerState::Cancelled);
    assert!(state.buffer_sync.pending.is_empty());
}
