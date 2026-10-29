use crate::Ferese;
use crate::state::{
    dispatch_test_client as dispatch, test_client_request as request, test_window as window,
};
use smithay::backend::input::{ButtonState, InputTime};
use smithay::input::dnd::DnDGrab;
use smithay::input::pointer::{ButtonEvent, MotionEvent};
use smithay::output::{Mode, Output, PhysicalProperties, Subpixel};
use smithay::reexports::calloop::EventLoop;
use smithay::reexports::wayland_server::protocol::wl_data_device_manager::WlDataDeviceManager;
use smithay::reexports::wayland_server::protocol::wl_data_source::WlDataSource;
use smithay::reexports::wayland_server::protocol::wl_seat::WlSeat;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::{Client, Resource};
use smithay::utils::SERIAL_COUNTER;
use std::io::Read;
use std::os::unix::net::UnixStream;

fn data_device(
    events: &mut EventLoop<'static, Ferese>,
    state: &mut Ferese,
    window: &smithay::desktop::Window,
    wire: &mut UnixStream,
    mime: &str,
    drag: bool,
) {
    let client = window.toplevel().unwrap().wl_surface().client().unwrap();
    let seat = bind_seat(events, state, &client, wire);
    let manager = client
        .create_resource::<WlDataDeviceManager, smithay::wayland::GlobalData, Ferese>(
            &state.display_handle,
            3,
            smithay::wayland::GlobalData,
        )
        .unwrap();
    request(wire, manager.id().protocol_id(), 0, &[9], None);
    let mut bytes = mime.as_bytes().to_vec();
    bytes.push(0);
    let mut args = vec![bytes.len() as u32];
    bytes.resize(bytes.len().next_multiple_of(4), 0);
    args.extend(
        bytes
            .chunks_exact(4)
            .map(|w| u32::from_ne_bytes(w.try_into().unwrap())),
    );
    request(wire, 9, 0, &args, None);
    if drag {
        request(wire, 9, 2, &[1], None);
    }
    request(
        wire,
        manager.id().protocol_id(),
        1,
        &[10, seat.id().protocol_id()],
        None,
    );
    dispatch(events, state);
}

#[test]
fn matching_serial_cannot_transfer_another_clients_drag() {
    for use_touch in [false, true] {
        let Some((mut events, mut state)) =
            fixture("matching_serial_cannot_transfer_another_clients_drag")
        else {
            return;
        };
        let (attacker, mut wire) = window(&mut state, &mut events, 0xff112233);
        data_device(
            &mut events,
            &mut state,
            &attacker,
            &mut wire,
            "text/plain",
            true,
        );
        let (victim, _victim_wire) = window(&mut state, &mut events, 0xff334455);
        let focus = victim.toplevel().unwrap().wl_surface().clone();
        assert_ne!(
            focus.client().unwrap().id(),
            attacker
                .toplevel()
                .unwrap()
                .wl_surface()
                .client()
                .unwrap()
                .id()
        );
        let serial = SERIAL_COUNTER.next_serial();
        if use_touch {
            state.seat.get_touch().unwrap().down(
                &mut state,
                Some((focus, (0., 0.).into())),
                &smithay::input::touch::DownEvent {
                    slot: Some(0).into(),
                    location: (20., 20.).into(),
                    serial,
                    time: InputTime::from_millis(1),
                },
            );
        } else {
            let pointer = state.seat.get_pointer().unwrap();
            pointer.motion(
                &mut state,
                Some((focus, (0., 0.).into())),
                &MotionEvent {
                    location: (20., 20.).into(),
                    serial,
                    time: InputTime::from_millis(1),
                },
            );
            pointer.button(
                &mut state,
                &ButtonEvent {
                    button: 0x110,
                    state: ButtonState::Pressed,
                    serial,
                    time: InputTime::from_millis(1),
                },
            );
        }
        // This serial belongs to the victim's interaction, never the sender.
        request(&mut wire, 10, 0, &[9, 2, 0, serial.into()], None);
        dispatch(&mut events, &mut state);
        let replaced = if use_touch {
            state
                .seat
                .get_touch()
                .unwrap()
                .with_grab(|_, grab| grab.is::<DnDGrab<Ferese, WlDataSource, WlSurface>>())
                .unwrap_or(false)
        } else {
            state
                .seat
                .get_pointer()
                .unwrap()
                .with_grab(|_, grab| grab.is::<DnDGrab<Ferese, WlDataSource, WlSurface>>())
                .unwrap_or(false)
        };
        assert!(
            !replaced,
            "a matching serial must not authorize another client's grab"
        );
        assert!(state.drag.is_none());
    }
}

#[test]
fn escape_and_source_destruction_cancel_the_drag() {
    for cause in ["escape", "source"] {
        let Some((mut events, mut state)) =
            fixture("escape_and_source_destruction_cancel_the_drag")
        else {
            return;
        };
        let (source, mut wire) = window(&mut state, &mut events, 0xff112233);
        data_device(
            &mut events,
            &mut state,
            &source,
            &mut wire,
            "text/plain",
            true,
        );
        let root = source.toplevel().unwrap().wl_surface().clone();
        let pointer = state.seat.get_pointer().unwrap();
        let serial = SERIAL_COUNTER.next_serial();
        pointer.motion(
            &mut state,
            Some((root, (0., 0.).into())),
            &MotionEvent {
                location: (20., 20.).into(),
                serial,
                time: InputTime::from_millis(1),
            },
        );
        pointer.button(
            &mut state,
            &ButtonEvent {
                button: 0x110,
                state: ButtonState::Pressed,
                serial,
                time: InputTime::from_millis(1),
            },
        );
        request(&mut wire, 10, 0, &[9, 2, 0, serial.into()], None);
        dispatch(&mut events, &mut state);
        assert!(state.drag.is_some());
        assert!(
            pointer
                .with_grab(|_, grab| grab.is::<DnDGrab<Ferese, WlDataSource, WlSurface>>())
                .unwrap()
        );
        if cause == "escape" {
            crate::input::binding_mode_tests::key(&mut state, 9, true);
            crate::input::binding_mode_tests::key(&mut state, 9, false);
        } else {
            request(&mut wire, 9, 1, &[], None);
            dispatch(&mut events, &mut state);
            state.poll_drag();
        }
        assert!(
            !pointer.is_grabbed(),
            "{cause} must cancel the grab before another input event"
        );
        assert!(state.drag.is_none());
        assert!(!state.cancel_drag(), "repeated cancellation is harmless");
    }
}

#[test]
fn locking_cancels_drag_and_blocks_desktop_selection_writes() {
    use smithay::reexports::wayland_protocols::ext::session_lock::v1::server::ext_session_lock_manager_v1::ExtSessionLockManagerV1;
    use smithay::wayland::selection::data_device::request_data_device_client_selection;
    let Some((mut events, mut state)) =
        fixture("locking_cancels_drag_and_blocks_desktop_selection_writes")
    else {
        return;
    };
    let (a, mut a_wire) = window(&mut state, &mut events, 0xff112233);
    data_device(
        &mut events,
        &mut state,
        &a,
        &mut a_wire,
        "application/x-probe-a",
        false,
    );
    let (b, mut b_wire) = window(&mut state, &mut events, 0xff334455);
    data_device(
        &mut events,
        &mut state,
        &b,
        &mut b_wire,
        "application/x-probe-b",
        true,
    );
    state.activate_managed_window(state.windows.ids()[&a]);
    request(&mut a_wire, 10, 1, &[9, 0], None);
    dispatch(&mut events, &mut state);
    let pointer = state.seat.get_pointer().unwrap();
    let serial = SERIAL_COUNTER.next_serial();
    pointer.motion(
        &mut state,
        Some((b.toplevel().unwrap().wl_surface().clone(), (0., 0.).into())),
        &MotionEvent {
            location: (20., 20.).into(),
            serial,
            time: InputTime::from_millis(1),
        },
    );
    pointer.button(
        &mut state,
        &ButtonEvent {
            button: 0x110,
            state: ButtonState::Pressed,
            serial,
            time: InputTime::from_millis(1),
        },
    );
    request(&mut b_wire, 10, 0, &[9, 2, 0, serial.into()], None);
    dispatch(&mut events, &mut state);
    assert!(
        pointer
            .with_grab(|_, grab| grab.is::<DnDGrab<Ferese, WlDataSource, WlSurface>>())
            .unwrap()
    );
    let client = b.toplevel().unwrap().wl_surface().client().unwrap();
    let manager = client
        .create_resource::<ExtSessionLockManagerV1, smithay::wayland::GlobalData, Ferese>(
            &state.display_handle,
            1,
            smithay::wayland::GlobalData,
        )
        .unwrap();
    request(&mut b_wire, manager.id().protocol_id(), 1, &[11], None);
    dispatch(&mut events, &mut state);
    assert!(state.session_lock.active());
    assert!(!pointer.is_grabbed());
    assert_eq!(state.seat.get_keyboard().unwrap().current_focus(), None);
    request(&mut b_wire, 10, 1, &[9, 0], None);
    dispatch(&mut events, &mut state);
    assert!(
        request_data_device_client_selection::<Ferese>(
            &state.seat,
            "application/x-probe-a".into(),
            tempfile::tempfile().unwrap().into()
        )
        .is_ok()
    );
}

#[test]
fn clipboard_write_focus_and_source_disconnect_are_enforced() {
    use smithay::wayland::selection::data_device::{
        SelectionRequestError, request_data_device_client_selection,
    };
    let Some((mut events, mut state)) =
        fixture("clipboard_write_focus_and_source_disconnect_are_enforced")
    else {
        return;
    };
    let (a, mut a_wire) = window(&mut state, &mut events, 0xff112233);
    data_device(
        &mut events,
        &mut state,
        &a,
        &mut a_wire,
        "application/x-probe-a",
        false,
    );
    let (b, mut b_wire) = window(&mut state, &mut events, 0xff334455);
    data_device(
        &mut events,
        &mut state,
        &b,
        &mut b_wire,
        "application/x-probe-b",
        false,
    );
    state.activate_managed_window(state.windows.ids()[&a]);
    request(&mut a_wire, 10, 1, &[9, 0], None);
    dispatch(&mut events, &mut state);
    let query = |state: &Ferese, mime: &str| {
        request_data_device_client_selection::<Ferese>(
            &state.seat,
            mime.into(),
            tempfile::tempfile().unwrap().into(),
        )
    };
    assert!(query(&state, "application/x-probe-a").is_ok());
    request(&mut b_wire, 10, 1, &[9, 0], None);
    dispatch(&mut events, &mut state);
    assert!(query(&state, "application/x-probe-a").is_ok());
    assert!(matches!(
        query(&state, "application/x-probe-b"),
        Err(SelectionRequestError::InvalidMimetype)
    ));
    state.activate_managed_window(state.windows.ids()[&b]);
    request(&mut b_wire, 10, 1, &[9, 0], None);
    dispatch(&mut events, &mut state);
    assert!(query(&state, "application/x-probe-b").is_ok());
    drop(b_wire);
    dispatch(&mut events, &mut state);
    assert!(matches!(
        query(&state, "application/x-probe-b"),
        Err(SelectionRequestError::NoSelection)
    ));
}
fn fixture(test: &str) -> Option<(EventLoop<'static, Ferese>, Ferese)> {
    if !crate::startup_tests::private_runtime(&format!("dnd::tests::{test}")) {
        return None;
    }
    let mut events = EventLoop::try_new().unwrap();
    let mut state = crate::startup_tests::state(&mut events);
    let output = Output::new(
        "probe".into(),
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
    state.register_output(&output, "probe".into());
    Some((events, state))
}

fn bind_seat(
    events: &mut EventLoop<'static, Ferese>,
    state: &mut Ferese,
    client: &Client,
    wire: &mut UnixStream,
) -> WlSeat {
    request(wire, 1, 1, &[7], None);
    dispatch(events, state);
    wire.set_nonblocking(true).unwrap();
    let mut bytes = Vec::new();
    let _ = wire.read_to_end(&mut bytes);
    wire.set_nonblocking(false).unwrap();
    let mut offset = 0;
    let mut seat_name = None;
    while offset < bytes.len() {
        let word = |i| u32::from_ne_bytes(bytes[offset + i..offset + i + 4].try_into().unwrap());
        let header = word(4);
        if word(0) == 7 && header & 0xffff == 0 {
            let length = word(12) as usize;
            if &bytes[offset + 16..offset + 16 + length] == b"wl_seat\0" {
                seat_name = Some(word(8));
            }
        }
        offset += (header >> 16) as usize;
    }
    let mut args = vec![seat_name.unwrap(), 8];
    args.extend(
        b"wl_seat\0"
            .chunks_exact(4)
            .map(|w| u32::from_ne_bytes(w.try_into().unwrap())),
    );
    args.extend([7, 8]);
    request(wire, 7, 0, &args, None);
    dispatch(events, state);
    state.seat.client_seats(client).into_iter().next().unwrap()
}
