use super::*;
use smithay::backend::renderer::sync::{Fence, Interrupted};
use smithay::reexports::calloop::EventLoop;
use smithay::reexports::wayland_server::Display;
use std::cell::RefCell;
use std::io::Write;
use std::os::fd::OwnedFd;
use std::os::unix::net::UnixStream;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Debug)]
struct MockFence {
    ready: Arc<AtomicBool>,
    read: UnixStream,
    exportable: bool,
}
impl Fence for MockFence {
    fn is_signaled(&self) -> bool {
        self.ready.load(Ordering::Acquire)
    }
    fn wait(&self) -> Result<(), Interrupted> {
        panic!("the event loop must never wait for readback")
    }
    fn is_exportable(&self) -> bool {
        self.exportable
    }
    fn export(&self) -> Option<OwnedFd> {
        self.exportable
            .then(|| self.read.try_clone().unwrap().into())
    }
}

type Outcomes = Rc<RefCell<Vec<Result<(), String>>>>;
fn setup(name: &str) -> Option<(Ferese, EventLoop<'static, Ferese>)> {
    if !crate::startup_tests::private_runtime(&format!("handlers::readback::tests::{name}")) {
        return None;
    }
    let mut events = EventLoop::try_new().unwrap();
    let mut config = crate::config::Config::default().runtime_config().unwrap();
    config.wallpaper.path = None;
    let state = Ferese::try_new(&mut events, Display::new().unwrap(), config).unwrap();
    Some((state, events))
}
fn new_fence(exportable: bool) -> (SyncPoint, Arc<AtomicBool>, UnixStream) {
    let (read, write) = UnixStream::pair().unwrap();
    let ready = Arc::new(AtomicBool::new(false));
    (
        MockFence {
            ready: ready.clone(),
            read,
            exportable,
        }
        .into(),
        ready,
        write,
    )
}
fn callback(outcomes: &Outcomes) -> Completion {
    let outcomes = outcomes.clone();
    Box::new(move |_, result| outcomes.borrow_mut().push(result))
}

#[test]
fn native_readiness_defers_completion_and_never_waits_or_repeats() {
    let Some((mut state, mut events)) =
        setup("native_readiness_defers_completion_and_never_waits_or_repeats")
    else {
        return;
    };
    let (fence, ready, mut signal) = new_fence(true);
    let outcomes = Outcomes::default();
    state.wait_readback(
        Some(fence),
        4,
        Box::new(|_| true),
        callback(&outcomes),
        Instant::now() + READBACK_TIMEOUT,
    );
    events.dispatch(Duration::ZERO, &mut state).unwrap();
    assert!(outcomes.borrow().is_empty());
    assert_eq!(state.readbacks.pending.len(), 1);
    ready.store(true, Ordering::Release);
    signal.write_all(&[1]).unwrap();
    events
        .dispatch(Duration::from_millis(100), &mut state)
        .unwrap();
    assert_eq!(*outcomes.borrow(), [Ok(())]);
    assert!(state.readbacks.pending.is_empty());
    assert_eq!(state.readbacks.bytes, 0);
    events.dispatch(Duration::ZERO, &mut state).unwrap();
    assert_eq!(outcomes.borrow().len(), 1);
}

#[test]
fn cancellation_retires_native_sources_and_ignores_late_readiness() {
    let Some((mut state, mut events)) =
        setup("cancellation_retires_native_sources_and_ignores_late_readiness")
    else {
        return;
    };
    let (fence, ready, mut signal) = new_fence(true);
    let retained_fence = fence.clone();
    let outcomes = Outcomes::default();
    state.wait_readback(
        Some(fence),
        4,
        Box::new(|_| true),
        callback(&outcomes),
        Instant::now() + READBACK_TIMEOUT,
    );
    state.capture_epoch = state.capture_epoch.wrapping_add(1);
    state.prune_readbacks();
    assert!(
        outcomes.borrow()[0]
            .as_ref()
            .unwrap_err()
            .contains("cancelled")
    );
    ready.store(true, Ordering::Release);
    signal.write_all(&[1]).unwrap();
    events.dispatch(Duration::ZERO, &mut state).unwrap();
    assert_eq!(outcomes.borrow().len(), 1);
    assert!(state.readbacks.pending.is_empty());
    drop(retained_fence);
}

#[test]
fn readiness_after_absolute_deadline_and_fence_errors_cannot_publish() {
    let Some((mut state, mut events)) =
        setup("readiness_after_absolute_deadline_and_fence_errors_cannot_publish")
    else {
        return;
    };
    let (fence, ready, mut signal) = new_fence(true);
    let outcomes = Outcomes::default();
    state.wait_readback(
        Some(fence),
        4,
        Box::new(|_| true),
        callback(&outcomes),
        Instant::now(),
    );
    ready.store(true, Ordering::Release);
    signal.write_all(&[1]).unwrap();
    events
        .dispatch(Duration::from_millis(100), &mut state)
        .unwrap();
    assert!(
        outcomes.borrow()[0]
            .as_ref()
            .unwrap_err()
            .contains("timed out")
    );
    let (fence, _, mut signal) = new_fence(true);
    state.wait_readback(
        Some(fence),
        4,
        Box::new(|_| true),
        callback(&outcomes),
        Instant::now() + READBACK_TIMEOUT,
    );
    signal.write_all(&[1]).unwrap();
    events
        .dispatch(Duration::from_millis(100), &mut state)
        .unwrap();
    assert!(
        outcomes.borrow()[1]
            .as_ref()
            .unwrap_err()
            .contains("fence failed")
    );
    assert!(state.readbacks.pending.is_empty());
}

#[test]
fn nonexportable_fences_expire_and_source_guards_revoke_pending_work() {
    let Some((mut state, mut events)) =
        setup("nonexportable_fences_expire_and_source_guards_revoke_pending_work")
    else {
        return;
    };
    let (fence, _, _signal) = new_fence(false);
    let outcomes = Outcomes::default();
    state.wait_readback(
        Some(fence),
        4,
        Box::new(|_| true),
        callback(&outcomes),
        Instant::now() + Duration::from_millis(10),
    );
    for _ in 0..8 {
        if !outcomes.borrow().is_empty() {
            break;
        }
        events
            .dispatch(Duration::from_millis(20), &mut state)
            .unwrap();
    }
    assert!(
        outcomes.borrow()[0]
            .as_ref()
            .unwrap_err()
            .contains("timed out")
    );
    let valid = Arc::new(AtomicBool::new(true));
    let guard = valid.clone();
    let (fence, _, _signal) = new_fence(true);
    state.wait_readback(
        Some(fence),
        4,
        Box::new(move |_| guard.load(Ordering::Acquire)),
        callback(&outcomes),
        Instant::now() + READBACK_TIMEOUT,
    );
    valid.store(false, Ordering::Release);
    state.prune_readbacks();
    assert!(outcomes.borrow()[1].is_err());
    assert!(state.readbacks.pending.is_empty());
    assert_eq!(state.readbacks.bytes, 0);
}

#[test]
fn readback_count_and_memory_budgets_are_reclaimed_on_cancellation() {
    let Some((mut state, _events)) =
        setup("readback_count_and_memory_budgets_are_reclaimed_on_cancellation")
    else {
        return;
    };
    let outcomes = Outcomes::default();
    let (fence, _, _signal) = new_fence(true);
    for _ in 0..MAX_READBACKS + 1 {
        state.wait_readback(
            Some(fence.clone()),
            4,
            Box::new(|_| true),
            callback(&outcomes),
            Instant::now() + READBACK_TIMEOUT,
        );
    }
    assert_eq!(state.readbacks.pending.len(), MAX_READBACKS);
    assert_eq!(outcomes.borrow().len(), 1);
    state.capture_epoch += 1;
    state.prune_readbacks();
    assert_eq!(outcomes.borrow().len(), MAX_READBACKS + 1);
    assert_eq!(state.readbacks.bytes, 0);
    assert!(state.readbacks.can_admit(MAX_READBACK_BYTES));
    state.wait_readback(
        Some(fence),
        MAX_READBACK_BYTES,
        Box::new(|_| true),
        callback(&outcomes),
        Instant::now() + READBACK_TIMEOUT,
    );
    assert!(!state.readbacks.can_admit(1));
    state.capture_epoch += 1;
    state.prune_readbacks();
    assert!(state.readbacks.can_admit(MAX_READBACK_BYTES));
}

#[test]
#[ignore = "requires an EGL rendering device"]
fn real_pbo_mapping_rejects_another_context_and_preserves_pixels_after_readiness() {
    use smithay::backend::allocator::Fourcc;
    use smithay::backend::egl::{EGLContext, EGLDevice, EGLDisplay};
    use smithay::backend::renderer::ImportMem;
    use smithay::utils::Rectangle;
    let device = EGLDevice::enumerate().unwrap().last().expect("EGL device");
    // SAFETY: both contexts retain this enumerated device display.
    let display = unsafe { EGLDisplay::new(device).unwrap() };
    let a = EGLContext::new(&display).unwrap();
    let b = EGLContext::new(&display).unwrap();
    // SAFETY: this test exclusively owns both fresh renderer contexts.
    let mut owner = unsafe { GlesRenderer::new(a).unwrap() };
    // SAFETY: the second fresh context is also owned only by this test thread.
    let mut other = unsafe { GlesRenderer::new(b).unwrap() };
    let expected = [17, 35, 67, 255, 91, 109, 127, 255];
    let texture = owner
        .import_memory(&expected, Fourcc::Abgr8888, (2, 1).into(), false)
        .unwrap();
    let mapping = owner
        .copy_texture(
            &texture,
            Rectangle::from_size((2, 1).into()),
            Fourcc::Abgr8888,
        )
        .unwrap();
    let mut readback = Readback::try_new(&mut owner, mapping, expected.len()).unwrap();
    assert!(readback.map(&mut other).unwrap_err().contains("replaced"));
    let deadline = Instant::now() + Duration::from_secs(2);
    while readback
        .fence
        .as_ref()
        .is_some_and(|fence| !fence.is_reached())
    {
        assert!(Instant::now() < deadline, "GPU test fence timed out");
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(readback.map(&mut owner).unwrap(), expected);
    let (fence, ready, _signal) = new_fence(false);
    readback.fence = Some(fence);
    assert!(
        readback
            .map(&mut owner)
            .unwrap_err()
            .contains("not complete")
    );
    ready.store(true, Ordering::Release);
    assert_eq!(readback.map(&mut owner).unwrap(), expected);
}

#[test]
fn disconnected_screenshot_owner_revokes_queued_gpu_work() {
    let Some((mut state, mut events)) =
        setup("disconnected_screenshot_owner_revokes_queued_gpu_work")
    else {
        return;
    };
    let (reply, _receiver) = std::sync::mpsc::sync_channel(1);
    let request = state
        .screenshot
        .admit(
            99,
            7,
            reply,
            vec![crate::handlers::screenshot::PartSpec {
                preserve_alpha: false,
                premultiplied_rgba: false,
                transform: smithay::utils::Transform::Normal,
                location: (0, 0),
                scale: 1.,
                logical_width: 1,
                logical_height: 1,
                buffer_width: 1,
                buffer_height: 1,
            }],
        )
        .unwrap();
    let (fence, ready, mut signal) = new_fence(true);
    let retained = fence.clone();
    let outcomes = Outcomes::default();
    state.wait_readback(
        Some(fence),
        4,
        Box::new(move |state| state.screenshot.is_live(request)),
        callback(&outcomes),
        Instant::now() + READBACK_TIMEOUT,
    );
    assert_eq!(state.screenshot.terminate_owner(99), [request]);
    state.prune_readbacks();
    assert_eq!(outcomes.borrow().len(), 1);
    assert!(outcomes.borrow()[0].is_err());
    ready.store(true, Ordering::Release);
    signal.write_all(&[1]).unwrap();
    events.dispatch(Duration::ZERO, &mut state).unwrap();
    assert_eq!(outcomes.borrow().len(), 1);
    assert_eq!(state.readbacks.bytes, 0);
    drop(retained);
}

#[test]
fn cancelled_hung_fence_quarantines_its_context_before_another_batch() {
    let Some((mut state, _events)) =
        setup("cancelled_hung_fence_quarantines_its_context_before_another_batch")
    else {
        return;
    };
    let context = smithay::backend::renderer::ContextId::<
        smithay::backend::renderer::gles::GlesTexture,
    >::new()
    .erased();
    let other = smithay::backend::renderer::ContextId::<
        smithay::backend::renderer::gles::GlesTexture,
    >::new()
    .erased();
    let (fence, ready, _signal) = new_fence(true);
    let retained = fence.clone();
    let identity = context.clone();
    let outcomes = Outcomes::default();
    let observed = outcomes.clone();
    state.wait_readback(
        Some(fence),
        4,
        Box::new(|_| true),
        Box::new(move |state, result| {
            // GPU-backed jobs invoke this same quarantine operation before dropping
            // their PBO and failing the caller. Keep admission charged by context.
            if result.is_err() {
                state.readbacks.quarantine(identity, retained);
            }
            observed.borrow_mut().push(result);
        }),
        Instant::now() + READBACK_TIMEOUT,
    );
    let id = *state.readbacks.pending.keys().next().unwrap();
    state.finish_readback(id, Trigger::Cancel, Err("caller disconnected".into()));
    assert_eq!(outcomes.borrow().len(), 1);
    assert_eq!(state.readbacks.bytes, 0);
    for _ in 0..MAX_READBACKS * 4 {
        assert!(
            !state
                .readbacks
                .can_admit_in_context(MAX_READBACK_BYTES, &context)
        );
    }
    assert_eq!(state.readbacks.quarantined.len(), 1);
    assert!(
        state
            .readbacks
            .can_admit_in_context(MAX_READBACK_BYTES, &other)
    );
    ready.store(true, Ordering::Release);
    assert!(
        state
            .readbacks
            .can_admit_in_context(MAX_READBACK_BYTES, &context)
    );
    state.prune_readbacks();
    assert!(state.readbacks.quarantined.is_empty());
}
