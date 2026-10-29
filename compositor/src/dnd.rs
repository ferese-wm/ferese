use std::time::{Duration, Instant};

use ferese_core::{WorkspaceId, WorkspaceLayout};
use smithay::backend::input::InputTime;
use smithay::input::Seat;
use smithay::input::dnd::{DnDGrab, DndGrabHandler, DndTarget, GrabType, Source};
use smithay::input::pointer::Focus;
use smithay::output::Output;
use smithay::reexports::calloop::RegistrationToken;
use smithay::reexports::calloop::timer::{TimeoutAction, Timer};
use smithay::reexports::wayland_server::protocol::wl_data_source::WlDataSource;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{IsAlive, Logical, Point, Rectangle, SERIAL_COUNTER, Serial};
use smithay::wayland::selection::data_device::WaylandDndGrabHandler;

use crate::Ferese;

pub(crate) struct Drag {
    kind: GrabType,
    icon: Option<WlSurface>,
    origin: WlSurface,
    offset: Point<i32, Logical>,
    location: Point<f64, Logical>,
    touch_slot: Option<smithay::backend::input::TouchSlot>,
    focus: Option<(WlSurface, Point<f64, Logical>)>,
    outputs: Vec<Output>,
    capture_protected: bool,
    scroll_timer: Option<RegistrationToken>,
    last_scroll: Instant,
}

impl WaylandDndGrabHandler for Ferese {
    fn dnd_requested<S: Source>(
        &mut self,
        source: S,
        icon: Option<WlSurface>,
        seat: Seat<Self>,
        serial: Serial,
        kind: GrabType,
    ) {
        if seat != self.seat
            || self.drag.is_some()
            || self.session_lock.active()
            || self.input_capture.active()
        {
            source.cancel();
            return;
        }
        let (location, origin, touch_slot) = match kind {
            GrabType::Pointer => {
                let start = seat
                    .get_pointer()
                    .expect("validated pointer drag")
                    .grab_start_data()
                    .expect("validated pointer grab");
                (
                    start.location,
                    start.focus.expect("validated origin").0,
                    None,
                )
            }
            GrabType::Touch => {
                let start = seat
                    .get_touch()
                    .expect("validated touch drag")
                    .grab_start_data()
                    .expect("validated touch grab");
                (
                    start.location,
                    start.focus.expect("validated origin").0,
                    Some(start.slot),
                )
            }
        };
        let capture_protected = self.drag_origin_protected(&origin);
        self.drag = Some(Drag {
            kind,
            icon,
            origin,
            location,
            touch_slot,
            capture_protected,
            offset: (0, 0).into(),
            focus: None,
            outputs: Vec::new(),
            scroll_timer: None,
            last_scroll: Instant::now(),
        });
        self.cursor_redraw_pending = true;
        match kind {
            GrabType::Pointer => {
                let pointer = seat.get_pointer().expect("validated pointer drag");
                let start = pointer.grab_start_data().expect("validated pointer grab");
                pointer.set_grab(
                    self,
                    DnDGrab::new_pointer(&self.display_handle, start, source, seat),
                    serial,
                    Focus::Keep,
                );
            }
            GrabType::Touch => {
                let touch = seat.get_touch().expect("validated touch drag");
                let start = touch.grab_start_data().expect("validated touch grab");
                touch.set_grab(
                    self,
                    DnDGrab::new_touch(&self.display_handle, start, source, seat),
                    serial,
                );
            }
        }
    }
}

impl DndGrabHandler for Ferese {
    fn dropped(
        &mut self,
        target: Option<DndTarget<'_, Self>>,
        validated: bool,
        _seat: Seat<Self>,
        location: Point<f64, Logical>,
    ) {
        self.finish_drag();
        let target = target
            .filter(|_| validated)
            .map(|target| target.into_inner().clone());
        // Smithay invokes this callback under the input grab's mutex.
        // Focus and relayout may query that grab, so defer them.
        let focused = self.focused_window;
        self.loop_handle.insert_idle(move |state| {
            if state.focused_window != focused
                || state.session_lock.active()
                || state.input_capture.active()
                || state.drag.is_some()
            {
                return;
            }
            if target
                .as_ref()
                .is_some_and(|surface| state.focus_drop_target(surface))
            {
                return;
            }
            // Dropping a tab on empty space may create a new window. Select
            // its destination output even when no data offer was accepted.
            state.focus_output_at(location);
            state.restore_keyboard_focus();
        });
    }

    fn cancelled(&mut self, _seat: Seat<Self>, _location: Point<f64, Logical>) {
        self.finish_drag();
    }
}

impl Ferese {
    pub(crate) fn pointer_drag_active(&self) -> bool {
        self.drag
            .as_ref()
            .is_some_and(|drag| drag.kind == GrabType::Pointer)
    }

    pub(crate) fn touch_drag_active(&self) -> bool {
        self.drag
            .as_ref()
            .is_some_and(|drag| drag.kind == GrabType::Touch)
    }

    pub(crate) fn cancel_drag(&mut self) -> bool {
        let Some(kind) = self.drag.as_ref().map(|drag| drag.kind) else {
            return false;
        };
        self.finish_drag();
        match kind {
            GrabType::Pointer => {
                if let Some(pointer) = self.seat.get_pointer() {
                    pointer.unset_grab(self, SERIAL_COUNTER.next_serial(), self.last_pointer_time);
                }
            }
            GrabType::Touch => {
                if let Some(touch) = self.seat.get_touch() {
                    touch.unset_grab(self);
                }
            }
        }
        self.cursor_redraw_pending = true;
        true
    }

    /// Source destruction and null-source origin destruction can occur without
    /// another input event. Retire their grabs in the event-loop epilogue.
    pub(crate) fn poll_drag(&mut self) {
        let Some(drag) = &self.drag else {
            return;
        };
        if self.session_lock.active() || self.input_capture.active() {
            self.cancel_drag();
            return;
        }
        let snapshot = match drag.kind {
            GrabType::Pointer => self
                .seat
                .get_pointer()
                .and_then(|pointer| {
                    pointer.with_grab(|_, grab| {
                        grab.downcast_ref::<DnDGrab<Self, WlDataSource, WlSurface>>()
                            .map(|grab| (grab.source().alive(), grab.location()))
                            .or_else(|| {
                                grab.downcast_ref::<DnDGrab<Self, WlSurface, WlSurface>>()
                                    .map(|grab| (grab.source().alive(), grab.location()))
                            })
                    })
                })
                .flatten(),
            GrabType::Touch => self
                .seat
                .get_touch()
                .and_then(|touch| {
                    touch.with_grab(|_, grab| {
                        grab.downcast_ref::<DnDGrab<Self, WlDataSource, WlSurface>>()
                            .map(|grab| (grab.source().alive(), grab.location()))
                            .or_else(|| {
                                grab.downcast_ref::<DnDGrab<Self, WlSurface, WlSurface>>()
                                    .map(|grab| (grab.source().alive(), grab.location()))
                            })
                    })
                })
                .flatten(),
        };
        let Some((true, location)) = snapshot else {
            self.cancel_drag();
            return;
        };
        let focus = self.surface_under(location);
        let protected =
            self.drag_origin_protected(&self.drag.as_ref().expect("active drag").origin);
        let drag = self.drag.as_mut().expect("active drag");
        drag.capture_protected |= protected;
        let moved = drag.location != location;
        let retarget = drag.focus != focus;
        drag.location = location;
        drag.focus = focus.clone();
        let kind = drag.kind;
        let slot = drag.touch_slot;
        if moved {
            self.cursor_redraw_pending = true;
        }
        // Physical motion already reached the grab. Only synthesize motion when
        // presentation moves a target underneath a stationary drag.
        if retarget && !moved {
            let serial = SERIAL_COUNTER.next_serial();
            let time = InputTime::now();
            match kind {
                GrabType::Pointer => {
                    let pointer = self.seat.get_pointer().expect("active pointer drag");
                    pointer.motion(
                        self,
                        focus,
                        &smithay::input::pointer::MotionEvent {
                            location,
                            serial,
                            time,
                        },
                    );
                    pointer.frame(self);
                }
                GrabType::Touch => {
                    let touch = self.seat.get_touch().expect("active touch drag");
                    touch.motion(
                        self,
                        focus,
                        &smithay::input::touch::MotionEvent {
                            location,
                            slot: slot.expect("initiating touch contact"),
                            time,
                        },
                    );
                    touch.frame(self);
                }
            }
        }
        self.update_drag_outputs();
        self.schedule_drag_scroll();
    }

    fn finish_drag(&mut self) {
        if let Some(drag) = self.drag.take() {
            if let Some(timer) = drag.scroll_timer {
                self.loop_handle.remove(timer);
            }
            if let Some(icon) = drag.icon {
                for output in drag.outputs {
                    smithay::desktop::utils::with_surfaces_surface_tree(&icon, |surface, _| {
                        output.leave(surface)
                    });
                }
            }
            self.cursor_redraw_pending = true;
        }
    }

    fn focus_drop_target(&mut self, surface: &WlSurface) -> bool {
        let mut root = surface.clone();
        while let Some(parent) = smithay::wayland::compositor::get_parent(&root) {
            root = parent;
        }
        if let Some(popup) = self.popups.find_popup(&root)
            && let Ok(parent) = smithay::desktop::find_popup_root_surface(&popup)
        {
            root = parent;
        }
        if let Some(id) = self.windows.id_for_surface(&root)
            && self
                .space
                .outputs()
                .any(|output| self.window_belongs_to_output(id, output))
        {
            return self.activate_managed_window(id);
        }
        false
    }

    fn drag_origin_protected(&self, origin: &WlSurface) -> bool {
        use smithay::reexports::wayland_server::Resource;
        self.windows.ids().keys().any(|window| {
            window
                .toplevel()
                .is_some_and(|top| top.wl_surface().id().same_client_as(&origin.id()))
                && self.capture_protected(window)
        })
    }

    pub(crate) fn drag_capture_protected(&self) -> bool {
        self.drag
            .as_ref()
            .is_some_and(|drag| drag.capture_protected || self.drag_origin_protected(&drag.origin))
    }

    pub(crate) fn is_drag_icon(&self, surface: &WlSurface) -> bool {
        self.drag.as_ref().and_then(|drag| drag.icon.as_ref()) == Some(surface)
    }

    pub(crate) fn drag_icon(&self) -> Option<(&WlSurface, Point<f64, Logical>)> {
        if self.session_lock.active() || self.input_capture.active() {
            return None;
        }
        let drag = self.drag.as_ref()?;
        let icon = drag.icon.as_ref().filter(|icon| icon.alive())?;
        Some((icon, drag.location + drag.offset.to_f64()))
    }

    pub(crate) fn drag_icon_bounds(&self) -> Option<Rectangle<f64, Logical>> {
        let (icon, location) = self.drag_icon()?;
        let mut bounds = smithay::desktop::utils::bbox_from_surface_tree(icon, (0, 0)).to_f64();
        bounds.loc += location;
        Some(bounds)
    }

    pub(crate) fn commit_drag_icon(&mut self, surface: &WlSurface) {
        use smithay::wayland::compositor::{SurfaceAttributes, with_states};
        if let Some(drag) = self.drag.as_mut()
            && drag.icon.as_ref() == Some(surface)
        {
            let delta = with_states(surface, |states| {
                states
                    .cached_state
                    .get::<SurfaceAttributes>()
                    .current()
                    .buffer_delta
                    .take()
                    .unwrap_or_default()
            });
            drag.offset.x = drag.offset.x.saturating_add(delta.x);
            drag.offset.y = drag.offset.y.saturating_add(delta.y);
        }
    }

    fn update_drag_outputs(&mut self) {
        let Some((icon, _)) = self.drag_icon() else {
            return;
        };
        let icon = icon.clone();
        let bounds = self.drag_icon_bounds().expect("active icon");
        let outputs = self
            .space
            .outputs()
            .filter(|output| {
                self.space
                    .output_geometry(output)
                    .is_some_and(|geometry| geometry.to_f64().overlaps(bounds))
            })
            .cloned()
            .collect::<Vec<_>>();
        let drag = self.drag.as_mut().expect("active drag");
        for output in &drag.outputs {
            if !outputs.contains(output) {
                smithay::desktop::utils::with_surfaces_surface_tree(&icon, |surface, _| {
                    output.leave(surface)
                });
            }
        }
        for output in &outputs {
            smithay::desktop::utils::with_surfaces_surface_tree(&icon, |surface, _| {
                output.enter(surface)
            });
        }
        if let Some(output) = self
            .space
            .output_under(drag.location)
            .next()
            .or_else(|| outputs.first())
        {
            crate::handlers::set_surface_tree_output(&icon, output);
        }
        drag.outputs = outputs;
    }

    fn drag_scroll_target(&self) -> Option<(WorkspaceId, f64)> {
        let drag = self.drag.as_ref()?;
        if self.overview.is_active() || self.session_lock.active() || self.input_capture.active() {
            return None;
        }
        let output = self.space.output_under(drag.location).next()?;
        let bounds = self.output_bounds_for(output)?;
        if self.layer_under(drag.location).is_some_and(|(layer, ..)| {
            matches!(
                layer.layer(),
                smithay::wayland::shell::wlr_layer::Layer::Top
                    | smithay::wayland::shell::wlr_layer::Layer::Overlay
            )
        }) {
            return None;
        }
        let direction = if drag.location.x < bounds.x + 32.0 {
            -1.0
        } else if drag.location.x > bounds.x + bounds.width - 32.0 {
            1.0
        } else {
            return None;
        };
        let workspace = self
            .output_id(output)
            .and_then(|id| self.output_workspaces.active_workspace(id))?;
        let state = self.workspaces.workspace(workspace)?;
        if state.fullscreen.is_some() {
            return None;
        }
        let WorkspaceLayout::Scrolling(layout) = &state.layout else {
            return None;
        };
        layout
            .can_scroll_viewport(direction)
            .then_some((workspace, direction))
    }

    fn schedule_drag_scroll(&mut self) {
        if self.drag_scroll_target().is_none() {
            if let Some(timer) = self.drag.as_mut().and_then(|drag| drag.scroll_timer.take()) {
                self.loop_handle.remove(timer);
            }
            return;
        }
        let Some(drag) = self
            .drag
            .as_mut()
            .filter(|drag| drag.scroll_timer.is_none())
        else {
            return;
        };
        drag.last_scroll = Instant::now();
        match self.loop_handle.insert_source(
            Timer::from_duration(Duration::from_millis(16)),
            |_, _, state| {
                if let Some(drag) = state.drag.as_mut() {
                    drag.scroll_timer = None;
                }
                state.scroll_drag(Instant::now());
                TimeoutAction::Drop
            },
        ) {
            Ok(timer) => self.drag.as_mut().expect("active drag").scroll_timer = Some(timer),
            Err(error) => tracing::warn!(%error, "failed to schedule data-drag edge scrolling"),
        }
    }

    fn scroll_drag(&mut self, now: Instant) {
        let Some((workspace, direction)) = self.drag_scroll_target() else {
            return;
        };
        let drag = self.drag.as_mut().expect("active drag");
        let elapsed = now
            .saturating_duration_since(drag.last_scroll)
            .as_secs_f64()
            .min(0.05);
        drag.last_scroll = now;
        if let Some(workspace_state) = self.workspaces.workspace_mut(workspace)
            && let WorkspaceLayout::Scrolling(layout) = &mut workspace_state.layout
            && layout.scroll_viewport(direction * elapsed * 800.0)
        {
            self.relayout_workspace_geometry(workspace);
        }
    }
}

#[cfg(test)]
mod tests;
