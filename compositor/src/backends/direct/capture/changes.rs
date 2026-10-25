//! Capture invalidation is independent of display damage and consumer requests.
use std::sync::atomic::{AtomicU64, Ordering};

use smithay::output::Output;
use smithay::utils::{Logical, Physical, Rectangle, Size, Transform};

#[derive(Default)]
struct Changes {
    content: AtomicU64,
    cursor: AtomicU64,
}

pub(crate) fn content_changed(output: &Output) {
    if let Some(changes) = output.user_data().get::<Changes>() {
        changes.content.fetch_add(1, Ordering::Relaxed);
    }
}

pub(crate) fn cursor_changed(output: &Output) {
    if let Some(changes) = output.user_data().get::<Changes>() {
        changes.cursor.fetch_add(1, Ordering::Relaxed);
    }
}

pub(crate) fn tracked(output: &Output) -> bool {
    output.user_data().get::<Changes>().is_some()
}

/// Also compare output configuration: mode changes can precede a redraw request.
/// Keeping the source identity prevents a remapped mirror from reusing its old view.
#[derive(Debug, PartialEq)]
pub(crate) struct Stamp {
    output: Output,
    content: u64,
    cursor: Option<u64>,
    epoch: u64,
    cursor_protected: bool,
    size: Option<Size<i32, Physical>>,
    scale: f64,
    transform: Transform,
    geometry: Option<Rectangle<i32, Logical>>,
}

impl Stamp {
    pub fn new(state: &crate::Ferese, output: &Output, include_cursor: bool) -> Self {
        let changes = output
            .user_data()
            .get_or_insert_threadsafe(Changes::default);
        Self {
            output: output.clone(),
            content: changes.content.load(Ordering::Relaxed),
            cursor: include_cursor.then(|| changes.cursor.load(Ordering::Relaxed)),
            epoch: state.capture_epoch,
            cursor_protected: include_cursor && state.capture_cursor_protected(),
            size: output.current_mode().map(|mode| mode.size),
            scale: output.current_scale().fractional_scale(),
            transform: output.current_transform(),
            geometry: state.space.output_geometry(output),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use smithay::output::{Mode, PhysicalProperties, Scale, Subpixel};

    fn output() -> Output {
        let output = Output::new(
            "capture-test".into(),
            PhysicalProperties {
                size: (0, 0).into(),
                subpixel: Subpixel::Unknown,
                make: String::new(),
                model: String::new(),
                serial_number: String::new(),
            },
        );
        output.change_current_state(
            Some(Mode {
                size: (640, 480).into(),
                refresh: 60_000,
            }),
            Some(Transform::Normal),
            Some(Scale::Fractional(1.25)),
            Some((0, 0).into()),
        );
        output
    }

    #[test]
    fn consumer_demand_does_not_invalidate_but_scene_redraws_do() {
        if !crate::startup_tests::private_runtime(
            "backends::direct::capture::changes::tests::consumer_demand_does_not_invalidate_but_scene_redraws_do",
        ) {
            return;
        }
        let mut events = smithay::reexports::calloop::EventLoop::try_new().unwrap();
        let mut state = crate::startup_tests::state(&mut events);
        let first = output();
        let second = output();
        state.space.map_output(&first, (0, 0));
        state.space.map_output(&second, (640, 0));
        let before = Stamp::new(&state, &first, false);
        let other = Stamp::new(&state, &second, false);
        crate::backends::direct::schedule_capture(&mut state, std::slice::from_ref(&first));
        assert_eq!(before, Stamp::new(&state, &first, false));
        crate::backends::direct::render_on(&mut state, std::slice::from_ref(&first));
        assert_ne!(before, Stamp::new(&state, &first, false));
        assert_eq!(other, Stamp::new(&state, &second, false));
        let before = Stamp::new(&state, &first, false);
        state.defer_output_redraw(first.clone());
        assert_ne!(before, Stamp::new(&state, &first, false));
        crate::backends::direct::render_all(&mut state);
        assert_ne!(other, Stamp::new(&state, &second, false));
    }

    #[test]
    fn cursor_privacy_and_output_configuration_change_the_appropriate_view() {
        if !crate::startup_tests::private_runtime(
            "backends::direct::capture::changes::tests::cursor_privacy_and_output_configuration_change_the_appropriate_view",
        ) {
            return;
        }
        let mut events = smithay::reexports::calloop::EventLoop::try_new().unwrap();
        let mut state = crate::startup_tests::state(&mut events);
        let output = output();
        state.space.map_output(&output, (0, 0));
        let hidden = Stamp::new(&state, &output, false);
        let embedded = Stamp::new(&state, &output, true);
        crate::backends::direct::render_cursor(&mut state);
        assert_eq!(hidden, Stamp::new(&state, &output, false));
        assert_ne!(embedded, Stamp::new(&state, &output, true));
        state.capture_epoch += 1;
        assert_ne!(hidden, Stamp::new(&state, &output, false));
        let before = Stamp::new(&state, &output, false);
        output.change_current_state(None, Some(Transform::_180), None, None);
        assert_ne!(before, Stamp::new(&state, &output, false));
        let before = Stamp::new(&state, &output, false);
        output.change_current_state(None, None, Some(Scale::Fractional(1.5)), None);
        assert_ne!(before, Stamp::new(&state, &output, false));
        let before = Stamp::new(&state, &output, false);
        state.space.map_output(&output, (100, 0));
        assert_ne!(before, Stamp::new(&state, &output, false));
    }

    #[test]
    fn commits_and_the_final_animation_frame_invalidate_capture() {
        if !crate::startup_tests::private_runtime(
            "backends::direct::capture::changes::tests::commits_and_the_final_animation_frame_invalidate_capture",
        ) {
            return;
        }
        let mut events = smithay::reexports::calloop::EventLoop::try_new().unwrap();
        let mut state = crate::startup_tests::state(&mut events);
        let output = output();
        state.space.map_output(&output, (0, 0));
        state.register_output(&output, "capture-animation".into());
        let (_window, mut client) = crate::state::test_window(&mut state, &mut events, 0xffff0000);
        let before = Stamp::new(&state, &output, false);
        // Even a callback-only commit goes through the mutation path. Damage
        // tracking can subsequently decide that the retained pixels still match.
        crate::state::test_client_request(&mut client, 2, 6, &[], None);
        crate::state::dispatch_test_client(&mut events, &mut state);
        assert_ne!(before, Stamp::new(&state, &output, false));
        let mut now = std::time::Instant::now();
        state.advance_animations(now);
        assert!(state.output_has_animations(&output));
        let mut settled = false;
        for _ in 0..500 {
            let before = Stamp::new(&state, &output, false);
            now += std::time::Duration::from_millis(16);
            state.advance_animations(now);
            assert_ne!(before, Stamp::new(&state, &output, false));
            if !state.output_has_animations(&output) {
                settled = true;
                break;
            }
        }
        assert!(
            settled,
            "the opening/focus animation must reach its final frame"
        );
    }
}
