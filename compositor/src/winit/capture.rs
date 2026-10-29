//! Capture demand must progress even when the host withholds its frame callback.
use std::cell::RefCell;
use std::collections::HashSet;
use std::time::{Duration, Instant};

use smithay::output::Output;
use smithay::reexports::calloop::timer::{TimeoutAction, Timer};

use crate::Ferese;

pub(super) fn request(state: &mut Ferese) {
    if !state.space.outputs().any(|output| demanded(state, output)) {
        return;
    }
    let refresh = state
        .space
        .outputs()
        .next()
        .and_then(Output::current_mode)
        .map_or(60_000, |mode| mode.refresh.max(1));
    queue(
        state,
        Duration::from_nanos(1_000_000_000_000 / refresh as u64),
        pump,
    );
}

fn demanded(state: &Ferese, output: &Output) -> bool {
    state.has_pending_screencopy(output, false) || state.has_pending_screencopy(output, true)
}

fn queue(state: &mut Ferese, interval: Duration, mut capture: impl FnMut(&mut Ferese) + 'static) {
    if state.nested_frames.capture_timer.is_some() {
        return;
    }
    let now = Instant::now();
    let deadline = state
        .nested_frames
        .last_capture
        .map_or(now, |last| (last + interval).max(now));
    match state
        .loop_handle
        .insert_source(Timer::from_deadline(deadline), move |_, _, state| {
            state.nested_frames.last_capture = Some(Instant::now());
            // Keep this source marked while rendering: animation updates can request
            // another frame, but the current pump already incorporates those updates.
            capture(state);
            state.nested_frames.capture_timer = None;
            TimeoutAction::Drop
        }) {
        Ok(token) => state.nested_frames.capture_timer = Some(token),
        Err(error) => {
            tracing::warn!(%error, "could not schedule nested capture");
            fail_pending(state);
        }
    }
}

fn pump(state: &mut Ferese) {
    let outputs = state
        .space
        .outputs()
        .filter(|output| demanded(state, output))
        .cloned()
        .collect::<Vec<_>>();
    if outputs.is_empty() {
        return;
    }
    let Some(backend) = state.nested_backend.clone() else {
        fail_pending(state);
        return;
    };
    let Ok(mut backend) = backend.try_borrow_mut() else {
        tracing::warn!("nested capture renderer is busy");
        fail_pending(state);
        return;
    };
    state.advance_animations(Instant::now());
    for output in outputs {
        let scene = state.sample_frame(&output, Duration::ZERO);
        let wanted = [
            state.has_pending_screencopy(&output, false),
            state.has_pending_screencopy(&output, true),
        ];
        let mut targets = std::mem::take(&mut state.nested_capture_texture);
        crate::backends::direct::capture::capture_output(
            state,
            backend.renderer(),
            &mut targets,
            &output,
            &scene,
        );
        callbacks(state, &output, &targets, wanted);
        state.nested_capture_texture = targets;
    }
    drop(backend);
    if let Err(error) = state.display_handle.flush_clients() {
        tracing::debug!(%error, "could not flush nested capture callbacks");
    }
}

fn fail_pending(state: &mut Ferese) {
    let output_iter = state.space.outputs().cloned().collect::<Vec<_>>();
    for output in output_iter {
        state.fail_screencopies(&output, false);
        state.fail_screencopies(&output, true);
    }
}

fn callbacks(
    state: &mut Ferese,
    output: &Output,
    targets: &[Option<crate::backends::direct::capture::CaptureTarget>; 2],
    wanted: [bool; 2],
) {
    if state.session_lock.active() {
        return;
    }
    let time = state.start_time.elapsed();
    let captured = RefCell::new(HashSet::new());
    let visible =
        |surface: &smithay::reexports::wayland_server::protocol::wl_surface::WlSurface| {
            targets
                .iter()
                .zip(wanted)
                .any(|(target, wanted)| {
                    wanted
                        && target
                            .as_ref()
                            .is_some_and(|target| target.surface_visible(surface))
                })
                .then(|| {
                    captured.borrow_mut().insert(surface.clone());
                    output.clone()
                })
        };
    let window_iter = state.space.elements().filter(|window| {
        !state.capture_protected(window)
            && state
                .windows
                .ids()
                .get(*window)
                .is_some_and(|id| state.window_belongs_to_output(*id, output))
    });
    for window in window_iter {
        window.send_frame(output, time, None, |surface, _| visible(surface));
    }
    for layer in super::layer_surfaces(output) {
        layer.send_frame(output, time, None, |surface, _| visible(surface));
    }
    if !state.capture_cursor_protected()
        && let smithay::input::pointer::CursorImageStatus::Surface(surface) = &state.cursor_status
    {
        let _ = visible(surface);
    }
    state.presentation_constraint_captured(|surface| captured.borrow().contains(surface));
}

#[cfg(test)]
mod tests {
    use super::*;
    use smithay::reexports::calloop::EventLoop;
    use std::cell::Cell;
    use std::rc::Rc;

    #[test]
    fn capture_demand_is_coalesced_and_progresses_without_a_host_redraw() {
        if !crate::startup_tests::private_runtime(
            "winit::capture::tests::capture_demand_is_coalesced_and_progresses_without_a_host_redraw",
        ) {
            return;
        }
        let mut events = EventLoop::try_new().unwrap();
        let mut state = crate::startup_tests::state(&mut events);
        // A host callback can remain withheld indefinitely while occluded.
        state.nested_frames.requested = true;
        let calls = Rc::new(Cell::new(0));
        for _ in 0..100 {
            let calls = calls.clone();
            queue(&mut state, Duration::ZERO, move |_| {
                calls.set(calls.get() + 1)
            });
        }
        events
            .dispatch(Duration::from_millis(2), &mut state)
            .unwrap();
        assert_eq!(calls.get(), 1);
        assert!(state.nested_frames.requested);
        assert!(state.nested_frames.capture_timer.is_none());
        let next = calls.clone();
        queue(&mut state, Duration::ZERO, move |_| {
            next.set(next.get() + 1)
        });
        events
            .dispatch(Duration::from_millis(2), &mut state)
            .unwrap();
        assert_eq!(calls.get(), 2);
        events
            .dispatch(Duration::from_millis(20), &mut state)
            .unwrap();
        assert_eq!(
            calls.get(),
            2,
            "capture demand must leave no periodic idle timer"
        );
        assert!(state.nested_frames.capture_timer.is_none());
    }
}
