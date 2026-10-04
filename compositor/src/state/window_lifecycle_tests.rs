use super::capture_privacy::tests::{ack_configure, dispatch, request, window};
use super::*;
use smithay::input::pointer::{Focus, GrabStartData, MotionEvent};
use smithay::reexports::wayland_server::Resource;
use smithay::utils::SERIAL_COUNTER;

fn fixture() -> (smithay::reexports::calloop::EventLoop<'static, Ferese>, Ferese, Output) {
    let mut events = smithay::reexports::calloop::EventLoop::try_new().unwrap();
    let mut state = crate::startup_tests::state(&mut events);
    state.animations_enabled = false;
    let output = Output::new(
        "test".into(),
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
    state.space.map_output(&output, (0, 0));
    state.register_output(&output, "test".into());
    (events, state, output)
}

#[test]
fn hidden_null_commit_removes_membership_and_remaps_without_workspace_switch() {
    if !crate::startup_tests::private_runtime(
        "state::window_lifecycle_tests::hidden_null_commit_removes_membership_and_remaps_without_workspace_switch",
    ) {
        return;
    }

    let (mut events, mut state, _) = fixture();
    let (window, mut wire) = window(&mut state, &mut events, 0xff112233);
    let id = state.windows.ids()[&window];
    state.switch_workspace(2);
    let active = state.workspaces.active_id();
    assert!(state.space.element_location(&window).is_none());
    assert_eq!(
        state.window_for_surface(window.toplevel().unwrap().wl_surface()),
        Some(window.clone())
    );

    request(&mut wire, 2, 1, &[0, 0, 0], None);
    request(&mut wire, 2, 6, &[], None);
    dispatch(&mut events, &mut state);
    assert!(!state.windows.ids().contains_key(&window));
    assert!(state.workspaces.workspace_for_window(id).is_none());

    request(&mut wire, 2, 6, &[], None);
    dispatch(&mut events, &mut state);
    ack_configure(&mut wire, 3);
    request(&mut wire, 2, 1, &[6, 0, 0], None);
    request(&mut wire, 2, 6, &[], None);
    dispatch(&mut events, &mut state);
    let remapped = state.windows.ids()[&window];
    assert_ne!(id, remapped);
    assert_eq!(state.workspaces.workspace_for_window(remapped), Some(active));
    assert_eq!(state.workspaces.active_id(), active);
}

#[test]
fn hidden_commits_update_client_geometry_and_release_resize_transactions() {
    if !crate::startup_tests::private_runtime(
        "state::window_lifecycle_tests::hidden_commits_update_client_geometry_and_release_resize_transactions",
    ) {
        return;
    }

    let (mut events, mut state, _) = fixture();
    let (window, mut wire) = window(&mut state, &mut events, 0xff112233);
    let id = state.windows.ids()[&window];
    state.switch_workspace(2);
    assert!(state.space.element_location(&window).is_none());
    let toplevel = window.toplevel().unwrap();
    toplevel.with_pending_state(|pending| pending.size = Some((80, 48).into()));
    let serial = toplevel.send_configure();
    state.windows.set_transaction(
        id,
        crate::resize_transaction::ResizeTransaction::new(serial, state.presentation_now()),
    );
    state.display_handle.flush_clients().unwrap();
    assert_eq!(ack_configure(&mut wire, 3), u32::from(serial));
    request(&mut wire, 3, 3, &[0, 0, 32, 48], None);
    request(&mut wire, 2, 1, &[6, 0, 0], None);
    request(&mut wire, 2, 6, &[], None);
    dispatch(&mut events, &mut state);
    assert_eq!(window.geometry().size, (32, 48).into());
    assert!(state.windows.transaction(&id).is_none());
    assert_eq!(
        state.windows.geometry(&id).unwrap().client.committed_size,
        Some(ClientSize { width: 32, height: 48 })
    );
    assert!(state.space.element_location(&window).is_none());
}

#[test]
fn floating_maximize_and_restore_work_in_both_layouts() {
    if !crate::startup_tests::private_runtime(
        "state::window_lifecycle_tests::floating_maximize_and_restore_work_in_both_layouts",
    ) {
        return;
    }

    let (mut events, mut state, _) = fixture();
    let (window, mut wire) = window(&mut state, &mut events, 0xff112233);
    let id = state.windows.ids()[&window];
    state.focused_window = Some(id);
    state.toggle_focused_floating();
    state.set_floating_window_geometry(&window, (120, 100).into(), (64, 48).into());
    state.display_handle.flush_clients().unwrap();
    ack_configure(&mut wire, 3);
    request(&mut wire, 2, 6, &[], None);
    dispatch(&mut events, &mut state);
    let normal = match state.workspaces.placement(id).unwrap() {
        WindowPlacement::Floating { rect } => rect,
        _ => panic!("expected floating window"),
    };

    let mut next_object = 7;
    for mode in [ferese_core::LayoutMode::Scrolling, ferese_core::LayoutMode::Tree] {
        let bounds = state.output_bounds().unwrap();
        state.workspaces.set_active_layout_mode(mode, bounds).unwrap();
        state.relayout();
        state.set_window_maximized(id, true);
        let maximized = maximized_rect(bounds, state.gap_config.outer);
        assert_eq!(state.windows.geometry(&id).unwrap().logical, maximized, "{mode:?}");
        assert_eq!(
            state.workspaces.placement(id),
            Some(WindowPlacement::Floating { rect: normal })
        );
        state.display_handle.flush_clients().unwrap();
        ack_configure(&mut wire, 3);
        // Commit the configured raster instead of keeping the original tiny buffer.
        use smithay::reexports::wayland_server::protocol::wl_shm::WlShm;
        use std::io::Write;
        use std::os::fd::AsRawFd;
        let client = window.toplevel().unwrap().wl_surface().client().unwrap();
        let shm = client
            .create_resource::<WlShm, (), Ferese>(&state.display_handle, 1, ())
            .unwrap();
        let (width, height) = (maximized.width.round() as u32, maximized.height.round() as u32);
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(&0xff112233u32.to_ne_bytes().repeat((width * height) as usize))
            .unwrap();
        request(
            &mut wire,
            shm.id().protocol_id(),
            0,
            &[next_object, width * height * 4],
            Some(file.as_raw_fd()),
        );
        request(
            &mut wire,
            next_object,
            0,
            &[next_object + 1, 0, width, height, width * 4, 0],
            None,
        );
        request(&mut wire, 3, 3, &[0, 0, width, height], None);
        request(&mut wire, 2, 1, &[next_object + 1, 0, 0], None);
        next_object += 2;
        request(&mut wire, 2, 6, &[], None);
        dispatch(&mut events, &mut state);
        assert!(
            window
                .toplevel()
                .unwrap()
                .current_state()
                .states
                .contains(xdg_toplevel::State::Maximized)
        );

        state.set_window_maximized(id, false);
        assert_eq!(state.windows.geometry(&id).unwrap().logical, normal, "{mode:?}");
        state.display_handle.flush_clients().unwrap();
        ack_configure(&mut wire, 3);
        request(&mut wire, 3, 3, &[0, 0, 64, 48], None);
        request(&mut wire, 2, 1, &[6, 0, 0], None);
        request(&mut wire, 2, 6, &[], None);
        dispatch(&mut events, &mut state);
        assert!(
            !window
                .toplevel()
                .unwrap()
                .current_state()
                .states
                .contains(xdg_toplevel::State::Maximized)
        );
        assert_eq!(
            state.workspaces.placement(id),
            Some(WindowPlacement::Floating { rect: normal })
        );
    }
}

#[test]
fn malformed_resize_limits_disconnect_only_the_client_and_motion_survives() {
    use std::io::Read;
    if !crate::startup_tests::private_runtime(
        "state::window_lifecycle_tests::malformed_resize_limits_disconnect_only_the_client_and_motion_survives",
    ) {
        return;
    }

    let (mut events, mut state, _) = fixture();
    for (minimum, maximum) in [
        ((800, 0), (400, 0)),
        ((0, 800), (0, 400)),
        ((-1, 0), (0, 0)),
        ((0, 0), (-1, 0)),
        ((0, -1), (0, 0)),
        ((0, 0), (0, -1)),
    ] {
        let (window, mut wire) = window(&mut state, &mut events, 0xff112233);
        let id = state.windows.ids()[&window];
        state.focused_window = Some(id);
        state.toggle_focused_floating();
        let pointer = state.seat.get_pointer().unwrap();
        pointer.set_grab(
            &mut state,
            crate::grabs::ResizeSurfaceGrab::new(
                GrabStartData {
                    focus: None,
                    button: 0x110,
                    location: (100.0, 100.0).into(),
                },
                window,
                xdg_toplevel::ResizeEdge::BottomRight.into(),
                Rectangle::new((100, 100).into(), (64, 48).into()),
            ),
            SERIAL_COUNTER.next_serial(),
            Focus::Clear,
        );
        request(&mut wire, 4, 8, &[minimum.0 as u32, minimum.1 as u32], None);
        request(&mut wire, 4, 7, &[maximum.0 as u32, maximum.1 as u32], None);
        request(&mut wire, 2, 6, &[], None);
        dispatch(&mut events, &mut state);
        pointer.motion(
            &mut state,
            None,
            &MotionEvent {
                location: (120.0, 130.0).into(),
                serial: SERIAL_COUNTER.next_serial(),
                time: 1,
            },
        );
        pointer.unset_grab(&mut state, SERIAL_COUNTER.next_serial(), 2);
        wire.set_nonblocking(true).unwrap();
        let mut messages = Vec::new();
        let _ = wire.read_to_end(&mut messages);
        let mut offset = 0;
        let mut rejected = false;
        while offset + 8 <= messages.len() {
            let object = u32::from_ne_bytes(messages[offset..offset + 4].try_into().unwrap());
            let header = u32::from_ne_bytes(messages[offset + 4..offset + 8].try_into().unwrap());
            if object == 1 && header & 0xffff == 0 {
                assert_eq!(
                    u32::from_ne_bytes(messages[offset + 12..offset + 16].try_into().unwrap()),
                    xdg_toplevel::Error::InvalidSize as u32
                );
                rejected = true;
            }

            offset += (header >> 16) as usize;
        }
        assert!(rejected, "invalid limits {minimum:?}/{maximum:?} were not rejected");
    }

    let (healthy, _wire) = window(&mut state, &mut events, 0xff112233);
    assert!(state.windows.ids().contains_key(&healthy));
}
