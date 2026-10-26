use std::io::Read;
use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

use calloop::EventLoop;
use smithay::backend::renderer::element::{
    Id, RenderElementPresentationState, RenderElementState, RenderElementStates,
};
use smithay::backend::renderer::utils::with_renderer_surface_state;
use smithay::output::{Mode, Output, PhysicalProperties, Subpixel};
use smithay::reexports::wayland_server::Resource;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;

use super::now;
use crate::Ferese;
use crate::state::{dispatch_test_client as dispatch, test_client_request as request};

fn messages(wire: &mut UnixStream) -> Vec<(u32, u16, Vec<u32>)> {
    wire.set_nonblocking(true).unwrap();
    let mut bytes = Vec::new();
    let _ = wire.read_to_end(&mut bytes);
    wire.set_nonblocking(false).unwrap();
    let mut offset = 0;
    let mut result = Vec::new();
    while offset < bytes.len() {
        let object = u32::from_ne_bytes(bytes[offset..offset + 4].try_into().unwrap());
        let header = u32::from_ne_bytes(bytes[offset + 4..offset + 8].try_into().unwrap());
        let size = (header >> 16) as usize;
        let args = bytes[offset + 8..offset + size]
            .chunks_exact(4)
            .map(|word| u32::from_ne_bytes(word.try_into().unwrap()))
            .collect();
        result.push((object, header as u16, args));
        offset += size;
    }
    result
}

fn bind(wire: &mut UnixStream, globals: &[(u32, u16, Vec<u32>)], interface: &str, id: u32) {
    let global = globals
        .iter()
        .find(|(object, opcode, args)| {
            if *object != 7 || *opcode != 0 {
                return false;
            }
            let name: Vec<_> = args[2..]
                .iter()
                .flat_map(|word| word.to_ne_bytes())
                .collect();
            name.get(..interface.len() + 1) == Some(format!("{interface}\0").as_bytes())
        })
        .unwrap_or_else(|| panic!("missing {interface}"));
    let mut name = interface.as_bytes().to_vec();
    name.push(0);
    let mut args = vec![global.2[0], name.len() as u32];
    name.resize(name.len().next_multiple_of(4), 0);
    args.extend(
        name.chunks_exact(4)
            .map(|word| u32::from_ne_bytes(word.try_into().unwrap())),
    );
    args.extend([1, id]);
    request(wire, 7, 0, &args, None);
}

fn fixture() -> (Ferese, EventLoop<'static, Ferese>, WlSurface, UnixStream) {
    let mut events = EventLoop::try_new().unwrap();
    let mut state = crate::startup_tests::state(&mut events);
    let output = Output::new(
        "constraints".into(),
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
    state.register_output(&output, "constraints".into());
    let (window, mut wire) = crate::state::test_window(&mut state, &mut events, 0xff112233);
    let surface = window.toplevel().unwrap().wl_surface().clone();
    request(&mut wire, 1, 1, &[7], None);
    dispatch(&mut events, &mut state);
    let globals = messages(&mut wire);
    bind(&mut wire, &globals, "wp_commit_timing_manager_v1", 8);
    request(&mut wire, 8, 1, &[9, 2], None);
    bind(&mut wire, &globals, "wp_fifo_manager_v1", 10);
    request(&mut wire, 10, 1, &[11, 2], None);
    dispatch(&mut events, &mut state);
    (state, events, surface, wire)
}

fn damage_commit(wire: &mut UnixStream) {
    request(wire, 2, 1, &[6, 0, 0], None);
    request(wire, 2, 2, &[0, 0, 1, 1], None);
    request(wire, 2, 6, &[], None);
}

fn timestamp(wire: &mut UnixStream, deadline: Duration) {
    request(
        wire,
        9,
        0,
        &[
            (deadline.as_secs() >> 32) as u32,
            deadline.as_secs() as u32,
            deadline.subsec_nanos(),
        ],
        None,
    );
}

fn pump(events: &mut EventLoop<'static, Ferese>, state: &mut Ferese, timeout: Duration) {
    let until = Instant::now() + timeout;
    while Instant::now() < until {
        state.poll_presentation_constraints();
        dispatch(events, state);
    }
    state.poll_presentation_constraints();
}

#[test]
fn timed_commits_preserve_order_and_cannot_apply_before_monotonic_deadline() {
    if !crate::startup_tests::private_runtime(
        "presentation_constraints::protocol_tests::timed_commits_preserve_order_and_cannot_apply_before_monotonic_deadline",
    ) {
        return;
    }
    let (mut state, mut events, surface, mut wire) = fixture();
    let before = with_renderer_surface_state(&surface, |state| state.current_commit()).unwrap();
    let deadline = now() + Duration::from_millis(60);
    timestamp(&mut wire, deadline);
    damage_commit(&mut wire);
    damage_commit(&mut wire); // Untimed updates must not overtake a timed update.
    request(&mut wire, 9, 1, &[], None); // Destroying either controller preserves constraints.
    request(&mut wire, 8, 0, &[], None);
    dispatch(&mut events, &mut state);
    state.poll_presentation_constraints();
    assert_eq!(
        with_renderer_surface_state(&surface, |state| state.current_commit()),
        Some(before)
    );
    assert_eq!(state.presentation_constraints.timed.len(), 1);
    pump(&mut events, &mut state, Duration::from_millis(75));
    assert!(now() >= deadline);
    assert_ne!(
        with_renderer_surface_state(&surface, |state| state.current_commit()),
        Some(before)
    );
    assert!(state.presentation_constraints.timed.is_empty());
    assert!(state.presentation_constraints.timer.is_none());
}

#[test]
fn fifo_hidden_surface_progress_releases_one_condition_without_a_polling_loop() {
    if !crate::startup_tests::private_runtime(
        "presentation_constraints::protocol_tests::fifo_hidden_surface_progress_releases_one_condition_without_a_polling_loop",
    ) {
        return;
    }
    let (mut state, mut events, surface, mut wire) = fixture();
    request(&mut wire, 11, 0, &[], None);
    damage_commit(&mut wire);
    dispatch(&mut events, &mut state);
    let before = with_renderer_surface_state(&surface, |state| state.current_commit()).unwrap();
    request(&mut wire, 11, 1, &[], None);
    damage_commit(&mut wire);
    dispatch(&mut events, &mut state);
    assert_eq!(
        with_renderer_surface_state(&surface, |state| state.current_commit()),
        Some(before)
    );
    state.poll_presentation_constraints();
    assert!(state.presentation_constraints.timer.is_some());
    pump(&mut events, &mut state, Duration::from_millis(30));
    assert_ne!(
        with_renderer_surface_state(&surface, |state| state.current_commit()),
        Some(before)
    );
    assert!(state.presentation_constraints.fifo.is_empty());
    assert!(state.presentation_constraints.timer.is_none());
}

#[test]
fn destroying_surface_cancels_far_future_constraints_and_reclaims_timer_and_budget() {
    if !crate::startup_tests::private_runtime(
        "presentation_constraints::protocol_tests::destroying_surface_cancels_far_future_constraints_and_reclaims_timer_and_budget",
    ) {
        return;
    }
    let (mut state, mut events, _surface, mut wire) = fixture();
    timestamp(&mut wire, now() + Duration::from_secs(3_600));
    damage_commit(&mut wire);
    dispatch(&mut events, &mut state);
    state.poll_presentation_constraints();
    assert_eq!(state.presentation_constraints.timed.len(), 1);
    request(&mut wire, 2, 0, &[], None);
    dispatch(&mut events, &mut state);
    state.poll_presentation_constraints();
    assert!(state.presentation_constraints.timed.is_empty());
    assert!(state.presentation_constraints.timer.is_none());
    assert_eq!(
        state
            .presentation_constraints
            .budget
            .total
            .load(std::sync::atomic::Ordering::Relaxed),
        0
    );
}

#[test]
fn invalid_nanoseconds_are_rejected_at_request_time() {
    if !crate::startup_tests::private_runtime(
        "presentation_constraints::protocol_tests::invalid_nanoseconds_are_rejected_at_request_time",
    ) {
        return;
    }
    let (mut state, mut events, _surface, mut wire) = fixture();
    request(&mut wire, 9, 0, &[0, 0, 1_000_000_000], None);
    dispatch(&mut events, &mut state);
    let messages = messages(&mut wire);
    assert!(messages.iter().any(|(object, opcode, args)| *object == 1
        && *opcode == 0
        && args.get(..2) == Some(&[9, 0])));
    assert!(state.presentation_constraints.timed.is_empty());
}

#[test]
fn excess_future_commits_disconnect_only_the_abusive_client() {
    if !crate::startup_tests::private_runtime(
        "presentation_constraints::protocol_tests::excess_future_commits_disconnect_only_the_abusive_client",
    ) {
        return;
    }
    let (mut state, mut events, surface, mut wire) = fixture();
    for _ in 0..=super::MAX_SURFACE_PENDING {
        timestamp(&mut wire, now() + Duration::from_secs(3_600));
        damage_commit(&mut wire);
    }
    dispatch(&mut events, &mut state);
    state.poll_presentation_constraints();
    assert!(!surface.is_alive());
    assert!(state.presentation_constraints.timed.is_empty());
    assert_eq!(
        state
            .presentation_constraints
            .budget
            .total
            .load(std::sync::atomic::Ordering::Relaxed),
        0
    );
    let (_window, mut healthy) = crate::state::test_window(&mut state, &mut events, 0xff334455);
    request(&mut healthy, 1, 0, &[7], None);
    dispatch(&mut events, &mut state);
    assert!(
        messages(&mut healthy)
            .iter()
            .any(|(object, opcode, _)| *object == 7 && *opcode == 0)
    );
}

#[test]
fn untimed_commits_behind_a_timed_commit_also_consume_the_queue_budget() {
    if !crate::startup_tests::private_runtime(
        "presentation_constraints::protocol_tests::untimed_commits_behind_a_timed_commit_also_consume_the_queue_budget",
    ) {
        return;
    }
    let (mut state, mut events, surface, mut wire) = fixture();
    timestamp(&mut wire, now() + Duration::from_secs(3_600));
    for _ in 0..=super::MAX_SURFACE_PENDING {
        damage_commit(&mut wire);
    }
    dispatch(&mut events, &mut state);
    state.poll_presentation_constraints();
    assert!(!surface.is_alive());
    assert!(state.presentation_constraints.timed.is_empty());
    assert_eq!(
        state
            .presentation_constraints
            .budget
            .total
            .load(std::sync::atomic::Ordering::Relaxed),
        0
    );
}

#[test]
fn visible_fifo_waits_for_its_callback_owner_and_output_loss_allows_progress() {
    if !crate::startup_tests::private_runtime(
        "presentation_constraints::protocol_tests::visible_fifo_waits_for_its_callback_owner_and_output_loss_allows_progress",
    ) {
        return;
    }
    let (mut state, mut events, surface, mut wire) = fixture();
    let output = state.space.outputs().next().unwrap().clone();
    let mut scene = RenderElementStates::default();
    scene.states.insert(
        Id::from(&surface),
        RenderElementState {
            needs_capture: false,
            visible_area: 1,
            presentation_state: RenderElementPresentationState::ZeroCopy,
        },
    );
    state.display_presentation.queued(&output, &scene);
    state.display_presentation.presented(&output);
    request(&mut wire, 11, 0, &[], None);
    damage_commit(&mut wire);
    dispatch(&mut events, &mut state);
    let before = with_renderer_surface_state(&surface, |state| state.current_commit()).unwrap();
    request(&mut wire, 11, 1, &[], None);
    damage_commit(&mut wire);
    dispatch(&mut events, &mut state);
    pump(&mut events, &mut state, Duration::from_millis(25));
    assert_eq!(
        with_renderer_surface_state(&surface, |state| state.current_commit()),
        Some(before)
    );
    assert!(
        state.presentation_constraints.timer.is_none(),
        "visible FIFO waits must not poll"
    );
    state.presentation_constraint_frame(&output);
    state.poll_presentation_constraints();
    assert_ne!(
        with_renderer_surface_state(&surface, |state| state.current_commit()),
        Some(before)
    );

    request(&mut wire, 11, 0, &[], None);
    damage_commit(&mut wire);
    dispatch(&mut events, &mut state);
    request(&mut wire, 11, 1, &[], None);
    damage_commit(&mut wire);
    dispatch(&mut events, &mut state);
    pump(&mut events, &mut state, Duration::from_millis(20));
    assert_eq!(state.presentation_constraints.fifo.len(), 1);
    state.display_presentation.remove_output(&output);
    state.space.unmap_output(&output);
    state.poll_presentation_constraints();
    assert!(state.presentation_constraints.fifo.is_empty());
    assert!(state.presentation_constraints.timer.is_none());
}
