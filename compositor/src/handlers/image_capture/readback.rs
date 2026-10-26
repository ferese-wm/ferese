use smithay::reexports::wayland_protocols::ext::image_copy_capture::v1::server::ext_image_copy_capture_frame_v1::FailureReason;
use smithay::reexports::wayland_server::Resource;
use smithay::reexports::wayland_server::protocol::wl_shm;
use smithay::utils::{Buffer, IsAlive, Size, Transform};
use smithay::wayland::image_copy_capture::{Frame, SessionRef};
use smithay::wayland::shm::with_buffer_contents_mut;

use crate::Ferese;

impl Ferese {
    pub(super) fn defer_stream_readback(
        &mut self,
        frame: Frame,
        session: SessionRef,
        generation: u64,
        readback: super::super::readback::Readback,
        size: Size<i32, Buffer>,
    ) {
        let (valid, complete) = self.stream_readback_callbacks(frame, session, generation, size);
        self.queue_readback(readback, valid, complete);
    }

    pub(super) fn stream_readback_callbacks(
        &self,
        frame: Frame,
        session: SessionRef,
        generation: u64,
        size: Size<i32, Buffer>,
    ) -> (
        super::super::readback::Valid,
        super::super::readback::Pixels,
    ) {
        let guard = session.clone();
        let reference = (*frame).clone();
        let buffer = frame.buffer();
        let live_buffer = buffer.clone();
        let geometry = self.capture_source_geometry(&session.source());
        let configuration = geometry.as_ref().map(|(_, output)| {
            (
                output.current_mode(),
                output.current_transform(),
                output.current_scale().fractional_scale(),
            )
        });
        (
            Box::new(move |state| {
                guard.alive()
                    && super::current_submission(&guard, generation)
                    && reference.alive()
                    && !reference.has_failed()
                    && live_buffer.is_alive()
                    && super::source_active(&guard.source())
                    && state.capture_source_geometry(&guard.source()) == geometry
                    && geometry.as_ref().is_some_and(|(_, output)| {
                        Some((
                            output.current_mode(),
                            output.current_transform(),
                            output.current_scale().fractional_scale(),
                        )) == configuration
                    })
                    && guard
                        .current_constraints()
                        .is_some_and(|constraints| constraints.size == size)
            }),
            Box::new(move |state, pixels| {
                let copied = pixels.and_then(|pixels| {
                    let row = usize::try_from(size.w)
                        .ok()
                        .and_then(|width| width.checked_mul(4))
                        .ok_or("Invalid capture dimensions")?;
                    let bytes = usize::try_from(size.h)
                        .ok()
                        .and_then(|height| height.checked_mul(row))
                        .ok_or("Invalid capture dimensions")?;
                    if pixels.len() < bytes {
                        return Err("Incomplete stream readback".into());
                    }
                    with_buffer_contents_mut(&buffer, |destination, length, data| {
                        let valid = data.width == size.w
                            && data.height == size.h
                            && data.format == wl_shm::Format::Xrgb8888
                            && data.stride >= size.w * 4
                            && data.offset >= 0
                            && usize::try_from(
                                i64::from(data.offset)
                                    + i64::from(data.stride) * i64::from(data.height),
                            )
                            .is_ok_and(|end| end <= length);
                        if !valid {
                            return false;
                        }
                        for (y, source) in pixels[..bytes].chunks_exact(row).enumerate() {
                            // SAFETY: offset, dimensions, stride and mapped bounds were
                            // rechecked above. Client SHM cannot alias the renderer PBO.
                            unsafe {
                                std::ptr::copy_nonoverlapping(
                                    source.as_ptr(),
                                    destination
                                        .add(data.offset as usize + y * data.stride as usize),
                                    row,
                                );
                            }
                        }
                        true
                    })
                    .map_err(|error| format!("{error:?}"))?
                    .then_some(())
                    .ok_or_else(|| "Invalid stream SHM buffer".into())
                });
                match copied {
                    Ok(()) => frame.success(
                        Transform::Normal,
                        None,
                        smithay::utils::Clock::<smithay::utils::Monotonic>::new().now(),
                    ),
                    Err(error) => {
                        tracing::debug!(%error, "stream readback failed");
                        frame.fail(FailureReason::Unknown);
                        state.reset_stream_submission(&session, generation);
                    }
                }
            }),
        )
    }
}
