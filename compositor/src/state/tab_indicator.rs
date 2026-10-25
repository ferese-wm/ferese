use super::{
    AnimatedValue, Duration, Ferese, Instant, Layer, Logical, Output, Point, Rect, SpringConfig,
    WindowId, WorkspaceLayout,
};
use crate::config::TabIndicatorSettings;
use smithay::reexports::calloop::RegistrationToken;
use smithay::reexports::calloop::timer::{TimeoutAction, Timer};

#[derive(Debug)]
pub(crate) struct Overlay {
    pub anchor: WindowId,
    pub opacity: AnimatedValue,
}

#[derive(Debug, Default)]
pub(crate) struct TabIndicatorState {
    pub hover: Option<Overlay>,
    pub flash: Option<Overlay>,
    pub first_row: usize,
    pub hover_width: f64,
    timer: Option<RegistrationToken>,
}

impl TabIndicatorState {
    pub fn is_animating(&self) -> bool {
        [&self.hover, &self.flash]
            .into_iter()
            .flatten()
            .any(|o| o.opacity.is_animating())
    }

    pub fn advance(&mut self, delta: Duration, spring: SpringConfig, enabled: bool) -> bool {
        let mut moving = false;
        for overlay in [&mut self.hover, &mut self.flash] {
            if let Some(value) = overlay {
                if enabled {
                    moving |= value
                        .opacity
                        .advance(delta, crate::presentation::emphasis_spring(spring));
                } else {
                    value.opacity.snap();
                }
                if !value.opacity.is_animating() && value.opacity.current == 0.0 {
                    *overlay = None;
                }
            }
        }
        moving
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct IndicatorGeometry {
    pub rail: Rect,
    pub hit: Rect,
    pub list: Rect,
    pub segment_height: f64,
    pub pitch: f64,
    pub row_height: f64,
    pub padding: f64,
    pub rows: usize,
}

pub(crate) fn contains(rect: Rect, p: Point<f64, Logical>) -> bool {
    p.x >= rect.x && p.x < rect.x + rect.width && p.y >= rect.y && p.y < rect.y + rect.height
}

impl IndicatorGeometry {
    pub fn new(
        content: Rect,
        output: Rect,
        left_boundary: f64,
        count: usize,
        c: TabIndicatorSettings,
    ) -> Option<Self> {
        if count < 2
            || content.width <= 0.0
            || content.height <= 0.0
            || output.width <= 0.0
            || output.height <= 0.0
        {
            return None;
        }
        let left = content.x.max(output.x);
        let right = (content.x + content.width).min(output.x + output.width);
        let top = content.y.max(output.y);
        let bottom = (content.y + content.height).min(output.y + output.height);
        if right - left < c.width || bottom <= top {
            return None;
        }
        let gap = (content.x - left_boundary).max(0.0);
        let x = if content.x >= output.x && gap >= c.gap_threshold {
            content.x - (c.gap_threshold + c.width) * 0.5
        } else {
            left
        }
        .clamp(output.x, (right - c.width).max(output.x));
        let natural =
            count as f64 * c.segment_height + count.saturating_sub(1) as f64 * c.segment_gap;
        let fit = ((bottom - top) / natural).min(1.0);
        let height = (natural * fit).min(bottom - top);
        let rail = Rect::new(x, top + (bottom - top - height) * 0.5, c.width, height);
        let hit_width = c.hit_width.min(output.width);
        let hit = Rect::new(
            (x + c.width * 0.5 - hit_width * 0.5)
                .clamp(output.x, output.x + output.width - hit_width),
            rail.y,
            hit_width,
            rail.height,
        );
        let padding = c.padding.min(output.height * 0.1).min(output.width * 0.1);
        let row_height = c
            .row_height
            .min((output.height - 2.0 * padding).max(f64::EPSILON));
        let rows = (((output.height - 2.0 * padding) / row_height).floor() as usize)
            .max(1)
            .min(count);
        let list_width = c.list_width.min(output.width);
        let list_height = (rows as f64 * row_height + 2.0 * padding).min(output.height);
        let x = if hit.x + hit.width + padding + list_width <= output.x + output.width {
            hit.x + hit.width + padding
        } else if hit.x - padding - list_width >= output.x {
            hit.x - padding - list_width
        } else {
            (hit.x + hit.width + padding).clamp(output.x, output.x + output.width - list_width)
        };
        let list = Rect::new(
            x,
            (rail.y + rail.height * 0.5 - list_height * 0.5)
                .clamp(output.y, output.y + output.height - list_height),
            list_width,
            list_height,
        );
        Some(Self {
            rail,
            hit,
            list,
            segment_height: c.segment_height * fit,
            pitch: (c.segment_height + c.segment_gap) * fit,
            row_height,
            padding,
            rows,
        })
    }

    pub fn row(&self, visible_index: usize) -> Rect {
        Rect::new(
            self.list.x + self.padding,
            self.list.y + self.padding + visible_index as f64 * self.row_height,
            (self.list.width - 2.0 * self.padding).max(0.0),
            self.row_height,
        )
    }

    pub fn hover_contains(&self, p: Point<f64, Logical>) -> bool {
        let left = self.hit.x.min(self.list.x);
        let top = self.hit.y.min(self.list.y);
        contains(
            Rect::new(
                left,
                top,
                (self.hit.x + self.hit.width).max(self.list.x + self.list.width) - left,
                (self.hit.y + self.hit.height).max(self.list.y + self.list.height) - top,
            ),
            p,
        )
    }
}

impl Ferese {
    pub(crate) fn scrolling_tab_bar(&self, anchor: WindowId) -> Option<&ferese_layout::TabBar> {
        let workspace = self.workspaces.workspace_for_window(anchor)?;
        if !matches!(
            self.workspaces.workspace(workspace)?.layout,
            WorkspaceLayout::Scrolling(_)
        ) {
            return None;
        }
        self.visible_tab_bars(workspace)
            .find(|bar| bar.group.windows.first() == Some(&anchor))
    }

    pub(crate) fn tab_indicator_geometry(
        &self,
        bar: &ferese_layout::TabBar,
        content: Rect,
    ) -> Option<IndicatorGeometry> {
        self.tab_indicator_geometry_sampled(bar, content, None)
    }

    pub(crate) fn tab_indicator_geometry_for_frame(
        &self,
        bar: &ferese_layout::TabBar,
        content: Rect,
        frame: &super::FrameScene,
    ) -> Option<IndicatorGeometry> {
        self.tab_indicator_geometry_sampled(
            bar,
            content,
            frame.capture_tabs.as_ref().map(|_| frame),
        )
    }

    fn tab_indicator_geometry_sampled(
        &self,
        bar: &ferese_layout::TabBar,
        content: Rect,
        frame: Option<&super::FrameScene>,
    ) -> Option<IndicatorGeometry> {
        let workspace = self.workspaces.workspace_for_window(bar.group.active)?;
        let output_id = self.output_workspaces.output_for_workspace(workspace)?;
        if frame.is_none() && self.output_workspaces.active_workspace(output_id) != Some(workspace)
        {
            return None;
        }
        let output = self.outputs_by_id.get(&output_id)?;
        let g = self.space.output_geometry(output)?;
        let bounds = Rect::new(
            g.loc.x as f64,
            g.loc.y as f64,
            g.size.w as f64,
            g.size.h as f64,
        );
        let mut boundary = bounds.x;
        // Measure the current visible gap, including animated column positions.
        let captured = frame
            .into_iter()
            .flat_map(|frame| frame.windows.keys().copied());
        let displayed = self
            .space
            .elements()
            .filter(|_| frame.is_none())
            .filter_map(|window| self.windows.ids().get(window).copied());
        for id in captured.chain(displayed) {
            if id == bar.group.active
                || self.workspaces.workspace_for_window(id) != Some(workspace)
                || !self.workspaces.workspace(workspace)?.layout.contains(id)
            {
                continue;
            }
            let Some(rect) = frame.map_or_else(
                || self.presented_window_rect(id),
                |frame| {
                    frame
                        .windows
                        .get(&id)
                        .map(|sample| sample.presentation.bounds.current)
                },
            ) else {
                continue;
            };
            if rect.x + rect.width <= content.x
                && rect.y < content.y + content.height
                && rect.y + rect.height > content.y
            {
                boundary = boundary.max(rect.x + rect.width);
            }
        }
        let mut settings = self.tab_indicator_settings;
        if self
            .tab_indicator
            .hover
            .as_ref()
            .is_some_and(|h| bar.group.windows.first() == Some(&h.anchor))
        {
            settings.list_width = self.tab_indicator.hover_width;
        }
        IndicatorGeometry::new(content, bounds, boundary, bar.group.windows.len(), settings)
    }

    fn tab_indicator_can_hit(&self, active: WindowId, p: Point<f64, Logical>) -> bool {
        if self.session_lock.active()
            || self.input_capture.active()
            || self.overview.is_presenting()
            || self.seat.get_pointer().is_some_and(|p| p.is_grabbed())
            || self
                .layer_surface_under(p, &[Layer::Overlay, Layer::Top])
                .is_some()
        {
            return false;
        }
        // Match the renderer's per-window stacking. An overlay can cover its
        // own client and lower windows, never floating windows/popups above it.
        for window in self.space.elements().rev() {
            let Some(id) = self.windows.ids().get(window).copied() else {
                continue;
            };
            if id == active {
                break;
            }
            if self
                .presented_window_rect(id)
                .is_some_and(|r| contains(r, p))
            {
                return false;
            }
        }
        if self
            .window_under_visual(p)
            .is_some_and(|w| self.windows.ids().get(&w) == Some(&active))
            && let Some((mut surface, _)) = self.window_surface_under(p)
        {
            while let Some(parent) = smithay::wayland::compositor::get_parent(&surface) {
                surface = parent;
            }
            if self
                .windows
                .window(active)
                .and_then(|w| w.toplevel())
                .is_none_or(|t| t.wl_surface() != &surface)
            {
                return false;
            }
        }
        true
    }

    pub(crate) fn indicator_tab_at(&self, p: Point<f64, Logical>) -> Option<WindowId> {
        let workspace = self.workspace_under_pointer(p)?;
        if !matches!(
            self.workspaces.workspace(workspace)?.layout,
            WorkspaceLayout::Scrolling(_)
        ) {
            return None;
        }
        for bar in self.visible_tab_bars(workspace) {
            let Some(content) = self.presented_window_rect(bar.group.active) else {
                continue;
            };
            let Some(g) = self.tab_indicator_geometry(bar, content) else {
                continue;
            };
            if !self.tab_indicator_can_hit(bar.group.active, p) {
                continue;
            }
            let anchor = bar.group.windows[0];
            let expanded = self
                .tab_indicator
                .hover
                .as_ref()
                .is_some_and(|h| h.anchor == anchor && h.opacity.target > 0.0);
            if expanded && contains(g.list, p) {
                let first = self
                    .tab_indicator
                    .first_row
                    .min(bar.group.windows.len().saturating_sub(g.rows));
                let index = ((p.y - g.list.y - g.padding).max(0.0) / g.row_height).floor() as usize;
                return bar
                    .group
                    .windows
                    .get(first + index.min(g.rows - 1))
                    .copied();
            }
            if contains(g.hit, p) {
                let index = ((p.y - g.rail.y) / g.pitch).floor() as usize;
                return bar
                    .group
                    .windows
                    .get(index.min(bar.group.windows.len() - 1))
                    .copied();
            }
            if expanded && g.hover_contains(p) {
                return Some(bar.group.active);
            }
        }
        None
    }

    pub(crate) fn update_tab_indicator_hover(&mut self, p: Point<f64, Logical>) {
        let anchor = self.indicator_tab_at(p).and_then(|id| {
            self.workspaces
                .workspace_for_window(id)
                .and_then(|ws| self.workspaces.workspace(ws))
                .and_then(|ws| ws.layout.tab_group(id))
                .and_then(|group| group.windows.first().copied())
        });
        let unchanged = match (anchor, &self.tab_indicator.hover) {
            (Some(anchor), Some(hover)) => hover.anchor == anchor && hover.opacity.target == 1.0,
            (None, Some(hover)) => hover.opacity.target == 0.0,
            (None, None) => true,
            _ => false,
        };
        if unchanged {
            return;
        }
        self.advance_animations(Instant::now());
        if let Some(anchor) = anchor {
            let same = self
                .tab_indicator
                .hover
                .as_ref()
                .is_some_and(|h| h.anchor == anchor);
            if !same {
                let selected = self
                    .scrolling_tab_bar(anchor)
                    .and_then(|bar| {
                        let g = self.tab_indicator_geometry(
                            bar,
                            self.presented_window_rect(bar.group.active)?,
                        )?;
                        let selected = bar
                            .group
                            .windows
                            .iter()
                            .position(|id| *id == bar.group.active)?;
                        Some(
                            selected
                                .saturating_sub(g.rows / 2)
                                .min(bar.group.windows.len().saturating_sub(g.rows)),
                        )
                    })
                    .unwrap_or(0);
                self.tab_indicator.first_row = selected;
                let settings = self.tab_indicator_settings;
                let text_width = self
                    .scrolling_tab_bar(anchor)
                    .into_iter()
                    .flat_map(|bar| &bar.group.windows)
                    .filter_map(|id| self.windows.record(*id))
                    .map(|record| {
                        let title = if !record.title.trim().is_empty() {
                            record.title.as_str()
                        } else if !record.app_id.is_empty() {
                            record.app_id.as_str()
                        } else {
                            "Window"
                        };
                        self.indicator_text_width(title, settings.font_size)
                    })
                    .fold(0.0_f64, f64::max);
                // Fix the width for this hover session, so metadata updates do
                // not move rows or their click targets under the pointer.
                self.tab_indicator.hover_width =
                    (text_width + 16.0 + 2.0 * settings.padding).clamp(80.0, settings.list_width);
                self.tab_indicator.hover = Some(Overlay {
                    anchor,
                    opacity: AnimatedValue::new(0.0),
                });
            }
            if let Some(hover) = &mut self.tab_indicator.hover {
                if hover.opacity.target == 1.0 {
                    return;
                }
                hover.opacity.retarget_preserving_motion(1.0);
                if !self.animations_enabled {
                    hover.opacity.snap();
                }
            }
        } else if let Some(hover) = &mut self.tab_indicator.hover {
            if hover.opacity.target == 0.0 {
                return;
            }
            hover.opacity.retarget_preserving_motion(0.0);
            if !self.animations_enabled {
                hover.opacity.snap();
            }
        } else {
            return;
        }
        crate::backends::direct::render_all(self);
    }

    pub(crate) fn scroll_tab_indicator(&mut self, p: Point<f64, Logical>, amount: f64) -> bool {
        let Some(hover) = &self.tab_indicator.hover else {
            return false;
        };
        if hover.opacity.target == 0.0 || self.indicator_tab_at(p).is_none() {
            return false;
        }
        let Some(bar) = self.scrolling_tab_bar(hover.anchor) else {
            return false;
        };
        let Some(content) = self.presented_window_rect(bar.group.active) else {
            return false;
        };
        let Some(g) = self.tab_indicator_geometry(bar, content) else {
            return false;
        };
        let max = bar.group.windows.len().saturating_sub(g.rows);
        if amount != 0.0 {
            self.tab_indicator.first_row = if amount > 0.0 {
                self.tab_indicator.first_row.saturating_add(1).min(max)
            } else {
                self.tab_indicator.first_row.saturating_sub(1).min(max)
            };
            crate::backends::direct::render_all(self);
        }
        true
    }

    pub(crate) fn flash_tab_switch(&mut self, previous: WindowId, current: WindowId) {
        if previous == current {
            return;
        }
        let Some(workspace) = self
            .workspaces
            .workspace_for_window(current)
            .and_then(|ws| self.workspaces.workspace(ws))
        else {
            return;
        };
        let WorkspaceLayout::Scrolling(layout) = &workspace.layout else {
            return;
        };
        let Some(group) = layout
            .tab_group(current)
            .filter(|g| g.windows.contains(&previous))
        else {
            return;
        };
        if self.tab_indicator_settings.flash_duration.is_zero() {
            return;
        }
        let anchor = group.windows[0];
        if let Some(token) = self.tab_indicator.timer.take() {
            self.loop_handle.remove(token);
        }
        if let Some(flash) = self
            .tab_indicator
            .flash
            .as_mut()
            .filter(|f| f.anchor == anchor)
        {
            flash.opacity.retarget_preserving_motion(1.0);
        } else {
            let mut opacity = AnimatedValue::new(0.0);
            opacity.set_target(1.0);
            self.tab_indicator.flash = Some(Overlay { anchor, opacity });
        }
        if !self.animations_enabled
            && let Some(flash) = &mut self.tab_indicator.flash
        {
            flash.opacity.snap();
        }
        match self.loop_handle.insert_source(
            Timer::from_duration(self.tab_indicator_settings.flash_duration),
            |_, _, state| {
                state.tab_indicator.timer = None;
                if let Some(flash) = &mut state.tab_indicator.flash {
                    flash.opacity.set_target(0.0);
                    if !state.animations_enabled {
                        flash.opacity.snap();
                    }
                }
                crate::backends::direct::render_all(state);
                TimeoutAction::Drop
            },
        ) {
            Ok(token) => self.tab_indicator.timer = Some(token),
            Err(error) => {
                self.tab_indicator.flash = None;
                tracing::warn!(%error, "could not schedule tab feedback");
            }
        }
        crate::backends::direct::render_all(self);
    }

    pub(crate) fn reconcile_tab_indicator(&mut self) {
        if self.session_lock.active()
            || self.input_capture.active()
            || self.overview.is_presenting()
        {
            self.clear_tab_indicator();
            return;
        }
        if self
            .tab_indicator
            .hover
            .as_ref()
            .is_some_and(|h| self.scrolling_tab_bar(h.anchor).is_none())
        {
            self.tab_indicator.hover = None;
        }
        if self
            .tab_indicator
            .flash
            .as_ref()
            .is_some_and(|h| self.scrolling_tab_bar(h.anchor).is_none())
        {
            self.tab_indicator.flash = None;
            if let Some(token) = self.tab_indicator.timer.take() {
                self.loop_handle.remove(token);
            }
        }
    }

    pub(crate) fn clear_tab_indicator(&mut self) {
        if let Some(token) = self.tab_indicator.timer.take() {
            self.loop_handle.remove(token);
        }
        self.tab_indicator = TabIndicatorState::default();
    }

    pub(crate) fn tab_indicator_output_animating(&self, output: &Output) -> bool {
        [&self.tab_indicator.hover, &self.tab_indicator.flash]
            .into_iter()
            .flatten()
            .any(|o| o.opacity.is_animating() && self.window_belongs_to_output(o.anchor, output))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings() -> TabIndicatorSettings {
        crate::config::Config::parse_source("")
            .unwrap()
            .tab_indicator_settings()
            .unwrap()
    }

    #[test]
    fn rail_uses_gap_or_inner_edge_without_changing_content_geometry() {
        let content = Rect::new(100.25, 20.5, 400., 300.);
        let output = Rect::new(0., 0., 800., 600.);
        let c = settings();
        for gap in [0., 7.99, 8., 24.] {
            let g = IndicatorGeometry::new(content, output, content.x - gap, 3, c).unwrap();
            assert_eq!(
                (g.rail.width, g.rail.height, g.segment_height, g.pitch),
                (4., 80., 24., 28.)
            );
            assert_eq!(g.rail.y, content.y + (content.height - 80.) * 0.5);
            assert_eq!(g.hit.width, 12.);
            if gap >= 8. {
                assert!(g.rail.x >= content.x - gap && g.rail.x + g.rail.width <= content.x);
            } else {
                assert!(g.rail.x >= content.x && g.rail.x + g.rail.width <= content.x + 12.);
            }
            assert!(g.hover_contains((g.hit.x + g.hit.width, g.rail.y + 2.).into()));
        }
        assert!(IndicatorGeometry::new(content, output, 0., 1, c).is_none());
    }

    #[test]
    fn overflow_and_fractional_output_edges_keep_list_and_rail_reachable() {
        for output in [
            Rect::new(-800.25, 10.5, 800., 600.),
            Rect::new(0., 0., 100., 40.),
            Rect::new(0., 0., 8., 0.5),
        ] {
            let content = Rect::new(output.x - 20., output.y, output.width + 40., output.height);
            let g = IndicatorGeometry::new(content, output, output.x, 100, settings()).unwrap();
            for rect in [g.rail, g.hit, g.list] {
                assert!(rect.x >= output.x && rect.y >= output.y);
                assert!(rect.x + rect.width <= output.x + output.width + 1e-8);
                assert!(rect.y + rect.height <= output.y + output.height + 1e-8);
            }
            assert!(g.rows < 100 && g.rows > 0);
            assert!(g.segment_height > 0.);
        }
    }

    #[test]
    fn interrupted_hover_spring_keeps_motion_and_settles_without_background_ticks() {
        let mut state = TabIndicatorState {
            hover: Some(Overlay {
                anchor: WindowId(1),
                opacity: AnimatedValue::new(0.),
            }),
            ..Default::default()
        };
        state.hover.as_mut().unwrap().opacity.set_target(1.);
        state.advance(Duration::from_millis(30), SpringConfig::default(), true);
        let before = state.hover.as_ref().unwrap().opacity;
        state
            .hover
            .as_mut()
            .unwrap()
            .opacity
            .retarget_preserving_motion(0.);
        let after = state.hover.as_ref().unwrap().opacity;
        assert_eq!(
            (before.current, before.velocity),
            (after.current, after.velocity)
        );
        for _ in 0..300 {
            state.advance(Duration::from_millis(16), SpringConfig::default(), true);
        }
        assert!(state.hover.is_none());
        assert!(!state.is_animating());
    }
}
