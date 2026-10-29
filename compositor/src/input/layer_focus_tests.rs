use super::*;
use crate::state::{
    ClientState, dispatch_test_client as dispatch, test_client_request as request,
    test_window as window,
};
use smithay::desktop::{LayerSurface, layer_map_for_output};
use smithay::output::{Mode, Output, PhysicalProperties, Subpixel};
use smithay::reexports::calloop::EventLoop;
use smithay::reexports::wayland_protocols_wlr::layer_shell::v1::server::zwlr_layer_shell_v1::ZwlrLayerShellV1;
use smithay::reexports::wayland_server::protocol::wl_compositor::WlCompositor;
use smithay::reexports::wayland_server::protocol::wl_shm::WlShm;
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::sync::Arc;

fn layer(
    state: &mut Ferese,
    events: &mut EventLoop<'static, Ferese>,
    output: &Output,
) -> (LayerSurface, UnixStream) {
    layer_with_policy(state, events, output, 0, 2)
}

fn layer_with_policy(
    state: &mut Ferese,
    events: &mut EventLoop<'static, Ferese>,
    output: &Output,
    interactivity: u32,
    level: u32,
) -> (LayerSurface, UnixStream) {
    layer_with_namespace(state, events, output, interactivity, level, "bar")
}

pub(crate) fn layer_with_namespace(
    state: &mut Ferese,
    events: &mut EventLoop<'static, Ferese>,
    output: &Output,
    interactivity: u32,
    level: u32,
    namespace: &str,
) -> (LayerSurface, UnixStream) {
    let initial_focus = state.seat.get_keyboard().unwrap().current_focus();
    let (server, mut wire) = UnixStream::pair().unwrap();
    let client = state
        .display_handle
        .insert_client(server, Arc::new(ClientState::default()))
        .unwrap();
    let compositor = client
        .create_resource::<WlCompositor, smithay::wayland::GlobalData, Ferese>(
            &state.display_handle,
            6,
            smithay::wayland::GlobalData,
        )
        .unwrap();
    let layers = client
        .create_resource::<ZwlrLayerShellV1, smithay::wayland::GlobalData, Ferese>(
            &state.display_handle,
            4,
            smithay::wayland::GlobalData,
        )
        .unwrap();
    let shm = client
        .create_resource::<WlShm, smithay::wayland::GlobalData, Ferese>(
            &state.display_handle,
            1,
            smithay::wayland::GlobalData,
        )
        .unwrap();
    request(&mut wire, compositor.id().protocol_id(), 0, &[2], None);
    let mut name = namespace.as_bytes().to_vec();
    name.push(0);
    let mut args = vec![3, 2, 0, level, name.len() as u32];
    name.resize(name.len().next_multiple_of(4), 0);
    args.extend(
        name.chunks_exact(4)
            .map(|word| u32::from_ne_bytes(word.try_into().unwrap())),
    );
    request(&mut wire, layers.id().protocol_id(), 0, &args, None);
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
            serial = Some(u32::from_ne_bytes(
                bytes[offset + 8..offset + 12].try_into().unwrap(),
            ));
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
    file.write_all(&0xff112233u32.to_ne_bytes().repeat(64 * 48))
        .unwrap();
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
            time: smithay::backend::input::InputTime::from_millis(0),
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
            time: smithay::backend::input::InputTime::from_millis(1),
        },
    );
    state.focus_window_at(outside, SERIAL_COUNTER.next_serial(), true);
    assert_eq!(
        keyboard.current_focus(),
        None,
        "empty-desktop clicks dismiss the panel"
    );
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
    let second_bounds = state
        .presented_window_rect(state.windows.ids()[&second])
        .unwrap();
    let on_window = Point::from((second_bounds.x + 20., second_bounds.y + 20.));
    state.input_settings.focus_follows_mouse = true;
    keyboard.set_focus(
        &mut state,
        Some(panel.wl_surface().clone()),
        SERIAL_COUNTER.next_serial(),
    );
    state.focus_window_under_pointer(&pointer, pointer.current_location(), on_window);
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

pub(super) fn fixture() -> (Ferese, EventLoop<'static, Ferese>, Output) {
    fixture_with_animations(false)
}

fn fixture_with_animations(animations: bool) -> (Ferese, EventLoop<'static, Ferese>, Output) {
    let mut events = EventLoop::try_new().unwrap();
    let mut config = crate::config::Config::default().runtime_config().unwrap();
    config.wallpaper.path = None;
    config.animations_enabled = animations;
    let mut state = Ferese::new(
        &mut events,
        smithay::reexports::wayland_server::Display::new().unwrap(),
        config,
    )
    .unwrap();
    let output = Output::new(
        "layers".into(),
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
    state.register_output(&output, "layers".into());
    (state, events, output)
}

fn commit_policy(
    state: &mut Ferese,
    events: &mut EventLoop<'static, Ferese>,
    wire: &mut UnixStream,
    keyboard: u32,
) {
    request(wire, 3, 4, &[keyboard], None);
    request(wire, 2, 6, &[], None);
    dispatch(events, state);
}

// Unmapping resets the layer's pending state. Reapply its policy and complete
// the initial bufferless configure/ack exchange before attaching again.
fn remap_layer(
    state: &mut Ferese,
    events: &mut EventLoop<'static, Ferese>,
    wire: &mut UnixStream,
    keyboard: u32,
    level: u32,
) {
    request(wire, 3, 0, &[65, 48], None);
    request(wire, 3, 1, &[1 | 4], None);
    request(wire, 3, 4, &[keyboard], None);
    request(wire, 3, 8, &[level], None);
    request(wire, 2, 6, &[], None);
    dispatch(events, state);
    ack_latest_configure(wire);
    request(wire, 2, 1, &[5, 0, 0], None);
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
        assert!(
            panel.layer_surface().alive(),
            "the layer role survives unmapping"
        );
        assert!(
            state
                .space
                .layer_for_surface(
                    panel.wl_surface(),
                    smithay::desktop::WindowSurfaceType::TOPLEVEL
                )
                .is_some()
        );
        assert!(
            smithay::backend::renderer::utils::with_renderer_surface_state(
                panel.wl_surface(),
                |s| s.buffer().is_none()
            )
            .unwrap()
        );
        assert_eq!(
            keyboard.current_focus(),
            fallback,
            "null commit must revoke keyboard focus for policy {interactivity}"
        );
        // An unmapped surface is back in its initial state. Keep this commit
        // valid while asserting that keyboard policy alone cannot remap it.
        request(&mut wire, 3, 0, &[64, 48], None);
        request(&mut wire, 3, 1, &[1 | 4], None);
        request(&mut wire, 3, 4, &[interactivity], None);
        request(&mut wire, 3, 8, &[2], None);
        request(&mut wire, 2, 6, &[], None);
        dispatch(&mut events, &mut state);
        assert_eq!(
            keyboard.current_focus(),
            fallback,
            "unmapped commits must not reclaim focus"
        );
        remap_layer(&mut state, &mut events, &mut wire, interactivity, 2);
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
        assert_eq!(
            keyboard.current_focus(),
            fallback,
            "destruction restores window focus"
        );
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
            serial = Some(u32::from_ne_bytes(
                bytes[offset + 8..offset + 12].try_into().unwrap(),
            ));
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
        .create_resource::<ExtSessionLockManagerV1, smithay::wayland::GlobalData, Ferese>(
            &state.display_handle,
            1,
            smithay::wayland::GlobalData,
        )
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
    remap_layer(&mut state, &mut events, &mut wire, 1, 3);
    assert_eq!(
        keyboard.current_focus(),
        None,
        "layer remapping cannot bypass the lock"
    );
    request(&mut wire, 3, 7, &[], None);
    dispatch(&mut events, &mut state);
    assert_eq!(
        keyboard.current_focus(),
        None,
        "layer destruction preserves lock-owned focus policy"
    );
}

#[test]
fn fullscreen_hidden_top_commits_cannot_steal_keyboard_focus() {
    if !crate::startup_tests::private_runtime(
        "input::layer_focus_tests::fullscreen_hidden_top_commits_cannot_steal_keyboard_focus",
    ) {
        return;
    }
    let (mut state, mut events, output) = fixture();
    let (window, _window_wire) = window(&mut state, &mut events, 0xff112233);
    let id = state.windows.ids()[&window];
    assert!(state.activate_managed_window(id));
    let fallback = Some(window.toplevel().unwrap().wl_surface().clone());
    let keyboard = state.seat.get_keyboard().unwrap();
    state.set_window_fullscreen(id, true);
    let (top, mut top_wire) = layer_with_policy(&mut state, &mut events, &output, 1, 2);
    assert!(state.output_has_fullscreen(&output));
    assert!(
        state.layer_under((20., 20.).into()).is_none(),
        "Top is excluded from hit testing by fullscreen policy"
    );
    assert_eq!(
        keyboard.current_focus(),
        fallback,
        "a hidden Top layer's map commit cannot steal keyboard focus"
    );
    commit_policy(&mut state, &mut events, &mut top_wire, 1);
    assert_eq!(
        keyboard.current_focus(),
        fallback,
        "hidden animation commits cannot steal focus"
    );
    state.set_window_fullscreen(id, false);
    assert_eq!(
        keyboard.current_focus(),
        Some(top.wl_surface().clone()),
        "fullscreen exit restores visible exclusive layers"
    );
    assert_eq!(state.layer_under((20., 20.).into()).unwrap().0, top);
    state.set_window_fullscreen(id, true);
    assert_eq!(
        keyboard.current_focus(),
        fallback,
        "fullscreen entry revokes hidden Top focus immediately"
    );
    let (overlay, mut overlay_wire) = layer_with_policy(&mut state, &mut events, &output, 1, 3);
    assert_eq!(
        keyboard.current_focus(),
        Some(overlay.wl_surface().clone()),
        "Overlay remains eligible over fullscreen"
    );
    assert_eq!(state.layer_under((20., 20.).into()).unwrap().0, overlay);
    request(&mut overlay_wire, 3, 7, &[], None);
    dispatch(&mut events, &mut state);
    assert_eq!(
        keyboard.current_focus(),
        fallback,
        "destroying Overlay must not restore hidden Top"
    );
    request(&mut top_wire, 2, 1, &[0, 0, 0], None);
    request(&mut top_wire, 2, 6, &[], None);
    dispatch(&mut events, &mut state);
    remap_layer(&mut state, &mut events, &mut top_wire, 1, 2);
    assert_eq!(
        keyboard.current_focus(),
        fallback,
        "hidden remapping cannot reclaim focus"
    );
    state.set_window_fullscreen(id, false);
    commit_policy(&mut state, &mut events, &mut top_wire, 2);
    state.focus_window_at((20., 20.).into(), SERIAL_COUNTER.next_serial(), true);
    assert_eq!(keyboard.current_focus(), Some(top.wl_surface().clone()));
    let pointer = state.seat.get_pointer().unwrap();
    pointer.motion(
        &mut state,
        Some((top.wl_surface().clone(), (0., 0.).into())),
        &MotionEvent {
            location: (20., 20.).into(),
            serial: SERIAL_COUNTER.next_serial(),
            time: smithay::backend::input::InputTime::from_millis(0),
        },
    );
    state.set_window_fullscreen(id, true);
    assert_eq!(
        keyboard.current_focus(),
        fallback,
        "fullscreen also revokes on-demand Top focus"
    );
    commit_policy(&mut state, &mut events, &mut top_wire, 0);
    commit_policy(&mut state, &mut events, &mut top_wire, 2);
    assert_eq!(
        keyboard.current_focus(),
        fallback,
        "a stale pointer owner cannot activate a hidden panel"
    );
    state.set_window_fullscreen(id, false);
    assert_eq!(
        keyboard.current_focus(),
        fallback,
        "on-demand layers require interaction after fullscreen exit"
    );
    state.focus_window_at((20., 20.).into(), SERIAL_COUNTER.next_serial(), true);
    assert_eq!(keyboard.current_focus(), Some(top.wl_surface().clone()));
}

#[test]
fn valid_layer_popup_grab_survives_commits_but_not_parent_unmapping() {
    use smithay::desktop::{PopupKeyboardGrab, PopupKind};
    use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_wm_base::XdgWmBase;
    use smithay::wayland::shell::xdg::{XdgShellHandler, XdgWmBaseUserData};
    if !crate::startup_tests::private_runtime(
        "input::layer_focus_tests::valid_layer_popup_grab_survives_commits_but_not_parent_unmapping",
    ) {
        return;
    }
    let (mut state, mut events, output) = fixture();
    let (top, mut wire) = layer_with_policy(&mut state, &mut events, &output, 2, 2);
    state.focus_window_at((20., 20.).into(), SERIAL_COUNTER.next_serial(), true);
    let client = top.wl_surface().client().unwrap();
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
    request(&mut wire, shell.id().protocol_id(), 1, &[6], None); // Positioner.
    request(&mut wire, 6, 1, &[32, 24], None);
    request(&mut wire, 6, 2, &[0, 0, 32, 24], None);
    request(&mut wire, compositor.id().protocol_id(), 0, &[7], None);
    request(&mut wire, shell.id().protocol_id(), 2, &[8, 7], None);
    request(&mut wire, 8, 2, &[9, 0, 6], None); // Popup with layer parent assigned next.
    request(&mut wire, 3, 5, &[9], None);
    dispatch(&mut events, &mut state);
    let popup = state
        .xdg_shell_state()
        .popup_surfaces()
        .iter()
        .find(|popup| {
            popup
                .wl_surface()
                .client()
                .is_some_and(|owner| owner.id() == client.id())
        })
        .unwrap()
        .clone();
    let serial = SERIAL_COUNTER.next_serial();
    let seat = state.seat.clone();
    let grab = state
        .popups
        .grab_popup(
            top.wl_surface().clone(),
            PopupKind::Xdg(popup.clone()),
            &seat,
            serial,
        )
        .unwrap();
    let keyboard = state.seat.get_keyboard().unwrap();
    keyboard.set_grab(&mut state, PopupKeyboardGrab::new(&grab), serial);
    keyboard.set_focus(&mut state, grab.current_grab(), serial);
    let popup_focus = Some(popup.wl_surface().clone());
    assert_eq!(keyboard.current_focus(), popup_focus);
    commit_policy(&mut state, &mut events, &mut wire, 2);
    assert!(keyboard.is_grabbed());
    assert_eq!(
        keyboard.current_focus(),
        popup_focus,
        "valid popup grabs survive parent commits"
    );
    let (_overlay, _overlay_wire) = layer_with_policy(&mut state, &mut events, &output, 1, 3);
    assert_eq!(
        keyboard.current_focus(),
        popup_focus,
        "new exclusive layers do not interrupt a valid popup grab"
    );
    request(&mut wire, 2, 1, &[0, 0, 0], None);
    request(&mut wire, 2, 6, &[], None);
    dispatch(&mut events, &mut state);
    assert!(
        !keyboard.is_grabbed(),
        "unmapping revokes the parent popup grab"
    );
    assert_ne!(keyboard.current_focus(), popup_focus);
    assert_ne!(keyboard.current_focus(), Some(top.wl_surface().clone()));
}

#[test]
fn fullscreen_layer_eligibility_is_per_output_and_respects_overview() {
    if !crate::startup_tests::private_runtime(
        "input::layer_focus_tests::fullscreen_layer_eligibility_is_per_output_and_respects_overview",
    ) {
        return;
    }
    let (mut state, mut events, output) = fixture();
    let (window, _window_wire) = window(&mut state, &mut events, 0xff112233);
    let id = state.windows.ids()[&window];
    assert!(state.activate_managed_window(id));
    state.set_window_fullscreen(id, true);
    let (top, _top_wire) = layer_with_policy(&mut state, &mut events, &output, 1, 2);
    assert!(!state.layer_keyboard_eligible(&top));
    assert!(!state.layer_visible_for_frame(
        &output,
        smithay::wayland::shell::wlr_layer::Layer::Top,
        false
    ));
    assert!(state.layer_visible_for_frame(
        &output,
        smithay::wayland::shell::wlr_layer::Layer::Top,
        true
    ));
    state.set_overview_active(true);
    assert!(state.layer_keyboard_eligible(&top));
    assert_eq!(
        state.seat.get_keyboard().unwrap().current_focus(),
        Some(top.wl_surface().clone())
    );
    state.set_overview_active(false);
    assert!(!state.layer_keyboard_eligible(&top));
    assert_eq!(
        state.seat.get_keyboard().unwrap().current_focus(),
        Some(window.toplevel().unwrap().wl_surface().clone())
    );
    let other = Output::new(
        "other".into(),
        PhysicalProperties {
            serial_number: String::new(),
            size: (0, 0).into(),
            subpixel: Subpixel::Unknown,
            make: "test".into(),
            model: "test".into(),
        },
    );
    other.change_current_state(
        Some(Mode {
            size: (800, 600).into(),
            refresh: 60_000,
        }),
        None,
        None,
        None,
    );
    state.space.map_output(&other, (800, 0));
    state.register_output(&other, "other".into());
    // Move the test role into the second output's map to isolate visibility
    // policy from the separately tested output-selection policy.
    let (other_top, mut wire) = layer_with_policy(&mut state, &mut events, &output, 0, 2);
    layer_map_for_output(&output).unmap_layer(&other_top);
    layer_map_for_output(&other).map_layer(&other_top).unwrap();
    commit_policy(&mut state, &mut events, &mut wire, 1);
    assert!(
        state.layer_keyboard_eligible(&other_top),
        "fullscreen on the first output does not hide Top on another"
    );
    assert_eq!(
        state.seat.get_keyboard().unwrap().current_focus(),
        Some(other_top.wl_surface().clone())
    );
    state.unregister_output(&other);
    assert!(!state.layer_keyboard_eligible(&other_top));
    assert_eq!(
        state.seat.get_keyboard().unwrap().current_focus(),
        Some(window.toplevel().unwrap().wl_surface().clone())
    );
}

#[test]
fn animated_overview_exit_revokes_top_focus_when_it_becomes_hidden() {
    if !crate::startup_tests::private_runtime(
        "input::layer_focus_tests::animated_overview_exit_revokes_top_focus_when_it_becomes_hidden",
    ) {
        return;
    }
    let (mut state, mut events, output) = fixture_with_animations(true);
    let (window, _window_wire) = window(&mut state, &mut events, 0xff112233);
    let id = state.windows.ids()[&window];
    assert!(state.activate_managed_window(id));
    state.set_window_fullscreen(id, true);
    let (top, _wire) = layer_with_policy(&mut state, &mut events, &output, 1, 2);
    let keyboard = state.seat.get_keyboard().unwrap();
    state.set_overview_active(true);
    assert!(state.layer_keyboard_eligible(&top));
    assert_eq!(keyboard.current_focus(), Some(top.wl_surface().clone()));
    let tick = std::time::Instant::now();
    for step in 1..=300 {
        state.advance_animations(tick + std::time::Duration::from_millis(16 * step));
    }
    state.set_overview_active(false);
    assert!(
        state.layer_keyboard_eligible(&top),
        "Top remains visible during the overview exit animation"
    );
    for step in 1..=300 {
        state.advance_animations(tick + std::time::Duration::from_millis(4800 + 16 * step));
    }
    assert!(!state.layer_keyboard_eligible(&top));
    assert_eq!(
        keyboard.current_focus(),
        Some(window.toplevel().unwrap().wl_surface().clone()),
        "completion of the visibility transition revokes focus without a client commit"
    );
}

#[test]
fn touch_opened_on_demand_layer_does_not_need_pointer_focus() {
    if !crate::startup_tests::private_runtime(
        "input::layer_focus_tests::touch_opened_on_demand_layer_does_not_need_pointer_focus",
    ) {
        return;
    }
    let (mut state, mut events, output) = fixture();
    let (panel, mut wire) = layer(&mut state, &mut events, &output);
    let pointer = state.seat.get_pointer().unwrap();
    let keyboard = state.seat.get_keyboard().unwrap();
    let inside = Point::from((20., 20.));
    assert_eq!(pointer.current_focus(), None);
    // TouchDown focuses the target before delivering the opening touch.
    // Unlike the pointer regression, no pointer motion establishes focus.
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
    let input_time = state.last_pointer_time;
    pointer.motion(
        &mut state,
        None,
        &MotionEvent {
            location: outside,
            serial: SERIAL_COUNTER.next_serial(),
            time: input_time,
        },
    );
    state.focus_window_at(outside, SERIAL_COUNTER.next_serial(), true);
    assert_eq!(
        keyboard.current_focus(),
        None,
        "empty-desktop clicks dismiss the panel"
    );
    request(&mut wire, 2, 6, &[], None);
    dispatch(&mut events, &mut state);
    assert_eq!(
        keyboard.current_focus(),
        None,
        "later animation commits must not reclaim focus"
    );
    commit_policy(&mut state, &mut events, &mut wire, 0);
    state.focus_window_at(inside, SERIAL_COUNTER.next_serial(), true);
    state.focus_window_at(outside, SERIAL_COUNTER.next_serial(), true);
    commit_policy(&mut state, &mut events, &mut wire, 2);
    assert_eq!(
        keyboard.current_focus(),
        None,
        "a later interaction cancels delayed focus"
    );
    commit_policy(&mut state, &mut events, &mut wire, 0);
    pointer.motion(
        &mut state,
        Some((panel.wl_surface().clone(), (0., 0.).into())),
        &MotionEvent {
            location: inside,
            serial: SERIAL_COUNTER.next_serial(),
            time: input_time,
        },
    );
    commit_policy(&mut state, &mut events, &mut wire, 2);
    assert_eq!(
        keyboard.current_focus(),
        None,
        "pointer hover alone cannot open keyboard focus"
    );
}
