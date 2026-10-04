use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::{ExportMem, TextureMapping};
use smithay::output::Output;
use smithay::reexports::wayland_protocols_wlr::screencopy::v1::server::zwlr_screencopy_frame_v1::ZwlrScreencopyFrameV1;
use smithay::reexports::wayland_protocols_wlr::screencopy::v1::server::zwlr_screencopy_manager_v1::ZwlrScreencopyManagerV1;
use smithay::reexports::wayland_protocols_wlr::screencopy::v1::server::{
    zwlr_screencopy_frame_v1, zwlr_screencopy_manager_v1,
};
use smithay::reexports::wayland_server::protocol::wl_buffer::WlBuffer;
use smithay::reexports::wayland_server::protocol::wl_shm;
use smithay::reexports::wayland_server::{Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource};
use smithay::utils::{Buffer, Logical, Rectangle, Size};
use smithay::wayland::shm::with_buffer_contents_mut;

use super::screenshot::{CaptureBuffer, PartOutcome, PartSender};
use crate::Ferese;

const BYTES_PER_PIXEL: usize = 4;

#[derive(Debug)]
pub(crate) struct FrameData {
    pub(super) output: Option<Output>,
    pub(super) region: Rectangle<i32, Buffer>,
    pub(super) overlay_cursor: bool,
    pub(super) used: Mutex<bool>,
    pub(super) snapshot: Mutex<Option<super::window_capture::Snapshot>>,
}

#[derive(Debug)]
pub(crate) struct PendingScreencopy {
    sink: CaptureSink,
    pub(crate) output: Output,
    region: Rectangle<i32, Buffer>,
    overlay_cursor: bool,
    with_damage: bool,
}

#[derive(Debug)]
pub(crate) enum CaptureSink {
    Shm {
        frame: ZwlrScreencopyFrameV1,
        buffer: WlBuffer,
    },
    Owned {
        request: u64,
        part: usize,
        complete: PartSender,
        published: AtomicBool,
        permit: Option<super::screenshot::BudgetPermit>,
    },
}

impl CaptureSink {
    pub(crate) fn owned(request: u64, part: usize, complete: PartSender) -> Self {
        Self::Owned {
            request,
            part,
            complete,
            published: AtomicBool::new(false),
            permit: None,
        }
    }

    // Every part reaches exactly one terminal state: pixels or an error. A
    // cancelled part is still answered, otherwise its request would wait
    // forever for a part that can no longer arrive.
    pub(crate) fn fail(&self) {
        match self {
            CaptureSink::Shm { frame, .. } => frame.failed(),
            CaptureSink::Owned {
                request,
                part,
                complete,
                published,
                permit,
            } => {
                if !published.swap(true, Ordering::AcqRel) {
                    let _ = complete.send(PartOutcome {
                        request: *request,
                        part: *part,
                        result: Err("Screenshot part failed".to_string()),
                        permit: permit.clone(),
                    });
                }
            }
        }
    }

    // One-shot: a part publishes at most once, so a second completion can
    // never overwrite pixels that were already handed off.
    pub(crate) fn publish(&self, buffer: CaptureBuffer) -> bool {
        let CaptureSink::Owned {
            request,
            part,
            complete,
            published,
            permit,
        } = self
        else {
            return false;
        };
        if published.swap(true, Ordering::AcqRel) {
            return false;
        }
        complete
            .send(PartOutcome {
                request: *request,
                part: *part,
                result: Ok(buffer),
                permit: permit.clone(),
            })
            .is_ok()
    }
}

impl PendingScreencopy {
    pub(crate) fn with_permit(mut self, permit: Option<super::screenshot::BudgetPermit>) -> Self {
        if let CaptureSink::Owned { permit: stored, .. } = &mut self.sink {
            *stored = permit;
        }
        self
    }

    pub(crate) fn owned(
        request: u64,
        part: usize,
        complete: PartSender,
        output: Output,
        region: Rectangle<i32, Buffer>,
    ) -> Self {
        Self {
            sink: CaptureSink::owned(request, part, complete),
            output,
            region,
            // The compositor-owned path deliberately omits the cursor.
            overlay_cursor: false,
            with_damage: false,
        }
    }

    pub(crate) fn fail(&self) {
        self.sink.fail();
    }

    // The compositor-owned request this readback belongs to, if any. Used to
    // drop readbacks whose request was already abandoned.
    pub(crate) fn request_id(&self) -> Option<u64> {
        match &self.sink {
            CaptureSink::Shm { .. } => None,
            CaptureSink::Owned { request, .. } => Some(*request),
        }
    }
}

pub(crate) fn capture_allowed() -> bool {
    static ALLOWED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ALLOWED.get_or_init(|| capture_opted_in(std::env::var_os("FERESE_ENABLE_SCREENCOPY").as_deref()))
}

fn capture_opted_in(value: Option<&std::ffi::OsStr>) -> bool {
    value.is_some_and(|value| value == "1")
}

pub(crate) fn init_global(
    display: &DisplayHandle,
    loop_handle: &smithay::reexports::calloop::LoopHandle<'static, Ferese>,
) {
    if capture_allowed() {
        display.create_global::<Ferese, ZwlrScreencopyManagerV1, ()>(3, ());
        super::window_capture::init_global(display, loop_handle);
        tracing::info!("authorized screencopy is enabled for this session");
    }
}

impl GlobalDispatch<ZwlrScreencopyManagerV1, ()> for Ferese {
    fn bind(
        _state: &mut Self,
        _display: &DisplayHandle,
        _client: &Client,
        resource: New<ZwlrScreencopyManagerV1>,
        _global_data: &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        data_init.init(resource, ());
    }
}

impl Dispatch<ZwlrScreencopyManagerV1, ()> for Ferese {
    fn request(
        _state: &mut Self,
        _client: &Client,
        _manager: &ZwlrScreencopyManagerV1,
        request: zwlr_screencopy_manager_v1::Request,
        _data: &(),
        _display: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            zwlr_screencopy_manager_v1::Request::CaptureOutput {
                frame,
                overlay_cursor,
                output,
            } => {
                create_frame(data_init, frame, output, None, overlay_cursor != 0);
            }
            zwlr_screencopy_manager_v1::Request::CaptureOutputRegion {
                frame,
                overlay_cursor,
                output,
                x,
                y,
                width,
                height,
            } => {
                create_frame(
                    data_init,
                    frame,
                    output,
                    Some(Rectangle::new((x, y).into(), (width, height).into())),
                    overlay_cursor != 0,
                );
            }
            zwlr_screencopy_manager_v1::Request::Destroy => {}
            _ => unreachable!(),
        }
    }
}

impl Dispatch<ZwlrScreencopyFrameV1, Arc<FrameData>> for Ferese {
    fn request(
        state: &mut Self,
        _client: &Client,
        frame: &ZwlrScreencopyFrameV1,
        request: zwlr_screencopy_frame_v1::Request,
        data: &Arc<FrameData>,
        _display: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            zwlr_screencopy_frame_v1::Request::Copy { buffer } => {
                state.queue_screencopy(frame, data, buffer, false);
            }
            zwlr_screencopy_frame_v1::Request::CopyWithDamage { buffer } => {
                state.queue_screencopy(frame, data, buffer, true);
            }
            zwlr_screencopy_frame_v1::Request::Destroy => {
                data.snapshot.lock().unwrap().take();
            }
            _ => unreachable!(),
        }
    }
}

impl Ferese {
    fn queue_screencopy(
        &mut self,
        frame: &ZwlrScreencopyFrameV1,
        data: &Arc<FrameData>,
        buffer: WlBuffer,
        with_damage: bool,
    ) {
        if data.snapshot.lock().unwrap().is_some() {
            super::window_capture::copy_snapshot(self, frame, data, &buffer);
            return;
        }
        let mut used = data.used.lock().unwrap();
        if self.session_lock.active() {
            frame.failed();
            return;
        }
        if *used {
            frame.post_error(
                zwlr_screencopy_frame_v1::Error::AlreadyUsed,
                "screencopy frame has already been used",
            );
            return;
        }
        *used = true;

        if !valid_shm_buffer(&buffer, data.region.size) {
            frame.post_error(
                zwlr_screencopy_frame_v1::Error::InvalidBuffer,
                "screencopy requires the advertised ARGB8888 SHM buffer",
            );
            return;
        }

        let Some(output) = data.output.clone() else {
            frame.failed();
            return;
        };

        self.pending_screencopies.push(PendingScreencopy {
            sink: CaptureSink::Shm {
                frame: frame.clone(),
                buffer,
            },
            output: output.clone(),
            region: data.region,
            overlay_cursor: data.overlay_cursor,
            with_damage,
        });
        crate::backends::direct::render_on(self, &[output]);
    }

    pub(crate) fn process_screencopies<R>(
        &mut self,
        renderer: &mut R,
        framebuffer: &R::Framebuffer<'_>,
        output: &Output,
        overlay_cursor: bool,
    ) -> bool
    where
        R: ExportMem,
    {
        let mut remaining = Vec::new();
        let mut framebuffer_binding_changed = false;
        let captures = std::mem::take(&mut self.pending_screencopies);

        for capture in captures {
            if &capture.output != output || !cursor_overlay_matches(capture.overlay_cursor, overlay_cursor) {
                remaining.push(capture);
                continue;
            }
            if let CaptureSink::Shm { frame, buffer } = &capture.sink
                && (!frame.is_alive() || !buffer.is_alive())
            {
                continue;
            }

            framebuffer_binding_changed = true;
            if complete_capture(renderer, framebuffer, &capture, self.start_time.elapsed()).is_err() {
                capture.fail();
            }
        }

        self.pending_screencopies = remaining;
        framebuffer_binding_changed
    }

    pub(crate) fn has_pending_screencopy(&self, output: &Output, overlay_cursor: bool) -> bool {
        self.pending_screencopies
            .iter()
            .any(|capture| capture.output == *output && cursor_overlay_matches(capture.overlay_cursor, overlay_cursor))
    }

    pub(crate) fn fail_screencopies(&mut self, output: &Output, overlay_cursor: bool) {
        let mut remaining = Vec::new();
        for capture in std::mem::take(&mut self.pending_screencopies) {
            if capture.output == *output && cursor_overlay_matches(capture.overlay_cursor, overlay_cursor) {
                capture.sink.fail();
            } else {
                remaining.push(capture);
            }
        }

        self.pending_screencopies = remaining;
    }
}

fn cursor_overlay_matches(requested: bool, rendered: bool) -> bool {
    requested == rendered
}

fn create_frame(
    data_init: &mut DataInit<'_, Ferese>,
    frame: New<ZwlrScreencopyFrameV1>,
    wl_output: smithay::reexports::wayland_server::protocol::wl_output::WlOutput,
    requested: Option<Rectangle<i32, Logical>>,
    overlay_cursor: bool,
) {
    let Some(output) = Output::from_resource(&wl_output) else {
        let frame = data_init.init(
            frame,
            Arc::new(FrameData {
                output: None,
                region: Rectangle::from_size((1, 1).into()),
                overlay_cursor,
                used: Mutex::new(true),
                snapshot: Mutex::new(None),
            }),
        );
        frame.failed();
        return;
    };
    let Some(region) = capture_region(&output, requested) else {
        let frame = data_init.init(
            frame,
            Arc::new(FrameData {
                output: Some(output),
                region: Rectangle::from_size((1, 1).into()),
                overlay_cursor,
                used: Mutex::new(true),
                snapshot: Mutex::new(None),
            }),
        );
        frame.failed();
        return;
    };

    let size = region.size;
    let data = Arc::new(FrameData {
        output: Some(output),
        region,
        overlay_cursor,
        used: Mutex::new(false),
        snapshot: Mutex::new(None),
    });
    let frame = data_init.init(frame, data);

    frame.buffer(
        wl_shm::Format::Argb8888,
        size.w as u32,
        size.h as u32,
        (size.w as usize * BYTES_PER_PIXEL) as u32,
    );
    if frame.version() >= 3 {
        frame.buffer_done();
    }
}

fn capture_region(output: &Output, requested: Option<Rectangle<i32, Logical>>) -> Option<Rectangle<i32, Buffer>> {
    let mode = output.current_mode()?;
    let scale = output.current_scale().fractional_scale();
    let transform = output.current_transform();
    capture_region_for_geometry(mode.size, scale, transform, requested)
}

pub(crate) fn output_logical_size(
    mode_size: Size<i32, smithay::utils::Physical>,
    scale: f64,
    transform: smithay::utils::Transform,
) -> Size<i32, Logical> {
    transform
        .transform_size(mode_size)
        .to_f64()
        .to_logical(scale)
        .to_i32_round()
}

pub(crate) fn capture_region_for_geometry(
    mode_size: Size<i32, smithay::utils::Physical>,
    scale: f64,
    transform: smithay::utils::Transform,
    requested: Option<Rectangle<i32, Logical>>,
) -> Option<Rectangle<i32, Buffer>> {
    let logical_size = output_logical_size(mode_size, scale, transform);
    let output_region = Rectangle::from_size(logical_size);
    let logical_region = requested.unwrap_or(output_region).intersection(output_region)?;

    if logical_region.size.w <= 0 || logical_region.size.h <= 0 {
        return None;
    }

    Some(
        logical_region
            .to_f64()
            .to_buffer(scale, transform, &logical_size.to_f64())
            .to_i32_round(),
    )
}

pub(super) fn valid_shm_buffer(buffer: &WlBuffer, expected: Size<i32, Buffer>) -> bool {
    with_buffer_contents_mut(buffer, |_, length, data| {
        let stride = expected.w.checked_mul(BYTES_PER_PIXEL as i32);
        let required = data
            .stride
            .checked_mul(data.height)
            .and_then(|bytes| data.offset.checked_add(bytes));

        data.format == wl_shm::Format::Argb8888
            && data.width == expected.w
            && data.height == expected.h
            && Some(data.stride) == stride
            && required.is_some_and(|required| required >= 0 && required as usize <= length)
    })
    .unwrap_or(false)
}

fn complete_capture<R>(
    renderer: &mut R,
    framebuffer: &R::Framebuffer<'_>,
    capture: &PendingScreencopy,
    timestamp: std::time::Duration,
) -> Result<(), ()>
where
    R: ExportMem,
{
    let mapping = renderer
        .copy_framebuffer(framebuffer, capture.region, Fourcc::Argb8888)
        .map_err(|error| {
            tracing::warn!(?error, "failed to copy output framebuffer");
        })?;
    if mapping.format() != Fourcc::Argb8888 {
        return Err(());
    }
    let source = renderer.map_texture(&mapping).map_err(|error| {
        tracing::warn!(?error, "failed to map output framebuffer copy");
    })?;
    let expected = capture.region.size;
    let stride = (expected.w as usize).checked_mul(BYTES_PER_PIXEL).ok_or(())?;
    let bytes = stride.checked_mul(expected.h as usize).ok_or(())?;
    if source.len() < bytes {
        return Err(());
    }
    // Smithay's output projection already accounts for OpenGL's Y axis, so these
    // rows must not be flipped vertically again. They are in output-buffer order
    // including the output transform, which the conversion stage normalizes.
    let copied = match &capture.sink {
        CaptureSink::Shm { buffer, .. } => {
            with_buffer_contents_mut(buffer, |destination, length, data| {
                let Ok(offset) = usize::try_from(data.offset) else {
                    return false;
                };
                if offset > length || bytes > length - offset {
                    return false;
                }

                // SAFETY: the mapped slice is at least `bytes` long, and the
                // checks above establish `bytes` writable bytes at `offset`
                // within the shm buffer. The regions cannot overlap: one is
                // renderer-owned and the other client-owned.
                unsafe {
                    std::ptr::copy_nonoverlapping(source.as_ptr(), destination.add(offset), bytes);
                }
                true
            })
            .unwrap_or(false)
        }
        CaptureSink::Owned { .. } => {
            let mut pixels = Vec::new();
            if pixels.try_reserve_exact(bytes).is_err() {
                false
            } else {
                pixels.extend_from_slice(&source[..bytes]);
                capture.sink.publish(CaptureBuffer {
                    width: expected.w,
                    height: expected.h,
                    stride,
                    pixels,
                })
            }
        }
    };

    if !copied {
        return Err(());
    }

    if let CaptureSink::Shm { frame, .. } = &capture.sink {
        if capture.with_damage {
            frame.damage(0, 0, expected.w as u32, expected.h as u32);
        }
        // TextureMapping::flipped describes importing the mapping as a texture;
        // using it here would make clients flip the already-correct output rows.
        frame.flags(zwlr_screencopy_frame_v1::Flags::empty());

        let seconds = timestamp.as_secs();
        frame.ready((seconds >> 32) as u32, seconds as u32, timestamp.subsec_nanos());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use smithay::utils::{Physical, Transform};

    use super::*;

    #[test]
    fn capture_is_off_unless_the_opt_in_is_exactly_one() {
        use std::ffi::OsStr;

        assert!(!capture_opted_in(None));
        assert!(!capture_opted_in(Some(OsStr::new("0"))));
        assert!(!capture_opted_in(Some(OsStr::new(""))));
        assert!(!capture_opted_in(Some(OsStr::new("true"))));
        assert!(!capture_opted_in(Some(OsStr::new("01"))));
        assert!(!capture_opted_in(Some(OsStr::new(" 1"))));
        assert!(capture_opted_in(Some(OsStr::new("1"))));
    }

    #[test]
    fn capture_region_clips_then_scales_to_buffer_coordinates() {
        let mode = Size::<i32, Physical>::from((1_920, 1_080));
        let requested = Rectangle::new((-10, 20).into(), (110, 50).into());

        let region = capture_region_for_geometry(mode, 2.0, Transform::Normal, Some(requested)).unwrap();

        assert_eq!(region, Rectangle::new((0, 40).into(), (200, 100).into()));
    }

    #[test]
    fn capture_region_rejects_regions_outside_the_output() {
        let mode = Size::<i32, Physical>::from((1_920, 1_080));
        let requested = Rectangle::new((1_000, 600).into(), (100, 100).into());

        assert!(capture_region_for_geometry(mode, 2.0, Transform::Normal, Some(requested),).is_none());
    }

    #[test]
    fn cursor_overlay_requests_use_only_the_matching_render_pass() {
        assert!(cursor_overlay_matches(false, false));
        assert!(cursor_overlay_matches(true, true));
        assert!(!cursor_overlay_matches(false, true));
        assert!(!cursor_overlay_matches(true, false));
    }
}
