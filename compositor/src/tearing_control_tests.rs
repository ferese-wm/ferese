use std::io::Read;
use std::os::unix::net::UnixStream;

use calloop::EventLoop;
use smithay::reexports::wayland_server::Resource;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;

use super::*;
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

fn fixture() -> (
    Ferese,
    EventLoop<'static, Ferese>,
    WlSurface,
    UnixStream,
    WpTearingControlManagerV1,
) {
    let mut events = EventLoop::try_new().unwrap();
    let mut state = crate::startup_tests::state(&mut events);
    let (window, mut wire) = crate::state::test_window(&mut state, &mut events, 0xff112233);
    let surface = window.toplevel().unwrap().wl_surface().clone();
    let manager = surface
        .client()
        .unwrap()
        .create_resource::<WpTearingControlManagerV1, (), Ferese>(&state.display_handle, 1, ())
        .unwrap();
    request(&mut wire, manager.id().protocol_id(), 1, &[7, 2], None);
    dispatch(&mut events, &mut state);
    (state, events, surface, wire, manager)
}

#[test]
fn hints_are_committed_atomically_and_controller_destruction_reverts_on_next_commit() {
    if !crate::startup_tests::private_runtime(
        "tearing_control::tests::hints_are_committed_atomically_and_controller_destruction_reverts_on_next_commit",
    ) {
        return;
    }
    let (mut state, mut events, surface, mut wire, manager) = fixture();
    assert!(!surface_allows_tearing(&surface));
    request(&mut wire, 7, 0, &[1], None);
    dispatch(&mut events, &mut state);
    assert!(!surface_allows_tearing(&surface));
    request(&mut wire, 2, 6, &[], None);
    dispatch(&mut events, &mut state);
    assert!(surface_allows_tearing(&surface));
    request(&mut wire, 7, 1, &[], None);
    dispatch(&mut events, &mut state);
    assert!(surface_allows_tearing(&surface));
    request(&mut wire, manager.id().protocol_id(), 1, &[8, 2], None);
    request(&mut wire, 2, 6, &[], None);
    dispatch(&mut events, &mut state);
    assert!(!surface_allows_tearing(&surface));
    request(&mut wire, 8, 0, &[u32::MAX], None);
    request(&mut wire, 2, 6, &[], None);
    dispatch(&mut events, &mut state);
    assert!(!surface_allows_tearing(&surface));
    request(&mut wire, 2, 0, &[], None);
    request(&mut wire, 8, 0, &[1], None); // An orphaned controller becomes inert.
    request(&mut wire, 1, 0, &[9], None);
    dispatch(&mut events, &mut state);
    assert!(
        messages(&mut wire)
            .iter()
            .any(|(object, opcode, _)| *object == 9 && *opcode == 0)
    );
}

#[test]
fn duplicate_controllers_report_the_standard_protocol_error() {
    if !crate::startup_tests::private_runtime(
        "tearing_control::tests::duplicate_controllers_report_the_standard_protocol_error",
    ) {
        return;
    }
    let (mut state, mut events, _surface, mut wire, manager) = fixture();
    request(&mut wire, manager.id().protocol_id(), 1, &[8, 2], None);
    dispatch(&mut events, &mut state);
    assert!(
        messages(&mut wire)
            .iter()
            .any(|(object, opcode, args)| *object == 1
                && *opcode == 0
                && args.get(..2) == Some(&[manager.id().protocol_id(), 0]))
    );
}

#[test]
fn fifo_updates_keep_synchronized_presentation_even_with_an_async_hint() {
    if !crate::startup_tests::private_runtime(
        "tearing_control::tests::fifo_updates_keep_synchronized_presentation_even_with_an_async_hint",
    ) {
        return;
    }
    let (mut state, mut events, surface, mut wire, _manager) = fixture();
    request(&mut wire, 7, 0, &[1], None);
    with_states(&surface, |states| {
        states
            .cached_state
            .get::<FifoCachedState>()
            .pending()
            .set_barrier = true
    });
    request(&mut wire, 2, 6, &[], None);
    dispatch(&mut events, &mut state);
    assert!(!surface_allows_tearing(&surface));
    request(&mut wire, 2, 6, &[], None);
    dispatch(&mut events, &mut state);
    assert!(surface_allows_tearing(&surface));
}

#[test]
fn nested_or_unavailable_backend_never_advertises_a_tearing_global() {
    if !crate::startup_tests::private_runtime(
        "tearing_control::tests::nested_or_unavailable_backend_never_advertises_a_tearing_global",
    ) {
        return;
    }
    let (mut state, mut events, _surface, mut wire, _manager) = fixture();
    sync(&mut state);
    assert!(state.tearing_control.global.is_none());
    request(&mut wire, 1, 1, &[8], None);
    dispatch(&mut events, &mut state);
    for (object, opcode, args) in messages(&mut wire) {
        if object == 8 && opcode == 0 {
            let name: Vec<_> = args[2..]
                .iter()
                .flat_map(|word| word.to_ne_bytes())
                .collect();
            assert!(!name.starts_with(b"wp_tearing_control_manager_v1\0"));
        }
    }
}

#[test]
fn native_registry_binding_and_global_retirement_preserve_existing_controllers() {
    if !crate::startup_tests::private_runtime(
        "tearing_control::tests::native_registry_binding_and_global_retirement_preserve_existing_controllers",
    ) {
        return;
    }
    let (mut state, mut events, surface, mut wire, _manager) = fixture();
    // Stand in for the DRM capability probe; the real factory/bind/request path
    // then runs over the socket, including retirement when hardware is absent.
    state.tearing_control.global = Some(
        state
            .display_handle
            .create_global::<Ferese, WpTearingControlManagerV1, _>(1, ()),
    );
    request(&mut wire, 7, 1, &[], None);
    request(&mut wire, 1, 1, &[8], None);
    dispatch(&mut events, &mut state);
    let interface = "wp_tearing_control_manager_v1";
    let globals = messages(&mut wire);
    let global = globals
        .iter()
        .find(|(object, opcode, args)| {
            if *object != 8 || *opcode != 0 {
                return false;
            }
            let name: Vec<_> = args[2..]
                .iter()
                .flat_map(|word| word.to_ne_bytes())
                .collect();
            name.starts_with(format!("{interface}\0").as_bytes())
        })
        .unwrap();
    let global_name = global.2[0];
    let mut name = format!("{interface}\0").into_bytes();
    let mut args = vec![global_name, name.len() as u32];
    name.resize(name.len().next_multiple_of(4), 0);
    args.extend(
        name.chunks_exact(4)
            .map(|word| u32::from_ne_bytes(word.try_into().unwrap())),
    );
    args.extend([1, 9]);
    request(&mut wire, 8, 0, &args, None);
    request(&mut wire, 9, 1, &[10, 2], None);
    request(&mut wire, 9, 0, &[], None);
    dispatch(&mut events, &mut state);
    sync(&mut state);
    state.display_handle.flush_clients().unwrap();
    assert!(state.tearing_control.global.is_none());
    assert!(
        messages(&mut wire)
            .iter()
            .any(|(object, opcode, args)| *object == 8
                && *opcode == 1
                && args.first() == Some(&global_name))
    );
    request(&mut wire, 10, 0, &[1], None);
    request(&mut wire, 2, 6, &[], None);
    request(&mut wire, 1, 0, &[11], None);
    dispatch(&mut events, &mut state);
    assert!(surface_allows_tearing(&surface));
    assert!(
        messages(&mut wire)
            .iter()
            .any(|(object, opcode, _)| *object == 11 && *opcode == 0)
    );
}
