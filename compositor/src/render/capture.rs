use super::{
    Arc, Bind, Color32F, CommitCounter, Duration, ExportMem, Ferese, Fourcc, Frame, GlesError,
    GlesRenderer, GlesTexture, Id, Logical, Offscreen, Physical, Point, Rectangle,
    RenderElementKind, Renderer, ResizeSnapshot, Size, Texture, Transform,
    WaylandSurfaceRenderElement, cursor_elements, draw_render_elements,
    render_elements_from_surface_tree,
};

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
    let reusable = cache
        .0
        .lock()
        .expect("the reusable capture texture cache must remain consistent across callbacks")
        .take();
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
    *cache
        .0
        .lock()
        .expect("the reusable capture texture cache must remain consistent across callbacks") =
        Some(snapshot.texture);

    Ok(crate::handlers::screenshot::CaptureBuffer {
        width: size.w,
        height: size.h,
        stride: size.w as usize * 4,
        pixels,
    })
}

/// Convert premultiplied ABGR readback into the capture protocol's straight ARGB.
pub(crate) fn convert_window_pixels(pixels: &mut [u8]) {
    // Compile the rounded, saturated conversion into 64 KiB of read-only data.
    // Capture threads share this table without allocating or dividing per pixel.
    static STRAIGHT_CHANNELS: [[u8; 256]; 256] = {
        let mut values = [[0; 256]; 256];
        let mut alpha = 1;
        while alpha < 256 {
            let mut channel = 0;
            while channel < 256 {
                let straight = (channel * 255 + alpha / 2) / alpha;
                values[alpha][channel] = if straight > 255 { 255 } else { straight as u8 };
                channel += 1;
            }
            alpha += 1;
        }
        values
    };
    for pixel in pixels.as_chunks_mut::<4>().0 {
        pixel.swap(0, 2);
        let alpha = pixel[3];
        // Opaque content needs only the channel swap; avoid three divisions
        // per pixel for the usual window-sharing frame.
        if alpha == 255 {
            continue;
        }
        if alpha == 0 {
            pixel[..3].fill(0);
            continue;
        }
        let channels = &STRAIGHT_CHANNELS[usize::from(alpha)];
        for channel in &mut pixel[..3] {
            *channel = channels[usize::from(*channel)];
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "manual release benchmark for window-capture pixel conversion"]
    fn profile_window_conversion() {
        use std::{
            hint::black_box,
            time::{Duration, Instant},
        };

        for (width, height) in [(1920usize, 1080usize), (3840, 2160)] {
            for workload in ["opaque", "edges", "translucent", "mixed"] {
                let mut source = vec![0u8; width * height * 4];
                for (i, pixel) in source.chunks_exact_mut(4).enumerate() {
                    let alpha: u8 = if workload == "mixed" {
                        i.wrapping_mul(37) as u8
                    } else if workload == "translucent"
                        || (workload == "edges"
                            && (i % width < 16
                                || i % width >= width - 16
                                || i / width < 16
                                || i / width >= height - 16))
                    {
                        128
                    } else {
                        255
                    };
                    let color = (i as u32).wrapping_mul(1664525).wrapping_add(1013904223);
                    let premultiplied =
                        |shift: u32| (((color >> shift) & 255) * u32::from(alpha) / 255) as u8;
                    pixel.copy_from_slice(&[
                        premultiplied(0),
                        premultiplied(8),
                        premultiplied(16),
                        alpha,
                    ]);
                }
                let mut pixels = source.clone();
                for run in 0..=3 {
                    let mut elapsed = Duration::ZERO;
                    for _ in 0..3 {
                        pixels.copy_from_slice(&source);
                        let started = Instant::now();
                        convert_window_pixels(black_box(&mut pixels));
                        elapsed += started.elapsed();
                        black_box(&pixels);
                    }
                    if run != 0 {
                        eprintln!(
                            "window_conversion_benchmark width={width} workload={workload} run={run} frames=3 elapsed_us={}",
                            elapsed.as_micros()
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn conversion_matches_straight_argb_for_every_alpha_and_channel_value() {
        let mut pixels = Vec::new();
        let mut expected = Vec::new();
        for alpha in 0..=255u8 {
            for channel in 0..=255u8 {
                let source = [channel, 255 - channel, channel ^ alpha, alpha];
                pixels.extend_from_slice(&source);
                let straight = |value: u8| {
                    if alpha == 0 {
                        0
                    } else {
                        ((u32::from(value) * 255 + u32::from(alpha) / 2) / u32::from(alpha))
                            .min(255) as u8
                    }
                };
                expected.extend_from_slice(&[
                    straight(source[2]),
                    straight(source[1]),
                    straight(source[0]),
                    alpha,
                ]);
            }
        }
        // Incomplete trailing pixels are outside the conversion's input domain.
        pixels.extend_from_slice(&[17, 29, 43]);
        expected.extend_from_slice(&[17, 29, 43]);
        convert_window_pixels(&mut pixels);
        assert_eq!(pixels, expected);
    }

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

#[derive(Default)]
struct StreamResources {
    textures: std::collections::VecDeque<GlesTexture>,
    bytes: usize,
    straight: Option<smithay::backend::renderer::gles::GlesTexProgram>,
    copy: Option<smithay::backend::renderer::gles::GlesTexProgram>,
}

impl StreamResources {
    fn take_texture(&mut self, size: Size<i32, smithay::utils::Buffer>) -> Option<GlesTexture> {
        let index = self
            .textures
            .iter()
            .position(|texture| texture.size() == size)?;
        let texture = self.textures.remove(index)?;
        self.bytes -= texture.size().w as usize * texture.size().h as usize * 4;
        Some(texture)
    }

    fn put_texture(&mut self, texture: GlesTexture) {
        let bytes = texture.size().w as usize * texture.size().h as usize * 4;
        // Scratch targets are shared by size across sources. Keep alternating
        // window sizes from reallocating every frame without retaining an
        // unbounded collection of old source sizes.
        const MAX_BYTES: usize = 128 * 1024 * 1024;
        if bytes > MAX_BYTES {
            return;
        }
        while self.textures.len() >= 8 || self.bytes + bytes > MAX_BYTES {
            let Some(old) = self.textures.pop_front() else {
                break;
            };
            self.bytes -= old.size().w as usize * old.size().h as usize * 4;
        }
        self.bytes += bytes;
        self.textures.push_back(texture);
    }
}

#[derive(Default)]
struct StreamCache(std::sync::Mutex<StreamResources>);

const STREAM_CAPTURE_SHADER: &str = r#"
//_DEFINES
#ifdef EXTERNAL
#extension GL_OES_EGL_image_external : require
#endif
precision highp float;
#ifdef EXTERNAL
uniform samplerExternalOES tex;
#else
uniform sampler2D tex;
#endif
varying vec2 v_coords;
uniform float alpha;
uniform float y_offset;
void main() {
    // The renderer negates Y for inverted textures; translate that origin
    // back into [0, 1] so full-buffer captures sample both rows correctly.
    vec4 pixel = texture2D(tex, v_coords + vec2(0.0, y_offset));
#ifdef NO_ALPHA
    pixel.a = 1.0;
#endif
#ifdef STRAIGHT
    float a = floor(pixel.a * 255.0 + 0.5);
    vec3 numerator = floor(pixel.rgb * 255.0 + 0.5) * 255.0 + floor(a * 0.5);
    vec3 quotient = floor(numerator / max(a, 1.0));
    vec3 remainder = numerator - quotient * max(a, 1.0);
    quotient += step(vec3(max(a, 1.0)), remainder);
    quotient -= vec3(lessThan(remainder, vec3(0.0)));
    gl_FragColor = vec4(a == 0.0 ? vec3(0.0) : clamp(quotient, 0.0, 255.0) / 255.0, 1.0);
#else
    gl_FragColor = vec4(pixel.rgb, 1.0);
#endif
}
"#;

pub(crate) fn capture_stream_window(
    renderer: &mut GlesRenderer,
    state: &Ferese,
    info: &crate::handlers::window_capture::WindowInfo,
    cursor: Option<Rectangle<i32, Logical>>,
    buffer: &smithay::reexports::wayland_server::protocol::wl_buffer::WlBuffer,
) -> Result<smithay::backend::renderer::sync::SyncPoint, String> {
    let cache = renderer
        .egl_context()
        .user_data()
        .get_or_insert_threadsafe(|| Arc::new(StreamCache::default()))
        .clone();
    let reusable = cache
        .0
        .lock()
        .expect("stream texture cache mutex poisoned")
        .take_texture({
            let size = info.geometry.size.to_physical_precise_round(info.scale);
            (size.w, size.h).into()
        });
    let mut snapshot = render_window_texture(
        renderer,
        &info.window,
        info.geometry,
        info.scale,
        128 * 1024 * 1024,
        reusable,
    )
    .map_err(|error| error.to_string())?
    .ok_or("Window has no capturable content")?;
    let size = snapshot.texture.size();
    if let Some(cursor) = cursor {
        let elements = cursor_elements(state, renderer, cursor, info.scale);
        let mut target = renderer
            .bind(&mut snapshot.texture)
            .map_err(|error| error.to_string())?;
        let mut frame = renderer
            .render(&mut target, (size.w, size.h).into(), Transform::Normal)
            .map_err(|error| error.to_string())?;
        draw_render_elements(
            &mut frame,
            info.scale,
            &elements,
            &[Rectangle::from_size((size.w, size.h).into())],
        )
        .map_err(|error| error.to_string())?;
        let _ = frame.finish().map_err(|error| error.to_string())?;
    }
    let result =
        capture_stream_texture(renderer, &snapshot.texture, buffer, Transform::Normal, true);
    cache
        .0
        .lock()
        .expect("stream texture cache mutex poisoned")
        .put_texture(snapshot.texture);
    result
}

pub(crate) fn capture_stream_texture(
    renderer: &mut GlesRenderer,
    texture: &GlesTexture,
    buffer: &smithay::reexports::wayland_server::protocol::wl_buffer::WlBuffer,
    transform: Transform,
    straight: bool,
) -> Result<smithay::backend::renderer::sync::SyncPoint, String> {
    use smithay::backend::renderer::sync::SyncPoint;
    use smithay::wayland::dmabuf::get_dmabuf;
    use smithay::wayland::shm::with_buffer_contents_mut;
    let cache = renderer
        .egl_context()
        .user_data()
        .get_or_insert_threadsafe(|| Arc::new(StreamCache::default()))
        .clone();
    let mut cache = cache.0.lock().expect("stream texture cache mutex poisoned");
    let program = if straight {
        &mut cache.straight
    } else {
        &mut cache.copy
    };
    if program.is_none() {
        *program =
            Some(compile_stream_program(renderer, straight).map_err(|error| error.to_string())?);
    }
    let program = program.as_ref().expect("stream shader compiled").clone();
    let source_size = texture.size();
    let size: Size<i32, Physical> = transform.transform_size((source_size.w, source_size.h).into());
    let draw = |renderer: &mut GlesRenderer,
                target: &mut smithay::backend::renderer::gles::GlesTarget<'_>| {
        draw_stream_texture(renderer, texture, target, transform, &program)
    };
    if let Ok(dmabuf) = get_dmabuf(buffer) {
        let mut dmabuf = dmabuf.clone();
        let mut target = renderer
            .bind(&mut dmabuf)
            .map_err(|error| error.to_string())?;
        return draw(renderer, &mut target);
    }
    let valid = with_buffer_contents_mut(buffer, |_, length, data| {
        data.width == size.w
            && data.height == size.h
            && data.format == smithay::reexports::wayland_server::protocol::wl_shm::Format::Xrgb8888
            && data.stride >= size.w * 4
            && data.offset >= 0
            && usize::try_from(
                i64::from(data.offset) + i64::from(data.stride) * i64::from(data.height),
            )
            .is_ok_and(|required| required <= length)
    })
    .unwrap_or(false);
    if !valid {
        return Err("Invalid stream SHM buffer".into());
    }
    let mut texture = match cache.take_texture((size.w, size.h).into()) {
        Some(texture) => texture,
        None => Offscreen::<GlesTexture>::create_buffer(
            renderer,
            Fourcc::Abgr8888,
            (size.w, size.h).into(),
        )
        .map_err(|error| error.to_string())?,
    };
    {
        let mut target = renderer
            .bind(&mut texture)
            .map_err(|error| error.to_string())?;
        let _ = draw(renderer, &mut target)?;
    }
    let mapping = renderer
        .copy_texture(
            &texture,
            Rectangle::from_size((size.w, size.h).into()),
            Fourcc::Argb8888,
        )
        .map_err(|error| error.to_string())?;
    let source = renderer
        .map_texture(&mapping)
        .map_err(|error| error.to_string())?;
    let row = size.w as usize * 4;
    let bytes = row * size.h as usize;
    if source.len() < bytes {
        return Err("Incomplete stream readback".into());
    }
    with_buffer_contents_mut(buffer, |destination, _, data| {
        for (y, pixels) in source[..bytes].chunks_exact(row).enumerate() {
            // SAFETY: the buffer bounds and stride were checked above. Source
            // memory belongs to the renderer and cannot overlap the client SHM.
            unsafe {
                std::ptr::copy_nonoverlapping(
                    pixels.as_ptr(),
                    destination.add(data.offset as usize + y * data.stride as usize),
                    row,
                );
            }
        }
    })
    .map_err(|error| format!("{error:?}"))?;
    cache.put_texture(texture);
    Ok(SyncPoint::signaled())
}

fn compile_stream_program(
    renderer: &mut GlesRenderer,
    straight: bool,
) -> Result<smithay::backend::renderer::gles::GlesTexProgram, GlesError> {
    use smithay::backend::renderer::gles::{UniformName, UniformType};
    let source = if straight {
        format!("#define STRAIGHT\n{STREAM_CAPTURE_SHADER}")
    } else {
        STREAM_CAPTURE_SHADER.to_string()
    };
    renderer
        .compile_custom_texture_shader(source, &[UniformName::new("y_offset", UniformType::_1f)])
}

fn draw_stream_texture(
    renderer: &mut GlesRenderer,
    texture: &GlesTexture,
    target: &mut smithay::backend::renderer::gles::GlesTarget<'_>,
    transform: Transform,
    program: &smithay::backend::renderer::gles::GlesTexProgram,
) -> Result<smithay::backend::renderer::sync::SyncPoint, String> {
    let source_size = texture.size();
    let size: Size<i32, Physical> = transform.transform_size((source_size.w, source_size.h).into());
    let mut frame = renderer
        .render(target, size, Transform::Normal)
        .map_err(|error| error.to_string())?;
    let rect = Rectangle::from_size(size);
    frame
        .clear(Color32F::new(0., 0., 0., 1.), &[rect])
        .map_err(|error| error.to_string())?;
    frame
        .render_texture_from_to(
            texture,
            Rectangle::from_size(source_size).to_f64(),
            rect,
            &[rect],
            &[],
            transform,
            1.0,
            Some(program),
            &[smithay::backend::renderer::gles::Uniform::new(
                "y_offset",
                if texture.is_y_inverted() {
                    1.0f32
                } else {
                    0.0f32
                },
            )],
        )
        .map_err(|error| error.to_string())?;
    frame.finish().map_err(|error| error.to_string())
}

pub(crate) fn clear_stream_capture_targets(renderer: &mut GlesRenderer) {
    if let Some(cache) = renderer.egl_context().user_data().get::<Arc<StreamCache>>() {
        let mut cache = cache.0.lock().expect("stream texture cache mutex poisoned");
        cache.textures.clear();
        cache.bytes = 0;
    }
}

#[cfg(test)]
mod stream_gpu_tests {
    use super::*;
    use smithay::backend::egl::{EGLContext, EGLDevice, EGLDisplay};
    use smithay::backend::renderer::ImportMem;

    #[test]
    #[ignore = "requires an EGL rendering device"]
    fn straight_capture_shader_matches_every_alpha_and_channel_value() {
        let device = EGLDevice::enumerate()
            .unwrap()
            .last()
            .expect("an EGL rendering device");
        // SAFETY: the display retains the enumerated device for this test.
        let display = unsafe { EGLDisplay::new(device).unwrap() };
        let context = EGLContext::new(&display).unwrap();
        // SAFETY: this thread exclusively owns the fresh context.
        let mut renderer = unsafe { GlesRenderer::new(context).unwrap() };
        let mut pixels = Vec::with_capacity(256 * 256 * 4);
        for alpha in 0..=255u8 {
            for channel in 0..=255u8 {
                pixels.extend_from_slice(&[
                    channel,
                    channel.wrapping_add(85),
                    channel.wrapping_add(170),
                    alpha,
                ]);
            }
        }
        let source = renderer
            .import_memory(&pixels, Fourcc::Abgr8888, (256, 256).into(), false)
            .unwrap();
        let program = compile_stream_program(&mut renderer, true).unwrap();
        let mut destination = Offscreen::<GlesTexture>::create_buffer(
            &mut renderer,
            Fourcc::Abgr8888,
            (256, 256).into(),
        )
        .unwrap();
        {
            let mut target = renderer.bind(&mut destination).unwrap();
            let _ = draw_stream_texture(
                &mut renderer,
                &source,
                &mut target,
                Transform::Normal,
                &program,
            )
            .unwrap();
        }
        let mapping = renderer
            .copy_texture(
                &destination,
                Rectangle::from_size((256, 256).into()),
                Fourcc::Argb8888,
            )
            .unwrap();
        let actual = renderer.map_texture(&mapping).unwrap();
        convert_window_pixels(&mut pixels);
        for (index, (actual, expected)) in actual
            .chunks_exact(4)
            .zip(pixels.chunks_exact(4))
            .enumerate()
        {
            assert_eq!(
                &actual[..3],
                &expected[..3],
                "alpha={} channel={}",
                index / 256,
                index % 256
            );
            assert_eq!(actual[3], 255, "BGRx capture must be opaque");
        }
    }
    #[test]
    #[ignore = "requires an EGL rendering device"]
    fn stream_transfer_matches_output_orientation_for_all_transforms() {
        let device = EGLDevice::enumerate()
            .unwrap()
            .last()
            .expect("an EGL rendering device");
        // SAFETY: the renderer retains the enumerated device's display.
        let display = unsafe { EGLDisplay::new(device).unwrap() };
        let context = EGLContext::new(&display).unwrap();
        // SAFETY: this test exclusively owns the renderer's context.
        let mut renderer = unsafe { GlesRenderer::new(context).unwrap() };
        use Transform::*;
        let cases = [
            (Normal, "abcdef"),
            (_90, "daebfc"),
            (_180, "fedcba"),
            (_270, "cfbead"),
            (Flipped, "cbafed"),
            (Flipped90, "adbecf"),
            (Flipped180, "defabc"),
            (Flipped270, "fcebda"),
        ];
        let program = compile_stream_program(&mut renderer, false).unwrap();
        let pixels: Vec<_> = b"abcdef"
            .iter()
            .flat_map(|value| [*value, 0, 0, 255])
            .collect();
        for flipped in [false, true] {
            let source = renderer
                .import_memory(&pixels, Fourcc::Abgr8888, (3, 2).into(), flipped)
                .unwrap();
            for (transform, expected) in cases {
                let size =
                    transform.transform_size(Size::<i32, smithay::utils::Buffer>::from((3, 2)));
                let mut target =
                    Offscreen::<GlesTexture>::create_buffer(&mut renderer, Fourcc::Abgr8888, size)
                        .unwrap();
                {
                    let mut target = renderer.bind(&mut target).unwrap();
                    let _ = draw_stream_texture(
                        &mut renderer,
                        &source,
                        &mut target,
                        transform,
                        &program,
                    )
                    .unwrap();
                }
                let mapping = renderer
                    .copy_texture(&target, Rectangle::from_size(size), Fourcc::Abgr8888)
                    .unwrap();
                let actual = renderer.map_texture(&mapping).unwrap();
                let actual: Vec<_> = actual.chunks_exact(4).map(|pixel| pixel[0]).collect();
                // A flipped texture describes the same image with reversed source
                // rows. Derive that reference before applying the output transform.
                let expected = if flipped {
                    expected
                        .bytes()
                        .map(|value| if value < b'd' { value + 3 } else { value - 3 })
                        .collect::<Vec<_>>()
                } else {
                    expected.as_bytes().to_vec()
                };
                assert_eq!(actual, expected, "{transform:?}, texture flipped={flipped}");
            }
        }
    }
    #[test]
    #[ignore = "requires an EGL rendering device"]
    fn alternating_capture_sizes_reuse_their_gpu_allocations() {
        let device = EGLDevice::enumerate()
            .unwrap()
            .last()
            .expect("an EGL rendering device");
        // SAFETY: the renderer retains the enumerated device's display.
        let display = unsafe { EGLDisplay::new(device).unwrap() };
        let context = EGLContext::new(&display).unwrap();
        // SAFETY: this test exclusively owns the renderer's context.
        let mut renderer = unsafe { GlesRenderer::new(context).unwrap() };
        let mut cache = StreamResources::default();
        let sizes = [Size::from((64, 48)), Size::from((96, 64))];
        let mut allocations = [None, None];
        for _ in 0..100 {
            for (index, size) in sizes.into_iter().enumerate() {
                let texture = cache.take_texture(size).unwrap_or_else(|| {
                    Offscreen::<GlesTexture>::create_buffer(&mut renderer, Fourcc::Abgr8888, size)
                        .unwrap()
                });
                if let Some(previous) = allocations[index] {
                    assert_eq!(
                        texture.tex_id(),
                        previous,
                        "switching source sizes allocated another GPU target"
                    );
                } else {
                    allocations[index] = Some(texture.tex_id());
                }
                cache.put_texture(texture);
            }
        }
        assert_eq!(cache.textures.len(), 2);
        assert_eq!(cache.bytes, (64 * 48 + 96 * 64) * 4);
    }
}
