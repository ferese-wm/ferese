use super::*;
use crate::input::binding_mode_tests::{button, key};
use smithay::input::keyboard::{
    FilterResult, GrabStartData, KeyboardGrab, KeyboardInnerHandle, Keycode, ModifiersState,
};
use smithay::reexports::calloop::EventLoop;
use smithay::reexports::wayland_server::Display;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::Serial;
use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;

const SOURCE: &str = r#"
binding "Super+r" binding-mode resize
binding-mode resize { binding h resize left; }
binding-mode launch { binding h binding-mode default; }
"#;

fn state(events: &mut EventLoop<'static, Ferese>) -> Ferese {
    let (_, mut config, _) = crate::config::prepare(SOURCE, std::path::Path::new(".")).unwrap();
    config.wallpaper.path = None;
    Ferese::try_new(events, Display::new().unwrap(), config).unwrap()
}

type KeyEvents = Rc<RefCell<Vec<(u32, KeyState)>>>;

struct ForwardedKeys {
    events: KeyEvents,
    start: GrabStartData<Ferese>,
}

impl KeyboardGrab<Ferese> for ForwardedKeys {
    fn input(
        &mut self,
        data: &mut Ferese,
        handle: &mut KeyboardInnerHandle<'_, Ferese>,
        code: Keycode,
        state: KeyState,
        modifiers: Option<ModifiersState>,
        serial: Serial,
        time: InputTime,
    ) {
        self.events.borrow_mut().push((code.raw(), state));
        handle.input(data, code, state, modifiers, serial, time);
    }

    fn set_focus(
        &mut self,
        data: &mut Ferese,
        handle: &mut KeyboardInnerHandle<'_, Ferese>,
        focus: Option<WlSurface>,
        serial: Serial,
    ) {
        handle.set_focus(data, focus, serial);
    }

    fn start_data(&self) -> &GrabStartData<Ferese> {
        &self.start
    }

    fn unset(&mut self, _: &mut Ferese) {}
}

pub(in crate::input) fn record(state: &mut Ferese) -> KeyEvents {
    let events = Rc::new(RefCell::new(Vec::new()));
    state.seat.get_keyboard().unwrap().set_grab(
        state,
        ForwardedKeys {
            events: events.clone(),
            start: GrabStartData { focus: None },
        },
        SERIAL_COUNTER.next_serial(),
    );
    events
}

#[test]
fn pause_releases_physical_modifiers_without_leaking_intercepted_keys() {
    if !crate::startup_tests::private_runtime(
        "input::session::tests::pause_releases_physical_modifiers_without_leaking_intercepted_keys",
    ) {
        return;
    }
    let mut events = EventLoop::try_new().unwrap();
    let mut state = state(&mut events);
    let forwarded = record(&mut state);
    let keyboard = state.seat.get_keyboard().unwrap();
    key(&mut state, 133, true); // Super is forwarded.
    key(&mut state, 27, true); // Super+r is compositor-owned.
    assert_eq!(state.binding_modes.active(), Some("resize"));
    assert!(keyboard.modifier_state().logo);
    state.reset_session_input();
    assert!(!keyboard.modifier_state().logo);
    assert!(keyboard.pressed_keys().is_empty());
    assert!(state.intercepted_keys.is_empty());
    assert!(state.binding_mode_pressed.is_empty());
    assert_eq!(state.binding_modes.active(), None);
    assert_eq!(
        *forwarded.borrow(),
        [(133, KeyState::Pressed), (133, KeyState::Released)]
    );
    assert!(!keyboard.is_grabbed());
    // The backend may deliver a stale release after resume; it cannot become
    // an unmatched client release or re-run an action.
    key(&mut state, 27, false);
    key(&mut state, 133, false);
    assert!(keyboard.pressed_keys().is_empty());
    state.reset_session_input(); // Teardown is idempotent.
}

#[test]
fn pause_discards_repeat_ownership_and_accepts_a_fresh_press() {
    if !crate::startup_tests::private_runtime(
        "input::session::tests::pause_discards_repeat_ownership_and_accepts_a_fresh_press",
    ) {
        return;
    }
    let mut events = EventLoop::try_new().unwrap();
    let mut state = state(&mut events);
    state.set_binding_mode("resize");
    key(&mut state, 43, true);
    assert!(state.binding_mode_repeats.contains_key(&Keycode::new(43)));
    state.reset_session_input();
    assert!(state.binding_mode_repeats.is_empty());
    key(&mut state, 43, false);
    state.set_binding_mode("launch");
    key(&mut state, 43, true);
    assert_eq!(state.binding_modes.active(), None);
    key(&mut state, 43, false);
    assert!(state.intercepted_keys.is_empty());
}

#[test]
fn pause_preserves_auxiliary_sources_including_a_shared_held_key() {
    if !crate::startup_tests::private_runtime(
        "input::session::tests::pause_preserves_auxiliary_sources_including_a_shared_held_key",
    ) {
        return;
    }
    let mut events = EventLoop::try_new().unwrap();
    let mut state = state(&mut events);
    let keyboard = state.seat.get_keyboard().unwrap();
    let auxiliary = KeyboardSource::new_auxiliary();
    keyboard.input_from_source::<(), _>(
        auxiliary,
        &mut state,
        Keycode::new(50),
        KeyState::Pressed,
        SERIAL_COUNTER.next_serial(),
        InputTime::now(),
        |_, _, _| FilterResult::Forward,
    );
    key(&mut state, 37, true);
    key(&mut state, 38, true);
    keyboard.input_from_source::<(), _>(
        auxiliary,
        &mut state,
        Keycode::new(38),
        KeyState::Pressed,
        SERIAL_COUNTER.next_serial(),
        InputTime::now(),
        |_, _, _| FilterResult::Forward,
    );
    state.reset_session_input();
    assert!(!keyboard.modifier_state().ctrl);
    assert!(keyboard.modifier_state().shift);
    assert_eq!(
        keyboard.pressed_keys(),
        HashSet::from([Keycode::new(38), Keycode::new(50)])
    );
    // Releasing the departing physical source again must leave both auxiliary
    // holds intact; the auxiliary source can finish its own pair normally.
    key(&mut state, 38, false);
    assert!(keyboard.pressed_keys().contains(&Keycode::new(38)));
    keyboard.release_source(&mut state, auxiliary);
    assert!(keyboard.pressed_keys().is_empty());
    assert!(!keyboard.modifier_state().shift);
}

#[test]
fn pause_releases_portal_shortcuts_and_disables_input_capture() {
    use crate::input_capture::{Barrier, Zone};
    use serde_json::json;

    if !crate::startup_tests::private_runtime(
        "input::session::tests::pause_releases_portal_shortcuts_and_disables_input_capture",
    ) {
        return;
    }
    let mut events = EventLoop::try_new().unwrap();
    let mut state = state(&mut events);
    state
        .portal_shortcuts
        .register(
            99,
            serde_json::from_value(json!([{"id":"review", "trigger":"F10"}])).unwrap(),
            &[],
            &state.input_settings,
        )
        .unwrap();
    key(&mut state, 76, true);
    assert_eq!(
        state.portal_shortcuts.poll(99, false)["events"][0]["active"],
        true
    );

    state.input_capture.update_zones(vec![Zone {
        width: 800,
        height: 600,
        x: 0,
        y: 0,
    }]);
    let id = state.input_capture.register(99, 3).unwrap();
    let zone_set = state.input_capture.zones(id).unwrap()["zone_set"]
        .as_u64()
        .unwrap() as u32;
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
    key(&mut state, 38, true);
    state.reset_session_input();
    assert!(!state.input_capture.active());
    assert!(!state.input_capture.motion((799., 50.), (810., 50.), true));
    assert!(state.seat.get_keyboard().unwrap().pressed_keys().is_empty());
    assert_eq!(
        state.portal_shortcuts.poll(99, false)["events"][0]["active"],
        false
    );
    let (send, receive) = std::sync::mpsc::sync_channel(1);
    state.input_capture.watch(99, 1, id, send);
    let reply = receive.try_recv().unwrap().result.unwrap();
    let events = reply["events"].as_array().unwrap();
    for expected in ["deactivated", "disabled"] {
        assert!(events.iter().any(|event| event["type"] == expected));
    }
}

#[test]
fn pause_retires_delivered_pointer_buttons_touch_contacts_and_swipes() {
    use smithay::input::touch::DownEvent;

    if !crate::startup_tests::private_runtime(
        "input::session::tests::pause_retires_delivered_pointer_buttons_touch_contacts_and_swipes",
    ) {
        return;
    }
    let (mut state, mut events, _) = crate::input::layer_focus_tests::fixture();
    let (window, _wire) = crate::state::test_window(&mut state, &mut events, 0xff112233);
    let surface = window.toplevel().unwrap().wl_surface().clone();
    let pointer = state.seat.get_pointer().unwrap();
    pointer.motion(
        &mut state,
        Some((surface.clone(), (0.0, 0.0).into())),
        &MotionEvent {
            location: (20.0, 20.0).into(),
            serial: SERIAL_COUNTER.next_serial(),
            time: InputTime::now(),
        },
    );
    for code in [0x110, 0x111] {
        button(&mut state, code, true);
    }
    assert_eq!(state.pointer_buttons, HashSet::from([0x110, 0x111]));
    assert!(pointer.is_grabbed());
    let touch = state.seat.get_touch().unwrap();
    touch.down(
        &mut state,
        Some((surface, (0.0, 0.0).into())),
        &DownEvent {
            slot: Some(0).into(),
            location: (20.0, 20.0).into(),
            serial: SERIAL_COUNTER.next_serial(),
            time: InputTime::now(),
        },
    );
    state.consumed_touch_slots.insert(Some(7).into());
    state.swipe.begin(3, false, 100);
    state.reset_session_input();
    assert!(!pointer.is_grabbed());
    assert!(pointer.current_focus().is_none());
    assert!(state.pointer_buttons.is_empty());
    assert!(!touch.is_grabbed());
    assert!(state.consumed_touch_slots.is_empty());
    assert!(!state.swipe.active());
    for code in [0x110, 0x111] {
        assert!(state.consumed_pointer_buttons.contains(&code));
        button(&mut state, code, false);
    }
    assert!(state.consumed_pointer_buttons.is_empty());
    button(&mut state, 0x110, true);
    assert!(state.pointer_buttons.contains(&0x110));
    button(&mut state, 0x110, false);
    assert!(state.pointer_buttons.is_empty());
}
