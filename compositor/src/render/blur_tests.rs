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

#[test]
#[ignore = "requires an EGL rendering device"]
fn opaque_window_above_blur_does_not_contaminate_visible_edges() {
    use smithay::backend::renderer::damage::OutputDamageTracker;
    use smithay::backend::renderer::element::solid::SolidColorBuffer;

    for (scale, offscreen) in [1.0, 1.25, 1.5]
        .into_iter()
        .flat_map(|scale| [false, true].map(|offscreen| (scale, offscreen)))
    {
        // Finish and release the EGL framebuffer before checking pixels so a
        // failed assertion cannot poison its context lock during unwinding.
        let pixel = {
            let mut renderer = super::corner_tests::renderer();
            let extent = (64.0 * scale) as i32;
            let mut p = parameters(extent, extent, (24.0 * scale) as f32);
            p.sample_geometry = Rectangle::from_size((64, 64).into());
            p.geometry = p.sample_geometry;
            let blur = element(&mut renderer, &p);
            let full = Rectangle::from_size((extent, extent).into());
            let mut output: GlesTexture = renderer
                .create_buffer(Fourcc::Abgr8888, (extent, extent).into())
                .unwrap();
            let mut target = renderer.bind(&mut output).unwrap();
            let mut frame = renderer
                .render(&mut target, (extent, extent).into(), Transform::Normal)
                .unwrap();
            // Stale pixels under the covering window must never enter the blur.
            frame.clear([1.0, 0.0, 0.0, 1.0].into(), &[full]).unwrap();
            frame.finish().unwrap().wait().unwrap();
            let white = SolidColorBuffer::new((32, 64), [1.0, 1.0, 1.0, 1.0]);
            let hidden_white = SolidColorBuffer::new((32, 64), [1.0, 1.0, 1.0, 1.0]);
            let cover = SolidColorBuffer::new((32, 64), [0.0, 0.0, 1.0, 1.0]);
            let elements: Vec<AnimatedWindowRenderElement> = vec![
                SolidColorRenderElement::from_buffer(
                    &cover,
                    ((32.0 * scale) as i32, 0),
                    scale,
                    1.0,
                    RenderElementKind::Unspecified,
                )
                .into(),
                blur.into(),
                SolidColorRenderElement::from_buffer(
                    &hidden_white,
                    ((32.0 * scale) as i32, 0),
                    scale,
                    1.0,
                    RenderElementKind::Unspecified,
                )
                .into(),
                SolidColorRenderElement::from_buffer(
                    &white,
                    (0, 0),
                    scale,
                    1.0,
                    RenderElementKind::Unspecified,
                )
                .into(),
            ];
            super::backdrop::update(&elements, scale);
            if offscreen {
                let mut frame = renderer
                    .render(&mut target, (extent, extent).into(), Transform::Normal)
                    .unwrap();
                smithay::backend::renderer::utils::draw_render_elements(
                    &mut frame,
                    scale,
                    &elements,
                    &[full],
                )
                .unwrap();
                frame.finish().unwrap().wait().unwrap();
            } else {
                let mut tracker =
                    OutputDamageTracker::new((extent, extent), scale, Transform::Normal);
                tracker
                    .render_output(
                        &mut renderer,
                        &mut target,
                        0,
                        &elements,
                        [0.0, 0.0, 0.0, 1.0],
                    )
                    .unwrap()
                    .sync
                    .wait()
                    .unwrap();
            }
            let mapping = renderer
                .copy_framebuffer(
                    &target,
                    Rectangle::from_size((extent, extent).into()),
                    Fourcc::Abgr8888,
                )
                .unwrap();
            let pixels = renderer.map_texture(&mapping).unwrap();
            let index = ((32.0 * scale) as usize * extent as usize + (30.0 * scale) as usize) * 4;
            pixels[index..index + 4].to_vec()
        };
        assert!(
            pixel[..3].iter().all(|channel| *channel >= 253),
            "occluded pixels entered blur at scale {scale}, offscreen={offscreen}: {pixel:?}"
        );
    }
}

#[test]
#[ignore = "requires an EGL rendering device"]
fn translated_material_reuses_both_textures_and_element_until_storage_size_changes() {
    use smithay::output::{Mode, PhysicalProperties, Scale, Subpixel};
    use smithay::wayland::seat::WaylandFocus;
    if !crate::startup_tests::private_runtime(
        "render::blur_tests::translated_material_reuses_both_textures_and_element_until_storage_size_changes",
    ) {
        return;
    }
    let mut events = smithay::reexports::calloop::EventLoop::try_new().unwrap();
    let mut state = crate::startup_tests::state(&mut events);
    let output = Output::new(
        "material-translation".into(),
        PhysicalProperties {
            size: (0, 0).into(),
            subpixel: Subpixel::Unknown,
            make: String::new(),
            model: String::new(),
            serial_number: String::new(),
        },
    );
    output.change_current_state(
        Some(Mode {
            size: (1024, 768).into(),
            refresh: 60_000,
        }),
        None,
        None,
        None,
    );
    state.space.map_output(&output, (0, 0));
    state.register_output(&output, "material-translation".into());
    let (window, _client) = crate::state::test_window(&mut state, &mut events, 0xffff0000);
    let surface = window.wl_surface().unwrap().into_owned();
    let mut renderer = super::corner_tests::renderer();
    state.theme_settings.shell_opacity = 0.7;
    state.theme_settings.backdrop_blur = 12.0;
    for scale in [1.0, 1.25, 1.5] {
        for transform in [Transform::Normal, Transform::_90, Transform::Flipped180] {
            output.change_current_state(
                None,
                Some(transform),
                Some(Scale::Fractional(scale)),
                None,
            );
            state.theme_settings.material_style = crate::config::MaterialStyle::Translucent;
            let make = |state: &mut Ferese, renderer: &mut GlesRenderer, x| {
                let geometry = Rectangle::new((x, 80).into(), (160, 100).into());
                materials::material_element_with_role(
                    state,
                    renderer,
                    &output,
                    &surface,
                    MaterialSurface {
                        shape_origin: geometry.loc,
                        geometry,
                        corners: RoundedRect::from_logical(geometry, scale, 16.0),
                        index: 0,
                        capture_geometry: geometry,
                        alpha: 1.0,
                    },
                    (crate::effects::SemanticRole::Modal, 0, 1.0),
                )
                .unwrap()
                .0
            };
            let AnimatedWindowRenderElement::Blur(initial) = make(&mut state, &mut renderer, 80)
            else {
                panic!("translucent material must have a blur element");
            };
            for x in [84, 88, 92] {
                initial.capture_dirty.store(false, Ordering::Relaxed);
                let AnimatedWindowRenderElement::Blur(moved) = make(&mut state, &mut renderer, x)
                else {
                    panic!("moving a material must preserve blur");
                };
                assert_eq!(
                    moved.texture.tex_id(),
                    initial.texture.tex_id(),
                    "{scale} {transform:?}"
                );
                assert_eq!(
                    moved.scratch.tex_id(),
                    initial.scratch.tex_id(),
                    "{scale} {transform:?}"
                );
                assert_eq!(
                    moved.id(),
                    initial.id(),
                    "translation must preserve element identity"
                );
                assert!(
                    initial.capture_dirty.load(Ordering::Relaxed),
                    "new backdrop must be recaptured"
                );
            }
            let AnimatedWindowRenderElement::Blur(clipped) = make(&mut state, &mut renderer, 0)
            else {
                panic!("edge-clipped material must retain blur");
            };
            assert_ne!(clipped.texture.size(), initial.texture.size());
            assert_ne!(clipped.texture.tex_id(), initial.texture.tex_id());
            assert_ne!(clipped.scratch.tex_id(), initial.scratch.tex_id());
            state.theme_settings.material_style = crate::config::MaterialStyle::Solid;
            assert!(matches!(
                make(&mut state, &mut renderer, 80),
                AnimatedWindowRenderElement::Border(_) | AnimatedWindowRenderElement::Effect(_)
            ));
            assert!(state.render.surfaces[&surface].captures.is_empty());
        }
    }
}

#[test]
#[ignore = "requires an EGL rendering device"]
fn centered_material_refreshes_after_output_rotation_without_reallocating() {
    use smithay::backend::renderer::element::solid::SolidColorRenderElement;
    use smithay::output::{Mode, PhysicalProperties, Subpixel};
    if !crate::startup_tests::private_runtime(
        "render::blur_tests::centered_material_refreshes_after_output_rotation_without_reallocating",
    ) {
        return;
    }
    let mut events = smithay::reexports::calloop::EventLoop::try_new().unwrap();
    let mut state = crate::startup_tests::state(&mut events);
    let output = Output::new(
        "centered-material".into(),
        PhysicalProperties {
            size: (0, 0).into(),
            subpixel: Subpixel::Unknown,
            make: String::new(),
            model: String::new(),
            serial_number: String::new(),
        },
    );
    output.change_current_state(
        Some(Mode {
            size: (128, 96).into(),
            refresh: 60_000,
        }),
        None,
        None,
        None,
    );
    state.space.map_output(&output, (0, 0));
    state.register_output(&output, "centered-material".into());
    let (window, _client) = crate::state::test_window(&mut state, &mut events, 0xffff0000);
    let surface = window.toplevel().unwrap().wl_surface().clone();
    let mut renderer = super::corner_tests::renderer();
    state.theme_settings.shell_opacity = 0.7;
    state.theme_settings.backdrop_blur = 12.0;
    state.theme_settings.material_style = crate::config::MaterialStyle::Translucent;
    let geometry = Rectangle::new((32, 24).into(), (64, 48).into());
    let make = |state: &mut Ferese, renderer: &mut GlesRenderer| {
        let (AnimatedWindowRenderElement::Blur(blur), _) = materials::material_element_with_role(
            state,
            renderer,
            &output,
            &surface,
            MaterialSurface {
                shape_origin: geometry.loc,
                geometry,
                corners: RoundedRect::from_logical(geometry, 1.0, 4.0),
                index: 0,
                capture_geometry: geometry,
                alpha: 1.0,
            },
            (crate::effects::SemanticRole::Modal, 0, 1.0),
        )
        .unwrap() else {
            panic!("translucent material must have a blur element")
        };
        blur
    };
    let backgrounds: Vec<SolidColorRenderElement> = [
        ((0, 0), [0.9_f32, 0.0, 0.0, 1.0]),
        ((64, 0), [0.0_f32, 0.0, 0.9, 1.0]),
    ]
    .into_iter()
    .map(|(origin, color)| {
        SolidColorRenderElement::new(
            Id::new(),
            Rectangle::new(origin.into(), (64, 96).into()),
            CommitCounter::default(),
            color,
            RenderElementKind::Unspecified,
        )
    })
    .collect();
    let mut target: GlesTexture = renderer
        .create_buffer(Fourcc::Abgr8888, (128, 96).into())
        .unwrap();
    let render =
        |renderer: &mut GlesRenderer, target: &mut GlesTexture, blur: BlurRenderElement| {
            let mut scene = vec![AnimatedWindowRenderElement::Blur(blur)];
            scene.extend(
                backgrounds
                    .iter()
                    .cloned()
                    .map(AnimatedWindowRenderElement::from),
            );
            super::backdrop::update(&scene, 1.0);
            let mut framebuffer = renderer.bind(target).unwrap();
            redraw_output(renderer, &mut framebuffer, &output, &scene).unwrap();
            let mapping = renderer
                .copy_framebuffer(
                    &framebuffer,
                    Rectangle::from_size((128, 96).into()),
                    Fourcc::Abgr8888,
                )
                .unwrap();
            renderer.map_texture(&mapping).unwrap().to_vec()
        };
    let initial = make(&mut state, &mut renderer);
    render(&mut renderer, &mut target, initial.clone());
    assert!(!initial.capture_dirty.load(Ordering::Relaxed));
    for transform in [
        Transform::_180,
        Transform::Flipped,
        Transform::Flipped180,
        Transform::Normal,
    ] {
        output.change_current_state(None, Some(transform), None, None);
        let rotated = make(&mut state, &mut renderer);
        assert_eq!(
            rotated.capture_rect, initial.capture_rect,
            "fixture must keep the sampling rectangle unchanged"
        );
        assert_eq!(rotated.texture.tex_id(), initial.texture.tex_id());
        assert_eq!(rotated.scratch.tex_id(), initial.scratch.tex_id());
        assert_eq!(rotated.id(), initial.id());
        let invalidated = rotated.capture_dirty.load(Ordering::Relaxed);
        let retained = render(&mut renderer, &mut target, rotated.clone());
        rotated.capture_dirty.store(true, Ordering::Relaxed);
        let fresh = render(&mut renderer, &mut target, rotated);
        let different_bytes = retained
            .iter()
            .zip(&fresh)
            .filter(|(left, right)| left != right)
            .count();
        assert_eq!(
            different_bytes, 0,
            "output rotation retained stale blurred pixels (dirty={invalidated})"
        );
        assert!(
            invalidated,
            "output rotation must invalidate the retained sample even when its rectangle is symmetric"
        );
    }
}
