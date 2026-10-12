use super::*;

impl Ferese {
    pub(crate) fn join_tab(&mut self, direction: Direction) {
        let Some(window) = self.focused_window else {
            return;
        };
        let Some(bounds) = self.output_bounds() else {
            return;
        };
        let workspace = self.workspaces.active();
        if workspace.fullscreen.is_some() {
            return;
        }
        let Ok(Some(target)) = workspace.layout.directional_neighbor(window, direction, bounds) else {
            return;
        };
        if let WorkspaceLayout::Scrolling(layout) = &mut self.workspaces.active_mut().layout {
            // Joining rows already in the same column changes their display only.
            let same_column = layout
                .columns()
                .iter()
                .any(|c| c.windows.contains(&window) && c.windows.contains(&target));
            if same_column {
                let _ = layout.set_column_display(window, ferese_layout::ColumnDisplay::Tabbed);
                self.relayout();
                return;
            }
        }
        if let Err(error) = self.workspaces.stack_window(window, target) {
            tracing::warn!(%error, "failed to join tab group");
            return;
        }
        if let WorkspaceLayout::Scrolling(layout) = &mut self.workspaces.active_mut().layout {
            let _ = layout.set_column_display(window, ferese_layout::ColumnDisplay::Tabbed);
        }
        self.relayout();
        self.restore_keyboard_focus();
    }

    pub(crate) fn toggle_column_tabbed(&mut self) {
        let Some(window) = self.focused_window else {
            return;
        };
        if self.workspaces.active().fullscreen.is_some() {
            return;
        }
        let WorkspaceLayout::Scrolling(layout) = &mut self.workspaces.active_mut().layout else {
            return;
        };
        if layout.toggle_column_tabbed(window).is_ok() {
            self.relayout();
        }
    }

    pub(crate) fn leave_tab(&mut self) {
        if self.workspaces.active().fullscreen.is_none()
            && self
                .focused_window
                .and_then(|id| self.workspaces.active().layout.tab_group(id))
                .is_some()
        {
            self.expel_focused_window();
        }
    }

    pub(crate) fn cycle_tab(&mut self, previous: bool) {
        let Some(window) = self.focused_window else {
            return;
        };
        if self.workspaces.active().fullscreen.is_some() {
            return;
        }
        if let Some(target) = self.workspaces.active().layout.adjacent_tab(window, previous)
            && self.activate_managed_window(target)
        {
            self.flash_tab_switch(window, target);
        }
    }

    pub(crate) fn visible_tab_bars(&self, workspace: WorkspaceId) -> impl Iterator<Item = &ferese_layout::TabBar> {
        self.tab_bars.get(&workspace).into_iter().flatten().filter(move |bar| {
            self.workspaces
                .workspace(workspace)
                .is_some_and(|ws| ws.fullscreen.is_none())
                && self.windows.record(bar.group.active).is_some_and(|w| !w.maximized)
                && self
                    .windows
                    .window(bar.group.active)
                    .is_some_and(|w| self.space.element_location(w).is_some())
        })
    }

    pub(crate) fn tab_at(&self, position: Point<f64, Logical>) -> Option<WindowId> {
        if self.session_lock.active()
            || self.overview.is_active()
            || self.input_capture.active()
            || self
                .layer_surface_under(position, &[Layer::Overlay, Layer::Top])
                .is_some()
        {
            return None;
        }
        let workspace = self.workspace_under_pointer(position)?;
        if matches!(
            self.workspaces.workspace(workspace)?.layout,
            WorkspaceLayout::Scrolling(_)
        ) {
            return self.indicator_tab_at(position);
        }
        // Floating windows and popups in front of the group keep their input.
        if self.window_surface_under(position).is_some() {
            return None;
        }
        self.visible_tab_bars(workspace).find_map(|bar| {
            let content = self.presented_window_rect(bar.group.active)?;
            let rect = Rect::new(content.x, content.y - bar.rect.height, content.width, bar.rect.height);
            tab_hit(&bar.group.windows, rect, position.x, position.y)
        })
    }
}

pub(super) fn tab_hit(windows: &[WindowId], rect: Rect, x: f64, y: f64) -> Option<WindowId> {
    if windows.is_empty()
        || rect.width <= 0.0
        || x < rect.x
        || x >= rect.x + rect.width
        || y < rect.y
        || y >= rect.y + rect.height
    {
        return None;
    }
    windows
        .get(((x - rect.x) / rect.width * windows.len() as f64).floor() as usize)
        .copied()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn tab_hit_uses_half_open_rectangles_at_fractional_coordinates() {
        let windows = [WindowId(1), WindowId(2), WindowId(3)];
        let rect = Rect::new(10.5, 20.5, 301., 30.);
        assert_eq!(tab_hit(&windows, rect, 10.5, 20.5), Some(windows[0]));
        assert_eq!(tab_hit(&windows, rect, 111., 25.), Some(windows[1]));
        assert_eq!(tab_hit(&windows, rect, 311.49, 25.), Some(windows[2]));
        for (x, y) in [(10., 25.), (311.5, 25.), (20., 50.5)] {
            assert_eq!(tab_hit(&windows, rect, x, y), None);
        }
    }
}
