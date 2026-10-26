use super::*;
use crate::Ferese;
use crate::input::gesture_tests::{fixture, messages};
use smithay::backend::input::{
    AbsolutePositionEvent, Event, GestureBeginEvent, GestureEndEvent, GestureHoldBeginEvent,
    GestureHoldEndEvent, GesturePinchBeginEvent, GesturePinchEndEvent, GesturePinchUpdateEvent,
    GestureSwipeBeginEvent, GestureSwipeEndEvent, GestureSwipeUpdateEvent, InputBackend,
    InputEvent, InputTime, KeyboardKeyEvent, PointerButtonEvent, TouchCancelEvent, TouchDownEvent,
    TouchEvent, TouchFrameEvent, TouchMotionEvent, TouchUpEvent, UnusedEvent,
};
use smithay::input::keyboard::FilterResult;
use smithay::utils::SERIAL_COUNTER;

#[derive(Debug)]
struct Input;

impl InputBackend for Input {
    type Device = TestDevice;
    type KeyboardKeyEvent = Sample;
    type PointerButtonEvent = Sample;
    type PointerMotionEvent = UnusedEvent;
    type PointerMotionAbsoluteEvent = UnusedEvent;
    type PointerAxisEvent = UnusedEvent;
    type GestureSwipeBeginEvent = Sample;
    type GestureSwipeUpdateEvent = Sample;
    type GestureSwipeEndEvent = Sample;
    type GesturePinchBeginEvent = Sample;
    type GesturePinchUpdateEvent = Sample;
    type GesturePinchEndEvent = Sample;
    type GestureHoldBeginEvent = Sample;
    type GestureHoldEndEvent = Sample;
    type TouchDownEvent = Sample;
    type TouchMotionEvent = Sample;
    type TouchUpEvent = Sample;
    type TouchFrameEvent = Sample;
    type TouchCancelEvent = Sample;
    type TabletToolAxisEvent = UnusedEvent;
    type TabletToolProximityEvent = UnusedEvent;
    type TabletToolTipEvent = UnusedEvent;
    type TabletToolButtonEvent = UnusedEvent;
    type SwitchToggleEvent = UnusedEvent;
    type SpecialEvent = UnusedEvent;
}

#[derive(Debug)]
struct Sample {
    device: TestDevice,
    code: u32,
    pressed: bool,
}

fn sample(device: u32, code: u32, pressed: bool) -> Sample {
    Sample {
        device: TestDevice(device),
        code,
        pressed,
    }
}

impl Event<Input> for Sample {
    fn time(&self) -> InputTime {
        InputTime::from_micros(1_000)
    }
    fn device(&self) -> TestDevice {
        self.device.clone()
    }
}
impl KeyboardKeyEvent<Input> for Sample {
    fn key_code(&self) -> Keycode {
        Keycode::new(self.code)
    }
    fn state(&self) -> KeyState {
        if self.pressed {
            KeyState::Pressed
        } else {
            KeyState::Released
        }
    }
    fn count(&self) -> u32 {
        1
    }
}
impl PointerButtonEvent<Input> for Sample {
    fn button_code(&self) -> u32 {
        self.code
    }
    fn state(&self) -> ButtonState {
        if self.pressed {
            ButtonState::Pressed
        } else {
            ButtonState::Released
        }
    }
}
impl TouchEvent<Input> for Sample {
    fn slot(&self) -> TouchSlot {
        Some(self.code).into()
    }
}
impl AbsolutePositionEvent<Input> for Sample {
    fn x(&self) -> f64 {
        100.0
    }
    fn y(&self) -> f64 {
        100.0
    }
    fn x_transformed(&self, _: i32) -> f64 {
        20.0
    }
    fn y_transformed(&self, _: i32) -> f64 {
        20.0
    }
}
impl TouchDownEvent<Input> for Sample {}
impl TouchMotionEvent<Input> for Sample {}
impl TouchUpEvent<Input> for Sample {}
impl TouchFrameEvent<Input> for Sample {}
impl TouchCancelEvent<Input> for Sample {}
impl GestureBeginEvent<Input> for Sample {
    fn fingers(&self) -> u32 {
        2
    }
}
impl GestureEndEvent<Input> for Sample {
    fn cancelled(&self) -> bool {
        false
    }
}
impl GestureSwipeBeginEvent<Input> for Sample {}
impl GestureSwipeEndEvent<Input> for Sample {}
impl GesturePinchBeginEvent<Input> for Sample {}
impl GesturePinchEndEvent<Input> for Sample {}
impl GestureHoldBeginEvent<Input> for Sample {}
impl GestureHoldEndEvent<Input> for Sample {}
impl GestureSwipeUpdateEvent<Input> for Sample {
    fn delta_x(&self) -> f64 {
        1.0
    }
    fn delta_y(&self) -> f64 {
        2.0
    }
}
impl GesturePinchUpdateEvent<Input> for Sample {
    fn delta_x(&self) -> f64 {
        1.0
    }
    fn delta_y(&self) -> f64 {
        2.0
    }
    fn scale(&self) -> f64 {
        1.5
    }
    fn rotation(&self) -> f64 {
        2.0
    }
}

fn private(name: &str) -> bool {
    crate::startup_tests::private_runtime(&format!("input::devices::tests::integration::{name}"))
}
fn add(state: &mut Ferese, device: u32) {
    state.process_input_event::<Input>(InputEvent::DeviceAdded {
        device: TestDevice(device),
    });
}
fn remove(state: &mut Ferese, device: u32) {
    state.process_input_event::<Input>(InputEvent::DeviceRemoved {
        device: TestDevice(device),
    });
}
fn key(state: &mut Ferese, device: u32, code: u32, pressed: bool) {
    state.process_input_event::<Input>(InputEvent::Keyboard {
        event: sample(device, code, pressed),
    });
}
fn button(state: &mut Ferese, device: u32, pressed: bool) {
    state.process_input_event::<Input>(InputEvent::PointerButton {
        event: sample(device, 0x110, pressed),
    });
}

#[test]
fn overlapping_keyboards_emit_one_pair_in_either_removal_order() {
    if !private("overlapping_keyboards_emit_one_pair_in_either_removal_order") {
        return;
    }
    let (mut state, _events, _wire) = fixture();
    let keyboard = state.seat.get_keyboard().unwrap();
    let forwarded = crate::input::session::tests::record(&mut state);
    for (a, b, departing, survivor) in [(1, 2, 1, 2), (3, 4, 4, 3)] {
        add(&mut state, a);
        add(&mut state, b);
        key(&mut state, a, 37, true);
        key(&mut state, b, 37, true);
        assert!(keyboard.modifier_state().ctrl);
        let before = forwarded.borrow().len();
        remove(&mut state, departing);
        assert!(keyboard.modifier_state().ctrl);
        assert_eq!(forwarded.borrow().len(), before);
        key(&mut state, departing, 37, false);
        assert!(
            keyboard.modifier_state().ctrl,
            "late removed-device event was ignored"
        );
        key(&mut state, survivor, 37, false);
        assert!(!keyboard.modifier_state().ctrl);
        remove(&mut state, survivor);
    }
    assert_eq!(
        *forwarded.borrow(),
        [
            (37, KeyState::Pressed),
            (37, KeyState::Released),
            (37, KeyState::Pressed),
            (37, KeyState::Released),
        ]
    );
}

#[test]
fn another_keyboard_cannot_repeat_or_release_an_intercepted_binding() {
    if !private("another_keyboard_cannot_repeat_or_release_an_intercepted_binding") {
        return;
    }
    let (mut state, _events, _wire) = fixture();
    state.bindings = crate::config::Config::parse_source("binding h toggle-floating")
        .unwrap()
        .bindings(&state.input_settings)
        .unwrap()
        .into();
    let window = state.focused_window.unwrap();
    add(&mut state, 1);
    add(&mut state, 2);
    key(&mut state, 1, 43, true);
    assert!(matches!(
        state.workspaces.placement(window),
        Some(ferese_core::WindowPlacement::Floating { .. })
    ));
    key(&mut state, 2, 43, true);
    assert!(matches!(
        state.workspaces.placement(window),
        Some(ferese_core::WindowPlacement::Floating { .. })
    ));
    key(&mut state, 1, 43, true);
    assert_eq!(
        state.workspaces.placement(window),
        Some(ferese_core::WindowPlacement::Tiled)
    );
    remove(&mut state, 1);
    assert!(state.intercepted_keys.contains(&Keycode::new(43)));
    remove(&mut state, 2);
    assert!(!state.intercepted_keys.contains(&Keycode::new(43)));
    assert!(state.binding_mode_pressed.is_empty());
}

#[test]
fn pause_and_device_removal_preserve_an_auxiliary_key_holder() {
    if !private("pause_and_device_removal_preserve_an_auxiliary_key_holder") {
        return;
    }
    let (mut state, _events, _wire) = fixture();
    add(&mut state, 1);
    add(&mut state, 2);
    key(&mut state, 1, 37, true);
    key(&mut state, 2, 37, true);
    key(&mut state, 2, 50, true);
    let keyboard = state.seat.get_keyboard().unwrap();
    let auxiliary = KeyboardSource::new_auxiliary();
    keyboard.input_from_source::<(), _>(
        auxiliary,
        &mut state,
        Keycode::new(37),
        KeyState::Pressed,
        SERIAL_COUNTER.next_serial(),
        InputTime::now(),
        |_, _, _| FilterResult::Forward,
    );
    remove(&mut state, 1);
    assert!(keyboard.modifier_state().ctrl);
    state.reset_session_input();
    assert!(keyboard.modifier_state().ctrl);
    assert!(!keyboard.modifier_state().shift);
    key(&mut state, 2, 37, false);
    assert!(keyboard.modifier_state().ctrl);
    keyboard.input_from_source::<(), _>(
        auxiliary,
        &mut state,
        Keycode::new(37),
        KeyState::Released,
        SERIAL_COUNTER.next_serial(),
        InputTime::now(),
        |state, _, _| state.filter_key_release(Keycode::new(37), InputTime::now()),
    );
    assert!(!keyboard.modifier_state().ctrl);
    assert!(state.binding_mode_pressed.is_empty());
    key(&mut state, 2, 37, true);
    assert!(keyboard.modifier_state().ctrl);
    key(&mut state, 2, 37, false);
    assert!(!keyboard.modifier_state().ctrl);
}

#[test]
fn overlapping_mice_keep_the_button_down_until_the_last_device_releases() {
    if !private("overlapping_mice_keep_the_button_down_until_the_last_device_releases") {
        return;
    }
    let (mut state, _events, mut wire) = fixture();
    let pointer = state.seat.get_pointer().unwrap();
    for (a, b, departing, survivor) in [(1, 2, 1, 2), (3, 4, 4, 3)] {
        add(&mut state, a);
        add(&mut state, b);
        button(&mut state, a, true);
        button(&mut state, b, true);
        assert!(pointer.is_grabbed());
        remove(&mut state, departing);
        assert!(state.pointer_buttons.contains(&0x110));
        assert!(pointer.is_grabbed());
        button(&mut state, departing, false);
        assert!(state.pointer_buttons.contains(&0x110));
        remove(&mut state, survivor);
        assert!(!state.pointer_buttons.contains(&0x110));
        assert!(!pointer.is_grabbed());
    }
    state.display_handle.flush_clients().unwrap();
    let states = messages(&mut wire)
        .into_iter()
        .filter(|message| message.object == 10 && message.opcode == 3)
        .map(|message| message.args[3])
        .collect::<Vec<_>>();
    assert_eq!(states, [1, 0, 1, 0]);
}

#[test]
fn removing_one_touchscreen_retires_only_its_remapped_contacts() {
    if !private("removing_one_touchscreen_retires_only_its_remapped_contacts") {
        return;
    }
    let (mut state, mut events, mut wire) = fixture();
    crate::state::test_client_request(&mut wire, 8, 2, &[14], None);
    crate::state::dispatch_test_client(&mut events, &mut state);
    messages(&mut wire);
    for device in [1, 2] {
        add(&mut state, device);
        state.process_input_event::<Input>(InputEvent::TouchDown {
            event: sample(device, 0, true),
        });
        state.process_input_event::<Input>(InputEvent::TouchFrame {
            event: sample(device, 0, true),
        });
    }
    remove(&mut state, 1);
    state.process_input_event::<Input>(InputEvent::TouchMotion {
        event: sample(1, 0, true),
    });
    state.process_input_event::<Input>(InputEvent::TouchMotion {
        event: sample(2, 0, true),
    });
    state.process_input_event::<Input>(InputEvent::TouchUp {
        event: sample(2, 0, false),
    });
    state.process_input_event::<Input>(InputEvent::TouchFrame {
        event: sample(2, 0, false),
    });
    state.display_handle.flush_clients().unwrap();
    let events = messages(&mut wire)
        .into_iter()
        .filter(|message| message.object == 14)
        .collect::<Vec<_>>();
    let down = events
        .iter()
        .filter(|message| message.opcode == 0)
        .map(|message| message.args[3])
        .collect::<Vec<_>>();
    assert_eq!(down.len(), 2);
    assert_ne!(down[0], down[1]);
    let up = events
        .iter()
        .filter(|message| message.opcode == 1)
        .map(|message| message.args[2])
        .collect::<Vec<_>>();
    assert_eq!(up, down);
    let motion = events
        .iter()
        .filter(|message| message.opcode == 2)
        .map(|message| message.args[1])
        .collect::<Vec<_>>();
    assert_eq!(motion, [down[1]]);
}

#[test]
fn removal_cancels_only_the_owning_device_gesture_and_rejects_late_updates() {
    if !private("removal_cancels_only_the_owning_device_gesture_and_rejects_late_updates") {
        return;
    }
    let (mut state, _events, mut wire) = fixture();
    add(&mut state, 1);
    add(&mut state, 2);
    state.process_input_event::<Input>(InputEvent::GestureSwipeBegin {
        event: sample(1, 0, true),
    });
    state.process_input_event::<Input>(InputEvent::GestureHoldBegin {
        event: sample(2, 0, true),
    });
    state.process_input_event::<Input>(InputEvent::GestureSwipeUpdate {
        event: sample(2, 0, true),
    });
    state.process_input_event::<Input>(InputEvent::GestureSwipeUpdate {
        event: sample(1, 0, true),
    });
    remove(&mut state, 1);
    state.process_input_event::<Input>(InputEvent::GestureSwipeUpdate {
        event: sample(1, 0, true),
    });
    state.process_input_event::<Input>(InputEvent::GestureSwipeEnd {
        event: sample(1, 0, false),
    });
    state.process_input_event::<Input>(InputEvent::GestureHoldEnd {
        event: sample(2, 0, false),
    });
    add(&mut state, 3);
    state.process_input_event::<Input>(InputEvent::GestureSwipeUpdate {
        event: sample(3, 0, true),
    });
    state.process_input_event::<Input>(InputEvent::GestureSwipeBegin {
        event: sample(3, 0, true),
    });
    state.process_input_event::<Input>(InputEvent::GestureSwipeEnd {
        event: sample(3, 0, false),
    });
    state.display_handle.flush_clients().unwrap();
    let events = messages(&mut wire);
    let swipe = events
        .iter()
        .filter(|message| message.object == 11)
        .collect::<Vec<_>>();
    assert_eq!(
        swipe
            .iter()
            .map(|message| message.opcode)
            .collect::<Vec<_>>(),
        [0, 1, 2, 0, 2]
    );
    assert_eq!(*swipe[2].args.last().unwrap(), 1);
    assert_eq!(*swipe[4].args.last().unwrap(), 0);
    let hold = events
        .iter()
        .filter(|message| message.object == 13)
        .collect::<Vec<_>>();
    assert_eq!(hold.len(), 2);
    assert_eq!(*hold[1].args.last().unwrap(), 0);
}
