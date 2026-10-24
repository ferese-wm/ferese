//! Exercise dismissal through the real input path, including contact lifetimes.
use super::*;
use ferese_protocols::shell::v1::server::ferese_shell_v1::FereseShellV1;
use smithay::backend::input::{
    TouchCancelEvent, TouchDownEvent, TouchFrameEvent, TouchMotionEvent, TouchSlot, TouchUpEvent,
    UnusedEvent,
};
use smithay::backend::winit::WinitVirtualDevice;
use std::io::Read;
use std::os::unix::net::UnixStream;
use std::sync::Arc;

#[derive(Debug)]
struct TouchInput;

impl InputBackend for TouchInput {
    type Device = WinitVirtualDevice;
    type KeyboardKeyEvent = UnusedEvent;
    type PointerButtonEvent = UnusedEvent;
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
    type TouchDownEvent = Contact;
    type TouchMotionEvent = Contact;
    type TouchUpEvent = Contact;
    type TouchCancelEvent = Contact;
    type TouchFrameEvent = Contact;
    type TabletToolAxisEvent = UnusedEvent;
    type TabletToolProximityEvent = UnusedEvent;
    type TabletToolTipEvent = UnusedEvent;
    type TabletToolButtonEvent = UnusedEvent;
    type SwitchToggleEvent = UnusedEvent;
    type SpecialEvent = UnusedEvent;
}

#[derive(Clone, Debug)]
struct Contact(u32, Point<f64, Logical>);

impl Event<TouchInput> for Contact {
    fn time(&self) -> smithay::backend::input::InputTime {
        smithay::backend::input::InputTime::from_millis(1)
    }
    fn device(&self) -> WinitVirtualDevice {
        WinitVirtualDevice
    }
}
impl TouchEvent<TouchInput> for Contact {
    fn slot(&self) -> TouchSlot {
        Some(self.0).into()
    }
}
impl AbsolutePositionEvent<TouchInput> for Contact {
    fn x(&self) -> f64 {
        self.1.x
    }
    fn y(&self) -> f64 {
        self.1.y
    }
    fn x_transformed(&self, _: i32) -> f64 {
        self.1.x
    }
    fn y_transformed(&self, _: i32) -> f64 {
        self.1.y
    }
}
impl TouchDownEvent<TouchInput> for Contact {}
impl TouchMotionEvent<TouchInput> for Contact {}
impl TouchUpEvent<TouchInput> for Contact {}
impl TouchCancelEvent<TouchInput> for Contact {}
impl TouchFrameEvent<TouchInput> for Contact {}

#[test]
fn outside_launcher_touches_are_consumed_until_up_or_cancel() {
    if !crate::startup_tests::private_runtime(
        "input::launcher_touch_tests::outside_launcher_touches_are_consumed_until_up_or_cancel",
    ) {
        return;
    }
    let (mut state, mut events, output) = super::layer_focus_tests::fixture();
    state.window_rules = crate::config::Config::parse_source(
        "window-rule app-id=dev.ferese.TouchTarget floating=#true\n",
    )
    .unwrap()
    .window_rules()
    .unwrap();
    let (window, _wire) = crate::state::test_window_with_app_id(
        &mut state,
        &mut events,
        0xff112233,
        "dev.ferese.TouchTarget",
    );
    let id = state.windows.ids()[&window];
    let bounds = state.windows.geometry(&id).unwrap().logical;
    let app = window.toplevel().unwrap().wl_surface().clone();
    let (launcher, _launcher_wire) = super::layer_focus_tests::layer_with_namespace(
        &mut state,
        &mut events,
        &output,
        1,
        3,
        "ferese-launcher",
    );
    let keyboard = state.seat.get_keyboard().unwrap();
    let touch = state.seat.get_touch().unwrap();
    assert_eq!(
        keyboard.current_focus(),
        Some(launcher.wl_surface().clone())
    );
    let outside = Contact(
        0,
        (bounds.x + bounds.width / 2., bounds.y + bounds.height / 2.).into(),
    );
    assert_eq!(state.surface_under(outside.1).unwrap().0, app);
    let (server, mut shell_wire) = UnixStream::pair().unwrap();
    let client = state
        .display_handle
        .insert_client(server, Arc::new(crate::state::ClientState::default()))
        .unwrap();
    let shell = client
        .create_resource::<FereseShellV1, (), Ferese>(&state.display_handle, 10, ())
        .unwrap();
    state.shell_resources.push(shell.downgrade());
    state.process_input_event::<TouchInput>(InputEvent::TouchDown {
        event: outside.clone(),
    });
    assert!(
        !touch.is_grabbed(),
        "dismissing must not begin an app touch sequence"
    );
    assert_eq!(
        keyboard.current_focus(),
        Some(launcher.wl_surface().clone())
    );
    // The launcher can close before the rest of this gesture arrives.
    keyboard.set_focus(&mut state, Some(app.clone()), SERIAL_COUNTER.next_serial());
    let second = Contact(1, outside.1);
    state.process_input_event::<TouchInput>(InputEvent::TouchDown {
        event: second.clone(),
    });
    for event in [outside.clone(), second.clone()] {
        state.process_input_event::<TouchInput>(InputEvent::TouchMotion {
            event: event.clone(),
        });
        state.process_input_event::<TouchInput>(InputEvent::TouchUp { event });
    }
    assert!(!touch.is_grabbed());
    assert!(state.consumed_touch_slots.is_empty());
    // A fresh tap must work after the swallowed contacts have ended.
    state.process_input_event::<TouchInput>(InputEvent::TouchDown {
        event: outside.clone(),
    });
    assert_eq!(touch.grab_start_data().unwrap().focus.unwrap().0, app);
    state.process_input_event::<TouchInput>(InputEvent::TouchUp {
        event: outside.clone(),
    });
    keyboard.set_focus(
        &mut state,
        Some(launcher.wl_surface().clone()),
        SERIAL_COUNTER.next_serial(),
    );
    state.process_input_event::<TouchInput>(InputEvent::TouchDown {
        event: outside.clone(),
    });
    state.process_input_event::<TouchInput>(InputEvent::TouchCancel {
        event: outside.clone(),
    });
    assert!(state.consumed_touch_slots.is_empty());
    assert!(!touch.is_grabbed());
    // An inside tap still reaches the launcher itself after cancellation.
    let inside = Contact(0, (20., 20.).into());
    state.process_input_event::<TouchInput>(InputEvent::TouchDown {
        event: inside.clone(),
    });
    assert_eq!(
        touch.grab_start_data().unwrap().focus.unwrap().0,
        *launcher.wl_surface()
    );
    state.process_input_event::<TouchInput>(InputEvent::TouchUp { event: inside });
    state.display_handle.flush_clients().unwrap();
    shell_wire.set_nonblocking(true).unwrap();
    let mut bytes = Vec::new();
    let _ = shell_wire.read_to_end(&mut bytes);
    let opcode = FereseShellV1::interface()
        .events
        .iter()
        .position(|event| event.name == "toggle_launcher")
        .unwrap() as u16;
    let (mut offset, mut dismissals) = (0, 0);
    while offset < bytes.len() {
        let object = u32::from_ne_bytes(bytes[offset..offset + 4].try_into().unwrap());
        let header = u32::from_ne_bytes(bytes[offset + 4..offset + 8].try_into().unwrap());
        if object == shell.id().protocol_id() && header as u16 == opcode {
            dismissals += 1;
        }
        offset += (header >> 16) as usize;
    }
    assert_eq!(dismissals, 2, "each outside gesture dismisses exactly once");
}
