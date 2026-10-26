use super::{
    AnimatedWindowRenderElement, Arc, AtomicBool, BlurRenderElement, Ferese, Fourcc, GlesRenderer,
    GlesTexture, MaterialParameters, Offscreen, Output, Rectangle, Renderer, RoundedRect, Texture,
    blur_program, expanded_blur_region, framebuffer_capture_rect, framebuffer_clip_rect,
};

#[derive(Debug)]
pub(super) struct WindowBlur {
    parameters: MaterialParameters,
    element: BlurRenderElement,
}

pub(super) fn window_blur_element(
    state: &mut Ferese,
    renderer: &mut GlesRenderer,
    output: &Output,
    opaque: impl FnOnce() -> bool,
    presentation: crate::presentation::WindowPresentation,
    corners: RoundedRect,
) -> Option<AnimatedWindowRenderElement> {
    let radius = state.theme_settings.backdrop_blur;
    if !state.window_effects.enabled
        || !state.window_effects.blur
        || state.theme_settings.reduce_transparency
        || radius <= 0.0
        || presentation.window_alpha <= 0.0
        || (presentation.window_alpha >= 1.0 && opaque())
        || presentation.alpha() <= 0.0
    {
        if let Some(window) = state.render.windows.get_mut(&presentation.id) {
            window.blur.clear();
        }
        return None;
    }

    let mode = output.current_mode()?;
    let scale = output.current_scale().fractional_scale();
    let transform = output.current_transform().invert();
    let output_size = output
        .current_transform()
        .transform_size(mode.size)
        .to_f64()
        .to_logical(scale)
        .to_i32_ceil();
    let geometry = corners.rect.to_f64().to_logical(scale).to_i32_up();
    let Some(visible) = geometry.intersection(Rectangle::from_size(output_size)) else {
        if let Some(window) = state.render.windows.get_mut(&presentation.id) {
            window.blur.clear();
        }
        return None;
    };
    let sample_geometry = expanded_blur_region(visible, radius.ceil() as i32, output_size);
    let sample_physical = sample_geometry.to_physical_precise_round(scale);
    let parameters = MaterialParameters {
        panel: None,
        corner_radii: [corners.radius; 4],
        // Replace the sharp backdrop completely. Window content supplies the
        // transparency; only the lifecycle animation fades this background.
        presentation_alpha: presentation.opacity.current.clamp(0.0, 1.0) as f32,
        background_opacity: 0.0,
        blur: (radius * scale) as f32,
        sample_geometry,
        sample_physical,
        sample_framebuffer: framebuffer_clip_rect(sample_physical, mode.size, transform),
        shadow_rect: [0.0; 4],
        shadow_values: [0.0; 2],
        shadow_bounds: geometry,
        geometry,
        tint: [0.0; 4],
        generation: 0,
        opaque: false,
        visible_framebuffer: framebuffer_clip_rect(corners.rect, mode.size, transform),
    };
    let key = (renderer.context_id().erased(), state.output_id(output)?);
    let program = blur_program(&mut state.render, renderer)?;
    let buffers = &mut state
        .render
        .windows
        .entry(presentation.id)
        .or_default()
        .blur;
    let capture_rect = framebuffer_capture_rect(parameters.sample_framebuffer);
    let capture_size = super::blur_cache::size(capture_rect, parameters.blur);
    if buffers
        .get(&key)
        .is_none_or(|cached| cached.element.texture.size() != capture_size)
    {
        let texture =
            match Offscreen::<GlesTexture>::create_buffer(renderer, Fourcc::Abgr8888, capture_size)
            {
                Ok(texture) => texture,
                Err(error) => {
                    tracing::warn!(%error, "window blur allocation failed");
                    buffers.remove(&key);
                    return None;
                }
            };
        let scratch =
            match Offscreen::<GlesTexture>::create_buffer(renderer, Fourcc::Abgr8888, capture_size)
            {
                Ok(texture) => texture,
                Err(error) => {
                    tracing::warn!(%error, "window blur allocation failed");
                    buffers.remove(&key);
                    return None;
                }
            };
        let element = BlurRenderElement::new(
            texture,
            scratch,
            program,
            &parameters,
            Arc::new(AtomicBool::new(true)),
        );
        buffers.insert(
            key.clone(),
            WindowBlur {
                parameters: parameters.clone(),
                element,
            },
        );
    }
    let cached = buffers.get_mut(&key)?;
    if cached.parameters != parameters {
        cached.element.update(&parameters);
        cached.parameters = parameters;
    }
    Some(cached.element.clone().into())
}

pub(super) fn client_is_opaque(window: &smithay::desktop::Window) -> bool {
    use smithay::backend::renderer::utils::with_renderer_surface_state;
    use smithay::wayland::alpha_modifier::AlphaModifierSurfaceCachedState;
    use smithay::wayland::compositor::with_states;
    use smithay::wayland::seat::WaylandFocus;

    let Some(surface) = window.wl_surface() else {
        return false;
    };
    let alpha = with_states(&surface, |states| {
        states
            .cached_state
            .get::<AlphaModifierSurfaceCachedState>()
            .current()
            .multiplier_f32()
            .unwrap_or(1.0)
    });
    // An opaque root covers its subsurfaces too. If it doesn't cover the whole
    // window geometry, conservatively keep blur for the uncovered client pixels.
    // Read geometry before locking renderer state: both access surface state.
    let geometry = window.geometry();
    with_renderer_surface_state(&surface, |state| {
        opaque_coverage(geometry, state.opaque_regions().unwrap_or_default(), alpha)
    })
    .unwrap_or(false)
}

fn opaque_coverage(
    geometry: Rectangle<i32, super::Logical>,
    regions: &[Rectangle<i32, super::Logical>],
    alpha: f32,
) -> bool {
    alpha >= 1.0
        && (regions.iter().any(|region| region.contains_rect(geometry))
            || geometry.subtract_rects(regions.iter().copied()).is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_transparency_and_alpha_modifiers_require_a_backdrop() {
        let geometry = Rectangle::new((10, 20).into(), (100, 80).into());
        assert!(opaque_coverage(geometry, &[geometry], 1.0));
        assert!(!opaque_coverage(geometry, &[], 1.0));
        assert!(!opaque_coverage(geometry, &[geometry], 0.9));
        let left = Rectangle::new((10, 20).into(), (50, 80).into());
        let right = Rectangle::new((60, 20).into(), (50, 80).into());
        assert!(opaque_coverage(geometry, &[left, right], 1.0));
        assert!(!opaque_coverage(geometry, &[left], 1.0));
        let inset = Rectangle::new((11, 21).into(), (98, 78).into());
        assert!(!opaque_coverage(geometry, &[inset], 1.0));
    }
}
