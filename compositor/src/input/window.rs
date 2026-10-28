use super::{
    ButtonEvent, ButtonState, Direction, Ferese, Focus, GrabStartData, Logical, MotionEvent, Point,
    PointerHandle, ResizeEdge, SERIAL_COUNTER, Serial, with_pointer_constraint,
};
use ferese_core::{WindowPlacement, WorkspaceId, WorkspaceLayout};
use ferese_layout::{Axis as LayoutAxis, DropPlacement, Rect, WindowId};
use smithay::desktop::Window;
use smithay::input::pointer::{CursorIcon, CursorImageStatus};
use smithay::output::Output;
use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel;
use smithay::wayland::compositor::with_states;
use smithay::wayland::shell::xdg::SurfaceCachedState;
use std::collections::HashSet;

pub(crate) struct WindowInteraction {
    pub id: WindowId,
    window: Window,
    workspace: WorkspaceId,
    output: Output,
    output_geometry: smithay::utils::Rectangle<i32, Logical>,
    start: Point<f64, Logical>,
    initial: smithay::utils::Rectangle<i32, Logical>,
    placement: WindowPlacement,
    applied_placement: WindowPlacement,
    edges: Option<ResizeEdge>,
    base_layout: WorkspaceLayout,
    applied_layout: WorkspaceLayout,
    revision: u64,
    serial: Serial,
    last_tick: std::time::Instant,
    scroll: f64,
    drop_floating: bool,
    pub location: Point<f64, Logical>,
    pub pressed: HashSet<u32>,
    pub finished: bool,
    pub cancelled: bool,
    started: bool,
    last_location: Point<f64, Logical>,
    was_maximized: bool,
    pub visual: Option<Rect>,
    snap_x: crate::floating::AxisSnap,
    snap_y: crate::floating::AxisSnap,
}

impl WindowInteraction {
    pub(crate) fn toggle_drop_mode(&mut self) {
        if self.edges.is_none() && !self.finished {
            self.drop_floating = !self.drop_floating;
        }
    }

    pub(crate) fn scroll(&mut self, amount: f64) {
        if self.edges.is_none() && amount.is_finite() && !self.finished {
            self.scroll += amount;
        }
    }
}

impl Ferese {
    pub(crate) fn begin_window_interaction(
        &mut self,
        pointer: &PointerHandle<Self>,
        window: Window,
        start: GrabStartData<Self>,
        serial: Serial,
        edges: Option<ResizeEdge>,
    ) -> bool {
        if self.session_lock.active()
            || self.input_capture.active()
            || self.overview.is_active()
            || self.pointer_operation.is_some()
            || self.keyboard_focused_layer().is_some()
            || !self.window_content_ready(&window)
            || pointer
                .with_grab(|_, grab| !grab.is::<smithay::input::pointer::ClickGrab<Self>>())
                .unwrap_or(false)
            || pointer.current_focus().is_some_and(|surface| {
                with_pointer_constraint(&surface, pointer, |constraint| {
                    constraint.is_some_and(|constraint| constraint.is_active())
                })
            })
        {
            return false;
        }
        let Some(id) = self.windows.ids().get(&window).copied() else {
            return false;
        };
        let Some(workspace) = self.workspaces.workspace_for_window(id) else {
            return false;
        };
        let Some(placement) = self.workspaces.placement(id) else {
            return false;
        };
        if !self.window_interaction_allowed(id, workspace) {
            return false;
        }
        let Some(output) = self
            .space
            .outputs()
            .find(|output| self.window_belongs_to_output(id, output))
            .cloned()
        else {
            return false;
        };
        let Some(output_geometry) = self.space.output_geometry(&output) else {
            return false;
        };
        let Some(mut initial) = self.visual_rect_for_window(&window) else {
            return false;
        };
        let was_maximized = self
            .windows
            .record(id)
            .is_some_and(|record| record.maximized);
        if was_maximized {
            self.set_window_maximized(id, false);
            if let Some(geometry) = self.windows.geometry(&id) {
                let restored = geometry.logical;
                let size = (
                    restored.width.round() as i32,
                    restored.height.round() as i32,
                )
                    .into();
                let x = ((start.location.x - initial.loc.x as f64) / initial.size.w.max(1) as f64)
                    .clamp(0.0, 1.0);
                let y = ((start.location.y - initial.loc.y as f64) / initial.size.h.max(1) as f64)
                    .clamp(0.0, 1.0);
                initial = smithay::utils::Rectangle::new(
                    (
                        (start.location.x - x * restored.width).round() as i32,
                        (start.location.y - y * restored.height).round() as i32,
                    )
                        .into(),
                    size,
                );
            }
        }
        let layout = self
            .workspaces
            .workspace(workspace)
            .expect("managed workspace")
            .layout
            .clone();
        self.pointer_operation = Some(WindowInteraction {
            id,
            window,
            workspace,
            output,
            output_geometry,
            initial,
            placement,
            applied_placement: placement,
            edges,
            base_layout: layout.clone(),
            applied_layout: layout,
            revision: self.ipc_events.config_revision,
            serial,
            last_tick: std::time::Instant::now(),
            scroll: 0.0,
            drop_floating: matches!(placement, WindowPlacement::Floating { .. }),
            start: start.location,
            location: start.location,
            last_location: start.location,
            pressed: HashSet::from([start.button]),
            finished: false,
            cancelled: false,
            started: false,
            was_maximized,
            visual: None,
            snap_x: Default::default(),
            snap_y: Default::default(),
        });
        self.cursor_status =
            CursorImageStatus::Named(edges.map_or(CursorIcon::Grabbing, ResizeEdge::cursor));
        pointer.set_grab(
            self,
            crate::grabs::WindowPointerGrab {
                start_data: start,
                finished: false,
            },
            serial,
            Focus::Clear,
        );
        use smithay::reexports::calloop::timer::{TimeoutAction, Timer};
        let interval = std::time::Duration::from_millis(16);
        if let Err(error) =
            self.loop_handle
                .insert_source(Timer::from_duration(interval), move |_, _, state| {
                    if state
                        .pointer_operation
                        .as_ref()
                        .is_none_or(|operation| operation.serial != serial)
                    {
                        return TimeoutAction::Drop;
                    }
                    state.poll_window_interaction();
                    if state.pointer_operation.is_some() {
                        TimeoutAction::ToDuration(interval)
                    } else {
                        TimeoutAction::Drop
                    }
                })
        {
            tracing::warn!(%error, "failed to schedule drag edge scrolling");
        }
        true
    }

    pub(crate) fn cancel_window_interaction(&mut self) {
        if let Some(operation) = self.pointer_operation.as_mut() {
            operation.cancelled = true;
        }
        self.poll_window_interaction();
    }

    pub(crate) fn poll_window_interaction(&mut self) {
        self.update_window_interaction();
        if self.window_grab_focus_pending
            && let Some(pointer) = self.seat.get_pointer()
            && !pointer.is_grabbed()
        {
            self.window_grab_focus_pending = false;
            let location = pointer.current_location();
            pointer.motion(
                self,
                self.surface_under(location),
                &MotionEvent {
                    location,
                    serial: SERIAL_COUNTER.next_serial(),
                    time: self.last_pointer_time,
                },
            );
            pointer.frame(self);
        }
    }

    fn update_window_interaction(&mut self) {
        let Some(mut operation) = self.pointer_operation.take() else {
            return;
        };
        let valid = !self.session_lock.active()
            && !self.input_capture.active()
            && self.keyboard_focused_layer().is_none()
            && self
                .windows
                .record(operation.id)
                .is_some_and(|record| !record.maximized)
            && operation.location.x.is_finite()
            && operation.location.y.is_finite()
            && self
                .windows
                .window(operation.id)
                .is_some_and(|window| self.window_content_ready(window))
            && self.workspaces.workspace_for_window(operation.id) == Some(operation.workspace)
            && self.space.output_geometry(&operation.output) == Some(operation.output_geometry)
            && self
                .output_id(&operation.output)
                .and_then(|id| self.output_workspaces.active_workspace(id))
                == Some(operation.workspace)
            && self.window_interaction_allowed(operation.id, operation.workspace)
            && self
                .workspaces
                .placement(operation.id)
                .is_some_and(|placement| {
                    matches!(
                        (placement, operation.placement),
                        (WindowPlacement::Tiled, WindowPlacement::Tiled)
                            | (
                                WindowPlacement::Floating { .. },
                                WindowPlacement::Floating { .. }
                            )
                    )
                })
            && self.ipc_events.config_revision == operation.revision;
        if !valid {
            operation.cancelled = true;
        }
        if self.overview.is_active() {
            operation.cancelled = true;
        }
        let layout_matches = operation.edges.is_none()
            || operation.placement != WindowPlacement::Tiled
            || self
                .workspaces
                .workspace(operation.workspace)
                .is_some_and(|workspace| same_layout(&workspace.layout, &operation.applied_layout));
        if operation.edges.is_some()
            && operation.placement == WindowPlacement::Tiled
            && !layout_matches
        {
            operation.cancelled = true;
        }
        let placement_matches =
            self.workspaces.placement(operation.id) == Some(operation.applied_placement);
        if operation.edges.is_some() && !placement_matches {
            operation.cancelled = true;
        }
        let delta = operation.location - operation.start;
        let distance = delta.x.hypot(delta.y);
        operation.started |= distance > 0.0 && distance >= self.input_settings.mouse.drag_threshold;
        if operation.cancelled {
            if operation.started
                && self.workspaces.workspace_for_window(operation.id) == Some(operation.workspace)
            {
                match operation.placement {
                    WindowPlacement::Tiled if operation.edges.is_some() && layout_matches => {
                        if let Some(workspace) = self.workspaces.workspace_mut(operation.workspace)
                        {
                            workspace.layout = operation.base_layout.clone();
                        }
                    }
                    WindowPlacement::Floating { rect }
                        if operation.edges.is_some() && placement_matches =>
                    {
                        self.set_floating_window_rect(&operation.window, rect);
                    }
                    _ => {}
                }
            }
            if let Some(pointer) = self.seat.get_pointer() {
                // Flush only a compositor-owned grab. The physical releases are
                // then consumed, preserving button state without sending them to clients.
                if pointer
                    .with_grab(|_, grab| grab.is::<crate::grabs::WindowPointerGrab>())
                    .unwrap_or(false)
                {
                    for button in &operation.pressed {
                        self.send_pointer_button(
                            &pointer,
                            &ButtonEvent {
                                button: *button,
                                state: ButtonState::Released,
                                serial: SERIAL_COUNTER.next_serial(),
                                time: self.last_pointer_time,
                            },
                        );
                        self.consumed_pointer_buttons.insert(*button);
                    }
                    if pointer
                        .with_grab(|_, grab| grab.is::<crate::grabs::WindowPointerGrab>())
                        .unwrap_or(false)
                    {
                        pointer.unset_grab(
                            self,
                            SERIAL_COUNTER.next_serial(),
                            self.last_pointer_time,
                        );
                    }
                }
            }
            self.finish_window_interaction(&operation, true);
            return;
        }
        if operation.started && operation.location != operation.last_location {
            let previous_visual = operation.visual;
            if let Some(edges) = operation.edges {
                self.resize_interaction(&mut operation, edges, delta);
            } else {
                let mut rect = Rect::new(
                    operation.initial.loc.x as f64 + delta.x,
                    operation.initial.loc.y as f64 + delta.y,
                    operation.initial.size.w as f64,
                    operation.initial.size.h as f64,
                );
                if operation.drop_floating {
                    let (xs, ys) = self.floating_snap_lines(&operation.window, rect);
                    if self.floating_snap_bypassed() {
                        operation.snap_x.clear();
                        operation.snap_y.clear();
                    } else {
                        rect.x = operation.snap_x.apply(rect.x, rect.width, &xs);
                        rect.y = operation.snap_y.apply(rect.y, rect.height, &ys);
                    }
                }
                operation.visual = Some(rect);
                self.space.raise_element(&operation.window, false);
            }
            operation.last_location = operation.location;
            let outputs = self
                .space
                .outputs()
                .filter(|output| {
                    *output == &operation.output
                        || self.space.output_geometry(output).is_some_and(|geometry| {
                            let bounds = Rect::new(
                                geometry.loc.x as f64,
                                geometry.loc.y as f64,
                                geometry.size.w as f64,
                                geometry.size.h as f64,
                            );
                            [previous_visual, operation.visual]
                                .into_iter()
                                .flatten()
                                .any(|rect| crate::floating::intersection(rect, bounds).is_some())
                        })
                })
                .cloned()
                .collect::<Vec<_>>();
            for output in outputs {
                self.defer_output_redraw(output);
            }
            self.cursor_redraw_pending = true;
        }
        if operation.started && operation.edges.is_none() && !operation.finished {
            self.scroll_drag_viewport(&mut operation);
        }
        if operation.finished {
            let committed = operation.started
                && (operation.edges.is_some() || self.drop_interaction(&operation));
            self.finish_window_interaction(&operation, !committed);
        } else {
            self.pointer_operation = Some(operation);
        }
    }

    fn scroll_drag_viewport(&mut self, operation: &mut WindowInteraction) {
        let now = std::time::Instant::now();
        let elapsed = now
            .duration_since(operation.last_tick)
            .as_secs_f64()
            .min(0.05);
        operation.last_tick = now;
        let wheel = std::mem::take(&mut operation.scroll);
        let Some(output) = self.space.output_under(operation.location).next().cloned() else {
            return;
        };
        let Some(bounds) = self.output_bounds_for(&output) else {
            return;
        };
        let Some(workspace) = self
            .output_id(&output)
            .and_then(|id| self.output_workspaces.active_workspace(id))
        else {
            return;
        };
        // The pointer can be held at an edge to reach columns outside the viewport.
        let edge = if operation.drop_floating
            || self
                .layer_under(operation.location)
                .is_some_and(|(layer, ..)| {
                    matches!(
                        layer.layer(),
                        smithay::wayland::shell::wlr_layer::Layer::Top
                            | smithay::wayland::shell::wlr_layer::Layer::Overlay
                    )
                }) {
            0.0
        } else if operation.location.x < bounds.x + 32.0 {
            -1.0
        } else if operation.location.x > bounds.x + bounds.width - 32.0 {
            1.0
        } else {
            0.0
        };
        if let Some(workspace_state) = self.workspaces.workspace_mut(workspace)
            && workspace_state.fullscreen.is_none()
            && let WorkspaceLayout::Scrolling(layout) = &mut workspace_state.layout
            && layout.scroll_viewport(wheel + edge * elapsed * 800.0)
        {
            self.relayout_workspace_geometry(workspace);
        }
    }

    fn resize_interaction(
        &mut self,
        operation: &mut WindowInteraction,
        edges: ResizeEdge,
        delta: Point<f64, Logical>,
    ) {
        if operation.placement == WindowPlacement::Tiled {
            let Some(bounds) = self.output_bounds_for(&operation.output) else {
                return;
            };
            let mut layout = operation.base_layout.clone();
            if edges.left() || edges.right() {
                let edge = if edges.left() {
                    Direction::Left
                } else {
                    Direction::Right
                };
                let _ = layout.resize_edge(operation.id, edge, delta.x, bounds);
            }
            if edges.top() || edges.bottom() {
                let edge = if edges.top() {
                    Direction::Up
                } else {
                    Direction::Down
                };
                let _ = layout.resize_edge(operation.id, edge, delta.y, bounds);
            }
            if let Some(workspace) = self.workspaces.workspace_mut(operation.workspace) {
                workspace.layout = layout;
            }
            self.relayout_workspace_geometry(operation.workspace);
            if let Some(workspace) = self.workspaces.workspace(operation.workspace) {
                operation.applied_layout = workspace.layout.clone();
            }
        } else if let Some(surface) = operation.window.toplevel() {
            let initial = operation.initial;
            let mut delta = delta;
            let left = initial.loc.x as f64 + if edges.left() { delta.x } else { 0.0 };
            let top = initial.loc.y as f64 + if edges.top() { delta.y } else { 0.0 };
            let right = initial.loc.x as f64
                + initial.size.w as f64
                + if edges.right() { delta.x } else { 0.0 };
            let bottom = initial.loc.y as f64
                + initial.size.h as f64
                + if edges.bottom() { delta.y } else { 0.0 };
            let raw = Rect::new(left, top, (right - left).max(1.0), (bottom - top).max(1.0));
            let (xs, ys) = self.floating_snap_lines(&operation.window, raw);
            if self.floating_snap_bypassed() {
                operation.snap_x.clear();
                operation.snap_y.clear();
            } else {
                if edges.left() || edges.right() {
                    let moving = if edges.left() { left } else { right };
                    delta.x += operation.snap_x.apply(moving, 0.0, &xs) - moving;
                }
                if edges.top() || edges.bottom() {
                    let moving = if edges.top() { top } else { bottom };
                    delta.y += operation.snap_y.apply(moving, 0.0, &ys) - moving;
                }
            }
            let (min, max) = with_states(surface.wl_surface(), |states| {
                let mut cache = states.cached_state.get::<SurfaceCachedState>();
                let cached = cache.current();
                (cached.min_size, cached.max_size)
            });
            let (min, max) =
                self.effective_size_constraints(&operation.window, (min.w, min.h), (max.w, max.h));
            let size =
                crate::grabs::constrained_size(operation.initial.size, delta, edges, min, max);
            let rect = crate::grabs::resized_rect(operation.initial, size, edges);
            self.set_floating_window_geometry(&operation.window, rect.loc, rect.size);
            operation.applied_placement = self
                .workspaces
                .placement(operation.id)
                .unwrap_or(operation.applied_placement);
            let initial = operation.initial;
            self.windows.update(operation.id, |record| {
                record.resize_anchor = Some((
                    edges.left(),
                    edges.top(),
                    Rect::new(
                        initial.loc.x as f64,
                        initial.loc.y as f64,
                        initial.size.w as f64,
                        initial.size.h as f64,
                    ),
                ))
            });
        }
        if let Some(surface) = operation.window.toplevel() {
            surface.with_pending_state(|state| {
                state.states.set(xdg_toplevel::State::Resizing);
            });
            surface.send_pending_configure();
        }
    }

    fn drop_interaction(&mut self, operation: &WindowInteraction) -> bool {
        if self
            .layer_under(operation.location)
            .is_some_and(|(layer, ..)| {
                matches!(
                    layer.layer(),
                    smithay::wayland::shell::wlr_layer::Layer::Top
                        | smithay::wayland::shell::wlr_layer::Layer::Overlay
                )
            })
        {
            return false;
        }
        let Some(output) = self.space.output_under(operation.location).next().cloned() else {
            return false;
        };
        let Some(destination) = self
            .output_id(&output)
            .and_then(|id| self.output_workspaces.active_workspace(id))
        else {
            return false;
        };
        if self
            .workspaces
            .workspace(destination)
            .is_none_or(|workspace| workspace.fullscreen.is_some())
            && !(destination == operation.workspace
                && operation.drop_floating
                && self.window_interaction_allowed(operation.id, destination))
        {
            return false;
        }
        let target = self
            .space
            .elements()
            .rev()
            .filter_map(|window| {
                let id = self.windows.ids().get(window).copied()?;
                if id == operation.id
                    || self.workspaces.workspace_for_window(id) != Some(destination)
                    || self.workspaces.placement(id) != Some(WindowPlacement::Tiled)
                {
                    return None;
                }
                let rect = self.visual_rect_for_window(window)?;
                rect.to_f64()
                    .contains(operation.location)
                    .then_some((id, rect))
            })
            .next();
        // Apply a drop transactionally, preserving all changes made while dragging.
        let mut next = self.workspaces.clone();
        let floating = matches!(operation.placement, WindowPlacement::Floating { .. });
        if floating != operation.drop_floating
            && next
                .toggle_floating(
                    operation.id,
                    operation
                        .visual
                        .expect("started move has a visual rectangle"),
                    LayoutAxis::Horizontal,
                    0.5,
                )
                .is_err()
        {
            return false;
        }
        if next
            .move_window_to_workspace(operation.id, destination, LayoutAxis::Horizontal, 0.5)
            .is_err()
        {
            return false;
        }
        match next
            .placement(operation.id)
            .expect("validated dragged window")
        {
            WindowPlacement::Tiled => {
                if let Some((target, rect)) = target {
                    let placement = drop_placement(operation.location, rect);
                    if next
                        .workspace_mut(destination)
                        .expect("validated destination")
                        .layout
                        .place_window(operation.id, target, placement)
                        .is_err()
                    {
                        return false;
                    }
                }
            }
            WindowPlacement::Floating { .. } => {
                if let Some(rect) = operation.visual
                    && next.set_floating_rect(operation.id, rect).is_err()
                {
                    return false;
                }
            }
        }
        if next.validate().is_err() {
            return false;
        }
        self.workspaces = next;
        // Start the landing animation at the position actually being displayed.
        if let Some(rect) = operation.visual {
            self.windows.clear_transaction(&operation.id);
            self.windows.update(operation.id, |record| {
                record.world_x = None;
                record.coupled_width = None;
                if let Some(geometry) = record.geometry.as_mut() {
                    geometry.visual = ferese_animation::AnimatedRect::new(rect);
                    geometry.presentation_changed = true;
                }
            });
        }
        self.activate_managed_window(operation.id);
        true
    }

    fn finish_window_interaction(&mut self, operation: &WindowInteraction, cancelled: bool) {
        if let Some(surface) = operation.window.toplevel() {
            surface.with_pending_state(|state| {
                state.states.unset(xdg_toplevel::State::Resizing);
            });
            surface.send_pending_configure();
        }
        if cancelled && operation.was_maximized && self.windows.window(operation.id).is_some() {
            self.set_window_maximized(operation.id, true);
        }
        if !cancelled {
            self.remember_floating(&operation.window);
        }
        self.cursor_status = CursorImageStatus::default_named();
        self.relayout();
        self.cursor_redraw_pending = true;
    }
}

fn drop_placement(
    location: Point<f64, Logical>,
    rect: smithay::utils::Rectangle<i32, Logical>,
) -> DropPlacement {
    let x = (location.x - rect.loc.x as f64) / rect.size.w.max(1) as f64;
    let y = (location.y - rect.loc.y as f64) / rect.size.h.max(1) as f64;
    if (0.25..=0.75).contains(&x) && (0.25..=0.75).contains(&y) {
        return DropPlacement::Tab;
    }
    let edges = [
        (x, DropPlacement::Left),
        (1.0 - x, DropPlacement::Right),
        (y, DropPlacement::Above),
        (1.0 - y, DropPlacement::Below),
    ];
    edges
        .into_iter()
        .min_by(|a, b| a.0.total_cmp(&b.0))
        .expect("four edges")
        .1
}

fn same_layout(a: &WorkspaceLayout, b: &WorkspaceLayout) -> bool {
    match (a, b) {
        (WorkspaceLayout::Scrolling(a), WorkspaceLayout::Scrolling(b)) => {
            a.columns() == b.columns()
        }
        _ => a == b,
    }
}
