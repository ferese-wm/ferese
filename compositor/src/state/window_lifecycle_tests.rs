use super::capture_privacy::tests::{ack_configure, dispatch, request, window};
use super::*;
use smithay::input::pointer::{GrabStartData, MotionEvent};
use smithay::reexports::wayland_server::Resource;
use smithay::utils::SERIAL_COUNTER;

pub(super) fn fixture() -> (
    smithay::reexports::calloop::EventLoop<'static, Ferese>,
    Ferese,
    Output,
) {
    let mut events = smithay::reexports::calloop::EventLoop::try_new().unwrap();
    let mut state = crate::startup_tests::state(&mut events);
    state.animations_enabled = false;
    let output = Output::new(
        "test".into(),
        smithay::output::PhysicalProperties {
            serial_number: String::new(),
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

fn set_title(
    events: &mut smithay::reexports::calloop::EventLoop<'static, Ferese>,
    state: &mut Ferese,
    wire: &mut std::os::unix::net::UnixStream,
    title: &str,
) {
    let mut text = title.as_bytes().to_vec();
    text.push(0);
    let length = text.len() as u32;
    text.resize(text.len().next_multiple_of(4), 0);
    let mut args = vec![length];
    args.extend(
        text.chunks_exact(4)
            .map(|bytes| u32::from_ne_bytes(bytes.try_into().unwrap())),
    );
    // The window fixture creates its xdg_toplevel as object 4; set_title is opcode 2.
    request(wire, 4, 2, &args, None);
    dispatch(events, state);
}

#[test]
fn title_only_changes_do_not_schedule_plain_window_redraws() {
    if !crate::startup_tests::private_runtime(
        "state::window_lifecycle_tests::title_only_changes_do_not_schedule_plain_window_redraws",
    ) {
        return;
    }
    let (mut events, mut state, _output) = fixture();
    let (app, mut wire) = window(&mut state, &mut events, 0xff112233);
    let id = state.windows.ids()[&app];
    state.output_redraw_pending.clear();
    for number in 0..20 {
        let title = format!("spinner {number}");
        set_title(&mut events, &mut state, &mut wire, &title);
        assert_eq!(state.windows.record(id).unwrap().title, title);
        assert!(
            state.output_redraw_pending.is_empty(),
            "metadata alone must not invalidate the output or its capture scene"
        );
    }
}

#[test]
fn title_changes_redraw_tab_headers_and_overview_labels() {
    if !crate::startup_tests::private_runtime(
        "state::window_lifecycle_tests::title_changes_redraw_tab_headers_and_overview_labels",
    ) {
        return;
    }
    let (mut events, mut state, output) = fixture();
    state.toggle_layout_mode();
    let (first, mut first_wire) = window(&mut state, &mut events, 0xff112233);
    let (_second, _second_wire) = window(&mut state, &mut events, 0xff445566);
    state.consume_focused_window();
    let workspace = state.workspaces.active_id();
    assert_eq!(state.visible_tab_bars(workspace).count(), 1);
    assert!(state.space.element_location(&first).is_none());
    state.output_redraw_pending.clear();
    set_title(
        &mut events,
        &mut state,
        &mut first_wire,
        "Inactive tab title",
    );
    assert!(state.output_redraw_pending.contains(&output));

    state.leave_tab();
    state.set_overview_active(true);
    assert!(state.overview.is_presenting());
    state.output_redraw_pending.clear();
    set_title(&mut events, &mut state, &mut first_wire, "Overview label");
    assert!(state.output_redraw_pending.contains(&output));
}

#[test]
fn title_privacy_transitions_still_invalidate_capture_and_output() {
    if !crate::startup_tests::private_runtime(
        "state::window_lifecycle_tests::title_privacy_transitions_still_invalidate_capture_and_output",
    ) {
        return;
    }
    let (mut events, mut state, output) = fixture();
    let (app, mut wire) = window(&mut state, &mut events, 0xff112233);
    let id = state.windows.ids()[&app];
    state
        .reload_config_source(
            "window-rule title=\"Private\" block-out-from-screencasts=#true".into(),
        )
        .unwrap();
    for (title, protected) in [("Private", true), ("Public", false)] {
        let epoch = state.capture_epoch;
        state.output_redraw_pending.clear();
        set_title(&mut events, &mut state, &mut wire, title);
        assert_eq!(state.capture_protected_windows.contains(&id), protected);
        assert!(state.capture_epoch > epoch);
        assert!(state.output_redraw_pending.contains(&output));
    }
}

#[test]
fn tree_resize_does_not_hold_motion_in_an_unchanged_neighboring_branch() {
    if !crate::startup_tests::private_runtime(
        "state::window_lifecycle_tests::tree_resize_does_not_hold_motion_in_an_unchanged_neighboring_branch",
    ) {
        return;
    }
    let (mut events, mut state, output) = fixture();
    state.animation_test_time = Some(Duration::ZERO);
    state.gap_config = GapConfig {
        inner: 10.0,
        outer: 10.0,
        smart: false,
    };
    let mut clients = (0..4)
        .map(|_| window(&mut state, &mut events, 0xff112233))
        .collect::<Vec<_>>();
    let ids = clients
        .iter()
        .map(|(window, _)| state.windows.ids()[window])
        .collect::<Vec<_>>();
    let [a, b, c, d] = ids.as_slice() else {
        unreachable!()
    };
    let (a, b, c, d) = (*a, *b, *c, *d);
    let mut tree = ferese_layout::LayoutTree::default();
    tree.insert(a, None, Axis::Horizontal, 0.5).unwrap();
    tree.insert(c, Some(a), Axis::Horizontal, 0.5).unwrap();
    tree.insert(b, Some(a), Axis::Vertical, 0.5).unwrap();
    tree.insert(d, Some(c), Axis::Vertical, 0.5).unwrap();
    state.workspaces.active_mut().layout = WorkspaceLayout::Tree(tree);
    state.relayout();
    state.animations_enabled = true;
    let resize = |state: &mut Ferese, id| {
        let WorkspaceLayout::Tree(tree) = &mut state.workspaces.active_mut().layout else {
            panic!()
        };
        tree.resize_window(id, Direction::Down, 0.1).unwrap();
        state.relayout();
        state.display_handle.flush_clients().unwrap();
    };
    resize(&mut state, c);
    for (_, wire) in &mut clients[2..] {
        ack_configure(wire, 3);
        request(wire, 2, 1, &[6, 0, 0], None);
        request(wire, 2, 6, &[], None);
    }
    dispatch(&mut events, &mut state);
    for id in [c, d] {
        assert!(state.windows.transaction(&id).is_none());
    }
    state.advance_animations_at(Duration::from_millis(16), Duration::from_millis(16));
    state.advance_animations_at(Duration::from_millis(16), Duration::from_millis(32));
    let right_targets = [c, d].map(|id| state.windows.geometry(&id).unwrap().logical);
    resize(&mut state, a);
    assert_eq!(
        right_targets,
        [c, d].map(|id| state.windows.geometry(&id).unwrap().logical)
    );
    assert!(state.windows.transaction(&a).is_some());
    let before = [a, b, c, d].map(|id| state.windows.geometry(&id).unwrap().visual.current);
    state.advance_animations_at(Duration::from_millis(16), Duration::from_millis(48));
    let after = [a, b, c, d].map(|id| state.windows.geometry(&id).unwrap().visual.current);
    assert_eq!(before[..2], after[..2], "linked left reflow must wait");
    assert_ne!(
        before[2], after[2],
        "right branch must continue its existing motion"
    );
    assert_ne!(before[3], after[3]);
    let frame = state.sample_frame(&output, Duration::from_millis(16));
    assert_eq!(frame.windows[&a].geometry.visual.current, after[0]);
    assert_ne!(frame.windows[&c].geometry.visual.current, after[2]);
    for id in [a, c] {
        let window = state.windows.window(id).unwrap();
        let geometry = state.windows.geometry(&id).unwrap();
        let location = state.space.element_location(window).unwrap();
        assert_eq!(location.x, geometry.visual.current.x.round() as i32);
        assert_eq!(location.y, geometry.visual.current.y.round() as i32);
        let point = (
            geometry.visual.current.x + 5.0,
            geometry.visual.current.y + 5.0,
        )
            .into();
        let (surface, origin) = state.window_surface_under(point).unwrap();
        assert_eq!(surface, *window.toplevel().unwrap().wl_surface());
        assert!((origin.y - geometry.visual.current.y).abs() < 0.001);
    }

    // Acknowledgement does not release the left branch; one committing client
    // cannot release the other tile's linked resize boundary either.
    ack_configure(&mut clients[0].1, 3);
    dispatch(&mut events, &mut state);
    assert!(state.windows.transaction(&a).is_some());
    request(&mut clients[0].1, 2, 1, &[6, 0, 0], None);
    request(&mut clients[0].1, 2, 6, &[], None);
    dispatch(&mut events, &mut state);
    assert!(state.windows.transaction(&a).is_none());
    assert!(
        state
            .presentation_dependencies
            .reflow_blocked(a, &state.windows)
    );
    assert!(
        state
            .presentation_dependencies
            .reflow_blocked(b, &state.windows)
    );
    state.advance_animations_at(Duration::from_millis(284), Duration::from_millis(332));
    assert!(state.windows.transaction(&b).is_none());
    assert_eq!(state.windows.geometry(&a).unwrap().visual.current, after[0]);
    state.advance_animations_at(Duration::from_millis(16), Duration::from_millis(348));
    assert_ne!(state.windows.geometry(&a).unwrap().visual.current, after[0]);

    // Removing the serial owner retires its wait, and switching layouts never
    // carries a tree cohort into the new scrolling coordinate system.
    resize(&mut state, a);
    request(&mut clients[0].1, 2, 1, &[0, 0, 0], None);
    request(&mut clients[0].1, 2, 6, &[], None);
    dispatch(&mut events, &mut state);
    assert!(
        !state
            .presentation_dependencies
            .reflow_blocked(c, &state.windows)
    );
    state
        .workspaces
        .set_active_layout_mode(
            ferese_core::LayoutMode::Scrolling,
            Rect::new(0.0, 0.0, 800.0, 600.0),
        )
        .unwrap();
    state.relayout();
    for id in [c, d] {
        state.windows.clear_transaction(&id);
    }
    state.release_presentation_dependencies();
    for id in [c, d] {
        assert!(
            !state
                .presentation_dependencies
                .reflow_blocked(id, &state.windows)
        );
    }
}

#[test]
fn width_cycle_keeps_stacked_rows_together_through_holds_forecasts_and_reversal() {
    if !crate::startup_tests::private_runtime(
        "state::window_lifecycle_tests::width_cycle_keeps_stacked_rows_together_through_holds_forecasts_and_reversal",
    ) {
        return;
    }
    for animations in [true, false] {
        let (mut events, mut state, output) = fixture();
        state.animation_test_time = Some(Duration::ZERO);
        state.gap_config = GapConfig {
            inner: 10.0,
            outer: 10.0,
            smart: false,
        };
        state.column_width_presets = vec![ColumnWidth::Proportion(0.5), ColumnWidth::Full];
        let WorkspaceLayout::Scrolling(layout) = &mut state.workspaces.active_mut().layout else {
            panic!("expected scrolling layout");
        };
        layout.set_default_width(ColumnWidth::Proportion(0.5));
        layout.set_focus_strategy(ferese_layout::ViewportFocusStrategy::Minimal);
        let (neighbor, _neighbor_wire) = window(&mut state, &mut events, 0xff112233);
        let (first, _first_wire) = window(&mut state, &mut events, 0xff445566);
        let (second, _second_wire) = window(&mut state, &mut events, 0xff778899);
        let a = state.windows.ids()[&first];
        let b = state.windows.ids()[&second];
        state.workspaces.stack_window(b, a).unwrap();
        state.focused_window = Some(state.windows.ids()[&neighbor]);
        state.relayout();
        state.focused_window = Some(b);
        state.relayout();
        let ids = [a, b];
        for id in state.windows.resizing().copied().collect::<Vec<_>>() {
            state.windows.clear_transaction(&id);
        }
        state.release_presentation_dependencies();
        state.advance_animations_at(Duration::ZERO, Duration::ZERO);
        let before = state.windows.geometry(&a).unwrap().visual.current.width;
        assert_eq!(
            state.windows.geometry(&b).unwrap().visual.current.width,
            before
        );
        state.animations_enabled = animations;
        state.spring_config = SpringConfig {
            stiffness: 900.0,
            damping: 65.0,
            ..Default::default()
        };
        state.viewport_spring_config = SpringConfig {
            stiffness: 200.0,
            damping: 30.0,
            ..Default::default()
        };
        state.focused_window = Some(b);
        let workspace = state.workspaces.active_id();
        let viewport_before = state.viewports[&workspace].motion().target;
        // An interrupted width motion must provide one source and velocity for
        // the column, even if a sibling previously used its general spring.
        if animations {
            state.windows.update(a, |record| {
                let mut width = AnimatedValue::new(before);
                width.velocity = 25.0;
                record.coupled_width = Some((workspace, width));
            });
        }
        state.cycle_focused_column_width();
        assert!(state.viewports[&workspace].motion().target > viewport_before);
        for id in ids {
            assert!(state.windows.record(id).unwrap().coupled_width.is_some());
            assert_eq!(state.windows.transaction(&id).is_some(), animations);
        }
        let assert_equal = |state: &Ferese| {
            let first = *state.windows.geometry(&a).unwrap();
            let second = *state.windows.geometry(&b).unwrap();
            assert_eq!(first.visual.current.width, second.visual.current.width);
            assert_eq!(first.visual.velocity.width, second.visual.velocity.width);
            for horizon in [
                Duration::ZERO,
                Duration::from_millis(4),
                Duration::from_millis(16),
            ] {
                let scene = state.sample_frame(&output, horizon);
                assert_eq!(
                    scene.windows[&a].geometry.visual.current.width,
                    scene.windows[&b].geometry.visual.current.width
                );
            }
            assert_eq!(*state.windows.geometry(&a).unwrap(), first);
            assert_eq!(*state.windows.geometry(&b).unwrap(), second);
        };
        assert_equal(&state);
        state.windows.clear_transaction(&a);
        state.release_presentation_dependencies();
        assert_eq!(
            state
                .presentation_dependencies
                .reflow_blocked(a, &state.windows),
            animations
        );
        assert_eq!(
            state
                .presentation_dependencies
                .reflow_blocked(b, &state.windows),
            animations
        );
        for millis in [16, 100, 299, 300, 316, 332] {
            state.advance_animations_at(Duration::from_millis(16), Duration::from_millis(millis));
            assert_equal(&state);
        }
        assert!(
            state.windows.transaction(&b).is_none(),
            "existing deadline releases the last sibling"
        );
        if animations {
            assert!(state.windows.geometry(&a).unwrap().visual.current.width > before);
        }
        // Retarget the same column while its spring is moving.
        state.cycle_focused_column_width();
        assert_equal(&state);
        for id in ids {
            state.windows.clear_transaction(&id);
        }
        state.release_presentation_dependencies();
        for frame in 1..=150 {
            state.advance_animations_at(
                Duration::from_millis(16),
                Duration::from_millis(332 + frame * 16),
            );
            assert_equal(&state);
        }
        assert_eq!(
            state.windows.geometry(&a).unwrap().visual.current.width,
            before
        );
        assert_eq!(
            state.windows.geometry(&b).unwrap().visual.current.width,
            before
        );
    }
}

#[test]
fn floating_placement_reserves_the_full_opening_window_footprint() {
    if !crate::startup_tests::private_runtime(
        "state::window_lifecycle_tests::floating_placement_reserves_the_full_opening_window_footprint",
    ) {
        return;
    }
    let (mut events, mut state, _) = fixture();
    let (window, _wire) = window(&mut state, &mut events, 0xff112233);
    let id = state.windows.ids()[&window];
    state.toggle_focused_floating();
    let bounds = state.output_bounds().unwrap();
    let logical = state.windows.geometry(&id).unwrap().logical;
    state.animations_enabled = true;
    state
        .windows
        .update(id, |record| record.opening = Some(AnimatedValue::new(0.0)));
    let visual = state.presented_window_rect(id).unwrap();
    assert!(visual.width < logical.width);
    let obstacles = state.floating_obstacles(None, bounds);
    assert_eq!(
        obstacles,
        vec![(
            id.0,
            crate::floating::intersection(logical, bounds).unwrap()
        )]
    );
    let placed = crate::floating::min_overlap(
        (80., 80.),
        bounds,
        &[obstacles[0].1],
        (logical.x, logical.y),
    )
    .unwrap();
    assert!(crate::floating::intersection(placed, logical).is_none());
}

fn floating_launch_fixture() -> (
    smithay::reexports::calloop::EventLoop<'static, Ferese>,
    Ferese,
    Output,
    Output,
) {
    let (events, mut state, first) = fixture();
    let second = Output::new("second".into(), first.physical_properties());
    second.change_current_state(first.current_mode(), None, None, None);
    state.space.map_output(&second, (800, 0));
    state.register_output(&second, "second".into());
    state.window_rules = crate::config::Config::parse_source(
        "window-rule app-id=dev.ferese.Settings floating=#true\n",
    )
    .unwrap()
    .window_rules()
    .unwrap();
    (events, state, first, second)
}

#[test]
fn floating_launch_keeps_keyboard_focused_output_when_pointer_is_elsewhere() {
    if !crate::startup_tests::private_runtime(
        "state::window_lifecycle_tests::floating_launch_keeps_keyboard_focused_output_when_pointer_is_elsewhere",
    ) {
        return;
    }
    let (mut events, mut state, first, second) = floating_launch_fixture();
    let pointer = state.seat.get_pointer().unwrap();
    pointer.motion(
        &mut state,
        None,
        &MotionEvent {
            location: (100., 100.).into(),
            serial: SERIAL_COUNTER.next_serial(),
            time: smithay::backend::input::InputTime::from_millis(1),
        },
    );
    let output_id = state.output_ids[&second];
    let destination = state.output_workspaces.active_workspace(output_id).unwrap();
    state.activate_output_workspace(output_id, destination);
    assert_eq!(pointer.current_location(), (100., 100.).into());
    assert_ne!(state.output_ids[&first], output_id);

    let (window, _wire) = super::capture_privacy::tests::window_with_app_id(
        &mut state,
        &mut events,
        0xff112233,
        "dev.ferese.Settings",
    );
    let id = state.windows.ids()[&window];
    assert_eq!(state.output_workspaces.focused_output(), Some(output_id));
    assert_eq!(state.workspaces.workspace_for_window(id), Some(destination));
    let bounds = state.output_bounds_for(&second).unwrap();
    let rect = state.windows.geometry(&id).unwrap().logical;
    assert_eq!(crate::floating::intersection(rect, bounds), Some(rect));
}

#[test]
fn floating_launch_restores_geometry_only_on_the_focused_output() {
    if !crate::startup_tests::private_runtime(
        "state::window_lifecycle_tests::floating_launch_restores_geometry_only_on_the_focused_output",
    ) {
        return;
    }
    for second_x in [800, 400, 0] {
        for restore_on_focused_output in [true, false] {
            let (mut events, mut state, first, second) = floating_launch_fixture();
            state.space.map_output(&second, (second_x, 0));
            let saved_output = if restore_on_focused_output {
                &second
            } else {
                &first
            };
            let saved_work = state.output_bounds_for(saved_output).unwrap();
            let saved = Rect::new(saved_work.x + 100., saved_work.y + 80., 240., 160.);
            state.floating_memory.save(
                "dev.ferese.Settings".into(),
                crate::floating::Remembered::new(saved_output.name(), saved, saved_work),
            );
            let output_id = state.output_ids[&second];
            let destination = state.output_workspaces.active_workspace(output_id).unwrap();
            state.activate_output_workspace(output_id, destination);
            let (window, _wire) = super::capture_privacy::tests::window_with_app_id(
                &mut state,
                &mut events,
                0xff112233,
                "dev.ferese.Settings",
            );
            let id = state.windows.ids()[&window];
            assert_eq!(state.output_workspaces.focused_output(), Some(output_id));
            assert_eq!(state.workspaces.workspace_for_window(id), Some(destination));
            let rect = state.windows.geometry(&id).unwrap().logical;
            let bounds = state.output_bounds_for(&second).unwrap();
            assert_eq!(crate::floating::intersection(rect, bounds), Some(rect));
            if restore_on_focused_output {
                assert_eq!(rect, saved, "second output at x={second_x}");
            } else {
                assert_eq!(
                    (rect.width, rect.height),
                    (64., 48.),
                    "memory belongs to the other output; second output at x={second_x}"
                );
            }
        }
    }
}

#[test]
fn floating_rule_restores_geometry_on_the_windows_output() {
    if !crate::startup_tests::private_runtime(
        "state::window_lifecycle_tests::floating_rule_restores_geometry_on_the_windows_output",
    ) {
        return;
    }
    for second_x in [800, 0] {
        let (mut events, mut state, first, second) = floating_launch_fixture();
        state.space.map_output(&second, (second_x, 0));
        let owner = state.output_ids[&second];
        let workspace = state.output_workspaces.active_workspace(owner).unwrap();
        state.activate_output_workspace(owner, workspace);
        let (window, _wire) = window(&mut state, &mut events, 0xff112233);
        let id = state.windows.ids()[&window];
        let work = state.output_bounds_for(&second).unwrap();
        let saved = Rect::new(work.x + 100., work.y + 80., 240., 160.);
        state.windows.update(id, |record| {
            record.floating_memory =
                Some(crate::floating::Remembered::new(second.name(), saved, work));
        });
        let focused = state.output_ids[&first];
        state.activate_output_workspace(
            focused,
            state.output_workspaces.active_workspace(focused).unwrap(),
        );
        state.apply_window_rule_result(
            &window,
            crate::window_rules::WindowRuleResult {
                floating: Some(true),
                ..Default::default()
            },
        );
        assert_eq!(state.output_workspaces.focused_output(), Some(focused));
        assert_eq!(state.workspaces.workspace_for_window(id), Some(workspace));
        assert_eq!(state.windows.geometry(&id).unwrap().logical, saved);
    }
}

#[test]
fn floating_memory_round_trip_preserves_output_identity() {
    if !crate::startup_tests::private_runtime(
        "state::window_lifecycle_tests::floating_memory_round_trip_preserves_output_identity",
    ) {
        return;
    }
    for second_x in [0, 400] {
        for on_second in [false, true] {
            let (mut events, mut state, first, second) = floating_launch_fixture();
            state.space.map_output(&second, (second_x, 0));
            let owner = if on_second { &second } else { &first };
            let output_id = state.output_ids[owner];
            let workspace = state.output_workspaces.active_workspace(output_id).unwrap();
            state.activate_output_workspace(output_id, workspace);
            let (window, wire) = super::capture_privacy::tests::window_with_app_id(
                &mut state,
                &mut events,
                0xff112233,
                "dev.ferese.Settings",
            );
            let id = state.windows.ids()[&window];
            // The whole rectangle fits both outputs, so area alone cannot
            // decide which monitor's placement memory should own it.
            let saved = Rect::new(f64::from(second_x) + 100., 80., 240., 160.);
            state.workspaces.set_floating_rect(id, saved).unwrap();
            state.relayout_window(id);
            state.remember_floating(&window);
            assert_eq!(
                state
                    .floating_memory
                    .get("dev.ferese.Settings")
                    .unwrap()
                    .output,
                owner.name(),
                "owner={} second_x={second_x}",
                owner.name(),
            );
            drop(wire);
            dispatch(&mut events, &mut state);
            assert!(state.windows.ids().is_empty());
            state.activate_output_workspace(output_id, workspace);
            let (restored, _wire) = super::capture_privacy::tests::window_with_app_id(
                &mut state,
                &mut events,
                0xff112233,
                "dev.ferese.Settings",
            );
            let id = state.windows.ids()[&restored];
            assert_eq!(state.windows.geometry(&id).unwrap().logical, saved);
        }
    }
}

#[test]
fn floating_memory_follows_a_drag_to_another_output() {
    if !crate::startup_tests::private_runtime(
        "state::window_lifecycle_tests::floating_memory_follows_a_drag_to_another_output",
    ) {
        return;
    }
    let (mut events, mut state, first, second) = floating_launch_fixture();
    let owner = state.output_ids[&first];
    let workspace = state.output_workspaces.active_workspace(owner).unwrap();
    state.activate_output_workspace(owner, workspace);
    let (window, _wire) = super::capture_privacy::tests::window_with_app_id(
        &mut state,
        &mut events,
        0xff112233,
        "dev.ferese.Settings",
    );
    let id = state.windows.ids()[&window];
    let saved = Rect::new(900., 80., 240., 160.);
    state.workspaces.set_floating_rect(id, saved).unwrap();
    state.relayout_window(id);
    state.remember_floating(&window);
    assert_eq!(state.workspaces.workspace_for_window(id), Some(workspace));
    let memory = state.floating_memory.get("dev.ferese.Settings").unwrap();
    assert_eq!(memory.output, second.name());
    assert_eq!(memory.restore(&state.floating_outputs()), Some(saved));
}

#[test]
fn keyboard_floating_move_and_resize_work_in_both_layouts() {
    if !crate::startup_tests::private_runtime(
        "state::window_lifecycle_tests::keyboard_floating_move_and_resize_work_in_both_layouts",
    ) {
        return;
    }
    let (mut events, mut state, _) = fixture();
    let (window, _wire) = window(&mut state, &mut events, 0xff112233);
    let id = state.windows.ids()[&window];
    state.toggle_focused_floating();
    let initial = Rect::new(120., 100., 160., 128.);
    let workspace = state.workspaces.active_id();
    for mode in [LayoutMode::Scrolling, LayoutMode::Tree] {
        state
            .workspaces
            .set_active_layout_mode(mode, state.output_bounds().unwrap())
            .unwrap();
        for (resize, direction, expected) in [
            (false, Direction::Left, Rect::new(88., 100., 160., 128.)),
            (false, Direction::Right, Rect::new(152., 100., 160., 128.)),
            (false, Direction::Up, Rect::new(120., 68., 160., 128.)),
            (false, Direction::Down, Rect::new(120., 132., 160., 128.)),
            (true, Direction::Left, Rect::new(120., 100., 128., 128.)),
            (true, Direction::Right, Rect::new(120., 100., 192., 128.)),
            (true, Direction::Up, Rect::new(120., 100., 160., 96.)),
            (true, Direction::Down, Rect::new(120., 100., 160., 160.)),
        ] {
            state.workspaces.set_floating_rect(id, initial).unwrap();
            state.relayout_window(id);
            if resize {
                state.resize_direction(direction);
            } else {
                state.move_direction(direction);
            }
            assert_eq!(
                state.workspaces.placement(id),
                Some(WindowPlacement::Floating { rect: expected })
            );
            assert_eq!(state.windows.geometry(&id).unwrap().logical, expected);
            assert_eq!(state.focused_window, Some(id));
            assert_eq!(state.workspaces.active_id(), workspace);
            assert_eq!(state.workspaces.workspace_for_window(id), Some(workspace));
        }
    }
}

#[test]
fn keyboard_floating_geometry_stays_reachable_and_respects_client_limits() {
    if !crate::startup_tests::private_runtime(
        "state::window_lifecycle_tests::keyboard_floating_geometry_stays_reachable_and_respects_client_limits",
    ) {
        return;
    }
    let (mut events, mut state, _) = fixture();
    let (window, mut wire) = window(&mut state, &mut events, 0xff112233);
    let id = state.windows.ids()[&window];
    state.toggle_focused_floating();
    request(&mut wire, 4, 8, &[100, 80], None);
    request(&mut wire, 4, 7, &[200, 160], None);
    request(&mut wire, 2, 6, &[], None);
    dispatch(&mut events, &mut state);
    state
        .workspaces
        .set_floating_rect(id, Rect::new(630., 470., 160., 128.))
        .unwrap();
    state.relayout_window(id);
    for _ in 0..5 {
        state.move_direction(Direction::Right);
        state.move_direction(Direction::Down);
        state.resize_direction(Direction::Right);
        state.resize_direction(Direction::Down);
    }
    assert_eq!(
        state.workspaces.placement(id),
        Some(WindowPlacement::Floating {
            rect: Rect::new(600., 440., 200., 160.),
        })
    );
    for _ in 0..30 {
        state.move_direction(Direction::Left);
        state.move_direction(Direction::Up);
        state.resize_direction(Direction::Left);
        state.resize_direction(Direction::Up);
    }
    assert_eq!(
        state.workspaces.placement(id),
        Some(WindowPlacement::Floating {
            rect: Rect::new(0., 0., 100., 80.),
        })
    );
    request(&mut wire, 4, 7, &[0, 0], None);
    request(&mut wire, 4, 8, &[900, 700], None);
    request(&mut wire, 2, 6, &[], None);
    dispatch(&mut events, &mut state);
    state.resize_direction(Direction::Left);
    state.resize_direction(Direction::Up);
    state.move_direction(Direction::Right);
    state.move_direction(Direction::Down);
    assert_eq!(
        state.workspaces.placement(id),
        Some(WindowPlacement::Floating {
            rect: Rect::new(0., 0., 900., 700.),
        })
    );
}

#[test]
fn keyboard_floating_actions_preserve_maximized_and_fullscreen_restore_geometry() {
    if !crate::startup_tests::private_runtime(
        "state::window_lifecycle_tests::keyboard_floating_actions_preserve_maximized_and_fullscreen_restore_geometry",
    ) {
        return;
    }
    let (mut events, mut state, _) = fixture();
    let (window, _wire) = window(&mut state, &mut events, 0xff112233);
    let id = state.windows.ids()[&window];
    state.toggle_focused_floating();
    let normal = state.workspaces.placement(id);
    for fullscreen in [false, true] {
        if fullscreen {
            state.set_window_fullscreen(id, true);
        } else {
            state.set_window_maximized(id, true);
        }
        let geometry = state.windows.geometry(&id).unwrap().logical;
        for direction in [
            Direction::Left,
            Direction::Right,
            Direction::Up,
            Direction::Down,
        ] {
            state.move_direction(direction);
            state.resize_direction(direction);
            assert_eq!(state.workspaces.placement(id), normal);
            assert_eq!(state.windows.geometry(&id).unwrap().logical, geometry);
        }
        if fullscreen {
            state.set_window_fullscreen(id, false);
        } else {
            state.set_window_maximized(id, false);
        }
        let WindowPlacement::Floating { rect } = normal.unwrap() else {
            panic!("floating window")
        };
        assert_eq!(state.windows.geometry(&id).unwrap().logical, rect);
    }
    state.focused_window = None;
    state.move_direction(Direction::Right);
    state.resize_direction(Direction::Down);
    assert_eq!(state.workspaces.placement(id), normal);
}

#[test]
fn keyboard_floating_resize_reuses_the_client_barrier_and_move_keeps_it_pending() {
    if !crate::startup_tests::private_runtime(
        "state::window_lifecycle_tests::keyboard_floating_resize_reuses_the_client_barrier_and_move_keeps_it_pending",
    ) {
        return;
    }
    let (mut events, mut state, _) = fixture();
    let (window, _wire) = window(&mut state, &mut events, 0xff112233);
    let id = state.windows.ids()[&window];
    state.toggle_focused_floating();
    state.set_floating_window_geometry(&window, (120, 100).into(), (64, 48).into());
    state.animations_enabled = true;
    let before = state.windows.geometry(&id).unwrap().visual.current;
    let client = state.windows.geometry(&id).unwrap().client.committed_size;
    state.resize_direction(Direction::Right);
    let transaction = *state
        .windows
        .transaction(&id)
        .expect("existing resize barrier");
    assert_eq!(transaction.source_geometry(), Some(window.geometry()));
    assert_eq!(
        state.windows.geometry(&id).unwrap().client.committed_size,
        client
    );
    assert_eq!(state.windows.geometry(&id).unwrap().visual.current, before);
    assert_eq!(
        state.windows.geometry(&id).unwrap().logical,
        Rect::new(120., 100., 96., 48.)
    );
    state.move_direction(Direction::Right);
    assert_eq!(
        state.windows.transaction(&id).unwrap().serial(),
        transaction.serial()
    );
    assert_eq!(
        state.windows.geometry(&id).unwrap().client.committed_size,
        client
    );
    assert_eq!(state.windows.geometry(&id).unwrap().visual.current, before);
    assert_eq!(
        state.windows.geometry(&id).unwrap().logical,
        Rect::new(152., 100., 96., 48.)
    );
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
        request(
            &mut wire,
            params,
            1,
            &[0, 0, 16, 0, 0],
            Some(file.as_raw_fd()),
        );
        let mut args = vec![4, 4, Fourcc::Argb8888 as u32, 0];
        if let Some(buffer) = buffer {
            args.insert(0, buffer);
        }
        request(
            &mut wire,
            params,
            if buffer.is_some() { 3 } else { 2 },
            &args,
            None,
        );
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
            assert_eq!(
                u32::from_ne_bytes(response[..4].try_into().unwrap()),
                params
            );
            assert_eq!(
                u32::from_ne_bytes(response[4..8].try_into().unwrap()) & 0xffff,
                0
            );
        } else {
            assert!(
                response.is_empty(),
                "create_immed success must not emit a failed event"
            );
        }
        assert!(client.get_credentials(&state.display_handle).is_ok());
    }

    // Deferred requests receive one terminal failure on timeout or removal.
    for params in [8, 9] {
        request(&mut wire, 4, 1, &[params], None);
        request(
            &mut wire,
            params,
            1,
            &[0, 0, 16, 0, 0],
            Some(file.as_raw_fd()),
        );
        request(
            &mut wire,
            params,
            2,
            &[4, 4, Fourcc::Argb8888 as u32, 0],
            None,
        );
        dispatch(&mut events, &mut state);
        state.process_pending_dmabuf_imports();
        if params == 8 {
            state
                .dmabuf_imports
                .expire(Instant::now() + Duration::from_secs(5));
        } else {
            state.dmabuf_imports.remove(b);
        }

        state.display_handle.flush_clients().unwrap();
        let response = read(&mut wire);
        assert_eq!(response.len(), 8);
        assert_eq!(
            u32::from_ne_bytes(response[..4].try_into().unwrap()),
            params
        );
        assert_eq!(
            u32::from_ne_bytes(response[4..8].try_into().unwrap()) & 0xffff,
            1
        );
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
    state.dmabuf_imports.process_with(b, None, |_| {
        panic!("disconnected client's request survived")
    });
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
    assert_eq!(
        state.workspaces.workspace_for_window(remapped),
        Some(active)
    );
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
        Some(ClientSize {
            width: 32,
            height: 48
        })
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
    for mode in [
        ferese_core::LayoutMode::Scrolling,
        ferese_core::LayoutMode::Tree,
    ] {
        let bounds = state.output_bounds().unwrap();
        state
            .workspaces
            .set_active_layout_mode(mode, bounds)
            .unwrap();
        state.relayout();
        state.set_window_maximized(id, true);
        let maximized = maximized_rect(bounds, state.gap_config.outer);
        assert_eq!(
            state.windows.geometry(&id).unwrap().logical,
            maximized,
            "{mode:?}"
        );
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
            .create_resource::<WlShm, smithay::wayland::GlobalData, Ferese>(
                &state.display_handle,
                1,
                smithay::wayland::GlobalData,
            )
            .unwrap();
        let (width, height) = (
            maximized.width.round() as u32,
            maximized.height.round() as u32,
        );
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(
            &0xff112233u32
                .to_ne_bytes()
                .repeat((width * height) as usize),
        )
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
        assert!(window.toplevel().unwrap().with_committed_state(|state| {
            state.is_some_and(|state| state.states.contains(xdg_toplevel::State::Maximized))
        }));

        state.set_window_maximized(id, false);
        assert_eq!(
            state.windows.geometry(&id).unwrap().logical,
            normal,
            "{mode:?}"
        );
        state.display_handle.flush_clients().unwrap();
        ack_configure(&mut wire, 3);
        request(&mut wire, 3, 3, &[0, 0, 64, 48], None);
        request(&mut wire, 2, 1, &[6, 0, 0], None);
        request(&mut wire, 2, 6, &[], None);
        dispatch(&mut events, &mut state);
        assert!(!window.toplevel().unwrap().with_committed_state(|state| {
            state.is_some_and(|state| state.states.contains(xdg_toplevel::State::Maximized))
        }));
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
        ((800i32, 0), (400, 0)),
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
        assert!(state.begin_window_interaction(
            &pointer,
            window,
            GrabStartData {
                focus: None,
                button: 0x110,
                location: (100.0, 100.0).into()
            },
            SERIAL_COUNTER.next_serial(),
            Some(xdg_toplevel::ResizeEdge::BottomRight.into()),
        ));
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
                time: smithay::backend::input::InputTime::from_millis(1),
            },
        );
        pointer.unset_grab(
            &mut state,
            SERIAL_COUNTER.next_serial(),
            smithay::backend::input::InputTime::from_millis(2),
        );
        state.poll_window_interaction();
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
        assert!(
            rejected,
            "invalid limits {minimum:?}/{maximum:?} were not rejected"
        );
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
    assert_eq!(
        state
            .effective_size_constraints(&window, (800, 600), (0, 0))
            .0,
        (800, 600)
    );
    assert_eq!(state.window_constraints()[&id].min_width, 800.0);
    assert_eq!(state.window_constraints()[&id].min_height, 600.0);

    state.window_rules = crate::config::Config::parse_source(
        "window-rule transient=#false min-width=500 min-height=400\n",
    )
    .unwrap()
    .window_rules()
    .unwrap();
    assert_eq!(
        state
            .effective_size_constraints(&window, (800, 600), (0, 0))
            .0,
        (500, 400)
    );
    let constraints = state.window_constraints();
    assert_eq!(constraints[&id].min_width, 500.0);
    assert_eq!(constraints[&id].min_height, 400.0);

    state.window_rules =
        crate::config::Config::parse_source("window-rule transient=#false min-height=400\n")
            .unwrap()
            .window_rules()
            .unwrap();
    assert_eq!(
        state
            .effective_size_constraints(&window, (800, 600), (0, 0))
            .0,
        (800, 400)
    );

    state.window_rules.clear();
    assert_eq!(
        state
            .effective_size_constraints(&window, (800, 600), (0, 0))
            .0,
        (800, 600)
    );
}

#[test]
fn resize_binding_mode_repeats_resize_a_mapped_window_and_stop_on_escape() {
    if !crate::startup_tests::private_runtime(
        "state::window_lifecycle_tests::resize_binding_mode_repeats_resize_a_mapped_window_and_stop_on_escape",
    ) {
        return;
    }
    use crate::input::binding_mode_tests::key;
    let (mut events, mut state, _) = fixture();
    state
        .reload_config_source("binding-mode resize { binding h resize left; }".into())
        .unwrap();
    let (window, _wire) = window(&mut state, &mut events, 0xff112233);
    let id = state.windows.ids()[&window];
    state.toggle_focused_floating();
    state
        .workspaces
        .set_floating_rect(id, Rect::new(120., 100., 224., 128.))
        .unwrap();
    state.relayout_window(id);
    state.set_binding_mode("resize");
    key(&mut state, 43, true);
    assert_eq!(
        state.workspaces.placement(id),
        Some(WindowPlacement::Floating {
            rect: Rect::new(120., 100., 192., 128.)
        })
    );
    key(&mut state, 43, true);
    assert_eq!(
        state.workspaces.placement(id),
        Some(WindowPlacement::Floating {
            rect: Rect::new(120., 100., 160., 128.)
        })
    );
    key(&mut state, 9, true);
    key(&mut state, 9, false);
    key(&mut state, 43, true);
    assert_eq!(
        state.workspaces.placement(id),
        Some(WindowPlacement::Floating {
            rect: Rect::new(120., 100., 160., 128.)
        })
    );
    key(&mut state, 43, false);
    assert!(state.intercepted_keys.is_empty());
}

#[test]
fn resize_binding_mode_repeats_require_the_original_modifiers() {
    if !crate::startup_tests::private_runtime(
        "state::window_lifecycle_tests::resize_binding_mode_repeats_require_the_original_modifiers",
    ) {
        return;
    }
    use crate::input::binding_mode_tests::key;
    let (mut events, mut state, _) = fixture();
    state
        .reload_config_source("binding-mode resize { binding h resize left; }".into())
        .unwrap();
    let (window, _wire) = window(&mut state, &mut events, 0xff112233);
    let id = state.windows.ids()[&window];
    state.toggle_focused_floating();
    state
        .workspaces
        .set_floating_rect(id, Rect::new(120., 100., 320., 128.))
        .unwrap();
    state.relayout_window(id);
    state.set_binding_mode("resize");
    key(&mut state, 43, true);
    let resized = state.workspaces.placement(id);
    key(&mut state, 133, true);
    key(&mut state, 43, true);
    assert_eq!(
        state.workspaces.placement(id),
        resized,
        "Super+h is not a resize-mode chord"
    );
    assert!(
        state
            .intercepted_keys
            .contains(&smithay::input::keyboard::Keycode::new(43))
    );
    key(&mut state, 133, false);
    key(&mut state, 43, true);
    assert_eq!(
        state.workspaces.placement(id),
        Some(WindowPlacement::Floating {
            rect: Rect::new(120., 100., 256., 128.),
        })
    );
    key(&mut state, 43, false);
    assert!(state.intercepted_keys.is_empty());
}

#[test]
fn reloading_mode_bindings_cancels_the_modes_focus_preview() {
    if !crate::startup_tests::private_runtime(
        "state::window_lifecycle_tests::reloading_mode_bindings_cancels_the_modes_focus_preview",
    ) {
        return;
    }
    use crate::input::binding_mode_tests::key;
    let (mut events, mut state, _) = fixture();
    let source = "binding-mode cycle { binding Alt+Tab focus-mru-next; }";
    state.reload_config_source(source.into()).unwrap();
    let (_first, _first_wire) = window(&mut state, &mut events, 0xff112233);
    let (_second, _second_wire) = window(&mut state, &mut events, 0xff223344);
    let origin = state.focused_window;
    state.set_binding_mode("cycle");
    key(&mut state, 64, true);
    key(&mut state, 23, true);
    assert!(state.focus_cycle.is_some());
    state
        .reload_config_source(format!("{source}\ninput {{ repeat-rate 30; }}"))
        .unwrap();
    assert_eq!(state.binding_modes.active(), None);
    assert!(
        state.focus_cycle.is_none(),
        "the exited mode cannot keep its focus preview"
    );
    assert_eq!(state.focused_window, origin);
    assert!(
        state
            .intercepted_keys
            .contains(&smithay::input::keyboard::Keycode::new(23))
    );
    key(&mut state, 23, false);
    key(&mut state, 64, false);
    assert!(state.intercepted_keys.is_empty());
}

#[test]
fn tree_tabs_join_switch_leave_and_hide_inactive_maximized_windows() {
    if !crate::startup_tests::private_runtime(
        "state::window_lifecycle_tests::tree_tabs_join_switch_leave_and_hide_inactive_maximized_windows",
    ) {
        return;
    }
    let (mut events, mut state, _output) = fixture();
    state.toggle_layout_mode();
    let (first, _first_wire) = window(&mut state, &mut events, 0xff112233);
    let (second, _second_wire) = window(&mut state, &mut events, 0xff445566);
    let a = state.windows.ids()[&first];
    let b = state.windows.ids()[&second];
    state.focused_window = Some(b);
    state.consume_focused_window();
    let workspace = state.workspaces.active_id();
    assert_eq!(state.tab_bars[&workspace].len(), 1);
    assert!(state.space.element_location(&first).is_none());
    assert!(state.space.element_location(&second).is_some());
    let bar = &state.tab_bars[&workspace][0];
    let header = bar.rect;
    let content = state.windows.geometry(&b).unwrap().visual.target;
    assert_eq!(content.y, header.y + header.height);
    let hit = Point::from((
        header.x + header.width * 0.25,
        header.y + header.height * 0.5,
    ));
    assert_eq!(state.tab_at(hit), Some(a));
    assert!(state.surface_under(hit).is_none());
    state.cycle_tab(false);
    assert_eq!(state.focused_window, Some(a));
    assert!(state.space.element_location(&first).is_some());
    assert!(state.space.element_location(&second).is_none());
    state.toggle_focused_maximized();
    assert_eq!(state.visible_tab_bars(workspace).count(), 0);
    state.cycle_tab(true);
    assert_eq!(state.focused_window, Some(b));
    assert!(
        state.space.element_location(&first).is_none(),
        "inactive maximized tab must remain hidden"
    );
    assert_eq!(state.visible_tab_bars(workspace).count(), 1);
    state.toggle_focused_fullscreen();
    let tree = state.workspaces.active().layout.clone();
    state.leave_tab();
    state.expel_focused_window();
    state.cycle_tab(false);
    assert_eq!(state.workspaces.active().layout, tree);
    assert_eq!(state.visible_tab_bars(workspace).count(), 0);
    state.toggle_focused_fullscreen();
    state.leave_tab();
    assert!(state.tab_bars[&workspace].is_empty());
    assert!(state.space.element_location(&first).is_some());
    assert!(state.space.element_location(&second).is_some());
    assert!(state.workspaces.active().layout.validate().is_ok());
}

#[test]
fn scrolling_tabs_switch_with_directional_focus_and_drop_hidden_resize_holds() {
    if !crate::startup_tests::private_runtime(
        "state::window_lifecycle_tests::scrolling_tabs_switch_with_directional_focus_and_drop_hidden_resize_holds",
    ) {
        return;
    }
    let (mut events, mut state, _output) = fixture();
    let (first, _first_wire) = window(&mut state, &mut events, 0xff112233);
    state.toggle_column_tabbed();
    let (second, _second_wire) = window(&mut state, &mut events, 0xff445566);
    state.consume_focused_window();
    let (third, _third_wire) = window(&mut state, &mut events, 0xff778899);
    state.consume_focused_window();
    let a = state.windows.ids()[&first];
    let b = state.windows.ids()[&second];
    let c = state.windows.ids()[&third];
    let workspace = state.workspaces.active_id();
    assert_eq!(state.tab_bars[&workspace][0].group.windows, [a, b, c]);
    assert_eq!(state.space.elements().count(), 1);
    let position = state.workspaces.active().layout.viewport_x();
    state.windows.set_transaction(
        c,
        crate::resize_transaction::ResizeTransaction::new(
            SERIAL_COUNTER.next_serial(),
            state.presentation_now(),
        ),
    );
    state.focus_direction(Direction::Up);
    assert_eq!(state.focused_window, Some(b));
    assert!(state.windows.transaction(&c).is_none());
    assert!(state.space.element_location(&third).is_none());
    assert!(state.space.element_location(&second).is_some());
    assert_eq!(state.workspaces.active().layout.viewport_x(), position);
    state.focus_direction(Direction::Down);
    assert_eq!(state.focused_window, Some(c));
    state.toggle_column_tabbed();
    assert_eq!(state.space.elements().count(), 3);
    assert!(state.tab_bars[&workspace].is_empty());
    state.toggle_column_tabbed();
    assert_eq!(state.space.elements().count(), 1);
    state.leave_tab();
    assert_eq!(state.space.elements().count(), 2);
    assert_eq!(state.tab_bars[&workspace][0].group.windows, [a, b]);
}

#[test]
fn tab_indicator_hover_switch_and_flash_preserve_window_geometry() {
    if !crate::startup_tests::private_runtime(
        "state::window_lifecycle_tests::tab_indicator_hover_switch_and_flash_preserve_window_geometry",
    ) {
        return;
    }
    let (mut events, mut state, _output) = fixture();
    let (first, _first_wire) = window(&mut state, &mut events, 0xff112233);
    state.toggle_column_tabbed();
    let (second, _second_wire) = window(&mut state, &mut events, 0xff445566);
    state.consume_focused_window();
    let a = state.windows.ids()[&first];
    let b = state.windows.ids()[&second];
    let workspace = state.workspaces.active_id();
    let bar = state.tab_bars[&workspace][0].clone();
    let before = state.windows.geometry(&b).unwrap().logical;
    let g = state
        .tab_indicator_geometry(&bar, state.presented_window_rect(b).unwrap())
        .unwrap();
    let point = (
        g.hit.x + g.hit.width * 0.5,
        g.rail.y + g.segment_height * 0.5,
    )
        .into();
    assert_eq!(state.tab_at(point), Some(a));
    assert!(
        state.surface_under(point).is_none(),
        "the rail must not deliver input to the client"
    );
    state.update_tab_indicator_hover(point);
    assert_eq!(
        state.tab_indicator.hover.as_ref().unwrap().opacity.current,
        1.
    );
    assert_eq!(state.windows.geometry(&b).unwrap().logical, before);
    let g = state
        .tab_indicator_geometry(&bar, state.presented_window_rect(b).unwrap())
        .unwrap();
    assert!(
        g.list.width < state.tab_indicator_settings.list_width,
        "short titles should not fill the width cap"
    );
    let row = g.row(0);
    let point = (row.x + row.width * 0.5, row.y + row.height * 0.5).into();
    state.update_tab_indicator_hover(point);
    assert_eq!(state.tab_at(point), Some(a));
    state.activate_managed_window(a);
    assert_eq!(state.windows.geometry(&a).unwrap().logical, before);
    assert!(
        state.tab_indicator.flash.is_none(),
        "pointer activation does not flash"
    );
    state.update_tab_indicator_hover((799., 599.).into());
    assert_eq!(
        state.tab_indicator.hover.as_ref().unwrap().opacity.current,
        0.
    );
    state.tab_indicator_settings.flash_duration = Duration::from_millis(1);
    state.cycle_tab(false);
    assert_eq!(state.focused_window, Some(b));
    assert_eq!(
        state.tab_indicator.flash.as_ref().unwrap().opacity.current,
        1.
    );
    assert_eq!(state.windows.geometry(&b).unwrap().logical, before);
    let deadline = Instant::now() + Duration::from_secs(1);
    while state
        .tab_indicator
        .flash
        .as_ref()
        .is_some_and(|f| f.opacity.target != 0.)
        && Instant::now() < deadline
    {
        events
            .dispatch(Duration::from_millis(5), &mut state)
            .unwrap();
    }
    assert_eq!(
        state.tab_indicator.flash.as_ref().unwrap().opacity.current,
        0.
    );
    state.toggle_column_tabbed();
    assert!(state.tab_indicator.hover.is_none());
    assert!(state.tab_indicator.flash.is_none());
    assert!(state.tab_at(point).is_none());
}

#[test]
fn tab_indicator_yields_to_a_floating_window_and_disappears_with_one_tab() {
    if !crate::startup_tests::private_runtime(
        "state::window_lifecycle_tests::tab_indicator_yields_to_a_floating_window_and_disappears_with_one_tab",
    ) {
        return;
    }
    let (mut events, mut state, _output) = fixture();
    let (first, _first_wire) = window(&mut state, &mut events, 0xff112233);
    state.toggle_column_tabbed();
    let (second, _second_wire) = window(&mut state, &mut events, 0xff445566);
    state.consume_focused_window();
    let a = state.windows.ids()[&first];
    let b = state.windows.ids()[&second];
    let workspace = state.workspaces.active_id();
    let (overlay, _overlay_wire) = window(&mut state, &mut events, 0xff778899);
    let overlay_id = state.windows.ids()[&overlay];
    state.toggle_focused_floating();
    let bar = state.tab_bars[&workspace][0].clone();
    let g = state
        .tab_indicator_geometry(&bar, state.presented_window_rect(b).unwrap())
        .unwrap();
    let point = (g.hit.x + 6., g.hit.y + 5.).into();
    state
        .workspaces
        .set_floating_rect(overlay_id, Rect::new(g.hit.x, g.hit.y, 200., 100.))
        .unwrap();
    state.relayout();
    assert!(
        state.tab_at(point).is_none(),
        "floating window above the rail keeps its pointer input"
    );
    state.activate_managed_window(b);
    state.leave_tab();
    assert!(state.scrolling_tab_bar(a).is_none());
    assert!(state.tab_indicator.hover.is_none());
}

#[test]
fn shell_can_move_a_window_to_the_current_workspace_on_another_output() {
    if !crate::startup_tests::private_runtime(
        "state::window_lifecycle_tests::shell_can_move_a_window_to_the_current_workspace_on_another_output",
    ) {
        return;
    }
    let (mut events, mut state, _) = fixture();
    let (window, _wire) = window(&mut state, &mut events, 0xff112233);
    let id = state.windows.ids()[&window];
    let source = state.workspaces.workspace_for_window(id).unwrap();
    let second = Output::new(
        "second".into(),
        smithay::output::PhysicalProperties {
            serial_number: String::new(),
            size: (0, 0).into(),
            subpixel: smithay::output::Subpixel::Unknown,
            make: "test".into(),
            model: "test".into(),
        },
    );
    second.change_current_state(
        Some(smithay::output::Mode {
            size: (800, 600).into(),
            refresh: 60_000,
        }),
        None,
        None,
        None,
    );
    state.space.map_output(&second, (800, 0));
    state.register_output(&second, "second".into());
    let output = state.output_ids[&second];
    let destination = state.output_workspaces.active_workspace(output).unwrap();
    assert_ne!(source, destination);
    assert!(state.move_managed_window_to_workspace(id, destination));
    assert_eq!(state.workspaces.workspace_for_window(id), Some(destination));
    assert!(state.move_managed_window_to_workspace(id, destination));
}

fn change_transient_parent(state: &mut Ferese, child: &Window, parent: Option<&Window>) {
    use smithay::wayland::compositor::with_states;
    use smithay::wayland::shell::xdg::{XdgShellHandler, XdgToplevelSurfaceData};
    let toplevel = child.toplevel().unwrap();
    with_states(toplevel.wl_surface(), |states| {
        states
            .data_map
            .get::<XdgToplevelSurfaceData>()
            .unwrap()
            .lock()
            .unwrap()
            .parent = parent.map(|parent| parent.toplevel().unwrap().wl_surface().clone());
    });
    state.parent_changed(toplevel.clone());
}

#[test]
fn transient_fullscreen_exception_follows_parent_changes() {
    if !crate::startup_tests::private_runtime(
        "state::window_lifecycle_tests::transient_fullscreen_exception_follows_parent_changes",
    ) {
        return;
    }
    let (mut events, mut state, _output) = fixture();
    let (parent, _parent_wire) = window(&mut state, &mut events, 0xff112233);
    let (other, _other_wire) = window(&mut state, &mut events, 0xff223344);
    let (child, _child_wire) = window(&mut state, &mut events, 0xff334455);
    let parent_id = state.windows.ids()[&parent];
    let other_id = state.windows.ids()[&other];
    let child_id = state.windows.ids()[&child];
    state.set_window_fullscreen(parent_id, true);
    change_transient_parent(&mut state, &child, Some(&parent));
    assert_eq!(
        state.floating_above_fullscreen.get(&child_id),
        Some(&parent_id)
    );
    change_transient_parent(&mut state, &child, None);
    assert!(
        !state.floating_above_fullscreen.contains_key(&child_id),
        "unparenting must revoke fullscreen priority"
    );
    assert!(
        matches!(
            state.workspaces.placement(child_id),
            Some(WindowPlacement::Floating { .. })
        ),
        "unparenting must not retile the dialog"
    );
    change_transient_parent(&mut state, &child, Some(&parent));
    change_transient_parent(&mut state, &child, Some(&other));
    assert!(
        !state.floating_above_fullscreen.contains_key(&child_id),
        "an ordinary parent must not retain the old fullscreen exception"
    );
    state.set_window_fullscreen(other_id, true);
    change_transient_parent(&mut state, &child, Some(&other));
    assert_eq!(
        state.floating_above_fullscreen.get(&child_id),
        Some(&other_id)
    );
    state.remove_tiled_window(&other);
    assert!(
        state
            .floating_above_fullscreen
            .values()
            .all(|parent| *parent != other_id),
        "removing a parent must revoke its children's exceptions"
    );
    assert!(state.windows.window(child_id).is_some());
}
