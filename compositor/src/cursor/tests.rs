use super::*;
use smithay::input::SeatHandler;
use smithay::input::pointer::MotionEvent;
use smithay::output::{Mode, Output, PhysicalProperties, Subpixel};
use smithay::reexports::calloop::EventLoop;
use smithay::utils::SERIAL_COUNTER;

fn file(frames: &[(u32, u32, u32, u32)]) -> Vec<u8> {
    let mut bytes = b"Xcur".to_vec();
    bytes.extend([16u32, 1, frames.len() as u32].into_iter().flat_map(u32::to_le_bytes));
    let mut pos = 16 + frames.len() * 12;
    for &(size, width, _, _) in frames {
        bytes.extend([0xfffd0002, size, pos as u32].into_iter().flat_map(u32::to_le_bytes));
        pos += 36 + width as usize * width as usize * 4;
    }
    for &(size, width, hotspot, delay) in frames {
        bytes.extend(
            [36, 0xfffd0002, size, 1, width, width, hotspot, hotspot, delay]
                .into_iter()
                .flat_map(u32::to_le_bytes),
        );
        bytes.extend([255, 255, 255, 255].repeat(width as usize * width as usize));
    }
    bytes
}

#[test]
fn hotspot_and_footprint_use_the_images_scale() {
    let bytes = file(&[(24, 24, 12, 50), (48, 48, 24, 50)]);
    for scale in [1.0f64, 1.25, 2.0] {
        let asset = parse_cursor(&bytes, 24, scale.ceil() as i32).unwrap();
        let frame = &asset.frames[0];
        let pointer: Point<f64, Logical> = (100.0, 120.0).into();
        let location = frame.physical_location(pointer, scale);
        let hotspot = f64::from(frame.hotspot.x) * scale / f64::from(frame.buffer_scale);
        assert_eq!(location.x + hotspot, pointer.x * scale);
        assert_eq!(location.y + hotspot, pointer.y * scale);
        assert_eq!(frame.logical_rect(pointer).size, (24.0, 24.0).into());
        if scale > 1.0 {
            assert_eq!(frame.size.w, 48);
        }
    }
}

#[test]
fn unavailable_hidpi_size_preserves_logical_size_and_hotspot() {
    let asset = parse_cursor(&file(&[(32, 32, 16, 50)]), 24, 2).unwrap();
    let frame = &asset.frames[0];
    assert_eq!(frame.buffer_scale, 2);
    assert_eq!(frame.size.w, 64);
    assert_eq!(frame.hotspot.x, 32);
    assert_eq!(frame.logical_rect((100.0, 100.0).into()).size, (32.0, 32.0).into());
}

#[test]
fn selects_a_nominal_sequence_and_preserves_frame_delays() {
    let asset = parse_cursor(&file(&[(48, 48, 24, 70), (24, 24, 12, 30), (24, 24, 12, 80)]), 24, 1).unwrap();
    assert_eq!(asset.frames.len(), 2);
    for (time, index, remaining) in [
        (0, 0, 30),
        (29, 0, 1),
        (30, 1, 80),
        (109, 1, 1),
        (110, 0, 30),
        (400, 1, 40),
    ] {
        assert_eq!(
            asset.sample(Duration::from_millis(time)),
            (index, Some(Duration::from_millis(remaining)))
        );
    }
    let still = parse_cursor(&file(&[(24, 24, 12, 0)]), 24, 1).unwrap();
    assert_eq!(still.sample(Duration::from_secs(300)), (0, None));
    let zero = parse_cursor(&file(&[(24, 24, 12, 0), (24, 24, 12, 0)]), 24, 1).unwrap();
    assert_eq!(zero.sample(Duration::from_millis(16)).0, 1);
}

#[test]
fn rejects_oversized_and_malformed_assets_before_buffer_allocation() {
    assert!(parse_cursor(&vec![0; MAX_FILE_BYTES + 1], 24, 1).is_none());
    let mut bytes = file(&[(24, 24, 12, 30)]);
    bytes[12..16].copy_from_slice(&((MAX_FRAMES + 1) as u32).to_le_bytes());
    assert!(parse_cursor(&bytes, 24, 1).is_none());
    let mut bytes = file(&[(24, 24, 12, 30)]);
    bytes[16 + 8..16 + 12].copy_from_slice(&u32::MAX.to_le_bytes());
    assert!(parse_cursor(&bytes, 24, 1).is_none());
    let mut bytes = file(&[(24, 24, 12, 30)]);
    bytes[28 + 16..28 + 20].copy_from_slice(&2048u32.to_le_bytes());
    assert!(parse_cursor(&bytes, 24, 1).is_none());
    // Repeated TOC references must not bypass the aggregate pixel limit.
    let mut bytes = file(&[(1024, 1024, 0, 30)]);
    let frame = bytes.split_off(28);
    bytes.truncate(16);
    bytes[12..16].copy_from_slice(&3u32.to_le_bytes());
    for _ in 0..3 {
        bytes.extend([0xfffd0002, 1024, 52].into_iter().flat_map(u32::to_le_bytes));
    }
    bytes.extend(frame);
    assert!(parse_cursor(&bytes, 24, 1).is_none());
    assert!(parse_cursor(&file(&[(24, 24, 12, 30)])[..40], 24, 1).is_none());
}

#[test]
fn delayed_loading_does_not_block_cursor_callbacks_or_pointer_dispatch() {
    if !crate::startup_tests::private_runtime(
        "cursor::tests::delayed_loading_does_not_block_cursor_callbacks_or_pointer_dispatch",
    ) {
        return;
    }
    let mut events = EventLoop::try_new().unwrap();
    let mut state = crate::startup_tests::state(&mut events);
    let (started, calls) = mpsc::channel();
    let (release, held) = mpsc::channel();
    state.cursor_loader = Loader::with_loader(events.get_signal(), move |request| {
        started.send(request).unwrap();
        if request.0 == CursorIcon::Default {
            held.recv_timeout(Duration::from_secs(2)).unwrap();
        }
        None
    })
    .unwrap();
    assert_eq!(
        calls.recv_timeout(Duration::from_secs(2)).unwrap().0,
        CursorIcon::Default
    );
    let seat = state.seat.clone();
    state.cursor_image(&seat, CursorImageStatus::Named(CursorIcon::Pointer));
    state.cursor_image(&seat, CursorImageStatus::Named(CursorIcon::Text));
    let pointer = state.seat.get_pointer().unwrap();
    pointer.motion(
        &mut state,
        None,
        &MotionEvent {
            location: (10.0, 20.0).into(),
            serial: SERIAL_COUNTER.next_serial(),
            time: 1,
        },
    );
    state.cursor_image(&seat, CursorImageStatus::Named(CursorIcon::Text));
    crate::after_dispatch_with_redraw(&mut state, |_, _| {});
    assert_eq!(pointer.current_location(), (10.0, 20.0).into());
    assert!(
        state.named_cursor_frame().is_some(),
        "cached fallback must remain visible"
    );
    assert!(calls.try_recv().is_err(), "decoder is still deliberately blocked");
    release.send(()).unwrap();
    assert_eq!(calls.recv_timeout(Duration::from_secs(2)).unwrap().0, CursorIcon::Text);
}

#[test]
fn cursor_animation_advances_and_cancels_on_hide_and_shape_change() {
    if !crate::startup_tests::private_runtime(
        "cursor::tests::cursor_animation_advances_and_cancels_on_hide_and_shape_change",
    ) {
        return;
    }
    let mut events = EventLoop::try_new().unwrap();
    let mut state = crate::startup_tests::state(&mut events);
    // No background result can replace the synthetic fixture.
    state.cursor_loader = Loader::with_loader(events.get_signal(), |_| None).unwrap();
    let output = Output::new(
        "cursor-test".into(),
        PhysicalProperties {
            size: (0, 0).into(),
            subpixel: Subpixel::Unknown,
            make: "test".into(),
            model: "test".into(),
        },
    );
    output.change_current_state(
        Some(Mode {
            size: (800, 600).into(),
            refresh: 60_000,
        }),
        None,
        None,
        None,
    );
    state.space.map_output(&output, (0, 0));
    let asset = parse_cursor(&file(&[(24, 24, 12, 30), (24, 24, 12, 80)]), 24, 1).unwrap();
    state.named_cursors.insert(CursorIcon::Wait, asset);
    state.cursor_status = CursorImageStatus::Named(CursorIcon::Wait);
    let now = Instant::now();
    state.update_named_cursor(now);
    assert_eq!(state.cursor_animation.frame, 0);
    assert!(state.cursor_animation.timer.is_some());
    // Invoke the same deadline callback by dispatching the real timer.
    while Instant::now() < now + Duration::from_millis(35) {
        events.dispatch(Duration::from_millis(5), &mut state).unwrap();
    }
    state.update_named_cursor(now + Duration::from_millis(30));
    assert_eq!(state.cursor_animation.frame, 1);
    assert!(state.cursor_animation.timer.is_some());
    let seat = state.seat.clone();
    state.cursor_image(&seat, CursorImageStatus::Hidden);
    state.update_named_cursor(now + Duration::from_millis(31));
    assert!(state.cursor_animation.timer.is_none());
    state.cursor_image(&seat, CursorImageStatus::Named(CursorIcon::Wait));
    state.update_named_cursor(now + Duration::from_millis(32));
    assert!(state.cursor_animation.timer.is_some());
    state.named_cursors.insert(CursorIcon::Text, fallback_cursor());
    state.cursor_image(&seat, CursorImageStatus::Named(CursorIcon::Text));
    state.update_named_cursor(now + Duration::from_millis(33));
    assert!(state.cursor_animation.timer.is_none());
}

#[test]
fn supplies_legacy_fallback_names_for_common_cursors() {
    assert_eq!(cursor_names(CursorIcon::Default), ["default", "left_ptr", "arrow"]);
    assert_eq!(cursor_names(CursorIcon::Pointer), ["pointer", "hand2", "left_ptr"]);
    assert_eq!(cursor_names(CursorIcon::Text), ["text", "xterm", "left_ptr"]);
    assert_eq!(
        cursor_names(CursorIcon::Crosshair),
        ["crosshair", "default", "left_ptr"]
    );
}
