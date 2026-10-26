//! Verify gesture delivery over a private Wayland connection through the real input path.
use super::*;
use crate::state::{dispatch_test_client as dispatch, test_client_request as request};
use smithay::backend::input::{
    GestureHoldBeginEvent as HoldBegin, GestureHoldEndEvent as HoldEnd,
    GesturePinchBeginEvent as PinchBegin, GesturePinchEndEvent as PinchEnd,
    GesturePinchUpdateEvent as PinchUpdate, GestureSwipeBeginEvent as SwipeBegin,
    GestureSwipeEndEvent as SwipeEnd, GestureSwipeUpdateEvent as SwipeUpdate, InputTime,
    UnusedEvent,
};
use smithay::backend::winit::WinitVirtualDevice;
use std::io::Read;
use std::os::unix::net::UnixStream;

#[derive(Debug)]
struct GestureInput;
impl InputBackend for GestureInput {
    type Device = WinitVirtualDevice;
    type KeyboardKeyEvent = UnusedEvent;
    type PointerButtonEvent = UnusedEvent;
    type PointerAxisEvent = UnusedEvent;
    type PointerMotionEvent = UnusedEvent;
    type PointerMotionAbsoluteEvent = UnusedEvent;
    type GestureSwipeBeginEvent = Sample;
    type GestureSwipeUpdateEvent = Sample;
    type GestureSwipeEndEvent = Sample;
    type GesturePinchBeginEvent = Sample;
    type GesturePinchUpdateEvent = Sample;
    type GesturePinchEndEvent = Sample;
    type GestureHoldBeginEvent = Sample;
    type GestureHoldEndEvent = Sample;
    type TouchDownEvent = UnusedEvent;
    type TouchMotionEvent = UnusedEvent;
    type TouchUpEvent = UnusedEvent;
    type TouchCancelEvent = UnusedEvent;
    type TouchFrameEvent = UnusedEvent;
    type TabletToolAxisEvent = UnusedEvent;
    type TabletToolProximityEvent = UnusedEvent;
    type TabletToolTipEvent = UnusedEvent;
    type TabletToolButtonEvent = UnusedEvent;
    type SwitchToggleEvent = UnusedEvent;
    type SpecialEvent = UnusedEvent;
}

#[derive(Clone, Debug)]
struct Sample;
impl Event<GestureInput> for Sample {
    fn time(&self) -> InputTime {
        InputTime::from_millis(10)
    }
    fn device(&self) -> WinitVirtualDevice {
        WinitVirtualDevice
    }
}
impl GestureBeginEvent<GestureInput> for Sample {
    fn fingers(&self) -> u32 {
        2
    }
}
impl GestureEndEvent<GestureInput> for Sample {
    fn cancelled(&self) -> bool {
        false
    }
}
impl SwipeBegin<GestureInput> for Sample {}
impl SwipeEnd<GestureInput> for Sample {}
impl PinchBegin<GestureInput> for Sample {}
impl PinchEnd<GestureInput> for Sample {}
impl HoldBegin<GestureInput> for Sample {}
impl HoldEnd<GestureInput> for Sample {}
impl SwipeUpdate<GestureInput> for Sample {
    fn delta_x(&self) -> f64 {
        2.0
    }
    fn delta_y(&self) -> f64 {
        -3.0
    }
}
impl PinchUpdate<GestureInput> for Sample {
    fn delta_x(&self) -> f64 {
        2.0
    }
    fn delta_y(&self) -> f64 {
        -3.0
    }
    fn scale(&self) -> f64 {
        1.5
    }
    fn rotation(&self) -> f64 {
        15.0
    }
}

#[derive(Debug)]
struct Message {
    object: u32,
    opcode: u16,
    args: Vec<u32>,
}

fn messages(wire: &mut UnixStream) -> Vec<Message> {
    wire.set_nonblocking(true).unwrap();
    let mut bytes = Vec::new();
    let _ = wire.read_to_end(&mut bytes);
    wire.set_nonblocking(false).unwrap();
    let mut result = Vec::new();
    let mut offset = 0;
    while offset < bytes.len() {
        let object = u32::from_ne_bytes(bytes[offset..offset + 4].try_into().unwrap());
        let header = u32::from_ne_bytes(bytes[offset + 4..offset + 8].try_into().unwrap());
        let size = (header >> 16) as usize;
        assert!(size >= 8 && offset + size <= bytes.len());
        let args = bytes[offset + 8..offset + size]
            .chunks_exact(4)
            .map(|word| u32::from_ne_bytes(word.try_into().unwrap()))
            .collect();
        result.push(Message {
            object,
            opcode: header as u16,
            args,
        });
        offset += size;
    }
    result
}

fn bind(wire: &mut UnixStream, globals: &[Message], interface: &str, version: u32, id: u32) {
    let global = globals
        .iter()
        .find(|message| {
            if message.object != 7 || message.opcode != 0 {
                return false;
            }
            let bytes: Vec<_> = message.args[2..]
                .iter()
                .flat_map(|word| word.to_ne_bytes())
                .collect();
            bytes.get(..interface.len() + 1) == Some(format!("{interface}\0").as_bytes())
        })
        .unwrap_or_else(|| panic!("missing advertised {interface}"));
    let mut name = interface.as_bytes().to_vec();
    name.push(0);
    let mut args = vec![global.args[0], name.len() as u32];
    name.resize(name.len().next_multiple_of(4), 0);
    args.extend(
        name.chunks_exact(4)
            .map(|word| u32::from_ne_bytes(word.try_into().unwrap())),
    );
    args.extend([version, id]);
    request(wire, 7, 0, &args, None);
}

fn fixture() -> (Ferese, calloop::EventLoop<'static, Ferese>, UnixStream) {
    let (mut state, mut events, _) = super::layer_focus_tests::fixture();
    let (window, mut wire) = crate::state::test_window(&mut state, &mut events, 0xff112233);
    request(&mut wire, 1, 1, &[7], None);
    dispatch(&mut events, &mut state);
    let globals = messages(&mut wire);
    bind(&mut wire, &globals, "wl_seat", 7, 8);
    bind(&mut wire, &globals, "zwp_pointer_gestures_v1", 3, 9);
    request(&mut wire, 8, 0, &[10], None);
    request(&mut wire, 9, 0, &[11, 10], None);
    request(&mut wire, 9, 1, &[12, 10], None);
    request(&mut wire, 9, 3, &[13, 10], None);
    dispatch(&mut events, &mut state);
    let pointer = state.seat.get_pointer().unwrap();
    pointer.motion(
        &mut state,
        Some((
            window.toplevel().unwrap().wl_surface().clone(),
            (0.0, 0.0).into(),
        )),
        &MotionEvent {
            location: (20.0, 20.0).into(),
            serial: SERIAL_COUNTER.next_serial(),
            time: InputTime::from_millis(1),
        },
    );
    state.display_handle.flush_clients().unwrap();
    messages(&mut wire);
    (state, events, wire)
}

#[test]
fn clients_receive_unbound_swipes_pinches_and_holds() {
    if !crate::startup_tests::private_runtime(
        "input::gesture_tests::clients_receive_unbound_swipes_pinches_and_holds",
    ) {
        return;
    }
    let (mut state, _events, mut wire) = fixture();
    for event in [
        InputEvent::GestureSwipeBegin { event: Sample },
        InputEvent::GestureSwipeUpdate { event: Sample },
        InputEvent::GestureSwipeEnd { event: Sample },
        InputEvent::GesturePinchBegin { event: Sample },
        InputEvent::GesturePinchUpdate { event: Sample },
        InputEvent::GesturePinchEnd { event: Sample },
        InputEvent::GestureHoldBegin { event: Sample },
        InputEvent::GestureHoldEnd { event: Sample },
    ] {
        state.process_input_event::<GestureInput>(event);
    }
    state.display_handle.flush_clients().unwrap();
    let gestures: Vec<_> = messages(&mut wire)
        .into_iter()
        .filter(|message| (11..=13).contains(&message.object))
        .collect();
    assert_eq!(
        gestures
            .iter()
            .map(|message| (message.object, message.opcode))
            .collect::<Vec<_>>(),
        [
            (11, 0),
            (11, 1),
            (11, 2),
            (12, 0),
            (12, 1),
            (12, 2),
            (13, 0),
            (13, 1)
        ]
    );
    assert_eq!(gestures[0].args[3], 2);
    assert_eq!(gestures[1].args, [10, 512, (-768_i32) as u32]);
    assert_eq!(gestures[4].args, [10, 512, (-768_i32) as u32, 384, 3840]);
    assert_eq!(*gestures[5].args.last().unwrap(), 0);
    assert_eq!(gestures[6].args[3], 2);
}

#[test]
fn pointer_leave_cancels_a_gesture_and_suppresses_its_late_events() {
    if !crate::startup_tests::private_runtime(
        "input::gesture_tests::pointer_leave_cancels_a_gesture_and_suppresses_its_late_events",
    ) {
        return;
    }
    let (mut state, _events, mut wire) = fixture();
    state.process_input_event::<GestureInput>(InputEvent::GesturePinchBegin { event: Sample });
    let pointer = state.seat.get_pointer().unwrap();
    pointer.motion(
        &mut state,
        None,
        &MotionEvent {
            location: (200.0, 200.0).into(),
            serial: SERIAL_COUNTER.next_serial(),
            time: InputTime::from_millis(11),
        },
    );
    state.process_input_event::<GestureInput>(InputEvent::GesturePinchUpdate { event: Sample });
    state.process_input_event::<GestureInput>(InputEvent::GesturePinchEnd { event: Sample });
    state.display_handle.flush_clients().unwrap();
    let gestures: Vec<_> = messages(&mut wire)
        .into_iter()
        .filter(|message| message.object == 12)
        .collect();
    assert_eq!(gestures.len(), 2, "{gestures:?}");
    assert_eq!((gestures[0].opcode, gestures[1].opcode), (0, 2));
    assert_eq!(*gestures[1].args.last().unwrap(), 1);
}
