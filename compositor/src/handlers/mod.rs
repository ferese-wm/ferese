pub(crate) mod activation;
mod compositor;
mod dispatch;
#[cfg(test)]
mod focus_tests;
pub(crate) mod gamma_control;
pub(crate) mod image_capture;
mod input_method;
mod layer_shell;
pub(crate) mod readback;
pub(crate) mod screencopy;
pub(crate) mod screenshot;
pub(crate) mod screenshot_worker;
pub(crate) mod security_context;
#[cfg(test)]
mod toplevel_icon_tests;
pub(crate) mod window_capture;
mod xdg_shell;

use smithay::desktop::{PopupManager, layer_map_for_output};
use smithay::input::pointer::{CursorImageStatus, PointerHandle};
use smithay::input::tablet::TabletSeatHandler;
use smithay::input::{Seat, SeatHandler, SeatState};
use smithay::reexports::wayland_server::Resource;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::wayland::compositor::{TraversalAction, with_states, with_surface_tree_downward};
use smithay::wayland::fractional_scale::{FractionalScaleHandler, with_fractional_scale};
use smithay::wayland::idle_inhibit::IdleInhibitHandler;
use smithay::wayland::idle_notify::{IdleNotifierHandler, IdleNotifierState};
use smithay::wayland::keyboard_shortcuts_inhibit::{
    KeyboardShortcutsInhibitHandler, KeyboardShortcutsInhibitState, KeyboardShortcutsInhibitor,
    KeyboardShortcutsInhibitorSeat,
};
use smithay::wayland::output::OutputHandler;
use smithay::wayland::pointer_constraints::{
    PointerConstraint, PointerConstraintsHandler, with_pointer_constraint,
};
use smithay::wayland::selection::SelectionHandler;
use smithay::wayland::selection::data_device::{
    DataDeviceHandler, DataDeviceState, WaylandDndGrabHandler, set_data_device_focus,
};
use smithay::wayland::selection::primary_selection::{
    PrimarySelectionHandler, PrimarySelectionState, set_primary_focus,
};
use smithay::wayland::xdg_foreign::{XdgForeignHandler, XdgForeignState};
use smithay::wayland::xdg_toplevel_icon::XdgToplevelIconHandler;

use crate::Ferese;

impl SeatHandler for Ferese {
    type KeyboardFocus = WlSurface;
    type PointerFocus = WlSurface;
    type TouchFocus = WlSurface;

    fn seat_state(&mut self) -> &mut SeatState<Self> {
        &mut self.seat_state
    }

    fn cursor_image(&mut self, _seat: &Seat<Self>, image: CursorImageStatus) {
        if let CursorImageStatus::Named(icon) = &image
            && !self.named_cursors.contains_key(icon)
        {
            self.cursor_loader
                .request(*icon, self.cursor_animation.buffer_scale);
        }
        self.cursor_animation.reset |= match (&self.cursor_status, &image) {
            (CursorImageStatus::Named(previous), CursorImageStatus::Named(next)) => {
                previous != next
            }
            (CursorImageStatus::Hidden, CursorImageStatus::Hidden) => false,
            (CursorImageStatus::Surface(previous), CursorImageStatus::Surface(next)) => {
                previous != next
            }
            _ => true,
        };
        self.cursor_status = image;
        self.update_capture_cursor_privacy();
        // Pointer focus transitions invoke this with the pointer lock held.
        // render_all -> cursor_elements -> current_location would try to
        // acquire that same lock, freezing the entire compositor thread.
        self.cursor_redraw_pending = true;
    }

    fn focus_changed(&mut self, seat: &Seat<Self>, focused: Option<&WlSurface>) {
        self.pending_layer_focus = None;
        // Smithay has already sent enter. last_enter uses a separate mutex;
        // querying current_focus here would reenter the held keyboard lock.
        if let Some(serial) = seat
            .get_keyboard()
            .and_then(|keyboard| keyboard.last_enter())
        {
            self.record_activation_input(serial, focused.cloned());
        }

        if let Some(id) = focused.and_then(|surface| self.windows.id_for_surface(surface)) {
            self.focus_history.record(id);
        }
        if let Some(inhibitor) = self.active_shortcuts_inhibitor.take()
            && inhibitor.is_active()
        {
            inhibitor.inactivate();
        }
        if let Some(inhibitor) =
            focused.and_then(|surface| seat.keyboard_shortcuts_inhibitor_for_surface(surface))
        {
            inhibitor.activate();
            self.active_shortcuts_inhibitor = Some(inhibitor);
        }

        let client = focused.and_then(|surface| self.display_handle.get_client(surface.id()).ok());
        set_data_device_focus(&self.display_handle, seat, client.clone());
        set_primary_focus(&self.display_handle, seat, client);
    }
}

impl OutputHandler for Ferese {}
impl smithay::wayland::text_input::TextInputActivation for Ferese {}

impl FractionalScaleHandler for Ferese {
    fn new_fractional_scale(&mut self, surface: WlSurface) {
        self.update_surface_preferences(&surface);
    }
}

impl IdleNotifierHandler for Ferese {
    fn idle_notifier_state(&mut self) -> &mut IdleNotifierState<Self> {
        &mut self.idle_notifier_state
    }
}

impl IdleInhibitHandler for Ferese {
    fn inhibit(&mut self, surface: WlSurface) {
        let count = self.idle_inhibitors.entry(surface).or_default();
        *count = count.saturating_add(1);
        self.refresh_idle_inhibition();
    }

    fn uninhibit(&mut self, surface: WlSurface) {
        if let Some(count) = self.idle_inhibitors.get_mut(&surface) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                self.idle_inhibitors.remove(&surface);
            }
        }
        self.refresh_idle_inhibition();
    }
}

impl KeyboardShortcutsInhibitHandler for Ferese {
    fn keyboard_shortcuts_inhibit_state(&mut self) -> &mut KeyboardShortcutsInhibitState {
        &mut self.keyboard_shortcuts_inhibit_state
    }

    fn new_inhibitor(&mut self, inhibitor: KeyboardShortcutsInhibitor) {
        let focused = self
            .seat
            .get_keyboard()
            .and_then(|keyboard| keyboard.current_focus());

        if focused.as_ref() == Some(inhibitor.wl_surface()) {
            inhibitor.activate();
            self.active_shortcuts_inhibitor = Some(inhibitor);
        }
    }

    fn inhibitor_destroyed(&mut self, inhibitor: KeyboardShortcutsInhibitor) {
        if self.active_shortcuts_inhibitor.as_ref() == Some(&inhibitor) {
            self.active_shortcuts_inhibitor = None;
        }
    }
}

impl Ferese {
    pub(crate) fn surface_outputs(&self, surface: &WlSurface) -> Vec<smithay::output::Output> {
        use smithay::desktop::{WindowSurfaceType, find_popup_root_surface};
        use smithay::wayland::compositor::get_parent;
        let mut root = surface.clone();
        while let Some(parent) = get_parent(&root) {
            root = parent;
        }

        if let Some(popup) = self.popups.find_popup(&root)
            && let Ok(parent) = find_popup_root_surface(&popup)
        {
            root = parent;
        }

        let window = self.windows.id_for_surface(&root);
        self.space
            .outputs()
            .filter(|output| {
                window.is_some_and(|id| self.window_belongs_to_output(id, output))
                    || layer_map_for_output(output)
                        .layer_for_surface(&root, WindowSurfaceType::ALL)
                        .is_some()
                    || self
                        .session_lock
                        .surfaces
                        .get(output)
                        .is_some_and(|lock| lock.wl_surface() == &root)
            })
            .cloned()
            .collect()
    }

    pub(crate) fn update_surface_preferences(&self, surface: &WlSurface) {
        use smithay::desktop::{WindowSurfaceType, find_popup_root_surface};
        use smithay::wayland::compositor::get_parent;

        let mut root = surface.clone();
        while let Some(parent) = get_parent(&root) {
            root = parent;
        }
        if let Some(popup) = self.popups.find_popup(&root)
            && let Ok(parent) = find_popup_root_surface(&popup)
        {
            root = parent;
        }
        let window = self.windows.id_for_surface(&root);
        let output = self.space.outputs().find(|output| {
            window.is_some_and(|id| self.window_belongs_to_output(id, output))
                || layer_map_for_output(output)
                    .layer_for_surface(&root, WindowSurfaceType::ALL)
                    .is_some()
                || self
                    .session_lock
                    .surfaces
                    .get(output)
                    .is_some_and(|lock| lock.wl_surface() == &root)
        });
        // Hidden workspaces retain their last output preferences until presented again.
        if window.is_some() && output.is_none() {
            return;
        }
        let output = output
            .or_else(|| self.focused_output())
            .or_else(|| self.space.outputs().next());
        with_states(surface, |states| {
            set_surface_preferences(surface, states, output)
        });
    }
}

fn set_surface_preferences(
    surface: &WlSurface,
    states: &smithay::wayland::compositor::SurfaceData,
    output: Option<&smithay::output::Output>,
) {
    let scale = output.map_or(1.0, |output| output.current_scale().fractional_scale());
    let transform = output.map_or(smithay::utils::Transform::Normal, |output| {
        output.current_transform()
    });
    smithay::wayland::compositor::send_surface_state(
        surface,
        states,
        scale.ceil() as i32,
        transform,
    );
    with_fractional_scale(states, |surface_scale| {
        surface_scale.set_preferred_scale(scale)
    });
}

pub(crate) fn set_surface_tree_output(surface: &WlSurface, output: &smithay::output::Output) {
    let update_tree = |root: &WlSurface| {
        with_surface_tree_downward(
            root,
            (),
            |_, _, &()| TraversalAction::DoChildren(()),
            |surface, states, &()| set_surface_preferences(surface, states, Some(output)),
            |_, _, &()| true,
        );
    };
    update_tree(surface);
    for (popup, _) in PopupManager::popups_for_surface(surface) {
        update_tree(popup.wl_surface());
    }
}

impl PointerConstraintsHandler for Ferese {
    fn new_constraint(&mut self, surface: &WlSurface, pointer: &PointerHandle<Self>) {
        if self
            .locked_pointer_hint
            .as_ref()
            .is_some_and(|hint| hint.surface == *surface)
        {
            self.locked_pointer_hint = None;
        }
        self.activate_focused_pointer_constraint(pointer);
    }

    fn cursor_position_hint(
        &mut self,
        surface: &WlSurface,
        pointer: &PointerHandle<Self>,
        location: smithay::utils::Point<f64, smithay::utils::Logical>,
    ) {
        let locked = with_pointer_constraint(surface, pointer, |constraint| {
            constraint.is_some_and(|constraint| {
                constraint.is_active() && matches!(&*constraint, PointerConstraint::Locked(_))
            })
        });
        if !locked || pointer.current_focus().as_ref() != Some(surface) {
            return;
        }

        self.locked_pointer_hint = Some(crate::input::LockedPointerHint {
            surface: surface.clone(),
            local: location,
        });
    }
}

impl SelectionHandler for Ferese {
    type SelectionUserData = ();
}

impl smithay::input::dnd::DndGrabHandler for Ferese {}

impl WaylandDndGrabHandler for Ferese {
    fn dnd_requested<S: smithay::input::dnd::Source>(
        &mut self,
        source: S,
        _icon: Option<WlSurface>,
        seat: Seat<Self>,
        serial: smithay::utils::Serial,
        type_: smithay::input::dnd::GrabType,
    ) {
        use smithay::input::dnd::{DnDGrab, GrabType};
        match type_ {
            GrabType::Pointer => {
                let pointer = seat.get_pointer().expect("validated pointer drag");
                let start = pointer.grab_start_data().expect("validated pointer grab");
                pointer.set_grab(
                    self,
                    DnDGrab::new_pointer(&self.display_handle, start, source, seat),
                    serial,
                    smithay::input::pointer::Focus::Keep,
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
impl TabletSeatHandler for Ferese {
    type ToolFocus = WlSurface;
}

impl DataDeviceHandler for Ferese {
    fn data_device_state(&mut self) -> &mut DataDeviceState {
        &mut self.data_device_state
    }
}

impl PrimarySelectionHandler for Ferese {
    fn primary_selection_state(&mut self) -> &mut PrimarySelectionState {
        &mut self.primary_selection_state
    }
}

impl smithay::wayland::selection::wlr_data_control::DataControlHandler for Ferese {
    fn data_control_state(
        &mut self,
    ) -> &mut smithay::wayland::selection::wlr_data_control::DataControlState {
        &mut self.wlr_data_control_state
    }
}

impl smithay::wayland::selection::ext_data_control::DataControlHandler for Ferese {
    fn data_control_state(
        &mut self,
    ) -> &mut smithay::wayland::selection::ext_data_control::DataControlState {
        &mut self.ext_data_control_state
    }
}

impl XdgForeignHandler for Ferese {
    fn xdg_foreign_state(&mut self) -> &mut XdgForeignState {
        &mut self.xdg_foreign_state
    }
}

impl XdgToplevelIconHandler for Ferese {}

pub(crate) mod workspace_capture;
