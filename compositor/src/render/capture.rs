use super::*;

#[derive(Default)]
struct CaptureTarget(std::sync::Mutex<Option<GlesTexture>>);

pub(crate) fn capture_resize_snapshot(
    renderer: &mut GlesRenderer,
    window: &smithay::desktop::Window,
    geometry: Rectangle<i32, Logical>,
    scale: f64,
    remaining: usize,
) -> Result<Option<ResizeSnapshot>, GlesError> {
    // Called before on_commit_buffer_handler: renderer surface state still
    // owns the previous buffer during an actual resize handoff.
    render_window_texture(renderer, window, geometry, scale, remaining, None)
}

fn render_window_texture(
    renderer: &mut GlesRenderer,
    window: &smithay::desktop::Window,
    geometry: Rectangle<i32, Logical>,
    scale: f64,
    remaining: usize,
    reusable: Option<GlesTexture>,
) -> Result<Option<ResizeSnapshot>, GlesError> {
    let Some(toplevel) = window.toplevel() else {
        return Ok(None);
    };
    let size: Size<i32, Physical> = geometry.size.to_physical_precise_round(scale);
    let bytes = (size.w.max(0) as usize)
        .saturating_mul(size.h.max(0) as usize)
        .saturating_mul(4);
    if bytes == 0 || bytes > remaining {
        return Ok(None);
    }
    let content = render_elements_from_surface_tree::<
        GlesRenderer,
        WaylandSurfaceRenderElement<GlesRenderer>,
    >(
        renderer,
        toplevel.wl_surface(),
        Point::<i32, Logical>::from((-geometry.loc.x, -geometry.loc.y))
            .to_physical_precise_round(scale),
        scale,
        1.0,
        RenderElementKind::Unspecified,
    );
    if content.is_empty() {
        return Ok(None);
    }
    let mut texture = match reusable.filter(|texture| texture.size() == (size.w, size.h).into()) {
        Some(texture) => texture,
        None => Offscreen::<GlesTexture>::create_buffer(
            renderer,
            Fourcc::Abgr8888,
            (size.w, size.h).into(),
        )?,
    };
    {
        let mut target = renderer.bind(&mut texture)?;
        let mut frame = renderer.render(&mut target, size, Transform::Normal)?;
        let damage = Rectangle::from_size(size);
        frame.clear(Color32F::new(0.0, 0.0, 0.0, 0.0), &[damage])?;
        draw_render_elements(&mut frame, scale, &content, &[damage])?;
        let _ = frame.finish()?;
    }
    Ok(Some(ResizeSnapshot {
        texture,
        context: renderer.context_id().erased(),
        id: Id::new(),
        commit: CommitCounter::default(),
        elapsed: Duration::ZERO,
        last_tick: None,
        scale,
    }))
}

pub(crate) fn capture_window_buffer(
    renderer: &mut GlesRenderer,
    window: &smithay::desktop::Window,
    geometry: Rectangle<i32, Logical>,
    scale: f64,
) -> Result<crate::handlers::screenshot::CaptureBuffer, String> {
    let mut buffer = capture_window_frame(renderer, window, geometry, scale, None)?;
    convert_window_pixels(&mut buffer.pixels);
    Ok(buffer)
}

pub(crate) fn capture_window_frame(
    renderer: &mut GlesRenderer,
    window: &smithay::desktop::Window,
    geometry: Rectangle<i32, Logical>,
    scale: f64,
    cursor: Option<(&Ferese, Rectangle<i32, Logical>)>,
) -> Result<crate::handlers::screenshot::CaptureBuffer, String> {
    // One reusable render target per EGL context. Resize handoff snapshots keep
    // their own textures because they outlive the capture call.
    let cache = renderer
        .egl_context()
        .user_data()
        .get_or_insert_threadsafe(|| Arc::new(CaptureTarget::default()))
        .clone();
    let reusable = cache.0.lock().unwrap().take();
    let mut snapshot = render_window_texture(
        renderer,
        window,
        geometry,
        scale,
        128 * 1024 * 1024,
        reusable,
    )
    .map_err(|error| error.to_string())?
    .ok_or("Window has no capturable content")?;
    let size = snapshot.texture.size();
    if let Some((state, cursor_geometry)) = cursor {
        let elements = cursor_elements(state, renderer, cursor_geometry, scale);
        let mut target = renderer
            .bind(&mut snapshot.texture)
            .map_err(|e| e.to_string())?;
        let mut frame = renderer
            .render(&mut target, (size.w, size.h).into(), Transform::Normal)
            .map_err(|e| e.to_string())?;
        draw_render_elements(
            &mut frame,
            scale,
            &elements,
            &[Rectangle::from_size((size.w, size.h).into())],
        )
        .map_err(|e| e.to_string())?;
        let _ = frame.finish().map_err(|e| e.to_string())?;
    }
    let mapping = renderer
        .copy_texture(
            &snapshot.texture,
            Rectangle::from_size(size),
            Fourcc::Abgr8888,
        )
        .map_err(|error| error.to_string())?;
    if mapping.format() != Some(Fourcc::Abgr8888) {
        return Err("Unsupported window readback format".into());
    }
    let bytes = (size.w as usize)
        .checked_mul(size.h as usize)
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or("Window is too large")?;
    // Smithay maps its PBO here and can wait for the GPU. Nonblocking readback
    // needs a retained mapping plus an EGL-context-owned fence, polled before
    // mapping; moving this call to the conversion worker is not context-safe.
    let source = renderer
        .map_texture(&mapping)
        .map_err(|error| error.to_string())?;
    if source.len() < bytes {
        return Err("Incomplete window readback".into());
    }
    let pixels = source[..bytes].to_vec();
    *cache.0.lock().unwrap() = Some(snapshot.texture);

    Ok(crate::handlers::screenshot::CaptureBuffer {
        width: size.w,
        height: size.h,
        stride: size.w as usize * 4,
        pixels,
    })
}

/// Convert premultiplied ABGR readback into the capture protocol's straight ARGB.
pub(crate) fn convert_window_pixels(pixels: &mut [u8]) {
    for pixel in pixels.as_chunks_mut::<4>().0 {
        pixel.swap(0, 2);
        let alpha = pixel[3] as u16;
        for channel in &mut pixel[..3] {
            *channel = (*channel as u16 * 255 + alpha / 2)
                .checked_div(alpha)
                .unwrap_or(0)
                .min(255) as u8;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn readback_conversion_preserves_color_and_handles_zero_alpha() {
        let mut pixels = vec![10, 20, 30, 255, 32, 64, 128, 128, 9, 8, 7, 0, 1, 1, 1, 1];
        convert_window_pixels(&mut pixels);
        assert_eq!(
            pixels,
            [
                30, 20, 10, 255, 255, 128, 64, 128, 0, 0, 0, 0, 255, 255, 255, 1
            ]
        );
    }
}
