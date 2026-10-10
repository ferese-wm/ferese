use smithay::desktop::Window;

use super::*;

pub(crate) fn animated_window_elements(
    state: &mut Ferese,
    renderer: &mut GlesRenderer,
    output: &Output,
) -> Vec<AnimatedWindowRenderElement> {
    output_elements(state, renderer, output, true)
}

pub(crate) fn frame_effect_metrics(_elements: &[AnimatedWindowRenderElement], _scale: f64) -> FrameEffectMetrics {
    FrameEffectMetrics
}

pub(crate) fn output_elements(
    state: &mut Ferese,
    renderer: &mut GlesRenderer,
    output: &Output,
    include_cursor: bool,
) -> Vec<AnimatedWindowRenderElement> {
    let frame = state.sample_frame(output, Duration::ZERO);

    sampled_output_elements(state, renderer, output, include_cursor, &frame)
}

pub(crate) fn sampled_output_elements(
    state: &mut Ferese,
    renderer: &mut GlesRenderer,
    output: &Output,
    include_cursor: bool,
    frame: &crate::state::FrameScene,
) -> Vec<AnimatedWindowRenderElement> {
    scene_elements(state, renderer, output, include_cursor, frame, false)
}

pub(crate) fn capture_output_elements(
    state: &mut Ferese,
    renderer: &mut GlesRenderer,
    output: &Output,
    include_cursor: bool,
    frame: &crate::state::FrameScene,
) -> Vec<AnimatedWindowRenderElement> {
    // Blur textures and resize handoff snapshots must never be shared with
    // display rendering. Only the capture scene can populate these caches.
    std::mem::swap(&mut state.render, &mut state.capture_render);
    let elements = scene_elements(state, renderer, output, include_cursor, frame, true);
    std::mem::swap(&mut state.render, &mut state.capture_render);
    elements
}

fn scene_elements(
    state: &mut Ferese,
    renderer: &mut GlesRenderer,
    output: &Output,
    include_cursor: bool,
    frame: &crate::state::FrameScene,
    capture: bool,
) -> Vec<AnimatedWindowRenderElement> {
    if state.session_lock.active() {
        let Some(geometry) = state.space.output_geometry(output) else {
            return Vec::new();
        };
        let scale = output.current_scale().fractional_scale();
        let mut elements = if include_cursor {
            cursor_elements(state, renderer, geometry, scale)
        } else {
            Vec::new()
        };
        let background = state.session_lock.backgrounds.entry(output.clone()).or_insert_with(|| {
            smithay::backend::renderer::element::solid::SolidColorBuffer::new(geometry.size, [0.0, 0.0, 0.0, 1.0])
        });
        background.resize(geometry.size);
        let opacity = state.session_lock.idle_opacity;
        if opacity > 0.0 {
            let overlay = state
                .session_lock
                .idle_overlays
                .entry(output.clone())
                .or_insert_with(|| {
                    smithay::backend::renderer::element::solid::SolidColorBuffer::new(
                        geometry.size,
                        [0.0, 0.0, 0.0, 1.0],
                    )
                });
            overlay.resize(geometry.size);
            elements.push(
                SolidColorRenderElement::from_buffer(overlay, (0, 0), scale, opacity, RenderElementKind::Unspecified)
                    .into(),
            );
        }
        if let Some(surface) = state
            .session_lock
            .surfaces
            .get(output)
            .filter(|surface| surface.alive())
        {
            elements.extend(
                render_elements_from_surface_tree::<GlesRenderer, WaylandSurfaceRenderElement<GlesRenderer>>(
                    renderer,
                    surface.wl_surface(),
                    (0, 0),
                    scale,
                    1.0,
                    RenderElementKind::Unspecified,
                )
                .into_iter()
                .map(AnimatedWindowRenderElement::from),
            );
        }
        elements.push(
            SolidColorRenderElement::from_buffer(background, (0, 0), scale, 1.0, RenderElementKind::Unspecified).into(),
        );
        return elements;
    }
    let Some(output_geometry) = state.space.output_geometry(output) else {
        return Vec::new();
    };
    let scale = output.current_scale().fractional_scale();

    let protected_cursor = capture && state.capture_cursor_protected();
    let mut elements = if include_cursor && !protected_cursor {
        cursor_elements(state, renderer, output_geometry, scale)
    } else {
        Vec::new()
    };
    let upper_layers: &[Layer] = if state.output_has_fullscreen_for_frame(output, frame.overview.is_presenting()) {
        &[Layer::Overlay]
    } else {
        &[Layer::Overlay, Layer::Top]
    };
    elements.extend(layer_elements(state, renderer, output, upper_layers));
    if frame.overview.is_presenting() {
        elements.extend(overview_strip_elements(
            state,
            renderer,
            output,
            output_geometry,
            scale,
            frame,
            capture,
        ));
    }
    let prepare_window = |window: &Window| {
        // Configure immediately, but present the frame/shadow only after
        // the first buffer commit has settled the actual client geometry.
        if (capture && state.capture_protected(window)) || !state.window_content_ready(window) {
            return None;
        }
        let id = *state.windows.ids().get(window)?;
        // Scrolling columns may sit outside their monitor's rectangle.
        // They must not reappear on a neighboring output just because
        // their global animated coordinates overlap it.
        if !state.window_belongs_to_output(id, output) {
            return None;
        }
        let sample = frame.windows.get(&id)?;
        let visual = sample.presentation.bounds.current;
        let presentation_alpha = sample.presentation.alpha();
        let decoration_progress = sample.geometry.decorations.clamp(0.0, 1.0);

        Some((
            window.clone(),
            id,
            sample,
            visual,
            decoration_progress,
            presentation_alpha,
        ))
    };
    let windows = if state.overview.is_active() {
        state
            .windows
            .overview_windows()
            .filter_map(prepare_window)
            .collect::<Vec<_>>()
    } else {
        state
            .space
            .elements()
            .rev()
            .filter_map(prepare_window)
            .collect::<Vec<_>>()
    };

    let mut closing = super::closing::grouped_elements(
        state,
        renderer,
        output,
        windows.iter().map(|(_, id, ..)| *id),
        frame.delta,
    );
    for (window, id, sample, visual, decoration_progress, presentation_alpha) in windows {
        if let Some(group) = closing.remove(&Some(id)) {
            elements.extend(group);
        }
        let constrain = rounded_visual_rect(visual, output_geometry.loc);
        let material_surface = window
            .toplevel()
            .map(|toplevel| toplevel.wl_surface())
            .filter(|surface| {
                crate::effects::surface_role(surface)
                    .is_some_and(|(role, _)| role == crate::effects::SemanticRole::Modal)
            });
        let window_radius = if material_surface.is_some() {
            state.theme_settings.material_radius
        } else {
            state.theme_settings.window_radius
        } * decoration_progress;
        let shape = window_corner_shape(&window);
        let corners = RoundedRect::new(visual, output_geometry.loc, scale, window_radius).with_shape(shape);
        let corner_shape_changed = state.render.prepare_window_corners(id, shape);
        let rounded_clip_program = corner_program(&mut state.render, renderer, shape);
        // Only overview/close intentionally scale the complete application.
        let scale_content = sample.presentation.scale_content;
        let behavior = resize_content_behavior(scale_content);

        let dim = sample.presentation.dim;
        if let Some(overlay) = window_tint_element(
            &mut state.render,
            renderer,
            id,
            constrain,
            corners,
            [0.0, 0.0, 0.0, dim as f32 * presentation_alpha],
            false,
            output,
        ) {
            // Front-to-back: dim the application and its border, not its shadow
            // or other windows/layer surfaces. The overlay is input-transparent.
            elements.push(overlay.into());
        }

        if let Some(programs) = rounded_clip_program.clone() {
            let shadow_offset_y = state.theme_settings.shadow_offset_y;
            let shadow_blur = state.theme_settings.shadow_blur;
            let shadow_opacity = state.theme_settings.shadow_opacity;
            let shadow_color = state.theme_settings.shadow_color.0;
            let focus = sample.presentation.focus();
            let border_width = state.theme_settings.border_width
                + (state.theme_settings.focus_ring_width - state.theme_settings.border_width) * focus;
            let border_color = state.theme_settings.border_color.0;
            let gradient = state.theme_settings.border_gradient;

            if let Some(border) = window_border_element(
                &mut state.render,
                &state.theme_settings,
                renderer,
                id,
                constrain,
                corners,
                scale,
                border_width,
                border_color,
                gradient,
                focus as f32,
                presentation_alpha * decoration_progress as f32,
                output,
                &programs,
            ) {
                elements.push(border.into());
            }
            let (offset_factor, blur_factor, opacity_factor) = sample.presentation.shadow_factors();
            let shadow = window_shadow_element(
                &mut state.render,
                renderer,
                id,
                constrain,
                corners,
                scale,
                shadow_offset_y * offset_factor,
                shadow_blur * blur_factor,
                shadow_opacity * opacity_factor * f64::from(presentation_alpha) * decoration_progress,
                shadow_color,
                output,
                &programs,
            );
            if let Some(snapshot) = state.render.snapshot(&id)
                && snapshot.context == renderer.context_id().erased()
                && (snapshot.scale - scale).abs() < 0.001
                && let Some(element) = super::window_content::snapshot_element(
                    snapshot,
                    sample.presentation,
                    corners,
                    scale,
                    output,
                    &programs,
                    crate::presentation::handoff_alpha(snapshot.elapsed),
                )
            {
                elements.push(element.into());
            }

            elements.extend(rounded_window_elements(
                renderer,
                &window,
                corners,
                scale,
                presentation_alpha,
                sample.geometry.presentation_changed || corner_shape_changed,
                output,
                programs.clone(),
                behavior,
                sample.presentation.native_size,
            ));
            let source = window.geometry().size;
            if material_surface.is_none()
                && !scale_content
                && (source.w < constrain.size.w || source.h < constrain.size.h)
            {
                let mut color = state.theme_settings.surface_base_color.0;
                color[3] = presentation_alpha;
                if let Some(fill) =
                    window_tint_element(&mut state.render, renderer, id, constrain, corners, color, true, output)
                {
                    // Front-to-back: fill uncovered strips behind the native
                    // content instead of stretching it or exposing wallpaper.
                    elements.push(fill.into());
                }
            } else {
                state.render.clear_tint(id, true);
            }
            if let Some(surface) = material_surface
                && let Some((background, _)) = material_element(
                    state,
                    renderer,
                    output,
                    surface,
                    MaterialSurface {
                        geometry: constrain,
                        corners,
                        index: 0,
                        capture_geometry: constrain,
                        alpha: presentation_alpha,
                    },
                )
            {
                elements.push(background);
            }
            if let Some(shadow) = shadow {
                elements.push(shadow.into());
            }
        } else {
            elements.extend(constrain_space_element::<GlesRenderer, _, AnimatedWindowRenderElement>(
                renderer,
                &window,
                constrain.loc,
                presentation_alpha,
                scale,
                constrain,
                ConstrainBehavior {
                    reference: ConstrainReference::Geometry,
                    behavior,
                    align: ConstrainAlign::TOP | ConstrainAlign::LEFT,
                },
            ));
        }
    }
    if let Some(group) = closing.remove(&None) {
        elements.extend(group);
    }
    elements.extend(layer_elements(
        state,
        renderer,
        output,
        &[Layer::Bottom, Layer::Background],
    ));
    state.prepare_theme_wallpaper(renderer, output);
    let progress = state.theme_progress();
    let wallpaper = state.wallpaper.element(renderer, output);
    let has_wallpaper = wallpaper.is_some();
    if let Some(mut wallpaper) = wallpaper {
        wallpaper.alpha = progress;
        elements.push(wallpaper.into());
    }
    if let Some(mut previous) = state.previous_theme_wallpaper(renderer, output) {
        if !has_wallpaper {
            previous.alpha = 1. - progress;
        }
        elements.push(previous.into());
    }
    backdrop::update(&elements, scale);
    elements
}

pub(crate) fn layer_surfaces(output: &Output) -> Vec<LayerSurface> {
    layer_map_for_output(output).layers().cloned().collect()
}

pub(super) fn layer_elements(
    state: &mut Ferese,
    renderer: &mut GlesRenderer,
    output: &Output,
    requested_layers: &[Layer],
) -> Vec<AnimatedWindowRenderElement> {
    let scale = output.current_scale().fractional_scale();
    let Some(mode) = output.current_mode() else {
        return Vec::new();
    };
    let output_crop = Rectangle::<i32, Physical>::from_size(mode.size);
    let layers = {
        let map = layer_map_for_output(output);

        requested_layers
            .iter()
            .flat_map(|requested| {
                map.layers_on(*requested)
                    .rev()
                    .filter_map(|layer| map.layer_geometry(layer).map(|geometry| (geometry, layer.clone())))
            })
            .collect::<Vec<_>>()
    };

    state.render.surfaces.retain(|surface, _| {
        surface.is_alive()
            || state
                .render
                .closing
                .iter()
                .any(|window| window.material.as_ref().is_some_and(|(key, ..)| key == surface))
    });

    let mut elements = Vec::new();
    for (geometry, layer) in layers {
        for (popup, offset) in PopupManager::popups_for_surface(layer.wl_surface()) {
            let mut bounds = popup.geometry();
            let content_origin = geometry.loc + offset - bounds.loc;
            bounds.loc = geometry.loc + offset;
            append_material_surface(
                state,
                renderer,
                output,
                popup.wl_surface(),
                bounds,
                content_origin,
                scale,
                output_crop,
                &mut elements,
            );
        }
        append_material_surface(
            state,
            renderer,
            output,
            layer.wl_surface(),
            geometry,
            geometry.loc,
            scale,
            output_crop,
            &mut elements,
        );
    }

    elements
}

pub(super) fn cursor_elements(
    state: &Ferese,
    renderer: &mut GlesRenderer,
    output_geometry: Rectangle<i32, Logical>,
    scale: f64,
) -> Vec<AnimatedWindowRenderElement> {
    if state.input_capture.active() {
        return Vec::new();
    }
    let Some(pointer) = state.seat.get_pointer() else {
        return Vec::new();
    };
    let output_crop = Rectangle::<i32, Logical>::from_size(output_geometry.size).to_physical_precise_round(scale);
    let origin = Point::<i32, Physical>::default();
    let pointer_location = pointer.current_location() - output_geometry.loc.to_f64();
    match &state.cursor_status {
        CursorImageStatus::Surface(surface) => {
            let hotspot = with_states(surface, |states| {
                states
                    .data_map
                    .get::<CursorImageSurfaceData>()
                    .map(|attributes| attributes.lock().unwrap().hotspot)
                    .unwrap_or_default()
            });
            let physical_location = (pointer_location - hotspot.to_f64()).to_physical_precise_round(scale);

            render_elements_from_surface_tree::<GlesRenderer, WaylandSurfaceRenderElement<GlesRenderer>>(
                renderer,
                surface,
                physical_location,
                scale,
                1.0,
                RenderElementKind::Cursor,
            )
            .into_iter()
            .filter_map(|element| {
                let element = RescaleRenderElement::from_element(element, origin, 1.0);
                let element = RelocateRenderElement::from_element(element, origin, Relocate::Relative);

                CropRenderElement::from_element(element, scale, output_crop).map(Into::into)
            })
            .collect()
        }
        CursorImageStatus::Named(_) => {
            let Some(cursor) = state.named_cursor_frame() else {
                return Vec::new();
            };
            let physical_location = cursor.physical_location(pointer_location, scale);
            let Ok(element) = MemoryRenderBufferRenderElement::from_buffer(
                renderer,
                physical_location,
                &cursor.buffer,
                None,
                None,
                None,
                RenderElementKind::Cursor,
            ) else {
                return Vec::new();
            };

            let element = RescaleRenderElement::from_element(element, origin, 1.0);
            let element = RelocateRenderElement::from_element(element, origin, Relocate::Relative);

            CropRenderElement::from_element(element, scale, output_crop)
                .map(|element| vec![element.into()])
                .unwrap_or_default()
        }
        CursorImageStatus::Hidden => Vec::new(),
    }
}
