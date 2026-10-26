use super::*;
use smithay::backend::renderer::ImportMem;

fn parameters(width: i32, height: i32, radius: f32) -> MaterialParameters {
    MaterialParameters {
        panel: None,
        corner_radii: [0.0; 4],
        presentation_alpha: 1.0,
        background_opacity: 0.0,
        blur: radius,
        sample_geometry: Rectangle::from_size((width, height).into()),
        sample_physical: Rectangle::from_size((width, height).into()),
        sample_framebuffer: [0.0, 0.0, width as f32, height as f32],
        shadow_rect: [0.0; 4],
        shadow_values: [0.0; 2],
        shadow_bounds: Rectangle::from_size((width, height).into()),
        geometry: Rectangle::from_size((width, height).into()),
        tint: [0.0; 4],
        generation: 0,
        opaque: false,
        visible_framebuffer: [0.0, 0.0, width as f32, height as f32],
    }
}

fn element(renderer: &mut GlesRenderer, p: &MaterialParameters) -> BlurRenderElement {
    let program = blur_program(&mut RenderResources::default(), renderer).unwrap();
    let size = super::blur_cache::size(framebuffer_capture_rect(p.sample_framebuffer), p.blur);
    let texture = renderer.create_buffer(Fourcc::Abgr8888, size).unwrap();
    let scratch = renderer.create_buffer(Fourcc::Abgr8888, size).unwrap();
    BlurRenderElement::new(
        texture,
        scratch,
        program,
        p,
        Arc::new(AtomicBool::new(true)),
    )
}

#[test]
#[ignore = "requires an EGL rendering device"]
fn foreground_redraws_reuse_filtered_pixels_until_backdrop_or_radius_changes() {
    let mut renderer = super::corner_tests::renderer();
    let mut p = parameters(64, 64, 24.0);
    let mut blur = element(&mut renderer, &p);
    let mut target_texture: GlesTexture = renderer
        .create_buffer(Fourcc::Abgr8888, (64, 64).into())
        .unwrap();
    let full = Rectangle::from_size((64, 64).into());
    let mut first = None;
    for step in 0..4 {
        if step == 2 {
            blur.capture_dirty.store(true, Ordering::Relaxed);
        }
        if step == 3 {
            p.blur = 25.0;
            blur.update(&p);
        }
        let dirty = blur.capture_dirty.load(Ordering::Relaxed);
        assert_eq!(dirty, step != 1);
        let color = match step {
            0 => [0.2, 0.4, 0.6, 1.0],
            1 => [0.9, 0.1, 0.2, 1.0],
            2 => [0.7, 0.3, 0.1, 1.0],
            _ => [0.1, 0.8, 0.3, 1.0],
        };
        let mut target = renderer.bind(&mut target_texture).unwrap();
        let mut frame = renderer
            .render(&mut target, (64, 64).into(), Transform::Normal)
            .unwrap();
        frame.clear(color.into(), &[full]).unwrap();
        blur.draw(&mut frame, blur.src(), full, &[full], &[], None)
            .unwrap();
        frame.finish().unwrap().wait().unwrap();
        assert!(!blur.capture_dirty.load(Ordering::Relaxed));
        let mapping = renderer
            .copy_framebuffer(
                &target,
                Rectangle::from_size((64, 64).into()),
                Fourcc::Abgr8888,
            )
            .unwrap();
        let rendered = renderer.map_texture(&mapping).unwrap().to_vec();
        if step == 0 {
            first = Some(rendered.clone());
        }
        if step == 1 {
            assert_eq!(
                rendered,
                *first.as_ref().unwrap(),
                "foreground damage recaptured the background"
            );
        } else {
            for (actual, expected) in rendered[(32 * 64 + 32) * 4..][..3].iter().zip(color) {
                assert!(
                    (f32::from(*actual) - expected * 255.0).abs() <= 2.0,
                    "stale filtered pixels after step {step}"
                );
            }
        }
    }
}

#[test]
#[ignore = "manual comparison of full-window redraw cost on an EGL device"]
fn profile_window_blur_redraws() {
    let mut renderer = super::corner_tests::renderer();
    // Reconstruct the former kernel as a negative control, with the same masks.
    let old_kernel = r#"
        vec4 color = vec4(0.0); float weights = 0.0;
        for (int i = 0; i < 256; i++) {
            float radius = sqrt((float(i) + 0.5) / 256.0);
            float angle = float(i) * 2.39996323;
            vec2 offset = vec2(cos(angle), sin(angle)) * radius * blur_radius / texture_size;
            float weight = exp(-4.5 * radius * radius);
            color += texture2D(tex, coords + offset) * weight; weights += weight;
        }
        vec3 background = color.rgb / weights;
    "#;
    let old_shader = corner_shader(BLUR_SHADER)
        .replace(
            "uniform float alpha;",
            "uniform float alpha; uniform float blur_radius;",
        )
        .replace("vec3 background = texture2D(tex, coords).rgb;", old_kernel);
    let mut names = materials::blur_uniform_names();
    names.push(UniformName::new("blur_radius", UniformType::_1f));
    let old = renderer
        .compile_custom_texture_shader(old_shader, &names)
        .unwrap();
    for (width, height) in [(2880, 1800), (3840, 2160)] {
        let p = parameters(width, height, 38.5);
        let blur = element(&mut renderer, &p);
        let pixels = (0..width * height)
            .flat_map(|i| {
                [
                    (i % 251) as u8,
                    ((i / width) % 251) as u8,
                    ((i * 13) % 251) as u8,
                    255,
                ]
            })
            .collect::<Vec<_>>();
        let texture = renderer
            .import_memory(&pixels, Fourcc::Abgr8888, (width, height).into(), false)
            .unwrap();
        let mut output: GlesTexture = renderer
            .create_buffer(Fourcc::Abgr8888, (width, height).into())
            .unwrap();
        let full = Rectangle::from_size((width, height).into());
        let mut uniforms = blur_uniforms(&p);
        uniforms.push(Uniform::new("blur_radius", p.blur));
        for mode in ["off", "old", "cached", "dirty"] {
            let mut timings = Vec::new();
            for i in 0..20 {
                if mode == "dirty" {
                    blur.capture_dirty.store(true, Ordering::Relaxed);
                }
                let started = std::time::Instant::now();
                let mut target = renderer.bind(&mut output).unwrap();
                let mut frame = renderer
                    .render(&mut target, (width, height).into(), Transform::Normal)
                    .unwrap();
                frame
                    .render_texture_from_to(
                        &texture,
                        Rectangle::from_size(texture.size().to_f64()),
                        full,
                        &[full],
                        &[full],
                        Transform::Normal,
                        1.0,
                        None,
                        &[],
                    )
                    .unwrap();
                if mode == "old" {
                    frame
                        .render_texture_from_to(
                            &texture,
                            Rectangle::from_size(texture.size().to_f64()),
                            full,
                            &[full],
                            &[],
                            Transform::Normal,
                            1.0,
                            Some(&old),
                            &uniforms,
                        )
                        .unwrap();
                } else if mode != "off" {
                    blur.draw(&mut frame, blur.src(), full, &[full], &[], None)
                        .unwrap();
                }
                // A translucent client repaints its complete content over the blur.
                frame
                    .render_texture_from_to(
                        &texture,
                        Rectangle::from_size(texture.size().to_f64()),
                        full,
                        &[full],
                        &[],
                        Transform::Normal,
                        0.87,
                        None,
                        &[],
                    )
                    .unwrap();
                frame.finish().unwrap().wait().unwrap();
                if i >= 4 {
                    timings.push(started.elapsed().as_secs_f64() * 1000.0);
                }
            }
            timings.sort_by(f64::total_cmp);
            println!(
                "{width}x{height} {mode}: median_ms={:.3} p95_ms={:.3} filtered_cache_bytes={}",
                timings[8],
                timings[15],
                blur.texture.size().w * blur.texture.size().h * 8
            );
        }
    }
}
