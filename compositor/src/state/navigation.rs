use super::*;

impl Ferese {
    pub fn focus_direction(&mut self, direction: Direction) {
        self.focus_direction_with_slide(direction, false);
    }

    pub(super) fn focus_swipe_is_current(&self, swipe: &FocusSwipe) -> bool {
        self.animations_enabled
            && !self.session_lock.active()
            && !self.overview.is_presenting()
            && self.workspaces.active_id() == swipe.workspace
            && self.focused_window == Some(swipe.from)
            && self.workspaces.workspace(swipe.workspace).is_some_and(|workspace| {
                workspace.fullscreen.is_none()
                    && workspace.layout.contains(swipe.from)
                    && workspace.layout.contains(swipe.to)
            })
    }

    pub(crate) fn preview_focus_swipe(
        &mut self,
        direction: Direction,
        gesture_direction: SwipeDirection,
        progress: f64,
    ) {
        if self.focus_swipe().is_none() {
            if self.swipe.preview_started() || !self.animations_enabled || self.overview.is_presenting() {
                return;
            }
            let Some(from) = self.focused_window else { return };
            let Some(bounds) = self.output_bounds() else { return };
            let workspace = self.workspaces.active();
            if workspace.fullscreen.is_some() || !matches!(workspace.layout, WorkspaceLayout::Scrolling(_)) {
                return;
            }
            let Ok(neighbor) = workspace.layout.directional_neighbor(from, direction, bounds) else {
                return;
            };
            let to = neighbor.unwrap_or(from);
            let workspace_id = workspace.id;
            let start = self
                .viewports
                .get(&workspace_id)
                .map(|viewport| viewport.motion().current)
                .or_else(|| workspace.layout.viewport_x())
                .unwrap_or(0.0);
            let mut candidate = workspace.layout.clone();
            if to != from
                && let WorkspaceLayout::Scrolling(layout) = &mut candidate
                && layout.slide_focus_from(from, to).is_err()
            {
                return;
            }
            if candidate
                .resolve_geometry_with_constraints(bounds, self.gap_config, &self.window_constraints(), Some(to))
                .is_err()
            {
                return;
            }
            let swipe = FocusSwipe {
                workspace: workspace_id,
                from,
                to,
                direction,
                gesture_direction,
                start,
                destination: if to == from {
                    start
                } else {
                    candidate.viewport_x().unwrap_or(start)
                },
                progress,
                dependencies: self.presentation_dependencies.save_viewport(workspace_id),
                layout: match candidate {
                    WorkspaceLayout::Scrolling(layout) => Some(layout),
                    _ => None,
                },
            };
            self.viewports
                .entry(workspace_id)
                .or_insert_with(|| ViewportPresentation::new(start))
                .begin_gesture(swipe);
            self.rebuild_presentation_dependencies();
            self.swipe.mark_preview_started();
        }
        if let Some(workspace) = self.focus_swipe().map(|swipe| swipe.workspace) {
            self.viewports.get_mut(&workspace).unwrap().update_gesture(progress);
        }

        if let Some(output) = self
            .focus_swipe()
            .and_then(|swipe| self.output_workspaces.output_for_workspace(swipe.workspace))
            .and_then(|id| self.outputs_by_id.get(&id))
            .cloned()
        {
            self.defer_output_redraw(output);
        }
    }

    pub(crate) fn finish_focus_swipe(&mut self, direction: Option<SwipeDirection>) -> bool {
        let Some(swipe) = self.focus_swipe() else {
            return false;
        };
        let current = self.focus_swipe_is_current(swipe);
        let blocked = self
            .presentation_dependencies
            .viewport_blocked(swipe.workspace, &self.windows);
        // Release from the last input position even if no frame rendered that update.
        let velocity = (current && !blocked).then(|| {
            swipe.release_velocity(self.swipe.release_velocity, self.swipe.unbounded_release_velocity)
                / self.animation_speed
        });
        let workspace = swipe.workspace;
        let swipe = self
            .viewports
            .get_mut(&workspace)
            .unwrap()
            .end_gesture(velocity)
            .unwrap();
        self.presentation_dependencies
            .restore_viewport(workspace, swipe.dependencies.clone());
        let neighbor = self.output_bounds().and_then(|bounds| {
            self.workspaces
                .active()
                .layout
                .directional_neighbor(swipe.from, swipe.direction, bounds)
                .ok()
                .flatten()
        });
        self.last_animation_tick = Instant::now();
        if current && direction == Some(swipe.gesture_direction) && neighbor == Some(swipe.to) {
            self.focus_direction_from_swipe(swipe.direction);
        } else {
            self.relayout();
        }
        true
    }

    pub(crate) fn focus_direction_from_swipe(&mut self, direction: Direction) {
        self.focus_direction_with_slide(direction, true);
    }

    pub(super) fn focus_direction_with_slide(&mut self, direction: Direction, slide: bool) {
        if self.input_capture.captures(1) {
            return;
        }
        if self.focus_overview_direction(direction) {
            return;
        }

        let Some(current) = self.focused_window else {
            return;
        };
        let Some(bounds) = self.output_bounds() else {
            return;
        };
        let Ok(Some(next)) = self
            .workspaces
            .active()
            .layout
            .directional_neighbor(current, direction, bounds)
        else {
            return;
        };

        if let Err(error) = self.workspaces.focus_window(next) {
            tracing::error!(%error, ?next, "failed to update workspace focus");
            return;
        }
        if slide
            && let WorkspaceLayout::Scrolling(layout) = &mut self.workspaces.active_mut().layout
            && let Err(error) = layout.slide_focus_from(current, next)
        {
            tracing::warn!(%error, ?current, ?next, "failed to slide swipe focus");
        }
        let Some(window) = self.windows.window(next).cloned() else {
            return;
        };
        let Some(surface) = window.toplevel().map(|toplevel| toplevel.wl_surface().clone()) else {
            return;
        };

        self.focused_window = Some(next);
        self.raise_window(&window, true);
        self.seat.get_keyboard().expect("seat has a keyboard").set_focus(
            self,
            Some(surface),
            smithay::utils::SERIAL_COUNTER.next_serial(),
        );

        for window in self.space.elements() {
            if let Some(toplevel) = window.toplevel() {
                toplevel.send_pending_configure();
            }
        }

        self.relayout();
    }

    pub fn move_direction(&mut self, direction: Direction) {
        let Some(current) = self.focused_window else {
            return;
        };
        if matches!(
            self.workspaces.placement(current),
            Some(WindowPlacement::Floating { .. })
        ) {
            self.adjust_floating_direction(current, direction, false);
            return;
        }
        let Some(bounds) = self.output_bounds() else {
            return;
        };

        match self
            .workspaces
            .active_mut()
            .layout
            .move_window(current, direction, bounds)
        {
            Ok(true) => self.relayout(),
            Ok(false) => {}
            Err(error) => tracing::error!(%error, ?current, "failed to move tiled window"),
        }
    }

    pub fn resize_direction(&mut self, direction: Direction) {
        const RESIZE_STEP: f64 = 0.05;

        let Some(current) = self.focused_window else {
            return;
        };

        if matches!(
            self.workspaces.placement(current),
            Some(WindowPlacement::Floating { .. })
        ) {
            self.adjust_floating_direction(current, direction, true);
            return;
        }

        match self
            .workspaces
            .active_mut()
            .layout
            .resize_window(current, direction, RESIZE_STEP)
        {
            Ok(true) => self.relayout(),
            Ok(false) => {}
            Err(error) => tracing::error!(%error, ?current, "failed to resize tiled window"),
        }
    }

    pub fn toggle_layout_mode(&mut self) {
        let Some(bounds) = self.output_bounds() else {
            return;
        };
        let mode = match self.workspaces.active().layout.mode() {
            LayoutMode::Scrolling => LayoutMode::Tree,
            LayoutMode::Tree => LayoutMode::Scrolling,
        };

        match self.workspaces.set_active_layout_mode(mode, bounds) {
            Ok(true) => self.relayout(),
            Ok(false) => {}
            Err(error) => tracing::error!(%error, ?mode, "failed to change layout mode"),
        }
    }

    pub fn consume_focused_window(&mut self) {
        let Some(window) = self.focused_window else {
            return;
        };
        let Some(bounds) = self.output_bounds() else {
            return;
        };
        if self.workspaces.active().layout.mode() != LayoutMode::Scrolling {
            return;
        }
        let target = self
            .workspaces
            .active()
            .layout
            .directional_neighbor(window, Direction::Left, bounds)
            .ok()
            .flatten()
            .or_else(|| {
                self.workspaces
                    .active()
                    .layout
                    .directional_neighbor(window, Direction::Right, bounds)
                    .ok()
                    .flatten()
            });
        let Some(target) = target else {
            return;
        };

        if let Err(error) = self.workspaces.stack_window(window, target) {
            tracing::error!(%error, ?window, ?target, "failed to consume window into column");
            return;
        }
        self.relayout();
    }

    pub fn expel_focused_window(&mut self) {
        let Some(window) = self.focused_window else {
            return;
        };

        match self.workspaces.extract_window(window) {
            Ok(true) => self.relayout(),
            Ok(false) => {}
            Err(error) => tracing::error!(%error, ?window, "failed to expel window from column"),
        }
    }

    pub fn cycle_focused_column_width(&mut self) {
        let Some(window) = self.focused_window else {
            return;
        };

        match self.workspaces.cycle_column_width(window, &self.column_width_presets) {
            Ok(true) => {
                self.windows.update(window, |w| w.column_width_pending = true);
                self.relayout();
            }
            Ok(false) => {}
            Err(error) => tracing::error!(%error, ?window, "failed to cycle column width"),
        }
    }

    pub fn center_focused_column(&mut self) {
        let Some(window) = self.focused_window else {
            return;
        };
        let Some(bounds) = self.output_bounds() else {
            return;
        };
        let constraints = self.window_constraints();

        match self
            .workspaces
            .center_window(window, bounds, self.gap_config, &constraints)
        {
            Ok(true) => self.relayout(),
            Ok(false) => {}
            Err(error) => tracing::error!(%error, ?window, "failed to center column"),
        }
    }

    pub(crate) fn activate_managed_window(&mut self, id: WindowId) -> bool {
        let Some(window) = self.windows.window(id).cloned() else {
            return false;
        };
        if !activate_window_workspace(&mut self.workspaces, &mut self.output_workspaces, id) {
            return false;
        }
        let workspace = self.workspaces.active_id();

        if let Some(output) = self.output_workspaces.output_for_workspace(workspace) {
            self.workspace_slides.remove(&output);
        }
        self.focused_window = Some(id);
        self.raise_window(&window, true);
        self.relayout();
        self.restore_keyboard_focus();
        true
    }

    pub(crate) fn activate_managed_workspace(&mut self, workspace: WorkspaceId) -> bool {
        self.activate_managed_workspace_internal(workspace, None)
    }

    pub(crate) fn activate_managed_workspace_from_swipe(
        &mut self,
        workspace: WorkspaceId,
        direction: SwipeDirection,
    ) -> bool {
        self.activate_managed_workspace_internal(workspace, Some(direction))
    }

    pub(super) fn activate_managed_workspace_internal(
        &mut self,
        workspace: WorkspaceId,
        slide_direction: Option<SwipeDirection>,
    ) -> bool {
        if self.workspaces.workspace(workspace).is_none() {
            return false;
        }

        let output = self
            .output_workspaces
            .focused_output()
            .or_else(|| self.output_workspaces.output_for_workspace(workspace));
        let Some(output) = output else {
            return false;
        };
        let previous = self.output_workspaces.active_workspace(output);
        let owner = match self.output_workspaces.switch_workspace(output, workspace) {
            Ok(ferese_core::WorkspaceSwitch::Activated(output))
            | Ok(ferese_core::WorkspaceSwitch::FocusedExisting(output)) => output,
            Err(_) => return false,
        };

        self.update_workspace_slide(output, owner, previous, workspace, slide_direction);
        self.activate_output_workspace(owner, workspace);
        self.relayout();
        self.restore_keyboard_focus();
        true
    }

    pub fn switch_workspace(&mut self, index: u32) {
        self.switch_workspace_internal(index, None, false);
    }

    pub(crate) fn switch_workspace_from_binding(&mut self, index: u32) {
        self.switch_workspace_internal(index, None, self.workspace_auto_back_and_forth);
    }

    pub(crate) fn workspace_back_and_forth(&mut self) {
        let Some(output) = self.output_workspaces.focused_output() else {
            return;
        };
        if let Some(workspace) = self.output_workspaces.previous_workspace(output) {
            self.activate_managed_workspace(workspace);
        }
    }

    pub(super) fn switch_workspace_internal(
        &mut self,
        index: u32,
        slide_direction: Option<SwipeDirection>,
        auto_back_and_forth: bool,
    ) {
        let Some(output) = self.output_workspaces.focused_output() else {
            return;
        };
        let Some(workspace) = self.output_workspaces.workspace_at(&self.workspaces, output, index) else {
            return;
        };

        let previous = self.output_workspaces.active_workspace(output);
        let owner = match self
            .output_workspaces
            .select_workspace(output, workspace, auto_back_and_forth)
        {
            Ok(ferese_core::WorkspaceSwitch::Activated(output))
            | Ok(ferese_core::WorkspaceSwitch::FocusedExisting(output)) => output,
            Err(error) => {
                tracing::error!(%error, index, "failed to assign workspace to output");
                return;
            }
        };
        let workspace = self
            .output_workspaces
            .active_workspace(owner)
            .expect("workspace owner is connected");
        self.update_workspace_slide(output, owner, previous, workspace, slide_direction);
        self.activate_output_workspace(owner, workspace);

        self.relayout();
        self.restore_keyboard_focus();
    }

    pub(crate) fn preview_workspace_swipe(&mut self, next: bool, direction: SwipeDirection, progress: f64) {
        if !self.animations_enabled || self.overview.is_presenting() {
            return;
        }
        if let Some((output, (from, to, _))) = self
            .workspace_slides
            .iter()
            .find_map(|(output, slide)| slide.gesture.map(|gesture| (*output, gesture)))
        {
            if !workspace_swipe_is_current(&self.output_workspaces, output, from, to) {
                self.finish_workspace_swipe(None);
                return;
            }
            self.workspace_slides
                .get_mut(&output)
                .expect("gesture output exists")
                .held_progress = Some(progress);
            self.refresh_workspace_slide_offsets();

            if let Some(output) = self.outputs_by_id.get(&output).cloned() {
                self.defer_output_redraw(output);
            }
            return;
        }
        if self.swipe.preview_started() {
            return;
        }
        let Some(output) = self.output_workspaces.focused_output() else {
            return;
        };
        let Some(from) = self.output_workspaces.active_workspace(output) else {
            return;
        };
        let Some(to) = self.relative_workspace_target(next) else {
            return;
        };
        let previous = self.workspace_slides.remove(&output);
        let mut slide = WorkspaceSlide::new(previous, from, to, direction);
        slide.spring = SpringConfig {
            position_tolerance: 0.00001,
            velocity_tolerance: 0.00001,
            ..self.viewport_spring_config
        };
        slide.held_progress = Some(progress);
        slide.gesture = Some((from, to, direction));
        self.workspace_slides.insert(output, slide);
        self.swipe.mark_preview_started();

        self.relayout();
    }

    pub(crate) fn finish_workspace_swipe(&mut self, direction: Option<SwipeDirection>) -> bool {
        let Some((output, (from, to, expected))) = self
            .workspace_slides
            .iter()
            .find_map(|(output, slide)| slide.gesture.map(|gesture| (*output, gesture)))
        else {
            return false;
        };
        let committed = direction == Some(expected)
            && workspace_swipe_is_current(&self.output_workspaces, output, from, to)
            && self.activate_managed_workspace_from_swipe(to, expected);
        if !committed {
            if let Some(slide) = self.workspace_slides.get_mut(&output) {
                slide.release_with_velocity(false, self.swipe.release_velocity / self.animation_speed);
            }
            self.last_animation_tick = Instant::now();
            self.relayout();
        }
        true
    }

    pub(crate) fn workspace_slide_workspaces(&self, output: OutputId) -> impl Iterator<Item = WorkspaceId> + '_ {
        self.workspace_slides
            .get(&output)
            .into_iter()
            .flat_map(|slide| slide.items.iter().map(|item| item.workspace))
    }

    pub(crate) fn update_workspace_slide(
        &mut self,
        requested_output: OutputId,
        owner: OutputId,
        previous: Option<WorkspaceId>,
        workspace: WorkspaceId,
        slide_direction: Option<SwipeDirection>,
    ) {
        if owner != requested_output {
            return;
        }
        let mut previous_slide = self.workspace_slides.remove(&owner);
        if previous == Some(workspace) {
            if let Some(slide) = previous_slide {
                self.workspace_slides.insert(owner, slide);
            }
            return;
        }
        if owner == requested_output
            && let Some(slide) = previous_slide.as_mut()
            && slide
                .gesture
                .is_some_and(|(_, to, direction)| to == workspace && Some(direction) == slide_direction)
        {
            slide.release_with_velocity(true, self.swipe.release_velocity / self.animation_speed);
            self.last_animation_tick = Instant::now();
            self.workspace_slides.insert(owner, previous_slide.unwrap());
            return;
        }
        let slide_direction = slide_direction.or_else(|| {
            let from = previous?;
            let ids = self
                .output_workspaces
                .ordered_workspace_ids(&self.workspaces, owner)
                .collect::<Vec<_>>();
            let from = ids.iter().position(|id| *id == from)?;
            let to = ids.iter().position(|id| *id == workspace)?;
            Some(if self.overview.is_active() {
                if to > from {
                    SwipeDirection::Left
                } else {
                    SwipeDirection::Right
                }
            } else if to > from {
                SwipeDirection::Up
            } else {
                SwipeDirection::Down
            })
        });
        if owner == requested_output
            && let (Some(from), Some(direction)) = (previous, slide_direction)
            && from != workspace
            && self.animations_enabled
            && (!self.overview.is_presenting() || self.overview.is_active())
        {
            self.last_animation_tick = Instant::now();
            let mut slide = WorkspaceSlide::new(previous_slide, from, workspace, direction);
            slide.speed = if self.overview.is_active() {
                crate::overview::OVERVIEW_MOTION_SPEED
            } else {
                1.0
            };
            slide.spring = SpringConfig {
                position_tolerance: 0.00001,
                velocity_tolerance: 0.00001,
                ..self.viewport_spring_config
            };
            self.workspace_slides.insert(owner, slide);
        }
    }

    pub fn move_focused_to_workspace(&mut self, index: u32) {
        let Some(window) = self.focused_window else {
            return;
        };
        let Some(output) = self.output_workspaces.focused_output() else {
            return;
        };
        let Some(destination) = self.output_workspaces.workspace_at(&self.workspaces, output, index) else {
            return;
        };
        let axis = self
            .output_bounds()
            .and_then(|bounds| {
                let target = self.workspaces.workspace(destination)?;
                target.layout.automatic_axis(target.last_focused, bounds).ok()
            })
            .unwrap_or(Axis::Horizontal);

        if let Err(error) = self.workspaces.move_window_to_workspace(window, destination, axis, 0.5) {
            tracing::error!(%error, ?window, index, "failed to move window to workspace");
            return;
        }

        self.focused_window = self.workspaces.active().last_focused;
        self.relayout();
        self.restore_keyboard_focus();
    }

    pub(crate) fn restore_keyboard_focus(&mut self) {
        if self.input_capture.captures(1) {
            return;
        }
        if self.session_lock.active() {
            self.focus_lock_surface();
            return;
        }
        let surface = self
            .focused_window
            .and_then(|focused| self.windows.window(focused))
            .and_then(|window| window.toplevel())
            .map(|toplevel| toplevel.wl_surface().clone());

        self.seat.get_keyboard().expect("seat has a keyboard").set_focus(
            self,
            surface,
            smithay::utils::SERIAL_COUNTER.next_serial(),
        );
    }
}
