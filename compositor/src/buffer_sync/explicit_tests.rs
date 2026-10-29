use super::*;
use std::io::Read;
use std::os::unix::net::UnixStream;
use std::sync::atomic::AtomicBool;

use smithay::backend::drm::DrmDeviceFd;
use smithay::backend::renderer::sync::{Fence, Interrupted, SyncPoint};
use smithay::backend::renderer::utils::with_renderer_surface_state;
use smithay::reexports::wayland_protocols::wp::linux_drm_syncobj::v1::server::wp_linux_drm_syncobj_manager_v1::WpLinuxDrmSyncobjManagerV1;
use smithay::utils::DeviceFd;
use smithay::wayland::GlobalData;
use smithay::wayland::drm_syncobj::{DrmSyncobjState, DrmTimeline};

fn messages(wire: &mut UnixStream) -> Vec<(u32, u16, Vec<u32>)> {
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
        result.push((
            object,
            header as u16,
            bytes[offset + 8..offset + size]
                .chunks_exact(4)
                .map(|word| u32::from_ne_bytes(word.try_into().unwrap()))
                .collect(),
        ));
        offset += size;
    }
    result
}

fn manager(state: &mut Ferese, surface: &WlSurface) -> WpLinuxDrmSyncobjManagerV1 {
    // Validation of missing points performs no timeline ioctls. This descriptor
    // lets a private test exercise real dispatch on machines without DRM.
    let fd = std::fs::File::open("/dev/null").unwrap();
    state.buffer_sync.explicit.state = Some(DrmSyncobjState::new::<Ferese>(
        &state.display_handle,
        DrmDeviceFd::new(DeviceFd::from(std::os::fd::OwnedFd::from(fd))),
    ));
    surface
        .client()
        .unwrap()
        .create_resource::<WpLinuxDrmSyncobjManagerV1, GlobalData, Ferese>(
            &state.display_handle,
            1,
            GlobalData,
        )
        .unwrap()
}

#[test]
fn explicit_attachment_requires_points_even_when_both_are_missing() {
    if !crate::startup_tests::private_runtime(
        "buffer_sync::explicit_tests::explicit_attachment_requires_points_even_when_both_are_missing",
    ) {
        return;
    }
    let (mut state, mut events, surface, mut wire) = super::tests::fixture();
    let manager = manager(&mut state, &surface);
    messages(&mut wire);
    crate::state::test_client_request(&mut wire, manager.id().protocol_id(), 1, &[7, 2], None);
    crate::state::test_client_request(&mut wire, 2, 1, &[6, 0, 0], None);
    crate::state::test_client_request(&mut wire, 2, 6, &[], None);
    super::tests::dispatch(&mut state, &mut events);
    assert!(
        messages(&mut wire)
            .iter()
            .any(|(object, opcode, args)| *object == 1 && *opcode == 0 && args[..2] == [7, 4])
    );
}

#[test]
fn destroying_controller_restores_ordinary_attachments() {
    if !crate::startup_tests::private_runtime(
        "buffer_sync::explicit_tests::destroying_controller_restores_ordinary_attachments",
    ) {
        return;
    }
    let (mut state, mut events, surface, mut wire) = super::tests::fixture();
    let manager = manager(&mut state, &surface);
    let before = with_renderer_surface_state(&surface, |data| data.current_commit()).unwrap();
    crate::state::test_client_request(&mut wire, manager.id().protocol_id(), 1, &[7, 2], None);
    crate::state::test_client_request(&mut wire, 7, 0, &[], None);
    crate::state::test_client_request(&mut wire, 2, 1, &[6, 0, 0], None);
    crate::state::test_client_request(&mut wire, 2, 2, &[0, 0, 64, 48], None);
    crate::state::test_client_request(&mut wire, 2, 6, &[], None);
    super::tests::dispatch(&mut state, &mut events);
    assert_ne!(
        before,
        with_renderer_surface_state(&surface, |data| data.current_commit()).unwrap()
    );
    assert!(surface.is_alive());
}

#[derive(Debug)]
struct FenceState(Arc<AtomicBool>);
impl Fence for FenceState {
    fn is_signaled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
    fn wait(&self) -> Result<(), Interrupted> {
        panic!("retirement must never block the compositor")
    }
    fn is_exportable(&self) -> bool {
        false
    }
    fn export(&self) -> Option<std::os::fd::OwnedFd> {
        None
    }
}

#[test]
fn retired_attachment_waits_for_every_gpu_and_releases_without_scene_work() {
    if !crate::startup_tests::private_runtime(
        "buffer_sync::explicit_tests::retired_attachment_waits_for_every_gpu_and_releases_without_scene_work",
    ) {
        return;
    }
    let (mut state, mut events, surface, mut wire) = super::tests::fixture();
    let buffer = with_renderer_surface_state(&surface, |data| data.buffer().cloned())
        .flatten()
        .unwrap();
    let first = Arc::new(AtomicBool::new(false));
    let second = Arc::new(AtomicBool::new(false));
    state.buffer_sync.explicit.retired.push(gpu::RetiredBuffer {
        _buffer: buffer,
        completions: vec![
            SyncPoint::from(FenceState(first.clone())),
            SyncPoint::from(FenceState(second.clone())),
        ],
    });
    messages(&mut wire);
    crate::state::test_client_request(&mut wire, 2, 1, &[0, 0, 0], None);
    crate::state::test_client_request(&mut wire, 2, 6, &[], None);
    super::tests::dispatch(&mut state, &mut events);
    assert_eq!(state.buffer_sync.explicit.retired.len(), 1);
    assert!(state.buffer_sync.explicit.timer.is_some());
    assert!(
        !messages(&mut wire)
            .iter()
            .any(|(object, opcode, _)| *object == 6 && *opcode == 0)
    );
    first.store(true, Ordering::Release);
    state.poll_buffer_sync();
    assert_eq!(state.buffer_sync.explicit.retired.len(), 1);
    second.store(true, Ordering::Release);
    state.poll_buffer_sync();
    state.display_handle.flush_clients().unwrap();
    assert!(state.buffer_sync.explicit.retired.is_empty());
    assert!(state.buffer_sync.explicit.timer.is_none());
    assert_eq!(
        messages(&mut wire)
            .iter()
            .filter(|(object, opcode, _)| *object == 6 && *opcode == 0)
            .count(),
        1
    );
}

#[test]
#[ignore = "requires a render node supporting DRM timeline eventfd; no modesets or live session changes"]
fn ready_acquire_retains_unused_release_until_its_delayed_transaction_is_retired() {
    if !crate::startup_tests::private_runtime(
        "buffer_sync::explicit_tests::ready_acquire_retains_unused_release_until_its_delayed_transaction_is_retired",
    ) {
        return;
    }
    use smithay::reexports::drm::control::Device;
    use smithay::wayland::compositor::{Barrier, add_blocker};
    let path =
        std::env::var("FERESE_TEST_RENDER_NODE").unwrap_or_else(|_| "/dev/dri/renderD128".into());
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .unwrap();
    let fd = DrmDeviceFd::new(DeviceFd::from(std::os::fd::OwnedFd::from(file)));
    assert!(smithay::wayland::drm_syncobj::supports_syncobj_eventfd(&fd));
    let handle = fd.create_syncobj(false).unwrap();
    let timeline = DrmTimeline::new(&fd, fd.syncobj_to_fd(handle, false).unwrap()).unwrap();
    let acquire = timeline.point(1);
    let release = timeline.point(2);
    let (mut state, mut events, surface, mut wire) = super::tests::fixture();
    assert!(state.admit_explicit_surface(&surface));
    let lease = state.new_release_lease(&surface, release.clone());
    let (_, source) = acquire.generate_blocker().unwrap();
    state.wait_for_buffer(&surface, source, None, Some(lease), MAX_WAIT);
    add_blocker(&surface, Barrier::new(false));
    crate::state::test_client_request(&mut wire, 2, 1, &[6, 0, 0], None);
    crate::state::test_client_request(&mut wire, 2, 6, &[], None);
    super::tests::dispatch(&mut state, &mut events);
    assert!(state.buffer_sync.pending.len() == 1);
    fd.syncobj_timeline_signal(&[handle], &[1]).unwrap();
    for _ in 0..3 {
        super::tests::dispatch(&mut state, &mut events);
    }
    assert!(state.buffer_sync.pending.is_empty());
    assert_eq!(timeline.query_signalled_point().unwrap(), 1);
    crate::state::test_client_request(&mut wire, 2, 0, &[], None);
    for _ in 0..3 {
        super::tests::dispatch(&mut state, &mut events);
    }
    assert!(timeline.query_signalled_point().unwrap() >= 2);
    fd.destroy_syncobj(handle).unwrap();
}

#[test]
#[ignore = "requires DRM timeline eventfd; exercises unused synchronized child destruction without modesetting"]
fn destroyed_synchronized_children_release_points_and_reclaim_budgets_before_parent_commit() {
    if !crate::startup_tests::private_runtime(
        "buffer_sync::explicit_tests::destroyed_synchronized_children_release_points_and_reclaim_budgets_before_parent_commit",
    ) {
        return;
    }
    use smithay::reexports::drm::control::Device;
    use smithay::reexports::wayland_server::protocol::wl_compositor::WlCompositor;
    use smithay::reexports::wayland_server::protocol::wl_subcompositor::WlSubcompositor;
    let path =
        std::env::var("FERESE_TEST_RENDER_NODE").unwrap_or_else(|_| "/dev/dri/renderD128".into());
    let fd = DrmDeviceFd::new(DeviceFd::from(std::os::fd::OwnedFd::from(
        std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .unwrap(),
    )));
    assert!(smithay::wayland::drm_syncobj::supports_syncobj_eventfd(&fd));
    let handle = fd.create_syncobj(false).unwrap();
    let timeline = DrmTimeline::new(&fd, fd.syncobj_to_fd(handle, false).unwrap()).unwrap();
    let (mut state, mut events, root, mut wire) = super::tests::fixture();
    let client = root.client().unwrap();
    let compositor = client
        .create_resource::<WlCompositor, GlobalData, Ferese>(&state.display_handle, 6, GlobalData)
        .unwrap();
    let subcompositor = client
        .create_resource::<WlSubcompositor, GlobalData, Ferese>(
            &state.display_handle,
            1,
            GlobalData,
        )
        .unwrap();
    // Keep each wl_subsurface alive and never commit the parent. More cycles
    // than the per-client admission cap must still be legal after destruction.
    for index in 0..MAX_PER_CLIENT + 2 {
        let child_id = 7 + 2 * index as u32;
        let sub_id = child_id + 1;
        crate::state::test_client_request(
            &mut wire,
            compositor.id().protocol_id(),
            0,
            &[child_id],
            None,
        );
        crate::state::test_client_request(
            &mut wire,
            subcompositor.id().protocol_id(),
            1,
            &[sub_id, child_id, 2],
            None,
        );
        super::tests::dispatch(&mut state, &mut events);
        let child = client
            .object_from_protocol_id::<WlSurface>(&state.display_handle, child_id)
            .unwrap();
        assert!(state.admit_explicit_surface(&child));
        let acquire = timeline.point(1 + 2 * index as u64);
        let release = timeline.point(2 + 2 * index as u64);
        let lease = state.new_release_lease(&child, release.clone());
        let (_, source) = acquire.generate_blocker().unwrap();
        state.wait_for_buffer(&child, source, None, Some(lease), MAX_WAIT);
        crate::state::test_client_request(&mut wire, child_id, 6, &[], None);
        super::tests::dispatch(&mut state, &mut events);
        acquire.signal().unwrap();
        for _ in 0..3 {
            super::tests::dispatch(&mut state, &mut events);
        }
        assert!(state.buffer_sync.pending.is_empty());
        assert_eq!(timeline.query_signalled_point().unwrap(), acquire.point());
        crate::state::test_client_request(&mut wire, child_id, 0, &[], None);
        for _ in 0..3 {
            super::tests::dispatch(&mut state, &mut events);
        }
        assert!(timeline.query_signalled_point().unwrap() >= release.point());
        assert!(root.is_alive());
    }
    fd.destroy_syncobj(handle).unwrap();
}

#[test]
#[ignore = "requires an EGL device; software Mesa supported"]
fn renderer_completion_covers_dropped_offscreen_frames() {
    use smithay::backend::allocator::Fourcc;
    use smithay::backend::egl::{EGLContext, EGLDevice, EGLDisplay};
    use smithay::backend::renderer::gles::{GlesRenderer, GlesTexture};
    use smithay::backend::renderer::{Bind, Color32F, ExportMem, Frame, Offscreen, Renderer};
    use smithay::utils::{Physical, Rectangle, Transform};
    let device = EGLDevice::enumerate().unwrap().last().unwrap();
    // SAFETY: the display retains its live enumerated native device.
    let display = unsafe { EGLDisplay::new(device).unwrap() };
    let context = EGLContext::new(&display).unwrap();
    // SAFETY: this fresh context is owned only by the current test thread.
    let mut renderer = unsafe { GlesRenderer::new(context).unwrap() };
    let completion = renderer.completion();
    assert!(completion.snapshot().is_reached());
    let mut texture: GlesTexture =
        Offscreen::create_buffer(&mut renderer, Fourcc::Abgr8888, (8, 8).into()).unwrap();
    {
        let mut target = renderer.bind(&mut texture).unwrap();
        let mut frame = renderer
            .render(&mut target, (8, 8).into(), Transform::Normal)
            .unwrap();
        frame
            .clear(
                Color32F::new(1., 0., 0., 1.),
                &[Rectangle::<i32, Physical>::from_size((8, 8).into())],
            )
            .unwrap();
        drop(frame);
    }
    completion.snapshot().wait().unwrap();
    let mapping = renderer
        .copy_texture(
            &texture,
            Rectangle::from_size((8, 8).into()),
            Fourcc::Abgr8888,
        )
        .unwrap();
    let pixels = renderer.map_texture(&mapping).unwrap();
    assert_eq!(&pixels[..4], &[255, 0, 0, 255]);
}

#[test]
#[ignore = "requires DRM timelines; isolates renderer attachment ownership without a scanout or modeset"]
fn ready_acquires_and_reused_storage_transfer_each_attachment_release_once() {
    if !crate::startup_tests::private_runtime(
        "buffer_sync::explicit_tests::ready_acquires_and_reused_storage_transfer_each_attachment_release_once",
    ) {
        return;
    }
    use smithay::reexports::drm::control::Device;
    let path =
        std::env::var("FERESE_TEST_RENDER_NODE").unwrap_or_else(|_| "/dev/dri/renderD128".into());
    let fd = DrmDeviceFd::new(DeviceFd::from(std::os::fd::OwnedFd::from(
        std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .unwrap(),
    )));
    let handles: Vec<_> = (0..3).map(|_| fd.create_syncobj(false).unwrap()).collect();
    let timelines: Vec<_> = handles
        .iter()
        .map(|handle| DrmTimeline::new(&fd, fd.syncobj_to_fd(*handle, false).unwrap()).unwrap())
        .collect();
    let acquire = timelines[0].point(1);
    acquire.signal().unwrap();
    let (mut state, mut events, surface, mut wire) = super::tests::fixture();
    // This helper-level fixture supplies cached points directly. The real wire
    // validator separately rejects SHM+explicit; no DMA-BUF import is simulated.
    let release = timelines[1].point(1);
    with_states(&surface, |states| {
        let mut cached = states.cached_state.get::<DrmSyncobjCachedState>();
        cached.pending().acquire_point = Some(acquire.clone());
        cached.pending().release_point = Some(release.clone());
    });
    state.prepare_explicit_buffer(&surface, None, acquire.clone(), release, MAX_WAIT);
    assert!(
        state.buffer_sync.pending.is_empty(),
        "ready points must not allocate acquire watchers/timers"
    );
    crate::state::test_client_request(&mut wire, 2, 1, &[6, 0, 0], None);
    crate::state::test_client_request(&mut wire, 2, 2, &[0, 0, 64, 48], None);
    crate::state::test_client_request(&mut wire, 2, 6, &[], None);
    super::tests::dispatch(&mut state, &mut events);
    assert!(
        with_renderer_surface_state(&surface, |data| data.buffer().unwrap().has_explicit_sync())
            .unwrap()
    );
    assert!(with_states(&surface, |states| states
        .cached_state
        .get::<DrmSyncobjCachedState>()
        .current()
        .release_point
        .is_none()));
    assert_eq!(timelines[1].query_signalled_point().unwrap(), 0);
    let first = with_renderer_surface_state(&surface, |data| data.buffer().cloned())
        .flatten()
        .unwrap();
    // Keep first attachment through the second update and prove that
    // its release is independent from the new attachment's release.
    with_states(&surface, |states| {
        let mut cached = states.cached_state.get::<DrmSyncobjCachedState>();
        cached.pending().acquire_point = Some(acquire.clone());
        cached.pending().release_point = Some(timelines[2].point(1));
    });
    state.prepare_explicit_buffer(
        &surface,
        None,
        acquire.clone(),
        timelines[2].point(1),
        MAX_WAIT,
    );
    crate::state::test_client_request(&mut wire, 2, 1, &[6, 0, 0], None);
    crate::state::test_client_request(&mut wire, 2, 6, &[], None);
    super::tests::dispatch(&mut state, &mut events);
    assert_eq!(timelines[1].query_signalled_point().unwrap(), 0);
    drop(first);
    assert_eq!(timelines[1].query_signalled_point().unwrap(), 1);
    assert_eq!(timelines[2].query_signalled_point().unwrap(), 0);
    assert!(with_states(&surface, |states| states
        .cached_state
        .get::<DrmSyncobjCachedState>()
        .current()
        .release_point
        .is_none()));
    crate::state::test_client_request(&mut wire, 2, 1, &[0, 0, 0], None);
    crate::state::test_client_request(&mut wire, 2, 6, &[], None);
    super::tests::dispatch(&mut state, &mut events);
    assert_eq!(timelines[2].query_signalled_point().unwrap(), 1);
    for handle in handles {
        fd.destroy_syncobj(handle).unwrap();
    }
}
