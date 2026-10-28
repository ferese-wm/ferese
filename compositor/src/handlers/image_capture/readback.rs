use smithay::reexports::wayland_protocols::ext::image_copy_capture::v1::server::ext_image_copy_capture_frame_v1::FailureReason;
use smithay::reexports::wayland_server::Resource;
use smithay::reexports::wayland_server::protocol::wl_shm;
use smithay::utils::{Buffer, IsAlive, Size, Transform};
use smithay::wayland::image_copy_capture::{Frame, ImageCopyCaptureHandler, SessionRef};
use smithay::wayland::shm::with_buffer_contents_mut;

use crate::Ferese;

impl Ferese {
    pub(super) fn stream_failure_reason(
        &mut self,
        session: &SessionRef,
        submitted_size: Size<i32, Buffer>,
    ) -> FailureReason {
        let source = session.source();
        let Some((size, _)) = self
            .capture_source_geometry(&source)
            .filter(|_| session.alive())
        else {
            return FailureReason::Stopped;
        };
        let constraints_size = session
            .current_constraints()
            .map(|constraints| constraints.size);
        if size == submitted_size && constraints_size == Some(submitted_size) {
            // Same-size source invalidation and real transfer errors are not
            // buffer mismatches. The protocol permits a bounded Unknown retry.
            return FailureReason::Unknown;
        }
        if constraints_size != Some(size) {
            // A completion can observe a resize before the normal post-dispatch
            // constraint refresh. Publish the new constraints before failed so
            // the client cannot retry with the old dimensions indefinitely.
            let Some(constraints) = self.capture_constraints(&source) else {
                return FailureReason::Stopped;
            };
            let reserved: u64 = self
                .image_capture
                .active
                .iter()
                .filter(|active| active.session.as_ref() != *session)
                .map(|active| super::stream_bytes(active.size))
                .sum();
            if super::stream_bytes(constraints.size)
                > super::MAX_STREAM_BYTES.saturating_sub(reserved)
            {
                tracing::debug!(
                    width = constraints.size.w,
                    height = constraints.size.h,
                    requested_bytes = super::stream_bytes(constraints.size),
                    reserved_bytes = reserved,
                    limit_bytes = super::MAX_STREAM_BYTES,
                    "capture completion resize exceeds reservation limit"
                );
                self.cancel_session_fences(session.clone());
                self.image_capture
                    .active
                    .retain(|active| active.session.as_ref() != *session);
                return FailureReason::Stopped;
            }
            let Some(active) = self
                .image_capture
                .active
                .iter_mut()
                .find(|active| active.session.as_ref() == *session)
            else {
                return FailureReason::Stopped;
            };
            active.size = constraints.size;
            active.dma_node = constraints.dma.as_ref().map(|dma| dma.node);
            active.revision = None;
            active.window_stamp = None;
            session.update_constraints(constraints);
        }
        FailureReason::BufferConstraints
    }

    pub(super) fn defer_stream_readback(
        &mut self,
        frame: Frame,
        session: SessionRef,
        generation: u64,
        readback: super::super::readback::Readback,
        size: Size<i32, Buffer>,
    ) {
        let (valid, consume, complete) =
            self.stream_readback_mapped_callbacks(frame, session, generation, size);
        self.queue_mapped_readback(readback, valid, consume, complete);
    }

    #[cfg(test)]
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
        let (valid, consume, complete) =
            self.stream_readback_mapped_callbacks(frame, session, generation, size);
        (
            valid,
            Box::new(move |state, pixels| {
                complete(state, pixels.and_then(|pixels| consume(&pixels)))
            }),
        )
    }

    pub(super) fn stream_readback_mapped_callbacks(
        &self,
        frame: Frame,
        session: SessionRef,
        generation: u64,
        size: Size<i32, Buffer>,
    ) -> (
        super::super::readback::Valid,
        super::super::readback::Mapped<()>,
        super::super::readback::Completion,
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
            Box::new(move |pixels| {
                let row = usize::try_from(size.w)
                    .ok()
                    .and_then(|width| width.checked_mul(4))
                    .ok_or("Invalid capture dimensions")?;
                let bytes = usize::try_from(size.h)
                    .ok()
                    .and_then(|height| height.checked_mul(row))
                    .ok_or("Invalid capture dimensions")?;
                if bytes == 0 || pixels.len() < bytes {
                    return Err("Incomplete stream readback".into());
                }
                with_buffer_contents_mut(&buffer, |destination, length, data| {
                    let valid = data.width == size.w
                        && data.height == size.h
                        && data.format == wl_shm::Format::Xrgb8888
                        && i64::from(data.stride) >= i64::from(size.w) * 4
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
                                destination.add(data.offset as usize + y * data.stride as usize),
                                row,
                            );
                        }
                    }
                    true
                })
                .map_err(|error| format!("{error:?}"))?
                .then_some(())
                .ok_or_else(|| "Invalid stream SHM buffer".into())
            }),
            Box::new(move |state, copied| match copied {
                Ok(()) => {
                    super::complete_submission(&session, generation);
                    frame.success(
                        Transform::Normal,
                        None,
                        smithay::utils::Clock::<smithay::utils::Monotonic>::new().now(),
                    );
                }
                Err(error) => {
                    tracing::debug!(%error, "stream readback failed");
                    state.reset_stream_submission(&session, generation);
                    let reason = state.stream_failure_reason(&session, size);
                    frame.fail(reason);
                }
            }),
        )
    }
}
