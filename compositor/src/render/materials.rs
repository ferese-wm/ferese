use super::*;

#[allow(clippy::too_many_arguments)]
pub(super) fn append_material_surface(
    state: &mut Ferese,
    renderer: &mut GlesRenderer,
    output: &Output,
    surface: &smithay::reexports::wayland_server::protocol::wl_surface::WlSurface,
    geometry: Rectangle<i32, Logical>,
    content_origin: Point<i32, Logical>,
    scale: f64,
    output_crop: Rectangle<i32, Physical>,
    elements: &mut Vec<AnimatedWindowRenderElement>,
) {
    let radius =
        if crate::effects::surface_role(surface).is_some_and(|(role, _)| role == crate::effects::SemanticRole::Panel) {
            state.theme_settings.panel_radius
        } else {
            state.theme_settings.material_radius
        };
    let regions = crate::effects::surface_regions(surface);
    let opacities = crate::effects::surface_region_opacities(surface);
    let targets: Vec<_> = match &regions {
        None => vec![(geometry, RoundedRect::from_logical(geometry, scale, radius), 1.0)],
        Some(regions) => regions
            .iter()
            .enumerate()
            .filter_map(|(index, r)| {
                material_region_geometry(*r, geometry, content_origin, scale).map(|(rect, corners)| {
                    (
                        rect,
                        corners,
                        opacities.get(index).copied().unwrap_or(1000) as f32 / 1000.0,
                    )
                })
            })
            .collect(),
    };
    let materials: Vec<_> = targets
        .iter()
        .enumerate()
        .filter_map(|(index, (rect, corners, alpha))| {
            material_element(
                state,
                renderer,
                output,
                surface,
                MaterialSurface {
                    geometry: *rect,
                    corners: *corners,
                    index,
                    capture_geometry: if crate::effects::surface_role(surface)
                        .is_some_and(|(role, _)| role == crate::effects::SemanticRole::Panel)
                    {
                        *rect
                    } else {
                        geometry
                    },
                    alpha: *alpha,
                },
            )
        })
        .collect();
    if let Some(buffers) = state.render.surfaces.get_mut(surface) {
        buffers.contexts.retain(|(_, _, index), _| *index < targets.len());
        buffers.captures.retain(|(_, _, index), _| *index < targets.len());
    }
    let material = (!materials.is_empty() && regions.is_none()).then_some(());
    let clip = material.as_ref().and_then(|_| {
        let corners = targets.first()?.1;
        let mode = output.current_mode()?;
        let program = rounded_clip_program(&mut state.render, renderer)?;
        Some((
            program,
            framebuffer_clip_rect(corners.rect, mode.size, output.current_transform().invert()),
            corners.radius,
        ))
    });
    // Smithay element lists are front to back: content, material, shadow.
    let content = render_elements_from_surface_tree::<GlesRenderer, WaylandSurfaceRenderElement<GlesRenderer>>(
        renderer,
        surface,
        content_origin.to_physical_precise_round(scale),
        scale,
        crate::effects::surface_opacity(surface),
        RenderElementKind::Unspecified,
    );
    elements.extend(content.into_iter().filter_map(|element| {
        let origin = Point::<i32, Physical>::default();
        let element = if let Some((program, clip_rect, radius)) = &clip {
            WindowContentRenderElement::Rounded(RoundedSurfaceRenderElement {
                inner: element,
                programs: program.clone(),
                clip_rect: *clip_rect,
                radius: *radius,
                clip_changed: false,
                opaque_clip: None,
            })
        } else {
            WindowContentRenderElement::Popup(element)
        };
        let element = RescaleRenderElement::from_element(element, origin, 1.0);
        let element = RelocateRenderElement::from_element(element, origin, Relocate::Relative);
        CropRenderElement::from_element(element, scale, output_crop).map(Into::into)
    }));
    for (background, shadow) in materials {
        elements.push(background);
        elements.push(shadow);
    }
}

/// Preserve the surface-local fractional edges until the shared physical snap.
fn material_region_geometry(
    region: [f64; 5],
    geometry: Rectangle<i32, Logical>,
    origin: Point<i32, Logical>,
    scale: f64,
) -> Option<(Rectangle<i32, Logical>, RoundedRect)> {
    let rect = Rectangle::<f64, Logical>::new(
        (f64::from(origin.x) + region[0], f64::from(origin.y) + region[1]).into(),
        (region[2], region[3]).into(),
    )
    .intersection(geometry.to_f64())?;
    let corners = RoundedRect::new(
        ferese_layout::Rect::new(rect.loc.x, rect.loc.y, rect.size.w, rect.size.h),
        (0, 0).into(),
        scale,
        region[4],
    );
    Some((rect.to_i32_up(), corners))
}

/// Apply the same output transform as framebuffer_clip_rect to corner identity.
fn framebuffer_corner_radii(radii: [f32; 4], transform: Transform) -> [f32; 4] {
    let size = Size::<f64, Physical>::from((1., 1.));
    let mut transformed = [0.; 4];
    for (radius, point) in radii.into_iter().zip([(0., 0.), (1., 0.), (1., 1.), (0., 1.)]) {
        let point = transform.transform_point_in(Point::<f64, Physical>::from(point), &size);
        let index = match (point.x > 0.5, point.y > 0.5) {
            (false, false) => 0,
            (true, false) => 1,
            (true, true) => 2,
            (false, true) => 3,
        };
        transformed[index] = radius;
    }
    transformed
}

pub(super) struct MaterialSurface {
    pub geometry: Rectangle<i32, Logical>,
    pub corners: RoundedRect,
    pub index: usize,
    pub capture_geometry: Rectangle<i32, Logical>,
    pub alpha: f32,
}

pub(super) fn material_element(
    state: &mut Ferese,
    renderer: &mut GlesRenderer,
    output: &Output,
    surface: &smithay::reexports::wayland_server::protocol::wl_surface::WlSurface,
    surface_geometry: MaterialSurface,
) -> Option<(AnimatedWindowRenderElement, AnimatedWindowRenderElement)> {
    let (role, generation) = crate::effects::surface_role(surface)?;
    material_element_with_role(
        state,
        renderer,
        output,
        surface,
        surface_geometry,
        (role, generation, crate::effects::surface_opacity(surface)),
    )
}

pub(super) fn material_element_with_role(
    state: &mut Ferese,
    renderer: &mut GlesRenderer,
    output: &Output,
    surface: &smithay::reexports::wayland_server::protocol::wl_surface::WlSurface,
    surface_geometry: MaterialSurface,
    (role, generation, opacity): (crate::effects::SemanticRole, u64, f32),
) -> Option<(AnimatedWindowRenderElement, AnimatedWindowRenderElement)> {
    let MaterialSurface {
        geometry,
        corners,
        index,
        capture_geometry,
        alpha,
    } = surface_geometry;
    let mut material = crate::effects::resolve_material(
        role,
        state.theme_settings.material_style,
        state.theme_settings.shell_opacity as f32,
    );
    if role == crate::effects::SemanticRole::Panel
        && let Some(opacity) = state.panel_background_opacity
    {
        material.opacity = opacity;
    }
    let presentation_alpha = opacity * alpha;
    let mode = output.current_mode()?;
    let scale = output.current_scale().fractional_scale();
    let transform = output.current_transform().invert();
    let background = if role == crate::effects::SemanticRole::Panel {
        state.theme_settings.bar_background_color
    } else {
        state.theme_settings.surface_base_color
    };
    let [red, green, blue, _] = background.0;
    let logical_radii = if role == crate::effects::SemanticRole::Panel {
        state.panel_corner_radius
    } else {
        None
    };
    let radii = logical_radii.map_or([corners.radius; 4], |radii| {
        radii
            .0
            .map(|value| clamp_radius(f64::from(value) * scale, corners.rect.size))
    });
    let output_size = output
        .current_transform()
        .transform_size(mode.size)
        .to_f64()
        .to_logical(scale)
        .to_i32_ceil();
    let touches_top = role == crate::effects::SemanticRole::Panel && geometry.loc.y == 0;
    let touches_bottom =
        role == crate::effects::SemanticRole::Panel && geometry.loc.y + geometry.size.h >= output_size.h;
    let radii = ferese_config::panel::CornerRadii(radii)
        .at_top_edge(touches_top)
        .at_edge(ferese_config::panel::Edge::Bottom, touches_bottom)
        .0;
    let radius = radii.into_iter().fold(0., f32::max);
    let corner_radii = framebuffer_corner_radii(radii, transform);
    let [offset_y, shadow_blur, shadow_opacity] = material.shadow;
    let edge_bar = radius == 0.0 && (touches_top || touches_bottom);
    let shadow_opacity = if edge_bar {
        0.0
    } else {
        shadow_opacity * (state.theme_settings.shadow_opacity / 0.2) * f64::from(presentation_alpha)
    };
    let shadow_opacity = if role == crate::effects::SemanticRole::Panel {
        shadow_opacity * f64::from(material.opacity)
    } else {
        shadow_opacity
    };
    let shadow_geometry = Rectangle::new(
        (
            corners.rect.loc.x,
            corners.rect.loc.y + (offset_y * scale).round() as i32,
        )
            .into(),
        corners.rect.size,
    );
    let background_opacity = material.opacity;
    let blur = material_blur_radius(material.style, material.opacity, state.theme_settings.backdrop_blur);
    let sample_geometry = expanded_blur_region(capture_geometry, blur.ceil() as i32, output_size);
    let sample_physical = sample_geometry.to_physical_precise_round(scale);
    let mut parameters = MaterialParameters {
        corner_radii,
        presentation_alpha,
        background_opacity,
        blur: (blur * scale) as f32,
        sample_geometry,
        sample_physical,
        sample_framebuffer: framebuffer_clip_rect(sample_physical, mode.size, transform),
        shadow_rect: framebuffer_clip_rect(shadow_geometry, mode.size, transform),
        shadow_values: [(shadow_blur * scale) as f32, shadow_opacity as f32],
        shadow_bounds: shadow_bounds(geometry, offset_y, shadow_blur),
        geometry,
        tint: [red, green, blue, material.opacity * presentation_alpha],
        generation,
        opaque: material.opacity == 1.0 && presentation_alpha == 1.0 && radius == 0.0,
        visible_framebuffer: framebuffer_clip_rect(corners.rect, mode.size, transform),
    };
    let context = renderer.context_id().erased();
    let program = material_program(&mut state.render, renderer)?;
    let blur_program = (blur > 0.0)
        .then(|| blur_program(&mut state.render, renderer))
        .flatten();
    let capture_rect = framebuffer_capture_rect(parameters.sample_framebuffer);
    let capture_size = Size::from((capture_rect[2], capture_rect[3]));
    let output_id = state.output_id(output)?;
    let capture_key = (context.clone(), output_id, index);
    let buffers = state.render.surfaces.entry(surface.clone()).or_default();
    let capture = if let Some(blur_program) = blur_program {
        if buffers
            .captures
            .get(&capture_key)
            .is_none_or(|c| c.geometry != sample_geometry || c.texture.size() != capture_size)
        {
            let texture = Offscreen::<GlesTexture>::create_buffer(renderer, Fourcc::Abgr8888, capture_size)
                .map_err(|error| {
                    tracing::warn!(%error, ?sample_physical, "backdrop capture allocation failed");
                    error
                })
                .ok();
            if let Some(texture) = texture {
                buffers.captures.insert(
                    capture_key.clone(),
                    BlurCapture {
                        texture,
                        dirty: Arc::new(AtomicBool::new(true)),
                        geometry: sample_geometry,
                    },
                );
            } else {
                buffers.captures.remove(&capture_key);
            }
        }
        buffers
            .captures
            .get(&capture_key)
            .cloned()
            .map(|capture| (capture, blur_program))
    } else {
        buffers.captures.remove(&capture_key);
        None
    };
    if capture.is_some() {
        parameters.tint[3] = state.theme_settings.material_tint_strength as f32;
    }
    let make_element = || {
        if let Some((capture, program)) = &capture {
            MaterialElement::Blur(BlurRenderElement::new(
                capture.texture.clone(),
                program.0.clone(),
                &parameters,
                capture.dirty.clone(),
            ))
        } else {
            MaterialElement::Fill(SharedPixelShaderElement::new(
                program.0.clone(),
                geometry,
                parameters.opaque.then(|| vec![Rectangle::from_size(geometry.size)]),
                1.0,
                material_uniforms(&parameters),
                RenderElementKind::Unspecified,
            ))
        }
    };
    let cached = buffers
        .contexts
        .entry((context, output_id, index))
        .or_insert_with(|| CachedMaterial {
            element: make_element(),
            parameters: parameters.clone(),
            shadow: SharedPixelShaderElement::new(
                program.0.clone(),
                parameters.shadow_bounds,
                None,
                1.0,
                decoration_uniforms(&parameters, 2.0),
                RenderElementKind::Unspecified,
            ),
        });
    let replace = match &cached.element {
        MaterialElement::Fill(_) => capture.is_some(),
        MaterialElement::Blur(element) => capture
            .as_ref()
            .is_none_or(|(capture, _)| capture.texture.tex_id() != element.texture.tex_id()),
    };
    if replace {
        cached.element = make_element();
    }
    if cached.parameters != parameters {
        if cached.parameters.shadow_rect != parameters.shadow_rect
            || cached.parameters.shadow_values != parameters.shadow_values
            || cached.parameters.shadow_bounds != parameters.shadow_bounds
            || cached.parameters.corner_radii != parameters.corner_radii
            || cached.parameters.visible_framebuffer != parameters.visible_framebuffer
        {
            cached.shadow.resize(parameters.shadow_bounds, None);
            cached.shadow.update_uniforms(decoration_uniforms(&parameters, 2.0));
        }
        match &mut cached.element {
            MaterialElement::Fill(element) => {
                element.resize(
                    geometry,
                    parameters.opaque.then(|| vec![Rectangle::from_size(geometry.size)]),
                );
                element.update_uniforms(material_uniforms(&parameters));
            }
            MaterialElement::Blur(element) => element.update(&parameters),
        }
        cached.parameters = parameters;
    }
    let background = match &cached.element {
        // Keep the shader's opaque regions when its canvas already has the
        // shared edges. Animated modal geometry may need a physical override.
        MaterialElement::Fill(element) if element.geometry(scale.into()) == corners.rect => element.clone().into(),
        MaterialElement::Fill(element) => PhysicalShaderElement {
            inner: element.clone(),
            geometry: corners.rect,
        }
        .into(),
        MaterialElement::Blur(element) => element.clone().into(),
    };
    Some((background, cached.shadow.clone().into()))
}

pub(super) fn material_blur_radius(style: crate::config::MaterialStyle, opacity: f32, radius: f64) -> f64 {
    if style == crate::config::MaterialStyle::Translucent && opacity > 0.0 {
        radius
    } else {
        0.0
    }
}

pub(super) fn blur_program(resources: &mut RenderResources, renderer: &mut GlesRenderer) -> Option<BlurProgram> {
    let context = renderer.context_id().erased();
    if let Some(program) = resources
        .contexts
        .get(&context)
        .and_then(|programs| programs.blur.as_ref())
    {
        return Some(program.clone());
    }
    match renderer.compile_custom_texture_shader(corner_shader(BLUR_SHADER), &blur_uniform_names()) {
        Ok(program) => {
            let program = BlurProgram(program);
            resources.contexts.entry(context).or_default().blur = Some(program.clone());
            Some(program)
        }
        Err(error) => {
            tracing::warn!(%error,"backdrop blur unavailable; using plain translucent fill");
            None
        }
    }
}

pub(super) fn blur_uniform_names() -> [UniformName<'static>; 8] {
    [
        UniformName::new("visible_rect", UniformType::_4f),
        UniformName::new("material_radii", UniformType::_4f),
        UniformName::new("texture_size", UniformType::_2f),
        UniformName::new("capture_origin", UniformType::_2f),
        UniformName::new("blur_radius", UniformType::_1f),
        UniformName::new("presentation_alpha", UniformType::_1f),
        UniformName::new("background_opacity", UniformType::_1f),
        UniformName::new("tint", UniformType::_4f),
    ]
}

pub(super) fn blur_uniforms(p: &MaterialParameters) -> Vec<Uniform<'static>> {
    vec![
        Uniform::new("visible_rect", p.visible_framebuffer),
        Uniform::new("material_radii", p.corner_radii),
        Uniform::new("texture_size", [p.sample_framebuffer[2], p.sample_framebuffer[3]]),
        Uniform::new("capture_origin", [p.sample_framebuffer[0], p.sample_framebuffer[1]]),
        Uniform::new("blur_radius", p.blur),
        Uniform::new("presentation_alpha", p.presentation_alpha),
        Uniform::new("background_opacity", p.background_opacity),
        Uniform::new("tint", p.tint),
    ]
}

pub(super) fn framebuffer_capture_rect(rect: [f32; 4]) -> [i32; 4] {
    rect.map(|v| v.round() as i32)
}

pub(super) fn expanded_blur_region(
    visible: Rectangle<i32, Logical>,
    radius: i32,
    output_size: Size<i32, Logical>,
) -> Rectangle<i32, Logical> {
    let left = (visible.loc.x - radius).max(0);
    let top = (visible.loc.y - radius).max(0);
    let right = (visible.loc.x + visible.size.w + radius).min(output_size.w);
    let bottom = (visible.loc.y + visible.size.h + radius).min(output_size.h);
    Rectangle::new(
        (left, top).into(),
        ((right - left).max(1), (bottom - top).max(1)).into(),
    )
}

pub(super) fn material_program(
    resources: &mut RenderResources,
    renderer: &mut GlesRenderer,
) -> Option<MaterialProgram> {
    material_program_for_corners(resources, renderer, CornerShape::Continuous)
}

pub(super) fn material_program_for_corners(
    resources: &mut RenderResources,
    renderer: &mut GlesRenderer,
    shape: CornerShape,
) -> Option<MaterialProgram> {
    let context = renderer.context_id().erased();
    if let Some(program) = resources.contexts.get(&context).and_then(|programs| match shape {
        CornerShape::Circular => programs.material.as_ref(),
        CornerShape::Continuous => programs.window_material.as_ref(),
    }) {
        return Some(program.clone());
    }

    let uniforms = [
        UniformName::new("visible_rect", UniformType::_4f),
        UniformName::new("material_radii", UniformType::_4f),
        UniformName::new("tint", UniformType::_4f),
        UniformName::new("paint_mode", UniformType::_1f),
        UniformName::new("shadow_rect", UniformType::_4f),
        UniformName::new("shadow_values", UniformType::_2f),
    ];
    match renderer.compile_custom_pixel_shader(corner_shader_for(MATERIAL_SHADER, shape), &uniforms) {
        Ok(program) => {
            let program = MaterialProgram(program);
            let context = resources.contexts.entry(context).or_default();
            match shape {
                CornerShape::Circular => context.material = Some(program.clone()),
                CornerShape::Continuous => context.window_material = Some(program.clone()),
            }
            Some(program)
        }
        Err(error) => {
            tracing::error!(%error, "failed to compile semantic material shader");
            None
        }
    }
}

pub(super) fn material_uniforms(parameters: &MaterialParameters) -> Vec<Uniform<'static>> {
    decoration_uniforms(parameters, 0.0)
}

pub(super) fn decoration_uniforms(parameters: &MaterialParameters, paint_mode: f32) -> Vec<Uniform<'static>> {
    vec![
        Uniform::new("paint_mode", paint_mode),
        Uniform::new("shadow_rect", parameters.shadow_rect),
        Uniform::new("shadow_values", parameters.shadow_values),
        Uniform::new("visible_rect", parameters.visible_framebuffer),
        Uniform::new("material_radii", parameters.corner_radii),
        Uniform::new("tint", parameters.tint),
    ]
}

pub(super) fn blur_damage(
    size: Size<i32, Physical>,
    current: CommitCounter,
    previous: Option<CommitCounter>,
) -> DamageSet<i32, Physical> {
    if previous == Some(current) {
        DamageSet::default()
    } else {
        // A new scene needs its entire sampling halo repainted before capture.
        DamageSet::from_slice(&[Rectangle::from_size((size.w, size.h).into())])
    }
}

#[cfg(test)]
mod region_geometry_tests {
    use super::*;

    #[test]
    fn framebuffer_transforms_preserve_each_logical_corner() {
        let radii = [1., 2., 3., 4.];
        assert_eq!(framebuffer_corner_radii(radii, Transform::Normal), radii);
        assert_eq!(framebuffer_corner_radii(radii, Transform::Flipped180), [4., 3., 2., 1.]);
        for (transform, expected) in [
            (Transform::Normal, [1., 2., 3., 4.]),
            (Transform::_90, [4., 1., 2., 3.]),
            (Transform::_180, [3., 4., 1., 2.]),
            (Transform::_270, [2., 3., 4., 1.]),
            (Transform::Flipped, [2., 1., 4., 3.]),
            (Transform::Flipped90, [1., 4., 3., 2.]),
            (Transform::Flipped180, [4., 3., 2., 1.]),
            (Transform::Flipped270, [3., 2., 1., 4.]),
        ] {
            assert_eq!(framebuffer_corner_radii(radii, transform), expected);
        }
    }

    #[test]
    fn fractional_regions_snap_once_in_output_pixels() {
        let bounds = Rectangle::from_size((200, 100).into());
        let (_, corners) = material_region_geometry([0.4, 0.4, 100.4, 40.4, 14.0], bounds, (0, 0).into(), 1.5).unwrap();
        assert_eq!(corners.rect, Rectangle::new((1, 1).into(), (150, 60).into()));
        assert_eq!(corners.radius, 21.0);
        // The old logical rounding would put this at physical (0, 0).
        let (_, clipped) =
            material_region_geometry([-0.4, -0.4, 100.4, 40.4, 100.0], bounds, (0, 0).into(), 1.5).unwrap();
        assert_eq!(clipped.rect, Rectangle::from_size((150, 60).into()));
        assert_eq!(clipped.radius, 30.0);
        assert!(material_region_geometry([300.0, 0.0, 10.0, 10.0, 2.0], bounds, (0, 0).into(), 1.5).is_none());
    }
}
