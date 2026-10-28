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
    messages_with_arguments(wire)
        .into_iter()
        .map(|(object, opcode, _)| (object, opcode))
        .collect()
}
fn messages_with_arguments(wire: &mut UnixStream) -> Vec<(u32, u16, Vec<u32>)> {
    wire.set_nonblocking(true).unwrap();
    let mut bytes = Vec::new();
    let _ = wire.read_to_end(&mut bytes);
    wire.set_nonblocking(false).unwrap();
    let mut result = Vec::new();
    let mut offset = 0;
    while offset < bytes.len() {
        let object = u32::from_ne_bytes(bytes[offset..offset + 4].try_into().unwrap());
        let header = u32::from_ne_bytes(bytes[offset + 4..offset + 8].try_into().unwrap());
        let end = offset + (header >> 16) as usize;
        let arguments = bytes[offset + 8..end]
            .chunks_exact(4)
            .map(|word| u32::from_ne_bytes(word.try_into().unwrap()))
            .collect();
        result.push((object, header as u16, arguments));
        offset = end;
    }
    result
}

struct CaptureFixture {
    events: EventLoop<'static, Ferese>,
    state: Ferese,
    output: Output,
    wire: UnixStream,
    file: std::fs::File,
    shm_id: u32,
}

fn capture_fixture() -> CaptureFixture {
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
    CaptureFixture {
        events,
        state,
        output,
        wire,
        file,
        shm_id: shm.id().protocol_id(),
    }
}

#[test]
fn destroyed_shm_frame_cannot_overwrite_reused_buffer_after_late_readiness() {
    if !private_capture_runtime(
        "handlers::image_capture::frame_liveness_tests::destroyed_shm_frame_cannot_overwrite_reused_buffer_after_late_readiness",
    ) {
        return;
    }
    let CaptureFixture {
        mut events,
        mut state,
        output: _,
        mut wire,
        file,
        ..
    } = capture_fixture();
    let active = &mut state.image_capture.active[0];
    let old = active
        .pending
        .take()
        .expect("first captured frame was accepted");
    let old_reference = (*old).clone();
    let session = active.session.as_ref();
    let generation = next_submission(&session, &old);
    let (valid, complete) =
        state.stream_readback_callbacks(old, session.clone(), generation, (WIDTH, HEIGHT).into());
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
    let generation = next_submission(&session, &new);
    let (valid, complete) =
        state.stream_readback_callbacks(new, session, generation, (WIDTH, HEIGHT).into());
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

#[test]
fn output_render_failure_fails_pending_frame_and_allows_shm_retry() {
    if !private_capture_runtime(
        "handlers::image_capture::frame_liveness_tests::output_render_failure_fails_pending_frame_and_allows_shm_retry",
    ) {
        return;
    }
    let CaptureFixture {
        mut events,
        mut state,
        output,
        mut wire,
        file,
        ..
    } = capture_fixture();
    messages(&mut wire);
    state.image_capture.active[0].revision = Some(u64::MAX);
    let failed: Option<()> =
        crate::backends::direct::capture::capture_request(&mut state, &output, false, || {
            Err("injected offscreen allocation failure")
        });
    assert!(failed.is_none());
    assert!(state.image_capture.active[0].pending.is_none());
    assert!(state.image_capture.active[0].revision.is_none());
    state.display_handle.flush_clients().unwrap();
    let delivered = messages(&mut wire);
    assert!(
        delivered.contains(&(5, 4)),
        "render failure must fail the frame"
    );
    assert!(
        !delivered.contains(&(5, 3)),
        "failed frame must not become ready"
    );
    assert_eq!(pixels(&file), [0xb7; BYTES]);

    request(&mut wire, 5, 0, &[], None);
    request(&mut wire, 2, 0, &[6], None);
    request(&mut wire, 6, 1, &[4], None);
    request(&mut wire, 6, 3, &[], None);
    dispatch_test_client(&mut events, &mut state);
    let active = &mut state.image_capture.active[0];
    let frame = active.pending.take().expect("retry frame was accepted");
    let session = active.session.as_ref();
    let expected = [12, 34, 56, 255].repeat((WIDTH * HEIGHT) as usize);
    let generation = next_submission(&session, &frame);
    let (valid, complete) =
        state.stream_readback_callbacks(frame, session, generation, (WIDTH, HEIGHT).into());
    let (fence, ready, _signal) = delayed_fence();
    ready.store(true, Ordering::Release);
    state.test_queue_readback_pixels(fence, valid, complete, expected.clone());
    dispatch(&mut events, &mut state);
    assert_eq!(pixels(&file), expected);
    let delivered = messages(&mut wire);
    assert!(delivered.contains(&(6, 3)), "retry frame must become ready");
    assert!(!delivered.contains(&(1, 0)), "client must remain connected");
}

#[test]
fn canceled_shm_failure_cannot_reset_a_replacement_submission() {
    if !private_capture_runtime(
        "handlers::image_capture::frame_liveness_tests::canceled_shm_failure_cannot_reset_a_replacement_submission",
    ) {
        return;
    }
    let CaptureFixture {
        mut events,
        mut state,
        output: _,
        mut wire,
        file,
        ..
    } = capture_fixture();
    messages(&mut wire);
    let active = &mut state.image_capture.active[0];
    let old = active.pending.take().unwrap();
    let session = active.session.as_ref();
    let old_generation = next_submission(&session, &old);
    let (valid, complete) = state.stream_readback_callbacks(
        old,
        session.clone(),
        old_generation,
        (WIDTH, HEIGHT).into(),
    );
    let expected = [12, 34, 56, 255].repeat((WIDTH * HEIGHT) as usize);
    let (fence, ready, mut signal) = delayed_fence();
    let retained_fence = fence.clone();
    state.test_queue_readback_pixels(fence, valid, complete, expected.clone());

    // Accept a replacement on the real protocol before the canceled readback
    // publishes its failure. Both submissions capture the same source pixels.
    request(&mut wire, 5, 0, &[], None);
    request(&mut wire, 2, 0, &[6], None);
    request(&mut wire, 6, 1, &[4], None);
    request(&mut wire, 6, 3, &[], None);
    dispatch_test_client(&mut events, &mut state);
    state.prune_readbacks();
    let new = state.image_capture.active[0].pending.take().unwrap();
    let new_generation = next_submission(&session, &new);
    let (valid, complete) = state.stream_readback_callbacks(
        new,
        session.clone(),
        new_generation,
        (WIDTH, HEIGHT).into(),
    );
    let stamp = WindowStamp {
        output: state.space.outputs().next().unwrap().clone(),
        topology: 0,
        surfaces: Arc::new(Vec::new()),
        geometry: Rectangle::from_size((WIDTH, HEIGHT).into()),
        scale: 1.,
        epoch: state.capture_epoch,
        cursor: None,
        appearance: WindowAppearance::new(&state),
    };
    state.image_capture.active[0].revision = Some(42);
    state.image_capture.active[0].window_stamp = Some(stamp);
    assert!(valid(&state));
    complete(&mut state, Ok(expected.clone()));
    assert_eq!(pixels(&file), expected);

    // The old idle error must not erase the newer successful delivery. A late
    // signal must not deliver the abandoned frame or touch its reused buffer.
    ready.store(true, Ordering::Release);
    signal.write_all(&[1]).unwrap();
    dispatch(&mut events, &mut state);
    assert_eq!(state.image_capture.active[0].revision, Some(42));
    assert!(state.image_capture.active[0].window_stamp.is_some());
    assert_eq!(pixels(&file), expected);
    let delivered = messages(&mut wire);
    assert!(delivered.contains(&(6, 3)));
    assert!(!delivered.contains(&(5, 3)));

    request(&mut wire, 6, 0, &[], None);
    request(&mut wire, 2, 0, &[7], None);
    request(&mut wire, 7, 1, &[4], None);
    request(&mut wire, 7, 3, &[], None);
    dispatch_test_client(&mut events, &mut state);
    dispatch(&mut events, &mut state);
    let active = &state.image_capture.active[0];
    assert_eq!(
        active.revision,
        Some(42),
        "unchanged source stays delivered"
    );
    assert!(
        active.pending.is_some(),
        "fresh frame waits for source damage"
    );
    assert!(!messages(&mut wire).contains(&(7, 3)));
    drop(retained_fence);
}

#[test]
fn deferred_gpu_failure_cannot_reset_a_newer_common_submission() {
    if !private_capture_runtime(
        "handlers::image_capture::frame_liveness_tests::deferred_gpu_failure_cannot_reset_a_newer_common_submission",
    ) {
        return;
    }
    let CaptureFixture {
        mut events,
        mut state,
        output: _,
        mut wire,
        file: _,
        ..
    } = capture_fixture();
    let active = &mut state.image_capture.active[0];
    let old = active.pending.take().unwrap();
    let session = active.session.as_ref();
    let old_generation = next_submission(&session, &old);
    request(&mut wire, 5, 0, &[], None);
    request(&mut wire, 2, 0, &[6], None);
    request(&mut wire, 6, 1, &[4], None);
    request(&mut wire, 6, 3, &[], None);
    dispatch_test_client(&mut events, &mut state);
    state.fail_stream_submission(old, session.clone(), old_generation, FailureReason::Unknown);
    let new = state.image_capture.active[0].pending.take().unwrap();
    state.image_capture.active[0].revision = Some(43);
    state.finish_stream_frame(
        new,
        session.clone(),
        Ok(crate::render::StreamCopy::Gpu(SyncPoint::signaled())),
    );
    assert!(!current_submission(&session, old_generation));
    dispatch(&mut events, &mut state);
    assert_eq!(state.image_capture.active[0].revision, Some(43));
    let delivered = messages(&mut wire);
    assert!(delivered.contains(&(6, 3)));
    assert!(!delivered.contains(&(5, 3)));
}

fn submit_test_pixels(
    state: &mut Ferese,
    frame: Frame,
    session: &SessionRef,
    fence: SyncPoint,
    pixels: Vec<u8>,
    gpu: bool,
) {
    if gpu {
        state.finish_stream_frame(
            frame,
            session.clone(),
            Ok(crate::render::StreamCopy::Gpu(fence)),
        );
    } else {
        // Inject owned pixels at the PBO boundary; all ownership, validation,
        // synchronization and protocol completion use the production path.
        let generation = next_submission(session, &frame);
        let (valid, complete) = state.stream_readback_callbacks(
            frame,
            session.clone(),
            generation,
            (WIDTH, HEIGHT).into(),
        );
        state.test_queue_readback_pixels(fence, valid, complete, pixels);
    }
}

fn next_frame(fixture: &mut CaptureFixture, old: u32, next: u32) {
    request(&mut fixture.wire, old, 0, &[], None);
    request(&mut fixture.wire, 2, 0, &[next], None);
    request(&mut fixture.wire, next, 1, &[4], None);
    request(&mut fixture.wire, next, 3, &[], None);
    dispatch_test_client(&mut fixture.events, &mut fixture.state);
}

#[test]
fn refresh_during_submitted_capture_is_satisfied_once_but_waiting_capture_resumes() {
    if !private_capture_runtime(
        "handlers::image_capture::frame_liveness_tests::refresh_during_submitted_capture_is_satisfied_once_but_waiting_capture_resumes",
    ) {
        return;
    }
    for gpu in [false, true] {
        let mut fixture = capture_fixture();
        messages(&mut fixture.wire);
        let appearance = WindowAppearance::new(&fixture.state);
        let active = &mut fixture.state.image_capture.active[0];
        let frame = active.pending.take().unwrap();
        let session = active.session.as_ref();
        let reference = (*frame).clone();
        active.revision = Some(51);
        active.window_stamp = Some(WindowStamp {
            output: fixture.output.clone(),
            topology: 0,
            surfaces: Arc::new(Vec::new()),
            geometry: Rectangle::from_size((WIDTH, HEIGHT).into()),
            scale: 1.,
            epoch: fixture.state.capture_epoch,
            cursor: None,
            appearance,
        });
        let expected = [8, 32, 64, 255].repeat((WIDTH * HEIGHT) as usize);
        let (fence, ready, mut signal) = delayed_fence();
        submit_test_pixels(
            &mut fixture.state,
            frame,
            &session,
            fence,
            expected.clone(),
            gpu,
        );
        assert_eq!(submitted_frame(&session), Some(reference));

        // PipeWire can enter Streaming twice while the first accepted copy is
        // still completing. Both refreshes must be satisfied by that one image.
        fixture.state.refresh_capture_source(&session.source());
        fixture.state.refresh_capture_source(&session.source());
        // An unrelated legacy/IPC preparation error has no pending stream
        // frame to fail. It must leave this already-submitted copy intact.
        fixture.state.fail_screencopies(&fixture.output, false);
        let active = &fixture.state.image_capture.active[0];
        assert_eq!(active.revision, Some(51));
        assert!(active.window_stamp.is_some());
        ready.store(true, Ordering::Release);
        signal.write_all(&[1]).unwrap();
        dispatch(&mut fixture.events, &mut fixture.state);
        assert!(submitted_frame(&session).is_none());
        assert!(messages(&mut fixture.wire).contains(&(5, 3)));
        if !gpu {
            assert_eq!(pixels(&fixture.file), expected);
        }

        next_frame(&mut fixture, 5, 6);
        let active = &mut fixture.state.image_capture.active[0];
        assert!(active.take_changed_frame(51).is_none());
        assert!(
            active.pending.is_some(),
            "unchanged pixels must stay pending"
        );
        // Genuine source damage admits the next frame through the same revision
        // gate used by process_output_streams, with no new refresh request.
        let frame = active.take_changed_frame(52).unwrap();
        active.revision = Some(52);
        let changed = [16, 64, 128, 255].repeat((WIDTH * HEIGHT) as usize);
        submit_test_pixels(
            &mut fixture.state,
            frame,
            &session,
            SyncPoint::signaled(),
            changed.clone(),
            gpu,
        );
        dispatch(&mut fixture.events, &mut fixture.state);
        assert!(messages(&mut fixture.wire).contains(&(6, 3)));
        if !gpu {
            assert_eq!(pixels(&fixture.file), changed);
        }

        next_frame(&mut fixture, 6, 7);
        let active = &mut fixture.state.image_capture.active[0];
        assert!(active.take_changed_frame(52).is_none());
        assert!(submitted_frame(&session).is_none());
        fixture.state.refresh_capture_source(&session.source());
        let active = &mut fixture.state.image_capture.active[0];
        assert!(
            active.revision.is_none(),
            "a resumed consumer needs an image"
        );
        let frame = active.take_changed_frame(52).unwrap();
        active.revision = Some(52);
        submit_test_pixels(
            &mut fixture.state,
            frame,
            &session,
            SyncPoint::signaled(),
            changed,
            gpu,
        );
        dispatch(&mut fixture.events, &mut fixture.state);
        assert!(messages(&mut fixture.wire).contains(&(7, 3)));
        next_frame(&mut fixture, 7, 8);
        assert!(
            fixture.state.image_capture.active[0]
                .take_changed_frame(52)
                .is_none()
        );
        assert!(!messages(&mut fixture.wire).contains(&(8, 3)));
    }
}

#[test]
fn destroying_submitted_capture_does_not_hide_a_waiting_refresh() {
    if !private_capture_runtime(
        "handlers::image_capture::frame_liveness_tests::destroying_submitted_capture_does_not_hide_a_waiting_refresh",
    ) {
        return;
    }
    for gpu in [false, true] {
        let mut fixture = capture_fixture();
        let active = &mut fixture.state.image_capture.active[0];
        let frame = active.pending.take().unwrap();
        let session = active.session.as_ref();
        active.revision = Some(61);
        let (fence, _, _signal) = delayed_fence();
        submit_test_pixels(
            &mut fixture.state,
            frame,
            &session,
            fence,
            vec![0; BYTES],
            gpu,
        );
        assert!(submitted_frame(&session).is_some());
        next_frame(&mut fixture, 5, 6);
        assert!(submitted_frame(&session).is_none());
        let active = &mut fixture.state.image_capture.active[0];
        active.revision = Some(61);
        assert!(active.take_changed_frame(61).is_none());
        fixture.state.refresh_capture_source(&session.source());
        let active = &mut fixture.state.image_capture.active[0];
        assert!(active.revision.is_none());
        let frame = active.take_changed_frame(61).unwrap();
        active.revision = Some(61);
        submit_test_pixels(
            &mut fixture.state,
            frame,
            &session,
            SyncPoint::signaled(),
            vec![7; BYTES],
            gpu,
        );
        fixture.state.prune_readbacks();
        dispatch(&mut fixture.events, &mut fixture.state);
        assert!(messages(&mut fixture.wire).contains(&(6, 3)));
        assert_eq!(fixture.state.image_capture.active[0].revision, Some(61));
    }
}

#[test]
fn stale_readbacks_publish_resize_constraints_and_keep_valid_sources_retryable() {
    if !private_capture_runtime(
        "handlers::image_capture::frame_liveness_tests::stale_readbacks_publish_resize_constraints_and_keep_valid_sources_retryable",
    ) {
        return;
    }
    // Resize, same-size transform, privacy epoch, source removal, source pause.
    for change in 0..5 {
        let mut fixture = capture_fixture();
        messages(&mut fixture.wire);
        let active = &mut fixture.state.image_capture.active[0];
        let frame = active.pending.take().unwrap();
        let session = active.session.as_ref();
        let (fence, _ready, _signal) = delayed_fence();
        submit_test_pixels(
            &mut fixture.state,
            frame,
            &session,
            fence,
            vec![9; BYTES],
            false,
        );
        let size: Size<i32, Buffer> = if change == 0 {
            (WIDTH * 2, HEIGHT / 2).into()
        } else {
            (WIDTH, HEIGHT).into()
        };
        match change {
            0 => fixture.output.change_current_state(
                Some(Mode {
                    size: (size.w, size.h).into(),
                    refresh: 60_000,
                }),
                None,
                None,
                None,
            ),
            1 => fixture
                .output
                .change_current_state(None, Some(Transform::_180), None, None),
            2 => fixture.state.capture_epoch = fixture.state.capture_epoch.wrapping_add(1),
            3 => {
                fixture.state.output_ids.remove(&fixture.output);
            }
            4 => {
                session
                    .source()
                    .user_data()
                    .insert_if_missing(super::SourcePaused::default);
                session
                    .source()
                    .user_data()
                    .get::<super::SourcePaused>()
                    .unwrap()
                    .0
                    .store(true, Ordering::Relaxed);
            }
            _ => unreachable!(),
        }
        // Deliberately do not call refresh_capture_sessions: the readback
        // completion must publish a resize observed before normal republishing.
        assert_eq!(
            session.current_constraints().unwrap().size,
            (WIDTH, HEIGHT).into()
        );
        fixture.state.prune_readbacks();
        dispatch(&mut fixture.events, &mut fixture.state);
        assert_eq!(pixels(&fixture.file), [0xb7; BYTES]);
        assert!(submitted_frame(&session).is_none());
        let delivered = messages_with_arguments(&mut fixture.wire);
        let expected_reason = match change {
            0 => 1,
            3 => 2,
            _ => 0,
        };
        let failed = delivered
            .iter()
            .position(|event| event == &(5, 4, vec![expected_reason]))
            .expect("stale frame needs the correct failure reason");
        assert!(!delivered.iter().any(|event| event.0 == 5 && event.1 == 3));
        if change == 0 {
            let constraints = delivered
                .iter()
                .position(|event| event == &(2, 0, vec![size.w as u32, size.h as u32]))
                .expect("new dimensions must precede buffer-constraints failure");
            assert!(constraints < failed);
            assert_eq!(session.current_constraints().unwrap().size, size);
        } else if change == 3 {
            continue;
        } else if change == 4 {
            session
                .source()
                .user_data()
                .get::<super::SourcePaused>()
                .unwrap()
                .0
                .store(false, Ordering::Relaxed);
        }

        request(&mut fixture.wire, 5, 0, &[], None);
        let buffer = if change == 0 {
            request(
                &mut fixture.wire,
                fixture.shm_id,
                0,
                &[6, BYTES as u32],
                Some(fixture.file.as_raw_fd()),
            );
            request(
                &mut fixture.wire,
                6,
                0,
                &[7, 0, size.w as u32, size.h as u32, size.w as u32 * 4, 1],
                None,
            );
            request(&mut fixture.wire, 6, 1, &[], None);
            7
        } else {
            4
        };
        let next = if change == 0 { 8 } else { 6 };
        request(&mut fixture.wire, 2, 0, &[next], None);
        request(&mut fixture.wire, next, 1, &[buffer], None);
        request(&mut fixture.wire, next, 3, &[], None);
        dispatch_test_client(&mut fixture.events, &mut fixture.state);
        assert_eq!(
            fixture.state.image_capture.active.len(),
            1,
            "retry session for invalidation {change} must stay connected"
        );
        let active = &mut fixture.state.image_capture.active[0];
        let frame = active.pending.take().expect("live session accepts retry");
        let generation = next_submission(&session, &frame);
        let (valid, complete) =
            fixture
                .state
                .stream_readback_callbacks(frame, session.clone(), generation, size);
        let expected = vec![23; BYTES];
        fixture.state.test_queue_readback_pixels(
            SyncPoint::signaled(),
            valid,
            complete,
            expected.clone(),
        );
        dispatch(&mut fixture.events, &mut fixture.state);
        assert_eq!(pixels(&fixture.file), expected);
        assert!(messages(&mut fixture.wire).contains(&(next, 3)));
    }
}

#[test]
fn mapped_stream_copy_preserves_shm_guards_and_rejects_incomplete_pixels() {
    if !private_capture_runtime(
        "handlers::image_capture::frame_liveness_tests::mapped_stream_copy_preserves_shm_guards_and_rejects_incomplete_pixels",
    ) {
        return;
    }
    let mut fixture = capture_fixture();
    let active = &mut fixture.state.image_capture.active[0];
    let frame = active.pending.take().unwrap();
    let session = active.session.as_ref();
    let generation = next_submission(&session, &frame);
    let (valid, consume, complete) = fixture.state.stream_readback_mapped_callbacks(
        frame,
        session.clone(),
        generation,
        (WIDTH, HEIGHT).into(),
    );
    assert!(valid(&fixture.state));
    let expected = [4, 17, 203, 255].repeat((WIDTH * HEIGHT) as usize);
    let result = consume(&expected[..expected.len() - 1]);
    assert!(result.is_err());
    complete(&mut fixture.state, result);
    assert_eq!(
        pixels(&fixture.file),
        [0xb7; BYTES],
        "a short PBO must not partially overwrite client memory"
    );

    request(&mut fixture.wire, 5, 0, &[], None);
    let stride = WIDTH * 4 + 16;
    let offset = 16;
    let length = offset + stride * HEIGHT;
    let file = tempfile::tempfile().unwrap();
    let guards = vec![0x6du8; length as usize];
    file.write_all_at(&guards, 0).unwrap();
    request(
        &mut fixture.wire,
        fixture.shm_id,
        0,
        &[6, length as u32],
        Some(file.as_raw_fd()),
    );
    request(
        &mut fixture.wire,
        6,
        0,
        &[
            7,
            offset as u32,
            WIDTH as u32,
            HEIGHT as u32,
            stride as u32,
            1,
        ],
        None,
    );
    request(&mut fixture.wire, 2, 0, &[8], None);
    request(&mut fixture.wire, 8, 1, &[7], None);
    request(&mut fixture.wire, 8, 3, &[], None);
    dispatch_test_client(&mut fixture.events, &mut fixture.state);
    let frame = fixture.state.image_capture.active[0]
        .pending
        .take()
        .unwrap();
    let generation = next_submission(&session, &frame);
    let (valid, consume, complete) = fixture.state.stream_readback_mapped_callbacks(
        frame,
        session,
        generation,
        (WIDTH, HEIGHT).into(),
    );
    assert!(valid(&fixture.state));
    let result = consume(&expected);
    assert!(result.is_ok());
    complete(&mut fixture.state, result);
    let mut actual = vec![0; length as usize];
    file.read_exact_at(&mut actual, 0).unwrap();
    let mut guarded_expected = guards;
    for y in 0..HEIGHT as usize {
        let row = WIDTH as usize * 4;
        let start = offset as usize + y * stride as usize;
        guarded_expected[start..start + row].copy_from_slice(&expected[y * row..(y + 1) * row]);
    }
    assert_eq!(
        actual, guarded_expected,
        "copy only image rows, preserving offset and stride padding"
    );
}

#[test]
fn window_capture_caches_follow_local_tree_changes_and_subsurface_destruction() {
    if !private_capture_runtime(
        "handlers::image_capture::frame_liveness_tests::window_capture_caches_follow_local_tree_changes_and_subsurface_destruction",
    ) {
        return;
    }
    use smithay::reexports::wayland_server::protocol::wl_compositor::WlCompositor;
    use smithay::reexports::wayland_server::protocol::wl_subcompositor::WlSubcompositor;
    let mut fixture = capture_fixture();
    let (window, mut wire) =
        crate::state::test_window(&mut fixture.state, &mut fixture.events, 0xffff0000);
    let (other, mut other_wire) =
        crate::state::test_window(&mut fixture.state, &mut fixture.events, 0xff00ff00);
    let id = fixture.state.windows.ids()[&window];
    let geometry = window.geometry();
    let first = window_surfaces(&window, geometry);
    assert!(Arc::ptr_eq(&first, &window_surfaces(&window, geometry)));
    request(&mut other_wire, 2, 6, &[], None);
    dispatch_test_client(&mut fixture.events, &mut fixture.state);
    assert!(
        Arc::ptr_eq(&first, &window_surfaces(&window, geometry)),
        "an unrelated client commit must not rebuild this stamp"
    );
    assert_ne!(super::super::window_capture::tree_revision(&other), 0);
    let initial = fixture.state.window_capture_info(id).unwrap().scale;
    let topology = super::super::window_capture::topology_revision(&window);

    request(&mut wire, 2, 3, &[7], None);
    request(&mut wire, 2, 6, &[], None);
    dispatch_test_client(&mut fixture.events, &mut fixture.state);
    let committed = window_surfaces(&window, geometry);
    assert!(!Arc::ptr_eq(&first, &committed));
    assert_eq!(
        topology,
        super::super::window_capture::topology_revision(&window)
    );
    let callback_stamp = |surfaces| WindowStamp {
        output: fixture.output.clone(),
        topology,
        surfaces,
        geometry,
        scale: initial,
        epoch: fixture.state.capture_epoch,
        cursor: None,
        appearance: WindowAppearance::new(&fixture.state),
    };
    assert!(
        callback_stamp(first.clone()) == callback_stamp(committed.clone()),
        "a callback-only commit must retain semantic capture equality"
    );
    assert!(window_pending_frame_callbacks(&window, geometry));
    smithay::desktop::utils::send_frames_surface_tree(
        window.toplevel().unwrap().wl_surface(),
        &fixture.output,
        fixture.state.start_time.elapsed(),
        None,
        |_, _| Some(fixture.output.clone()),
    );
    update_window_frame_callbacks(&window);
    assert!(!window_pending_frame_callbacks(&window, geometry));
    assert!(Arc::ptr_eq(&committed, &window_surfaces(&window, geometry)));

    let client = window.toplevel().unwrap().wl_surface().client().unwrap();
    let compositor = client
        .create_resource::<WlCompositor, _, Ferese>(
            &fixture.state.display_handle,
            6,
            smithay::wayland::GlobalData,
        )
        .unwrap();
    let subcompositor = client
        .create_resource::<WlSubcompositor, _, Ferese>(
            &fixture.state.display_handle,
            1,
            smithay::wayland::GlobalData,
        )
        .unwrap();
    request(&mut wire, compositor.id().protocol_id(), 0, &[8], None);
    request(
        &mut wire,
        subcompositor.id().protocol_id(),
        1,
        &[9, 8, 2],
        None,
    );
    request(&mut wire, 8, 8, &[2], None);
    request(&mut wire, 8, 1, &[6, 0, 0], None);
    request(&mut wire, 8, 6, &[], None);
    request(&mut wire, 2, 6, &[], None);
    dispatch_test_client(&mut fixture.events, &mut fixture.state);
    assert_eq!(
        fixture.state.window_capture_info(id).unwrap().scale,
        initial.max(2.0)
    );
    let with_child = window_surfaces(&window, geometry);
    assert!(
        with_child
            .iter()
            .any(|surface| surface.id.protocol_id() == 8)
    );
    let old_topology = super::super::window_capture::topology_revision(&window);
    request(&mut wire, 9, 3, &[2], None);
    dispatch_test_client(&mut fixture.events, &mut fixture.state);
    let reordered = window_surfaces(&window, geometry);
    assert!(
        !Arc::ptr_eq(&with_child, &reordered),
        "a reorder-only request must invalidate the source tree"
    );
    let stamp_for = |surfaces, topology| WindowStamp {
        output: fixture.output.clone(),
        topology,
        surfaces,
        geometry,
        scale: initial.max(2.0),
        epoch: fixture.state.capture_epoch,
        cursor: None,
        appearance: WindowAppearance::new(&fixture.state),
    };
    assert!(
        stamp_for(with_child.clone(), old_topology)
            != stamp_for(
                reordered,
                super::super::window_capture::topology_revision(&window)
            ),
        "equal preorder descriptors must not hide a stacking revision"
    );
    request(&mut wire, 9, 0, &[], None);
    dispatch_test_client(&mut fixture.events, &mut fixture.state);
    assert_eq!(
        fixture.state.window_capture_info(id).unwrap().scale,
        initial,
        "detaching a dense child must invalidate scale without another parent commit"
    );
    let without_child = window_surfaces(&window, geometry);
    assert!(!Arc::ptr_eq(&with_child, &without_child));
    assert!(
        !without_child
            .iter()
            .any(|surface| surface.id.protocol_id() == 8)
    );
    request(
        &mut wire,
        subcompositor.id().protocol_id(),
        1,
        &[10, 8, 2],
        None,
    );
    request(&mut wire, 8, 6, &[], None);
    request(&mut wire, 2, 6, &[], None);
    dispatch_test_client(&mut fixture.events, &mut fixture.state);
    assert_eq!(
        fixture.state.window_capture_info(id).unwrap().scale,
        initial.max(2.0)
    );
    request(&mut wire, 4, 0, &[], None);
    request(&mut wire, 3, 0, &[], None);
    request(&mut wire, 2, 0, &[], None);
    dispatch_test_client(&mut fixture.events, &mut fixture.state);
    assert!(
        fixture.state.window_capture_info(id).is_none(),
        "destroying the parent must close capture even if child surfaces and cached stamps remain alive"
    );
}

#[test]
fn session_lock_revokes_a_paused_window_session_without_a_pending_frame() {
    if !private_capture_runtime(
        "handlers::image_capture::frame_liveness_tests::session_lock_revokes_a_paused_window_session_without_a_pending_frame",
    ) {
        return;
    }
    use smithay::reexports::wayland_protocols::ext::session_lock::v1::server::ext_session_lock_manager_v1::ExtSessionLockManagerV1;
    let mut fixture = capture_fixture();
    assert!(fixture.state.nested_backend.is_none());
    let (window, _window_wire) =
        crate::state::test_window(&mut fixture.state, &mut fixture.events, 0xffff0000);
    let id = fixture.state.windows.ids()[&window];
    let (server, mut wire) = UnixStream::pair().unwrap();
    let client = fixture
        .state
        .display_handle
        .insert_client(
            server,
            Arc::new(ClientState {
                capabilities: crate::private_client::ClientCapabilities::WINDOW_CAPTURE,
                ..ClientState::default()
            }),
        )
        .unwrap();
    let source = ImageCaptureSource::new();
    source.user_data().insert_if_missing(|| Source::Window(id));
    source
        .user_data()
        .insert_if_missing(|| SourcePaused(AtomicBool::new(true)));
    let resource = client
        .create_resource::<ExtImageCaptureSourceV1, _, Ferese>(
            &fixture.state.display_handle,
            1,
            ImageCaptureSourceData {
                source: source.clone(),
            },
        )
        .unwrap();
    source.add_instance(&resource);
    let manager = client
        .create_resource::<ExtImageCopyCaptureManagerV1, _, Ferese>(
            &fixture.state.display_handle,
            1,
            smithay::wayland::GlobalData,
        )
        .unwrap();
    request(
        &mut wire,
        manager.id().protocol_id(),
        0,
        &[2, resource.id().protocol_id(), 0],
        None,
    );
    dispatch_test_client(&mut fixture.events, &mut fixture.state);
    let session = fixture
        .state
        .image_capture
        .active
        .iter()
        .find(|active| active.session.source() == source)
        .unwrap();
    assert!(session.pending.is_none());
    assert!(!source_active(&source));
    let session = session.session.as_ref();
    assert!(session.alive());
    messages(&mut wire);
    let lock_manager = client
        .create_resource::<ExtSessionLockManagerV1, _, Ferese>(
            &fixture.state.display_handle,
            1,
            smithay::wayland::GlobalData,
        )
        .unwrap();
    request(&mut wire, lock_manager.id().protocol_id(), 1, &[3], None);
    dispatch_test_client(&mut fixture.events, &mut fixture.state);
    assert!(
        fixture.state.session_lock.active(),
        "the offscreen harness must accept a real unrestricted lock request"
    );
    fixture.state.refresh_capture_sessions();
    dispatch_test_client(&mut fixture.events, &mut fixture.state);
    assert!(
        !session.alive(),
        "lock must revoke a paused capture even though no image was submitted"
    );
    assert!(
        messages(&mut wire).contains(&(2, 5)),
        "the paused consumer must receive the real session stopped event"
    );
    assert!(
        fixture
            .state
            .image_capture
            .active
            .iter()
            .all(|active| active.session.source() != source)
    );
}
