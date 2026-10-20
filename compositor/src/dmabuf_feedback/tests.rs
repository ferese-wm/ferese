//! Real v4 feedback subscriptions over a private Wayland socket; no DRM/EGL.
use std::io::Read;
use std::os::unix::net::UnixStream;

use smithay::backend::allocator::{Format, Fourcc, Modifier};
use smithay::output::{Mode, Output, PhysicalProperties, Subpixel};
use smithay::reexports::calloop::EventLoop;
use smithay::reexports::wayland_protocols::wp::linux_dmabuf::zv1::server::zwp_linux_dmabuf_feedback_v1::TrancheFlags;
use smithay::reexports::wayland_server::Display;
use smithay::wayland::dmabuf::DmabufFeedbackBuilder;

use super::*;
use crate::Ferese;
use crate::state::{dispatch_test_client as dispatch, test_client_request as request, test_window};

fn messages(wire: &mut UnixStream) -> Vec<(u32, u32, Vec<u32>)> {
    wire.set_nonblocking(true).unwrap();
    let mut bytes = Vec::new();
    let _ = wire.read_to_end(&mut bytes);
    wire.set_nonblocking(false).unwrap();
    let mut offset = 0;
    let mut result = Vec::new();
    while offset < bytes.len() {
        let object = u32::from_ne_bytes(bytes[offset..offset + 4].try_into().unwrap());
        let header = u32::from_ne_bytes(bytes[offset + 4..offset + 8].try_into().unwrap());
        let size = (header >> 16) as usize;
        result.push((
            object,
            header & 0xffff,
            bytes[offset + 8..offset + size]
                .chunks_exact(4)
                .map(|word| u32::from_ne_bytes(word.try_into().unwrap()))
                .collect(),
        ));
        offset += size;
    }
    result
}

fn flags(events: &[(u32, u32, Vec<u32>)], object: u32) -> Vec<u32> {
    events
        .iter()
        .filter(|(id, opcode, _)| *id == object && *opcode == 6)
        .map(|(_, _, args)| args[0])
        .collect()
}

fn done(events: &[(u32, u32, Vec<u32>)], object: u32) -> usize {
    events
        .iter()
        .filter(|(id, opcode, _)| *id == object && *opcode == 0)
        .count()
}

#[test]
fn subscriptions_update_deduplicate_and_retire_device_preferences() {
    if !crate::startup_tests::private_runtime(
        "dmabuf_feedback::tests::subscriptions_update_deduplicate_and_retire_device_preferences",
    ) {
        return;
    }
    let mut events = EventLoop::try_new().unwrap();
    let mut config = crate::config::Config::default().runtime_config().unwrap();
    config.wallpaper.path = None;
    let mut state = Ferese::new(&mut events, Display::new().unwrap(), config).unwrap();
    let output = Output::new(
        "feedback-test".into(),
        PhysicalProperties {
            serial_number: String::new(),
            size: (0, 0).into(),
            subpixel: Subpixel::Unknown,
            make: "test".into(),
            model: "test".into(),
        },
    );
    output.change_current_state(
        Some(Mode {
            size: (100, 100).into(),
            refresh: 60_000,
        }),
        None,
        None,
        None,
    );
    state.space.map_output(&output, (0, 0));
    state.register_output(&output, "feedback-test".into());
    let f = Format {
        code: Fourcc::Argb8888,
        modifier: Modifier::Linear,
    };
    let fallback = DmabufFeedbackBuilder::new(10, [f]).build().unwrap();
    let preferred = DmabufFeedbackBuilder::new(10, [f])
        .add_preference_tranche(20, TrancheFlags::Scanout, [f], 4..=6)
        .build()
        .unwrap();
    let global = state
        .dmabuf_state
        .create_global_with_default_feedback::<Ferese>(&state.display_handle, &fallback);
    state.dmabuf_feedback.register(global, fallback.clone());
    let (window, mut wire) = test_window(&mut state, &mut events, 0xffabcdef);
    let surface = window.toplevel().unwrap().wl_surface().clone();
    request(&mut wire, 1, 1, &[7], None); // wl_display.get_registry
    dispatch(&mut events, &mut state);
    let registry = messages(&mut wire);
    let dmabuf_name = registry
        .iter()
        .find_map(|(object, opcode, args)| {
            if *object != 7 || *opcode != 0 {
                return None;
            }
            let bytes = args[2..].iter().flat_map(|word| word.to_ne_bytes()).collect::<Vec<_>>();
            bytes.starts_with(b"zwp_linux_dmabuf_v1\0").then_some(args[0])
        })
        .expect("DMA-BUF v4 global");
    let name = b"zwp_linux_dmabuf_v1\0";
    let mut padded = name.to_vec();
    padded.resize(name.len().next_multiple_of(4), 0);
    let bind = [dmabuf_name, name.len() as u32]
        .into_iter()
        .chain(
            padded
                .chunks_exact(4)
                .map(|word| u32::from_ne_bytes(word.try_into().unwrap())),
        )
        .chain([4, 8])
        .collect::<Vec<_>>();
    request(&mut wire, 7, 0, &bind, None);
    request(&mut wire, 8, 3, &[9, 2], None); // get_surface_feedback
    dispatch(&mut events, &mut state);
    let initial = messages(&mut wire);
    assert_eq!(flags(&initial, 9), [0]);
    assert_eq!(done(&initial, 9), 1);
    assert_eq!(state.dmabuf_feedback.requested().count(), 1);
    let main = initial.iter().find(|(id, opcode, _)| *id == 9 && *opcode == 2).unwrap();
    assert_eq!(main.2[0] as usize, std::mem::size_of::<libc::dev_t>());
    assert_eq!(main.2[1], 10, "main device remains the renderer");

    let selected = HashMap::from([(surface.clone(), preferred.clone())]);
    state.dmabuf_feedback.refresh(&selected);
    state.display_handle.flush_clients().unwrap();
    let changed = messages(&mut wire);
    assert_eq!(flags(&changed, 9), [TrancheFlags::Scanout.bits(), 0]);
    assert_eq!(done(&changed, 9), 1);
    let target = changed.iter().find(|(id, opcode, _)| *id == 9 && *opcode == 4).unwrap();
    assert_eq!(target.2[1], 20, "scanout tranche targets the primary DRM device");
    state.dmabuf_feedback.refresh(&selected);
    state.display_handle.flush_clients().unwrap();
    assert!(messages(&mut wire).is_empty(), "Smithay suppresses unchanged feedback");

    request(&mut wire, 8, 3, &[10, 2], None);
    dispatch(&mut events, &mut state);
    let second = messages(&mut wire);
    assert_eq!(flags(&second, 10), [TrancheFlags::Scanout.bits(), 0]);
    state.dmabuf_feedback.refresh(&HashMap::new()); // migration/ineligibility fallback
    state.display_handle.flush_clients().unwrap();
    let fallback_events = messages(&mut wire);
    assert_eq!(flags(&fallback_events, 9), [0]);
    assert_eq!(flags(&fallback_events, 10), [0]);
    assert_eq!(done(&fallback_events, 9), 1);
    assert_eq!(done(&fallback_events, 10), 1);

    request(&mut wire, 9, 0, &[], None);
    request(&mut wire, 10, 0, &[], None);
    dispatch(&mut events, &mut state);
    messages(&mut wire);
    request(&mut wire, 8, 3, &[11, 2], None);
    request(&mut wire, 8, 2, &[12], None); // get_default_feedback
    dispatch(&mut events, &mut state);
    let renewed = messages(&mut wire);
    assert_eq!(flags(&renewed, 11), [0], "renewal starts with safe render feedback");
    assert_eq!(flags(&renewed, 12), [0]);
    state.dmabuf_feedback.refresh(&selected);
    state.display_handle.flush_clients().unwrap();
    let promoted = messages(&mut wire);
    assert_eq!(flags(&promoted, 11), [TrancheFlags::Scanout.bits(), 0]);
    state.dmabuf_feedback.remove(global);
    state
        .dmabuf_state
        .disable_global::<Ferese>(&state.display_handle, &global);
    state.display_handle.flush_clients().unwrap();
    let retired = messages(&mut wire);
    assert_eq!(done(&retired, 11), 1);
    assert_eq!(
        flags(&retired, 11),
        [0],
        "retirement keeps a valid main-device render tranche"
    );
    assert_eq!(
        done(&retired, 12),
        0,
        "default renderer capabilities remain unchanged on existing bindings"
    );
    assert!(
        retired
            .iter()
            .any(|(id, opcode, args)| *id == 7 && *opcode == 1 && args[0] == dmabuf_name)
    );
    state.dmabuf_feedback.refresh(&selected); // A stale choice cannot resurrect a retired preference.
    state.display_handle.flush_clients().unwrap();
    assert!(messages(&mut wire).is_empty());
    assert!(state.dmabuf_feedback.new_surface(&surface, global).is_none());
    drop(wire);
    dispatch(&mut events, &mut state);
    state.dmabuf_feedback.refresh(&HashMap::new());
    assert_eq!(state.dmabuf_feedback.requested().count(), 0);
}
