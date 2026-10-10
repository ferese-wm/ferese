//! Independent path oracle for the measured UIKit profile and GPU edge coverage.
use super::*;

const EXTENT: f64 = 1.52866498;
type Point2 = [f64; 2];
type Cubic = [Point2; 4];

// Fixture: normalized points extracted from UIKit's UIBezierPath output.
// https://liamrosenfeld.com/posts/apple_icon_quest/
const APPLE: [Cubic; 3] = [
    [
        [0.0, EXTENT],
        [0.0, 1.08849296],
        [0.0, 0.86840694],
        [0.07491139, 0.63149379],
    ],
    [
        [0.07491139, 0.63149379],
        [0.16905956, 0.37282383],
        [0.37282383, 0.16905956],
        [0.63149379, 0.07491139],
    ],
    [
        [0.63149379, 0.07491139],
        [0.86840694, 0.0],
        [1.08849296, 0.0],
        [EXTENT, 0.0],
    ],
];

const CIRCLE: [Cubic; 3] = [
    [
        [0.0, 1.0],
        [0.0, 0.8686781289],
        [0.0258657631, 0.7386421566],
        [0.0761204675, 0.6173165676],
    ],
    [
        [0.0761204675, 0.6173165676],
        [0.1776144241, 0.3722884810],
        [0.3722884810, 0.1776144241],
        [0.6173165676, 0.0761204675],
    ],
    [
        [0.6173165676, 0.0761204675],
        [0.7386421566, 0.0258657631],
        [0.8686781289, 0.0],
        [1.0, 0.0],
    ],
];

fn lerp(a: Point2, b: Point2, t: f64) -> Point2 {
    [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t]
}

fn curve(mut points: Cubic, t: f64) -> Point2 {
    // De Casteljau, independent of the shader's polynomial/Newton calculation.
    for count in (1..4).rev() {
        for index in 0..count {
            points[index] = lerp(points[index], points[index + 1], t);
        }
    }

    points[0]
}

fn squared_distance(a: Point2, b: Point2) -> f64 {
    (a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2)
}

fn nearest_distance(points: Cubic, query: Point2) -> f64 {
    let index = (0usize..=64)
        .min_by(|a, b| {
            squared_distance(curve(points, *a as f64 / 64.0), query)
                .total_cmp(&squared_distance(curve(points, *b as f64 / 64.0), query))
        })
        .unwrap();
    let mut low = index.saturating_sub(1) as f64 / 64.0;
    let mut high = (index + 1).min(64) as f64 / 64.0;

    // Bracketed minimization, rather than reimplementing the GPU Newton solver.
    for _ in 0..36 {
        let a = low + (high - low) / 3.0;
        let b = high - (high - low) / 3.0;

        if squared_distance(curve(points, a), query) < squared_distance(curve(points, b), query) {
            high = b;
        } else {
            low = a;
        }
    }

    squared_distance(curve(points, (low + high) * 0.5), query).sqrt()
}

fn reference_distance(point: Point2, rect: [f64; 4], radius: f64, shape: CornerShape) -> f64 {
    let half = [rect[2] * 0.5, rect[3] * 0.5];
    let inset = [
        half[0] - (point[0] - rect[0] - half[0]).abs(),
        half[1] - (point[1] - rect[1] - half[1]).abs(),
    ];
    let limit = half[0].min(half[1]);

    if shape == CornerShape::Circular || radius <= 0.0 || radius >= limit {
        let q = [radius - inset[0], radius - inset[1]];
        return q[0].max(0.0).hypot(q[1].max(0.0)) + q[0].max(q[1]).min(0.0) - radius;
    }

    let blend = ((limit / radius - 1.0) / (EXTENT - 1.0)).clamp(0.0, 1.0);
    let extent = 1.0 + (EXTENT - 1.0) * blend;

    if inset[0].max(inset[1]) >= extent * radius {
        return -inset[0].min(inset[1]);
    }

    let query = [inset[0] / radius, inset[1] / radius];
    let curves = std::array::from_fn::<_, 3, _>(|segment| {
        std::array::from_fn(|index| lerp(CIRCLE[segment][index], APPLE[segment][index], blend))
    });
    let distance = curves
        .iter()
        .map(|points| nearest_distance(*points, query))
        .fold(f64::INFINITY, f64::min);
    let mut inside = false;

    if query[0] >= 0.0 && query[1] >= 0.0 {
        for points in curves {
            if query[0] >= points[0][0] && query[0] <= points[3][0] {
                let mut low = 0.0;
                let mut high = 1.0;

                for _ in 0..36 {
                    let middle = (low + high) * 0.5;

                    if curve(points, middle)[0] < query[0] {
                        low = middle;
                    } else {
                        high = middle;
                    }
                }

                inside = query[1] >= curve(points, (low + high) * 0.5)[1];
                break;
            }
        }
    }

    distance * radius * if inside { -1.0 } else { 1.0 }
}

fn coverage(distance: f64) -> f64 {
    (0.5 - distance).clamp(0.0, 1.0)
}

#[test]
fn shared_geometry_matches_the_existing_window_profile() {
    let bounds = [1.25, 2.5, 61.5, 59.0];

    for shape in [CornerShape::Circular, CornerShape::Continuous] {
        for radius in [0.0, 0.375, 8.25, 12.375, 24.0, 29.49, 29.5] {
            let outline = ferese_shape::Outline::new(bounds, [radius; 4], shape).unwrap();

            for y in 0..64 {
                for x in 0..64 {
                    let point = [x as f64 + 0.5, y as f64 + 0.5];
                    let expected = reference_distance(point, bounds, radius, shape);
                    let actual = outline.signed_distance(point);
                    assert!(
                        (actual - expected).abs() < 0.01,
                        "{shape:?} radius={radius} point={point:?}"
                    );
                    assert!((coverage(actual) - coverage(expected)).abs() <= 2.0 / 255.0);
                }
            }
        }
    }
}

#[test]
fn shell_masks_match_the_shared_squircle_profile() {
    for source in [BLUR_SHADER, MATERIAL_SHADER, ROUNDED_TEXTURE_SHADER] {
        assert_eq!(
            corner_shader(source),
            corner_shader_for(source, CornerShape::Continuous)
        );
        assert!(corner_shader(source).contains("CORNER_EXTENT"));
        assert!(!corner_shader_for(source, CornerShape::Circular).contains("CORNER_EXTENT"));
    }
}

fn renderer() -> GlesRenderer {
    use smithay::backend::egl::{EGLContext, EGLDevice, EGLDisplay};

    let devices = EGLDevice::enumerate().unwrap().collect::<Vec<_>>();
    let software = std::env::var_os("FERESE_TEST_EGL_SOFTWARE").is_some();
    let device = devices
        .into_iter()
        .find(|device| device.is_software() == software)
        .expect("the requested EGL device");
    let display = unsafe { EGLDisplay::new(device).unwrap() };
    unsafe { GlesRenderer::new(EGLContext::new(&display).unwrap()).unwrap() }
}

#[test]
#[ignore = "manual EGL timing sample; requires a rendering device"]
fn window_corner_shader_timing_sample() {
    let mut renderer = renderer();
    let driver = renderer
        .with_context(|gl| {
            unsafe { std::ffi::CStr::from_ptr(gl.GetString(0x1F01).cast()) }
                .to_string_lossy()
                .into_owned()
        })
        .unwrap();
    let mut resources = RenderResources::default();
    let size = (1024, 768).into();
    let rect = Rectangle::<i32, Physical>::from_size(size);
    let uniforms = [
        Uniform::new("clip_rect", [0.0f32, 0.0, 1024.0, 768.0]),
        Uniform::new("radius", 16.0f32),
    ];
    let mut texture: GlesTexture = renderer.create_buffer(Fourcc::Abgr8888, (1024, 768).into()).unwrap();

    for shape in [CornerShape::Circular, CornerShape::Continuous] {
        let programs = corner_program(&mut resources, &mut renderer, shape).unwrap();
        let mut samples = Vec::new();

        for iteration in 0..110 {
            let started = std::time::Instant::now();
            let mut target = renderer.bind(&mut texture).unwrap();
            let mut frame = renderer.render(&mut target, size, Transform::Normal).unwrap();
            frame.clear(Color32F::TRANSPARENT, &[rect]).unwrap();
            frame
                .render_pixel_shader_to(
                    &programs.solid,
                    Rectangle::from_size((1024.0, 768.0).into()),
                    rect,
                    (1024, 768).into(),
                    Some(&[rect]),
                    1.0,
                    &uniforms,
                )
                .unwrap();
            frame.finish().unwrap().wait().unwrap();

            if iteration >= 10 {
                samples.push(started.elapsed());
            }
        }

        samples.sort_unstable();
        eprintln!(
            "corner shader {shape:?}: driver={driver}, median={:?}, p95={:?}, size=1024x768, radius=16",
            samples[50], samples[95]
        );
    }
}

fn rasterize(renderer: &mut GlesRenderer, program: &GlesPixelProgram, uniforms: &[Uniform<'_>]) -> Vec<u8> {
    let size = (64, 64).into();
    let mut texture: GlesTexture = renderer.create_buffer(Fourcc::Abgr8888, size).unwrap();
    let rect = Rectangle::<i32, Physical>::from_size((64, 64).into());
    let mut target = renderer.bind(&mut texture).unwrap();
    {
        let mut frame = renderer
            .render(&mut target, (64, 64).into(), Transform::Normal)
            .unwrap();
        frame.clear(Color32F::TRANSPARENT, &[rect]).unwrap();
        frame
            .render_pixel_shader_to(
                program,
                Rectangle::from_size(size.to_f64()),
                rect,
                size,
                Some(&[rect]),
                1.0,
                uniforms,
            )
            .unwrap();
        let _ = frame.finish().unwrap();
    }

    let mapping = renderer
        .copy_framebuffer(&target, Rectangle::from_size(size), Fourcc::Abgr8888)
        .unwrap();
    renderer.map_texture(&mapping).unwrap().to_vec()
}

#[test]
#[ignore = "requires an EGL rendering device"]
fn apple_window_fill_and_inset_border_match_path_reference_at_fractional_scales() {
    let mut renderer = renderer();
    let mut resources = RenderResources::default();
    let programs = corner_program(&mut resources, &mut renderer, CornerShape::Continuous).unwrap();

    // Include tiny radii, fractional physical coordinates, overlapping shoulder
    // adaptation, exact pills, square/fullscreen, and broad inset borders.
    for (radius, width) in [
        (0.0f32, 1.0f32),
        (0.375, 0.75),
        (8.25, 1.25),
        (12.375, 2.25),
        (24.0, 3.0),
        (29.49, 2.5),
        (29.5, 2.5),
        (12.375, 12.0),
    ] {
        let rect = [1.25f32, 2.5, 61.5, 59.0];
        let fill = rasterize(
            &mut renderer,
            &programs.solid,
            &[
                Uniform::new("clip_rect", rect),
                Uniform::new("radius", radius),
                Uniform::new("color", [1.0f32; 4]),
            ],
        );
        let border = BorderParameters {
            geometry: Rectangle::from_size((64, 64).into()),
            clip_rect: rect,
            radius,
            width,
            color: [1.0; 4],
            color_to: [1.0; 4],
            gradient_line: [0.0; 4],
            focus_color: [1.0; 4],
            focus_color_to: [1.0; 4],
            focus_gradient_line: [0.0; 4],
            focus_mix: 0.0,
        };
        let edge = rasterize(&mut renderer, &programs.border, &border_uniforms(&border));

        for y in 0..64 {
            for x in 0..64 {
                let distance = reference_distance(
                    [x as f64 + 0.5, y as f64 + 0.5],
                    rect.map(f64::from),
                    radius.into(),
                    CornerShape::Continuous,
                );
                let outer = coverage(distance);
                let inner = coverage(distance + f64::from(width));
                let index = (y * 64 + x) * 4;

                for (actual, expected) in [(fill[index + 3], outer), (edge[index + 3], (outer - inner).max(0.0))] {
                    let expected = (expected * 255.0).round() as u8;
                    assert!(
                        actual.abs_diff(expected) <= 2,
                        "x={x} y={y} radius={radius} width={width}: GPU={actual}, reference={expected}"
                    );
                }
            }
        }
    }
}

#[test]
#[ignore = "requires an EGL rendering device"]
fn circular_and_squircle_programs_keep_independent_caches() {
    let mut renderer = renderer();
    let mut resources = RenderResources::default();
    let context = renderer.context_id().erased();
    let circular = corner_program(&mut resources, &mut renderer, CornerShape::Circular).unwrap();
    material_program_for_corners(&mut resources, &mut renderer, CornerShape::Circular).unwrap();
    blur_program(&mut resources, &mut renderer).unwrap();
    let uniforms = [
        Uniform::new("clip_rect", [0.0f32, 0.0, 64.0, 64.0]),
        Uniform::new("radius", 12.0f32),
        Uniform::new("color", [1.0f32; 4]),
    ];
    let before = rasterize(&mut renderer, &circular.solid, &uniforms);
    corner_program(&mut resources, &mut renderer, CornerShape::Continuous).unwrap();
    material_program_for_corners(&mut resources, &mut renderer, CornerShape::Continuous).unwrap();
    let circular = corner_program(&mut resources, &mut renderer, CornerShape::Circular).unwrap();
    let after = rasterize(&mut renderer, &circular.solid, &uniforms);
    assert_eq!(before, after);
    let cache = &resources.contexts[&context];
    assert!(cache.material.is_some() && cache.window_material.is_some() && cache.blur.is_some());
}

#[test]
fn apple_shoulder_extent_never_claims_transparent_pixels_as_opaque() {
    let clip = Rectangle::from_size((64, 64).into());

    for radius in [0.375f32, 8.25, 12.375, 24.0, 31.99, 32.0] {
        let extent = corner_extent(radius, clip.size, CornerShape::Continuous);
        let opaque = rounded_opaque_regions(&[clip], Point::default(), clip, extent);

        for region in opaque.iter() {
            for y in region.loc.y..region.loc.y + region.size.h {
                for x in region.loc.x..region.loc.x + region.size.w {
                    let distance = reference_distance(
                        [x as f64 + 0.5, y as f64 + 0.5],
                        [0.0, 0.0, 64.0, 64.0],
                        radius.into(),
                        CornerShape::Continuous,
                    );
                    assert_eq!(
                        coverage(distance),
                        1.0,
                        "transparent opaque claim at ({x}, {y}), radius={radius}"
                    );
                }
            }
        }
    }
}

#[test]
#[ignore = "requires an EGL rendering device"]
fn docked_panel_fill_covers_the_entire_top_row() {
    let mut renderer = renderer();
    let mut resources = RenderResources::default();
    let material = material_program_for_corners(&mut resources, &mut renderer, CornerShape::Circular).unwrap();
    let radii = ferese_config::panel::CornerRadii([14., 8., 12., 20.])
        .at_top_edge(true)
        .0;
    let pixels = rasterize(
        &mut renderer,
        &material.0,
        &[
            Uniform::new("visible_rect", [0.0f32, 0.0, 64.0, 36.0]),
            Uniform::new("material_radii", radii),
            Uniform::new("tint", [1.0f32; 4]),
            Uniform::new("paint_mode", 0.0f32),
            Uniform::new("shadow_rect", [0.0f32, 0.0, 64.0, 36.0]),
            Uniform::new("shadow_values", [8.0f32, 1.0]),
        ],
    );
    for x in 0..64 {
        assert_eq!(pixels[x * 4 + 3], 255, "wallpaper exposed at top pixel {x}");
    }
    assert_eq!(pixels[(35 * 64) * 4 + 3], 0, "bottom corners remain rounded");
}

#[test]
#[ignore = "requires an EGL rendering device"]
fn asymmetric_panel_fill_and_shadow_match_each_corner() {
    let mut renderer = renderer();
    let mut resources = RenderResources::default();
    let material = material_program_for_corners(&mut resources, &mut renderer, CornerShape::Circular).unwrap();
    let rect = [2.0f32, 2.0, 60.0, 60.0];
    let radii = [0.0f32, 4.0, 12.0, 20.0];
    for paint_mode in [0.0f32, 2.0] {
        let pixels = rasterize(
            &mut renderer,
            &material.0,
            &[
                Uniform::new("visible_rect", rect),
                Uniform::new("material_radii", radii),
                Uniform::new("tint", [1.0f32; 4]),
                Uniform::new("paint_mode", paint_mode),
                Uniform::new("shadow_rect", rect),
                Uniform::new("shadow_values", [8.0f32, 1.0]),
            ],
        );
        for y in 0..64 {
            for x in 0..64 {
                let radius = match (x < 32, y < 32) {
                    (true, true) => radii[0],
                    (false, true) => radii[1],
                    (false, false) => radii[2],
                    (true, false) => radii[3],
                };
                let distance = reference_distance(
                    [x as f64 + 0.5, y as f64 + 0.5],
                    rect.map(f64::from),
                    radius.into(),
                    CornerShape::Circular,
                );
                let expected = if paint_mode == 0. {
                    coverage(distance)
                } else {
                    (-0.5 * (distance.max(0.0) / 4.0).powi(2)).exp() * (1. - coverage(distance))
                };
                assert!(
                    pixels[(y * 64 + x) * 4 + 3].abs_diff((expected * 255.).round() as u8) <= 2,
                    "corner mismatch at ({x}, {y}), mode {paint_mode}"
                );
            }
        }
    }
}

#[test]
#[ignore = "requires an EGL rendering device"]
fn apple_shadow_and_window_tint_match_the_same_outline() {
    let mut renderer = renderer();
    let mut resources = RenderResources::default();
    let programs = corner_program(&mut resources, &mut renderer, CornerShape::Continuous).unwrap();
    let material = material_program_for_corners(&mut resources, &mut renderer, CornerShape::Continuous).unwrap();
    let rect = [1.25f32, 2.5, 61.5, 59.0];
    let radius = 12.375f32;
    let shadow = rasterize(
        &mut renderer,
        &programs.shadow,
        &[
            Uniform::new("shadow_rect", rect),
            Uniform::new("radius", radius),
            Uniform::new("blur", 8.0f32),
            Uniform::new("opacity", 1.0f32),
            Uniform::new("shadow_color", [1.0f32; 4]),
        ],
    );
    let tint = rasterize(
        &mut renderer,
        &material.0,
        &[
            Uniform::new("visible_rect", rect),
            Uniform::new("material_radii", [radius; 4]),
            Uniform::new("tint", [1.0f32; 4]),
            Uniform::new("paint_mode", 0.0f32),
            Uniform::new("shadow_rect", rect),
            Uniform::new("shadow_values", [0.0f32; 2]),
        ],
    );

    for y in 0..64 {
        for x in 0..64 {
            let distance = reference_distance(
                [x as f64 + 0.5, y as f64 + 0.5],
                rect.map(f64::from),
                radius.into(),
                CornerShape::Continuous,
            );
            let shadow_alpha = (-0.5 * (distance.max(0.0) / 4.0).powi(2)).exp();
            let index = (y * 64 + x) * 4;

            for (actual, expected) in [(tint[index + 3], coverage(distance)), (shadow[index + 3], shadow_alpha)] {
                assert!(
                    actual.abs_diff((expected * 255.0).round() as u8) <= 2,
                    "outline mismatch at ({x}, {y})"
                );
            }
        }
    }
}

#[test]
#[ignore = "requires an EGL rendering device"]
fn window_role_change_invalidates_decorations_and_preserves_the_resize_snapshot() {
    let mut renderer = renderer();
    let mut resources = RenderResources::default();
    let id = ferese_layout::WindowId(1);
    let context = renderer.context_id().erased();
    assert!(!resources.prepare_window_corners(id, CornerShape::Continuous));
    let programs = corner_program(&mut resources, &mut renderer, CornerShape::Continuous).unwrap();
    let parameters = BorderParameters {
        geometry: Rectangle::from_size((64, 64).into()),
        clip_rect: [0.0, 0.0, 64.0, 64.0],
        radius: 12.0,
        width: 2.0,
        color: [1.0; 4],
        color_to: [1.0; 4],
        gradient_line: [0.0; 4],
        focus_color: [1.0; 4],
        focus_color_to: [1.0; 4],
        focus_gradient_line: [0.0; 4],
        focus_mix: 0.0,
    };
    let element = SharedPixelShaderElement::new(
        programs.border,
        parameters.geometry,
        None,
        1.0,
        border_uniforms(&parameters),
        RenderElementKind::Unspecified,
    );
    resources
        .windows
        .get_mut(&id)
        .unwrap()
        .borders
        .contexts
        .insert(context.clone(), CachedBorder { element, parameters });
    let texture = renderer.create_buffer(Fourcc::Abgr8888, (64, 64).into()).unwrap();
    resources.set_snapshot(
        id,
        ResizeSnapshot {
            texture,
            context: context.clone(),
            id: Id::new(),
            commit: CommitCounter::default(),
            elapsed: Duration::ZERO,
            last_tick: None,
            scale: 1.0,
        },
    );
    let snapshot_id = resources.snapshot(&id).unwrap().id.clone();
    let commit = resources.snapshot(&id).unwrap().commit;
    assert!(!resources.prepare_window_corners(id, CornerShape::Continuous));
    assert_eq!(resources.windows[&id].borders.contexts.len(), 1);
    assert!(resources.prepare_window_corners(id, CornerShape::Circular));
    assert!(resources.windows[&id].borders.contexts.is_empty());
    let snapshot = resources.snapshot(&id).unwrap();
    assert_eq!(snapshot.id, snapshot_id);
    assert_ne!(snapshot.commit, commit);
    let updated_commit = snapshot.commit;
    assert!(!resources.prepare_window_corners(id, CornerShape::Circular));
    assert_eq!(resources.snapshot(&id).unwrap().commit, updated_commit);
}

fn endpoint_derivatives(points: Cubic, end: bool) -> (Point2, Point2) {
    if end {
        (
            [3.0 * (points[3][0] - points[2][0]), 3.0 * (points[3][1] - points[2][1])],
            [
                6.0 * (points[3][0] - 2.0 * points[2][0] + points[1][0]),
                6.0 * (points[3][1] - 2.0 * points[2][1] + points[1][1]),
            ],
        )
    } else {
        (
            [3.0 * (points[1][0] - points[0][0]), 3.0 * (points[1][1] - points[0][1])],
            [
                6.0 * (points[2][0] - 2.0 * points[1][0] + points[0][0]),
                6.0 * (points[2][1] - 2.0 * points[1][1] + points[0][1]),
            ],
        )
    }
}

fn curvature(tangent: Point2, acceleration: Point2) -> f64 {
    (tangent[0] * acceleration[1] - tangent[1] * acceleration[0]).abs() / tangent[0].hypot(tangent[1]).powi(3)
}

#[test]
fn measured_uikit_profile_has_flat_shoulders_but_small_internal_join_discontinuities() {
    let (start, start_acceleration) = endpoint_derivatives(APPLE[0], false);
    let (end, end_acceleration) = endpoint_derivatives(APPLE[2], true);
    assert_eq!(start[0], 0.0);
    assert_eq!(end[1], 0.0);
    assert_eq!(curvature(start, start_acceleration), 0.0);
    assert_eq!(curvature(end, end_acceleration), 0.0);

    for pair in APPLE.windows(2) {
        assert_eq!(pair[0][3], pair[1][0], "join must be position-continuous");
        let (before, before_acceleration) = endpoint_derivatives(pair[0], true);
        let (after, after_acceleration) = endpoint_derivatives(pair[1], false);
        let cosine =
            (before[0] * after[0] + before[1] * after[1]) / before[0].hypot(before[1]) / after[0].hypot(after[1]);
        let angle = cosine.clamp(-1.0, 1.0).acos().to_degrees();
        let curvature_jump = (curvature(before, before_acceleration) - curvature(after, after_acceleration)).abs();
        // This is a measured Apple-style approximation, not a globally G1/G2
        // curve. Preserve and document its limitation instead of falsely
        // passing a continuity test, or silently changing the source profile.
        assert!((angle - 2.453_166_93).abs() < 1.0e-6);
        assert!((curvature_jump - 0.354_989_57).abs() < 1.0e-6);
    }
}

#[test]
#[ignore = "manual EGL timing; optional FERESE_CORNER_REFERENCE_SHADER path"]
fn corner_heavy_shader_timing_sample() {
    use smithay::backend::renderer::gles::{UniformName, UniformType};
    let mut renderer = renderer();
    let reference = std::env::var_os("FERESE_CORNER_REFERENCE_SHADER")
        .map(|path| std::fs::read_to_string(path).expect("reference GLSL"));
    let current = include_str!("shaders/window_corners.glsl");
    let size = (1024, 768).into();
    let rect = Rectangle::<i32, Physical>::from_size(size);
    let mut texture: GlesTexture = renderer.create_buffer(Fourcc::Abgr8888, (1024, 768).into()).unwrap();
    let body = "\nvoid main() { float d = rounded_rect_distance(mod(gl_FragCoord.xy, vec2(64.0)), vec4(0.0, 0.0, 64.0, 64.0), radius); gl_FragColor = vec4(edge_coverage(d)); }";
    for (name, source) in reference
        .as_deref()
        .map(|s| ("reference", s))
        .into_iter()
        .chain([("optimized", current)])
    {
        let shader = format!("precision highp float; uniform float radius;\n{source}{body}");
        let program = renderer
            .compile_custom_pixel_shader(shader, &[UniformName::new("radius", UniformType::_1f)])
            .unwrap();
        for radius in [8.0f32, 16.0, 24.0] {
            let mut samples = Vec::new();
            for i in 0..110 {
                let start = std::time::Instant::now();
                let mut target = renderer.bind(&mut texture).unwrap();
                let mut frame = renderer.render(&mut target, size, Transform::Normal).unwrap();
                frame.clear(Color32F::TRANSPARENT, &[rect]).unwrap();
                frame
                    .render_pixel_shader_to(
                        &program,
                        Rectangle::from_size((1024.0, 768.0).into()),
                        rect,
                        (1024, 768).into(),
                        Some(&[rect]),
                        1.0,
                        &[Uniform::new("radius", radius)],
                    )
                    .unwrap();
                frame.finish().unwrap().wait().unwrap();
                if i >= 10 {
                    samples.push(start.elapsed());
                }
            }
            samples.sort_unstable();
            eprintln!(
                "corner-heavy {name} radius={radius}: median={:?}, p95={:?}",
                samples[50], samples[95]
            );
        }
    }
}
