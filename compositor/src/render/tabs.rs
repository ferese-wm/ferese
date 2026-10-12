use super::*;

pub(super) fn tab_elements(
    state: &mut Ferese,
    renderer: &mut GlesRenderer,
    output: &Output,
    frame: &crate::state::FrameScene,
    active: ferese_layout::WindowId,
    capture: bool,
) -> Vec<AnimatedWindowRenderElement> {
    if frame.overview.is_presenting() {
        return Vec::new();
    }
    let Some(workspace) = state.workspaces.workspace_for_window(active) else {
        return Vec::new();
    };
    let Some(bar) = state
        .visible_tab_bars(workspace)
        .find(|bar| bar.group.active == active)
        .cloned()
    else {
        return Vec::new();
    };
    let Some(sample) = frame.windows.get(&active) else {
        return Vec::new();
    };
    if matches!(
        state.workspaces.workspace(workspace).map(|w| &w.layout),
        Some(ferese_core::WorkspaceLayout::Scrolling(_))
    ) {
        return super::tab_indicator::elements(
            state,
            renderer,
            output,
            &bar,
            sample.presentation.bounds.current,
            sample.presentation.alpha(),
            capture,
        );
    }
    if bar.rect.height <= 0.0 {
        return Vec::new();
    }
    let content = sample.presentation.bounds.current;
    let alpha = sample.presentation.alpha();
    let Some(output_geometry) = state.space.output_geometry(output) else {
        return Vec::new();
    };
    let scale = output.current_scale().fractional_scale();
    let width = content.width / bar.group.windows.len() as f64;
    let shell = ferese_layout::Rect::new(content.x, content.y - bar.rect.height, content.width, bar.rect.height);
    let radius = state
        .theme_settings
        .material_radius
        .min(shell.height * 0.5)
        .min(shell.width * 0.5);
    let inset = 2.0_f64.min(shell.height * 0.25).min(width * 0.25);
    let material = crate::effects::resolve_material(
        crate::effects::SemanticRole::Panel,
        state.theme_settings.material_style,
        state.theme_settings.shell_opacity as f32,
    );
    let opacity = if state.theme_settings.reduce_transparency {
        1.0
    } else {
        material.opacity
    };
    let mut elements = Vec::new();
    state.retain_overview_labels();
    for (index, id) in bar.group.windows.iter().copied().enumerate() {
        let rect = ferese_layout::Rect::new(
            content.x + width * index as f64,
            content.y - bar.rect.height,
            width,
            bar.rect.height,
        );
        let protected = capture
            && state
                .windows
                .window(id)
                .is_none_or(|window| state.capture_protected(window));
        if !protected
            && width > 20.0
            && rect.height >= 16.0
            && let Some((buffer, size)) = state.window_title_label(id, scale, width, 12.0)
        {
            let location = Point::<i32, Physical>::from((
                ((rect.x - f64::from(output_geometry.loc.x)
                    + ((rect.width - f64::from(size.w) / scale) * 0.5).max(8.0))
                    * scale)
                    .round() as i32,
                ((rect.y - f64::from(output_geometry.loc.y) + rect.height * 0.5) * scale).round() as i32 - size.h / 2,
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
                let clip = ferese_layout::Rect::new(rect.x + 8.0, rect.y, (rect.width - 16.0).max(0.0), rect.height);
                if let Some(element) =
                    CropRenderElement::from_element(element, scale, physical_rect(clip, output_geometry.loc, scale))
                {
                    elements.push(element.into());
                }
            }
        }
        if id == active {
            let selected = ferese_layout::Rect::new(
                rect.x + inset,
                rect.y + inset,
                (rect.width - 2.0 * inset).max(0.0),
                (rect.height - 2.0 * inset).max(0.0),
            );
            // The shell palette supplies the surface and accent. Keep selection
            // inside the shell so its corners follow the same continuous profile.
            let mut fill = state.theme_settings.surface_raised_color.0;
            for (channel, accent) in fill[..3].iter_mut().zip(state.theme_settings.accent_color.0) {
                *channel += (accent - *channel) * 0.16;
            }
            fill[3] = alpha * opacity;
            if let Some(element) = overview_chrome_element(
                state,
                renderer,
                output,
                output_geometry,
                scale,
                (id.0, OverviewChromePart::TabActive),
                selected,
                fill,
                0.0,
                (radius - inset).max(0.0),
            ) {
                elements.push(element.into());
            }
        }
    }
    let mut fill = state.theme_settings.surface_base_color.0;
    fill[3] = alpha * opacity;
    if let Some(element) = overview_chrome_element(
        state,
        renderer,
        output,
        output_geometry,
        scale,
        (bar.group.windows[0].0, OverviewChromePart::Tab),
        shell,
        fill,
        0.0,
        radius,
    ) {
        elements.push(element.into());
    }
    elements
}
