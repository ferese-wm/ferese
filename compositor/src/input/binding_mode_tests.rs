use super::*;
use smithay::backend::input::UnusedEvent;
use smithay::backend::winit::WinitVirtualDevice;
use smithay::input::keyboard::Keycode;
use smithay::reexports::{calloop::EventLoop, wayland_server::Display};

#[derive(Debug)]
struct TestInput;

impl InputBackend for TestInput {
    type Device = WinitVirtualDevice;
    type PointerButtonEvent = UnusedEvent;
    type KeyboardKeyEvent = TestKey;
    type PointerAxisEvent = UnusedEvent;
    type PointerMotionEvent = UnusedEvent;
    type PointerMotionAbsoluteEvent = UnusedEvent;
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
struct TestKey(Keycode, KeyState);
impl Event<TestInput> for TestKey {
    fn time(&self) -> smithay::backend::input::InputTime {
        smithay::backend::input::InputTime::from_micros(1_000)
    }
    fn device(&self) -> WinitVirtualDevice {
        WinitVirtualDevice
    }
}
impl KeyboardKeyEvent<TestInput> for TestKey {
    fn key_code(&self) -> Keycode {
        self.0
    }
    fn state(&self) -> KeyState {
        self.1
    }
    fn count(&self) -> u32 {
        1
    }
}
pub(crate) fn key(state: &mut Ferese, code: u32, pressed: bool) {
    state.process_input_event::<TestInput>(InputEvent::Keyboard {
        event: TestKey(
            Keycode::new(code),
            if pressed { KeyState::Pressed } else { KeyState::Released },
        ),
    });
}
fn tap(state: &mut Ferese, code: u32) {
    key(state, code, true);
    key(state, code, false);
}
const SOURCE: &str = r#"
binding "Super+r" binding-mode resize
binding-mode resize { binding h resize left; binding l binding-mode launch; }
binding-mode launch { binding h binding-mode default; }
"#;
fn state(events: &mut EventLoop<'static, Ferese>) -> Ferese {
    let (_, mut config, _) = crate::config::prepare(SOURCE, std::path::Path::new(".")).unwrap();
    config.wallpaper.path = None;
    let mut state = Ferese::new(events, Display::new().unwrap(), config).unwrap();
    state.config_source = Some(SOURCE.into());
    let mut sections = ferese_config::Document::parse(SOURCE).unwrap().value().clone();
    sections.as_object_mut().unwrap().remove("theme");
    state.config_sections = Some(sections);
    state
}
#[test]
fn keyboard_modes_pair_releases_and_handle_held_modifiers_and_keys() {
    if !crate::startup_tests::private_runtime(
        "input::binding_mode_tests::keyboard_modes_pair_releases_and_handle_held_modifiers_and_keys",
    ) {
        return;
    }
    let mut events = EventLoop::try_new().unwrap();
    let mut state = state(&mut events);
    // Enter with Super held. A new unmodified mode shortcut must not match
    // until the modifier is released; Escape remains usable throughout.
    key(&mut state, 133, true);
    key(&mut state, 27, true);
    assert_eq!(state.binding_modes.active(), Some("resize"));
    assert!(state.intercepted_keys.contains(&Keycode::new(27)));
    tap(&mut state, 46); // Super+l does not switch to launch.
    assert_eq!(state.binding_modes.active(), Some("resize"));
    tap(&mut state, 9); // Super+Escape always leaves.
    assert_eq!(state.binding_modes.active(), None);
    key(&mut state, 27, false);
    key(&mut state, 133, false);
    assert!(state.intercepted_keys.is_empty());

    // An application-owned h stays application-owned after entering a mode.
    key(&mut state, 43, true);
    state.set_binding_mode("launch");
    key(&mut state, 43, true);
    assert_eq!(state.binding_modes.active(), Some("launch"));
    assert!(!state.intercepted_keys.contains(&Keycode::new(43)));
    key(&mut state, 43, false);
    assert_eq!(state.binding_modes.active(), Some("launch"));
    tap(&mut state, 43);
    assert_eq!(state.binding_modes.active(), None);

    // A consumed resize key retains release ownership across an explicit exit.
    state.set_binding_mode("resize");
    key(&mut state, 43, true);
    assert!(state.intercepted_keys.contains(&Keycode::new(43)));
    tap(&mut state, 9);
    assert!(state.intercepted_keys.contains(&Keycode::new(43)));
    key(&mut state, 43, false);
    assert!(state.intercepted_keys.is_empty());
    assert!(state.binding_mode_pressed.is_empty());
    key(&mut state, 9, true);
    state.set_binding_mode("launch");
    key(&mut state, 9, false);
    assert_eq!(
        state.binding_modes.active(),
        Some("launch"),
        "an application-owned Escape release cannot exit a new mode"
    );
    tap(&mut state, 9);
    assert_eq!(state.binding_modes.active(), None);
}

#[test]
fn reload_transitions_cancel_modes_without_losing_key_ownership() {
    if !crate::startup_tests::private_runtime(
        "input::binding_mode_tests::reload_transitions_cancel_modes_without_losing_key_ownership",
    ) {
        return;
    }
    let mut events = EventLoop::try_new().unwrap();
    let mut state = state(&mut events);
    state.set_binding_mode("resize");
    key(&mut state, 43, true);
    assert!(state.reload_config_source("binding-mode default {}".into()).is_err());
    assert_eq!(state.binding_modes.active(), Some("resize"));
    assert!(state.intercepted_keys.contains(&Keycode::new(43)));
    state
        .reload_config_source(format!("{SOURCE}\nanimations {{ speed 0.75; }}"))
        .unwrap();
    assert_eq!(
        state.binding_modes.active(),
        Some("resize"),
        "unrelated reload preserves mode"
    );
    state
        .reload_config_source(format!("{SOURCE}\ninput {{ repeat-rate 30; }}"))
        .unwrap();
    assert_eq!(state.binding_modes.active(), None, "input reload exits mode");
    state.set_binding_mode("resize");
    state
        .reload_config_source("binding-mode launch { binding t spawn terminal; }".into())
        .unwrap();
    assert_eq!(state.binding_modes.active(), None);
    assert!(!state.binding_modes.set("resize"));
    key(&mut state, 43, false);
    assert!(state.intercepted_keys.is_empty());
}

#[test]
fn escape_exits_with_every_modifier_combination() {
    if !crate::startup_tests::private_runtime("input::binding_mode_tests::escape_exits_with_every_modifier_combination")
    {
        return;
    }
    let mut events = EventLoop::try_new().unwrap();
    let mut state = state(&mut events);
    let modifiers = [133, 37, 64, 50]; // Super, Ctrl, Alt, Shift.
    for bits in 0..16 {
        state.set_binding_mode("resize");
        for (bit, code) in modifiers.iter().enumerate() {
            if bits & (1 << bit) != 0 {
                key(&mut state, *code, true);
            }
        }
        tap(&mut state, 9);
        assert_eq!(state.binding_modes.active(), None, "modifiers={bits:04b}");
        for (bit, code) in modifiers.iter().enumerate() {
            if bits & (1 << bit) != 0 {
                key(&mut state, *code, false);
            }
        }
        assert!(state.intercepted_keys.is_empty());
        assert!(state.binding_mode_pressed.is_empty());
    }
}

#[test]
fn input_capture_exits_modes_and_prevents_reentry() {
    if !crate::startup_tests::private_runtime(
        "input::binding_mode_tests::input_capture_exits_modes_and_prevents_reentry",
    ) {
        return;
    }
    use crate::input_capture::{Barrier, Zone};
    let mut events = EventLoop::try_new().unwrap();
    let mut state = state(&mut events);
    state.set_binding_mode("resize");
    state.input_capture.update_zones(vec![Zone {
        width: 800,
        height: 600,
        x: 0,
        y: 0,
    }]);
    let id = state.input_capture.register(1, 3).unwrap();
    let zone_set = state.input_capture.zones(id).unwrap()["zone_set"].as_u64().unwrap() as u32;
    state
        .input_capture
        .set_barriers(
            id,
            zone_set,
            vec![Barrier {
                id: 1,
                position: [800, 0, 800, 599],
            }],
        )
        .unwrap();
    state.input_capture.enable(id).unwrap();
    assert!(state.input_capture.motion((799., 50.), (810., 50.), true));
    crate::after_dispatch_with_redraw(&mut state, |_, _| {});
    assert_eq!(state.binding_modes.active(), None);
    state.set_binding_mode("resize");
    assert_eq!(state.binding_modes.active(), None);
    state.input_capture.disable(id).unwrap();
    crate::after_dispatch_with_redraw(&mut state, |_, _| {});
    assert_eq!(state.binding_modes.active(), None);
}
