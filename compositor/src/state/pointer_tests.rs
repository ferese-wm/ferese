use super::capture_privacy::tests::window;
use super::window_lifecycle_tests::fixture;
use super::*;
use smithay::input::pointer::{GrabStartData, MotionEvent};
use smithay::utils::SERIAL_COUNTER;

fn pointer_interaction_begin(
    state: &mut Ferese,
    window: Window,
    resize: bool,
) -> smithay::input::pointer::PointerHandle<Ferese> {
    use smithay::backend::input::{ButtonState, InputTime};
    let pointer = state.seat.get_pointer().unwrap();
    let rect = state.visual_rect_for_window(&window).unwrap();
    let location = (
        rect.loc.x as f64 + rect.size.w as f64 / 2.0,
        rect.loc.y as f64 + rect.size.h as f64 / 2.0,
    )
        .into();
    assert!(state.begin_window_interaction(
        &pointer,
        window,
        GrabStartData {
            focus: None,
            button: 0x110,
            location
        },
        SERIAL_COUNTER.next_serial(),
        resize.then(|| xdg_toplevel::ResizeEdge::BottomRight.into())
    ));
    pointer.button(
        state,
        &smithay::input::pointer::ButtonEvent {
            button: 0x110,
            state: ButtonState::Pressed,
            serial: SERIAL_COUNTER.next_serial(),
            time: InputTime::from_millis(1),
        },
    );
    pointer
}

#[test]
fn pointer_drag_keeps_layout_until_drop_and_escape_preserves_new_windows() {
    if !crate::startup_tests::private_runtime(
        "state::pointer_tests::pointer_drag_keeps_layout_until_drop_and_escape_preserves_new_windows",
    ) {
        return;
    }
    let (mut events, mut state, _) = fixture();
    let (first, _wire) = window(&mut state, &mut events, 0xff112233);
    let first_id = state.windows.ids()[&first];
    let before = state.workspaces.active().layout.clone();
    let pointer = pointer_interaction_begin(&mut state, first, false);
    pointer.motion(
        &mut state,
        None,
        &MotionEvent {
            location: (700.0, 300.0).into(),
            serial: SERIAL_COUNTER.next_serial(),
            time: smithay::backend::input::InputTime::from_millis(2),
        },
    );
    state.poll_window_interaction();
    assert_eq!(state.workspaces.active().layout, before);
    assert!(state.pointer_operation.as_ref().unwrap().visual.is_some());
    let (second, _wire2) = window(&mut state, &mut events, 0xff445566);
    let second_id = state.windows.ids()[&second];
    state.cancel_window_interaction();
    assert!(state.pointer_operation.is_none());
    assert!(!pointer.is_grabbed());
    assert!(state.workspaces.active().layout.contains(first_id));
    assert!(state.workspaces.active().layout.contains(second_id));
    state.workspaces.validate().unwrap();
}

#[test]
fn pointer_resize_cancels_cleanly_and_finishes_on_its_own_button() {
    use smithay::backend::input::{ButtonState, InputTime};
    if !crate::startup_tests::private_runtime(
        "state::pointer_tests::pointer_resize_cancels_cleanly_and_finishes_on_its_own_button",
    ) {
        return;
    }
    let (mut events, mut state, _) = fixture();
    let (first, _wire) = window(&mut state, &mut events, 0xff112233);
    let id = state.windows.ids()[&first];
    state.focused_window = Some(id);
    state.toggle_focused_floating();
    let before = state.workspaces.placement(id);
    let pointer = pointer_interaction_begin(&mut state, first.clone(), true);
    pointer.motion(
        &mut state,
        None,
        &MotionEvent {
            location: (700.0, 500.0).into(),
            serial: SERIAL_COUNTER.next_serial(),
            time: InputTime::from_millis(2),
        },
    );
    state.poll_window_interaction();
    assert_ne!(state.workspaces.placement(id), before);
    state.cancel_window_interaction();
    assert_eq!(state.workspaces.placement(id), before);
    let pointer = pointer_interaction_begin(&mut state, first, false);
    for (button, pressed) in [(0x111, true), (0x110, false)] {
        pointer.button(
            &mut state,
            &smithay::input::pointer::ButtonEvent {
                button,
                state: if pressed {
                    ButtonState::Pressed
                } else {
                    ButtonState::Released
                },
                serial: SERIAL_COUNTER.next_serial(),
                time: InputTime::from_millis(3),
            },
        );
    }
    state.poll_window_interaction();
    assert!(state.pointer_operation.is_none());
    assert!(
        pointer.is_grabbed(),
        "remaining button is drained without moving the window"
    );
    pointer.button(
        &mut state,
        &smithay::input::pointer::ButtonEvent {
            button: 0x111,
            state: ButtonState::Released,
            serial: SERIAL_COUNTER.next_serial(),
            time: InputTime::from_millis(4),
        },
    );
    assert!(!pointer.is_grabbed());
}

fn motion(state: &mut Ferese, point: (f64, f64)) {
    let pointer = state.seat.get_pointer().unwrap();
    pointer.motion(
        state,
        None,
        &MotionEvent {
            location: point.into(),
            serial: SERIAL_COUNTER.next_serial(),
            time: smithay::backend::input::InputTime::from_millis(2),
        },
    );
    state.poll_window_interaction();
}

fn button(state: &mut Ferese, code: u32, pressed: bool) {
    let pointer = state.seat.get_pointer().unwrap();
    pointer.button(
        state,
        &smithay::input::pointer::ButtonEvent {
            button: code,
            state: if pressed {
                smithay::backend::input::ButtonState::Pressed
            } else {
                smithay::backend::input::ButtonState::Released
            },
            serial: SERIAL_COUNTER.next_serial(),
            time: smithay::backend::input::InputTime::from_millis(3),
        },
    );
    state.poll_window_interaction();
}

#[test]
fn pointer_drop_joins_tabs_and_can_float_and_retile_in_both_layouts() {
    if !crate::startup_tests::private_runtime(
        "state::pointer_tests::pointer_drop_joins_tabs_and_can_float_and_retile_in_both_layouts",
    ) {
        return;
    }
    for tree in [false, true] {
        let (mut events, mut state, _) = fixture();
        let (first, _wire1) = window(&mut state, &mut events, 0xff112233);
        let (second, _wire2) = window(&mut state, &mut events, 0xff445566);
        let a = state.windows.ids()[&first];
        let b = state.windows.ids()[&second];
        if tree {
            let mut layout = ferese_layout::LayoutTree::default();
            layout.insert(a, None, Axis::Horizontal, 0.5).unwrap();
            layout.insert(b, Some(a), Axis::Horizontal, 0.5).unwrap();
            state.workspaces.active_mut().layout = WorkspaceLayout::Tree(layout);
            state.relayout();
        }
        let target = state.visual_rect_for_window(&second).unwrap();
        pointer_interaction_begin(&mut state, first.clone(), false);
        motion(
            &mut state,
            (target.loc.x as f64 + target.size.w as f64 / 2.0, 250.0),
        );
        button(&mut state, 0x110, false);
        let group = state.workspaces.active().layout.tab_group(a).unwrap();
        assert!(group.windows.contains(&b));
        assert_eq!(group.active, a);
        assert!(state.space.element_location(&second).is_none());

        pointer_interaction_begin(&mut state, first.clone(), false);
        button(&mut state, 0x111, true);
        motion(&mut state, (200.0, 180.0));
        button(&mut state, 0x111, false);
        button(&mut state, 0x110, false);
        assert!(matches!(
            state.workspaces.placement(a),
            Some(WindowPlacement::Floating { .. })
        ));
        assert!(state.space.element_location(&second).is_some());

        let target = state.visual_rect_for_window(&second).unwrap();
        pointer_interaction_begin(&mut state, first.clone(), false);
        button(&mut state, 0x111, true);
        motion(&mut state, (target.loc.x as f64 + 10.0, 200.0));
        button(&mut state, 0x111, false);
        button(&mut state, 0x110, false);
        assert_eq!(state.workspaces.placement(a), Some(WindowPlacement::Tiled));
        assert!(state.workspaces.active().layout.tab_group(a).is_none());
        state.workspaces.validate().unwrap();
    }
}

#[test]
fn pointer_drop_crosses_negative_rotated_scaled_output_and_preserves_visual_origin() {
    if !crate::startup_tests::private_runtime(
        "state::pointer_tests::pointer_drop_crosses_negative_rotated_scaled_output_and_preserves_visual_origin",
    ) {
        return;
    }
    let (mut events, mut state, first_output) = fixture();
    let second = Output::new("second".into(), first_output.physical_properties());
    second.change_current_state(
        first_output.current_mode(),
        Some(smithay::utils::Transform::_90),
        Some(smithay::output::Scale::Fractional(1.5)),
        None,
    );
    state.space.map_output(&second, (-400, -150));
    state.register_output(&second, "second".into());
    let unaffected = Output::new("unaffected".into(), first_output.physical_properties());
    unaffected.change_current_state(first_output.current_mode(), None, None, None);
    state.space.map_output(&unaffected, (1600, 0));
    state.register_output(&unaffected, "unaffected".into());
    let (window, _wire) = window(&mut state, &mut events, 0xff112233);
    let id = state.windows.ids()[&window];
    state.animations_enabled = true;
    pointer_interaction_begin(&mut state, window.clone(), false);
    state.output_redraw_pending.clear();
    motion(&mut state, (-200.0, 100.0));
    assert!(
        !state.output_redraw_pending.contains(&unaffected),
        "dragging must not redraw an unrelated output"
    );
    let dragged = state.pointer_operation.as_ref().unwrap().visual.unwrap();
    let frame = state.sample_frame(&second, Duration::ZERO);
    assert!(
        frame.windows.contains_key(&id),
        "dragged window must be sampled on the destination output"
    );
    assert_eq!(frame.windows[&id].presentation.bounds.current, dragged);
    button(&mut state, 0x110, false);
    assert_eq!(
        state.workspaces.workspace_for_window(id),
        state
            .output_id(&second)
            .and_then(|id| state.output_workspaces.active_workspace(id))
    );
    assert_eq!(
        state.windows.geometry(&id).unwrap().visual.current,
        dragged,
        "landing must animate from the pointer, not the old tile"
    );
    let pointer = state.seat.get_pointer().unwrap();
    assert_eq!(
        pointer.current_focus(),
        state
            .surface_under(pointer.current_location())
            .map(|(surface, _)| surface)
    );
    state.workspaces.validate().unwrap();
}

#[test]
fn pointer_grab_cancels_on_reload_output_loss_and_unmap() {
    if !crate::startup_tests::private_runtime(
        "state::pointer_tests::pointer_grab_cancels_on_reload_output_loss_and_unmap",
    ) {
        return;
    }
    for reason in ["reload", "output", "unmap", "overview", "escape"] {
        let (mut events, mut state, output) = fixture();
        let (window, _wire) = window(&mut state, &mut events, 0xff112233);
        let pointer = pointer_interaction_begin(&mut state, window.clone(), false);
        motion(&mut state, (700.0, 300.0));
        match reason {
            "reload" => {
                state.reload_config_source("input { touchpad { accel-speed 0.4; disable-while-mouse-connected #true; } }".into()).unwrap();
            }
            "output" => state.unregister_output(&output),
            "unmap" => state.remove_tiled_window(&window),
            "overview" => state.set_overview_active(true),
            "escape" => {
                crate::input::binding_mode_tests::key(&mut state, 9, true);
                crate::input::binding_mode_tests::key(&mut state, 9, false);
            }
            _ => unreachable!(),
        }
        state.poll_window_interaction();
        assert!(state.pointer_operation.is_none(), "{reason}");
        assert!(!pointer.is_grabbed(), "{reason}");
        state.workspaces.validate().unwrap();
        if reason == "reload" {
            assert_eq!(state.input_settings.touchpad.accel_speed, 0.4);
            assert!(state.input_settings.touchpad.disable_while_mouse_connected);
        }
    }
}

#[test]
fn pointer_tiled_resize_rolls_back_without_losing_concurrent_layout_changes() {
    if !crate::startup_tests::private_runtime(
        "state::pointer_tests::pointer_tiled_resize_rolls_back_without_losing_concurrent_layout_changes",
    ) {
        return;
    }
    let (mut events, mut state, _) = fixture();
    let (first, _wire1) = window(&mut state, &mut events, 0xff112233);
    let (second, _wire2) = window(&mut state, &mut events, 0xff445566);
    let a = state.windows.ids()[&first];
    let before = state.workspaces.active().layout.clone();
    pointer_interaction_begin(&mut state, first.clone(), true);
    motion(&mut state, (750.0, 500.0));
    assert_ne!(state.workspaces.active().layout, before);
    state.cancel_window_interaction();
    assert_eq!(
        state
            .workspaces
            .active()
            .layout
            .geometry(Rect::new(0., 0., 800., 600.))
            .unwrap(),
        before.geometry(Rect::new(0., 0., 800., 600.)).unwrap()
    );
    pointer_interaction_begin(&mut state, first, true);
    motion(&mut state, (750.0, 500.0));
    let (third, _wire3) = window(&mut state, &mut events, 0xff778899);
    state.poll_window_interaction();
    assert!(state.pointer_operation.is_none());
    for id in [a, state.windows.ids()[&second], state.windows.ids()[&third]] {
        assert!(state.workspaces.active().layout.contains(id));
    }
    state.workspaces.validate().unwrap();
}

#[test]
fn pointer_fullscreen_and_maximize_preserve_restore_geometry_and_zero_threshold_is_safe() {
    if !crate::startup_tests::private_runtime(
        "state::pointer_tests::pointer_fullscreen_and_maximize_preserve_restore_geometry_and_zero_threshold_is_safe",
    ) {
        return;
    }
    let (mut events, mut state, _) = fixture();
    let (window, _wire) = window(&mut state, &mut events, 0xff112233);
    let id = state.windows.ids()[&window];
    state.focused_window = Some(id);
    state.toggle_focused_floating();
    state.set_floating_window_geometry(&window, (200, 150).into(), (300, 200).into());
    let original = state.workspaces.placement(id);
    state.set_window_fullscreen(id, true);
    let pointer = state.seat.get_pointer().unwrap();
    assert!(!state.begin_window_interaction(
        &pointer,
        window.clone(),
        GrabStartData {
            focus: None,
            button: 0x110,
            location: (300.0, 200.0).into()
        },
        SERIAL_COUNTER.next_serial(),
        None
    ));
    state.set_window_fullscreen(id, false);
    state.animations_enabled = true;
    state.set_window_maximized(id, true);
    pointer_interaction_begin(&mut state, window.clone(), false);
    motion(&mut state, (500.0, 350.0));
    let visual = state.pointer_operation.as_ref().unwrap().visual.unwrap();
    assert_eq!((visual.width, visual.height), (300.0, 200.0));
    state.cancel_window_interaction();
    assert!(state.windows.record(id).unwrap().maximized);
    assert_eq!(state.workspaces.placement(id), original);
    state.input_settings.mouse.drag_threshold = 0.0;
    pointer_interaction_begin(&mut state, window, false);
    button(&mut state, 0x111, true);
    button(&mut state, 0x111, false);
    button(&mut state, 0x110, false);
    assert!(state.windows.record(id).unwrap().maximized);
    assert_eq!(state.workspaces.placement(id), original);
}

#[test]
fn pointer_scroll_reaches_hidden_columns_without_changing_window_membership() {
    if !crate::startup_tests::private_runtime(
        "state::pointer_tests::pointer_scroll_reaches_hidden_columns_without_changing_window_membership",
    ) {
        return;
    }
    let (mut events, mut state, _) = fixture();
    let clients = (0..5)
        .map(|_| window(&mut state, &mut events, 0xff112233))
        .collect::<Vec<_>>();
    let before = state.workspaces.active().layout.viewport_x().unwrap();
    assert!(before > 0.0);
    let window = clients.last().unwrap().0.clone();
    let pointer = pointer_interaction_begin(&mut state, window, false);
    motion(&mut state, (400.0, 200.0));
    pointer.axis(
        &mut state,
        smithay::input::pointer::AxisFrame::new(smithay::backend::input::InputTime::from_millis(4))
            .value(smithay::backend::input::Axis::Vertical, -200.0),
    );
    state.poll_window_interaction();
    assert!(state.workspaces.active().layout.viewport_x().unwrap() < before);
    assert_eq!(state.workspaces.active().layout.window_ids().count(), 5);
    state.cancel_window_interaction();
    state.workspaces.validate().unwrap();
}

#[test]
fn pointer_lock_cancels_drag_and_releases_buttons_before_lock_surfaces_receive_input() {
    use smithay::reexports::wayland_server::Resource;
    use smithay::reexports::wayland_protocols::ext::session_lock::v1::server::ext_session_lock_manager_v1::ExtSessionLockManagerV1;
    if !crate::startup_tests::private_runtime(
        "state::pointer_tests::pointer_lock_cancels_drag_and_releases_buttons_before_lock_surfaces_receive_input",
    ) {
        return;
    }
    let (mut events, mut state, _) = fixture();
    let (window, _window_wire) = window(&mut state, &mut events, 0xff112233);
    let (server, mut wire) = std::os::unix::net::UnixStream::pair().unwrap();
    let client = state
        .display_handle
        .insert_client(server, std::sync::Arc::new(ClientState::default()))
        .unwrap();
    let pointer = pointer_interaction_begin(&mut state, window, false);
    motion(&mut state, (700.0, 200.0));
    let manager = client
        .create_resource::<ExtSessionLockManagerV1, smithay::wayland::GlobalData, Ferese>(
            &state.display_handle,
            1,
            smithay::wayland::GlobalData,
        )
        .unwrap();
    super::capture_privacy::tests::request(&mut wire, manager.id().protocol_id(), 1, &[2], None);
    for _ in 0..20 {
        super::capture_privacy::tests::dispatch(&mut events, &mut state);
        if state.session_lock.active() {
            break;
        }
    }
    state.poll_window_interaction();
    assert!(state.session_lock.active());
    assert!(state.pointer_operation.is_none());
    assert!(!pointer.is_grabbed());
    assert!(state.consumed_pointer_buttons.contains(&0x110));
    assert_eq!(pointer.current_focus(), None);
}

#[test]
fn pointer_floating_resize_keeps_edge_snapping_and_shift_bypass() {
    if !crate::startup_tests::private_runtime(
        "state::pointer_tests::pointer_floating_resize_keeps_edge_snapping_and_shift_bypass",
    ) {
        return;
    }
    let (mut events, mut state, _) = fixture();
    let (window, _wire) = window(&mut state, &mut events, 0xff112233);
    let id = state.windows.ids()[&window];
    state.focused_window = Some(id);
    state.toggle_focused_floating();
    state.set_floating_window_geometry(&window, (100, 100).into(), (200, 200).into());
    pointer_interaction_begin(&mut state, window, true);
    // The helper starts at (200, 200). A 493px delta puts the right edge 7px
    // from the output edge, inside the existing 10px snap distance.
    motion(&mut state, (693.0, 210.0));
    let Some(WindowPlacement::Floating { rect }) = state.workspaces.placement(id) else {
        panic!("floating window");
    };
    assert_eq!(rect.x + rect.width, 800.0);
    crate::input::binding_mode_tests::key(&mut state, 50, true);
    motion(&mut state, (694.0, 210.0));
    let Some(WindowPlacement::Floating { rect }) = state.workspaces.placement(id) else {
        panic!("floating window");
    };
    assert_eq!(rect.x + rect.width, 794.0);
    crate::input::binding_mode_tests::key(&mut state, 50, false);
    state.cancel_window_interaction();
}

#[test]
fn pointer_scrolling_left_resize_anchors_the_right_edge_and_cancels() {
    if !crate::startup_tests::private_runtime(
        "state::pointer_tests::pointer_scrolling_left_resize_anchors_the_right_edge_and_cancels",
    ) {
        return;
    }
    let (mut events, mut state, _) = fixture();
    let (window, _wire) = window(&mut state, &mut events, 0xff112233);
    let id = state.windows.ids()[&window];
    let before = state.windows.geometry(&id).unwrap().logical;
    let pointer = state.seat.get_pointer().unwrap();
    let start = Point::from((before.x, before.y + before.height / 2.0));
    assert!(state.begin_window_interaction(
        &pointer,
        window,
        GrabStartData {
            focus: None,
            button: 0x111,
            location: start,
        },
        SERIAL_COUNTER.next_serial(),
        Some(xdg_toplevel::ResizeEdge::Left.into())
    ));
    button(&mut state, 0x111, true);
    motion(&mut state, (start.x + 100.0, start.y));
    let after = state.windows.geometry(&id).unwrap().logical;
    assert_eq!(
        after.x + after.width,
        before.x + before.width,
        "dragging the left edge moved the opposite edge: {before:?} -> {after:?}"
    );
    assert_eq!(after.x, before.x + 100.0);
    state.cancel_window_interaction();
    assert!(!pointer.is_grabbed());
    assert_eq!(state.windows.geometry(&id).unwrap().logical, before);
}
