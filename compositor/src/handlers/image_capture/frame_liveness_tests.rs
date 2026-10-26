//! Real protocol objects and SHM reuse, with a delayed fence instead of a GPU.
use super::*;
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, OwnedFd};
use std::os::unix::fs::FileExt;
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, Ordering};

use smithay::backend::renderer::sync::{Fence, Interrupted, SyncPoint};
use smithay::output::{Mode, PhysicalProperties, Subpixel};
use smithay::reexports::calloop::EventLoop;
use smithay::reexports::wayland_protocols::ext::image_capture_source::v1::server::ext_image_capture_source_v1::ExtImageCaptureSourceV1;
use smithay::reexports::wayland_protocols::ext::image_copy_capture::v1::server::ext_image_copy_capture_manager_v1::ExtImageCopyCaptureManagerV1;
use smithay::reexports::wayland_server::protocol::wl_shm::WlShm;
use smithay::wayland::image_capture_source::ImageCaptureSourceData;
use crate::state::{ClientState, dispatch_test_client, test_client_request as request};

const WIDTH: i32 = 8;
const HEIGHT: i32 = 2;
const BYTES: usize = WIDTH as usize * HEIGHT as usize * 4;

#[derive(Debug)]
struct DelayedFence {
    ready: Arc<AtomicBool>,
    read: UnixStream,
}
impl Fence for DelayedFence {
    fn is_signaled(&self) -> bool {
        self.ready.load(Ordering::Acquire)
    }
    fn wait(&self) -> Result<(), Interrupted> {
        panic!("capture must not wait for the GPU on the event loop");
    }
    fn is_exportable(&self) -> bool {
        true
    }
    fn export(&self) -> Option<OwnedFd> {
        Some(self.read.try_clone().unwrap().into())
    }
}
fn delayed_fence() -> (SyncPoint, Arc<AtomicBool>, UnixStream) {
    let (read, write) = UnixStream::pair().unwrap();
    let ready = Arc::new(AtomicBool::new(false));
    (
        DelayedFence {
            ready: ready.clone(),
            read,
        }
        .into(),
        ready,
        write,
    )
}

fn private_capture_runtime(test: &str) -> bool {
    // Enable capture in a child process, then reuse the standard bounded runtime
    // fixture. No process-global environment is mutated alongside other tests.
    if std::env::var_os("FERESE_ENABLE_SCREENCOPY").is_none_or(|value| value != "1") {
        let result = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", test, "--nocapture"])
            .env("FERESE_ENABLE_SCREENCOPY", "1")
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        return false;
    }
    crate::startup_tests::private_runtime(test)
}

fn dispatch(events: &mut EventLoop<'static, Ferese>, state: &mut Ferese) {
    events.dispatch(Duration::from_millis(20), state).unwrap();
    state.display_handle.flush_clients().unwrap();
}
fn pixels(file: &std::fs::File) -> Vec<u8> {
    let mut bytes = vec![0; BYTES];
    file.read_exact_at(&mut bytes, 0).unwrap();
    bytes
}
fn messages(wire: &mut UnixStream) -> Vec<(u32, u16)> {
    wire.set_nonblocking(true).unwrap();
    let mut bytes = Vec::new();
    let _ = wire.read_to_end(&mut bytes);
    wire.set_nonblocking(false).unwrap();
    let mut result = Vec::new();
    let mut offset = 0;
    while offset < bytes.len() {
        let object = u32::from_ne_bytes(bytes[offset..offset + 4].try_into().unwrap());
        let header = u32::from_ne_bytes(bytes[offset + 4..offset + 8].try_into().unwrap());
        result.push((object, header as u16));
        offset += (header >> 16) as usize;
    }
    result
}

#[test]
fn destroyed_shm_frame_cannot_overwrite_reused_buffer_after_late_readiness() {
    if !private_capture_runtime(
        "handlers::image_capture::frame_liveness_tests::destroyed_shm_frame_cannot_overwrite_reused_buffer_after_late_readiness",
    ) {
        return;
    }
    let mut events = EventLoop::try_new().unwrap();
    let mut state = crate::startup_tests::state(&mut events);
    let output = Output::new(
        "capture-liveness".into(),
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
            size: (WIDTH, HEIGHT).into(),
            refresh: 60_000,
        }),
        None,
        None,
        None,
    );
    state.space.map_output(&output, (0, 0));
    state.register_output(&output, "capture-liveness".into());
    let (server, mut wire) = UnixStream::pair().unwrap();
    let client = state
        .display_handle
        .insert_client(server, Arc::new(ClientState::default()))
        .unwrap();
    let source = ImageCaptureSource::new();
    source
        .user_data()
        .insert_if_missing(|| Source::Output(output.downgrade()));
    let resource = client
        .create_resource::<ExtImageCaptureSourceV1, _, Ferese>(
            &state.display_handle,
            1,
            ImageCaptureSourceData {
                source: source.clone(),
            },
        )
        .unwrap();
    source.add_instance(&resource);
    let manager = client
        .create_resource::<ExtImageCopyCaptureManagerV1, _, Ferese>(
            &state.display_handle,
            1,
            smithay::wayland::GlobalData,
        )
        .unwrap();
    let shm = client
        .create_resource::<WlShm, _, Ferese>(&state.display_handle, 1, smithay::wayland::GlobalData)
        .unwrap();
    let file = tempfile::tempfile().unwrap();
    file.set_len(BYTES as u64).unwrap();
    file.write_all_at(&[0xb7; BYTES], 0).unwrap();
    request(
        &mut wire,
        manager.id().protocol_id(),
        0,
        &[2, resource.id().protocol_id(), 0],
        None,
    );
    request(
        &mut wire,
        shm.id().protocol_id(),
        0,
        &[3, BYTES as u32],
        Some(file.as_raw_fd()),
    );
    request(
        &mut wire,
        3,
        0,
        &[4, 0, WIDTH as u32, HEIGHT as u32, WIDTH as u32 * 4, 1],
        None,
    );
    request(&mut wire, 2, 0, &[5], None);
    request(&mut wire, 5, 1, &[4], None);
    request(&mut wire, 5, 3, &[], None);
    dispatch_test_client(&mut events, &mut state);
    assert_eq!(state.image_capture.active.len(), 1);
    let active = &mut state.image_capture.active[0];
    let old = active
        .pending
        .take()
        .expect("first captured frame was accepted");
    let old_reference = (*old).clone();
    let session = active.session.as_ref();
    let (valid, complete) =
        state.stream_readback_callbacks(old, session.clone(), (WIDTH, HEIGHT).into());
    let (old_fence, old_ready, mut old_signal) = delayed_fence();
    let retained_old_fence = old_fence.clone();
    let old_pixels = [99, 11, 3, 255].repeat((WIDTH * HEIGHT) as usize);
    state.test_queue_readback_pixels(old_fence, valid, complete, old_pixels);
    dispatch(&mut events, &mut state);
    assert_eq!(pixels(&file), [0xb7; BYTES]);

    request(&mut wire, 5, 0, &[], None);
    dispatch_test_client(&mut events, &mut state);
    assert!(!old_reference.alive());
    assert!(
        session.alive(),
        "destroying a frame must keep its session alive"
    );
    // The same client-owned buffer is immediately reused by a later capture.
    request(&mut wire, 2, 0, &[6], None);
    request(&mut wire, 6, 1, &[4], None);
    request(&mut wire, 6, 3, &[], None);
    dispatch_test_client(&mut events, &mut state);
    let new = state.image_capture.active[0]
        .pending
        .take()
        .expect("replacement frame was accepted");
    let new_pixels = [8, 32, 64, 255].repeat((WIDTH * HEIGHT) as usize);
    let (valid, complete) = state.stream_readback_callbacks(new, session, (WIDTH, HEIGHT).into());
    let (new_fence, new_ready, mut new_signal) = delayed_fence();
    state.test_queue_readback_pixels(new_fence, valid, complete, new_pixels.clone());
    new_ready.store(true, Ordering::Release);
    new_signal.write_all(&[1]).unwrap();
    for _ in 0..5 {
        dispatch(&mut events, &mut state);
        if pixels(&file) == new_pixels {
            break;
        }
    }
    assert_eq!(pixels(&file), new_pixels);
    old_ready.store(true, Ordering::Release);
    old_signal.write_all(&[1]).unwrap();
    dispatch(&mut events, &mut state);
    assert_eq!(
        pixels(&file),
        new_pixels,
        "an abandoned frame must never overwrite a reused buffer"
    );
    let delivered = messages(&mut wire);
    assert!(
        delivered.contains(&(6, 3)),
        "replacement frame should become ready"
    );
    assert!(!delivered.contains(&(1, 0)), "client must remain connected");
    drop(retained_old_fence);
}
