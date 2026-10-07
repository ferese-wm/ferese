use super::*;

impl Ferese {
    pub(super) fn adjust_floating_direction(&mut self, id: WindowId, direction: Direction, resize: bool) {
        const STEP: f64 = 32.0;
        let workspace = self.workspaces.active();
        if self.session_lock.active()
            || self.input_capture.captures(1)
            || self.workspaces.workspace_for_window(id) != Some(workspace.id)
            || workspace.fullscreen == Some(id)
            || self.windows.record(id).is_some_and(|record| record.maximized)
        {
            return;
        }
        let Some(WindowPlacement::Floating { rect }) = self.workspaces.placement(id) else {
            return;
        };
        let Some(work) = self.floating_bounds_for_window(id) else {
            return;
        };
        let mut destination = rect;
        match (resize, direction) {
            (false, Direction::Left) => destination.x -= STEP,
            (false, Direction::Right) => destination.x += STEP,
            (false, Direction::Up) => destination.y -= STEP,
            (false, Direction::Down) => destination.y += STEP,
            (true, Direction::Left) => destination.width -= STEP,
            (true, Direction::Right) => destination.width += STEP,
            (true, Direction::Up) => destination.height -= STEP,
            (true, Direction::Down) => destination.height += STEP,
        }
        if resize {
            destination = constrained_floating_rect(
                destination,
                self.window_constraints().get(&id).copied().unwrap_or_default(),
            );
        }
        destination = crate::floating::clamp_to_work(destination, work);
        if destination == rect {
            return;
        }
        if let Err(error) = self.workspaces.set_floating_rect(id, destination) {
            tracing::error!(%error, ?id, "failed to adjust floating window geometry");
            return;
        }
        self.windows.update(id, |record| {
            record.resize_anchor = resize.then_some((false, false, destination))
        });
        self.relayout_window(id);
        if let Some(window) = self.windows.window(id).cloned() {
            self.remember_floating(&window);
        }
    }

    pub fn toggle_focused_floating(&mut self) {
        let Some(window) = self.focused_window else {
            return;
        };
        let Some(bounds) = self.output_bounds() else {
            return;
        };
        let mut floating_rect = match self.workspaces.placement(window) {
            Some(WindowPlacement::Tiled) => self
                .tiled_layout(bounds)
                .ok()
                .and_then(|layout| layout.geometry.get(&window).copied())
                .unwrap_or_else(|| centered_floating_rect(bounds)),
            Some(WindowPlacement::Floating { rect }) => rect,
            None => return,
        };
        if let Some(handle) = self.windows.window(window).cloned() {
            if matches!(
                self.workspaces.placement(window),
                Some(WindowPlacement::Floating { .. })
            ) {
                self.remember_floating(&handle);
            } else {
                floating_rect = self.place_floating(
                    &handle,
                    Some(window),
                    bounds,
                    (floating_rect.width, floating_rect.height),
                    None,
                );
            }
        }
        let axis = self
            .workspaces
            .active()
            .layout
            .automatic_axis(Some(window), bounds)
            .unwrap_or(Axis::Horizontal);

        if let Err(error) = self.workspaces.toggle_floating(window, floating_rect, axis, 0.5) {
            tracing::error!(%error, ?window, "failed to toggle floating window");
            return;
        }

        self.relayout_window(window);
    }

    pub fn toggle_focused_fullscreen(&mut self) {
        let Some(window) = self.focused_window else {
            return;
        };

        if let Err(error) = self.workspaces.toggle_fullscreen(window) {
            tracing::error!(%error, ?window, "failed to toggle fullscreen window");
            return;
        }

        self.relayout_window(window);
    }

    pub fn toggle_focused_maximized(&mut self) {
        let Some(window) = self.focused_window else {
            return;
        };
        // Super+F from true fullscreen enters decorated maximization.
        let fullscreen = self
            .workspaces
            .workspace_for_window(window)
            .and_then(|workspace| self.workspaces.workspace(workspace))
            .is_some_and(|workspace| workspace.fullscreen == Some(window));
        let enabled = fullscreen || !self.windows.record(window).is_some_and(|w| w.maximized);
        self.set_window_maximized(window, enabled);
    }

    pub fn set_window_maximized(&mut self, window: WindowId, enabled: bool) {
        if self.workspaces.workspace_for_window(window).is_none() {
            return;
        }
        if enabled {
            let _ = self.workspaces.set_fullscreen(window, false);
            self.windows.update(window, |w| w.maximized = true);
        } else {
            self.windows.update(window, |w| w.maximized = false);
        }
        if self.workspaces.placement(window) == Some(WindowPlacement::Tiled) {
            let workspace_id = self.workspaces.workspace_for_window(window).unwrap();
            if let Some(workspace) = self.workspaces.workspace_mut(workspace_id)
                && let WorkspaceLayout::Scrolling(layout) = &mut workspace.layout
            {
                if enabled {
                    if let Some(column) = layout.columns().iter().find(|column| column.windows.contains(&window)) {
                        self.windows.update(window, |w| {
                            w.maximized_column_width.get_or_insert(column.width);
                        });
                    }
                    let _ = layout.set_column_width(window, ColumnWidth::Proportion(1.0));
                } else if let Some(width) = self
                    .windows
                    .update(window, |w| w.maximized_column_width.take())
                    .flatten()
                {
                    let _ = layout.set_column_width(window, width);
                }
            }
        }
        self.relayout_window(window);
    }

    pub(crate) fn output_has_fullscreen(&self, output: &Output) -> bool {
        self.output_has_fullscreen_for_frame(output, self.overview.is_presenting())
    }

    pub(crate) fn output_has_fullscreen_for_frame(&self, output: &Output, overview: bool) -> bool {
        !overview
            && self
                .output_id(output)
                .and_then(|id| self.output_workspaces.active_workspace(id))
                .and_then(|workspace| self.workspaces.workspace(workspace))
                .is_some_and(|workspace| workspace.fullscreen.is_some())
    }

    pub fn set_window_fullscreen(&mut self, window: WindowId, enabled: bool) {
        match self.workspaces.set_fullscreen(window, enabled) {
            Ok(true) => self.relayout_window(window),
            Ok(false) => {}
            Err(error) => {
                tracing::error!(%error, ?window, enabled, "failed to set fullscreen window")
            }
        }
    }

    pub fn is_floating_window(&self, window: &Window) -> bool {
        self.windows
            .ids()
            .get(window)
            .and_then(|id| self.workspaces.placement(*id))
            .is_some_and(|placement| matches!(placement, WindowPlacement::Floating { .. }))
    }

    pub fn set_floating_window_geometry(
        &mut self,
        window: &Window,
        location: Point<i32, Logical>,
        size: Size<i32, Logical>,
    ) {
        let Some(id) = self.windows.ids().get(window).copied() else {
            return;
        };
        self.windows.update(id, |w| w.resize_anchor.take()).flatten();
        let rect = Rect::new(
            location.x as f64,
            location.y as f64,
            size.w.max(1) as f64,
            size.h.max(1) as f64,
        );

        if let Err(error) = self.workspaces.set_floating_rect(id, rect) {
            tracing::error!(%error, ?id, "failed to update floating window geometry");
            return;
        }

        // Direct manipulation follows the pointer, including while the client is
        // still drawing its next buffer. Rendering and hit testing share this rect.
        #[cfg(feature = "resize-metrics")]
        self.resize_metrics
            .end(id, self.start_time.elapsed(), crate::resize_metrics::End::Cancelled);
        self.windows.clear_transaction(&id);
        self.render.clear_snapshot(&id);
        let Some(record) = self.windows.record_mut(id) else {
            return;
        };
        let geometry = record
            .geometry
            .get_or_insert_with(|| WindowGeometry::new(rect, client_size(window)));

        if geometry.is_zooming() {
            self.stacking_cache.invalidate();
        }

        if let Some(size) = geometry.follow_pointer(rect, self.start_time.elapsed())
            && let Some(toplevel) = window.toplevel()
        {
            toplevel.with_pending_state(|state| {
                state.size = Some((size.width, size.height).into());
            });
            toplevel.send_pending_configure();
        }
        self.windows.update(id, |record| record.world_x = None);
        self.windows.update(id, |record| record.coupled_width = None);
        self.map_window_geometry(window.clone(), location);
        self.sync_window_stacking();
        // Move/resize/cancel callbacks run with Smithay's pointer mutex held.
        // Cursor rendering reads current_location(), which would lock it again.
        // The event-loop epilogue coalesces this redraw after the grab returns.
        let outputs = self
            .space
            .outputs()
            .filter(|output| self.window_belongs_to_output(id, output))
            .cloned()
            .collect::<Vec<_>>();
        for output in outputs {
            self.defer_output_redraw(output);
        }

        self.cursor_redraw_pending = true;
    }

    pub(super) fn output_bounds(&self) -> Option<Rect> {
        let output = self.focused_output().or_else(|| self.space.outputs().next())?;
        self.output_bounds_for(output)
    }

    pub(super) fn floating_outputs(&self) -> Vec<(String, Rect)> {
        self.space
            .outputs()
            .filter_map(|output| self.output_bounds_for(output).map(|work| (output.name(), work)))
            .collect()
    }

    pub(super) fn floating_metadata(window: &Window) -> (Option<String>, Option<WlSurface>) {
        window
            .toplevel()
            .map(|top| {
                let app = with_states(top.wl_surface(), |states| {
                    states
                        .data_map
                        .get::<XdgToplevelSurfaceData>()
                        .and_then(|data| data.lock().ok().and_then(|data| data.app_id.clone()))
                });
                (app, top.parent())
            })
            .unwrap_or_default()
    }

    pub(super) fn floating_obstacles(&self, exclude: Option<WindowId>, work: Rect) -> Vec<(u64, Rect)> {
        self.space
            .elements()
            .filter_map(|window| {
                let id = *self.windows.ids().get(window)?;
                if Some(id) == exclude {
                    return None;
                }
                let workspace = self.workspaces.workspace_for_window(id)?;
                let owner = self.output_workspaces.output_for_workspace(workspace)?;
                if self.output_workspaces.active_workspace(owner) != Some(workspace) {
                    return None;
                }
                if self
                    .workspaces
                    .workspace(workspace)?
                    .fullscreen
                    .is_some_and(|full| full != id)
                    && !matches!(self.workspaces.placement(id), Some(WindowPlacement::Floating { .. }))
                {
                    return None;
                }
                let rect = self.presented_window_rect(id)?;
                crate::floating::intersection(rect, work).map(|visible| (id.0, visible))
            })
            .collect()
    }

    pub(super) fn place_floating(
        &mut self,
        window: &Window,
        id: Option<WindowId>,
        work: Rect,
        size: (f64, f64),
        overrides: Option<(Option<f64>, Option<f64>)>,
    ) -> Rect {
        use crate::floating::{centered, clamp_to_work, intersection, min_overlap};
        let outputs = self.floating_outputs();
        let output = outputs
            .iter()
            .find(|(_, area)| *area == work)
            .map(|(name, _)| name.as_str())
            .unwrap_or("unknown");
        let (app, parent) = Self::floating_metadata(window);
        let parent_rect = parent
            .as_ref()
            .and_then(|surface| self.windows.id_for_surface(surface))
            .and_then(|parent| self.presented_window_rect(parent))
            .and_then(|rect| intersection(rect, work));
        let dialog = crate::window_rules::is_native_dialog(app.as_deref());
        let remembered = id
            .and_then(|id| self.windows.record(id).and_then(|w| w.floating_memory.as_ref()))
            .or_else(|| {
                if parent.is_none() {
                    app.as_deref().and_then(|app| self.floating_memory.get(app))
                } else {
                    None
                }
            })
            .filter(|entry| !dialog && entry.output == output)
            .and_then(|entry| entry.restore(&outputs));
        let (width, height) = overrides.unwrap_or_default();
        let constraints = window
            .toplevel()
            .map(|top| {
                let (minimum, maximum) = with_states(top.wl_surface(), |states| {
                    let mut cache = states.cached_state.get::<SurfaceCachedState>();
                    let state = cache.current();
                    (
                        (state.min_size.w, state.min_size.h),
                        (state.max_size.w, state.max_size.h),
                    )
                });
                let (minimum, maximum) = self.effective_size_constraints(window, minimum, maximum);
                SizeConstraints {
                    min_width: minimum.0.max(1) as f64,
                    min_height: minimum.1.max(1) as f64,
                    max_width: (maximum.0 > 0).then_some(maximum.0 as f64),
                    max_height: (maximum.1 > 0).then_some(maximum.1 as f64),
                }
            })
            .unwrap_or_default();
        let stored_size = if parent_rect.is_none() {
            remembered.map(|r| (r.width, r.height)).unwrap_or(size)
        } else {
            size
        };
        let sized = constrained_floating_rect(
            Rect::new(0., 0., width.unwrap_or(stored_size.0), height.unwrap_or(stored_size.1)),
            constraints,
        );
        let dimensions = (sized.width, sized.height);
        let rect = if dialog {
            centered(dimensions, work)
        } else if let Some(parent) = parent_rect {
            centered(dimensions, parent)
        } else if let Some(mut rect) = remembered {
            rect.width = dimensions.0;
            rect.height = dimensions.1;
            rect
        } else {
            let others = self
                .floating_obstacles(id, work)
                .into_iter()
                .map(|(_, r)| r)
                .collect::<Vec<_>>();
            let anchor = id
                .and_then(|id| self.windows.record(id).and_then(|w| w.placement_anchor))
                .or_else(|| {
                    self.focused_window
                        .filter(|focused| Some(*focused) != id)
                        .and_then(|focused| self.presented_window_rect(focused))
                })
                .and_then(|rect| intersection(rect, work))
                .unwrap_or(work);
            min_overlap(
                dimensions,
                work,
                &others,
                (anchor.x + anchor.width / 2., anchor.y + anchor.height / 2.),
            )
            .unwrap_or_else(|| self.floating_cascade.place(output, work, dimensions))
        };
        clamp_to_work(rect, work)
    }

    pub(crate) fn remember_floating(&mut self, window: &Window) {
        let Some(id) = self.windows.ids().get(window).copied() else {
            return;
        };
        let Some(WindowPlacement::Floating { rect }) = self.workspaces.placement(id) else {
            return;
        };
        let (app, parent) = Self::floating_metadata(window);
        if parent.is_some() || crate::window_rules::is_native_dialog(app.as_deref()) {
            return;
        }
        // A drag can finish on another output without changing workspace ownership.
        let Some((output, work)) = self
            .floating_outputs()
            .into_iter()
            .max_by(|(_, a), (_, b)| {
                let area = |work| crate::floating::intersection(rect, work).map_or(0., |r| r.width * r.height);
                area(*a).total_cmp(&area(*b))
            })
            .filter(|(_, work)| crate::floating::intersection(rect, *work).is_some())
        else {
            return;
        };
        let entry = crate::floating::Remembered::new(output, rect, work);
        self.windows.update(id, |w| w.floating_memory = Some(entry.clone()));
        if let Some(app) = app {
            self.floating_memory.save(app, entry);
        }
        if self.nested_backend.is_none()
            && let Some(path) = crate::floating::Memory::path()
            && let Err(error) = self.floating_save_worker.queue(path, &self.floating_memory)
        {
            tracing::warn!(%error,"could not queue floating window placement save");
        }
    }

    pub(crate) fn floating_snap_lines(
        &self,
        window: &Window,
        raw: Rect,
    ) -> (Vec<crate::floating::SnapLine>, Vec<crate::floating::SnapLine>) {
        let id = self.windows.ids().get(window).copied();
        let work = self
            .floating_outputs()
            .into_iter()
            .max_by(|(_, a), (_, b)| {
                let area = |work| crate::floating::intersection(raw, work).map_or(0., |r| r.width * r.height);
                area(*a).total_cmp(&area(*b))
            })
            .filter(|(_, work)| crate::floating::intersection(raw, *work).is_some())
            .map(|(_, work)| work)
            .or_else(|| id.and_then(|id| self.floating_bounds_for_window(id)));
        let Some(work) = work else {
            return Default::default();
        };
        crate::floating::snap_lines(raw, work, &self.floating_obstacles(id, work))
    }

    pub(crate) fn floating_snap_bypassed(&self) -> bool {
        self.seat
            .get_keyboard()
            .is_some_and(|keyboard| keyboard.modifier_state().shift)
    }

    pub(super) fn floating_bounds_for_window(&self, window: WindowId) -> Option<Rect> {
        let output_id = self
            .workspaces
            .workspace_for_window(window)
            .and_then(|workspace| self.output_workspaces.output_for_workspace(workspace));
        output_id
            .and_then(|id| self.outputs_by_id.get(&id))
            .and_then(|output| self.output_bounds_for(output))
            .or_else(|| self.output_bounds())
    }

    pub(crate) fn output_bounds_for(&self, output: &Output) -> Option<Rect> {
        let geometry = self.space.output_geometry(output)?;
        let zone = layer_map_for_output(output).non_exclusive_zone();

        Some(Rect::new(
            (geometry.loc.x + zone.loc.x) as f64,
            (geometry.loc.y + zone.loc.y) as f64,
            zone.size.w.max(0) as f64,
            zone.size.h.max(0) as f64,
        ))
    }

    pub(super) fn full_output_bounds_for(&self, output: &Output) -> Option<Rect> {
        let geometry = self.space.output_geometry(output)?;

        Some(Rect::new(
            geometry.loc.x as f64,
            geometry.loc.y as f64,
            geometry.size.w as f64,
            geometry.size.h as f64,
        ))
    }
}
