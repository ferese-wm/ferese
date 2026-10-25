use super::{
    ClientSize, CursorImageStatus, Ferese, Layer, LayerSurface, Logical, Output, Point, Rectangle,
    Window, WindowId, WindowSurfaceType, WlSurface, WorkspaceId, layer_map_for_output,
    send_frames_surface_tree, window_has_buffer,
};

impl Ferese {
    pub fn surface_under(
        &self,
        position: Point<f64, Logical>,
    ) -> Option<(WlSurface, Point<f64, Logical>)> {
        if self.session_lock.active() {
            return self.lock_surface_under(position);
        }
        if self.input_capture.active() {
            return None;
        }
        // The workspace strip is compositor UI, including its live miniatures.
        // Surfaces behind it must not acquire pointer focus or receive clicks.
        if self.overview_strip_at(position).is_some() {
            return None;
        }
        if self.tab_at(position).is_some() {
            return None;
        }
        self.layer_surface_under(position, &[Layer::Overlay, Layer::Top])
            .map(|(_, surface, origin)| (surface, origin))
            .or_else(|| self.window_surface_under(position))
            .or_else(|| {
                if self.overview.is_active() {
                    return None;
                }

                self.layer_surface_under(position, &[Layer::Bottom, Layer::Background])
                    .map(|(_, surface, origin)| (surface, origin))
            })
    }

    pub fn layer_under(
        &self,
        position: Point<f64, Logical>,
    ) -> Option<(LayerSurface, WlSurface, Point<f64, Logical>)> {
        if self.session_lock.active() {
            return None;
        }
        if self.overview_strip_at(position).is_some() {
            return None;
        }
        if let Some(layer) = self.layer_surface_under(position, &[Layer::Overlay, Layer::Top]) {
            return Some(layer);
        }
        if self.overview.is_active() || self.window_surface_under(position).is_some() {
            return None;
        }

        self.layer_surface_under(position, &[Layer::Bottom, Layer::Background])
    }

    pub(super) fn window_surface_under(
        &self,
        position: Point<f64, Logical>,
    ) -> Option<(WlSurface, Point<f64, Logical>)> {
        if self.overview.is_active() {
            return None;
        }

        let workspace = self.workspace_under_pointer(position)?;
        self.space.elements().rev().find_map(|window| {
            let id = self.windows.ids().get(window)?;
            if self.workspaces.workspace_for_window(*id) != Some(workspace) {
                return None;
            }
            let visual = self.presented_window_rect(*id)?;
            let inside_visual = position.x >= visual.x
                && position.y >= visual.y
                && position.x < visual.x + visual.width
                && position.y < visual.y + visual.height;
            if !inside_visual {
                return None;
            }

            let (source_x, source_y) =
                self.inverse_presented_window_point(*id, position.x, position.y)?;
            let source_point = Point::from((source_x, source_y)) + window.geometry().loc.to_f64();

            let surface = window.surface_under(source_point, WindowSurfaceType::ALL);
            let (surface, surface_location) = match surface {
                Some(hit) => hit,
                None if self.window_fills_visual_bounds(*id, workspace) => {
                    (window.toplevel()?.wl_surface().clone(), Point::from((0, 0)))
                }
                None => return None,
            };
            let surface_point = source_point - surface_location.to_f64();
            Some((surface, position - surface_point))
        })
    }

    pub(super) fn window_fills_visual_bounds(&self, id: WindowId, workspace: WorkspaceId) -> bool {
        self.windows.record(id).is_some_and(|w| w.maximized)
            || self
                .workspaces
                .workspace(workspace)
                .is_some_and(|workspace| workspace.fullscreen == Some(id))
    }

    pub(super) fn layer_surface_under(
        &self,
        position: Point<f64, Logical>,
        layers: &[Layer],
    ) -> Option<(LayerSurface, WlSurface, Point<f64, Logical>)> {
        let output = self.space.output_under(position).next()?;
        let output_geometry = self.space.output_geometry(output)?;
        let output_position = position - output_geometry.loc.to_f64();
        let map = layer_map_for_output(output);

        for requested in layers {
            if !self.layer_visible_on_output(output, *requested) {
                continue;
            }
            for layer in map.layers_on(*requested).rev() {
                let geometry = map.layer_geometry(layer)?;
                let layer_position = output_position - geometry.loc.to_f64();
                let Some((surface, surface_location)) =
                    layer.surface_under(layer_position, WindowSurfaceType::ALL)
                else {
                    continue;
                };
                let origin = output_geometry.loc + geometry.loc + surface_location;

                return Some((layer.clone(), surface, origin.to_f64()));
            }
        }

        None
    }

    pub fn send_cursor_frame(&self, output: &Output) {
        let eligible = self.callback_outputs();
        let CursorImageStatus::Surface(surface) = &self.cursor_status else {
            return;
        };
        let Some(pointer) = self.seat.get_pointer() else {
            return;
        };
        if self.space.output_under(pointer.current_location()).next() != Some(output) {
            return;
        }

        send_frames_surface_tree(
            surface,
            output,
            self.start_time.elapsed(),
            None,
            |surface, _| eligible.get(&surface.into()).cloned(),
        );
    }

    pub fn window_under_visual(&self, position: Point<f64, Logical>) -> Option<Window> {
        let overview_active = self.overview.is_active();
        let workspace = self.workspace_under_pointer(position)?;

        let hit = |window: &Window| -> Option<Window> {
            if !self.window_content_ready(window) {
                return None;
            }

            let id = self.windows.ids().get(window)?;
            if self.workspaces.workspace_for_window(*id) != Some(workspace) {
                return None;
            }

            let visual = self.presented_window_rect(*id)?;
            let caption_height = if overview_active { 34.0 } else { 0.0 };
            let inside_visual = position.x >= visual.x
                && position.y >= visual.y
                && position.x < visual.x + visual.width
                && position.y < visual.y + visual.height + caption_height;
            if !inside_visual {
                return None;
            }

            if !overview_active {
                let (source_x, source_y) =
                    self.inverse_presented_window_point(*id, position.x, position.y)?;
                let source_point =
                    Point::from((source_x, source_y)) + window.geometry().loc.to_f64();
                if window
                    .surface_under(source_point, WindowSurfaceType::ALL)
                    .is_none()
                    && !self.window_fills_visual_bounds(*id, workspace)
                {
                    return None;
                }
            }

            Some(window.clone())
        };

        if overview_active {
            self.windows.overview_windows().find_map(hit)
        } else {
            self.space.elements().rev().find_map(hit)
        }
    }

    pub(super) fn workspace_under_pointer(
        &self,
        position: Point<f64, Logical>,
    ) -> Option<WorkspaceId> {
        let output = self.space.output_under(position).next()?;
        self.output_workspaces
            .active_workspace(self.output_id(output)?)
    }

    pub(crate) fn window_belongs_to_output(&self, window: WindowId, output: &Output) -> bool {
        if let Some(rect) = self
            .pointer_operation
            .as_ref()
            .filter(|operation| operation.id == window)
            .and_then(|operation| operation.visual)
        {
            return self.space.output_geometry(output).is_some_and(|area| {
                rect.x < (area.loc.x + area.size.w) as f64
                    && rect.x + rect.width > area.loc.x as f64
                    && rect.y < (area.loc.y + area.size.h) as f64
                    && rect.y + rect.height > area.loc.y as f64
            });
        }
        let Some(output_id) = self.output_id(output) else {
            return false;
        };
        if self.focus_cycle.is_some() && self.overview.is_active() {
            return self.overview.has_window_preview(window)
                && self.focus_preview_output(window) == Some(output_id);
        }
        let workspace = self.workspaces.workspace_for_window(window);
        workspace == self.output_workspaces.active_workspace(output_id)
            || self
                .workspace_slides
                .get(&output_id)
                .is_some_and(|slide| workspace.is_some_and(|workspace| slide.contains(workspace)))
    }

    pub fn visual_scale_for_window(&self, window: &Window) -> Option<(f64, f64)> {
        let id = self.windows.ids().get(window)?;
        if !self.overview.is_presenting() && self.windows.record(*id)?.opening.is_none() {
            return Some((1.0, 1.0));
        }
        let geometry = self.windows.geometry(id)?;
        let source = if self.overview.is_presenting() {
            geometry.client.committed_size?
        } else {
            ClientSize::from_rect(geometry.visual.current)
        };
        let presented = self.presented_window_rect(*id)?;

        if source.width <= 0 || source.height <= 0 {
            return None;
        }

        Some((
            presented.width / f64::from(source.width),
            presented.height / f64::from(source.height),
        ))
    }

    pub(crate) fn visual_rect_for_window(
        &self,
        window: &Window,
    ) -> Option<Rectangle<i32, Logical>> {
        let id = self.windows.ids().get(window)?;
        let rect = self.presented_window_rect(*id)?;
        let size = ClientSize::from_rect(rect);
        Some(Rectangle::new(
            (rect.x.round() as i32, rect.y.round() as i32).into(),
            (size.width, size.height).into(),
        ))
    }

    pub(crate) fn window_content_ready(&self, window: &Window) -> bool {
        window_has_buffer(window)
            && self
                .windows
                .ids()
                .get(window)
                .and_then(|id| self.windows.geometry(id))
                .is_some_and(|geometry| geometry.client.committed_size.is_some())
    }
}
