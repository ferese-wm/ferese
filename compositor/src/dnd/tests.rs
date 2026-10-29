use crate::Ferese;
use crate::state::{
    dispatch_test_client as dispatch, test_client_request as request, test_window as window,
};
use ferese_core::WorkspaceLayout;
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

fn start_drag(
    events: &mut EventLoop<'static, Ferese>,
    state: &mut Ferese,
    source: &smithay::desktop::Window,
    wire: &mut UnixStream,
    use_touch: bool,
    with_icon: bool,
) -> Option<WlSurface> {
    use smithay::reexports::wayland_server::protocol::wl_compositor::WlCompositor;
    data_device(events, state, source, wire, "text/plain", true);
    let root = source.toplevel().unwrap().wl_surface().clone();
    let icon = with_icon.then(|| {
        let client = root.client().unwrap();
        let compositor = client
            .create_resource::<WlCompositor, smithay::wayland::GlobalData, Ferese>(
                &state.display_handle,
                6,
                smithay::wayland::GlobalData,
            )
            .unwrap();
        request(wire, compositor.id().protocol_id(), 0, &[11], None);
        dispatch(events, state);
        client
            .object_from_protocol_id::<WlSurface>(&state.display_handle, 11)
            .unwrap()
    });
    let serial = SERIAL_COUNTER.next_serial();
    let location = (300.25, 200.75).into();
    if use_touch {
        state.seat.get_touch().unwrap().down(
            state,
            Some((root, (0., 0.).into())),
            &smithay::input::touch::DownEvent {
                slot: Some(0).into(),
                location,
                serial,
                time: InputTime::from_millis(1),
            },
        );
    } else {
        let pointer = state.seat.get_pointer().unwrap();
        pointer.motion(
            state,
            Some((root, (0., 0.).into())),
            &MotionEvent {
                location,
                serial,
                time: InputTime::from_millis(1),
            },
        );
        pointer.button(
            state,
            &ButtonEvent {
                button: 0x110,
                state: ButtonState::Pressed,
                serial,
                time: InputTime::from_millis(1),
            },
        );
    }
    request(
        wire,
        10,
        0,
        &[9, 2, if with_icon { 11 } else { 0 }, serial.into()],
        None,
    );
    dispatch(events, state);
    assert!(state.drag.is_some());
    if with_icon {
        // The window fixture's buffer is also a valid client-owned icon buffer.
        request(wire, 11, 1, &[6, 0, 0], None);
        request(wire, 11, 10, &[(-7i32) as u32, (-9i32) as u32], None);
        request(wire, 11, 6, &[], None);
        dispatch(events, state);
    }
    state.poll_drag();
    icon
}

#[test]
fn drag_icons_follow_the_initiating_contact_and_retire_cleanly() {
    use smithay::input::pointer::CursorImageStatus;
    for use_touch in [false, true] {
        let Some((mut events, mut state)) =
            fixture("drag_icons_follow_the_initiating_contact_and_retire_cleanly")
        else {
            return;
        };
        let (source, mut wire) = window(&mut state, &mut events, 0xff112233);
        let icon =
            start_drag(&mut events, &mut state, &source, &mut wire, use_touch, true).unwrap();
        state.cursor_status = CursorImageStatus::Hidden;
        assert_eq!(state.drag_icon().unwrap(), (&icon, (293.25, 191.75).into()));
        assert_eq!(state.drag_icon_bounds().unwrap().size, (64., 48.).into());
        let location = (450.5, 330.25).into();
        if use_touch {
            let touch = state.seat.get_touch().unwrap();
            // A second contact must not move the icon.
            touch.motion(
                &mut state,
                None,
                &smithay::input::touch::MotionEvent {
                    slot: Some(1).into(),
                    location: (10., 10.).into(),
                    time: InputTime::from_millis(2),
                },
            );
            state.poll_drag();
            assert_eq!(state.drag_icon().unwrap().1, (293.25, 191.75).into());
            touch.motion(
                &mut state,
                None,
                &smithay::input::touch::MotionEvent {
                    slot: Some(0).into(),
                    location,
                    time: InputTime::from_millis(3),
                },
            );
        } else {
            state.seat.get_pointer().unwrap().motion(
                &mut state,
                None,
                &MotionEvent {
                    location,
                    serial: SERIAL_COUNTER.next_serial(),
                    time: InputTime::from_millis(2),
                },
            );
        }
        state.poll_drag();
        assert_eq!(state.drag_icon().unwrap().1, (443.5, 321.25).into());
        request(&mut wire, 11, 10, &[3, 4], None);
        request(&mut wire, 11, 6, &[], None);
        dispatch(&mut events, &mut state);
        state.poll_drag();
        assert_eq!(state.drag_icon().unwrap().1, (446.5, 325.25).into());
        let drag = state.drag.as_ref().unwrap();
        assert!(!drag.outputs.is_empty());
        assert!(
            drag.scroll_timer.is_none(),
            "no edge timer in the output interior"
        );
        assert!(state.cancel_drag());
        assert!(state.drag_icon().is_none());
        assert!(state.cursor_redraw_pending);
    }
}

#[test]
fn drag_icon_privacy_survives_its_protected_origin() {
    use smithay::wayland::compositor::with_states;
    use smithay::wayland::shell::xdg::XdgToplevelSurfaceData;
    let Some((mut events, mut state)) = fixture("drag_icon_privacy_survives_its_protected_origin")
    else {
        return;
    };
    let (source, mut wire) = window(&mut state, &mut events, 0xff112233);
    let root = source.toplevel().unwrap().wl_surface().clone();
    with_states(&root, |states| {
        states
            .data_map
            .get::<XdgToplevelSurfaceData>()
            .unwrap()
            .lock()
            .unwrap()
            .app_id = Some("dev.ferese.Authentication".into());
    });
    start_drag(&mut events, &mut state, &source, &mut wire, false, true);
    assert!(state.capture_protected(&source));
    assert!(state.drag_capture_protected());
    assert!(state.capture_cursor_protected());
    assert!(state.has_capture_exclusions());
    // Protection follows the icon even if the source toplevel is removed.
    state.remove_tiled_window(&source);
    assert!(state.drag_capture_protected());
    state.cancel_drag();
    assert!(!state.drag_capture_protected());
}

#[test]
#[ignore = "requires an offscreen EGL rendering device"]
fn drag_icon_rendering_and_callbacks_follow_fractional_outputs() {
    use smithay::backend::allocator::Fourcc;
    use smithay::backend::egl::{EGLContext, EGLDevice, EGLDisplay};
    use smithay::backend::renderer::damage::OutputDamageTracker;
    use smithay::backend::renderer::element::{Element, Id};
    use smithay::backend::renderer::gles::{GlesRenderer, GlesTexture};
    use smithay::backend::renderer::{Bind, Offscreen};
    use smithay::input::pointer::CursorImageStatus;
    use smithay::output::Scale;
    use std::time::Duration;
    for use_touch in [false, true] {
        let Some((mut events, mut state)) =
            fixture("drag_icon_rendering_and_callbacks_follow_fractional_outputs")
        else {
            return;
        };
        let (source, mut wire) = window(&mut state, &mut events, 0xff112233);
        let icon =
            start_drag(&mut events, &mut state, &source, &mut wire, use_touch, true).unwrap();
        state.cursor_status = CursorImageStatus::Hidden;
        let output = state.space.outputs().next().unwrap().clone();
        let device = EGLDevice::enumerate().unwrap().last().expect("EGL device");
        // SAFETY: The enumerated native display is retained by the EGL objects.
        let display = unsafe { EGLDisplay::new(device).unwrap() };
        let context = EGLContext::new(&display).unwrap();
        // SAFETY: This test exclusively owns and uses the fresh EGL context.
        let mut renderer = unsafe { GlesRenderer::new(context).unwrap() };
        let mut texture = Offscreen::<GlesTexture>::create_buffer(
            &mut renderer,
            Fourcc::Abgr8888,
            (800, 600).into(),
        )
        .unwrap();
        for scale in [1.0, 1.25, 1.5] {
            output.change_current_state(None, None, Some(Scale::Fractional(scale)), None);
            state.poll_drag();
            let scene = state.sample_frame(&output, Duration::ZERO);
            let elements = crate::render::sampled_output_elements(
                &mut state,
                &mut renderer,
                &output,
                true,
                &scene,
            );
            let element = elements
                .iter()
                .find(|element| element.id() == &Id::from(&icon))
                .expect("drag icon rendered even with a hidden pointer");
            assert_eq!(
                element.geometry(scale.into()).loc,
                smithay::utils::Point::<f64, smithay::utils::Physical>::from((
                    293.25 * scale,
                    191.75 * scale
                ))
                .to_i32_round()
            );
            let mut target = renderer.bind(&mut texture).unwrap();
            let mut tracker = OutputDamageTracker::from_output(&output);
            let rendered = tracker
                .render_output(&mut renderer, &mut target, 0, &elements, [0., 0., 0., 1.])
                .unwrap();
            state.display_presentation.queued(
                &output,
                &rendered.states,
                scene.input_windows(&output),
            );
            state.display_presentation.presented(&output);
            assert_eq!(
                state.callback_outputs().get(&Id::from(&icon)),
                Some(&output)
            );
            request(&mut wire, 11, 3, &[12], None);
            request(&mut wire, 11, 6, &[], None);
            dispatch(&mut events, &mut state);
            state.send_cursor_frame(&output);
            state.display_handle.flush_clients().unwrap();
            wire.set_nonblocking(true).unwrap();
            let mut replies = Vec::new();
            let _ = wire.read_to_end(&mut replies);
            wire.set_nonblocking(false).unwrap();
            let mut offset = 0;
            let mut done = false;
            while offset < replies.len() {
                let word =
                    |i| u32::from_ne_bytes(replies[offset + i..offset + i + 4].try_into().unwrap());
                let header = word(4);
                done |= word(0) == 12 && header & 0xffff == 0;
                offset += (header >> 16) as usize;
            }
            assert!(done, "icon callbacks continue at scale {scale}");
        }
        use smithay::wayland::compositor::with_states;
        use smithay::wayland::shell::xdg::XdgToplevelSurfaceData;
        with_states(source.toplevel().unwrap().wl_surface(), |states| {
            states
                .data_map
                .get::<XdgToplevelSurfaceData>()
                .unwrap()
                .lock()
                .unwrap()
                .app_id = Some("dev.ferese.Authentication".into());
        });
        state.refresh_capture_privacy();
        let scene = state.sample_frame(&output, Duration::ZERO);
        let captured = crate::render::capture_output_elements(
            &mut state,
            &mut renderer,
            &output,
            true,
            &scene,
        );
        assert!(
            !captured
                .iter()
                .any(|element| element.id() == &Id::from(&icon)),
            "a protected drag icon is removed before capture rendering"
        );
        let displayed = crate::render::sampled_output_elements(
            &mut state,
            &mut renderer,
            &output,
            true,
            &scene,
        );
        assert!(
            displayed
                .iter()
                .any(|element| element.id() == &Id::from(&icon)),
            "capture policy must not hide the live drag"
        );
        state.cancel_drag();
    }
}

#[test]
fn edge_scrolling_stops_at_limits_and_never_changes_focus() {
    use std::time::Duration;
    let Some((mut events, mut state)) =
        fixture("edge_scrolling_stops_at_limits_and_never_changes_focus")
    else {
        return;
    };
    let (source, mut wire) = window(&mut state, &mut events, 0xff112233);
    let (_b, _b_wire) = window(&mut state, &mut events, 0xff334455);
    let (_c, _c_wire) = window(&mut state, &mut events, 0xff556677);
    state.activate_managed_window(state.windows.ids()[&source]);
    start_drag(&mut events, &mut state, &source, &mut wire, false, false);
    let focused = state.focused_window;
    let output = state.space.outputs().next().unwrap().clone();
    let bounds = state.output_bounds_for(&output).unwrap();
    let pointer = state.seat.get_pointer().unwrap();
    pointer.motion(
        &mut state,
        None,
        &MotionEvent {
            location: (bounds.x + bounds.width - 1., bounds.y + bounds.height / 2.).into(),
            serial: SERIAL_COUNTER.next_serial(),
            time: InputTime::from_millis(2),
        },
    );
    state.poll_drag();
    let (workspace, direction) = state.drag_scroll_target().expect("right edge can pan");
    assert_eq!(direction, 1.);
    assert!(state.drag.as_ref().unwrap().scroll_timer.is_some());
    let viewport = |state: &Ferese| {
        let WorkspaceLayout::Scrolling(layout) =
            &state.workspaces.workspace(workspace).unwrap().layout
        else {
            panic!("scrolling layout");
        };
        layout.viewport_x()
    };
    let before = viewport(&state);
    let last = state.drag.as_ref().unwrap().last_scroll;
    state.scroll_drag(last + Duration::from_millis(16));
    assert!(viewport(&state) > before);
    assert_eq!(state.focused_window, focused);
    for _ in 0..300 {
        let last = state.drag.as_ref().unwrap().last_scroll;
        state.scroll_drag(last + Duration::from_millis(50));
    }
    assert!(
        state.drag_scroll_target().is_none(),
        "stop at the last column"
    );
    state.poll_drag();
    assert!(state.drag.as_ref().unwrap().scroll_timer.is_none());
    pointer.motion(
        &mut state,
        None,
        &MotionEvent {
            location: (bounds.x + bounds.width / 2., bounds.y + bounds.height / 2.).into(),
            serial: SERIAL_COUNTER.next_serial(),
            time: InputTime::from_millis(3),
        },
    );
    state.poll_drag();
    assert!(state.drag_scroll_target().is_none());
    state.cancel_drag();
    assert_eq!(state.focused_window, focused);
}

#[test]
fn negotiated_drop_focuses_its_recipient_after_the_input_callback() {
    let Some((mut events, mut state)) =
        fixture("negotiated_drop_focuses_its_recipient_after_the_input_callback")
    else {
        return;
    };
    let (source, mut wire) = window(&mut state, &mut events, 0xff112233);
    let (recipient, mut recipient_wire) = window(&mut state, &mut events, 0xff334455);
    data_device(
        &mut events,
        &mut state,
        &recipient,
        &mut recipient_wire,
        "text/plain",
        true,
    );
    let source_id = state.windows.ids()[&source];
    let recipient_id = state.windows.ids()[&recipient];
    state.activate_managed_window(source_id);
    start_drag(&mut events, &mut state, &source, &mut wire, false, false);
    let target = recipient.toplevel().unwrap().wl_surface().clone();
    let pointer = state.seat.get_pointer().unwrap();
    pointer.motion(
        &mut state,
        Some((target, (0., 0.).into())),
        &MotionEvent {
            location: (500., 200.).into(),
            serial: SERIAL_COUNTER.next_serial(),
            time: InputTime::from_millis(2),
        },
    );
    dispatch(&mut events, &mut state);
    let offer = wire_events(&mut recipient_wire)
        .into_iter()
        .find(|(object, opcode, _)| *object == 10 && *opcode == 0)
        .expect("recipient's data offer")
        .2[0];
    let mut accept = vec![0, 11];
    accept.extend(
        b"text/plain\0\0"
            .chunks_exact(4)
            .map(|word| u32::from_ne_bytes(word.try_into().unwrap())),
    );
    request(&mut recipient_wire, offer, 0, &accept, None);
    request(&mut recipient_wire, offer, 4, &[1, 1], None);
    dispatch(&mut events, &mut state);
    pointer.button(
        &mut state,
        &ButtonEvent {
            button: 0x110,
            state: ButtonState::Released,
            serial: SERIAL_COUNTER.next_serial(),
            time: InputTime::from_millis(3),
        },
    );
    assert_eq!(
        state.focused_window,
        Some(source_id),
        "focus waits until seat locks are released"
    );
    dispatch(&mut events, &mut state);
    assert_eq!(state.focused_window, Some(recipient_id));
    assert!(
        wire_events(&mut wire)
            .iter()
            .any(|(object, opcode, _)| *object == 9 && *opcode == 3),
        "source confirms a completed negotiated drop"
    );
    assert!(state.drag.is_none());
}

fn wire_events(wire: &mut UnixStream) -> Vec<(u32, u32, Vec<u32>)> {
    wire.set_nonblocking(true).unwrap();
    let mut bytes = Vec::new();
    let _ = wire.read_to_end(&mut bytes);
    wire.set_nonblocking(false).unwrap();
    let mut events = Vec::new();
    let mut offset = 0;
    while offset < bytes.len() {
        let object = u32::from_ne_bytes(bytes[offset..offset + 4].try_into().unwrap());
        let header = u32::from_ne_bytes(bytes[offset + 4..offset + 8].try_into().unwrap());
        let size = (header >> 16) as usize;
        let args = bytes[offset + 8..offset + size]
            .chunks_exact(4)
            .map(|word| u32::from_ne_bytes(word.try_into().unwrap()))
            .collect();
        events.push((object, header & 0xffff, args));
        offset += size;
    }
    events
}

#[test]
fn null_source_drag_still_requires_the_exact_origin_and_retires_with_it() {
    use smithay::reexports::wayland_server::protocol::wl_compositor::WlCompositor;
    let Some((mut events, mut state)) =
        fixture("null_source_drag_still_requires_the_exact_origin_and_retires_with_it")
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
    let client = root.client().unwrap();
    let compositor = client
        .create_resource::<WlCompositor, smithay::wayland::GlobalData, Ferese>(
            &state.display_handle,
            6,
            smithay::wayland::GlobalData,
        )
        .unwrap();
    request(&mut wire, compositor.id().protocol_id(), 0, &[11], None);
    dispatch(&mut events, &mut state);
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
    request(&mut wire, 10, 0, &[0, 11, 0, serial.into()], None);
    dispatch(&mut events, &mut state);
    assert!(
        state.drag.is_none(),
        "another surface of the same client does not own the interaction"
    );
    request(&mut wire, 10, 0, &[0, 2, 0, serial.into()], None);
    dispatch(&mut events, &mut state);
    assert!(
        state.drag.is_some(),
        "the original interaction remains usable after rejection"
    );
    state.poll_drag();
    assert!(
        state.drag.is_some(),
        "a live null-source drag must not be mistaken for a missing source"
    );
    request(&mut wire, 4, 0, &[], None);
    request(&mut wire, 3, 0, &[], None);
    request(&mut wire, 2, 0, &[], None);
    dispatch(&mut events, &mut state);
    state.poll_drag();
    assert!(state.drag.is_none());
    assert!(!pointer.is_grabbed());
}

#[test]
fn dropping_on_empty_space_selects_the_destination_output() {
    let Some((mut events, mut state)) =
        fixture("dropping_on_empty_space_selects_the_destination_output")
    else {
        return;
    };
    let (source, mut wire) = window(&mut state, &mut events, 0xff112233);
    let output = Output::new(
        "drop-output".into(),
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
    state.space.map_output(&output, (800, 0));
    state.register_output(&output, "drop-output".into());
    state.activate_managed_window(state.windows.ids()[&source]);
    start_drag(&mut events, &mut state, &source, &mut wire, false, false);
    let pointer = state.seat.get_pointer().unwrap();
    pointer.motion(
        &mut state,
        None,
        &MotionEvent {
            location: (1100., 200.).into(),
            serial: SERIAL_COUNTER.next_serial(),
            time: InputTime::from_millis(2),
        },
    );
    pointer.button(
        &mut state,
        &ButtonEvent {
            button: 0x110,
            state: ButtonState::Released,
            serial: SERIAL_COUNTER.next_serial(),
            time: InputTime::from_millis(3),
        },
    );
    dispatch(&mut events, &mut state);
    assert_eq!(
        state.output_workspaces.focused_output(),
        state.output_id(&output)
    );
    assert!(state.drag.is_none());
}
