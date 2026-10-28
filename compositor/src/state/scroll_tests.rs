//! Exercise continuous scrolling through the real gesture input path.
use super::*;
use crate::input::gesture_tests::{GestureInput, Sample, fixture};
use crate::state::dispatch_test_client as dispatch;
use smithay::backend::input::InputEvent;
use smithay::reexports::calloop;
use std::os::unix::net::UnixStream;

struct ScrollTest {
    state: Ferese,
    events: calloop::EventLoop<'static, Ferese>,
    wires: Vec<UnixStream>,
    windows: Vec<ferese_layout::WindowId>,
}

fn scroll_test(animations: bool) -> ScrollTest {
    use ferese_core::WorkspaceLayout;
    use ferese_layout::ColumnWidth;
    let (mut state, mut events, wire) = fixture();
    let mut wires = vec![wire];
    for _ in 1..6 {
        let (_, wire) = crate::state::test_window(&mut state, &mut events, 0xff112233);
        wires.push(wire);
    }
    let windows = state
        .workspaces
        .active()
        .layout
        .window_ids()
        .collect::<Vec<_>>();
    assert_eq!(windows.len(), 6);
    state.gap_config = ferese_layout::GapConfig {
        inner: 0.,
        outer: 0.,
        smart: false,
    };
    let WorkspaceLayout::Scrolling(layout) = &mut state.workspaces.active_mut().layout else {
        panic!("scrolling fixture");
    };
    for id in &windows {
        layout
            .set_column_width(*id, ColumnWidth::Fixed(400.))
            .unwrap();
    }
    state.focused_window = Some(windows[0]);
    state.workspaces.focus_window(windows[0]).unwrap();
    state.animations_enabled = false;
    state.relayout();
    state.animation_test_time = Some(std::time::Duration::from_secs(5));
    state.advance_animations_at(std::time::Duration::ZERO, std::time::Duration::from_secs(5));
    state.animations_enabled = animations;
    ScrollTest {
        state,
        events,
        wires,
        windows,
    }
}

fn pan(state: &mut Ferese, time: u32, dx: f64) {
    state.process_input_event::<GestureInput>(InputEvent::GestureSwipeUpdate {
        event: Sample::scroll(time, dx, false),
    });
    state.advance_animations_at(std::time::Duration::ZERO, std::time::Duration::from_secs(5));
}

#[test]
fn horizontal_swipes_cross_columns_reverse_and_release_without_focus_churn() {
    if !crate::startup_tests::private_runtime(
        "state::scroll_tests::horizontal_swipes_cross_columns_reverse_and_release_without_focus_churn",
    ) {
        return;
    }
    for animations in [true, false] {
        let mut test = scroll_test(animations);
        let first = test.windows[0];
        let origin = test.state.presented_window_rect(first).unwrap().x;
        test.state
            .process_input_event::<GestureInput>(InputEvent::GestureSwipeBegin {
                event: Sample::scroll(0, 0., false),
            });
        pan(&mut test.state, 20, -320.);
        assert_eq!(test.state.focused_window, Some(first));
        let one = test.state.presented_window_rect(first).unwrap().x;
        pan(&mut test.state, 40, -320.);
        let two = test.state.presented_window_rect(first).unwrap().x;
        assert!((origin - one - 800.).abs() < 1e-6);
        assert!(
            (origin - two - 1600.).abs() < 1e-6,
            "swipe stopped at its first neighbor"
        );
        pan(&mut test.state, 60, 160.);
        let reversed = test.state.presented_window_rect(first).unwrap().x;
        assert!((origin - reversed - 1200.).abs() < 1e-6);
        assert_eq!(test.state.focused_window, Some(first));
        // Pausing before release removes momentum.
        test.state
            .process_input_event::<GestureInput>(InputEvent::GestureSwipeEnd {
                event: Sample::scroll(200, 0., false),
            });
        assert_eq!(test.state.focused_window, Some(test.windows[3]));
        assert_eq!(
            test.state.workspaces.active().layout.viewport_x(),
            Some(1200.)
        );
        assert!((test.state.presented_window_rect(first).unwrap().x - reversed).abs() < 1e-6);
    }
}

#[test]
fn cancelled_and_invalidated_scrolls_do_not_activate_a_new_window() {
    if !crate::startup_tests::private_runtime(
        "state::scroll_tests::cancelled_and_invalidated_scrolls_do_not_activate_a_new_window",
    ) {
        return;
    }
    for change in ["cancel", "width", "removed"] {
        let mut test = scroll_test(false);
        let first = test.windows[0];
        let origin = test.state.presented_window_rect(first).unwrap().x;
        test.state
            .process_input_event::<GestureInput>(InputEvent::GestureSwipeBegin {
                event: Sample::scroll(0, 0., false),
            });
        pan(&mut test.state, 20, -200.);
        match change {
            "cancel" => {}
            "width" => {
                let ferese_core::WorkspaceLayout::Scrolling(layout) =
                    &mut test.state.workspaces.active_mut().layout
                else {
                    unreachable!()
                };
                layout
                    .set_column_width(test.windows[1], ferese_layout::ColumnWidth::Fixed(500.))
                    .unwrap();
                test.state.relayout();
            }
            "removed" => {
                test.wires
                    .last()
                    .unwrap()
                    .shutdown(std::net::Shutdown::Both)
                    .unwrap();
                dispatch(&mut test.events, &mut test.state);
            }
            _ => unreachable!(),
        }
        test.state
            .process_input_event::<GestureInput>(InputEvent::GestureSwipeEnd {
                event: Sample::scroll(200, 0., change == "cancel"),
            });
        assert_eq!(test.state.focused_window, Some(first), "{change}");
        assert!(
            (test.state.presented_window_rect(first).unwrap().x - origin).abs() < 1e-6,
            "{change}"
        );
    }
}

#[test]
fn flick_momentum_crosses_columns_and_releases_from_the_last_unrendered_input() {
    if !crate::startup_tests::private_runtime(
        "state::scroll_tests::flick_momentum_crosses_columns_and_releases_from_the_last_unrendered_input",
    ) {
        return;
    }
    let mut test = scroll_test(true);
    let first = test.windows[0];
    let origin = test.state.presented_window_rect(first).unwrap().x;
    test.state
        .process_input_event::<GestureInput>(InputEvent::GestureSwipeBegin {
            event: Sample::scroll(0, 0., false),
        });
    test.state
        .process_input_event::<GestureInput>(InputEvent::GestureSwipeUpdate {
            event: Sample::scroll(20, -32., false),
        });
    // There has been no animation tick since the last finger movement.
    test.state
        .process_input_event::<GestureInput>(InputEvent::GestureSwipeEnd {
            event: Sample::scroll(22, 0., false),
        });
    assert_eq!(test.state.focused_window, Some(test.windows[4]));
    assert_eq!(
        test.state.workspaces.active().layout.viewport_x(),
        Some(1600.)
    );
    let released = test.state.presented_window_rect(first).unwrap().x;
    assert!(
        (origin - released - 80.).abs() < 1e-6,
        "release discarded the unrendered input"
    );
}

#[test]
fn output_loss_cancels_the_scroll_without_applying_a_stale_destination() {
    if !crate::startup_tests::private_runtime(
        "state::scroll_tests::output_loss_cancels_the_scroll_without_applying_a_stale_destination",
    ) {
        return;
    }
    let mut test = scroll_test(false);
    test.state
        .process_input_event::<GestureInput>(InputEvent::GestureSwipeBegin {
            event: Sample::scroll(0, 0., false),
        });
    pan(&mut test.state, 20, -320.);
    let output = test.state.space.outputs().next().unwrap().clone();
    test.state.space.unmap_output(&output);
    test.state.unregister_output(&output);
    let focus = test.state.focused_window;
    test.state
        .process_input_event::<GestureInput>(InputEvent::GestureSwipeEnd {
            event: Sample::scroll(200, 0., false),
        });
    assert_eq!(test.state.focused_window, focus);
    assert!(test.state.scroll_gesture().is_none());
}
