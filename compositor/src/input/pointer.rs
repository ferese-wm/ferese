use super::{AxisSource, BindingAction, Ferese, PointerHandle, Serial, with_pointer_constraint};
use crate::config::{PointerTrigger, ScrollDirection};
use std::collections::HashMap;
use std::time::{Duration, Instant};

#[derive(Clone, Debug, PartialEq, Eq)]
struct ScrollContext {
    modifiers: u8,
    mode: Option<String>,
    revision: u64,
}

#[derive(Default)]
struct ScrollAxis {
    remainder: f64,
    context: Option<ScrollContext>,
    finger_owner: Option<bool>,
    last_action: Option<Instant>,
}

impl ScrollAxis {
    fn feed(
        &mut self,
        delta: Option<f64>,
        finger: bool,
        context: &ScrollContext,
        bound: bool,
        now: Instant,
        cooldown: Duration,
    ) -> (bool, i32) {
        let owned = if finger {
            *self.finger_owner.get_or_insert(bound)
        } else {
            bound
        };
        let Some(delta) = delta else {
            *self = Self::default();
            return (owned, 0);
        };
        if self.context.as_ref() != Some(context) {
            self.remainder = 0.0;
            self.last_action = None;
            if finger && self.context.is_some() && owned {
                // Finish an intercepted finger gesture without delivering its tail
                // or changing actions halfway through it.
                return (true, 0);
            }
            self.context = Some(context.clone());
        }
        if !owned || !bound || !delta.is_finite() {
            self.remainder = 0.0;
            return (owned, 0);
        }
        if self.remainder.signum() != delta.signum() {
            self.remainder = 0.0;
        }
        self.remainder += delta;
        let threshold = if finger { 15.0 } else { 120.0 };
        let ticks = (self.remainder / threshold).trunc().clamp(-32.0, 32.0) as i32;
        self.remainder %= threshold;
        if ticks == 0
            || self
                .last_action
                .is_some_and(|last| now.duration_since(last) < cooldown)
        {
            return (true, 0);
        }
        self.last_action = Some(now);
        (
            true,
            if cooldown.is_zero() {
                ticks
            } else {
                ticks.signum()
            },
        )
    }
}

#[derive(Default)]
struct DeviceScroll {
    axes: [ScrollAxis; 2],
    fraction: [f64; 2],
    factor: f64,
}

#[derive(Default)]
pub(crate) struct ScrollBindings {
    devices: HashMap<String, DeviceScroll>,
}

impl ScrollBindings {
    pub(super) fn remove_device(&mut self, device: &str) {
        self.devices.remove(device);
    }
}

impl Ferese {
    fn pointer_bindings_allowed(&self, pointer: &PointerHandle<Self>) -> bool {
        !self.session_lock.active()
            && !self.input_capture.active()
            && !self.overview.is_active()
            && self.active_shortcuts_inhibitor.is_none()
            && !pointer.is_grabbed()
            && !pointer.current_focus().is_some_and(|surface| {
                with_pointer_constraint(&surface, pointer, |constraint| {
                    constraint.is_some_and(|constraint| constraint.is_active())
                })
            })
    }

    pub(super) fn handle_pointer_binding(
        &mut self,
        pointer: &PointerHandle<Self>,
        button: u32,
        serial: Serial,
    ) -> bool {
        if !self.pointer_bindings_allowed(pointer) {
            return false;
        }
        let Some(keyboard) = self.seat.get_keyboard() else {
            return false;
        };
        let action = self
            .binding_modes
            .bindings(&self.bindings)
            .binding_for_pointer(PointerTrigger::Button(button), keyboard.modifier_state())
            .map(|binding| binding.action.clone());
        let Some(action) = action else {
            return false;
        };
        if matches!(
            action,
            BindingAction::PointerMove | BindingAction::PointerResize
        ) {
            return self.start_pointer_window_grab(
                pointer,
                button,
                serial,
                action == BindingAction::PointerResize,
            );
        }
        // Bindings use the current keyboard focus, like ordinary shortcuts.
        self.consumed_pointer_buttons.insert(button);
        self.execute_binding(action);
        true
    }
}

impl Ferese {
    pub(super) fn filter_pointer_scroll(
        &mut self,
        device: String,
        source: AxisSource,
        values: [f64; 2],
        v120: [Option<f64>; 2],
        stops: [bool; 2],
    ) -> [bool; 2] {
        let Some(pointer) = self.seat.get_pointer() else {
            return [false; 2];
        };
        let Some(keyboard) = self.seat.get_keyboard() else {
            return [false; 2];
        };
        let modifiers = keyboard.modifier_state();
        let allowed = self.pointer_bindings_allowed(&pointer);
        let finger = source == AxisSource::Finger;
        let wheel = matches!(source, AxisSource::Wheel | AxisSource::WheelTilt);
        if !finger && !wheel {
            return [false; 2];
        }
        let context = ScrollContext {
            modifiers: (modifiers.logo as u8)
                | ((modifiers.ctrl as u8) << 1)
                | ((modifiers.alt as u8) << 2)
                | ((modifiers.shift as u8) << 3),
            mode: self.binding_modes.active().map(str::to_owned),
            revision: self.ipc_events.config_revision,
        };
        let bindings = self.binding_modes.bindings(&self.bindings);
        let actions = [
            ScrollDirection::Left,
            ScrollDirection::Right,
            ScrollDirection::Up,
            ScrollDirection::Down,
        ]
        .map(|direction| {
            allowed
                .then(|| {
                    bindings.binding_for_pointer(
                        if finger {
                            PointerTrigger::Touchpad(direction)
                        } else {
                            PointerTrigger::Wheel(direction)
                        },
                        modifiers,
                    )
                })
                .flatten()
                .map(|binding| binding.action.clone())
        });
        // Bound storage even for synthetic backends that invent device IDs.
        if self.pointer_scroll.devices.len() >= 64
            && !self.pointer_scroll.devices.contains_key(&device)
        {
            self.pointer_scroll.devices.clear();
        }
        let axes = &mut self.pointer_scroll.devices.entry(device).or_default().axes;
        let mut consumed = [false; 2];
        let mut steps = [0; 2];
        for axis in 0..2 {
            if values[axis] == 0.0 && v120[axis].is_none() && !stops[axis] {
                continue;
            }
            let delta = if finger {
                values[axis]
            } else {
                v120[axis].unwrap_or(values[axis] * 8.0)
            };
            (consumed[axis], steps[axis]) = axes[axis].feed(
                (!stops[axis]).then_some(delta),
                finger,
                &context,
                actions[axis * 2].is_some() || actions[axis * 2 + 1].is_some(),
                Instant::now(),
                Duration::from_millis(self.input_settings.mouse.scroll_cooldown_ms),
            );
        }
        let axis_iter = steps.into_iter().enumerate();
        for (axis, steps) in axis_iter {
            let index = axis * 2 + usize::from(steps > 0);
            if let Some(action) = &actions[index] {
                for _ in 0..steps.unsigned_abs() {
                    if self.session_lock.active() || self.input_capture.active() {
                        break;
                    }
                    self.execute_binding(action.clone());
                }
            }
        }
        consumed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn context(modifiers: u8) -> ScrollContext {
        ScrollContext {
            modifiers,
            mode: None,
            revision: 0,
        }
    }

    #[test]
    fn wheel_accumulates_high_resolution_steps_and_discards_reversed_remainder() {
        let mut axis = ScrollAxis::default();
        let now = Instant::now();
        let feed = |axis: &mut ScrollAxis, amount| {
            axis.feed(Some(amount), false, &context(1), true, now, Duration::ZERO)
        };
        assert_eq!(feed(&mut axis, 60.0), (true, 0));
        assert_eq!(feed(&mut axis, 60.0), (true, 1));
        assert_eq!(feed(&mut axis, 90.0), (true, 0));
        assert_eq!(feed(&mut axis, -60.0), (true, 0));
        assert_eq!(feed(&mut axis, -60.0), (true, -1));
        assert_eq!(feed(&mut axis, f64::NAN), (true, 0));
    }

    #[test]
    fn wheel_remainder_does_not_cross_modifier_or_config_changes() {
        let mut axis = ScrollAxis::default();
        let now = Instant::now();
        axis.feed(Some(60.0), false, &context(1), true, now, Duration::ZERO);
        assert_eq!(
            axis.feed(Some(60.0), false, &context(2), true, now, Duration::ZERO),
            (true, 0)
        );
        let mut updated = context(2);
        updated.revision = 1;
        assert_eq!(
            axis.feed(Some(60.0), false, &updated, true, now, Duration::ZERO),
            (true, 0)
        );
    }

    #[test]
    fn finger_gesture_keeps_ownership_until_stop() {
        let now = Instant::now();
        let mut axis = ScrollAxis::default();
        assert_eq!(
            axis.feed(Some(15.0), true, &context(1), true, now, Duration::ZERO),
            (true, 1)
        );
        assert_eq!(
            axis.feed(Some(15.0), true, &context(0), false, now, Duration::ZERO),
            (true, 0)
        );
        assert_eq!(
            axis.feed(None, true, &context(0), false, now, Duration::ZERO),
            (true, 0)
        );
        assert_eq!(
            axis.feed(Some(15.0), true, &context(0), false, now, Duration::ZERO),
            (false, 0)
        );
        assert_eq!(
            axis.feed(Some(15.0), true, &context(1), true, now, Duration::ZERO),
            (false, 0)
        );
        assert_eq!(
            axis.feed(None, true, &context(1), true, now, Duration::ZERO),
            (false, 0)
        );
    }

    #[test]
    fn cooldown_consumes_ticks_without_replaying_them_later() {
        let mut axis = ScrollAxis::default();
        let now = Instant::now();
        let cooldown = Duration::from_millis(150);
        assert_eq!(
            axis.feed(Some(120.0), false, &context(1), true, now, cooldown),
            (true, 1)
        );
        assert_eq!(
            axis.feed(Some(240.0), false, &context(1), true, now, cooldown),
            (true, 0)
        );
        assert_eq!(
            axis.feed(
                Some(120.0),
                false,
                &context(1),
                true,
                now + cooldown,
                cooldown
            ),
            (true, 1)
        );
    }
}

impl Ferese {
    pub(super) fn scale_mouse_scroll(
        &mut self,
        device: String,
        source: AxisSource,
        values: [f64; 2],
        v120: [Option<f64>; 2],
    ) -> ([f64; 2], [Option<i32>; 2]) {
        if !matches!(source, AxisSource::Wheel | AxisSource::WheelTilt) {
            return (values, v120.map(|value| value.map(|value| value as i32)));
        }
        if self.pointer_scroll.devices.len() >= 64
            && !self.pointer_scroll.devices.contains_key(&device)
        {
            self.pointer_scroll.devices.clear();
        }
        let device = self.pointer_scroll.devices.entry(device).or_default();
        let factor = self.input_settings.mouse.scroll_factor;
        if device.factor != factor {
            device.factor = factor;
            device.fraction = [0.0; 2];
        }
        let scaled = std::array::from_fn(|axis| {
            v120[axis].filter(|value| value.is_finite()).map(|value| {
                let value = value * factor + device.fraction[axis];
                device.fraction[axis] = value.fract();
                value as i32
            })
        });
        (values.map(|value| value * factor), scaled)
    }
}

#[cfg(test)]
mod routing_tests {
    use super::*;
    use crate::input::{
        AbsolutePositionEvent, ButtonEvent, ButtonState, Event, InputBackend, InputEvent,
        PointerButtonEvent, PointerMotionEvent, SERIAL_COUNTER,
    };
    use smithay::backend::input::UnusedEvent;
    use smithay::backend::winit::WinitVirtualDevice;

    #[derive(Debug)]
    struct Input;
    impl InputBackend for Input {
        type Device = WinitVirtualDevice;
        type PointerButtonEvent = Button;
        type KeyboardKeyEvent = UnusedEvent;
        type PointerAxisEvent = UnusedEvent;
        type PointerMotionEvent = Motion;
        type PointerMotionAbsoluteEvent = Motion;
        type GestureSwipeBeginEvent = UnusedEvent;
        type GestureSwipeUpdateEvent = UnusedEvent;
        type GestureSwipeEndEvent = UnusedEvent;
        type GesturePinchBeginEvent = UnusedEvent;
        type GesturePinchUpdateEvent = UnusedEvent;
        type GesturePinchEndEvent = UnusedEvent;
        type GestureHoldBeginEvent = UnusedEvent;
        type GestureHoldEndEvent = UnusedEvent;
        type TouchDownEvent = UnusedEvent;
        type TouchUpEvent = UnusedEvent;
        type TouchMotionEvent = UnusedEvent;
        type TouchCancelEvent = UnusedEvent;
        type TouchFrameEvent = UnusedEvent;
        type TabletToolAxisEvent = UnusedEvent;
        type TabletToolProximityEvent = UnusedEvent;
        type TabletToolTipEvent = UnusedEvent;
        type TabletToolButtonEvent = UnusedEvent;
        type SwitchToggleEvent = UnusedEvent;
        type SpecialEvent = UnusedEvent;
    }

    #[derive(Debug)]
    struct Motion(f64, f64);

    impl Event<Input> for Motion {
        fn time(&self) -> smithay::backend::input::InputTime {
            smithay::backend::input::InputTime::from_millis(1)
        }
        fn device(&self) -> WinitVirtualDevice {
            WinitVirtualDevice
        }
    }

    impl PointerMotionEvent<Input> for Motion {
        fn delta_x(&self) -> f64 {
            self.0
        }
        fn delta_y(&self) -> f64 {
            self.1
        }
        fn delta_x_unaccel(&self) -> f64 {
            self.0
        }
        fn delta_y_unaccel(&self) -> f64 {
            self.1
        }
    }

    impl AbsolutePositionEvent<Input> for Motion {
        fn x(&self) -> f64 {
            self.0
        }
        fn y(&self) -> f64 {
            self.1
        }
        fn x_transformed(&self, _width: i32) -> f64 {
            self.0
        }
        fn y_transformed(&self, _height: i32) -> f64 {
            self.1
        }
    }

    impl smithay::backend::input::PointerMotionAbsoluteEvent<Input> for Motion {}

    #[derive(Debug)]
    struct Button(bool);
    impl Event<Input> for Button {
        fn time(&self) -> smithay::backend::input::InputTime {
            smithay::backend::input::InputTime::from_millis(1)
        }
        fn device(&self) -> WinitVirtualDevice {
            WinitVirtualDevice
        }
    }
    impl PointerButtonEvent<Input> for Button {
        fn button_code(&self) -> u32 {
            275
        }
        fn state(&self) -> ButtonState {
            if self.0 {
                ButtonState::Pressed
            } else {
                ButtonState::Released
            }
        }
    }

    #[test]
    fn mouse_motion_within_a_window_preserves_shortcut_focus() {
        if !crate::startup_tests::private_runtime(
            "input::pointer::routing_tests::mouse_motion_within_a_window_preserves_shortcut_focus",
        ) {
            return;
        }
        for absolute in [false, true] {
            let (mut state, mut events, _output) = crate::input::layer_focus_tests::fixture();
            state
                .reload_config_source(
                    r#"
                    animations { enabled #false; }
                    input { focus-follows-mouse #true; }
                    scrolling { default-column-width 0.3; }
                    binding "Super+Shift+WheelUp" action="focus" argument="left"
                    binding "Super+MouseBack" action="focus" argument="left"
                    "#
                    .into(),
                )
                .unwrap();
            let (first, _first_wire) =
                crate::state::test_window(&mut state, &mut events, 0xff112233);
            let (second, _second_wire) =
                crate::state::test_window(&mut state, &mut events, 0xff334455);
            let (third, _third_wire) =
                crate::state::test_window(&mut state, &mut events, 0xff556677);
            let first_id = state.windows.ids()[&first];
            let second_id = state.windows.ids()[&second];
            let third_id = state.windows.ids()[&third];
            let pointer = state.seat.get_pointer().unwrap();
            let keyboard = state.seat.get_keyboard().unwrap();
            let motion = |state: &mut Ferese, x, y| {
                let current = pointer.current_location();
                state.process_input_event::<Input>(if absolute {
                    InputEvent::PointerMotionAbsolute {
                        event: Motion(x, y),
                    }
                } else {
                    InputEvent::PointerMotion {
                        event: Motion(x - current.x, y - current.y),
                    }
                });
            };
            let rect = state.presented_window_rect(third_id).unwrap();
            let (x, y) = (rect.x + 20., rect.y + 20.);
            motion(&mut state, x, y);
            assert_eq!(state.focused_window, Some(third_id));

            crate::input::binding_mode_tests::key(&mut state, 133, true); // Super.
            crate::input::binding_mode_tests::key(&mut state, 50, true); // Shift.
            assert_eq!(
                state.filter_pointer_scroll(
                    "mouse".into(),
                    AxisSource::Wheel,
                    [0., -15.],
                    [None, Some(-120.)],
                    [false; 2],
                ),
                [false, true]
            );
            assert_eq!(state.focused_window, Some(second_id));
            motion(&mut state, x + 1., y);
            assert_eq!(
                state.focused_window,
                Some(second_id),
                "wheel focus must survive mouse jitter"
            );

            crate::input::binding_mode_tests::key(&mut state, 50, false);
            state.process_input_event::<Input>(InputEvent::PointerButton {
                event: Button(true),
            });
            assert_eq!(state.focused_window, Some(first_id));
            motion(&mut state, x + 2., y);
            assert_eq!(
                state.focused_window,
                Some(first_id),
                "side-button focus must survive mouse jitter"
            );
            state.process_input_event::<Input>(InputEvent::PointerButton {
                event: Button(false),
            });
            crate::input::binding_mode_tests::key(&mut state, 133, false);
            motion(&mut state, x + 3., y);
            assert_eq!(
                state.focused_window,
                Some(first_id),
                "releasing modifiers must preserve the selection"
            );
            assert_eq!(
                keyboard.current_focus(),
                Some(first.toplevel().unwrap().wl_surface().clone())
            );

            let rect = state.presented_window_rect(second_id).unwrap();
            motion(&mut state, rect.x + 20., rect.y + 20.);
            assert_eq!(
                state.focused_window,
                Some(second_id),
                "entering a different window must focus it"
            );
            motion(&mut state, x, y);
            assert_eq!(state.focused_window, Some(third_id));

            state.focus_direction(ferese_layout::Direction::Left);
            motion(&mut state, x + 1., y);
            assert_eq!(state.focused_window, Some(second_id));
            // A click still explicitly focuses the window under the pointer.
            state.process_input_event::<Input>(InputEvent::PointerButton {
                event: Button(true),
            });
            assert_eq!(state.focused_window, Some(third_id));
            state.process_input_event::<Input>(InputEvent::PointerButton {
                event: Button(false),
            });
        }
    }

    #[test]
    fn viewport_scroll_under_pointer_does_not_restore_hover_focus() {
        if !crate::startup_tests::private_runtime(
            "input::pointer::routing_tests::viewport_scroll_under_pointer_does_not_restore_hover_focus",
        ) {
            return;
        }
        let (mut state, mut events, _output) = crate::input::layer_focus_tests::fixture();
        state.input_settings.focus_follows_mouse = true;
        let (first, _first_wire) = crate::state::test_window(&mut state, &mut events, 0xff112233);
        let (_second, _second_wire) =
            crate::state::test_window(&mut state, &mut events, 0xff334455);
        let (third, _third_wire) = crate::state::test_window(&mut state, &mut events, 0xff556677);
        let first_id = state.windows.ids()[&first];
        let rect = state
            .presented_window_rect(state.windows.ids()[&third])
            .unwrap();
        let pointer = state.seat.get_pointer().unwrap();
        state.process_input_event::<Input>(InputEvent::PointerMotionAbsolute {
            event: Motion(rect.x + 20., rect.y + 20.),
        });
        assert_eq!(
            state.window_under_visual(pointer.current_location()),
            Some(third.clone())
        );
        state.focus_direction(ferese_layout::Direction::Left);
        state.focus_direction(ferese_layout::Direction::Left);
        assert_eq!(state.focused_window, Some(first_id));
        assert_ne!(
            state.window_under_visual(pointer.current_location()),
            Some(third)
        );
        state.process_input_event::<Input>(InputEvent::PointerMotion {
            event: Motion(1., 0.),
        });
        assert_eq!(
            state.focused_window,
            Some(first_id),
            "layout movement beneath the pointer is not a hover entry"
        );
    }

    #[test]
    fn pointer_bindings_use_modes_and_consume_releases_after_modifier_changes() {
        if !crate::startup_tests::private_runtime(
            "input::pointer::routing_tests::pointer_bindings_use_modes_and_consume_releases_after_modifier_changes",
        ) {
            return;
        }
        let mut events = smithay::reexports::calloop::EventLoop::try_new().unwrap();
        let mut state = crate::startup_tests::state(&mut events);
        state
            .reload_config_source(
                r#"
            binding "Super+MouseBack" action="binding-mode" argument="browse"
            binding-mode "browse" { binding "MouseBack" action="binding-mode" argument="default"; }
        "#
                .into(),
            )
            .unwrap();
        let pointer = state.seat.get_pointer().unwrap();
        super::super::binding_mode_tests::key(&mut state, 133, true);
        state.process_input_event::<Input>(InputEvent::PointerButton {
            event: Button(true),
        });
        assert_eq!(state.binding_modes.active(), Some("browse"));
        assert!(state.consumed_pointer_buttons.contains(&275));
        assert!(!pointer.is_grabbed());
        super::super::binding_mode_tests::key(&mut state, 133, false);
        state.process_input_event::<Input>(InputEvent::PointerButton {
            event: Button(false),
        });
        assert!(!state.consumed_pointer_buttons.contains(&275));
        state.process_input_event::<Input>(InputEvent::PointerButton {
            event: Button(true),
        });
        assert_eq!(state.binding_modes.active(), None);
        state.process_input_event::<Input>(InputEvent::PointerButton {
            event: Button(false),
        });
        // A missing release after unplugging must not poison the next click.
        state.consumed_pointer_buttons.insert(275);
        // With no matching modifiers, the application receives an ordinary click grab.
        state.process_input_event::<Input>(InputEvent::PointerButton {
            event: Button(true),
        });
        assert!(
            pointer
                .with_grab(|_, grab| grab.is::<smithay::input::pointer::ClickGrab<Ferese>>())
                .unwrap()
        );
        assert!(!state.consumed_pointer_buttons.contains(&275));
        state.process_input_event::<Input>(InputEvent::PointerButton {
            event: Button(false),
        });
        assert!(!pointer.is_grabbed());
    }

    #[test]
    fn pointer_scroll_routes_axes_independently_and_preserves_fractional_ticks() {
        if !crate::startup_tests::private_runtime(
            "input::pointer::routing_tests::pointer_scroll_routes_axes_independently_and_preserves_fractional_ticks",
        ) {
            return;
        }
        let mut events = smithay::reexports::calloop::EventLoop::try_new().unwrap();
        let mut state = crate::startup_tests::state(&mut events);
        state
            .reload_config_source(
                r#"
            binding "WheelUp" action="none"
            binding "Super+TouchpadScrollLeft" action="none"
            input { mouse { scroll-factor 0.25; } }
        "#
                .into(),
            )
            .unwrap();
        let filtered = state.filter_pointer_scroll(
            "mouse".into(),
            AxisSource::Wheel,
            [15.0, -15.0],
            [Some(120.0), Some(-120.0)],
            [false; 2],
        );
        assert_eq!(filtered, [false, true]);
        for index in 1..=4 {
            let (values, ticks) = state.scale_mouse_scroll(
                "mouse".into(),
                AxisSource::Wheel,
                [1.0, 0.0],
                [Some(1.0), None],
            );
            assert_eq!(values, [0.25, 0.0]);
            assert_eq!(ticks, [Some(i32::from(index == 4)), None]);
        }
        let (values, ticks) =
            state.scale_mouse_scroll("touchpad".into(), AxisSource::Finger, [1.0, 2.0], [None; 2]);
        assert_eq!(values, [1.0, 2.0]);
        assert_eq!(ticks, [None; 2]);
        // Pointer grabs retain their scroll events even when a binding exists.
        let pointer = state.seat.get_pointer().unwrap();
        pointer.button(
            &mut state,
            &ButtonEvent {
                button: 272,
                state: ButtonState::Pressed,
                serial: SERIAL_COUNTER.next_serial(),
                time: smithay::backend::input::InputTime::from_millis(1),
            },
        );
        assert_eq!(
            state.filter_pointer_scroll(
                "mouse".into(),
                AxisSource::Wheel,
                [0.0, -15.0],
                [None, Some(-120.0)],
                [false; 2]
            ),
            [false; 2]
        );
    }
}
