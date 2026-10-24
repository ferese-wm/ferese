//! Persistent, damage-driven capture for the native portal. One frame may be
//! outstanding per session; client buffers remain owned until GPU completion.
use smithay::reexports::calloop::RegistrationToken;
use std::collections::HashMap;
use std::sync::Arc;
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
use smithay::utils::{Buffer, Logical, Point, Rectangle, Size, Transform};
use smithay::wayland::compositor::{TraversalAction, with_surface_tree_downward};
use smithay::wayland::image_capture_source::{ImageCaptureSource, ImageCaptureSourceHandler, OutputCaptureSourceHandler, OutputCaptureSourceState};
use smithay::wayland::image_copy_capture::{BufferConstraints, DmabufConstraints, Frame, FrameRef, ImageCopyCaptureHandler, ImageCopyCaptureState, Session, SessionRef};

use crate::Ferese;

const MAX_SESSIONS: usize = 16;

pub(super) enum Source {
    Output(WeakOutput),
    Window(WindowId),
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
struct CursorStamp {
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

struct Active {
    session: Session,
    pending: Option<Frame>,
    revision: Option<u64>,
    window_stamp: Option<WindowStamp>,
    size: Size<i32, Buffer>,
    dma_node: Option<smithay::backend::drm::DrmNode>,
}

struct InFlight {
    frame: Frame,
    session: SessionRef,
    epoch: u64,
    fence: RegistrationToken,
    timeout: RegistrationToken,
}

pub(crate) struct CaptureSessions {
    copy: ImageCopyCaptureState,
    outputs: OutputCaptureSourceState,
    active: Vec<Active>,
    in_flight: HashMap<u64, InFlight>,
    next_fence: u64,
    callback_timer: Option<RegistrationToken>,
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
    pub(crate) fn new(display: &DisplayHandle) -> Self {
        Self {
            copy: ImageCopyCaptureState::new_with_filter::<Ferese, _>(display, authorized),
            outputs: OutputCaptureSourceState::new_with_filter::<Ferese, _>(display, authorized),
            active: Vec::new(),
            in_flight: HashMap::new(),
            next_fence: 0,
            callback_timer: None,
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
    fn capture_source_geometry(
        &self,
        source: &ImageCaptureSource,
    ) -> Option<(Size<i32, Buffer>, Output)> {
        if self.session_lock.active() || !source.alive() || !super::screencopy::capture_allowed() {
            return None;
        }
        let (size, output) = match source.user_data().get::<Source>()? {
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
        crate::backends::direct::schedule_capture(self, &outputs);
    }

    pub(crate) fn refresh_capture_sessions(&mut self) {
        self.image_capture.copy.cleanup();
        let mut callback_delay: Option<Duration> = None;
        let mut sessions = std::mem::take(&mut self.image_capture.active);
        sessions.retain_mut(|active| {
            let Some((size, output)) = self.capture_source_geometry(&active.session.source())
            else {
                let session = active.session.as_ref();
                let fences: Vec<_> = self
                    .image_capture
                    .in_flight
                    .iter()
                    .filter_map(|(id, pending)| (pending.session == session).then_some(*id))
                    .collect();
                for id in fences {
                    self.cancel_capture_fence(id);
                }
                return false;
            };
            let dma = self.capture_dma_constraints(&output);
            let dma_node = dma.as_ref().map(|dma| dma.node);
            if size != active.size || dma_node != active.dma_node {
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
                        active.window_stamp = Some(stamp);
                        self.finish_stream_frame(frame, active.session.as_ref(), result);
                    }
                }
            }
            true
        });
        self.image_capture.active = sessions;
        if let Some(delay) = callback_delay {
            if self.image_capture.callback_timer.is_none() {
                use smithay::reexports::calloop::timer::{TimeoutAction, Timer};
                match self
                    .loop_handle
                    .insert_source(Timer::from_duration(delay), |_, _, state| {
                        state.image_capture.callback_timer = None;
                        TimeoutAction::Drop
                    }) {
                    Ok(token) => self.image_capture.callback_timer = Some(token),
                    Err(error) => {
                        tracing::warn!(%error, "could not schedule captured window callbacks")
                    }
                }
            }
        } else if let Some(timer) = self.image_capture.callback_timer.take() {
            self.loop_handle.remove(timer);
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
                );
                active.revision = Some(revision);
                self.finish_stream_frame(frame, active.session.as_ref(), result);
            }
        }
        self.image_capture.active = sessions;
    }

    fn cancel_capture_fence(&mut self, id: u64) {
        if let Some(pending) = self.image_capture.in_flight.remove(&id) {
            self.loop_handle.remove(pending.fence);
            self.loop_handle.remove(pending.timeout);
        }
    }

    fn finish_stream_frame(
        &mut self,
        frame: Frame,
        session: SessionRef,
        result: Result<smithay::backend::renderer::sync::SyncPoint, String>,
    ) {
        match result {
            Ok(sync) if sync.is_reached() => frame.success(
                Transform::Normal,
                None,
                smithay::utils::Clock::<smithay::utils::Monotonic>::new().now(),
            ),
            Ok(sync) => {
                let Some(fd) = sync.export() else {
                    frame.fail(FailureReason::Unknown);
                    return;
                };
                use smithay::reexports::calloop::timer::{TimeoutAction, Timer};
                use smithay::reexports::calloop::{Interest, Mode, PostAction, generic::Generic};
                self.image_capture.next_fence = self.image_capture.next_fence.wrapping_add(1);
                let id = self.image_capture.next_fence;
                let source = Generic::new(fd, Interest::READ, Mode::OneShot);
                let fence = match self.loop_handle.insert_source(source, move |_, _, state| {
                    if let Some(pending) = state.image_capture.in_flight.remove(&id) {
                        state.loop_handle.remove(pending.timeout);
                        if pending.epoch == state.capture_epoch
                            && state
                                .capture_source_geometry(&pending.session.source())
                                .is_some()
                            && !pending.frame.has_failed()
                        {
                            pending.frame.success(
                                Transform::Normal,
                                None,
                                smithay::utils::Clock::<smithay::utils::Monotonic>::new().now(),
                            );
                        } else {
                            pending.frame.fail(
                                if state
                                    .capture_source_geometry(&pending.session.source())
                                    .is_some()
                                {
                                    FailureReason::BufferConstraints
                                } else {
                                    FailureReason::Stopped
                                },
                            );
                        }
                    }
                    Ok(PostAction::Remove)
                }) {
                    Ok(token) => token,
                    Err(error) => {
                        tracing::warn!(%error, "could not register capture fence");
                        frame.fail(FailureReason::Unknown);
                        return;
                    }
                };
                let timeout = match self.loop_handle.insert_source(
                    Timer::from_duration(Duration::from_secs(5)),
                    move |_, _, state| {
                        if let Some(pending) = state.image_capture.in_flight.remove(&id) {
                            state.loop_handle.remove(pending.fence);
                            pending.frame.fail(FailureReason::Stopped);
                            state
                                .image_capture
                                .active
                                .retain(|active| active.session.as_ref() != pending.session);
                        }
                        TimeoutAction::Drop
                    },
                ) {
                    Ok(token) => token,
                    Err(error) => {
                        self.loop_handle.remove(fence);
                        tracing::warn!(%error, "could not register capture deadline");
                        frame.fail(FailureReason::Unknown);
                        return;
                    }
                };
                self.image_capture.in_flight.insert(
                    id,
                    InFlight {
                        frame,
                        session,
                        epoch: self.capture_epoch,
                        fence,
                        timeout,
                    },
                );
            }
            Err(error) => {
                tracing::warn!(%error, "stream capture failed");
                frame.fail(FailureReason::Unknown);
            }
        }
    }
}
