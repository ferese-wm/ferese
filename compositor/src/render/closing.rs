use super::*;
use ferese_animation::{CrossingPolicy, SpringConfig};
use ferese_core::OutputId;
use ferese_layout::WindowId;

/// Detached presentation only: no live window, input, focus, or layout entry.
#[derive(Clone)]
pub(crate) struct ClosedWindow {
    pub presentation: crate::presentation::WindowPresentation,
    pub output: OutputId,
    pub below: Option<WindowId>,
    pub snapshot: ResizeSnapshot,
    // Keep a resize handoff's visible content when the client unmaps mid-resize.
    pub handoff: Option<(ResizeSnapshot, Option<ferese_animation::ClientSize>)>,
    pub radius: f64,
    pub shape: CornerShape,
    pub decorations: f64,
    pub fill: Option<[f32; 4]>,
    pub material: Option<(
        smithay::reexports::wayland_server::protocol::wl_surface::WlSurface,
        crate::effects::SemanticRole,
        u64,
        f32,
    )>,
}

impl ClosedWindow {
    pub fn bytes(&self) -> usize {
        self.snapshot.bytes() + self.handoff.as_ref().map_or(0, |(snapshot, _)| snapshot.bytes())
    }

    pub fn advance(&mut self, delta: Duration, spring: SpringConfig) -> bool {
        // The client is gone, so this handoff no longer waits for configure
        // acknowledgements. Delta already includes the global motion speed.
        if let Some((snapshot, _)) = &mut self.handoff {
            snapshot.elapsed = snapshot.elapsed.saturating_add(delta);
            snapshot.commit.increment();
            if snapshot.elapsed >= crate::presentation::HANDOFF {
                self.handoff = None;
            }
        }
        self.presentation.bounds.advance(delta, spring);
        let opacity = self.presentation.opacity.advance_with_policy(
            delta,
            SpringConfig {
                position_tolerance: 0.001,
                velocity_tolerance: 0.001,
                ..spring
            },
            CrossingPolicy::NoCrossing,
        );
        self.presentation
            .emphasis
            .advance(delta, crate::presentation::emphasis_spring(spring));
        self.presentation
            .shadow
            .advance(delta, crate::presentation::shadow_spring(spring));
        self.snapshot.commit.increment();
        // Texture lifetime ends when the shared presentation becomes invisible.
        opacity
    }
}

pub(super) fn grouped_elements(
    state: &mut Ferese,
    renderer: &mut GlesRenderer,
    output: &Output,
    live: impl IntoIterator<Item = WindowId>,
    delta: Duration,
) -> std::collections::HashMap<Option<WindowId>, Vec<AnimatedWindowRenderElement>> {
    if state.render.closing.is_empty() || state.session_lock.active() {
        return Default::default();
    }
    let Some(output_id) = state.output_id(output) else {
        return Default::default();
    };
    let Some(output_geometry) = state.space.output_geometry(output) else {
        return Default::default();
    };
    let scale = output.current_scale().fractional_scale();
    let windows: Vec<_> = state
        .render
        .closing
        .iter()
        .filter(|window| window.output == output_id && window.snapshot.context == renderer.context_id().erased())
        .cloned()
        .collect();
    if windows.is_empty() {
        return Default::default();
    }
    let live: std::collections::HashSet<_> = live.into_iter().collect();
    let mut groups = std::collections::HashMap::new();
    for mut window in windows {
        let anchor = window.below.filter(|id| live.contains(id));
        let elements: &mut Vec<AnimatedWindowRenderElement> = groups.entry(anchor).or_default();
        if !delta.is_zero() {
            window.advance(delta, state.spring_config);
        }
        let visual = window.presentation.bounds.current;
        let corners = RoundedRect::new(visual, output_geometry.loc, scale, window.radius).with_shape(window.shape);
        let constrain = rounded_visual_rect(visual, output_geometry.loc);
        let Some(programs) = corner_program(&mut state.render, renderer, window.shape) else {
            continue;
        };
        let alpha = window.presentation.alpha();
        if let Some(dim) = window_tint_element(
            &mut state.render,
            renderer,
            window.presentation.id,
            constrain,
            corners,
            [0.0, 0.0, 0.0, window.presentation.dim as f32 * alpha],
            false,
            output,
        ) {
            elements.push(dim.into());
        }
        let theme = &state.theme_settings;
        if let Some(border) = window_border_element(
            &mut state.render,
            theme,
            renderer,
            window.presentation.id,
            constrain,
            corners,
            scale,
            theme.border_width + (theme.focus_ring_width - theme.border_width) * window.presentation.focus(),
            theme.border_color.0,
            theme.border_gradient,
            window.presentation.focus() as f32,
            alpha * window.decorations as f32,
            output,
            &programs,
        ) {
            elements.push(border.into());
        }
        if let Some((snapshot, native_size)) = &window.handoff {
            let mut presentation = window.presentation;
            presentation.scale_content = native_size.is_none();
            presentation.native_size = *native_size;
            if let Some(element) = super::window_content::snapshot_element(
                snapshot,
                presentation,
                corners,
                scale,
                output,
                &programs,
                crate::presentation::handoff_alpha(snapshot.elapsed),
            ) {
                elements.push(element.into());
            }
        }
        if let Some(element) = super::window_content::snapshot_element(
            &window.snapshot,
            window.presentation,
            corners,
            scale,
            output,
            &programs,
            1.0,
        ) {
            elements.push(element.into());
        }
        if let Some(mut fill) = window.fill {
            fill[3] = alpha;
            if let Some(fill) = window_tint_element(
                &mut state.render,
                renderer,
                window.presentation.id,
                constrain,
                corners,
                fill,
                true,
                output,
            ) {
                elements.push(fill.into());
            }
        }
        if let Some((surface, role, generation, opacity)) = &window.material
            && let Some((material, _)) = material_element_with_role(
                state,
                renderer,
                output,
                surface,
                MaterialSurface {
                    geometry: constrain,
                    corners,
                    index: 0,
                    capture_geometry: constrain,
                    alpha,
                },
                (*role, *generation, *opacity),
            )
        {
            elements.push(material);
        }
        let theme = &state.theme_settings;
        let (offset_factor, blur_factor, opacity_factor) = window.presentation.shadow_factors();
        if let Some(shadow) = window_shadow_element(
            &mut state.render,
            renderer,
            window.presentation.id,
            constrain,
            corners,
            scale,
            theme.shadow_offset_y * offset_factor,
            theme.shadow_blur * blur_factor,
            theme.shadow_opacity * opacity_factor * f64::from(alpha) * window.decorations,
            theme.shadow_color.0,
            output,
            &programs,
        ) {
            elements.push(shadow.into());
        }
    }
    groups
}
