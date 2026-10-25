use glam::{Mat3, Vec2};
use smithay::backend::renderer::gles::ffi;

use super::{Buffer, GlesError, GlesFrame, GlesTexProgram, GlesTexture, Size, Texture, Uniform};

pub(super) fn size(rect: [i32; 4], radius: f32) -> Size<i32, Buffer> {
    let divisor = (radius / 12.0).floor().clamp(1.0, 4.0) as i32;
    (
        (rect[2].max(1) + divisor - 1) / divisor,
        (rect[3].max(1) + divisor - 1) / divisor,
    )
        .into()
}

// These textures belong to one renderer context. Both passes stay on the GPU;
// an unchanged backdrop needs only the final masked texture draw.
pub(super) fn refresh(
    frame: &mut GlesFrame<'_, '_>,
    texture: &GlesTexture,
    scratch: &GlesTexture,
    filter: &GlesTexProgram,
    rect: [i32; 4],
    radius: f32,
) -> Result<(), GlesError> {
    let size = texture.size();
    let mut read = 0;
    let mut draw = 0;
    let mut read_buffer = 0;
    let mut viewport = [0; 4];
    let mut active_texture = 0;
    let mut binding = 0;
    let mut framebuffer = 0;
    // SAFETY: the frame owns the current context. All queried scalars and arrays
    // are live, and both textures were allocated by this renderer.
    let scissor = frame.with_context(|gl| unsafe {
        gl.GetIntegerv(ffi::READ_FRAMEBUFFER_BINDING, &mut read);
        gl.GetIntegerv(ffi::DRAW_FRAMEBUFFER_BINDING, &mut draw);
        gl.GetIntegerv(ffi::READ_BUFFER, &mut read_buffer);
        gl.GetIntegerv(ffi::VIEWPORT, viewport.as_mut_ptr());
        gl.GetIntegerv(ffi::ACTIVE_TEXTURE, &mut active_texture);
        gl.ActiveTexture(ffi::TEXTURE0);
        gl.GetIntegerv(ffi::TEXTURE_BINDING_2D, &mut binding);
        for target in [texture, scratch] {
            gl.BindTexture(ffi::TEXTURE_2D, target.tex_id());
            gl.TexParameteri(
                ffi::TEXTURE_2D,
                ffi::TEXTURE_WRAP_S,
                ffi::CLAMP_TO_EDGE as i32,
            );
            gl.TexParameteri(
                ffi::TEXTURE_2D,
                ffi::TEXTURE_WRAP_T,
                ffi::CLAMP_TO_EDGE as i32,
            );
        }
        let scissor = gl.IsEnabled(ffi::SCISSOR_TEST) == ffi::TRUE;
        gl.Disable(ffi::SCISSOR_TEST);
        gl.GenFramebuffers(1, &mut framebuffer);
        gl.BindFramebuffer(ffi::DRAW_FRAMEBUFFER, framebuffer);
        gl.Viewport(0, 0, size.w, size.h);
        scissor
    })?;

    let result = (|| {
        // SAFETY: the live cache texture is attached to a temporary framebuffer.
        // The original frame remains bound for reading until capture completes.
        let captured = frame.with_context(|gl| unsafe {
            gl.FramebufferTexture2D(
                ffi::DRAW_FRAMEBUFFER,
                ffi::COLOR_ATTACHMENT0,
                ffi::TEXTURE_2D,
                texture.tex_id(),
                0,
            );
            if gl.CheckFramebufferStatus(ffi::DRAW_FRAMEBUFFER) != ffi::FRAMEBUFFER_COMPLETE {
                return false;
            }
            gl.ReadBuffer(if read == 0 {
                ffi::BACK
            } else {
                ffi::COLOR_ATTACHMENT0
            });
            gl.GetError();
            gl.BlitFramebuffer(
                rect[0],
                rect[1],
                rect[0] + rect[2],
                rect[1] + rect[3],
                0,
                0,
                size.w,
                size.h,
                ffi::COLOR_BUFFER_BIT,
                ffi::LINEAR,
            );
            gl.GetError() == ffi::NO_ERROR
        })?;
        if !captured {
            return Err(GlesError::FramebufferBindingError);
        }

        // Cancel the output projection: the cache uses framebuffer coordinates,
        // including on rotated and flipped outputs.
        let matrix = Mat3::from_cols_array(frame.projection()).inverse()
            * Mat3::from_translation(Vec2::splat(-1.0))
            * Mat3::from_scale(Vec2::splat(2.0));
        for (source, target, step) in [
            (texture, scratch, [radius / (12.0 * rect[2] as f32), 0.0]),
            (scratch, texture, [0.0, radius / (12.0 * rect[3] as f32)]),
        ] {
            // SAFETY: the framebuffer and target texture are live in this
            // context. Source and destination are distinct on both passes.
            frame.with_context(|gl| unsafe {
                gl.FramebufferTexture2D(
                    ffi::DRAW_FRAMEBUFFER,
                    ffi::COLOR_ATTACHMENT0,
                    ffi::TEXTURE_2D,
                    target.tex_id(),
                    0,
                );
            })?;
            frame.render_texture(
                source,
                Mat3::IDENTITY,
                matrix,
                None::<[f32; 4]>,
                1.0,
                Some(filter),
                &[
                    Uniform::new("cache_size", [size.w as f32, size.h as f32]),
                    Uniform::new("sample_step", step),
                ],
            )?;
        }
        Ok(())
    })();

    // Restore the original frame even when a capture or filtering pass fails.
    // SAFETY: saved bindings belong to the still-live frame; only our temporary
    // framebuffer is deleted. No saved GL object is destroyed here.
    frame.with_context(|gl| unsafe {
        gl.BindFramebuffer(ffi::READ_FRAMEBUFFER, read as u32);
        gl.ReadBuffer(read_buffer as u32);
        gl.BindFramebuffer(ffi::DRAW_FRAMEBUFFER, draw as u32);
        gl.Viewport(viewport[0], viewport[1], viewport[2], viewport[3]);
        if scissor {
            gl.Enable(ffi::SCISSOR_TEST);
        }
        gl.ActiveTexture(ffi::TEXTURE0);
        gl.BindTexture(ffi::TEXTURE_2D, binding as u32);
        gl.ActiveTexture(active_texture as u32);
        gl.DeleteFramebuffers(1, &framebuffer);
    })?;
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_resolution_tracks_radius_and_rounds_outward() {
        assert_eq!(size([0, 0, 3840, 2160], 48.6), (960, 540).into());
        assert_eq!(size([0, 0, 3839, 2159], 48.6), (960, 540).into());
        assert_eq!(size([0, 0, 101, 99], 27.0), (51, 50).into());
        assert_eq!(size([0, 0, 101, 99], 3.0), (101, 99).into());
        assert_eq!(size([0, 0, 1, 1], 200.0), (1, 1).into());
    }
}
