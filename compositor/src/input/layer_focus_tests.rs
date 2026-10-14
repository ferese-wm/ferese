use super::*;
use crate::state::ClientState;
use crate::state::{dispatch_test_client as dispatch, test_client_request as request, test_window as window};
use smithay::desktop::{LayerSurface, layer_map_for_output};
use smithay::output::{Mode, Output, PhysicalProperties, Subpixel};
use smithay::reexports::calloop::EventLoop;
use smithay::reexports::wayland_protocols_wlr::layer_shell::v1::server::zwlr_layer_shell_v1::ZwlrLayerShellV1;
use smithay::reexports::wayland_server::protocol::{wl_compositor::WlCompositor, wl_shm::WlShm};
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::sync::Arc;

fn layer(state: &mut Ferese, events: &mut EventLoop<'static, Ferese>, output: &Output) -> (LayerSurface, UnixStream) {
    layer_with_policy(state, events, output, 0, 2)
}

fn layer_with_policy(
    state: &mut Ferese,
    events: &mut EventLoop<'static, Ferese>,
    output: &Output,
    interactivity: u32,
    level: u32,
) -> (LayerSurface, UnixStream) {
    let initial_focus = state.seat.get_keyboard().unwrap().current_focus();
    let (server, mut wire) = UnixStream::pair().unwrap();
    let client = state
        .display_handle
        .insert_client(server, Arc::new(ClientState::default()))
        .unwrap();
    let compositor = client
        .create_resource::<WlCompositor, (), Ferese>(&state.display_handle, 6, ())
        .unwrap();
    let layers = client
        .create_resource::<ZwlrLayerShellV1, (), Ferese>(&state.display_handle, 4, ())
        .unwrap();
    let shm = client
        .create_resource::<WlShm, (), Ferese>(&state.display_handle, 1, ())
        .unwrap();
    request(&mut wire, compositor.id().protocol_id(), 0, &[2], None);
    request(
        &mut wire,
        layers.id().protocol_id(),
        0,
        &[3, 2, 0, level, 4, u32::from_ne_bytes(*b"bar\0")],
        None,
    );
    request(&mut wire, 3, 0, &[64, 48], None);
    request(&mut wire, 3, 1, &[1 | 4], None); // Top left.
    request(&mut wire, 3, 4, &[interactivity], None);
    request(&mut wire, 2, 6, &[], None);
    dispatch(events, state);
    assert_eq!(
        state.seat.get_keyboard().unwrap().current_focus(),
        initial_focus,
        "an initially unconfigured layer must not acquire focus"
    );

    wire.set_nonblocking(true).unwrap();
    let mut bytes = Vec::new();
    assert_eq!(
        wire.read_to_end(&mut bytes).unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
    wire.set_nonblocking(false).unwrap();
    let mut serial = None;
    let mut offset = 0;
    while offset < bytes.len() {
        let object = u32::from_ne_bytes(bytes[offset..offset + 4].try_into().unwrap());
        let header = u32::from_ne_bytes(bytes[offset + 4..offset + 8].try_into().unwrap());
        if object == 3 && header & 0xffff == 0 {
            serial = Some(u32::from_ne_bytes(bytes[offset + 8..offset + 12].try_into().unwrap()));
        }
        offset += (header >> 16) as usize;
    }
    request(&mut wire, 3, 6, &[serial.expect("layer configure")], None);
    dispatch(events, state);
    assert_eq!(
        state.seat.get_keyboard().unwrap().current_focus(),
        initial_focus,
        "acknowledging configure without mapping must not acquire focus"
    );
    let mut file = tempfile::tempfile().unwrap();
    file.write_all(&0xff112233u32.to_ne_bytes().repeat(64 * 48)).unwrap();
    request(
        &mut wire,
        shm.id().protocol_id(),
        0,
        &[4, 64 * 48 * 4],
        Some(file.as_raw_fd()),
    );
    request(&mut wire, 4, 0, &[5, 0, 64, 48, 64 * 4, 0], None);
    request(&mut wire, 2, 1, &[5, 0, 0], None);
    request(&mut wire, 2, 6, &[], None);
    dispatch(events, state);
    let layer = layer_map_for_output(output)
        .layers()
        .find(|layer| {
            layer
                .wl_surface()
                .client()
                .is_some_and(|owner| owner.id() == client.id())
        })
        .unwrap()
        .clone();
    (layer, wire)
}

#[test]
fn on_demand_layer_focus_follows_opening_and_outside_clicks() {
    if !crate::startup_tests::private_runtime(
        "input::layer_focus_tests::on_demand_layer_focus_follows_opening_and_outside_clicks",
    ) {
        return;
    }
    let mut events = EventLoop::try_new().unwrap();
    let mut config = crate::config::Config::default().runtime_config().unwrap();
    config.wallpaper.path = None;
    config.animations_enabled = false;
    let mut state = Ferese::new(
        &mut events,
        smithay::reexports::wayland_server::Display::new().unwrap(),
        config,
    )
    .unwrap();
    let output = Output::new(
        "focus".into(),
        PhysicalProperties {
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
    let (panel, mut wire) = layer(&mut state, &mut events, &output);
    let pointer = state.seat.get_pointer().unwrap();
    let keyboard = state.seat.get_keyboard().unwrap();
    let inside = Point::from((20., 20.));
    pointer.motion(
        &mut state,
        Some((panel.wl_surface().clone(), (0., 0.).into())),
        &MotionEvent {
            location: inside,
            serial: SERIAL_COUNTER.next_serial(),
            time: 0,
        },
    );
    // The opening click arrives while the collapsed panel is noninteractive.
    state.focus_window_at(inside, SERIAL_COUNTER.next_serial(), true);
    assert_eq!(keyboard.current_focus(), None);
    request(&mut wire, 3, 4, &[2], None); // OnDemand.
    request(&mut wire, 2, 6, &[], None);
    dispatch(&mut events, &mut state);
    assert_eq!(
        keyboard.current_focus(),
        Some(panel.wl_surface().clone()),
        "opening must not require a second click"
    );
    state.focus_window_at(inside, SERIAL_COUNTER.next_serial(), true);
    assert_eq!(
        keyboard.current_focus(),
        Some(panel.wl_surface().clone()),
        "inside clicks keep the panel open"
    );

    let outside = Point::from((400., 300.));
    pointer.motion(
        &mut state,
        None,
        &MotionEvent {
            location: outside,
            serial: SERIAL_COUNTER.next_serial(),
            time: 1,
        },
    );
    state.focus_window_at(outside, SERIAL_COUNTER.next_serial(), true);
    assert_eq!(keyboard.current_focus(), None, "empty-desktop clicks dismiss the panel");
    request(&mut wire, 2, 6, &[], None);
    dispatch(&mut events, &mut state);
    assert_eq!(
        keyboard.current_focus(),
        None,
        "later animation commits must not reclaim focus"
    );

    let (wallpaper, _wallpaper_wire) = layer(&mut state, &mut events, &output);
    keyboard.set_focus(
        &mut state,
        Some(panel.wl_surface().clone()),
        SERIAL_COUNTER.next_serial(),
    );
    assert_eq!(state.layer_under(inside).unwrap().0, wallpaper);
    state.focus_window_at(inside, SERIAL_COUNTER.next_serial(), true);
    assert_eq!(
        keyboard.current_focus(),
        None,
        "noninteractive-layer clicks dismiss the panel"
    );

    let (first, _first_wire) = window(&mut state, &mut events, 0xff112233);
    let (second, _second_wire) = window(&mut state, &mut events, 0xff334455);
    let first_id = state.windows.ids()[&first];
    assert!(state.activate_managed_window(first_id));
    let second_bounds = state.presented_window_rect(state.windows.ids()[&second]).unwrap();
    let on_window = Point::from((second_bounds.x + 20., second_bounds.y + 20.));
    state.input_settings.focus_follows_mouse = true;
    keyboard.set_focus(
        &mut state,
        Some(panel.wl_surface().clone()),
        SERIAL_COUNTER.next_serial(),
    );
    state.focus_window_under_pointer(&pointer, on_window);
    assert_eq!(
        keyboard.current_focus(),
        Some(panel.wl_surface().clone()),
        "pointer leave must not dismiss the panel"
    );
    state.focus_window_at(on_window, SERIAL_COUNTER.next_serial(), true);
    assert_eq!(
        keyboard.current_focus(),
        Some(second.toplevel().unwrap().wl_surface().clone()),
        "an outside click must still focus its window"
    );
    request(&mut wire, 3, 4, &[0], None); // None.
    request(&mut wire, 2, 6, &[], None);
    dispatch(&mut events, &mut state);
    request(&mut wire, 3, 4, &[2], None); // OnDemand, without a pointer on the panel.
    request(&mut wire, 2, 6, &[], None);
    dispatch(&mut events, &mut state);
    assert_eq!(
        keyboard.current_focus(),
        Some(second.toplevel().unwrap().wl_surface().clone()),
        "enabling a panel away from the pointer must not steal window focus"
    );
}

fn fixture() -> (Ferese, EventLoop<'static, Ferese>, Output) {
    let mut events = EventLoop::try_new().unwrap();
    let mut config = crate::config::Config::default().runtime_config().unwrap();
    config.wallpaper.path = None;
    config.animations_enabled = false;
    let mut state = Ferese::new(
        &mut events,
        smithay::reexports::wayland_server::Display::new().unwrap(),
        config,
    )
    .unwrap();
    let output = Output::new(
        "layers".into(),
        PhysicalProperties {
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
    state.register_output(&output, "layers".into());
    (state, events, output)
}

fn commit_policy(state: &mut Ferese, events: &mut EventLoop<'static, Ferese>, wire: &mut UnixStream, keyboard: u32) {
    request(wire, 3, 4, &[keyboard], None);
    request(wire, 2, 6, &[], None);
    dispatch(events, state);
}

#[test]
fn null_buffer_commit_revokes_layer_focus_without_destroying_role() {
    if !crate::startup_tests::private_runtime(
        "input::layer_focus_tests::null_buffer_commit_revokes_layer_focus_without_destroying_role",
    ) {
        return;
    }
    let (mut state, mut events, output) = fixture();
    let (window, _window_wire) = window(&mut state, &mut events, 0xff112233);
    let id = state.windows.ids()[&window];
    assert!(state.activate_managed_window(id));
    let fallback = Some(window.toplevel().unwrap().wl_surface().clone());
    let keyboard = state.seat.get_keyboard().unwrap();
    for interactivity in [1, 2] {
        let (panel, mut wire) = layer(&mut state, &mut events, &output);
        commit_policy(&mut state, &mut events, &mut wire, interactivity);
        state.focus_window_at((20., 20.).into(), SERIAL_COUNTER.next_serial(), true);
        assert_eq!(keyboard.current_focus(), Some(panel.wl_surface().clone()));
        request(&mut wire, 2, 1, &[0, 0, 0], None); // wl_surface.attach(NULL).
        request(&mut wire, 2, 6, &[], None);
        dispatch(&mut events, &mut state);
        assert!(panel.layer_surface().alive(), "the layer role survives unmapping");
        assert!(
            state
                .space
                .layer_for_surface(panel.wl_surface(), smithay::desktop::WindowSurfaceType::TOPLEVEL)
                .is_some()
        );
        assert!(
            smithay::backend::renderer::utils::with_renderer_surface_state(panel.wl_surface(), |s| s
                .buffer()
                .is_none())
            .unwrap()
        );
        assert_eq!(
            keyboard.current_focus(),
            fallback,
            "null commit must revoke keyboard focus for policy {interactivity}"
        );
        request(&mut wire, 2, 6, &[], None);
        dispatch(&mut events, &mut state);
        assert_eq!(
            keyboard.current_focus(),
            fallback,
            "unmapped commits must not reclaim focus"
        );
        // Reconfigure with a changed size before mapping again. This follows
        // the protocol's bufferless-commit/configure/ack sequence, including
        // with Smithay 0.7 which retains configure state on null commits.
        request(&mut wire, 3, 0, &[65, 48], None);
        request(&mut wire, 2, 6, &[], None);
        dispatch(&mut events, &mut state);
        ack_latest_configure(&mut wire);
        request(&mut wire, 2, 1, &[5, 0, 0], None);
        request(&mut wire, 2, 6, &[], None);
        dispatch(&mut events, &mut state);
        if interactivity == 2 {
            assert_eq!(
                keyboard.current_focus(),
                fallback,
                "on-demand remap requires interaction"
            );
            state.focus_window_at((20., 20.).into(), SERIAL_COUNTER.next_serial(), true);
        }
        assert_eq!(
            keyboard.current_focus(),
            Some(panel.wl_surface().clone()),
            "remapped layer remains focusable"
        );
        request(&mut wire, 3, 7, &[], None); // Destroy the role.
        dispatch(&mut events, &mut state);
        assert_eq!(keyboard.current_focus(), fallback, "destruction restores window focus");
    }
}

fn ack_latest_configure(wire: &mut UnixStream) {
    wire.set_nonblocking(true).unwrap();
    let mut bytes = Vec::new();
    assert_eq!(
        wire.read_to_end(&mut bytes).unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
    wire.set_nonblocking(false).unwrap();
    let mut serial = None;
    let mut offset = 0;
    while offset < bytes.len() {
        let object = u32::from_ne_bytes(bytes[offset..offset + 4].try_into().unwrap());
        let header = u32::from_ne_bytes(bytes[offset + 4..offset + 8].try_into().unwrap());
        if object == 3 && header & 0xffff == 0 {
            serial = Some(u32::from_ne_bytes(bytes[offset + 8..offset + 12].try_into().unwrap()));
        }
        offset += (header >> 16) as usize;
    }
    request(wire, 3, 6, &[serial.expect("layer configure")], None);
}

#[test]
fn exclusive_layer_priority_and_output_removal_restore_eligible_focus() {
    if !crate::startup_tests::private_runtime(
        "input::layer_focus_tests::exclusive_layer_priority_and_output_removal_restore_eligible_focus",
    ) {
        return;
    }
    let (mut state, mut events, output) = fixture();
    let (window, _window_wire) = window(&mut state, &mut events, 0xff112233);
    assert!(state.activate_managed_window(state.windows.ids()[&window]));
    let fallback = Some(window.toplevel().unwrap().wl_surface().clone());
    let keyboard = state.seat.get_keyboard().unwrap();
    let (top, mut top_wire) = layer_with_policy(&mut state, &mut events, &output, 1, 2);
    assert_eq!(keyboard.current_focus(), Some(top.wl_surface().clone()));
    let (overlay, mut overlay_wire) = layer_with_policy(&mut state, &mut events, &output, 1, 3);
    assert_eq!(keyboard.current_focus(), Some(overlay.wl_surface().clone()));
    commit_policy(&mut state, &mut events, &mut top_wire, 1);
    assert_eq!(
        keyboard.current_focus(),
        Some(overlay.wl_surface().clone()),
        "Top commits cannot steal from an exclusive Overlay"
    );
    request(&mut overlay_wire, 2, 1, &[0, 0, 0], None);
    request(&mut overlay_wire, 2, 6, &[], None);
    dispatch(&mut events, &mut state);
    assert_eq!(
        keyboard.current_focus(),
        Some(top.wl_surface().clone()),
        "unmapping falls back to another eligible exclusive layer"
    );
    commit_policy(&mut state, &mut events, &mut top_wire, 0);
    assert_eq!(
        keyboard.current_focus(),
        fallback,
        "disabling keyboard interaction restores the window"
    );
    commit_policy(&mut state, &mut events, &mut top_wire, 1);
    assert_eq!(keyboard.current_focus(), Some(top.wl_surface().clone()));
    state.unregister_output(&output);
    assert_eq!(
        keyboard.current_focus(),
        None,
        "removed outputs cannot retain layer focus"
    );
}

#[test]
fn layer_mapping_commits_do_not_override_session_lock() {
    use smithay::reexports::wayland_protocols::ext::session_lock::v1::server::ext_session_lock_manager_v1::ExtSessionLockManagerV1;
    if !crate::startup_tests::private_runtime(
        "input::layer_focus_tests::layer_mapping_commits_do_not_override_session_lock",
    ) {
        return;
    }
    let (mut state, mut events, output) = fixture();
    let (panel, mut wire) = layer_with_policy(&mut state, &mut events, &output, 1, 3);
    let client = panel.wl_surface().client().unwrap();
    let manager = client
        .create_resource::<ExtSessionLockManagerV1, (), Ferese>(&state.display_handle, 1, ())
        .unwrap();
    request(&mut wire, manager.id().protocol_id(), 1, &[6], None);
    dispatch(&mut events, &mut state);
    let keyboard = state.seat.get_keyboard().unwrap();
    assert_eq!(keyboard.current_focus(), None);
    commit_policy(&mut state, &mut events, &mut wire, 1);
    assert_eq!(
        keyboard.current_focus(),
        None,
        "an exclusive layer cannot focus during lock acquisition"
    );
    request(&mut wire, 2, 1, &[0, 0, 0], None);
    request(&mut wire, 2, 6, &[], None);
    dispatch(&mut events, &mut state);
    request(&mut wire, 2, 1, &[5, 0, 0], None);
    request(&mut wire, 2, 6, &[], None);
    dispatch(&mut events, &mut state);
    assert_eq!(keyboard.current_focus(), None, "layer remapping cannot bypass the lock");
    request(&mut wire, 3, 7, &[], None);
    dispatch(&mut events, &mut state);
    assert_eq!(
        keyboard.current_focus(),
        None,
        "layer destruction preserves lock-owned focus policy"
    );
}
