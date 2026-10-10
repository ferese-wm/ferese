use super::*;

#[allow(clippy::too_many_arguments)]
pub(super) fn overview_chrome_element(
    state: &mut Ferese,
    renderer: &mut GlesRenderer,
    output: &Output,
    output_geometry: Rectangle<i32, Logical>,
    scale: f64,
    key: (u64, OverviewChromePart),
    rect: ferese_layout::Rect,
    color: [f32; 4],
    width: f64,
    radius: f64,
) -> Option<PhysicalShaderElement> {
    let output_id = state.output_id(output)?;
    let geometry = rounded_visual_rect(rect, output_geometry.loc);
    let corners = RoundedRect::new(rect, output_geometry.loc, scale, radius);
    let mode = output.current_mode()?;
    let parameters = BorderParameters {
        geometry,
        clip_rect: framebuffer_clip_rect(corners.rect, mode.size, output.current_transform().invert()),
        radius: corners.radius,
        width: (width * scale) as f32,
        color,
        color_to: color,
        gradient_line: [0.0; 4],
        focus_color: color,
        focus_color_to: color,
        focus_gradient_line: [0.0; 4],
        focus_mix: 0.0,
    };
    let program = if key.1 == OverviewChromePart::Outline {
        rounded_clip_program(&mut state.render, renderer)?.border
    } else {
        material_program(&mut state.render, renderer)?.0
    };
    let uniforms = |p: &BorderParameters| {
        if key.1 == OverviewChromePart::Outline {
            border_uniforms(p)
        } else {
            vec![
                Uniform::new("visible_rect", p.clip_rect),
                Uniform::new("material_radii", [p.radius; 4]),
                Uniform::new("tint", p.color),
                Uniform::new("paint_mode", 0.0_f32),
                Uniform::new("shadow_rect", p.clip_rect),
                Uniform::new("shadow_values", [0.0_f32; 2]),
            ]
        }
    };
    let context = renderer.context_id().erased();
    let chrome = &mut state
        .render
        .outputs
        .entry(output_id)
        .or_insert_with(|| OverviewScrim { chrome: HashMap::new() })
        .chrome;
    let cached = chrome
        .entry(key)
        .or_default()
        .entry(context)
        .or_insert_with(|| CachedBorder {
            element: SharedPixelShaderElement::new(
                program,
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
        cached.element.update_uniforms(uniforms(&parameters));
        cached.parameters = parameters;
    }
    Some(PhysicalShaderElement {
        inner: cached.element.clone(),
        geometry: corners.rect,
    })
}

pub(super) fn overview_strip_elements(
    state: &mut Ferese,
    renderer: &mut GlesRenderer,
    output: &Output,
    output_geometry: Rectangle<i32, Logical>,
    scale: f64,
    frame: &crate::state::FrameScene,
    capture: bool,
) -> Vec<AnimatedWindowRenderElement> {
    let alpha = frame.overview.opacity();
    if alpha <= 0.001 {
        return Vec::new();
    }
    let Some(bounds) = state.output_bounds_for(output) else {
        return Vec::new();
    };
    let cards = state.overview_workspace_cards_for_frame(output, Some(frame));
    let strip = crate::overview::workspace_strip(bounds);
    let panel = match (cards.first(), cards.last()) {
        (Some(first), Some(last)) => ferese_layout::Rect::new(
            first.rect.x - 12.0,
            strip.y,
            last.rect.x + last.rect.width - first.rect.x + 24.0,
            strip.height,
        ),
        _ => strip,
    };
    let radius = state.theme_settings.material_radius;
    let material_opacity = crate::effects::resolve_material(
        crate::effects::SemanticRole::Panel,
        state.theme_settings.material_style,
        state.theme_settings.shell_opacity as f32,
    )
    .opacity;
    let mut elements = Vec::new();
    state.retain_overview_labels();

    let card_ids = cards
        .iter()
        .map(|card| card.workspace.0)
        .collect::<std::collections::HashSet<_>>();
    if let Some(output_id) = state.output_id(output)
        && let Some(cache) = state.render.outputs.get_mut(&output_id)
    {
        cache.chrome.retain(|(id, part), _| match part {
            OverviewChromePart::Caption => state.windows.window(ferese_layout::WindowId(*id)).is_some(),
            OverviewChromePart::Card | OverviewChromePart::Outline => card_ids.contains(id),
            OverviewChromePart::Strip => true,
        });
    }
    // Cached title textures are independent of window content and stay readable
    // when thumbnails are small. Their pills use the shell's material and radius.
    // Match the grid's stable front-to-back order. A sampled frame's HashMap
    // has a fresh seed; iterating it changes z indices and damages still captions.
    let caption_ids = state.windows.ordered_ids().rev().collect::<Vec<_>>();
    for id in caption_ids {
        if (capture
            && state
                .windows
                .window(id)
                .is_none_or(|window| state.capture_protected(window)))
            || !state.window_belongs_to_output(id, output)
        {
            continue;
        }
        let Some(rect) = frame.windows.get(&id).map(|sample| sample.presentation.bounds.current) else {
            continue;
        };
        let Some((buffer, size)) = state.overview_window_label(id, scale, rect.width) else {
            continue;
        };
        let width = (f64::from(size.w) / scale + 16.0).min(rect.width);
        let caption = ferese_layout::Rect::new(
            rect.x + (rect.width - width) * 0.5,
            rect.y + rect.height + 8.0,
            width,
            26.0,
        );
        let location = Point::<i32, Physical>::from((
            ((caption.x - f64::from(output_geometry.loc.x) + caption.width * 0.5) * scale).round() as i32 - size.w / 2,
            ((caption.y - f64::from(output_geometry.loc.y) + 4.0) * scale).round() as i32,
        ));
        if let Ok(element) = MemoryRenderBufferRenderElement::from_buffer(
            renderer,
            location.to_f64(),
            &buffer,
            Some(alpha),
            Some(Rectangle::from_size((f64::from(size.w), f64::from(size.h)).into())),
            Some(
                (
                    (f64::from(size.w) / scale).round() as i32,
                    (f64::from(size.h) / scale).round() as i32,
                )
                    .into(),
            ),
            RenderElementKind::Unspecified,
        ) {
            let element = RescaleRenderElement::from_element(element, location, 1.0);
            let element = RelocateRenderElement::from_element(element, (0, 0), Relocate::Relative);
            if let Some(element) =
                CropRenderElement::from_element(element, scale, physical_rect(caption, output_geometry.loc, scale))
            {
                elements.push(element.into());
            }
        }
        let mut fill = state.theme_settings.surface_base_color.0;
        fill[3] = alpha * material_opacity;
        if let Some(background) = overview_chrome_element(
            state,
            renderer,
            output,
            output_geometry,
            scale,
            (id.0, OverviewChromePart::Caption),
            caption,
            fill,
            0.0,
            radius.min(13.0),
        ) {
            elements.push(background.into());
        }
    }
    for card in cards {
        // Front-to-back: outline and live miniatures above each card, all above
        // the strip. Nothing is painted behind the main overview window grid.
        if card.selected {
            let mut color = state.theme_settings.accent_color.0;
            color[3] *= alpha;
            if let Some(border) = overview_chrome_element(
                state,
                renderer,
                output,
                output_geometry,
                scale,
                (card.workspace.0, OverviewChromePart::Outline),
                card.rect,
                color,
                1.0,
                radius,
            ) {
                elements.push(border.into());
            }
        }
        for (id, rect) in &card.windows {
            if let Some(window) = state.windows.window(*id) {
                if capture && state.capture_protected(window) {
                    continue;
                }
                let shape = window_corner_shape(window);

                if let Some(programs) = corner_program(&mut state.render, renderer, shape) {
                    let clip_changed = state
                        .output_id(output)
                        .is_some_and(|output| state.render.thumbnail_corner_changed(*id, output, shape));
                    let Some(sample) = frame.windows.get(id) else { continue };
                    let presentation = sample.presentation.thumbnail(*rect);
                    let corners = RoundedRect::new(presentation.bounds.current, output_geometry.loc, scale, 4.0)
                        .with_shape(shape);
                    if let Some(snapshot) = state.render.snapshot(id)
                        && snapshot.context == renderer.context_id().erased()
                        && (snapshot.scale - scale).abs() < 0.001
                        && let Some(element) = super::window_content::snapshot_element(
                            snapshot,
                            presentation,
                            corners,
                            scale,
                            output,
                            &programs,
                            alpha * crate::presentation::handoff_alpha(snapshot.elapsed),
                        )
                    {
                        elements.push(element.into());
                    }
                    elements.extend(rounded_window_elements(
                        renderer,
                        window,
                        corners,
                        scale,
                        alpha * presentation.alpha(),
                        // Crop/scale/alpha/geometry already track their own
                        // damage. Role changes also alter the corner shader,
                        // even when the client buffer itself is unchanged.
                        clip_changed,
                        output,
                        programs.clone(),
                        ConstrainScaleBehavior::Stretch,
                        None,
                    ));
                }
            }
        }
        if let Some((buffer, size)) = state.overview_workspace_label(card.index, scale) {
            let location = Point::<i32, Physical>::from((
                ((card.rect.x - f64::from(output_geometry.loc.x) + card.rect.width * 0.5) * scale).round() as i32
                    - size.w / 2,
                ((card.rect.y - f64::from(output_geometry.loc.y) + card.rect.height - 20.0) * scale).round() as i32,
            ));
            if let Ok(element) = MemoryRenderBufferRenderElement::from_buffer(
                renderer,
                location.to_f64(),
                &buffer,
                Some(alpha),
                Some(Rectangle::from_size((f64::from(size.w), f64::from(size.h)).into())),
                Some(
                    (
                        (f64::from(size.w) / scale).round() as i32,
                        (f64::from(size.h) / scale).round() as i32,
                    )
                        .into(),
                ),
                RenderElementKind::Unspecified,
            ) {
                let element = RescaleRenderElement::from_element(element, location, 1.0);
                let element = RelocateRenderElement::from_element(element, (0, 0), Relocate::Relative);
                if let Some(element) = CropRenderElement::from_element(
                    element,
                    scale,
                    physical_rect(card.rect, output_geometry.loc, scale),
                ) {
                    elements.push(element.into());
                }
            }
        }
        let mut card_color = state.theme_settings.surface_base_color.0;
        for component in &mut card_color[..3] {
            *component = (*component + 0.035).min(1.0);
        }
        card_color[3] = alpha * material_opacity;
        if let Some(card_fill) = overview_chrome_element(
            state,
            renderer,
            output,
            output_geometry,
            scale,
            (card.workspace.0, OverviewChromePart::Card),
            card.rect,
            card_color,
            0.0,
            radius,
        ) {
            elements.push(card_fill.into());
        }
    }
    let mut color = state.theme_settings.surface_base_color.0;
    color[3] = alpha
        * crate::effects::resolve_material(
            crate::effects::SemanticRole::Panel,
            state.theme_settings.material_style,
            state.theme_settings.shell_opacity as f32,
        )
        .opacity;
    if let Some(strip) = overview_chrome_element(
        state,
        renderer,
        output,
        output_geometry,
        scale,
        (0, OverviewChromePart::Strip),
        panel,
        color,
        0.0,
        radius,
    ) {
        elements.push(strip.into());
    }
    elements
}
