use super::*;
use ferese_layout::{Rect, TabBar, WindowId};

#[allow(clippy::too_many_arguments)]
fn text_element(
    state: &mut Ferese,
    renderer: &mut GlesRenderer,
    output_origin: Point<i32, Logical>,
    scale: f64,
    rect: Rect,
    text: &str,
    alpha: f32,
    centered: bool,
) -> Option<AnimatedWindowRenderElement> {
    let settings = state.tab_indicator_settings;
    let color = settings.text_color.unwrap_or(state.theme_settings.text_primary_color.0);
    let limit = ((rect.width - 16.0) / (settings.font_size * 0.65)).floor().max(1.0) as usize;
    let mut chars = text.chars().filter(|c| !c.is_control());
    let mut title = chars.by_ref().take(limit).collect::<String>();
    if chars.next().is_some() {
        title.pop();
        title.push('…');
    }
    let (buffer, size) = state.indicator_label(&title, scale, color, settings.font_size)?;
    let x = if centered {
        ((rect.width - f64::from(size.w) / scale) * 0.5).max(8.0)
    } else {
        8.0
    };
    let location = Point::<i32, Physical>::from((
        ((rect.x - f64::from(output_origin.x) + x) * scale).round() as i32,
        ((rect.y - f64::from(output_origin.y) + rect.height * 0.5) * scale).round() as i32 - size.h / 2,
    ));
    let element = MemoryRenderBufferRenderElement::from_buffer(
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
    )
    .ok()?;
    let element = RescaleRenderElement::from_element(element, location, 1.0);
    let element = RelocateRenderElement::from_element(element, (0, 0), Relocate::Relative);
    let clip = Rect::new(rect.x + 8.0, rect.y, (rect.width - 16.0).max(0.0), rect.height);
    CropRenderElement::from_element(element, scale, physical_rect(clip, output_origin, scale)).map(Into::into)
}

fn title(state: &Ferese, id: WindowId, capture: bool) -> String {
    if capture && state.windows.window(id).is_none_or(|w| state.capture_protected(w)) {
        return "Protected window".into();
    }
    state
        .windows
        .record(id)
        .map(|r| {
            if !r.title.trim().is_empty() {
                r.title.clone()
            } else if !r.app_id.is_empty() {
                r.app_id.clone()
            } else {
                "Window".into()
            }
        })
        .unwrap_or_default()
}

#[allow(clippy::too_many_arguments)]
pub(super) fn elements(
    state: &mut Ferese,
    renderer: &mut GlesRenderer,
    output: &Output,
    bar: &TabBar,
    content: Rect,
    alpha: f32,
    capture: bool,
) -> Vec<AnimatedWindowRenderElement> {
    let Some(g) = state.tab_indicator_geometry(bar, content) else {
        return Vec::new();
    };
    let Some(output_geometry) = state.space.output_geometry(output) else {
        return Vec::new();
    };
    let scale = output.current_scale().fractional_scale();
    let settings = state.tab_indicator_settings;
    let anchor = bar.group.windows[0];
    let accent = settings.active_color.unwrap_or(state.theme_settings.accent_color.0);
    let inactive = settings.inactive_color.unwrap_or(state.theme_settings.border_color.0);
    let background = settings
        .background_color
        .unwrap_or(state.theme_settings.surface_base_color.0);
    let radius = state.theme_settings.material_radius;
    let hover = state
        .tab_indicator
        .hover
        .as_ref()
        .filter(|h| h.anchor == anchor)
        .map_or(0.0, |h| h.opacity.current.clamp(0.0, 1.0) as f32);
    let flash = state
        .tab_indicator
        .flash
        .as_ref()
        .filter(|h| h.anchor == anchor)
        .map_or(0.0, |h| h.opacity.current.clamp(0.0, 1.0) as f32);
    let mut elements = Vec::new();
    state.retain_overview_labels();
    if hover > 0.0 {
        let first = state
            .tab_indicator
            .first_row
            .min(bar.group.windows.len().saturating_sub(g.rows));
        for (row, id) in bar.group.windows.iter().copied().skip(first).take(g.rows).enumerate() {
            let rect = g.row(row);
            let label = title(state, id, capture);
            if let Some(element) = text_element(
                state,
                renderer,
                output_geometry.loc,
                scale,
                rect,
                &label,
                alpha * hover,
                false,
            ) {
                elements.push(element);
            }
            if id == bar.group.active {
                let mut fill = background;
                for i in 0..3 {
                    fill[i] += (accent[i] - fill[i]) * 0.2;
                }
                fill[3] = alpha * hover;
                if let Some(element) = overview_chrome_element(
                    state,
                    renderer,
                    output,
                    output_geometry,
                    scale,
                    (id.0, OverviewChromePart::TabRow),
                    rect,
                    fill,
                    0.0,
                    (radius - g.padding).max(0.0).min(rect.height * 0.5),
                ) {
                    elements.push(element.into());
                }
            }
        }
        if let Some(element) = overview_chrome_element(
            state,
            renderer,
            output,
            output_geometry,
            scale,
            (anchor.0, OverviewChromePart::TabList),
            g.list,
            color_with_alpha(background, alpha * hover),
            0.0,
            radius.min(g.list.height * 0.5),
        ) {
            elements.push(element.into());
        }
    } else if flash > 0.0 {
        let index = bar
            .group
            .windows
            .iter()
            .position(|id| *id == bar.group.active)
            .unwrap_or(0);
        let label = format!(
            "{}/{} · {}",
            index + 1,
            bar.group.windows.len(),
            title(state, bar.group.active, capture)
        );
        let width = (label.chars().count() as f64 * settings.font_size * 0.65 + 24.0).min(g.list.width);
        let bounds = Rect::new(
            output_geometry.loc.x as f64,
            output_geometry.loc.y as f64,
            output_geometry.size.w as f64,
            output_geometry.size.h as f64,
        );
        let rect = Rect::new(
            (content.x + (content.width - width) * 0.5).clamp(bounds.x, bounds.x + bounds.width - width),
            (content.y + settings.padding).clamp(bounds.y, bounds.y + bounds.height - g.row_height),
            width,
            g.row_height,
        );
        if let Some(element) = text_element(
            state,
            renderer,
            output_geometry.loc,
            scale,
            rect,
            &label,
            alpha * flash,
            true,
        ) {
            elements.push(element);
        }
        if let Some(element) = overview_chrome_element(
            state,
            renderer,
            output,
            output_geometry,
            scale,
            (anchor.0, OverviewChromePart::TabFlash),
            rect,
            color_with_alpha(background, alpha * flash),
            0.0,
            rect.height * 0.5,
        ) {
            elements.push(element.into());
        }
    }
    for (index, id) in bar.group.windows.iter().copied().enumerate() {
        let rect = Rect::new(
            g.rail.x,
            g.rail.y + index as f64 * g.pitch,
            g.rail.width,
            g.segment_height,
        );
        let color = if id == bar.group.active { accent } else { inactive };
        if let Some(element) = overview_chrome_element(
            state,
            renderer,
            output,
            output_geometry,
            scale,
            (id.0, OverviewChromePart::TabRail),
            rect,
            color_with_alpha(color, alpha),
            0.0,
            rect.width.min(rect.height) * 0.5,
        ) {
            elements.push(element.into());
        }
    }
    elements
}
