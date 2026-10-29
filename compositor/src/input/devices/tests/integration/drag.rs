use super::*;
use crate::state::{dispatch_test_client as dispatch, test_client_request as request};
use smithay::input::dnd::DnDGrab;
use smithay::reexports::wayland_server::Resource;
use smithay::reexports::wayland_server::protocol::wl_data_device_manager::WlDataDeviceManager;
use smithay::reexports::wayland_server::protocol::wl_data_source::WlDataSource;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;

#[test]
fn only_real_releases_complete_negotiated_drops() {
    if !private("drag::only_real_releases_complete_negotiated_drops") {
        return;
    }
    for case in [
        "pointer-up",
        "pointer-remove",
        "pointer-reset",
        "touch-up",
        "touch-cancel",
        "touch-remove",
        "touch-reset",
    ] {
        let (mut state, mut events, mut wire) = fixture();
        let pointer = state.seat.get_pointer().unwrap();
        let touch = state.seat.get_touch().unwrap();
        let root = pointer.current_focus().expect("fixture's window");
        let client = root.client().unwrap();
        let seat = state.seat.client_seats(&client).into_iter().next().unwrap();
        let manager = client
            .create_resource::<WlDataDeviceManager, smithay::wayland::GlobalData, Ferese>(
                &state.display_handle,
                3,
                smithay::wayland::GlobalData,
            )
            .unwrap();
        request(&mut wire, manager.id().protocol_id(), 0, &[14], None);
        // text/plain, including its terminating NUL and Wayland padding.
        let mut bytes = b"text/plain\0".to_vec();
        let mut mime = vec![bytes.len() as u32];
        bytes.resize(bytes.len().next_multiple_of(4), 0);
        mime.extend(
            bytes
                .chunks_exact(4)
                .map(|w| u32::from_ne_bytes(w.try_into().unwrap())),
        );
        request(&mut wire, 14, 0, &mime, None);
        request(&mut wire, 14, 2, &[1], None);
        request(
            &mut wire,
            manager.id().protocol_id(),
            1,
            &[15, seat.id().protocol_id()],
            None,
        );
        let is_touch = case.starts_with("touch");
        if is_touch {
            request(&mut wire, 8, 2, &[16], None);
        }
        dispatch(&mut events, &mut state);
        messages(&mut wire);
        add(&mut state, 1);
        if is_touch {
            state.process_input_event::<Input>(InputEvent::TouchDown {
                event: sample(1, 0, true),
            });
        } else {
            button(&mut state, 1, true);
        }
        let serial = if is_touch {
            touch.with_grab(|serial, _| serial).unwrap()
        } else {
            pointer.with_grab(|serial, _| serial).unwrap()
        };
        request(
            &mut wire,
            15,
            0,
            &[14, root.id().protocol_id(), 0, serial.into()],
            None,
        );
        dispatch(&mut events, &mut state);
        if is_touch {
            assert!(
                touch
                    .with_grab(|_, grab| grab.is::<DnDGrab<Ferese, WlDataSource, WlSurface>>())
                    .unwrap(),
                "{case}: touch drag not admitted"
            );
            let slot = touch.grab_start_data().unwrap().slot;
            touch.motion(
                &mut state,
                Some((root.clone(), (0., 0.).into())),
                &smithay::input::touch::MotionEvent {
                    slot,
                    location: (20., 20.).into(),
                    time: InputTime::from_millis(2),
                },
            );
        } else {
            assert!(
                pointer
                    .with_grab(|_, grab| grab.is::<DnDGrab<Ferese, WlDataSource, WlSurface>>())
                    .unwrap(),
                "{case}: pointer drag not admitted"
            );
            pointer.motion(
                &mut state,
                Some((root.clone(), (0., 0.).into())),
                &smithay::input::pointer::MotionEvent {
                    location: (20., 20.).into(),
                    serial: SERIAL_COUNTER.next_serial(),
                    time: InputTime::from_millis(2),
                },
            );
        }
        dispatch(&mut events, &mut state);
        let offer = messages(&mut wire)
            .into_iter()
            .find(|message| message.object == 15 && message.opcode == 0)
            .expect("drag data offer")
            .args[0];
        let mut accept = vec![0];
        accept.extend(mime);
        request(&mut wire, offer, 0, &accept, None);
        request(&mut wire, offer, 4, &[1, 1], None);
        dispatch(&mut events, &mut state);
        messages(&mut wire);
        match case {
            "pointer-up" => button(&mut state, 1, false),
            "pointer-remove" | "touch-remove" => remove(&mut state, 1),
            "pointer-reset" | "touch-reset" => state.reset_session_input(),
            "touch-up" => state.process_input_event::<Input>(InputEvent::TouchUp {
                event: sample(1, 0, false),
            }),
            "touch-cancel" => state.process_input_event::<Input>(InputEvent::TouchCancel {
                event: sample(1, 0, false),
            }),
            _ => unreachable!(),
        }
        dispatch(&mut events, &mut state);
        let delivered = messages(&mut wire);
        let completed = delivered
            .iter()
            .any(|message| message.object == 14 && message.opcode == 3);
        let cancelled = delivered
            .iter()
            .any(|message| message.object == 14 && message.opcode == 2);
        if !matches!(case, "pointer-up" | "touch-up") {
            assert!(!completed && cancelled, "{case}: should cancel");
        } else {
            assert!(
                completed && !cancelled,
                "{case}: a real release should complete the negotiated drop, events={delivered:?}"
            );
        }
    }
}
