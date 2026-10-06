use smithay::desktop::{
    PopupKeyboardGrab, PopupKind, PopupPointerGrab, Window, WindowSurfaceType, find_popup_root_surface,
    get_popup_toplevel_coords,
};
use smithay::input::Seat;
use smithay::input::pointer::{Focus, GrabStartData};
use smithay::reexports::wayland_protocols::xdg::decoration::zv1::server::zxdg_toplevel_decoration_v1::Mode;
use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel;
use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel::State as ToplevelState;
use smithay::reexports::wayland_server::protocol::wl_seat;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::{Client, DataInit, Dispatch, DisplayHandle, Resource, backend::ClientId};
use smithay::utils::{Logical, Point, Rectangle, Serial};
use smithay::wayland::compositor::with_states;
use smithay::wayland::shell::xdg::decoration::XdgDecorationHandler;
use smithay::wayland::shell::xdg::{
    PopupSurface, PositionerState, SurfaceCachedState, ToplevelSurface, XdgShellHandler, XdgShellState,
    XdgShellSurfaceUserData, XdgToplevelSurfaceData,
};

use crate::Ferese;
use crate::grabs::{MoveSurfaceGrab, ResizeEdge, ResizeSurfaceGrab};

impl XdgShellHandler for Ferese {
    fn xdg_shell_state(&mut self) -> &mut XdgShellState {
        &mut self.xdg_shell_state
    }

    fn new_toplevel(&mut self, surface: ToplevelSurface) {
        let window = Window::new_wayland_window(surface);
        // app_id, title and parent arrive after get_toplevel. Keep the surface
        // discoverable, but do not mutate/focus the layout until its initial
        // commit supplies the metadata needed to resolve placement rules.
        self.space.map_element(window, (0, 0), false);
        self.invalidate_window_stacking();
    }

    fn title_changed(&mut self, surface: ToplevelSurface) {
        self.refresh_capture_privacy();
        if self.windows.refresh_metadata(surface.wl_surface()) {
            self.refresh_idle_inhibition();
            self.send_shell_snapshots();
            for output in self.surface_outputs(surface.wl_surface()) {
                self.defer_output_redraw(output);
            }
        }
    }

    fn app_id_changed(&mut self, surface: ToplevelSurface) {
        self.refresh_capture_privacy();
        if self.windows.refresh_metadata(surface.wl_surface()) {
            self.refresh_idle_inhibition();
            self.send_shell_snapshots();
            for output in self.surface_outputs(surface.wl_surface()) {
                self.defer_output_redraw(output);
            }
        }
    }

    fn parent_changed(&mut self, surface: ToplevelSurface) {
        self.refresh_capture_privacy();
        self.refresh_idle_inhibition();
        let Some(parent) = surface.parent() else {
            return;
        };
        let parent = self.windows.id_for_surface(&parent);
        let child = self
            .windows
            .id_for_surface(surface.wl_surface())
            .and_then(|id| self.windows.window(id))
            .or_else(|| {
                self.space.elements().find(|window| {
                    window
                        .toplevel()
                        .is_some_and(|toplevel| toplevel.wl_surface() == surface.wl_surface())
                })
            })
            .cloned();

        if let (Some(parent), Some(child)) = (parent, child) {
            self.make_window_transient(&child, parent);
        }
    }

    fn toplevel_destroyed(&mut self, surface: ToplevelSurface) {
        // Hidden workspace windows are managed but unmapped from Space.
        let Some(window) = self.window_for_surface(surface.wl_surface()) else {
            return;
        };

        self.remove_tiled_window(&window);
    }

    fn new_popup(&mut self, surface: PopupSurface, _positioner: PositionerState) {
        self.unconstrain_popup(&surface);
        if let Err(error) = self.popups.track_popup(PopupKind::Xdg(surface)) {
            tracing::warn!(?error, "failed to track xdg popup");
        }
    }

    fn reposition_request(&mut self, surface: PopupSurface, positioner: PositionerState, token: u32) {
        surface.with_pending_state(|state| {
            state.geometry = positioner.get_geometry();
            state.positioner = positioner;
        });
        self.unconstrain_popup(&surface);
        surface.send_repositioned(token);
    }

    fn move_request(&mut self, surface: ToplevelSurface, seat: wl_seat::WlSeat, serial: Serial) {
        if self.session_lock.active() {
            return;
        }
        let Some(seat) = Seat::from_resource(&seat) else {
            return;
        };
        let Some(start_data) = check_grab(&seat, surface.wl_surface(), serial) else {
            return;
        };
        let Some(pointer) = seat.get_pointer() else {
            return;
        };
        let Some(window) = self
            .space
            .elements()
            .find(|window| {
                window
                    .toplevel()
                    .is_some_and(|toplevel| toplevel.wl_surface() == surface.wl_surface())
            })
            .cloned()
        else {
            return;
        };
        let Some(initial_rect) = self.visual_rect_for_window(&window) else {
            return;
        };
        if !self.is_floating_window(&window) {
            return;
        }
        pointer.set_grab(
            self,
            MoveSurfaceGrab {
                start_data,
                window,
                initial_location: initial_rect.loc,
                initial_size: initial_rect.size,
                finished: false,
                snap_x: Default::default(),
                snap_y: Default::default(),
            },
            serial,
            Focus::Clear,
        );
    }

    fn resize_request(
        &mut self,
        surface: ToplevelSurface,
        seat: wl_seat::WlSeat,
        serial: Serial,
        edges: smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel::ResizeEdge,
    ) {
        if self.session_lock.active() {
            return;
        }
        let Some(seat) = Seat::from_resource(&seat) else {
            return;
        };
        let Some(start_data) = check_grab(&seat, surface.wl_surface(), serial) else {
            return;
        };
        let Some(pointer) = seat.get_pointer() else {
            return;
        };
        let Some(window) = self
            .space
            .elements()
            .find(|window| {
                window
                    .toplevel()
                    .is_some_and(|toplevel| toplevel.wl_surface() == surface.wl_surface())
            })
            .cloned()
        else {
            return;
        };
        let Some(rect) = self.visual_rect_for_window(&window) else {
            return;
        };
        if !self.is_floating_window(&window) {
            return;
        }
        pointer.set_grab(
            self,
            ResizeSurfaceGrab::new(start_data, window, ResizeEdge::from(edges), rect),
            serial,
            Focus::Clear,
        );
    }

    fn maximize_request(&mut self, surface: ToplevelSurface) {
        if let Some(id) = self.windows.id_for_surface(surface.wl_surface()) {
            self.set_window_maximized(id, true);
        } else {
            surface.with_pending_state(|state| state.states.set(ToplevelState::Maximized));
        }
    }

    fn unmaximize_request(&mut self, surface: ToplevelSurface) {
        if let Some(id) = self.windows.id_for_surface(surface.wl_surface()) {
            self.set_window_maximized(id, false);
        } else {
            surface.with_pending_state(|state| state.states.unset(ToplevelState::Maximized));
        }
    }

    fn fullscreen_request(
        &mut self,
        surface: ToplevelSurface,
        _output: Option<smithay::reexports::wayland_server::protocol::wl_output::WlOutput>,
    ) {
        if let Some(window) = self.windows.id_for_surface(surface.wl_surface()) {
            self.set_window_fullscreen(window, true);
        } else {
            surface.with_pending_state(|state| state.states.set(ToplevelState::Fullscreen));
        }
    }

    fn unfullscreen_request(&mut self, surface: ToplevelSurface) {
        if let Some(window) = self.windows.id_for_surface(surface.wl_surface()) {
            self.set_window_fullscreen(window, false);
        } else {
            surface.with_pending_state(|state| state.states.unset(ToplevelState::Fullscreen));
        }
    }

    fn grab(&mut self, surface: PopupSurface, seat: wl_seat::WlSeat, serial: Serial) {
        if self.session_lock.active() || self.input_capture.active() {
            return;
        }
        let Some(seat) = Seat::from_resource(&seat) else {
            return;
        };
        let popup = PopupKind::Xdg(surface);
        let Ok(root) = find_popup_root_surface(&popup) else {
            return;
        };
        let popup_grab = match self.popups.grab_popup(root, popup, &seat, serial) {
            Ok(grab) => grab,
            Err(error) => {
                tracing::debug!(?error, "rejected xdg popup grab");
                return;
            }
        };

        if let Some(pointer) = seat.get_pointer() {
            pointer.set_grab(self, PopupPointerGrab::new(&popup_grab), serial, Focus::Keep);
        }
        if let Some(keyboard) = seat.get_keyboard() {
            keyboard.set_grab(self, PopupKeyboardGrab::new(&popup_grab), serial);
            // Focus the popup immediately, rather than waiting for its first
            // key event. Shell clients use focus to initialize popup effects,
            // and Escape must be delivered to the popup from the outset.
            keyboard.set_focus(self, popup_grab.current_grab(), serial);
        }
    }
}

fn check_grab(seat: &Seat<Ferese>, surface: &WlSurface, serial: Serial) -> Option<GrabStartData<Ferese>> {
    let pointer = seat.get_pointer()?;
    if !pointer.has_grab(serial) {
        return None;
    }
    let start_data = pointer.grab_start_data()?;
    let (focus, _) = start_data.focus.as_ref()?;
    focus.id().same_client_as(&surface.id()).then_some(start_data)
}

impl XdgDecorationHandler for Ferese {
    fn new_decoration(&mut self, toplevel: ToplevelSurface) {
        let mode = Mode::ServerSide;
        toplevel.with_pending_state(|state| state.decoration_mode = Some(mode));

        // Wait for the initial surface commit so app-id and saved floating
        // size can be included in the first configure. If already configured,
        // send unconditionally: the existing toplevel state may have the same
        // mode, but the new decoration object still needs its configure event.
        if initial_configure_sent(&toplevel) {
            toplevel.send_configure();
        }
    }

    fn request_mode(&mut self, toplevel: ToplevelSurface, mode: Mode) {
        set_decoration_mode(&toplevel, mode);
    }

    fn unset_mode(&mut self, toplevel: ToplevelSurface) {
        let mode = Mode::ServerSide;
        set_decoration_mode(&toplevel, mode);
    }
}

fn set_decoration_mode(toplevel: &ToplevelSurface, mode: Mode) {
    toplevel.with_pending_state(|state| state.decoration_mode = Some(mode));
    if initial_configure_sent(toplevel) {
        toplevel.send_pending_configure();
    }
}

pub(crate) fn initial_configure_sent(toplevel: &ToplevelSurface) -> bool {
    with_states(toplevel.wl_surface(), |states| {
        states
            .data_map
            .get::<XdgToplevelSurfaceData>()
            .unwrap()
            .lock()
            .unwrap()
            .initial_configure_sent
    })
}

pub(crate) fn validate_size_constraints(toplevel: &ToplevelSurface) -> bool {
    use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel;
    use smithay::wayland::shell::xdg::SurfaceCachedState;

    let valid = with_states(toplevel.wl_surface(), |states| {
        let mut cached = states.cached_state.get::<SurfaceCachedState>();
        let state = cached.current();
        let valid_axis = |min: i32, max: i32| min >= 0 && max >= 0 && (max == 0 || min <= max);
        valid_axis(state.min_size.w, state.max_size.w) && valid_axis(state.min_size.h, state.max_size.h)
    });
    if !valid {
        toplevel
            .xdg_toplevel()
            .post_error(xdg_toplevel::Error::InvalidSize, "invalid minimum/maximum window size");
    }

    valid
}

pub fn handle_commit(state: &mut Ferese, surface: &WlSurface) {
    if let Some(window) = state.window_for_surface(surface) {
        let configured = with_states(surface, |states| {
            states
                .data_map
                .get::<XdgToplevelSurfaceData>()
                .expect("xdg toplevel state exists")
                .lock()
                .expect("xdg toplevel state is not poisoned")
                .initial_configure_sent
        });
        if !configured {
            window
                .toplevel()
                .expect("mapped window has a toplevel")
                .send_configure();
        }
    }

    state.popups.commit(surface);
    if let Some(PopupKind::Xdg(popup)) = state.popups.find_popup(surface)
        && !popup.is_initial_configure_sent()
    {
        popup.send_configure().expect("initial popup configure is valid");
    }
}

pub fn apply_initial_window_rules(state: &mut Ferese, window: &Window) {
    let Some(toplevel) = window.toplevel() else {
        return;
    };
    let (app_id, title, transient) = with_states(toplevel.wl_surface(), |states| {
        let attributes = states
            .data_map
            .get::<XdgToplevelSurfaceData>()
            .expect("xdg toplevel state exists")
            .lock()
            .expect("xdg toplevel state is not poisoned");

        (
            attributes.app_id.clone(),
            attributes.title.clone(),
            attributes.parent.is_some(),
        )
    });

    let initial = !state.windows.ids().contains_key(window);
    let requested = initial.then(|| {
        toplevel.with_pending_state(|pending| {
            (
                pending.states.contains(ToplevelState::Maximized),
                pending.states.contains(ToplevelState::Fullscreen),
            )
        })
    });
    if initial {
        let parent = toplevel
            .parent()
            .and_then(|parent| state.windows.id_for_surface(&parent));
        state.add_rule_placed_window(window.clone(), app_id.as_deref(), title.as_deref(), parent);
    }
    state.apply_initial_window_rules(window, app_id.as_deref(), title.as_deref(), transient);
    if let Some((maximized, fullscreen)) = requested
        && let Some(id) = state.windows.ids().get(window).copied()
    {
        if maximized {
            state.set_window_maximized(id, true);
        }
        if fullscreen {
            state.set_window_fullscreen(id, true);
        }
    }
}

impl Ferese {
    pub(crate) fn unconstrain_popup(&self, popup: &PopupSurface) {
        let Ok(root) = find_popup_root_surface(&PopupKind::Xdg(popup.clone())) else {
            return;
        };
        let window = self
            .space
            .elements()
            .find(|window| window.toplevel().is_some_and(|toplevel| toplevel.wl_surface() == &root));
        let geometry = (|| {
            if let Some(window) = window {
                let id = *self.windows.ids().get(window)?;
                let output = self
                    .space
                    .outputs()
                    .find(|output| self.window_belongs_to_output(id, output))?;
                let output_geometry = self.space.output_geometry(output)?;
                let presented = self.presented_window_rect(id)?;
                let location = (presented.x.round() as i32, presented.y.round() as i32).into();

                Some((output_geometry, location))
            } else {
                let layer = self.space.layer_for_surface(&root, WindowSurfaceType::TOPLEVEL)?;

                self.space.outputs().find_map(|output| {
                    let output_geometry = self.space.output_geometry(output)?;
                    let layer_geometry = smithay::desktop::layer_map_for_output(output).layer_geometry(&layer)?;

                    Some((output_geometry, output_geometry.loc + layer_geometry.loc))
                })
            }
        })();
        let Some((target, root_location)) = geometry else {
            return;
        };

        let target = popup_constraint_target(
            target,
            root_location,
            get_popup_toplevel_coords(&PopupKind::Xdg(popup.clone())),
        );
        popup.with_pending_state(|state| {
            state.geometry = state.positioner.get_unconstrained_geometry(target);
        });
    }
}

fn popup_constraint_target(
    mut output: Rectangle<i32, Logical>,
    root_location: Point<i32, Logical>,
    parent_offset: Point<i32, Logical>,
) -> Rectangle<i32, Logical> {
    output.loc -= root_location + parent_offset;
    output
}

impl Dispatch<xdg_toplevel::XdgToplevel, XdgShellSurfaceUserData> for Ferese {
    fn request(
        state: &mut Self,
        client: &Client,
        resource: &xdg_toplevel::XdgToplevel,
        request: xdg_toplevel::Request,
        data: &XdgShellSurfaceUserData,
        dhandle: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        let unusable = match &request {
            xdg_toplevel::Request::SetMinSize { width, height } => {
                !size_limits_usable(state, resource, (*width, *height), true)
            }
            xdg_toplevel::Request::SetMaxSize { width, height } => {
                !size_limits_usable(state, resource, (*width, *height), false)
            }
            _ => false,
        };
        if unusable {
            resource.post_error(xdg_toplevel::Error::InvalidSize, "size limits are unusable");
            return;
        }

        <XdgShellState as Dispatch<xdg_toplevel::XdgToplevel, XdgShellSurfaceUserData, Ferese>>::request(
            state, client, resource, request, data, dhandle, data_init,
        );
    }

    fn destroyed(
        state: &mut Self,
        client: ClientId,
        resource: &xdg_toplevel::XdgToplevel,
        data: &XdgShellSurfaceUserData,
    ) {
        <XdgShellState as Dispatch<xdg_toplevel::XdgToplevel, XdgShellSurfaceUserData, Ferese>>::destroyed(
            state, client, resource, data,
        );
    }
}

fn size_limits_usable(
    state: &Ferese,
    toplevel: &xdg_toplevel::XdgToplevel,
    proposed: (i32, i32),
    minimum: bool,
) -> bool {
    if proposed.0 < 0 || proposed.1 < 0 {
        return false;
    }

    let Some(surface) = state.xdg_shell_state.get_toplevel(toplevel) else {
        return true;
    };
    let (min, max) = with_states(surface.wl_surface(), |states| {
        let mut cached = states.cached_state.get::<SurfaceCachedState>();
        let pending = cached.pending();
        (
            (pending.min_size.w, pending.min_size.h),
            (pending.max_size.w, pending.max_size.h),
        )
    });
    let (min, max) = if minimum { (proposed, max) } else { (min, proposed) };
    (max.0 == 0 || min.0 <= max.0) && (max.1 == 0 || min.1 <= max.1)
}

#[cfg(test)]
mod tests {
    use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_positioner;

    use super::*;

    #[test]
    fn popup_uses_presented_parent_coordinates_on_an_offset_monitor() {
        let output = Rectangle::new((1920, -200).into(), (1920, 1080).into());
        let root = Point::from((2200, -100));
        let parent = Point::from((40, 30));
        let target = popup_constraint_target(output, root, parent);
        assert_eq!(target.loc, Point::from((-320, -130)));
        let positioner = PositionerState {
            rect_size: (320, 180).into(),
            anchor_rect: Rectangle::new((1850, 900).into(), (1, 1).into()),
            anchor_edges: xdg_positioner::Anchor::BottomRight,
            gravity: xdg_positioner::Gravity::BottomRight,
            constraint_adjustment: xdg_positioner::ConstraintAdjustment::SlideX
                | xdg_positioner::ConstraintAdjustment::SlideY,
            ..Default::default()
        };
        let popup = positioner.get_unconstrained_geometry(target);
        assert_eq!(popup.loc, Point::from((1280, 770)));
        let global = Rectangle::new(root + parent + popup.loc, popup.size);
        assert_eq!(global.intersection(output), Some(global));
    }
}
