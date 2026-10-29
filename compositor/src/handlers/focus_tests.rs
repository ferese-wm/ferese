//! Private Wayland clients exercise admission and first-buffer focus changes.

use std::io::Read;
use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

use smithay::backend::input::{ButtonState, InputTime};
use smithay::desktop::Window;
use smithay::input::pointer::{ButtonEvent, MotionEvent};
use smithay::output::{Mode, Output, PhysicalProperties, Subpixel};
use smithay::reexports::calloop::EventLoop;
use smithay::reexports::wayland_protocols::xdg::activation::v1::server::xdg_activation_v1::XdgActivationV1;
use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_wm_base::XdgWmBase;
use smithay::reexports::wayland_server::protocol::wl_compositor::WlCompositor;
use smithay::reexports::wayland_server::protocol::wl_seat::WlSeat;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::{Client, Resource};
use smithay::utils::{SERIAL_COUNTER, Serial};
use smithay::wayland::shell::xdg::XdgWmBaseUserData;
use smithay::wayland::xdg_activation::{
    XdgActivationHandler, XdgActivationToken, XdgActivationTokenData,
};

use crate::Ferese;
use crate::state::{
    dispatch_test_client as dispatch, test_client_request as request, test_window as window,
};

fn fixture(test: &str) -> Option<(EventLoop<'static, Ferese>, Ferese)> {
    if !crate::startup_tests::private_runtime(&format!("handlers::focus_tests::{test}")) {
        return None;
    }
    let mut events = EventLoop::try_new().unwrap();
    let mut state = crate::startup_tests::state(&mut events);
    let output = Output::new(
        "focus".into(),
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
    state.register_output(&output, "focus".into());
    Some((events, state))
}

fn read_events(wire: &mut UnixStream) -> Vec<u8> {
    wire.set_nonblocking(true).unwrap();
    let mut bytes = Vec::new();
    let _ = wire.read_to_end(&mut bytes);
    wire.set_nonblocking(false).unwrap();
    bytes
}

fn bind_seat(
    events: &mut EventLoop<'static, Ferese>,
    state: &mut Ferese,
    client: &Client,
    wire: &mut UnixStream,
) -> WlSeat {
    request(wire, 1, 1, &[7], None);
    dispatch(events, state);
    let bytes = read_events(wire);
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
    let mut args = vec![seat_name.expect("seat advertised"), 8];
    args.extend(
        b"wl_seat\0"
            .chunks_exact(4)
            .map(|w| u32::from_ne_bytes(w.try_into().unwrap())),
    );
    args.extend([7, 8]);
    request(wire, 7, 0, &args, None);
    dispatch(events, state);
    state
        .seat
        .client_seats(client)
        .into_iter()
        .next()
        .expect("seat bound")
}

fn popup(
    events: &mut EventLoop<'static, Ferese>,
    state: &mut Ferese,
    wire: &mut UnixStream,
    seat: &WlSeat,
    parent: u32,
    first_id: u32,
    serial: Serial,
) -> WlSurface {
    let client = seat.client().unwrap();
    let compositor = client
        .create_resource::<WlCompositor, smithay::wayland::GlobalData, Ferese>(
            &state.display_handle,
            6,
            smithay::wayland::GlobalData,
        )
        .unwrap();
    let shell = client
        .create_resource::<XdgWmBase, XdgWmBaseUserData, Ferese>(
            &state.display_handle,
            6,
            Default::default(),
        )
        .unwrap();
    request(wire, compositor.id().protocol_id(), 0, &[first_id], None);
    request(
        wire,
        shell.id().protocol_id(),
        2,
        &[first_id + 1, first_id],
        None,
    );
    request(wire, shell.id().protocol_id(), 1, &[first_id + 2], None);
    request(wire, first_id + 2, 1, &[140, 80], None);
    request(wire, first_id + 2, 2, &[0, 0, 1, 1], None);
    request(
        wire,
        first_id + 1,
        2,
        &[first_id + 3, parent, first_id + 2],
        None,
    );
    request(
        wire,
        first_id + 3,
        1,
        &[seat.id().protocol_id(), serial.into()],
        None,
    );
    if parent == 0 {
        // The layer fixture's role is object 3; its popup has no XDG parent.
        request(wire, 3, 5, &[first_id + 3], None);
    }
    request(wire, first_id, 6, &[], None);
    dispatch(events, state);
    state
        .xdg_shell_state
        .popup_surfaces()
        .iter()
        .find(|popup| {
            popup.wl_surface().id().protocol_id() == first_id
                && popup.wl_surface().client().unwrap().id() == client.id()
        })
        .expect("popup created")
        .wl_surface()
        .clone()
}

fn token(
    events: &mut EventLoop<'static, Ferese>,
    state: &mut Ferese,
    launcher: &Window,
    wire: &mut UnixStream,
) -> (XdgActivationToken, XdgActivationTokenData) {
    let surface = launcher.toplevel().unwrap().wl_surface().clone();
    let client = surface.client().unwrap();
    let seat = bind_seat(events, state, &client, wire);
    let activation = client
        .create_resource::<XdgActivationV1, smithay::wayland::GlobalData, Ferese>(
            &state.display_handle,
            1,
            smithay::wayland::GlobalData,
        )
        .unwrap();
    let serial = SERIAL_COUNTER.next_serial();
    state.record_activation_input(serial, Some(surface));
    request(wire, activation.id().protocol_id(), 1, &[9], None);
    request(wire, 9, 0, &[serial.into(), seat.id().protocol_id()], None);
    request(wire, 9, 3, &[], None);
    request(wire, 9, 4, &[], None);
    dispatch(events, state);
    state
        .xdg_activation_state
        .tokens()
        .next()
        .map(|(token, data)| (token.clone(), data.clone()))
        .expect("approved token retained after resource destruction")
}

#[test]
fn background_popup_cannot_take_keyboard_focus() {
    let Some((mut events, mut state)) = fixture("background_popup_cannot_take_keyboard_focus")
    else {
        return;
    };
    let (background, mut wire) = window(&mut state, &mut events, 0xff112233);
    let root = background.toplevel().unwrap().wl_surface().clone();
    let serial = SERIAL_COUNTER.next_serial();
    state.record_activation_input(serial, Some(root.clone()));
    let (foreground, _foreground_wire) = window(&mut state, &mut events, 0xff334455);
    let focus = foreground.toplevel().unwrap().wl_surface().clone();
    let seat = bind_seat(&mut events, &mut state, &root.client().unwrap(), &mut wire);
    // Reject even a real earlier interaction once another window owns focus.
    popup(&mut events, &mut state, &mut wire, &seat, 3, 9, serial);
    assert_eq!(
        state.seat.get_keyboard().unwrap().current_focus(),
        Some(focus)
    );
    assert!(!state.seat.get_keyboard().unwrap().is_grabbed());
    assert!(!state.seat.get_pointer().unwrap().is_grabbed());
    let bytes = read_events(&mut wire);
    let mut offset = 0;
    let mut dismissed = false;
    while offset < bytes.len() {
        let object = u32::from_ne_bytes(bytes[offset..offset + 4].try_into().unwrap());
        let header = u32::from_ne_bytes(bytes[offset + 4..offset + 8].try_into().unwrap());
        dismissed |= object == 12 && header & 0xffff == 1;
        offset += (header >> 16) as usize;
    }
    assert!(dismissed, "denied popup must receive popup_done");
}

#[test]
fn keyboard_menus_nest_and_release_focus_when_another_window_activates() {
    for directional in [false, true] {
        let Some((mut events, mut state)) =
            fixture("keyboard_menus_nest_and_release_focus_when_another_window_activates")
        else {
            return;
        };
        let (other, _other_wire) = window(&mut state, &mut events, 0xff112233);
        let (parent, mut wire) = window(&mut state, &mut events, 0xff334455);
        let root = parent.toplevel().unwrap().wl_surface().clone();
        let seat = bind_seat(&mut events, &mut state, &root.client().unwrap(), &mut wire);
        // Keyboard-opened menus may carry an old pointer serial from the toolkit.
        let serial = Serial::from(0x12345678);
        let first = popup(&mut events, &mut state, &mut wire, &seat, 3, 9, serial);
        assert_eq!(
            state.seat.get_keyboard().unwrap().current_focus(),
            Some(first)
        );
        let child = popup(&mut events, &mut state, &mut wire, &seat, 10, 13, serial);
        assert_eq!(
            state.seat.get_keyboard().unwrap().current_focus(),
            Some(child)
        );
        assert!(state.seat.get_keyboard().unwrap().is_grabbed());
        assert!(state.seat.get_pointer().unwrap().is_grabbed());
        if directional {
            state.focus_direction(ferese_layout::Direction::Left);
        } else {
            assert!(state.activate_managed_window(state.windows.ids()[&other]));
        }
        assert_eq!(
            state.seat.get_keyboard().unwrap().current_focus(),
            Some(other.toplevel().unwrap().wl_surface().clone())
        );
        assert!(!state.seat.get_keyboard().unwrap().is_grabbed());
        assert!(!state.seat.get_pointer().unwrap().is_grabbed());
    }
}

#[test]
fn popup_cannot_replace_another_clients_pointer_grab() {
    let Some((mut events, mut state)) =
        fixture("popup_cannot_replace_another_clients_pointer_grab")
    else {
        return;
    };
    let (other, _other_wire) = window(&mut state, &mut events, 0xff112233);
    let (parent, mut wire) = window(&mut state, &mut events, 0xff334455);
    let root = parent.toplevel().unwrap().wl_surface().clone();
    let seat = bind_seat(&mut events, &mut state, &root.client().unwrap(), &mut wire);
    let pointer = state.seat.get_pointer().unwrap();
    let serial = SERIAL_COUNTER.next_serial();
    pointer.motion(
        &mut state,
        Some((
            other.toplevel().unwrap().wl_surface().clone(),
            (0., 0.).into(),
        )),
        &MotionEvent {
            location: (10., 10.).into(),
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
    assert!(pointer.has_grab(serial));
    popup(&mut events, &mut state, &mut wire, &seat, 3, 9, serial);
    assert_eq!(
        state.seat.get_keyboard().unwrap().current_focus(),
        Some(root)
    );
    assert!(!state.seat.get_keyboard().unwrap().is_grabbed());
    assert!(pointer.has_grab(serial));
    assert!(
        pointer
            .with_grab(|_, grab| grab.is::<smithay::input::pointer::ClickGrab<Ferese>>())
            .unwrap()
    );
}

#[test]
fn popup_cannot_replace_another_clients_touch_interaction() {
    let Some((mut events, mut state)) =
        fixture("popup_cannot_replace_another_clients_touch_interaction")
    else {
        return;
    };
    let (other, _other_wire) = window(&mut state, &mut events, 0xff112233);
    let (parent, mut wire) = window(&mut state, &mut events, 0xff334455);
    let root = parent.toplevel().unwrap().wl_surface().clone();
    let seat = bind_seat(&mut events, &mut state, &root.client().unwrap(), &mut wire);
    let touch = state.seat.get_touch().unwrap();
    let serial = SERIAL_COUNTER.next_serial();
    touch.down(
        &mut state,
        Some((
            other.toplevel().unwrap().wl_surface().clone(),
            (0., 0.).into(),
        )),
        &smithay::input::touch::DownEvent {
            slot: Some(0).into(),
            location: (10., 10.).into(),
            serial,
            time: InputTime::from_millis(1),
        },
    );
    assert!(touch.has_grab(serial));
    popup(&mut events, &mut state, &mut wire, &seat, 3, 9, serial);
    assert_eq!(
        state.seat.get_keyboard().unwrap().current_focus(),
        Some(root)
    );
    assert!(!state.seat.get_keyboard().unwrap().is_grabbed());
    assert!(!state.seat.get_pointer().unwrap().is_grabbed());
    assert!(touch.has_grab(serial));
}

#[test]
fn activation_waits_for_mapping_and_preserves_its_original_deadline() {
    let Some((mut events, mut state)) =
        fixture("activation_waits_for_mapping_and_preserves_its_original_deadline")
    else {
        return;
    };
    for expires in [false, true] {
        let (target, mut target_wire) = window(&mut state, &mut events, 0xff112233);
        let target_surface = target.toplevel().unwrap().wl_surface().clone();
        // Unmapping leaves the toplevel alive and resets its initial configure.
        request(&mut target_wire, 2, 1, &[0, 0, 0], None);
        request(&mut target_wire, 2, 6, &[], None);
        dispatch(&mut events, &mut state);
        assert!(state.windows.id_for_surface(&target_surface).is_none());
        let (launcher, mut launcher_wire) = window(&mut state, &mut events, 0xff334455);
        let launcher_id = state.windows.ids()[&launcher];
        state.set_window_fullscreen(launcher_id, true);
        let (token, mut data) = token(&mut events, &mut state, &launcher, &mut launcher_wire);
        if expires {
            data.timestamp = Instant::now() - Duration::from_secs(10) + Duration::from_millis(20);
        }
        state.request_activation(token.clone(), data, target_surface.clone());
        assert!(state.xdg_activation_state.data_for_token(&token).is_none());
        assert_eq!(
            state.seat.get_keyboard().unwrap().current_focus(),
            Some(launcher.toplevel().unwrap().wl_surface().clone())
        );
        if expires {
            std::thread::sleep(Duration::from_millis(30));
        }
        // The fixture retains its original SHM buffer as object 6.
        request(&mut target_wire, 2, 6, &[], None);
        dispatch(&mut events, &mut state);
        ack_configure(&mut target_wire, 3);
        request(&mut target_wire, 2, 1, &[6, 0, 0], None);
        request(&mut target_wire, 2, 6, &[], None);
        dispatch(&mut events, &mut state);
        let expected = if expires {
            launcher.toplevel().unwrap().wl_surface()
        } else {
            &target_surface
        };
        assert_eq!(
            state.seat.get_keyboard().unwrap().current_focus().as_ref(),
            Some(expected)
        );
        if !expires {
            assert_eq!(state.workspaces.active().fullscreen, None);
        }
    }
}

fn ack_configure(wire: &mut UnixStream, xdg: u32) {
    let bytes = read_events(wire);
    let mut offset = 0;
    let mut serial = None;
    while offset < bytes.len() {
        let object = u32::from_ne_bytes(bytes[offset..offset + 4].try_into().unwrap());
        let header = u32::from_ne_bytes(bytes[offset + 4..offset + 8].try_into().unwrap());
        if object == xdg && header & 0xffff == 0 {
            serial = Some(u32::from_ne_bytes(
                bytes[offset + 8..offset + 12].try_into().unwrap(),
            ));
        }
        offset += (header >> 16) as usize;
    }
    request(wire, xdg, 4, &[serial.expect("initial configure")], None);
}

#[test]
fn unused_tokens_expire_without_idle_timer_wakeups() {
    let Some((mut events, mut state)) = fixture("unused_tokens_expire_without_idle_timer_wakeups")
    else {
        return;
    };
    assert!(!state.activation_timer_pending);
    let (launcher, mut wire) = window(&mut state, &mut events, 0xff112233);
    let (token, _) = token(&mut events, &mut state, &launcher, &mut wire);
    assert!(state.activation_timer_pending);
    assert!(state.xdg_activation_state.data_for_token(&token).is_some());
    drop(wire);
    let deadline = Instant::now() + Duration::from_millis(10_100);
    while Instant::now() < deadline {
        events
            .dispatch(Duration::from_millis(50), &mut state)
            .unwrap();
    }
    assert_eq!(state.xdg_activation_state.tokens().count(), 0);
    assert!(!state.activation_timer_pending);
}

#[test]
fn panel_menus_accept_pointer_and_touch_and_release_both_grabs_on_unmap() {
    for touch_input in [false, true] {
        let Some((mut events, mut state)) =
            fixture("panel_menus_accept_pointer_and_touch_and_release_both_grabs_on_unmap")
        else {
            return;
        };
        let (_window, _window_wire) = window(&mut state, &mut events, 0xff112233);
        let output = state.space.outputs().next().unwrap().clone();
        let (layer, mut wire) = crate::input::layer_focus_tests::layer_with_namespace(
            &mut state,
            &mut events,
            &output,
            0,
            2,
            "bar",
        );
        let root = layer.wl_surface().clone();
        let client = root.client().unwrap();
        let compositor = client
            .create_resource::<WlCompositor, smithay::wayland::GlobalData, Ferese>(
                &state.display_handle,
                6,
                smithay::wayland::GlobalData,
            )
            .unwrap();
        // The layer helper uses objects 2..5; reserve 6 before the seat helper.
        request(&mut wire, compositor.id().protocol_id(), 0, &[6], None);
        let seat = bind_seat(&mut events, &mut state, &client, &mut wire);
        let serial = SERIAL_COUNTER.next_serial();
        let pointer = state.seat.get_pointer().unwrap();
        if touch_input {
            assert_eq!(pointer.current_focus(), None);
            state.seat.get_touch().unwrap().down(
                &mut state,
                Some((root.clone(), (0., 0.).into())),
                &smithay::input::touch::DownEvent {
                    slot: Some(0).into(),
                    location: (20., 20.).into(),
                    serial,
                    time: InputTime::from_millis(1),
                },
            );
        } else {
            pointer.motion(
                &mut state,
                Some((root.clone(), (0., 0.).into())),
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
        state.record_activation_input(serial, Some(root));
        let popup = popup(&mut events, &mut state, &mut wire, &seat, 0, 9, serial);
        assert_eq!(
            state.seat.get_keyboard().unwrap().current_focus(),
            Some(popup)
        );
        assert!(pointer.is_grabbed());
        request(&mut wire, 2, 1, &[0, 0, 0], None);
        request(&mut wire, 2, 6, &[], None);
        dispatch(&mut events, &mut state);
        assert!(
            !pointer.is_grabbed(),
            "unmapped panel must release its popup's pointer grab"
        );
        assert!(!state.seat.get_keyboard().unwrap().is_grabbed());
    }
}

#[test]
fn exclusive_layer_blocks_new_window_popup_grabs() {
    let Some((mut events, mut state)) = fixture("exclusive_layer_blocks_new_window_popup_grabs")
    else {
        return;
    };
    let (parent, mut wire) = window(&mut state, &mut events, 0xff112233);
    let root = parent.toplevel().unwrap().wl_surface().clone();
    let seat = bind_seat(&mut events, &mut state, &root.client().unwrap(), &mut wire);
    let output = state.space.outputs().next().unwrap().clone();
    let (overlay, _overlay_wire) = crate::input::layer_focus_tests::layer_with_namespace(
        &mut state,
        &mut events,
        &output,
        1,
        3,
        "overlay",
    );
    assert_eq!(
        state.seat.get_keyboard().unwrap().current_focus(),
        Some(overlay.wl_surface().clone())
    );
    assert!(!state.seat.get_keyboard().unwrap().is_grabbed());
    assert!(!state.seat.get_pointer().unwrap().is_grabbed());
    popup(
        &mut events,
        &mut state,
        &mut wire,
        &seat,
        3,
        9,
        SERIAL_COUNTER.next_serial(),
    );
    assert_eq!(
        state.seat.get_keyboard().unwrap().current_focus(),
        Some(overlay.wl_surface().clone())
    );
    assert!(!state.seat.get_keyboard().unwrap().is_grabbed());
}

#[test]
fn fullscreen_focus_is_preserved_for_regular_floating_apps() {
    let Some((mut events, mut state)) =
        fixture("fullscreen_focus_is_preserved_for_regular_floating_apps")
    else {
        return;
    };
    state.window_rules = crate::config::Config::parse_source(
        "window-rule app-id=org.example.Floating floating=#true\n",
    )
    .unwrap()
    .window_rules()
    .unwrap();
    let (full, _full_wire) = window(&mut state, &mut events, 0xff112233);
    let id = state.windows.ids()[&full];
    state.set_window_fullscreen(id, true);
    let (_tile, _tile_wire) = window(&mut state, &mut events, 0xff334455);
    let (float, _float_wire) = crate::state::test_window_with_app_id(
        &mut state,
        &mut events,
        0xff556677,
        "org.example.Floating",
    );
    let floating = state.windows.ids()[&float];
    assert_eq!(state.workspaces.active().fullscreen, Some(id));
    assert_eq!(
        state.seat.get_keyboard().unwrap().current_focus(),
        Some(full.toplevel().unwrap().wl_surface().clone())
    );
    assert!(!state.window_interaction_allowed(floating, state.workspaces.active_id()));
    // Authentication must still be reachable over a fullscreen application.
    let (auth, _auth_wire) = crate::state::test_window_with_app_id(
        &mut state,
        &mut events,
        0xff778899,
        "dev.ferese.Authentication",
    );
    assert_eq!(
        state.seat.get_keyboard().unwrap().current_focus(),
        Some(auth.toplevel().unwrap().wl_surface().clone())
    );
    assert!(
        state.window_interaction_allowed(state.windows.ids()[&auth], state.workspaces.active_id())
    );
}
