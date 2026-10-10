use super::*;

pub(super) fn rounded_clip_program(
    resources: &mut RenderResources,
    renderer: &mut GlesRenderer,
) -> Option<RoundedClipPrograms> {
    corner_program(resources, renderer, CornerShape::Continuous)
}

pub(super) fn corner_program(
    resources: &mut RenderResources,
    renderer: &mut GlesRenderer,
    shape: CornerShape,
) -> Option<RoundedClipPrograms> {
    let context = renderer.context_id().erased();
    if let Some(program) = resources.contexts.get(&context).and_then(|programs| match shape {
        CornerShape::Circular => programs.rounded.as_ref(),
        CornerShape::Continuous => programs.continuous.as_ref(),
    }) {
        return Some(program.clone());
    }

    let texture_uniforms = [
        UniformName::new("clip_rect", UniformType::_4f),
        UniformName::new("radius", UniformType::_1f),
    ];
    let border_uniforms = [
        UniformName::new("clip_rect", UniformType::_4f),
        UniformName::new("radius", UniformType::_1f),
        UniformName::new("border_width", UniformType::_1f),
        UniformName::new("border_color", UniformType::_4f),
        UniformName::new("border_color_to", UniformType::_4f),
        UniformName::new("gradient_line", UniformType::_4f),
        UniformName::new("focus_color", UniformType::_4f),
        UniformName::new("focus_color_to", UniformType::_4f),
        UniformName::new("focus_gradient_line", UniformType::_4f),
        UniformName::new("focus_mix", UniformType::_1f),
    ];
    let shadow_uniforms = [
        UniformName::new("shadow_rect", UniformType::_4f),
        UniformName::new("radius", UniformType::_1f),
        UniformName::new("blur", UniformType::_1f),
        UniformName::new("opacity", UniformType::_1f),
        UniformName::new("shadow_color", UniformType::_4f),
    ];
    let texture =
        renderer.compile_custom_texture_shader(corner_shader_for(ROUNDED_TEXTURE_SHADER, shape), &texture_uniforms);
    let border =
        renderer.compile_custom_pixel_shader(corner_shader_for(ROUNDED_BORDER_SHADER, shape), &border_uniforms);
    let shadow = renderer.compile_custom_pixel_shader(corner_shader_for(WINDOW_SHADOW_SHADER, shape), &shadow_uniforms);
    let solid = renderer.compile_custom_pixel_shader(
        corner_shader_for(ROUNDED_SOLID_SHADER, shape),
        &[
            UniformName::new("clip_rect", UniformType::_4f),
            UniformName::new("radius", UniformType::_1f),
            UniformName::new("color", UniformType::_4f),
        ],
    );
    let compiled = texture.and_then(|texture| {
        border.and_then(|border| shadow.and_then(|shadow| solid.map(|solid| (texture, border, shadow, solid))))
    });
    match compiled {
        Ok((texture, border, shadow, solid)) => {
            let programs = RoundedClipPrograms {
                shape,
                texture,
                solid,
                border,
                shadow,
            };
            let context = resources.contexts.entry(context).or_default();
            match shape {
                CornerShape::Circular => context.rounded = Some(programs.clone()),
                CornerShape::Continuous => context.continuous = Some(programs.clone()),
            }
            Some(programs)
        }
        Err(error) => {
            let context = resources.contexts.entry(context).or_default();
            let warned = match shape {
                CornerShape::Circular => &mut context.rounded_warned,
                CornerShape::Continuous => &mut context.continuous_warned,
            };

            if !std::mem::replace(warned, true) {
                tracing::warn!(%error, "rounded-window shader unavailable; falling back to unrounded rendering");
            }
            None
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn window_border_element(
    resources: &mut RenderResources,
    theme: &crate::config::ThemeSettings,
    renderer: &GlesRenderer,
    id: ferese_layout::WindowId,
    geometry: Rectangle<i32, Logical>,
    corners: RoundedRect,
    scale: f64,
    requested_width: f64,
    color: [f32; 4],
    gradient: Option<crate::config::BorderGradient>,
    focus_mix: f32,
    opacity: f32,
    output: &Output,
    programs: &RoundedClipPrograms,
) -> Option<PhysicalShaderElement> {
    let mode = output.current_mode()?;
    let width = clamp_radius(requested_width * scale, corners.rect.size);
    if width == 0.0 {
        return None;
    }

    let (from, to, gradient_line) = match gradient {
        Some(gradient) => (
            gradient.from.0,
            gradient.to.0,
            border_gradient_line(
                corners.rect,
                mode.size,
                output.current_transform().invert(),
                gradient.angle,
            ),
        ),
        None => (color, color, [0.0; 4]),
    };
    let (focus_from, focus_to, focus_gradient_line) = match theme.focus_ring_gradient {
        Some(g) => (
            g.from.0,
            g.to.0,
            border_gradient_line(corners.rect, mode.size, output.current_transform().invert(), g.angle),
        ),
        None => (theme.accent_color.0, theme.accent_color.0, [0.0; 4]),
    };
    let parameters = BorderParameters {
        geometry,
        clip_rect: framebuffer_clip_rect(corners.rect, mode.size, output.current_transform().invert()),
        radius: corners.radius,
        width,
        color: color_with_alpha(from, opacity),
        color_to: color_with_alpha(to, opacity),
        gradient_line,
        focus_color: color_with_alpha(focus_from, opacity),
        focus_color_to: color_with_alpha(focus_to, opacity),
        focus_gradient_line,
        focus_mix,
    };
    let context = renderer.context_id().erased();
    let buffers = &mut resources.windows.entry(id).or_default().borders;
    if !buffers.contexts.contains_key(&context) {
        let element = SharedPixelShaderElement::new(
            programs.border.clone(),
            geometry,
            None,
            1.0,
            border_uniforms(&parameters),
            RenderElementKind::Unspecified,
        );
        buffers.contexts.insert(
            context.clone(),
            CachedBorder {
                element,
                parameters: parameters.clone(),
            },
        );
    }

    let cached = buffers.contexts.get_mut(&context)?;
    if cached.parameters != parameters {
        if cached.parameters.geometry != parameters.geometry {
            cached.element.resize(parameters.geometry, None);
        }
        cached.element.update_uniforms(border_uniforms(&parameters));
        cached.parameters = parameters;
    }

    Some(PhysicalShaderElement {
        inner: cached.element.clone(),
        geometry: corners.rect,
    })
}

pub(super) fn border_uniforms(parameters: &BorderParameters) -> Vec<Uniform<'static>> {
    vec![
        Uniform::new("clip_rect", parameters.clip_rect),
        Uniform::new("radius", parameters.radius),
        Uniform::new("border_width", parameters.width),
        Uniform::new("border_color", parameters.color),
        Uniform::new("border_color_to", parameters.color_to),
        Uniform::new("gradient_line", parameters.gradient_line),
        Uniform::new("focus_color", parameters.focus_color),
        Uniform::new("focus_color_to", parameters.focus_color_to),
        Uniform::new("focus_gradient_line", parameters.focus_gradient_line),
        Uniform::new("focus_mix", parameters.focus_mix),
    ]
}

#[allow(clippy::too_many_arguments)]
pub(super) fn window_tint_element(
    resources: &mut RenderResources,
    renderer: &mut GlesRenderer,
    id: ferese_layout::WindowId,
    geometry: Rectangle<i32, Logical>,
    corners: RoundedRect,
    color: [f32; 4],
    resize_fill: bool,
    output: &Output,
) -> Option<PhysicalShaderElement> {
    if color[3] <= 0.0 {
        if resize_fill {
            resources.clear_tint(id, true);
        } else {
            resources.clear_tint(id, false);
        }
        return None;
    }
    let mode = output.current_mode()?;
    let program = material_program_for_corners(resources, renderer, corners.shape)?;
    let parameters = BorderParameters {
        geometry,
        clip_rect: framebuffer_clip_rect(corners.rect, mode.size, output.current_transform().invert()),
        radius: corners.radius,
        width: 0.0,
        color,
        color_to: color,
        gradient_line: [0.0; 4],
        focus_color: color,
        focus_color_to: color,
        focus_gradient_line: [0.0; 4],
        focus_mix: 0.0,
    };
    let uniforms = |p: &BorderParameters| {
        vec![
            Uniform::new("visible_rect", p.clip_rect),
            Uniform::new("material_radii", [p.radius; 4]),
            Uniform::new("tint", p.color),
            Uniform::new("paint_mode", 0.0_f32),
            Uniform::new("shadow_rect", p.clip_rect),
            Uniform::new("shadow_values", [0.0_f32; 2]),
        ]
    };
    let context = renderer.context_id().erased();
    let buffers = if resize_fill {
        &mut resources.windows.entry(id).or_default().resize_fill
    } else {
        &mut resources.windows.entry(id).or_default().dim
    };
    let cached = buffers.contexts.entry(context).or_insert_with(|| CachedBorder {
        element: SharedPixelShaderElement::new(
            program.0,
            geometry,
            None,
            1.0,
            uniforms(&parameters),
            RenderElementKind::Unspecified,
        ),
        parameters: parameters.clone(),
    });
    if cached.parameters != parameters {
        if cached.parameters.geometry != geometry {
            cached.element.resize(geometry, None);
        }
        // Updating uniforms advances the element's commit so unchanged client
        // buffers still repaint while focus dimming animates.
        cached.element.update_uniforms(uniforms(&parameters));
        cached.parameters = parameters;
    }
    Some(PhysicalShaderElement {
        inner: cached.element.clone(),
        geometry: corners.rect,
    })
}

#[allow(clippy::too_many_arguments)]
pub(super) fn window_shadow_element(
    resources: &mut RenderResources,
    renderer: &GlesRenderer,
    id: ferese_layout::WindowId,
    geometry: Rectangle<i32, Logical>,
    corners: RoundedRect,
    scale: f64,
    offset_y: f64,
    blur: f64,
    opacity: f64,
    color: [f32; 4],
    output: &Output,
    programs: &RoundedClipPrograms,
) -> Option<PhysicalShaderElement> {
    let mode = output.current_mode()?;
    if opacity == 0.0 || color[3] == 0.0 {
        return None;
    }

    let shadow_geometry = Rectangle::new(
        (
            corners.rect.loc.x,
            corners.rect.loc.y + (offset_y * scale).round() as i32,
        )
            .into(),
        corners.rect.size,
    );
    let bounds = shadow_bounds(geometry, offset_y, blur);
    let parameters = ShadowParameters {
        blur: (blur * scale) as f32,
        bounds,
        shadow_rect: framebuffer_clip_rect(shadow_geometry, mode.size, output.current_transform().invert()),
        radius: corners.radius,
        opacity: opacity as f32,
        color,
    };
    let context = renderer.context_id().erased();
    let buffers = &mut resources.windows.entry(id).or_default().shadow;
    if !buffers.contexts.contains_key(&context) {
        let element = SharedPixelShaderElement::new(
            programs.shadow.clone(),
            bounds,
            None,
            1.0,
            shadow_uniforms(&parameters),
            RenderElementKind::Unspecified,
        );
        buffers.contexts.insert(
            context.clone(),
            CachedShadow {
                element,
                parameters: parameters.clone(),
            },
        );
    }

    let cached = buffers.contexts.get_mut(&context)?;
    if cached.parameters != parameters {
        if cached.parameters.bounds != parameters.bounds {
            cached.element.resize(parameters.bounds, None);
        }
        cached.element.update_uniforms(shadow_uniforms(&parameters));
        cached.parameters = parameters;
    }

    let extent = (blur * scale * 2.0).ceil() as i32;
    Some(PhysicalShaderElement {
        inner: cached.element.clone(),
        geometry: Rectangle::new(
            (shadow_geometry.loc.x - extent, shadow_geometry.loc.y - extent).into(),
            (shadow_geometry.size.w + 2 * extent, shadow_geometry.size.h + 2 * extent).into(),
        ),
    })
}

pub(super) fn shadow_uniforms(parameters: &ShadowParameters) -> Vec<Uniform<'static>> {
    vec![
        Uniform::new("shadow_rect", parameters.shadow_rect),
        Uniform::new("radius", parameters.radius),
        Uniform::new("blur", parameters.blur),
        Uniform::new("opacity", parameters.opacity),
        Uniform::new("shadow_color", parameters.color),
    ]
}

pub(super) fn shadow_bounds(geometry: Rectangle<i32, Logical>, offset_y: f64, blur: f64) -> Rectangle<i32, Logical> {
    let extent = (blur * 2.0).ceil() as i32;
    let offset_y = offset_y.round() as i32;

    Rectangle::new(
        (geometry.loc.x - extent, geometry.loc.y + offset_y - extent).into(),
        (geometry.size.w + extent * 2, geometry.size.h + extent * 2).into(),
    )
}

#[allow(clippy::too_many_arguments)]
pub(super) fn rounded_window_elements(
    renderer: &mut GlesRenderer,
    window: &smithay::desktop::Window,
    corners: RoundedRect,
    scale: f64,
    alpha: f32,
    clip_changed: bool,
    output: &Output,
    programs: RoundedClipPrograms,
    behavior: ConstrainScaleBehavior,
    native_size: Option<ferese_animation::ClientSize>,
) -> Vec<AnimatedWindowRenderElement> {
    let Some(toplevel) = window.toplevel() else {
        return Vec::new();
    };
    let Some(mode) = output.current_mode() else {
        return Vec::new();
    };

    let physical_constrain = corners.rect;
    let geometry = window.geometry();
    let mut reference = geometry.to_physical_precise_round(scale);
    let behavior = if let Some(size) = native_size {
        reference.size = Size::<i32, Logical>::from((size.width, size.height)).to_physical_precise_round(scale);
        ConstrainScaleBehavior::Stretch
    } else {
        behavior
    };
    let location = physical_constrain.loc - geometry.loc.to_physical_precise_round(scale);
    let clip = framebuffer_clip_rect(physical_constrain, mode.size, output.current_transform().invert());
    let radius = corners.radius;
    let surface = toplevel.wl_surface();

    let mut content = PopupManager::popups_for_surface(surface)
        .flat_map(|(popup, popup_offset)| {
            let offset = (geometry.loc + popup_offset - popup.geometry().loc).to_physical_precise_round(scale);

            render_elements_from_surface_tree::<GlesRenderer, WaylandSurfaceRenderElement<GlesRenderer>>(
                renderer,
                popup.wl_surface(),
                location + offset,
                scale,
                alpha,
                RenderElementKind::Unspecified,
            )
            .into_iter()
            .map(WindowContentRenderElement::from)
        })
        .collect::<Vec<_>>();
    content.extend(
        render_elements_from_surface_tree::<GlesRenderer, WaylandSurfaceRenderElement<GlesRenderer>>(
            renderer,
            surface,
            location,
            scale,
            alpha,
            RenderElementKind::Unspecified,
        )
        .into_iter()
        .map(|inner| {
            if radius == 0.0 {
                // Rectangular clipping is already enforced by the outer crop
                // element. Keep the raw surface available for fullscreen scanout.
                WindowContentRenderElement::from(inner)
            } else {
                RoundedSurfaceRenderElement {
                    inner,
                    programs: programs.clone(),
                    clip_rect: clip,
                    radius,
                    clip_changed,
                    opaque_clip: matches!(behavior, ConstrainScaleBehavior::CutOff).then_some(physical_constrain),
                }
                .into()
            }
        }),
    );

    constrain_render_elements(
        content,
        location,
        physical_constrain,
        reference,
        behavior,
        ConstrainAlign::TOP | ConstrainAlign::LEFT,
        scale,
    )
    .map(Into::into)
    .collect()
}

pub(super) fn resize_content_behavior(intentional_scale: bool) -> ConstrainScaleBehavior {
    if intentional_scale {
        ConstrainScaleBehavior::Stretch
    } else {
        ConstrainScaleBehavior::CutOff
    }
}

#[cfg(test)]
pub(super) use crate::presentation::scaled_visual_rect;

pub(super) fn color_with_alpha(mut color: [f32; 4], alpha: f32) -> [f32; 4] {
    color[3] *= alpha;
    color
}

pub(super) fn rounded_visual_rect(
    rect: ferese_layout::Rect,
    output_location: Point<i32, Logical>,
) -> Rectangle<i32, Logical> {
    let left = (rect.x - f64::from(output_location.x)).round() as i32;
    let top = (rect.y - f64::from(output_location.y)).round() as i32;
    let right = (rect.x + rect.width - f64::from(output_location.x)).round() as i32;
    let bottom = (rect.y + rect.height - f64::from(output_location.y)).round() as i32;

    Rectangle::new(
        (left, top).into(),
        ((right - left).max(1), (bottom - top).max(1)).into(),
    )
}

pub(super) fn framebuffer_clip_rect(
    geometry: Rectangle<i32, Physical>,
    output_size: smithay::utils::Size<i32, Physical>,
    transform: Transform,
) -> [f32; 4] {
    // Match GlesFrame's projection: Smithay already accounts for GL's Y axis.
    // An extra bottom-left conversion here mirrors shader clips independently
    // of the surface geometry (notably with the nested Flipped180 output).
    let element_size = transform.transform_size(output_size);
    let transformed = transform.transform_rect_in(geometry, &element_size);

    [
        transformed.loc.x as f32,
        transformed.loc.y as f32,
        transformed.size.w as f32,
        transformed.size.h as f32,
    ]
}

pub(super) fn border_gradient_line(
    geometry: Rectangle<i32, Physical>,
    output_size: Size<i32, Physical>,
    transform: Transform,
    angle: f64,
) -> [f32; 4] {
    let angle = angle.to_radians();
    let direction = (angle.cos(), angle.sin());
    let width = f64::from(geometry.size.w);
    let height = f64::from(geometry.size.h);
    let extent = (width * direction.0.abs() + height * direction.1.abs()) * 0.5;
    let center = geometry.loc.to_f64() + Point::from((width * 0.5, height * 0.5));
    let offset: Point<f64, Physical> = (direction.0 * extent, direction.1 * extent).into();
    // Apply the same output-to-framebuffer transform as the rounded clip.
    // This keeps angles in window coordinates on rotated/flipped monitors.
    let area = transform.transform_size(output_size).to_f64();
    let start = transform.transform_point_in(center - offset, &area);
    let end = transform.transform_point_in(center + offset, &area);
    let delta = end - start;
    let length_squared = (delta.x * delta.x + delta.y * delta.y).max(0.000001);
    [
        start.x as f32,
        start.y as f32,
        (delta.x / length_squared) as f32,
        (delta.y / length_squared) as f32,
    ]
}
