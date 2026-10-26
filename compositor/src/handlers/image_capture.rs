//! Persistent, damage-driven capture for the native portal. One frame may be
//! outstanding per session; client buffers remain owned until GPU completion.
use smithay::reexports::calloop::RegistrationToken;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use ferese_layout::WindowId;
use smithay::backend::allocator::{Fourcc, Modifier};
use smithay::backend::egl::EGLDevice;
use smithay::backend::renderer::gles::{Capability, GlesRenderer, GlesTexture};
use smithay::backend::renderer::Texture;
use smithay::backend::renderer::utils::{CommitCounter, RendererSurfaceStateUserData, SurfaceView};
use smithay::output::{Output, WeakOutput};
use smithay::reexports::wayland_protocols::ext::image_copy_capture::v1::server::ext_image_copy_capture_frame_v1::FailureReason;
use smithay::reexports::wayland_server::backend::ObjectId;
use smithay::reexports::wayland_server::{Client, DisplayHandle, Resource};
use smithay::reexports::wayland_server::protocol::wl_shm;
use smithay::utils::{Buffer, IsAlive, Logical, Point, Rectangle, Size, Transform};
use smithay::wayland::compositor::{TraversalAction, with_surface_tree_downward};
use smithay::wayland::image_capture_source::{ImageCaptureSource, ImageCaptureSourceHandler, OutputCaptureSourceHandler, OutputCaptureSourceState};
use smithay::wayland::image_copy_capture::{BufferConstraints, DmabufConstraints, Frame, FrameRef, ImageCopyCaptureHandler, ImageCopyCaptureState, Session, SessionRef};

use crate::Ferese;

const MAX_SESSIONS: usize = 16;
// Reserve for the maximum eight portal pool buffers, a retained target and a
// fallback transfer texture. Imports of the same allocation are not extra pixels.
const MAX_STREAM_BYTES: u64 = 512 * 1024 * 1024;
fn stream_bytes(size: Size<i32, Buffer>) -> u64 {
    // The portal rejects DMA strides above this allowance. Driver bookkeeping
    // and encoder memory are separate from this pixel-storage reservation.
    let stride = (size.w.max(0) as u64 * 4 + 4095) & !4095;
    stride
        .saturating_mul(size.h.max(0) as u64)
        .saturating_mul(10)
}

pub(super) enum Source {
    Output(WeakOutput),
    Window(WindowId),
    Workspace(super::workspace_capture::WorkspaceSource),
}

#[derive(Default)]
pub(super) struct SourcePaused(pub std::sync::atomic::AtomicBool);

fn source_active(source: &ImageCaptureSource) -> bool {
    source
        .user_data()
        .get::<SourcePaused>()
        .is_none_or(|paused| !paused.0.load(std::sync::atomic::Ordering::Relaxed))
}

#[derive(PartialEq)]
struct SurfaceStamp {
    id: ObjectId,
    commit: CommitCounter,
    view: SurfaceView,
    scale: i32,
    transform: Transform,
    alpha: Option<f32>,
}

#[derive(PartialEq)]
struct WindowStamp {
    surfaces: Vec<SurfaceStamp>,
    geometry: Rectangle<i32, Logical>,
    scale: f64,
    epoch: u64,
    cursor: Option<CursorStamp>,
}

#[derive(PartialEq)]
pub(super) struct CursorStamp {
    location: Point<f64, Logical>,
    geometry: Rectangle<i32, Logical>,
    image: CursorImageStamp,
}

#[derive(PartialEq)]
enum CursorImageStamp {
    Named(Option<smithay::input::pointer::CursorIcon>, Instant, usize),
    Surface(Vec<SurfaceStamp>, Point<i32, Logical>),
    Hidden,
}

fn surface_stamp(
    root: &smithay::reexports::wayland_server::protocol::wl_surface::WlSurface,
    crop: Option<Rectangle<i32, Logical>>,
) -> Vec<SurfaceStamp> {
    let mut stamp = Vec::new();
    with_surface_tree_downward(
        root,
        Point::<f64, Logical>::default(),
        |surface, states, location| {
            let Some(data) = states.data_map.get::<RendererSurfaceStateUserData>() else {
                return TraversalAction::SkipChildren;
            };
            use smithay::wayland::alpha_modifier::AlphaModifierSurfaceCachedState;
            let alpha = states
                .cached_state
                .get::<AlphaModifierSurfaceCachedState>()
                .current()
                .multiplier_f32();
            let data = data.lock().expect("surface metadata mutex poisoned");
            let Some(view) = data.view() else {
                return TraversalAction::SkipChildren;
            };
            let location = *location + view.offset.to_f64();
            if crop.is_none_or(|crop| {
                Rectangle::new(location, view.dst.to_f64()).overlaps(crop.to_f64())
            }) {
                stamp.push(SurfaceStamp {
                    id: surface.id(),
                    commit: data.current_commit(),
                    view,
                    scale: data.buffer_scale(),
                    transform: data.buffer_transform(),
                    alpha,
                });
            }
            // An off-crop parent can have a child offset back into the captured area.
            TraversalAction::DoChildren(location)
        },
        |_, _, _| {},
        |_, _, _| true,
    );
    stamp
}

fn pending_frame_callbacks(
    root: &smithay::reexports::wayland_server::protocol::wl_surface::WlSurface,
) -> bool {
    let mut pending = false;
    with_surface_tree_downward(
        root,
        (),
        |_, states, _| {
            if states
                .data_map
                .get::<RendererSurfaceStateUserData>()
                .is_some_and(|data| {
                    data.lock()
                        .expect("surface metadata mutex poisoned")
                        .view()
                        .is_none()
                })
            {
                return TraversalAction::SkipChildren;
            }
            pending |= !states
                .cached_state
                .get::<smithay::wayland::compositor::SurfaceAttributes>()
                .current()
                .frame_callbacks
                .is_empty();
            TraversalAction::DoChildren(())
        },
        |_, _, _| {},
        |_, _, _| true,
    );
    pending
}

pub(super) struct Active {
    pub(super) session: Session,
    pub(super) pending: Option<Frame>,
    pub(super) revision: Option<u64>,
    window_stamp: Option<WindowStamp>,
    size: Size<i32, Buffer>,
    dma_node: Option<smithay::backend::drm::DrmNode>,
}

#[derive(Default)]
struct SubmissionGeneration(AtomicU64);

fn next_submission(session: &SessionRef) -> u64 {
    session
        .user_data()
        .insert_if_missing(SubmissionGeneration::default);
    session
        .user_data()
        .get::<SubmissionGeneration>()
        .expect("submission generation initialized")
        .0
        .fetch_add(1, Ordering::Relaxed)
        .wrapping_add(1)
}

fn current_submission(session: &SessionRef, generation: u64) -> bool {
    session
        .user_data()
        .get::<SubmissionGeneration>()
        .is_some_and(|current| current.0.load(Ordering::Relaxed) == generation)
}

struct InFlight {
    frame: Frame,
    session: SessionRef,
    generation: u64,
    epoch: u64,
    fence: RegistrationToken,
    timeout: RegistrationToken,
    completion: smithay::backend::renderer::sync::SyncPoint,
    deadline: Instant,
    stamp: CompletionStamp,
}

#[derive(PartialEq)]
struct CompletionStamp {
    size: Size<i32, Buffer>,
    output: Output,
    mode: Option<smithay::output::Mode>,
    transform: Transform,
    scale: f64,
}

pub(crate) struct CaptureSessions {
    copy: ImageCopyCaptureState,
    outputs: OutputCaptureSourceState,
    active: Vec<Active>,
    in_flight: HashMap<u64, InFlight>,
    next_fence: u64,
    callback_timer: Option<RegistrationToken>,
    callback_deadline: Option<Instant>,
    workspace_callbacks: HashMap<ferese_core::WorkspaceId, Instant>,
    workspaces:
        HashMap<(ferese_core::WorkspaceId, bool, bool), super::workspace_capture::WorkspaceCapture>,
}

fn authorized(client: &Client) -> bool {
    super::screencopy::capture_allowed()
        && client
            .get_data::<crate::state::ClientState>()
            .is_some_and(|state| {
                state
                    .capabilities
                    .contains(crate::private_client::ClientCapabilities::WINDOW_CAPTURE)
            })
}

impl CaptureSessions {
    pub(super) fn workspace_callback_deadline(
        &self,
        id: ferese_core::WorkspaceId,
        now: Instant,
    ) -> Instant {
        self.workspace_callbacks.get(&id).copied().unwrap_or(now)
    }
    pub(super) fn set_workspace_callback_deadline(
        &mut self,
        id: ferese_core::WorkspaceId,
        deadline: Instant,
    ) {
        self.workspace_callbacks.insert(id, deadline);
    }
    pub(crate) fn invalidate_workspaces(&mut self) {
        for view in self.workspaces.values_mut() {
            view.invalidate();
        }
    }
    pub(crate) fn workspace_changed(&mut self, id: ferese_core::WorkspaceId) {
        for ((workspace, _, _), view) in &mut self.workspaces {
            if *workspace == id {
                view.content_changed();
            }
        }
    }
    pub(crate) fn workspace_geometry_changed(&mut self, id: ferese_core::WorkspaceId) {
        for ((workspace, _, _), view) in &mut self.workspaces {
            if *workspace == id {
                view.invalidate();
            }
        }
    }
    pub(crate) fn captured_workspaces(&self) -> Vec<ferese_core::WorkspaceId> {
        self.workspaces.keys().map(|(id, _, _)| *id).collect()
    }

    pub(crate) fn new(display: &DisplayHandle) -> Self {
        Self {
            copy: ImageCopyCaptureState::new_with_filter::<Ferese, _>(display, authorized),
            outputs: OutputCaptureSourceState::new_with_filter::<Ferese, _>(display, authorized),
            active: Vec::new(),
            in_flight: HashMap::new(),
            next_fence: 0,
            callback_timer: None,
            callback_deadline: None,
            workspace_callbacks: HashMap::new(),
            workspaces: HashMap::new(),
        }
    }

    pub(crate) fn has_sessions(&self) -> bool {
        self.active.iter().any(|active| {
            active.pending.is_some()
                && matches!(
                    active.session.source().user_data().get::<Source>(),
                    Some(Source::Output(_))
                )
        })
    }

    pub(super) fn pending_output(&self, output: &Output, cursor: bool) -> bool {
        self.active.iter().any(|active| {
            active.pending.is_some()
                && active.session.draw_cursor() == cursor
                && matches!(active.session.source().user_data().get::<Source>(),
                    Some(Source::Output(weak)) if weak.upgrade().as_ref() == Some(output))
        })
    }
}

pub(super) fn dma_constraints(renderer: &GlesRenderer) -> Option<Arc<DmabufConstraints>> {
    struct Cached(Option<Arc<DmabufConstraints>>);
    renderer
        .egl_context()
        .user_data()
        .get_or_insert_threadsafe(|| Cached(query_dma_constraints(renderer).map(Arc::new)))
        .0
        .clone()
}

fn query_dma_constraints(renderer: &GlesRenderer) -> Option<DmabufConstraints> {
    if !renderer.capabilities().contains(&Capability::ExportFence) {
        return None;
    }
    let display = renderer.egl_context().display();
    if !display
        .extensions()
        .iter()
        .any(|extension| extension == "EGL_ANDROID_native_fence_sync")
    {
        return None;
    }
    let node = EGLDevice::device_for_display(display)
        .ok()?
        .try_get_render_node()
        .ok()??;
    let format = smithay::backend::allocator::Format {
        code: Fourcc::Xrgb8888,
        modifier: Modifier::Linear,
    };
    display
        .dmabuf_render_formats()
        .contains(&format)
        .then_some(DmabufConstraints {
            node,
            formats: vec![(Fourcc::Xrgb8888, vec![Modifier::Linear])],
        })
}

impl ImageCaptureSourceHandler for Ferese {}
impl OutputCaptureSourceHandler for Ferese {
    fn output_capture_source_state(&mut self) -> &mut OutputCaptureSourceState {
        &mut self.image_capture.outputs
    }
    fn output_source_created(&mut self, source: ImageCaptureSource, output: &Output) {
        source
            .user_data()
            .insert_if_missing(|| Source::Output(output.downgrade()));
    }
}

impl ImageCopyCaptureHandler for Ferese {
    fn image_copy_capture_state(&mut self) -> &mut ImageCopyCaptureState {
        &mut self.image_capture.copy
    }
    fn capture_constraints(&mut self, source: &ImageCaptureSource) -> Option<BufferConstraints> {
        let (size, output) = self.capture_source_geometry(source)?;
        Some(BufferConstraints {
            size,
            shm: vec![wl_shm::Format::Xrgb8888],
            dma: self.capture_dma_constraints(&output).as_deref().cloned(),
        })
    }
    fn new_session(&mut self, session: Session) {
        if self.image_capture.active.len() >= MAX_SESSIONS {
            return;
        }
        let Some(constraints) = session.current_constraints() else {
            return;
        };
        let reserved: u64 = self
            .image_capture
            .active
            .iter()
            .map(|active| stream_bytes(active.size))
            .sum();
        if stream_bytes(constraints.size) > MAX_STREAM_BYTES.saturating_sub(reserved) {
            return;
        }
        self.image_capture.active.push(Active {
            session,
            pending: None,
            revision: None,
            window_stamp: None,
            size: constraints.size,
            dma_node: constraints.dma.as_ref().map(|dma| dma.node),
        });
    }
    fn frame(&mut self, session: &SessionRef, frame: Frame) {
        let Some(active) = self
            .image_capture
            .active
            .iter_mut()
            .find(|active| active.session.as_ref() == *session)
        else {
            return;
        };
        if active.pending.is_some() {
            frame.fail(FailureReason::Unknown);
            return;
        }
        active.pending = Some(frame);
        if let Some(Source::Output(weak)) = session.source().user_data().get::<Source>()
            && let Some(output) = weak.upgrade()
        {
            crate::backends::direct::schedule_capture(self, &[output]);
        }
    }

    fn session_destroyed(&mut self, session: SessionRef) {
        self.image_capture
            .active
            .retain(|active| active.session.as_ref() != session);
        let fences: Vec<_> = self
            .image_capture
            .in_flight
            .iter()
            .filter_map(|(id, pending)| (pending.session == session).then_some(*id))
            .collect();
        for id in fences {
            self.cancel_capture_fence(id);
        }
    }
    fn frame_aborted(&mut self, frame: FrameRef) {
        let fences: Vec<_> = self
            .image_capture
            .in_flight
            .iter()
            .filter_map(|(id, pending)| (pending.frame == frame).then_some(*id))
            .collect();
        for id in fences {
            self.cancel_capture_fence(id);
        }
        for active in &mut self.image_capture.active {
            if active
                .pending
                .as_ref()
                .is_some_and(|pending| *pending == frame)
            {
                active.pending.take();
            }
        }
    }
}

impl Ferese {
    pub(super) fn capture_cursor_stamp(
        &self,
        geometry: Rectangle<i32, Logical>,
    ) -> Option<CursorStamp> {
        use smithay::input::pointer::CursorImageStatus;
        let pointer = self.seat.get_pointer()?;
        let image = match &self.cursor_status {
            CursorImageStatus::Named(_) => {
                let (icon, epoch, frame) = self.cursor_animation.capture_revision();
                CursorImageStamp::Named(icon, epoch, frame)
            }
            CursorImageStatus::Surface(surface) => {
                let hotspot = smithay::wayland::compositor::with_states(surface, |states| {
                    states
                        .data_map
                        .get::<smithay::input::pointer::CursorImageSurfaceData>()
                        .map(|data| data.lock().expect("cursor metadata mutex poisoned").hotspot)
                        .unwrap_or_default()
                });
                CursorImageStamp::Surface(surface_stamp(surface, None), hotspot)
            }
            CursorImageStatus::Hidden => CursorImageStamp::Hidden,
        };
        Some(CursorStamp {
            location: pointer.current_location(),
            geometry,
            image,
        })
    }

    fn capture_source_geometry(
        &self,
        source: &ImageCaptureSource,
    ) -> Option<(Size<i32, Buffer>, Output)> {
        if self.session_lock.active() || !source.alive() || !super::screencopy::capture_allowed() {
            return None;
        }
        let (size, output) = match source.user_data().get::<Source>()? {
            Source::Workspace(view) => {
                if !view.valid(self) {
                    return None;
                }
                (view.size, view.output.clone())
            }
            Source::Window(id) => {
                let info = self.window_capture_info(*id)?;
                let size = info.geometry.size.to_physical_precise_round(info.scale);
                (Size::from((size.w, size.h)), info.output)
            }
            Source::Output(weak) => {
                let output = weak.upgrade()?;
                if !self.output_ids.contains_key(&output) {
                    return None;
                }
                let size = output
                    .current_transform()
                    .transform_size(output.current_mode()?.size);
                (Size::from((size.w, size.h)), output)
            }
        };
        let bytes = u64::try_from(size.w)
            .ok()?
            .checked_mul(u64::try_from(size.h).ok()?)?
            .checked_mul(4)?;
        if bytes == 0 || bytes > 128 * 1024 * 1024 {
            return None;
        }
        Some((size, output))
    }

    fn capture_dma_constraints(&mut self, output: &Output) -> Option<Arc<DmabufConstraints>> {
        if let Some(backend) = self.nested_backend.clone() {
            backend
                .try_borrow_mut()
                .ok()
                .and_then(|mut backend| dma_constraints(backend.renderer()))
        } else {
            self.direct_backend
                .as_mut()
                .and_then(|backend| backend.capture_renderer(output))
                .and_then(|renderer| dma_constraints(renderer))
        }
    }

    pub(super) fn refresh_capture_source(&mut self, source: &ImageCaptureSource) {
        let mut outputs = Vec::new();
        for active in &mut self.image_capture.active {
            if active.session.source() != *source {
                continue;
            }
            active.revision = None;
            active.window_stamp = None;
            if active.pending.is_some()
                && let Some(Source::Output(weak)) = source.user_data().get::<Source>()
                && let Some(output) = weak.upgrade()
                && !outputs.contains(&output)
            {
                outputs.push(output);
            }
        }
        if !outputs.is_empty() {
            crate::backends::direct::schedule_capture(self, &outputs);
        }
    }

    pub(crate) fn refresh_capture_sessions(&mut self) {
        self.image_capture.copy.cleanup();
        let mut callback_delay: Option<Duration> = None;
        // Yield between distinct workspace compositions so input and display
        // deadlines can run even when several consumers request changed views.
        let mut workspace_compositions = 1usize;
        let mut sessions = std::mem::take(&mut self.image_capture.active);
        let mut workspaces = std::mem::take(&mut self.image_capture.workspaces);
        let mut reserved: u64 = sessions
            .iter()
            .map(|active| stream_bytes(active.size))
            .sum();
        sessions.retain_mut(|active| {
            let Some((size, output)) = self.capture_source_geometry(&active.session.source())
            else {
                self.cancel_session_fences(active.session.as_ref());
                reserved = reserved.saturating_sub(stream_bytes(active.size));
                return false;
            };
            let dma = self.capture_dma_constraints(&output);
            let dma_node = dma.as_ref().map(|dma| dma.node);
            if size != active.size || dma_node != active.dma_node {
                let next_reserved = reserved
                    .saturating_sub(stream_bytes(active.size))
                    .saturating_add(stream_bytes(size));
                if next_reserved > MAX_STREAM_BYTES {
                    self.cancel_session_fences(active.session.as_ref());
                    reserved = reserved.saturating_sub(stream_bytes(active.size));
                    return false;
                }
                reserved = next_reserved;
                if let Some(Source::Workspace(source)) =
                    active.session.source().user_data().get::<Source>()
                {
                    // GPU resources belong to the renderer that created them.
                    workspaces.remove(&(
                        source.id,
                        active.session.draw_cursor(),
                        source.include_shell,
                    ));
                }
                active.size = size;
                active.dma_node = dma_node;
                active.revision = None;
                active.window_stamp = None;
                active.session.update_constraints(BufferConstraints {
                    size,
                    shm: vec![wl_shm::Format::Xrgb8888],
                    dma: dma.as_deref().cloned(),
                });
                if let Some(frame) = active.pending.take() {
                    frame.fail(FailureReason::BufferConstraints);
                }
            }
            if let Some(Source::Workspace(source)) =
                active.session.source().user_data().get::<Source>()
                && source_active(&active.session.source())
            {
                let source = source.clone();
                let key = (
                    source.id,
                    active.session.draw_cursor(),
                    source.include_shell,
                );
                if let Some(frame) = active.pending.take() {
                    let view = workspaces.entry(key).or_insert_with(|| {
                        super::workspace_capture::WorkspaceCapture::new(
                            source.clone(),
                            self.capture_epoch,
                        )
                    });
                    if !view.source.valid(self) {
                        *view = super::workspace_capture::WorkspaceCapture::new(
                            source.clone(),
                            self.capture_epoch,
                        );
                    }
                    if let Some(delay) = super::workspace_capture::process(
                        view,
                        self,
                        active,
                        frame,
                        &mut workspace_compositions,
                    ) {
                        callback_delay =
                            Some(callback_delay.map_or(delay, |existing| existing.min(delay)));
                    }
                }
            }
            if let Some(Source::Window(id)) = active.session.source().user_data().get::<Source>() {
                let id = *id;
                if let Some(frame) = active.pending.take() {
                    let Some(info) = self.window_capture_info(id) else {
                        return false;
                    };
                    let callback_interval = Duration::from_nanos(
                        1_000_000_000_000
                            / info
                                .output
                                .current_mode()
                                .map(|mode| mode.refresh.max(1000) as u64)
                                .unwrap_or(60_000),
                    );
                    if let Some(toplevel) = info.window.toplevel()
                        && pending_frame_callbacks(toplevel.wl_surface())
                    {
                        smithay::desktop::utils::send_frames_surface_tree(
                            toplevel.wl_surface(),
                            &info.output,
                            self.start_time.elapsed(),
                            Some(callback_interval),
                            |_, _| None,
                        );
                    }
                    let mut stamp = WindowStamp {
                        surfaces: Vec::new(),
                        geometry: info.geometry,
                        scale: info.scale,
                        epoch: self.capture_epoch,
                        cursor: None,
                    };
                    if let Some(toplevel) = info.window.toplevel() {
                        stamp.surfaces = surface_stamp(toplevel.wl_surface(), Some(info.geometry));
                    }
                    let cursor = active
                        .session
                        .draw_cursor()
                        .then(|| self.window_capture_cursor(id, &info.window, info.geometry))
                        .flatten();
                    if let Some(geometry) = cursor
                        && let Some(pointer) = self.seat.get_pointer()
                    {
                        use smithay::input::pointer::CursorImageStatus;
                        let image = match &self.cursor_status {
                            CursorImageStatus::Named(_) => {
                                let (icon, epoch, frame) = self.cursor_animation.capture_revision();
                                CursorImageStamp::Named(icon, epoch, frame)
                            }
                            CursorImageStatus::Surface(surface) => {
                                let hotspot =
                                    smithay::wayland::compositor::with_states(surface, |states| {
                                        states
                                            .data_map
                                            .get::<smithay::input::pointer::CursorImageSurfaceData>(
                                            )
                                            .map(|data| {
                                                data.lock()
                                                    .expect("cursor metadata mutex poisoned")
                                                    .hotspot
                                            })
                                            .unwrap_or_default()
                                    });
                                CursorImageStamp::Surface(surface_stamp(surface, None), hotspot)
                            }
                            CursorImageStatus::Hidden => CursorImageStamp::Hidden,
                        };
                        stamp.cursor = Some(CursorStamp {
                            location: pointer.current_location(),
                            geometry,
                            image,
                        });
                    }
                    if active.window_stamp.as_ref() == Some(&stamp) {
                        active.pending = Some(frame);
                        if info
                            .window
                            .toplevel()
                            .is_some_and(|toplevel| pending_frame_callbacks(toplevel.wl_surface()))
                        {
                            callback_delay =
                                Some(callback_delay.map_or(callback_interval, |delay| {
                                    delay.min(callback_interval)
                                }));
                        }
                    } else {
                        crate::backends::direct::capture::touch_cache(self);
                        let result = if let Some(backend) = self.nested_backend.clone() {
                            backend
                                .try_borrow_mut()
                                .map_err(|error| error.to_string())
                                .and_then(|mut backend| {
                                    crate::render::capture_stream_window(
                                        backend.renderer(),
                                        self,
                                        &info,
                                        cursor,
                                        &frame.buffer(),
                                    )
                                })
                        } else if let Some(mut backend) = self.direct_backend.take() {
                            let result = backend
                                .capture_renderer(&info.output)
                                .ok_or_else(|| "Capture output is unavailable".to_string())
                                .and_then(|renderer| {
                                    crate::render::capture_stream_window(
                                        renderer,
                                        self,
                                        &info,
                                        cursor,
                                        &frame.buffer(),
                                    )
                                });
                            self.direct_backend = Some(backend);
                            result
                        } else {
                            Err("Capture renderer is unavailable".into())
                        };
                        active.window_stamp = result.as_ref().ok().map(|_| stamp);
                        self.finish_stream_frame(frame, active.session.as_ref(), result);
                    }
                }
            }
            true
        });
        workspaces.retain(|(id, cursor, include_shell), view| {
            let matches = |active: &Active| {
                active.session.draw_cursor() == *cursor
                    && matches!(active.session.source().user_data().get::<Source>(), Some(Source::Workspace(source)) if source.id == *id && source.include_shell == *include_shell)
            };
            if !sessions.iter().any(matches) { return false; }
            let pending = sessions.iter().any(|active| matches(active)
                && active.pending.is_some() && source_active(&active.session.source()));
            if !pending && view.target.is_some() {
                let idle = view.last_used.elapsed();
                if idle >= Duration::from_secs(2) {
                    view.release_idle_target();
                } else {
                    let delay = Duration::from_secs(2) - idle;
                    callback_delay = Some(callback_delay.map_or(delay, |existing| existing.min(delay)));
                }
            }
            true
        });
        self.image_capture
            .workspace_callbacks
            .retain(|id, _| workspaces.keys().any(|(workspace, _, _)| id == workspace));
        self.image_capture.workspaces = workspaces;
        self.image_capture.active = sessions;
        if let Some(delay) = callback_delay {
            let deadline = Instant::now() + delay;
            if self
                .image_capture
                .callback_deadline
                .is_none_or(|previous| deadline < previous)
            {
                if let Some(timer) = self.image_capture.callback_timer.take() {
                    self.loop_handle.remove(timer);
                }
                self.image_capture.callback_deadline = Some(deadline);
                use smithay::reexports::calloop::timer::{TimeoutAction, Timer};
                match self
                    .loop_handle
                    .insert_source(Timer::from_duration(delay), |_, _, state| {
                        state.image_capture.callback_timer = None;
                        state.image_capture.callback_deadline = None;
                        TimeoutAction::Drop
                    }) {
                    Ok(token) => self.image_capture.callback_timer = Some(token),
                    Err(error) => {
                        self.image_capture.callback_deadline = None;
                        tracing::warn!(%error, "could not schedule capture callbacks")
                    }
                }
            }
        } else if let Some(timer) = self.image_capture.callback_timer.take() {
            self.loop_handle.remove(timer);
            self.image_capture.callback_deadline = None;
        }
    }

    pub(crate) fn process_output_streams(
        &mut self,
        renderer: &mut GlesRenderer,
        texture: &GlesTexture,
        output: &Output,
        cursor: bool,
        revision: u64,
    ) {
        let mut sessions = std::mem::take(&mut self.image_capture.active);
        for active in &mut sessions {
            let matches = matches!(active.session.source().user_data().get::<Source>(), Some(Source::Output(weak)) if weak.upgrade().as_ref() == Some(output));
            if matches {
                let size = output.current_transform().transform_size(texture.size());
                if active.size != Size::from((size.w, size.h)) {
                    if let Some(frame) = active.pending.take() {
                        frame.fail(FailureReason::BufferConstraints);
                    }
                    active.revision = None;
                    continue;
                }
            }
            if matches
                && active.session.draw_cursor() == cursor
                && active.revision != Some(revision)
                && let Some(frame) = active.pending.take()
            {
                let result = crate::render::capture_stream_texture(
                    renderer,
                    texture,
                    &frame.buffer(),
                    output.current_transform(),
                    false,
                    &self.readbacks,
                );
                active.revision = result.as_ref().ok().map(|_| revision);
                self.finish_stream_frame(frame, active.session.as_ref(), result);
            }
        }
        self.image_capture.active = sessions;
    }

    pub(crate) fn fail_output_streams(&mut self, output: &Output, cursor: bool) {
        for active in &mut self.image_capture.active {
            if active.session.draw_cursor() == cursor
                && matches!(active.session.source().user_data().get::<Source>(), Some(Source::Output(weak)) if weak.upgrade().as_ref() == Some(output))
            {
                if let Some(frame) = active.pending.take() {
                    frame.fail(FailureReason::Unknown);
                }
                active.revision = None;
            }
        }
    }

    fn cancel_session_fences(&mut self, session: SessionRef) {
        let fences: Vec<_> = self
            .image_capture
            .in_flight
            .iter()
            .filter_map(|(id, pending)| (pending.session == session).then_some(*id))
            .collect();
        for id in fences {
            self.cancel_capture_fence(id);
        }
    }

    fn cancel_capture_fence(&mut self, id: u64) {
        if let Some(pending) = self.image_capture.in_flight.remove(&id) {
            self.loop_handle.remove(pending.fence);
            self.loop_handle.remove(pending.timeout);
            self.reset_stream_submission(&pending.session, pending.generation);
        }
    }

    fn capture_completion_stamp(&self, source: &ImageCaptureSource) -> Option<CompletionStamp> {
        if !source_active(source) {
            return None;
        }
        let (size, output) = self.capture_source_geometry(source)?;
        Some(CompletionStamp {
            size,
            mode: output.current_mode(),
            transform: output.current_transform(),
            scale: output.current_scale().fractional_scale(),
            output,
        })
    }

    fn reset_stream_submission(&mut self, session: &SessionRef, generation: u64) {
        if current_submission(session, generation)
            && let Some(active) = self
                .image_capture
                .active
                .iter_mut()
                .find(|active| active.session.as_ref() == *session)
        {
            active.revision = None;
            active.window_stamp = None;
        }
    }

    fn fail_stream_submission(
        &mut self,
        frame: Frame,
        session: SessionRef,
        generation: u64,
        reason: FailureReason,
    ) {
        self.loop_handle.insert_idle(move |state| {
            state.reset_stream_submission(&session, generation);
            frame.fail(reason);
        });
    }

    pub(super) fn finish_stream_frame(
        &mut self,
        frame: Frame,
        session: SessionRef,
        result: Result<crate::render::StreamCopy, String>,
    ) {
        // Sessions can be outside `image_capture.active` while their copies are
        // submitted. Keep the generation on the session so every completion
        // can reject an older failure after a replacement has been submitted.
        let generation = next_submission(&session);
        if !session.alive() || !frame.alive() || frame.has_failed() || !frame.buffer().is_alive() {
            self.fail_stream_submission(frame, session, generation, FailureReason::Stopped);
            return;
        }
        let Some(stamp) = self.capture_completion_stamp(&session.source()) else {
            self.fail_stream_submission(frame, session, generation, FailureReason::Stopped);
            return;
        };
        let result = match result {
            Ok(crate::render::StreamCopy::Shm { readback, size }) => {
                self.defer_stream_readback(frame, session, generation, readback, size);
                return;
            }
            Ok(crate::render::StreamCopy::Gpu(sync)) => Ok(sync),
            Err(error) => Err(error),
        };
        match result {
            Ok(sync) if sync.is_reached() => frame.success(
                Transform::Normal,
                None,
                smithay::utils::Clock::<smithay::utils::Monotonic>::new().now(),
            ),
            Ok(sync) => {
                let Some(fd) = sync.export() else {
                    self.fail_stream_submission(frame, session, generation, FailureReason::Unknown);
                    return;
                };
                use smithay::reexports::calloop::timer::{TimeoutAction, Timer};
                use smithay::reexports::calloop::{Interest, Mode, PostAction, generic::Generic};
                self.image_capture.next_fence = self.image_capture.next_fence.wrapping_add(1);
                let id = self.image_capture.next_fence;
                let deadline = Instant::now() + Duration::from_secs(5);
                let source = Generic::new(fd, Interest::READ, Mode::OneShot);
                let fence = match self.loop_handle.insert_source(source, move |_, _, state| {
                    if let Some(pending) = state.image_capture.in_flight.remove(&id) {
                        state.loop_handle.remove(pending.timeout);
                        if pending.epoch == state.capture_epoch
                            && current_submission(&pending.session, pending.generation)
                            && Instant::now() < pending.deadline
                            && pending.completion.is_reached()
                            && pending.session.alive()
                            && pending.frame.alive()
                            && pending.frame.buffer().is_alive()
                            && state
                                .capture_completion_stamp(&pending.session.source())
                                .as_ref()
                                == Some(&pending.stamp)
                            && !pending.frame.has_failed()
                        {
                            pending.frame.success(
                                Transform::Normal,
                                None,
                                smithay::utils::Clock::<smithay::utils::Monotonic>::new().now(),
                            );
                        } else {
                            state.fail_stream_submission(
                                pending.frame,
                                pending.session,
                                pending.generation,
                                FailureReason::Unknown,
                            );
                        }
                    }
                    Ok(PostAction::Remove)
                }) {
                    Ok(token) => token,
                    Err(error) => {
                        tracing::warn!(%error, "could not register capture fence");
                        self.fail_stream_submission(
                            frame,
                            session,
                            generation,
                            FailureReason::Unknown,
                        );
                        return;
                    }
                };
                let timeout = match self.loop_handle.insert_source(
                    Timer::from_deadline(deadline),
                    move |_, _, state| {
                        if let Some(pending) = state.image_capture.in_flight.remove(&id) {
                            state.loop_handle.remove(pending.fence);
                            pending.frame.fail(FailureReason::Stopped);
                            if current_submission(&pending.session, pending.generation) {
                                state
                                    .image_capture
                                    .active
                                    .retain(|active| active.session.as_ref() != pending.session);
                            }
                        }
                        TimeoutAction::Drop
                    },
                ) {
                    Ok(token) => token,
                    Err(error) => {
                        self.loop_handle.remove(fence);
                        tracing::warn!(%error, "could not register capture deadline");
                        self.fail_stream_submission(
                            frame,
                            session,
                            generation,
                            FailureReason::Unknown,
                        );
                        return;
                    }
                };
                self.image_capture.in_flight.insert(
                    id,
                    InFlight {
                        frame,
                        session,
                        generation,
                        epoch: self.capture_epoch,
                        fence,
                        timeout,
                        completion: sync,
                        deadline,
                        stamp,
                    },
                );
            }
            Err(error) => {
                tracing::warn!(%error, "stream capture failed");
                self.fail_stream_submission(frame, session, generation, FailureReason::Unknown);
            }
        }
    }
}

#[path = "image_capture/readback.rs"]
mod readback;

#[cfg(test)]
#[path = "workspace_capture_tests.rs"]
mod workspace_tests;

#[cfg(test)]
#[path = "image_capture/frame_liveness_tests.rs"]
mod frame_liveness_tests;
