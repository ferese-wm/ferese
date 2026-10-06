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
fn late_keymap_failure_preserves_capture_sessions_and_runtime_settings() {
    if !crate::startup_tests::private_runtime(
        "state::window_lifecycle_tests::late_keymap_failure_preserves_capture_sessions_and_runtime_settings",
    ) {
        return;
    }

    let (mut events, mut state, _) = fixture();
    let (window, _wire) = window(&mut state, &mut events, 0xff112233);
    let focus = state.focused_window;
    let session = state.input_capture.register(42, 1).unwrap();
    state.input_capture.enable(session).unwrap();
    let previous_layout = state.input_settings.xkb_layout.clone();
    let previous_repeat = state.input_settings.repeat_rate;
    let mut runtime = crate::config::Config::default().runtime_config().unwrap();
    // Preparation succeeded; force the real late application failure.
    runtime.input_settings.xkb_layout = "ferese-nonexistent-layout-for-regression".into();
    runtime.input_settings.repeat_rate = previous_repeat + 1;
    let result = state.apply_runtime_config(runtime, &serde_json::json!({"input": "changed"}));
    assert!(result.unwrap_err().contains("keymap reload failed"));
    assert!(state.input_capture.has_sessions());
    state.input_capture.enable(session).unwrap();
    assert!(!state.input_capture.restore_focus);
    assert_eq!(state.focused_window, focus);
    assert_eq!(state.input_settings.xkb_layout, previous_layout);
    assert_eq!(state.input_settings.repeat_rate, previous_repeat);
    assert!(state.windows.ids().contains_key(&window));
}

#[test]
fn dmabuf_validation_uses_the_originating_global_without_a_desktop_frame() {
    use smithay::backend::allocator::{Buffer, Format, Fourcc, Modifier};
    use std::io::Read;
    use std::os::fd::AsRawFd;
    use std::os::unix::net::UnixStream;
    use std::sync::Arc;

    if !crate::startup_tests::private_runtime(
        "state::window_lifecycle_tests::dmabuf_validation_uses_the_originating_global_without_a_desktop_frame",
    ) {
        return;
    }

    let mut events = smithay::reexports::calloop::EventLoop::try_new().unwrap();
    let mut state = crate::startup_tests::state(&mut events);
    let formats = [Format {
        code: Fourcc::Argb8888,
        modifier: Modifier::Linear,
    }];
    let a = state
        .dmabuf_state
        .create_global::<Ferese>(&state.display_handle, formats);
    state.dmabuf_imports.register(a);
    let (server, mut wire) = UnixStream::pair().unwrap();
    let client = state
        .display_handle
        .insert_client(server, Arc::new(ClientState::default()))
        .unwrap();
    request(&mut wire, 1, 1, &[2], None);
    dispatch(&mut events, &mut state);
    let read = |wire: &mut UnixStream| {
        wire.set_nonblocking(true).unwrap();
        let mut bytes = Vec::new();
        let _ = wire.read_to_end(&mut bytes);
        wire.set_nonblocking(false).unwrap();
        bytes
    };
    let mut registry = read(&mut wire);
    // Register B after the initial registry roundtrip so notification order
    // identifies A and B without depending on backend map iteration order.
    let b = state
        .dmabuf_state
        .create_global::<Ferese>(&state.display_handle, formats);
    state.dmabuf_imports.register(b);
    state.display_handle.flush_clients().unwrap();
    registry.extend(read(&mut wire));
    let mut offset = 0;
    let mut names = Vec::new();
    while offset < registry.len() {
        let header = u32::from_ne_bytes(registry[offset + 4..offset + 8].try_into().unwrap());
        let length = (header >> 16) as usize;
        let message = &registry[offset..offset + length];
        if message
            .windows(b"zwp_linux_dmabuf_v1\0".len())
            .any(|text| text == b"zwp_linux_dmabuf_v1\0")
        {
            names.push(u32::from_ne_bytes(message[8..12].try_into().unwrap()));
        }
        offset += length;
    }
    assert_eq!(names.len(), 2);
    let interface = b"zwp_linux_dmabuf_v1\0";
    for (name, id) in names.into_iter().zip([3, 4]) {
        let mut args = vec![name, interface.len() as u32];
        let mut padded = interface.to_vec();
        padded.resize(padded.len().next_multiple_of(4), 0);
        args.extend(
            padded
                .chunks_exact(4)
                .map(|word| u32::from_ne_bytes(word.try_into().unwrap())),
        );
        args.extend([3, id]);
        request(&mut wire, 2, 0, &args, None);
    }
    dispatch(&mut events, &mut state);
    read(&mut wire);
    let file = tempfile::tempfile().unwrap();
    file.set_len(64).unwrap();
    // Both asynchronous and immediate imports must survive the wrong GPU
    // being visited first. The mock capability accepts only B's buffers.
    for (params, buffer) in [(5, None), (6, Some(7))] {
        request(&mut wire, 4, 1, &[params], None);
        request(&mut wire, params, 1, &[0, 0, 16, 0, 0], Some(file.as_raw_fd()));
        let mut args = vec![4, 4, Fourcc::Argb8888 as u32, 0];
        if let Some(buffer) = buffer {
            args.insert(0, buffer);
        }
        request(&mut wire, params, if buffer.is_some() { 3 } else { 2 }, &args, None);
        dispatch(&mut events, &mut state);
        assert!(state.space.outputs().next().is_none());
        state.process_pending_dmabuf_imports();
        assert!(
            read(&mut wire).is_empty(),
            "unavailable renderer must defer notification"
        );
        state
            .dmabuf_imports
            .process_with(a, None, |_| panic!("B's request reached A's renderer"));
        let mut calls = 0;
        state.dmabuf_imports.process_with(b, None, |dmabuf| {
            calls += 1;
            assert_eq!(dmabuf.size(), (4, 4).into());
            true
        });
        assert_eq!(calls, 1);
        state.display_handle.flush_clients().unwrap();
        let response = read(&mut wire);
        if buffer.is_none() {
            assert_eq!(u32::from_ne_bytes(response[..4].try_into().unwrap()), params);
            assert_eq!(u32::from_ne_bytes(response[4..8].try_into().unwrap()) & 0xffff, 0);
        } else {
            assert!(response.is_empty(), "create_immed success must not emit a failed event");
        }
        assert!(client.get_credentials(&state.display_handle).is_ok());
    }

    // Deferred requests receive one terminal failure on timeout or removal.
    for params in [8, 9] {
        request(&mut wire, 4, 1, &[params], None);
        request(&mut wire, params, 1, &[0, 0, 16, 0, 0], Some(file.as_raw_fd()));
        request(&mut wire, params, 2, &[4, 4, Fourcc::Argb8888 as u32, 0], None);
        dispatch(&mut events, &mut state);
        state.process_pending_dmabuf_imports();
        if params == 8 {
            state.dmabuf_imports.expire(Instant::now() + Duration::from_secs(5));
        } else {
            state.dmabuf_imports.remove(b);
        }

        state.display_handle.flush_clients().unwrap();
        let response = read(&mut wire);
        assert_eq!(response.len(), 8);
        assert_eq!(u32::from_ne_bytes(response[..4].try_into().unwrap()), params);
        assert_eq!(u32::from_ne_bytes(response[4..8].try_into().unwrap()) & 0xffff, 1);
        state
            .dmabuf_imports
            .process_with(b, None, |_| panic!("failed request survived"));
        assert!(client.get_credentials(&state.display_handle).is_ok());
    }

    state.dmabuf_imports.register(b);
    request(&mut wire, 4, 1, &[10], None);
    request(&mut wire, 10, 1, &[0, 0, 16, 0, 0], Some(file.as_raw_fd()));
    request(&mut wire, 10, 2, &[4, 4, Fourcc::Argb8888 as u32, 0], None);
    dispatch(&mut events, &mut state);
    drop(wire);
    dispatch(&mut events, &mut state);
    state.process_pending_dmabuf_imports();
    state
        .dmabuf_imports
        .process_with(b, None, |_| panic!("disconnected client's request survived"));
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

#[test]
fn window_rule_minimum_size_replaces_the_client_advertised_minimum() {
    if !crate::startup_tests::private_runtime(
        "state::window_lifecycle_tests::window_rule_minimum_size_replaces_the_client_advertised_minimum",
    ) {
        return;
    }

    let (mut events, mut state, _) = fixture();
    let (window, mut wire) = window(&mut state, &mut events, 0xff112233);
    let id = state.windows.ids()[&window];
    request(&mut wire, 4, 8, &[800, 600], None);
    request(&mut wire, 2, 6, &[], None);
    dispatch(&mut events, &mut state);
    assert_eq!(state.effective_minimum_size(&window, (800, 600)), (800, 600));
    assert_eq!(state.window_constraints()[&id].min_width, 800.0);
    assert_eq!(state.window_constraints()[&id].min_height, 600.0);

    state.window_rules =
        crate::config::Config::parse_source("window-rule transient=#false min-width=500 min-height=400\n")
            .unwrap()
            .window_rules()
            .unwrap();
    assert_eq!(state.effective_minimum_size(&window, (800, 600)), (500, 400));
    let constraints = state.window_constraints();
    assert_eq!(constraints[&id].min_width, 500.0);
    assert_eq!(constraints[&id].min_height, 400.0);

    state.window_rules = crate::config::Config::parse_source("window-rule transient=#false min-height=400\n")
        .unwrap()
        .window_rules()
        .unwrap();
    assert_eq!(state.effective_minimum_size(&window, (800, 600)), (800, 400));

    state.window_rules.clear();
    assert_eq!(state.effective_minimum_size(&window, (800, 600)), (800, 600));
}
