use std::os::unix::fs::MetadataExt;
use std::sync::{Arc, Mutex};

use ferese_layout::WindowId;
use ferese_protocols::window_capture::v1::server::ferese_window_capture_manager_v1 as manager;
use ferese_protocols::window_capture::v1::server::ferese_window_capture_manager_v1::FereseWindowCaptureManagerV1;
use smithay::backend::renderer::utils::{RendererSurfaceStateUserData, SurfaceView};
use smithay::reexports::wayland_protocols_wlr::screencopy::v1::server::zwlr_screencopy_frame_v1 as frame;
use smithay::reexports::wayland_protocols_wlr::screencopy::v1::server::zwlr_screencopy_frame_v1::ZwlrScreencopyFrameV1;
use smithay::reexports::wayland_server::protocol::wl_buffer::WlBuffer;
use smithay::reexports::wayland_server::protocol::wl_shm;
use smithay::reexports::wayland_server::{
    Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource,
};
use smithay::utils::{Logical, Point, Rectangle};
use smithay::wayland::compositor::{TraversalAction, with_surface_tree_downward};
use smithay::wayland::shm::with_buffer_contents_mut;

use super::screencopy::FrameData;
use super::screenshot::CaptureBuffer;
use crate::Ferese;

const MAX_BYTES: usize = 128 * 1024 * 1024;
const MAX_FRAMES: usize = 8;

fn capture_scale(output_scale: f64, buffer_scale: i32, view: SurfaceView) -> f64 {
    if view.dst.w <= 0 || view.dst.h <= 0 {
        return output_scale;
    }
    let native_x = view.src.size.w * f64::from(buffer_scale) / f64::from(view.dst.w);
    let native_y = view.src.size.h * f64::from(buffer_scale) / f64::from(view.dst.h);
    output_scale.max(native_x).max(native_y)
}

#[derive(Debug, Default)]
pub(crate) struct Budget {
    bytes: usize,
    frames: usize,
}

#[derive(Debug)]
struct Permit {
    budget: Arc<Mutex<Budget>>,
    bytes: usize,
}

impl Permit {
    fn reserve(budget: &Arc<Mutex<Budget>>, bytes: usize) -> Option<Self> {
        let mut available = budget
            .lock()
            .expect("surface rendering state must remain consistent across callbacks");
        if bytes == 0
            || bytes > MAX_BYTES.saturating_sub(available.bytes)
            || available.frames >= MAX_FRAMES
        {
            return None;
        }
        available.bytes += bytes;
        available.frames += 1;
        Some(Self {
            budget: budget.clone(),
            bytes,
        })
    }
}

impl Drop for Permit {
    fn drop(&mut self) {
        let mut budget = self
            .budget
            .lock()
            .expect("surface rendering state must remain consistent across callbacks");
        budget.bytes -= self.bytes;
        budget.frames -= 1;
    }
}

#[derive(Debug)]
pub(super) struct Snapshot {
    id: WindowId,
    epoch: u64,
    logical_size: (u32, u32),
    pixels: CaptureBuffer,
    _permit: Permit,
}

#[derive(Clone)]
pub(crate) struct Global {
    budget: Arc<Mutex<Budget>>,
    worker: std::sync::mpsc::SyncSender<Conversion>,
}

struct Conversion {
    snapshot: Snapshot,
    frame: ZwlrScreencopyFrameV1,
    manager: FereseWindowCaptureManagerV1,
}

pub(crate) fn is_portal(stream: &std::os::unix::net::UnixStream) -> bool {
    use std::os::fd::AsRawFd;
    let mut credentials = libc::ucred {
        pid: 0,
        uid: 0,
        gid: 0,
    };
    let mut length = std::mem::size_of_val(&credentials) as libc::socklen_t;
    // SAFETY: the connected socket is live and both out pointers cover their
    // initialized stack allocations. SO_PEERCRED identifies the connecting process.
    let valid = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&raw mut credentials).cast(),
            &raw mut length,
        ) == 0
    };
    if !valid || length as usize != std::mem::size_of_val(&credentials) || credentials.pid <= 0 {
        return false;
    }
    let expected = std::env::current_exe()
        .ok()
        .and_then(|exe| {
            exe.parent()
                .map(|dir| dir.join("xdg-desktop-portal-ferese"))
        })
        .and_then(|exe| std::fs::metadata(exe).ok());
    let actual = std::fs::metadata(format!("/proc/{}/exe", credentials.pid)).ok();
    matches!((expected, actual), (Some(expected), Some(actual)) if expected.dev() == actual.dev() && expected.ino() == actual.ino())
}

pub(super) fn init_global(
    display: &DisplayHandle,
    loop_handle: &smithay::reexports::calloop::LoopHandle<'static, Ferese>,
) {
    use smithay::reexports::calloop::channel;
    let (worker, requests) = std::sync::mpsc::sync_channel::<Conversion>(MAX_FRAMES);
    let (results, source) = channel::sync_channel::<Conversion>(MAX_FRAMES);
    let Ok(token) = loop_handle.insert_source(source, |event, _, state| {
        if let channel::Event::Msg(result) = event {
            finish_conversion(state, result);
        }
    }) else {
        tracing::warn!("window capture result source could not be registered");
        return;
    };

    if let Err(error) = std::thread::Builder::new()
        .name("ferese-capture".into())
        .spawn(move || {
            while let Ok(mut job) = requests.recv() {
                if job.frame.is_alive() {
                    crate::render::convert_window_pixels(&mut job.snapshot.pixels.pixels);
                }
                // Permits cap the total number of queued, working and completed
                // frames at MAX_FRAMES, so the result queue cannot fill here.
                if results.send(job).is_err() {
                    break;
                }
            }
        })
    {
        loop_handle.remove(token);
        tracing::warn!(%error, "window capture worker could not be started");
        return;
    }

    display.create_global::<Ferese, FereseWindowCaptureManagerV1, Global>(
        2,
        Global {
            budget: Arc::default(),
            worker,
        },
    );
}

fn finish_conversion(state: &Ferese, result: Conversion) {
    let Conversion {
        snapshot,
        frame,
        manager,
    } = result;
    let Some(data) = frame.data::<Arc<FrameData>>() else {
        return;
    };
    if !frame.is_alive()
        || *data
            .used
            .lock()
            .expect("surface rendering state must remain consistent across callbacks")
    {
        return;
    }
    if state.session_lock.active()
        || !super::screencopy::capture_allowed()
        || !state.capture_window_allowed(snapshot.id, snapshot.epoch)
    {
        *data
            .used
            .lock()
            .expect("surface rendering state must remain consistent across callbacks") = true;
        frame.failed();
        return;
    }

    let size = (snapshot.pixels.width as u32, snapshot.pixels.height as u32);
    let logical_size = snapshot.logical_size;
    *data
        .snapshot
        .lock()
        .expect("surface rendering state must remain consistent across callbacks") = Some(snapshot);
    if manager.is_alive() {
        manager.geometry(&frame, logical_size.0, logical_size.1);
    }
    frame.buffer(wl_shm::Format::Argb8888, size.0, size.1, size.0 * 4);
}

impl GlobalDispatch<FereseWindowCaptureManagerV1, Global> for Ferese {
    fn can_view(client: Client, _: &Global) -> bool {
        client
            .get_data::<crate::state::ClientState>()
            .is_some_and(|state| {
                state
                    .capabilities
                    .contains(crate::private_client::ClientCapabilities::WINDOW_CAPTURE)
            })
    }

    fn bind(
        _: &mut Self,
        _: &DisplayHandle,
        _: &Client,
        resource: New<FereseWindowCaptureManagerV1>,
        global: &Global,
        data_init: &mut DataInit<'_, Self>,
    ) {
        data_init.init(resource, global.clone());
    }
}

impl Dispatch<FereseWindowCaptureManagerV1, Global> for Ferese {
    fn request(
        state: &mut Self,
        _: &Client,
        manager: &FereseWindowCaptureManagerV1,
        request: manager::Request,
        global: &Global,
        _: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        if let manager::Request::RefreshSource { source } = request {
            if let Some(source) =
                smithay::wayland::image_capture_source::ImageCaptureSource::from_resource(&source)
            {
                state.refresh_capture_source(&source);
            }
            return;
        }
        if let manager::Request::CreateSource {
            window_hi,
            window_lo,
            source,
        } = request
        {
            use smithay::wayland::image_capture_source::{
                ImageCaptureSource, ImageCaptureSourceData,
            };
            let id = WindowId((u64::from(window_hi) << 32) | u64::from(window_lo));
            let capture = ImageCaptureSource::new();
            capture
                .user_data()
                .insert_if_missing(|| super::image_capture::Source::Window(id));
            let resource = data_init.init(
                source,
                ImageCaptureSourceData {
                    source: capture.clone(),
                },
            );
            capture.add_instance(&resource);
            if let Some(info) = state.window_capture_info(id) {
                manager.source_geometry(
                    &resource,
                    info.geometry.size.w as u32,
                    info.geometry.size.h as u32,
                );
            }
            return;
        }
        if let manager::Request::CaptureWindow {
            window_hi,
            window_lo,
            overlay_cursor,
            frame,
        } = request
        {
            let id = WindowId((u64::from(window_hi) << 32) | u64::from(window_lo));
            let mut busy = false;
            let snapshot = (overlay_cursor <= 1)
                .then(|| state.window_snapshot(id, overlay_cursor == 1, &global.budget, &mut busy))
                .flatten();
            let size = snapshot
                .as_ref()
                .map(|snapshot| (snapshot.pixels.width, snapshot.pixels.height))
                .unwrap_or((1, 1));
            let failed = snapshot.is_none();
            let resource = data_init.init(
                frame,
                Arc::new(FrameData {
                    output: None,
                    region: Rectangle::from_size(size.into()),
                    overlay_cursor: false,
                    used: Mutex::new(failed),
                    snapshot: Mutex::new(None),
                    history: None,
                }),
            );
            if failed {
                if busy {
                    manager.busy(&resource);
                }
                resource.failed();
            } else if global
                .worker
                .try_send(Conversion {
                    snapshot: snapshot
                        .expect("conversion is queued only after obtaining a capture snapshot"),
                    frame: resource.clone(),
                    manager: manager.clone(),
                })
                .is_err()
            {
                *resource
                    .data::<Arc<FrameData>>()
                    .expect("capture frame resources are initialized with their FrameData")
                    .used
                    .lock()
                    .expect("surface rendering state must remain consistent across callbacks") =
                    true;
                manager.busy(&resource);
                resource.failed();
            }
        }
    }
}

pub(crate) struct WindowInfo {
    pub window: smithay::desktop::Window,
    pub output: smithay::output::Output,
    pub geometry: Rectangle<i32, Logical>,
    pub scale: f64,
    pub bytes: usize,
}

impl Ferese {
    pub(super) fn window_capture_info(&self, id: WindowId) -> Option<WindowInfo> {
        if self.session_lock.active() || !super::screencopy::capture_allowed() {
            return None;
        }
        let window = self
            .windows
            .ids()
            .iter()
            .find(|(_, candidate)| **candidate == id)?
            .0
            .clone();
        if self.capture_protected(&window) {
            return None;
        }
        let workspace = self.workspaces.workspace_for_window(id)?;
        let owner = self.output_workspaces.output_for_workspace(workspace)?;
        let output = self
            .output_ids
            .iter()
            .find(|(_, candidate)| **candidate == owner)?
            .0
            .clone();
        let geometry = window.geometry();
        let output_scale = output.current_scale().fractional_scale();
        // A low-density parent can contain higher-density content. Match the
        // rendered surface tree, excluding buffers outside the captured geometry.
        let mut scale = output_scale;
        if let Some(toplevel) = window.toplevel() {
            with_surface_tree_downward(
                toplevel.wl_surface(),
                Point::<f64, Logical>::default(),
                |_, states, location| {
                    let Some(data) = states.data_map.get::<RendererSurfaceStateUserData>() else {
                        return TraversalAction::SkipChildren;
                    };
                    let data = data
                        .lock()
                        .expect("Smithay surface metadata must remain consistent across callbacks");
                    let Some(view) = data.view() else {
                        return TraversalAction::SkipChildren;
                    };
                    let location = *location + view.offset.to_f64();
                    if Rectangle::new(location, view.dst.to_f64()).overlaps(geometry.to_f64()) {
                        scale = capture_scale(scale, data.buffer_scale(), view);
                    }
                    TraversalAction::DoChildren(location)
                },
                |_, _, _| {},
                |_, _, _| true,
            );
        }
        let size: smithay::utils::Size<i32, smithay::utils::Physical> =
            geometry.size.to_physical_precise_round(scale);
        let bytes = usize::try_from(size.w)
            .ok()?
            .checked_mul(usize::try_from(size.h).ok()?)?
            .checked_mul(4)?;
        if bytes == 0 || bytes > MAX_BYTES {
            return None;
        }
        Some(WindowInfo {
            window,
            output,
            geometry,
            scale,
            bytes,
        })
    }

    pub(super) fn window_capture_cursor(
        &self,
        id: WindowId,
        window: &smithay::desktop::Window,
        geometry: Rectangle<i32, Logical>,
    ) -> Option<Rectangle<i32, Logical>> {
        if !self.overview.is_presenting() && !self.capture_cursor_protected() {
            self.seat.get_pointer().and_then(|pointer| {
                let location = pointer.current_location();
                if self.window_under_visual(location).as_ref() != Some(window) {
                    return None;
                }
                let (x, y) = self.inverse_presented_window_point(id, location.x, location.y)?;
                let surface_point = smithay::utils::Point::from((x, y)) + geometry.loc.to_f64();
                let (surface, _) = window
                    .surface_under(surface_point, smithay::desktop::WindowSurfaceType::ALL)?;
                if pointer.current_focus().as_ref() != Some(&surface) {
                    return None;
                }
                Some(Rectangle::new(
                    (location - smithay::utils::Point::from((x, y))).to_i32_round(),
                    geometry.size,
                ))
            })
        } else {
            None
        }
    }

    fn window_snapshot(
        &mut self,
        id: WindowId,
        overlay_cursor: bool,
        budget: &Arc<Mutex<Budget>>,
        busy: &mut bool,
    ) -> Option<Snapshot> {
        let WindowInfo {
            window,
            output,
            geometry,
            scale,
            bytes,
        } = self.window_capture_info(id)?;
        let Some(permit) = Permit::reserve(budget, bytes) else {
            *busy = true;
            return None;
        };
        let cursor = overlay_cursor
            .then(|| self.window_capture_cursor(id, &window, geometry))
            .flatten();
        let result = if let Some(backend) = self.nested_backend.clone() {
            let mut backend = backend.try_borrow_mut().ok()?;
            crate::render::capture_window_frame(
                backend.renderer(),
                &window,
                geometry,
                scale,
                cursor.map(|rect| (&*self, rect)),
            )
        } else {
            let mut backend = self.direct_backend.take()?;
            let result = backend.capture_window_frame(self, &window, &output, cursor, scale);
            self.direct_backend = Some(backend);
            result
        };
        let pixels = result.ok()?;
        if let Some(toplevel) = window.toplevel() {
            smithay::desktop::utils::send_frames_surface_tree(
                toplevel.wl_surface(),
                &output,
                self.start_time.elapsed(),
                Some(std::time::Duration::ZERO),
                |_, _| Some(output.clone()),
            );
        }
        if pixels.pixels.len() != bytes {
            return None;
        }
        Some(Snapshot {
            id,
            epoch: self.capture_epoch,
            logical_size: (geometry.size.w as u32, geometry.size.h as u32),
            pixels,
            _permit: permit,
        })
    }
}

pub(super) fn copy_snapshot(
    state: &Ferese,
    resource: &ZwlrScreencopyFrameV1,
    data: &FrameData,
    buffer: &WlBuffer,
) {
    let mut used = data
        .used
        .lock()
        .expect("surface rendering state must remain consistent across callbacks");
    if *used {
        resource.post_error(
            frame::Error::AlreadyUsed,
            "window capture frame has already been used",
        );
        return;
    }
    *used = true;
    let Some(snapshot) = data
        .snapshot
        .lock()
        .expect("surface rendering state must remain consistent across callbacks")
        .take()
    else {
        resource.failed();
        return;
    };
    if state.session_lock.active()
        || !super::screencopy::capture_allowed()
        || !state.capture_window_allowed(snapshot.id, snapshot.epoch)
    {
        resource.failed();
        return;
    }
    if !super::screencopy::valid_shm_buffer(buffer, data.region.size) {
        resource.post_error(
            frame::Error::InvalidBuffer,
            "window capture requires the advertised ARGB8888 SHM buffer",
        );
        return;
    }
    let copied = with_buffer_contents_mut(buffer, |destination, length, layout| {
        let bytes = snapshot.pixels.pixels.len();
        let Ok(offset) = usize::try_from(layout.offset) else {
            return false;
        };
        if offset.checked_add(bytes).is_none_or(|end| end > length) {
            return false;
        }
        // SAFETY: Smithay owns the writable mapping, validated format/stride and
        // checked offset cover the complete snapshot; the source is separately owned.
        unsafe {
            std::ptr::copy_nonoverlapping(
                snapshot.pixels.pixels.as_ptr(),
                destination.add(offset),
                bytes,
            );
        }
        true
    })
    .unwrap_or(false);
    if !copied {
        resource.failed();
        return;
    }
    let timestamp = state.start_time.elapsed();
    resource.flags(frame::Flags::empty());
    resource.ready(
        (timestamp.as_secs() >> 32) as u32,
        timestamp.as_secs() as u32,
        timestamp.subsec_nanos(),
    );
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    #[test]
    fn sharing_preserves_backing_density_at_fractional_display_scales() {
        let integer = SurfaceView {
            src: Rectangle::from_size((640., 480.).into()),
            dst: (640, 480).into(),
            offset: (0, 0).into(),
        };
        let viewport = SurfaceView {
            src: Rectangle::from_size((1280., 960.).into()),
            ..integer
        };
        for output_scale in [1., 1.25, 1.5, 1.75, 2.] {
            assert_eq!(capture_scale(output_scale, 2, integer), 2.);
            assert_eq!(capture_scale(output_scale, 1, viewport), 2.);
        }
        assert_eq!(capture_scale(2.5, 2, integer), 2.5);
        let fractional = SurfaceView {
            src: Rectangle::from_size((1120., 840.).into()),
            ..integer
        };
        assert_eq!(capture_scale(1.25, 1, fractional), 1.75);
        assert_eq!(capture_scale(1.75, 1, fractional), 1.75);
    }

    #[test]
    fn sharing_uses_the_viewport_crop_instead_of_the_whole_buffer() {
        let cropped = SurfaceView {
            src: Rectangle::new((320., 240.).into(), (640., 480.).into()),
            dst: (640, 480).into(),
            offset: (0, 0).into(),
        };
        assert_eq!(capture_scale(1.75, 1, cropped), 1.75);
    }

    /// Exercise both asynchronous publication boundaries using real resources.
    pub(crate) fn check_deferred_snapshot(
        state: &Ferese,
        window: &smithay::desktop::Window,
        epoch: u64,
        allowed: bool,
    ) {
        let client = window.toplevel().unwrap().wl_surface().client().unwrap();
        let id = state.windows.ids()[window];
        let budget = Arc::default();
        let snapshot = || Snapshot {
            id,
            epoch,
            logical_size: (64, 48),
            pixels: CaptureBuffer {
                width: 64,
                height: 48,
                stride: 256,
                pixels: vec![0; 64 * 48 * 4],
            },
            _permit: Permit::reserve(&budget, 64 * 48 * 4).unwrap(),
        };
        let frame_data = || {
            Arc::new(FrameData {
                output: None,
                region: Rectangle::from_size((64, 48).into()),
                overlay_cursor: false,
                used: Mutex::new(false),
                snapshot: Mutex::new(None),
                history: None,
            })
        };
        let (worker, _requests) = std::sync::mpsc::sync_channel(1);
        let manager = client
            .create_resource::<FereseWindowCaptureManagerV1, Global, Ferese>(
                &state.display_handle,
                1,
                Global {
                    budget: budget.clone(),
                    worker,
                },
            )
            .unwrap();
        let data = frame_data();
        let frame = client
            .create_resource::<ZwlrScreencopyFrameV1, Arc<FrameData>, Ferese>(
                &state.display_handle,
                3,
                data.clone(),
            )
            .unwrap();
        finish_conversion(
            state,
            Conversion {
                snapshot: snapshot(),
                frame,
                manager,
            },
        );
        assert_eq!(data.snapshot.lock().unwrap().is_some(), allowed);
        assert_eq!(*data.used.lock().unwrap(), !allowed);

        let data = frame_data();
        *data.snapshot.lock().unwrap() = Some(snapshot());
        let frame = client
            .create_resource::<ZwlrScreencopyFrameV1, Arc<FrameData>, Ferese>(
                &state.display_handle,
                3,
                data.clone(),
            )
            .unwrap();
        let buffer = client
            .object_from_protocol_id::<WlBuffer>(&state.display_handle, 6)
            .unwrap();
        let before = with_buffer_contents_mut(&buffer, |ptr, _, layout| {
            // SAFETY: the fixture allocates and validates this 64x48 SHM buffer.
            unsafe {
                std::slice::from_raw_parts(ptr.add(layout.offset as usize), 64 * 48 * 4).to_vec()
            }
        })
        .unwrap();
        copy_snapshot(state, &frame, &data, &buffer);
        with_buffer_contents_mut(&buffer, |ptr, _, layout| {
            // SAFETY: same fixture-owned buffer as above.
            let after =
                unsafe { std::slice::from_raw_parts(ptr.add(layout.offset as usize), 64 * 48 * 4) };
            if allowed {
                assert!(after.iter().all(|byte| *byte == 0));
            } else {
                assert_eq!(after, before, "rejected snapshot wrote private pixels");
            }
            // Restore the client's colored buffer for subsequent renderer checks.
            // SAFETY: before was copied from this checked SHM layout, so the destination has
            // before.len() writable bytes and does not overlap the owned backup.
            unsafe {
                std::ptr::copy_nonoverlapping(
                    before.as_ptr(),
                    ptr.add(layout.offset as usize),
                    before.len(),
                );
            }
        })
        .unwrap();
    }

    #[test]
    fn outstanding_snapshots_release_both_limits_when_dropped() {
        let budget = Arc::default();
        let permits = (0..MAX_FRAMES)
            .map(|_| Permit::reserve(&budget, 4).unwrap())
            .collect::<Vec<_>>();
        assert!(Permit::reserve(&budget, 4).is_none());
        drop(permits);
        let full = Permit::reserve(&budget, MAX_BYTES).unwrap();
        assert!(Permit::reserve(&budget, 4).is_none());
        drop(full);
        assert!(Permit::reserve(&budget, MAX_BYTES).is_some());
        assert!(Permit::reserve(&budget, MAX_BYTES + 1).is_none());
        assert!(Permit::reserve(&budget, 0).is_none());
        assert_eq!(budget.lock().unwrap().bytes, 0);
        assert_eq!(budget.lock().unwrap().frames, 0);
    }
}
