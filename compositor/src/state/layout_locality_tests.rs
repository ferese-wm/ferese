use super::{
    AnimatedValue, Duration, Ferese, HashMap, Output, Point, Rect, WindowId, WorkspaceId,
    WorkspaceLayout, xdg_toplevel,
};
use crate::resize_transaction::ResizeTransaction;
use crate::state::capture_privacy::tests::window;
use crate::state::window_lifecycle_tests::fixture;
use ferese_core::LayoutMode;
use ferese_layout::{Axis, Direction};
use smithay::input::pointer::{GrabStartData, MotionEvent};
use smithay::output::Scale;
use smithay::utils::{SERIAL_COUNTER, Transform};

use std::cell::RefCell;

thread_local! {
    static VISITS: RefCell<(Vec<WorkspaceId>, Vec<WindowId>)> = const { RefCell::new((Vec::new(), Vec::new())) };
}

pub(super) fn resolved_workspace(id: WorkspaceId) {
    VISITS.with_borrow_mut(|visits| visits.0.push(id));
}

pub(super) fn visited_window(id: WindowId) {
    VISITS.with_borrow_mut(|visits| visits.1.push(id));
}

fn clear_visits() {
    VISITS.with_borrow_mut(|visits| *visits = Default::default());
}

fn take_visits() -> (Vec<WorkspaceId>, Vec<WindowId>) {
    VISITS.with_borrow_mut(std::mem::take)
}

fn second_output(state: &mut Ferese, first: &Output) -> (Output, WorkspaceId) {
    let output = Output::new("second".into(), first.physical_properties());
    output.change_current_state(first.current_mode(), None, None, None);
    state.space.map_output(&output, (800, 0));
    state.register_output(&output, "second".into());
    let id = state.output_ids[&output];
    let workspace = state.output_workspaces.active_workspace(id).unwrap();
    (output, workspace)
}

fn geometry_targets(state: &Ferese) -> HashMap<WindowId, Rect> {
    state
        .windows
        .geometries()
        .map(|(&id, geometry)| (id, geometry.logical))
        .collect()
}

#[test]
fn tiled_pointer_resize_visits_only_its_workspace_and_matches_full_layout() {
    if !crate::startup_tests::private_runtime(
        "state::layout::locality_tests::tiled_pointer_resize_visits_only_its_workspace_and_matches_full_layout",
    ) {
        return;
    }
    for mode in [LayoutMode::Scrolling, LayoutMode::Tree] {
        for other_count in [1, 12] {
            let (mut events, mut state, first) = fixture();
            state.animation_test_time = Some(Duration::ZERO);
            first.change_current_state(
                None,
                Some(Transform::Flipped180),
                Some(Scale::Fractional(1.5)),
                None,
            );
            let local_workspace = state.workspaces.active_id();
            state
                .workspaces
                .set_active_layout_mode(mode, state.output_bounds_for(&first).unwrap())
                .unwrap();
            let (local, local_wire) = window(&mut state, &mut events, 0xff112233);
            let (sibling, sibling_wire) = window(&mut state, &mut events, 0xff223344);
            let local_id = state.windows.ids()[&local];
            let sibling_id = state.windows.ids()[&sibling];
            let (second, other_workspace) = second_output(&mut state, &first);
            state.activate_output_workspace(state.output_ids[&second], other_workspace);
            let mut wires = vec![local_wire, sibling_wire];
            let mut unrelated = Vec::new();
            for _ in 0..other_count {
                let (app, wire) = window(&mut state, &mut events, 0xff445566);
                unrelated.push(state.windows.ids()[&app]);
                wires.push(wire);
            }
            let (hidden_window, hidden_wire) = window(&mut state, &mut events, 0xff556677);
            wires.push(hidden_wire);
            let hidden_id = state.windows.ids()[&hidden_window];
            let hidden_workspace = state.workspaces.create_workspace();
            state
                .workspaces
                .move_window_to_workspace(hidden_id, hidden_workspace, Axis::Horizontal, 0.5)
                .unwrap();
            state.activate_output_workspace(state.output_ids[&first], local_workspace);
            state.focused_window = Some(local_id);
            state.relayout();
            assert!(state.space.element_location(&hidden_window).is_none());
            state.windows.update(hidden_id, |record| {
                record.column_width_pending = true;
                record.world_x = Some((hidden_workspace, AnimatedValue::new(123.0)));
            });
            let before = geometry_targets(&state);
            let pointer = state.seat.get_pointer().unwrap();
            let rect = state.visual_rect_for_window(&local).unwrap();
            let start = Point::from((
                f64::from(rect.loc.x + rect.size.w),
                f64::from(rect.loc.y + rect.size.h / 2),
            ));
            assert!(state.begin_window_interaction(
                &pointer,
                local,
                GrabStartData {
                    focus: None,
                    button: 0x110,
                    location: start,
                },
                SERIAL_COUNTER.next_serial(),
                Some(xdg_toplevel::ResizeEdge::Right.into())
            ));
            clear_visits();
            pointer.motion(
                &mut state,
                None,
                &MotionEvent {
                    location: (start.x + 50.0, start.y).into(),
                    serial: SERIAL_COUNTER.next_serial(),
                    time: smithay::backend::input::InputTime::from_millis(2),
                },
            );
            state.poll_window_interaction();
            assert!(state.pointer_operation.is_some());
            let (resolutions, visits) = take_visits();
            assert_eq!(resolutions, [local_workspace]);
            assert_eq!(visits.len(), 2);
            assert!(visits.contains(&local_id) && visits.contains(&sibling_id));
            assert_ne!(
                state.windows.geometry(&local_id).unwrap().logical.width,
                before[&local_id].width
            );
            for id in &unrelated {
                assert_eq!(state.windows.geometry(id).unwrap().logical, before[id]);
                assert!(
                    state
                        .space
                        .element_location(state.windows.window(*id).unwrap())
                        .is_some()
                );
            }
            let hidden = state.windows.record(hidden_id).unwrap();
            assert!(
                hidden.column_width_pending,
                "local layout consumed an unrelated request"
            );
            assert_eq!(hidden.world_x.unwrap().1.current, 123.0);
            assert!(state.space.element_location(&hidden_window).is_none());
            let scoped = geometry_targets(&state);
            clear_visits();
            state.relayout();
            let (full_resolutions, full_visits) = take_visits();
            assert_eq!(full_resolutions.len(), 2);
            assert!(full_resolutions.contains(&other_workspace));
            assert_eq!(full_visits.len(), 2 + other_count);
            assert_eq!(
                geometry_targets(&state),
                scoped,
                "scoped geometry diverged from full layout: {mode:?}"
            );
            state.cancel_window_interaction();
            clear_visits();
            state.relayout_workspace_geometry(hidden_workspace);
            assert_eq!(
                take_visits().0.len(),
                2,
                "a hidden workspace must use full reconciliation"
            );
            assert!(state.space.element_location(&hidden_window).is_none());
            state.workspaces.validate().unwrap();
            drop(wires);
        }
    }
}

#[test]
fn scoped_layout_preserves_other_resize_dependencies_and_column_requests() {
    if !crate::startup_tests::private_runtime(
        "state::layout::locality_tests::scoped_layout_preserves_other_resize_dependencies_and_column_requests",
    ) {
        return;
    }
    for mode in [LayoutMode::Scrolling, LayoutMode::Tree] {
        let (mut events, mut state, first) = fixture();
        state.animation_test_time = Some(Duration::ZERO);
        let local_workspace = state.workspaces.active_id();
        state
            .workspaces
            .set_active_layout_mode(mode, state.output_bounds_for(&first).unwrap())
            .unwrap();
        let (local, _local_wire) = window(&mut state, &mut events, 0xff112233);
        let (local_sibling, _local_sibling_wire) = window(&mut state, &mut events, 0xff223344);
        let local_id = state.windows.ids()[&local];
        let local_sibling_id = state.windows.ids()[&local_sibling];
        let (second, other_workspace) = second_output(&mut state, &first);
        state.activate_output_workspace(state.output_ids[&second], other_workspace);
        let (other, _other_wire) = window(&mut state, &mut events, 0xff445566);
        let (other_sibling, _other_sibling_wire) = window(&mut state, &mut events, 0xff556677);
        let other_id = state.windows.ids()[&other];
        let other_sibling_id = state.windows.ids()[&other_sibling];
        state.activate_output_workspace(state.output_ids[&first], local_workspace);
        state.relayout();
        state.animations_enabled = true;
        state.windows.set_transaction(
            other_id,
            ResizeTransaction::new(SERIAL_COUNTER.next_serial(), Duration::ZERO)
                .with_column_width(Some(100.0), Some(200.0)),
        );
        state.windows.update(other_sibling_id, |record| {
            record.column_width_pending = true
        });
        state.rebuild_presentation_dependencies();
        let other_waits = [other_id, other_sibling_id].map(|id| {
            state
                .presentation_dependencies
                .reflow_blocked(id, &state.windows)
        });
        assert!(other_waits[0]);
        let before = geometry_targets(&state);
        let bounds = state.output_bounds_for(&first).unwrap();
        state
            .workspaces
            .workspace_mut(local_workspace)
            .unwrap()
            .layout
            .resize_edge(local_id, Direction::Right, 50.0, bounds)
            .unwrap();
        clear_visits();
        state.relayout_workspace_geometry(local_workspace);
        assert_eq!(take_visits().0, [local_workspace]);
        assert_eq!(
            [other_id, other_sibling_id].map(|id| state
                .presentation_dependencies
                .reflow_blocked(id, &state.windows)),
            other_waits
        );
        assert!(
            state
                .windows
                .record(other_sibling_id)
                .unwrap()
                .column_width_pending
        );
        assert_eq!(
            state.windows.geometry(&other_id).unwrap().logical,
            before[&other_id]
        );
        assert_eq!(
            state.windows.geometry(&other_sibling_id).unwrap().logical,
            before[&other_sibling_id]
        );
        let local_waits = [local_id, local_sibling_id].map(|id| {
            state
                .presentation_dependencies
                .reflow_blocked(id, &state.windows)
        });
        assert!(
            local_waits[0],
            "resized client must still hold its own configure"
        );
        let viewport = state
            .presentation_dependencies
            .save_viewport(local_workspace);
        let scoped = geometry_targets(&state);
        state.rebuild_presentation_dependencies();
        assert_eq!(
            [local_id, local_sibling_id].map(|id| state
                .presentation_dependencies
                .reflow_blocked(id, &state.windows)),
            local_waits
        );
        assert!(
            state
                .presentation_dependencies
                .save_viewport(local_workspace)
                == viewport
        );
        state.relayout();
        assert_eq!(geometry_targets(&state), scoped);
        state.workspaces.validate().unwrap();
    }
}

#[test]
fn column_sources_keep_own_transactions_and_ignore_inactive_tab_siblings() {
    if !crate::startup_tests::private_runtime(
        "state::layout::locality_tests::column_sources_keep_own_transactions_and_ignore_inactive_tab_siblings",
    ) {
        return;
    }
    let (mut events, mut state, _) = fixture();
    let (first, _first_wire) = window(&mut state, &mut events, 0xff112233);
    let (second, _second_wire) = window(&mut state, &mut events, 0xff223344);
    let a = state.windows.ids()[&first];
    let b = state.windows.ids()[&second];
    let workspace = state.workspaces.active_id();
    state.workspaces.stack_window(b, a).unwrap();
    state.relayout();
    state.windows.set_transaction(
        a,
        ResizeTransaction::new(100.into(), Duration::ZERO)
            .with_column_width(Some(160.0), Some(320.0)),
    );
    state.windows.set_transaction(
        b,
        ResizeTransaction::new(101.into(), Duration::ZERO)
            .with_column_width(Some(170.0), Some(320.0)),
    );
    let widths = state.source_column_widths(Some(workspace));
    assert_eq!(widths[&a], 160.0);
    assert_eq!(widths[&b], 170.0);
    state.windows.clear_transaction(&a);
    let widths = state.source_column_widths(Some(workspace));
    assert_eq!(
        widths[&a], 170.0,
        "committed sibling lost its cohort's source"
    );
    state.workspaces.focus_window(a).unwrap();
    let WorkspaceLayout::Scrolling(layout) = &mut state.workspaces.active_mut().layout else {
        panic!("scrolling layout")
    };
    layout
        .set_column_display(a, ferese_layout::ColumnDisplay::Tabbed)
        .unwrap();
    let allocated = layout.allocated_column_width(a).unwrap();
    let widths = state.source_column_widths(Some(workspace));
    assert_eq!(
        widths[&a], allocated,
        "inactive tab supplied a visible tab's source"
    );
    assert_eq!(widths[&b], 170.0);
}
