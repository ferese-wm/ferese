use ferese_layout::Direction;
use smithay::backend::input::{
    AbsolutePositionEvent, Axis, AxisSource, ButtonState, Event, GestureBeginEvent, GestureEndEvent,
    GestureSwipeUpdateEvent as _, InputBackend, InputEvent, KeyState, KeyboardKeyEvent, PointerAxisEvent,
    PointerButtonEvent, PointerMotionEvent, Switch, SwitchState, SwitchToggleEvent, TouchEvent,
};
use smithay::input::keyboard::{FilterResult, keysyms};
use smithay::input::pointer::{
    AxisFrame, ButtonEvent, Focus, GestureSwipeBeginEvent, GestureSwipeEndEvent, GestureSwipeUpdateEvent,
    GrabStartData, MotionEvent, PointerHandle, RelativeMotionEvent,
};
use smithay::input::touch::{DownEvent, MotionEvent as TouchMotionEvent, UpEvent};
use smithay::reexports::wayland_server::Resource;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{Logical, Point, SERIAL_COUNTER, Serial};
use smithay::wayland::keyboard_shortcuts_inhibit::KeyboardShortcutsInhibitorSeat;
use smithay::wayland::pointer_constraints::{PointerConstraint, with_pointer_constraint};

use crate::Ferese;
use crate::config::BindingAction;
use crate::gestures::SwipeDirection;
use crate::grabs::{MoveSurfaceGrab, ResizeEdge, ResizeSurfaceGrab};
use crate::process::spawn_client;

const BTN_LEFT: u32 = 0x110;
const BTN_RIGHT: u32 = 0x111;

pub(crate) struct LockedPointerHint {
    pub(crate) surface: WlSurface,
    pub(crate) local: Point<f64, Logical>,
}

impl Ferese {
    pub(crate) fn pointer_hint_position(
        &self,
        surface: &WlSurface,
        hint: Point<f64, Logical>,
    ) -> Option<Point<f64, Logical>> {
        for window in self.space.elements() {
            let root = window.toplevel()?.wl_surface();
            let geometry = window.geometry();
            let Some(local) = surface_tree_position(root, surface, geometry.loc) else {
                continue;
            };
            let id = self.windows.ids().get(window)?;
            let presented = self.presented_window_rect(*id)?;
            let (scale_x, scale_y) = self.visual_scale_for_window(window)?;
            let point = local.to_f64() + hint - geometry.loc.to_f64();
            return Some(Point::from((
                presented.x + point.x * scale_x,
                presented.y + point.y * scale_y,
            )));
        }
        for output in self.space.outputs() {
            let output_geometry = self.space.output_geometry(output)?;
            let layers = smithay::desktop::layer_map_for_output(output);
            for layer in layers.layers() {
                let Some(local) = surface_tree_position(layer.wl_surface(), surface, (0, 0).into()) else {
                    continue;
                };
                let geometry = layers.layer_geometry(layer)?;
                return Some((output_geometry.loc + geometry.loc + local).to_f64() + hint);
            }
        }
        None
    }

    pub(crate) fn apply_unlocked_pointer_hint(&mut self) {
        let Some(hint) = self.locked_pointer_hint.take() else {
            return;
        };
        if !hint.surface.is_alive() || self.session_lock.active() || self.input_capture.active() {
            return;
        }
        let Some(pointer) = self.seat.get_pointer() else {
            return;
        };
        let locked = with_pointer_constraint(&hint.surface, &pointer, |constraint| {
            constraint.is_some_and(|constraint| {
                constraint.is_active() && matches!(&*constraint, PointerConstraint::Locked(_))
            })
        });
        if locked {
            self.locked_pointer_hint = Some(hint);
            return;
        }
        if pointer.current_focus().is_some_and(|surface| {
            with_pointer_constraint(&surface, &pointer, |constraint| {
                constraint.is_some_and(|constraint| constraint.is_active())
            })
        }) {
            return;
        }
        let Some(global) = self.pointer_hint_position(&hint.surface, hint.local) else {
            return;
        };
        let location = self.clamp_pointer_position(global);
        let focus = self.surface_under(location);
        pointer.motion(
            self,
            focus,
            &MotionEvent {
                location,
                serial: SERIAL_COUNTER.next_serial(),
                time: self.last_pointer_time,
            },
        );
        pointer.frame(self);
        self.cursor_redraw_pending = true;
    }

    pub fn process_input_event<I: InputBackend>(&mut self, event: InputEvent<I>) {
        let seat = self.seat.clone();
        self.idle_notifier_state.notify_activity(&seat);
        if !matches!(
            &event,
            InputEvent::DeviceAdded { .. } | InputEvent::DeviceRemoved { .. } | InputEvent::SwitchToggle { .. }
        ) {
            self.lock_input_activity();
        }

        if self.session_lock.active() {
            self.prepare_lock_input();
        }

        if self.input_capture.active()
            && matches!(
                &event,
                InputEvent::GestureSwipeBegin { .. }
                    | InputEvent::GestureSwipeUpdate { .. }
                    | InputEvent::GestureSwipeEnd { .. }
                    | InputEvent::GesturePinchBegin { .. }
                    | InputEvent::GesturePinchUpdate { .. }
                    | InputEvent::GesturePinchEnd { .. }
                    | InputEvent::GestureHoldBegin { .. }
                    | InputEvent::GestureHoldEnd { .. }
                    | InputEvent::TouchDown { .. }
                    | InputEvent::TouchMotion { .. }
                    | InputEvent::TouchUp { .. }
                    | InputEvent::TouchCancel { .. }
                    | InputEvent::TouchFrame { .. }
            )
        {
            return;
        }

        match event {
            InputEvent::GestureSwipeBegin { event } => {
                let pointer = seat.get_pointer().expect("seat has a pointer");
                self.finish_workspace_swipe(None);
                self.finish_focus_swipe(None);
                self.swipe.begin(
                    event.fingers(),
                    self.swipe_navigation_blocked()
                        || !self
                            .bindings
                            .iter()
                            .any(|binding| binding.swipe_fingers(event.fingers())),
                    self.input_settings.touchpad.swipe_threshold,
                );
                self.swipe.start_time(std::time::Duration::from_micros(event.time()));
                if self.swipe.active() {
                    self.focus_output_at(pointer.current_location());
                } else {
                    pointer.gesture_swipe_begin(
                        self,
                        &GestureSwipeBeginEvent {
                            serial: SERIAL_COUNTER.next_serial(),
                            time: event.time_msec(),
                            fingers: event.fingers(),
                        },
                    );
                }
            }
            InputEvent::GestureSwipeUpdate { event } => {
                if self.swipe.active() {
                    self.swipe.update_at(
                        event.delta_x(),
                        event.delta_y(),
                        std::time::Duration::from_micros(event.time()),
                    );
                    if self.swipe_navigation_blocked() {
                        self.finish_workspace_swipe(None);
                        self.finish_focus_swipe(None);
                    } else if let Some((direction, progress)) = self.swipe.preview()
                        && let Some(action) = self
                            .bindings
                            .iter()
                            .find(|binding| binding.matches_swipe(self.swipe.fingers(), direction))
                            .map(|binding| binding.action.clone())
                    {
                        match action {
                            BindingAction::SwitchRelativeWorkspace(next) => {
                                self.swipe.mark_momentum_navigation();
                                self.preview_workspace_swipe(next, direction, progress);
                            }
                            BindingAction::Focus(focus @ (Direction::Left | Direction::Right)) => {
                                self.swipe.mark_momentum_navigation();
                                let raw = self.swipe.unbounded_preview().map_or(progress, |(_, value)| value);
                                self.preview_focus_swipe(focus, direction, raw);
                            }
                            _ => {}
                        }
                    }
                } else {
                    seat.get_pointer().expect("seat has a pointer").gesture_swipe_update(
                        self,
                        &GestureSwipeUpdateEvent {
                            time: event.time_msec(),
                            delta: event.delta(),
                        },
                    );
                }
            }
            InputEvent::GestureSwipeEnd { event } => {
                if self.swipe.active() {
                    let fingers = self.swipe.fingers();
                    let cancelled = event.cancelled() || self.swipe_navigation_blocked();
                    let direction = self
                        .swipe
                        .finish_at(cancelled, std::time::Duration::from_micros(event.time()));
                    let workspace_handled = self.finish_workspace_swipe(direction);
                    let focus_handled = self.finish_focus_swipe(direction);
                    let handled = workspace_handled || focus_handled || self.swipe.preview_started();
                    if !handled
                        && let Some(direction) = direction
                        && let Some(action) = self
                            .bindings
                            .iter()
                            .find(|binding| binding.matches_swipe(fingers, direction))
                            .map(|binding| binding.action.clone())
                    {
                        match action {
                            BindingAction::Focus(direction @ (Direction::Left | Direction::Right)) => {
                                self.focus_direction_from_swipe(direction);
                            }
                            BindingAction::SwitchRelativeWorkspace(next) => {
                                self.switch_relative_workspace(next, Some(direction));
                            }
                            action => self.execute_binding(action),
                        }
                    }
                } else {
                    seat.get_pointer().expect("seat has a pointer").gesture_swipe_end(
                        self,
                        &GestureSwipeEndEvent {
                            serial: SERIAL_COUNTER.next_serial(),
                            time: event.time_msec(),
                            cancelled: event.cancelled(),
                        },
                    );
                }
            }
            InputEvent::SwitchToggle { event } if event.switch() == Some(Switch::Lid) => {
                crate::backends::direct::set_lid_closed(self, event.state() == SwitchState::On);
            }
            InputEvent::Keyboard { event, .. } => {
                let keyboard = self.seat.get_keyboard().expect("seat has a keyboard");
                let keycode = event.key_code();
                let state = event.state();

                let was_captured = self.input_capture.active();
                if self.input_capture.captures(1) {
                    keyboard.unset_grab(self);
                    if keyboard.current_focus().is_some() {
                        keyboard.set_focus(self, None, SERIAL_COUNTER.next_serial());
                    }
                }
                let serial = SERIAL_COUNTER.next_serial();
                let interaction_focus = keyboard.current_focus();
                let client_grab = keyboard
                    .with_grab(|_, grab| grab.is::<smithay::desktop::PopupKeyboardGrab<Self>>())
                    .unwrap_or(true);
                let intercepted = keyboard.input::<(), _>(
                    self,
                    keycode,
                    state,
                    serial,
                    event.time_msec(),
                    |data, modifiers, keysym| {
                        if data.focus_cycle.is_some() {
                            if data.session_lock.active() || data.input_capture.captures(1) {
                                data.cancel_focus_cycle();
                            } else if !modifiers.alt {
                                data.finish_focus_cycle();
                            }
                        }
                        if state == KeyState::Released {
                            data.portal_shortcuts.release(keycode, Event::time(&event));
                            if data.intercepted_keys.remove(&keycode) {
                                return FilterResult::Intercept(());
                            }
                        }
                        if data.session_lock.active() {
                            return FilterResult::Forward;
                        }
                        let symbol = keysym.modified_sym().raw();
                        if data.focus_cycle.is_some() && symbol == keysyms::KEY_Escape {
                            if state == KeyState::Pressed {
                                data.intercepted_keys.insert(keycode);
                                data.cancel_focus_cycle();
                            }
                            return FilterResult::Intercept(());
                        }
                        if data.input_capture.active() && emergency_shortcut_escape(symbol, modifiers.ctrl, modifiers.alt) {
                            if state == KeyState::Pressed {
                                data.input_capture.disable_all();
                                data.intercepted_keys.insert(keycode);
                            }
                            return FilterResult::Intercept(());
                        }
                        if data.input_capture.captures(1) {
                            data.input_capture.push(serde_json::json!({"type":"key", "key":keycode.raw().saturating_sub(8), "pressed":state == KeyState::Pressed}));
                            return FilterResult::Forward;
                        }
                        if data.overview.is_active()
                            && matches!(symbol, keysyms::KEY_Return | keysyms::KEY_KP_Enter)
                        {
                            if state == KeyState::Pressed {
                                data.intercepted_keys.insert(keycode);
                                if let Some(id) = data.overview.selected() {
                                    data.select_overview_window(id);
                                }
                            }
                            return FilterResult::Intercept(());
                        }
                        if overview_escape(symbol, data.overview.is_active()) {
                            if state == KeyState::Pressed {
                                data.intercepted_keys.insert(keycode);
                                data.set_overview_active(false);
                            }

                            return FilterResult::Intercept(());
                        }
                        if emergency_shortcut_escape(symbol, modifiers.ctrl, modifiers.alt) {
                            if state == KeyState::Pressed {
                                data.intercepted_keys.insert(keycode);

                                if let Some(inhibitor) = data.active_shortcuts_inhibitor.take() {
                                    inhibitor.inactivate();
                                }
                            }

                            return FilterResult::Intercept(());
                        }

                        if data.seat.keyboard_shortcuts_inhibited() {
                            return FilterResult::Forward;
                        }

                        if let Some(vt) = virtual_terminal(
                            symbol,
                            modifiers.ctrl,
                            modifiers.alt,
                            data.direct_backend.is_some(),
                        ) {
                            if state == KeyState::Pressed {
                                data.intercepted_keys.insert(keycode);
                                crate::backends::direct::switch_vt(data, vt);
                            }

                            return FilterResult::Intercept(());
                        }

                        let raw_symbols = keysym
                            .raw_syms()
                            .into_iter()
                            .map(|symbol| symbol.raw())
                            .collect::<Vec<_>>();
                        let action = data.bindings.action_for_key(
                            keycode,
                            &raw_symbols,
                            modifiers.logo,
                            modifiers.ctrl,
                            modifiers.alt,
                            modifiers.shift,
                        );
                        if data.focus_cycle.is_some()
                            && state == KeyState::Pressed
                            && !matches!(action, Some(BindingAction::FocusMru(_)))
                            && !matches!(symbol,
                                keysyms::KEY_Alt_L | keysyms::KEY_Alt_R
                                | keysyms::KEY_Shift_L | keysyms::KEY_Shift_R
                                | keysyms::KEY_Control_L | keysyms::KEY_Control_R
                                | keysyms::KEY_Super_L | keysyms::KEY_Super_R)
                        {
                            data.intercepted_keys.insert(keycode);
                            return FilterResult::Intercept(());
                        }
                        let Some(action) = action else {
                            if state == KeyState::Pressed
                                && data.portal_shortcuts.press(
                                    keycode,
                                    &raw_symbols,
                                    modifiers,
                                    Event::time(&event),
                                )
                            {
                                data.intercepted_keys.insert(keycode);
                                return FilterResult::Intercept(());
                            }
                            return FilterResult::Forward;
                        };

                        if state == KeyState::Pressed {
                            data.intercepted_keys.insert(keycode);
                            let action = action.clone();

                            if let BindingAction::FocusMru(reverse) = action {
                                data.cycle_focus(reverse, modifiers.alt);
                            } else {
                                data.execute_binding(action);
                            }
                        }

                        FilterResult::Intercept(())
                    },
                );
                if state == KeyState::Pressed && intercepted.is_none() && client_grab {
                    self.record_activation_input(serial, interaction_focus);
                }

                if was_captured && !self.input_capture.active() {
                    self.restore_input_capture_focus();
                }
            }
            InputEvent::PointerMotionAbsolute { event, .. } => {
                self.last_pointer_time = event.time_msec();
                let Some(position) = self.absolute_event_position(&event) else {
                    return;
                };
                let pointer = self.seat.get_pointer().expect("seat has a pointer");
                if self.capture_pointer_motion(pointer.current_location(), position, true) {
                    return;
                }
                let position = self.constrain_pointer_position(&pointer, position);

                pointer.motion(
                    self,
                    self.surface_under(position),
                    &MotionEvent {
                        location: position,
                        serial: SERIAL_COUNTER.next_serial(),
                        time: event.time_msec(),
                    },
                );
                pointer.frame(self);
                self.focus_window_under_pointer(&pointer, position);
                self.activate_focused_pointer_constraint(&pointer);
                crate::backends::direct::render_cursor(self);
            }
            InputEvent::PointerMotion { event, .. } => {
                self.last_pointer_time = event.time_msec();
                let pointer = self.seat.get_pointer().expect("seat has a pointer");
                let position = pointer.current_location();
                let focus = self.surface_under(position);
                let (scale_x, scale_y) = if self.overview.is_presenting() {
                    self.window_under_visual(position)
                        .and_then(|window| self.visual_scale_for_window(&window))
                        .unwrap_or((1.0, 1.0))
                } else {
                    (1.0, 1.0)
                };
                let delta = event.delta();
                let delta_unaccel = event.delta_unaccel();
                if self.capture_pointer_motion(position, position + delta, false) {
                    return;
                }

                pointer.relative_motion(
                    self,
                    focus,
                    &RelativeMotionEvent {
                        delta: (delta.x / scale_x, delta.y / scale_y).into(),
                        delta_unaccel: (delta_unaccel.x / scale_x, delta_unaccel.y / scale_y).into(),
                        utime: event.time(),
                    },
                );
                let requested = self.clamp_pointer_position(position + delta);
                let location = self.constrain_pointer_position(&pointer, requested);

                pointer.motion(
                    self,
                    self.surface_under(location),
                    &MotionEvent {
                        location,
                        serial: SERIAL_COUNTER.next_serial(),
                        time: event.time_msec(),
                    },
                );
                pointer.frame(self);
                self.focus_window_under_pointer(&pointer, location);
                self.activate_focused_pointer_constraint(&pointer);
                crate::backends::direct::render_cursor(self);
            }
            InputEvent::PointerButton { event, .. } => {
                self.last_pointer_time = event.time_msec();
                if self.input_capture.active() {
                    if self.input_capture.captures(2) {
                        self.input_capture.push(serde_json::json!({"type":"button", "button":event.button_code(), "pressed":event.state() == ButtonState::Pressed}));
                    }
                    return;
                }
                let pointer = self.seat.get_pointer().expect("seat has a pointer");
                let serial = SERIAL_COUNTER.next_serial();
                if self.session_lock.active() {
                    if event.state() == ButtonState::Pressed {
                        self.focus_window_at(pointer.current_location(), serial, true);
                        if !pointer.is_grabbed() {
                            pointer.motion(
                                self,
                                self.lock_surface_under(pointer.current_location()),
                                &MotionEvent {
                                    location: pointer.current_location(),
                                    serial,
                                    time: event.time_msec(),
                                },
                            );
                        }
                    }
                    pointer.button(
                        self,
                        &ButtonEvent {
                            button: event.button_code(),
                            state: event.state(),
                            serial,
                            time: event.time_msec(),
                        },
                    );
                    pointer.frame(self);
                    return;
                }

                // Native popup_done destroys Iced's window immediately. For
                // effects-capable shell popups, release input now but defer
                // popup_done until the compositor's whole-surface fade ends.
                if event.state() == ButtonState::Pressed
                    && pointer.is_grabbed()
                    && let Some(surface) = self.seat.get_keyboard().and_then(|k| k.current_focus())
                    && let Some(popup) = self.popups.find_popup(&surface)
                    && !self
                        .surface_under(pointer.current_location())
                        .is_some_and(|(target, _)| surface.id().same_client_as(&target.id()))
                    && let Ok(root) = smithay::desktop::find_popup_root_surface(&popup)
                    && crate::effects::begin_surface_dismiss(&surface)
                {
                    let opacity = crate::effects::surface_opacity(&surface);
                    self.dismissing_popups.push((
                        root.clone(),
                        popup,
                        crate::focus_effect::BoundedFade::new(f64::from(opacity)),
                    ));
                    pointer.unset_grab(self, serial, event.time_msec());
                    self.focus_window_at(pointer.current_location(), serial, true);
                    crate::backends::direct::render_surface(self, &root);
                }

                if self.overview.is_active() {
                    let position = pointer.current_location();
                    if self.layer_under(position).is_some() {
                        if event.state() == ButtonState::Pressed {
                            self.record_activation_input(serial, pointer.current_focus());
                        }

                        pointer.button(
                            self,
                            &ButtonEvent {
                                button: event.button_code(),
                                state: event.state(),
                                serial,
                                time: event.time_msec(),
                            },
                        );
                        pointer.frame(self);
                        return;
                    }

                    if event.state() == ButtonState::Pressed {
                        if self.click_overview_workspace(position) {
                            return;
                        }

                        if let Some(window) = self.window_under_visual(position)
                            && let Some(id) = self.windows.ids().get(&window).copied()
                        {
                            self.select_overview_window(id);
                        } else {
                            self.set_overview_active(false);
                        }
                    }

                    return;
                }

                if event.state() == ButtonState::Pressed
                    && self.start_floating_pointer_grab(&pointer, event.button_code(), serial)
                {
                    pointer.button(
                        self,
                        &ButtonEvent {
                            button: event.button_code(),
                            state: event.state(),
                            serial,
                            time: event.time_msec(),
                        },
                    );
                    pointer.frame(self);
                    return;
                }

                if event.state() == ButtonState::Pressed && !pointer.is_grabbed() {
                    self.focus_window_at(pointer.current_location(), serial, true);
                }

                let client_grab = pointer
                    .with_grab(|_, grab| {
                        grab.is::<smithay::desktop::PopupPointerGrab<Self>>()
                            || grab.is::<smithay::input::pointer::ClickGrab<Self>>()
                    })
                    .unwrap_or(true);
                if event.state() == ButtonState::Pressed && client_grab {
                    self.record_activation_input(serial, pointer.current_focus());
                }

                pointer.button(
                    self,
                    &ButtonEvent {
                        button: event.button_code(),
                        state: event.state(),
                        serial,
                        time: event.time_msec(),
                    },
                );
                pointer.frame(self);
            }
            InputEvent::PointerAxis { event, .. } => {
                self.last_pointer_time = event.time_msec();
                let source = event.source();
                let horizontal = event
                    .amount(Axis::Horizontal)
                    .unwrap_or_else(|| event.amount_v120(Axis::Horizontal).unwrap_or(0.0) * 15.0 / 120.0);
                let vertical = event
                    .amount(Axis::Vertical)
                    .unwrap_or_else(|| event.amount_v120(Axis::Vertical).unwrap_or(0.0) * 15.0 / 120.0);
                if self.input_capture.active() {
                    if self.input_capture.captures(2) {
                        self.input_capture.push(serde_json::json!({"type":"scroll", "x":horizontal, "y":vertical, "v120_x":event.amount_v120(Axis::Horizontal), "v120_y":event.amount_v120(Axis::Vertical), "stop_x":source == AxisSource::Finger && event.amount(Axis::Horizontal) == Some(0.0), "stop_y":source == AxisSource::Finger && event.amount(Axis::Vertical) == Some(0.0)}));
                    }
                    return;
                }
                let pointer = self.seat.get_pointer().expect("seat has a pointer");
                if self.scroll_overview_strip(
                    pointer.current_location(),
                    if horizontal != 0.0 { horizontal } else { vertical },
                ) {
                    return;
                }
                let mut frame = AxisFrame::new(event.time_msec()).source(source);
                if horizontal != 0.0 {
                    frame = frame.value(Axis::Horizontal, horizontal);
                    if let Some(value) = event.amount_v120(Axis::Horizontal) {
                        frame = frame.v120(Axis::Horizontal, value as i32);
                    }
                }

                if vertical != 0.0 {
                    frame = frame.value(Axis::Vertical, vertical);
                    if let Some(value) = event.amount_v120(Axis::Vertical) {
                        frame = frame.v120(Axis::Vertical, value as i32);
                    }
                }

                if source == AxisSource::Finger {
                    if event.amount(Axis::Horizontal) == Some(0.0) {
                        frame = frame.stop(Axis::Horizontal);
                    }
                    if event.amount(Axis::Vertical) == Some(0.0) {
                        frame = frame.stop(Axis::Vertical);
                    }
                }

                let pointer = self.seat.get_pointer().expect("seat has a pointer");

                pointer.axis(self, frame);
                pointer.frame(self);
            }
            InputEvent::TouchDown { event, .. } => {
                let Some(location) = self.absolute_event_position(&event) else {
                    return;
                };
                self.focus_window_at(location, SERIAL_COUNTER.next_serial(), true);
                let touch = self.seat.get_touch().expect("seat has touch capability");
                let serial = SERIAL_COUNTER.next_serial();

                if !touch.is_grabbed() {
                    self.record_activation_input(serial, self.surface_under(location).map(|(surface, _)| surface));
                }

                touch.down(
                    self,
                    self.surface_under(location),
                    &DownEvent {
                        slot: event.slot(),
                        location,
                        serial,
                        time: event.time_msec(),
                    },
                );
            }
            InputEvent::TouchMotion { event, .. } => {
                let Some(location) = self.absolute_event_position(&event) else {
                    return;
                };
                let touch = self.seat.get_touch().expect("seat has touch capability");

                touch.motion(
                    self,
                    self.surface_under(location),
                    &TouchMotionEvent {
                        slot: event.slot(),
                        location,
                        time: event.time_msec(),
                    },
                );
            }
            InputEvent::TouchUp { event, .. } => {
                let touch = self.seat.get_touch().expect("seat has touch capability");
                touch.up(
                    self,
                    &UpEvent {
                        slot: event.slot(),
                        serial: SERIAL_COUNTER.next_serial(),
                        time: event.time_msec(),
                    },
                );
            }
            InputEvent::TouchCancel { .. } => {
                let touch = self.seat.get_touch().expect("seat has touch capability");
                touch.cancel(self);
            }
            InputEvent::TouchFrame { .. } => {
                let touch = self.seat.get_touch().expect("seat has touch capability");
                touch.frame(self);
            }
            _ => {}
        }
    }

    fn start_floating_pointer_grab(&mut self, pointer: &PointerHandle<Self>, button: u32, serial: Serial) -> bool {
        if self.session_lock.active()
            || pointer.is_grabbed()
            || !matches!(button, BTN_LEFT | BTN_RIGHT)
            || !self
                .seat
                .get_keyboard()
                .is_some_and(|keyboard| keyboard.modifier_state().logo)
        {
            return false;
        }

        let location = pointer.current_location();
        if self.layer_under(location).is_some() {
            return false;
        }
        let Some(window) = self.window_under_visual(location) else {
            return false;
        };
        if !self.is_floating_window(&window) {
            return false;
        }
        let Some(rect) = self.visual_rect_for_window(&window) else {
            return false;
        };

        self.focus_window_at(location, serial, true);
        let start_data = GrabStartData {
            focus: None,
            button,
            location,
        };

        if button == BTN_LEFT {
            pointer.set_grab(
                self,
                MoveSurfaceGrab {
                    start_data,
                    window,
                    initial_location: rect.loc,
                    initial_size: rect.size,
                    finished: false,
                    snap_x: Default::default(),
                    snap_y: Default::default(),
                },
                serial,
                Focus::Clear,
            );
        } else {
            let edge = ResizeEdge::at(location, rect);
            pointer.set_grab(
                self,
                ResizeSurfaceGrab::new(start_data, window, edge, rect),
                serial,
                Focus::Clear,
            );
        }

        true
    }

    fn absolute_event_position<I, E>(&self, event: &E) -> Option<Point<f64, Logical>>
    where
        I: InputBackend,
        E: AbsolutePositionEvent<I>,
    {
        let output = self.focused_output().or_else(|| self.space.outputs().next())?;
        let geometry = self.space.output_geometry(output)?;

        Some(event.position_transformed(geometry.size) + geometry.loc.to_f64())
    }

    fn swipe_navigation_blocked(&self) -> bool {
        self.session_lock.active()
            || self.active_shortcuts_inhibitor.is_some()
            || self.seat.get_pointer().is_some_and(|pointer| pointer.is_grabbed())
            || self.seat.get_keyboard().is_some_and(|keyboard| keyboard.is_grabbed())
    }

    fn switch_relative_workspace(&mut self, next: bool, slide: Option<SwipeDirection>) {
        if let Some(workspace) = self.relative_workspace_target(next) {
            if let Some(direction) = slide {
                self.activate_managed_workspace_from_swipe(workspace, direction);
            } else {
                self.activate_managed_workspace(workspace);
            }
        }
    }

    pub(crate) fn relative_workspace_target(&self, next: bool) -> Option<ferese_core::WorkspaceId> {
        let output = self.output_workspaces.focused_output()?;
        let current = self.output_workspaces.active_workspace(output)?;
        let candidates = self.output_workspaces.ordered_workspaces(&self.workspaces, output);
        let index = candidates.iter().position(|id| *id == current)?;
        let neighbor = if next {
            index.checked_add(1)?
        } else {
            index.checked_sub(1)?
        };

        candidates.get(neighbor).copied()
    }

    fn focus_window_at(&mut self, position: Point<f64, Logical>, serial: Serial, raise: bool) {
        if self.input_capture.captures(1) {
            return;
        }
        if self.session_lock.active() {
            self.focus_output_at(position);
            self.focus_lock_surface();
            return;
        }
        let keyboard = self.seat.get_keyboard().expect("seat has a keyboard");
        self.focus_output_at(position);

        if let Some((layer, _, _)) = self.layer_under(position) {
            if layer.can_receive_keyboard_focus() {
                keyboard.set_focus(self, Some(layer.wl_surface().clone()), serial);
            }
            return;
        }

        let previous = self.focused_window;

        if let Some(window) = self.window_under_visual(position) {
            let focused = self.windows.ids().get(&window).copied();
            if let Some(focused) = focused {
                let result = if raise {
                    self.workspaces.focus_window(focused)
                } else {
                    self.workspaces.focus_window_without_reveal(focused)
                };
                if let Err(error) = result {
                    tracing::error!(%error, ?focused, "failed to update workspace focus");
                    return;
                }
            }
            self.focused_window = focused;
            if raise {
                self.raise_window(&window, true);
            } else {
                // Hover transfers keyboard focus without changing the persistent
                // stack: an exposed window must not cover the floats above it.
                if let Some(previous) = previous
                    .filter(|id| Some(*id) != focused)
                    .and_then(|id| self.windows.window(id))
                {
                    previous.set_activated(false);

                    if let Some(toplevel) = previous.toplevel() {
                        toplevel.send_pending_configure();
                    }
                }

                window.set_activated(true);

                if let Some(toplevel) = window.toplevel() {
                    toplevel.send_pending_configure();
                }
            }
            let surface = window
                .toplevel()
                .expect("mapped window has a toplevel")
                .wl_surface()
                .clone();
            keyboard.set_focus(self, Some(surface), serial);
        } else {
            if let Some(previous) = previous.and_then(|id| self.windows.window(id)) {
                previous.set_activated(false);

                if let Some(toplevel) = previous.toplevel() {
                    toplevel.send_pending_configure();
                }
            }

            self.focused_window = None;
            keyboard.set_focus(self, Option::<WlSurface>::None, serial);
        }

        if raise {
            let outputs = self
                .space
                .outputs()
                .filter(|output| {
                    self.space
                        .output_geometry(output)
                        .is_some_and(|bounds| bounds.to_f64().contains(position))
                        || previous
                            .into_iter()
                            .chain(self.focused_window)
                            .any(|id| self.window_belongs_to_output(id, output))
                })
                .cloned()
                .collect::<Vec<_>>();
            self.relayout_on(&outputs);
        } else {
            // Focus, dimming and the bar title update without moving the layout.
            self.send_shell_snapshots();
            let outputs = self
                .space
                .outputs()
                .filter(|output| {
                    previous
                        .into_iter()
                        .chain(self.focused_window)
                        .any(|id| self.window_belongs_to_output(id, output))
                })
                .cloned()
                .collect::<Vec<_>>();
            crate::backends::direct::render_on(self, &outputs);
        }
    }

    fn focus_window_under_pointer(&mut self, pointer: &PointerHandle<Self>, position: Point<f64, Logical>) {
        if self.session_lock.active() {
            return;
        }
        if self.overview.is_active() {
            self.hover_overview_window(position);
            return;
        }
        // Keep the explicitly selected window focused until Overview's exit
        // animation settles. Moving previews must not steal focus on pointer jitter.
        if self.overview.is_presenting() || !self.input_settings.focus_follows_mouse || pointer.is_grabbed() {
            return;
        }
        if self.layer_under(position).is_some() {
            return;
        }

        let Some(window) = self.window_under_visual(position) else {
            return;
        };
        if self.windows.ids().get(&window).copied() == self.focused_window {
            return;
        }

        self.focus_window_at(position, SERIAL_COUNTER.next_serial(), false);
    }

    fn clamp_pointer_position(&self, requested: Point<f64, Logical>) -> Point<f64, Logical> {
        self.space
            .outputs()
            .filter_map(|output| self.space.output_geometry(output))
            .map(|geometry| {
                let position = Point::from((
                    requested.x.clamp(
                        f64::from(geometry.loc.x),
                        f64::from(geometry.loc.x + geometry.size.w - 1),
                    ),
                    requested.y.clamp(
                        f64::from(geometry.loc.y),
                        f64::from(geometry.loc.y + geometry.size.h - 1),
                    ),
                ));
                let x = position.x - requested.x;
                let y = position.y - requested.y;
                (x * x + y * y, position)
            })
            .min_by(|(left, _), (right, _)| left.total_cmp(right))
            .map(|(_, position)| position)
            .unwrap_or(requested)
    }

    fn constrain_pointer_position(
        &self,
        pointer: &PointerHandle<Self>,
        requested: Point<f64, Logical>,
    ) -> Point<f64, Logical> {
        if self.session_lock.active() {
            return requested;
        }
        let current = pointer.current_location();
        let Some(focused_surface) = pointer.current_focus() else {
            return requested;
        };
        let Some((surface, origin)) = self.surface_under(current) else {
            return requested;
        };
        if surface != focused_surface {
            return requested;
        }

        with_pointer_constraint(&surface, pointer, |constraint| {
            let Some(constraint) = constraint.filter(|constraint| constraint.is_active()) else {
                return requested;
            };

            match &*constraint {
                PointerConstraint::Locked(_) => current,
                PointerConstraint::Confined(_) => {
                    let remains_on_surface = self
                        .surface_under(requested)
                        .is_some_and(|(candidate, _)| candidate == surface);
                    let inside_region = constraint
                        .region()
                        .is_none_or(|region| region.contains((requested - origin).to_i32_round()));

                    if remains_on_surface && inside_region {
                        requested
                    } else {
                        current
                    }
                }
            }
        })
    }

    pub fn activate_focused_pointer_constraint(&self, pointer: &PointerHandle<Self>) {
        if self.session_lock.active() {
            return;
        }
        let position = pointer.current_location();
        let Some((surface, origin)) = self.surface_under(position) else {
            return;
        };
        if pointer.current_focus().as_ref() != Some(&surface) {
            return;
        }

        with_pointer_constraint(&surface, pointer, |constraint| {
            let Some(constraint) = constraint else {
                return;
            };
            let inside_region = constraint
                .region()
                .is_none_or(|region| region.contains((position - origin).to_i32_round()));

            if !constraint.is_active() && inside_region {
                constraint.activate();
            }
        });
    }

    fn execute_binding(&mut self, action: BindingAction) {
        match action {
            BindingAction::None => {}
            BindingAction::Spawn(argv) => {
                if let Some(child) =
                    spawn_client(self, argv.iter(), crate::private_client::ClientCapabilities::default())
                {
                    crate::process::watch_client_exit(&self.loop_handle, child);
                }
            }
            BindingAction::Close => self.close_focused_window(),
            BindingAction::Exit => self.request_logout_confirmation(),
            BindingAction::Focus(direction) => self.focus_direction(direction),
            BindingAction::FocusLastWindow => self.focus_last_window(),
            BindingAction::FocusMru(reverse) => self.cycle_focus(reverse, false),
            BindingAction::Move(direction) => self.move_direction(direction),
            BindingAction::Resize(direction) => self.resize_direction(direction),
            BindingAction::SwitchRelativeWorkspace(next) => self.switch_relative_workspace(next, None),
            BindingAction::WorkspaceBackAndForth => self.workspace_back_and_forth(),
            BindingAction::SwitchWorkspace(workspace) => {
                self.switch_workspace_from_binding(u32::from(workspace));
            }
            BindingAction::MoveToWorkspace(workspace) => {
                self.move_focused_to_workspace(u32::from(workspace));
            }
            BindingAction::ToggleFullscreen => self.toggle_focused_fullscreen(),
            BindingAction::ToggleMaximized => self.toggle_focused_maximized(),
            BindingAction::ToggleLayout => self.toggle_layout_mode(),
            BindingAction::CycleColumnWidth => self.cycle_focused_column_width(),
            BindingAction::CenterColumn => self.center_focused_column(),
            BindingAction::Consume => self.consume_focused_window(),
            BindingAction::Expel => self.expel_focused_window(),
            BindingAction::ToggleFloating => self.toggle_focused_floating(),
            BindingAction::ToggleOverview => self.toggle_overview(),
            BindingAction::Media(action) => {
                if let Err(error) = self.media_engine.action(serde_json::json!({"action": action}), None) {
                    tracing::debug!(%error, "media action unavailable");
                }
            }
            BindingAction::ToggleDisplayMode => {
                self.toggle_display_mode();
            }
            BindingAction::ToggleKeybindingGuide => {
                self.toggle_keybinding_guide();
            }
        }
    }
}

fn surface_tree_position(
    root: &WlSurface,
    target: &WlSurface,
    geometry_origin: Point<i32, Logical>,
) -> Option<Point<i32, Logical>> {
    use smithay::desktop::PopupManager;

    surface_tree_offset(root, target).or_else(|| {
        PopupManager::popups_for_surface(root).find_map(|(popup, offset)| {
            surface_tree_offset(popup.wl_surface(), target)
                .map(|local| geometry_origin + offset - popup.geometry().loc + local)
        })
    })
}

fn surface_tree_offset(root: &WlSurface, target: &WlSurface) -> Option<Point<i32, Logical>> {
    use std::cell::Cell;

    use smithay::backend::renderer::utils::RendererSurfaceStateUserData;
    use smithay::wayland::compositor::{TraversalAction, with_surface_tree_downward};

    let found = Cell::new(None);
    with_surface_tree_downward(
        root,
        Point::<i32, Logical>::default(),
        |_, states, location| {
            states
                .data_map
                .get::<RendererSurfaceStateUserData>()
                .and_then(|data| data.lock().unwrap().view())
                .map_or(TraversalAction::SkipChildren, |view| {
                    TraversalAction::DoChildren(*location + view.offset)
                })
        },
        |surface, states, location| {
            if surface == target
                && let Some(view) = states
                    .data_map
                    .get::<RendererSurfaceStateUserData>()
                    .and_then(|data| data.lock().unwrap().view())
            {
                found.set(Some(*location + view.offset));
            }
        },
        |_, _, _| found.get().is_none(),
    );
    found.get()
}

fn virtual_terminal(symbol: u32, ctrl: bool, alt: bool, direct: bool) -> Option<i32> {
    if !direct {
        return None;
    }

    if (keysyms::KEY_XF86Switch_VT_1..=keysyms::KEY_XF86Switch_VT_12).contains(&symbol) {
        return Some((symbol - keysyms::KEY_XF86Switch_VT_1 + 1) as i32);
    }

    (ctrl && alt && (keysyms::KEY_F1..=keysyms::KEY_F12).contains(&symbol))
        .then_some((symbol - keysyms::KEY_F1 + 1) as i32)
}

fn emergency_shortcut_escape(symbol: u32, ctrl: bool, alt: bool) -> bool {
    ctrl && alt && symbol == keysyms::KEY_Escape
}

fn overview_escape(symbol: u32, overview_active: bool) -> bool {
    overview_active && symbol == keysyms::KEY_Escape
}

#[cfg(test)]
mod tests {
    use super::{emergency_shortcut_escape, overview_escape, virtual_terminal};

    #[test]
    fn binding_commands_are_reaped_while_the_compositor_runs() {
        if !crate::startup_tests::private_runtime("input::tests::binding_commands_are_reaped_while_the_compositor_runs")
        {
            return;
        }

        let mut events = smithay::reexports::calloop::EventLoop::try_new().unwrap();
        let mut state = crate::startup_tests::state(&mut events);
        let children_path = format!("/proc/self/task/{}/children", unsafe { libc::gettid() });
        assert!(std::fs::read_to_string(&children_path).unwrap().trim().is_empty());
        for _ in 0..24 {
            state.execute_binding(crate::config::BindingAction::Spawn(vec!["/bin/true".into()].into()));
        }

        let children: Vec<i32> = std::fs::read_to_string(&children_path)
            .unwrap()
            .split_whitespace()
            .map(|pid| pid.parse().unwrap())
            .collect();
        assert_eq!(children.len(), 24);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while !std::fs::read_to_string(&children_path).unwrap().trim().is_empty() {
            assert!(std::time::Instant::now() < deadline, "binding children were not reaped");
            events
                .dispatch(Some(std::time::Duration::from_millis(20)), &mut state)
                .unwrap();
            crate::after_dispatch(&mut state);
        }

        for pid in children {
            let mut status = 0;
            assert_eq!(unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) }, -1);
            assert_eq!(std::io::Error::last_os_error().raw_os_error(), Some(libc::ECHILD));
        }
    }

    #[test]
    fn escape_closes_only_an_active_overview() {
        assert!(overview_escape(keysyms::KEY_Escape, true));
        assert!(!overview_escape(keysyms::KEY_Escape, false));
        assert!(!overview_escape(keysyms::KEY_q, true));
    }

    #[test]
    fn emergency_escape_requires_control_alt_escape() {
        assert!(emergency_shortcut_escape(keysyms::KEY_Escape, true, true));
        assert!(!emergency_shortcut_escape(keysyms::KEY_Escape, true, false));
        assert!(!emergency_shortcut_escape(keysyms::KEY_q, true, true));
    }
    use smithay::input::keyboard::keysyms;

    #[test]
    fn maps_xkb_virtual_terminal_symbols_for_direct_sessions() {
        assert_eq!(
            virtual_terminal(keysyms::KEY_XF86Switch_VT_1, false, false, true),
            Some(1)
        );
        assert_eq!(
            virtual_terminal(keysyms::KEY_XF86Switch_VT_12, false, false, true),
            Some(12)
        );
    }

    #[test]
    fn accepts_plain_function_symbols_only_with_control_alt() {
        assert_eq!(virtual_terminal(keysyms::KEY_F7, true, true, true), Some(7));
        assert_eq!(virtual_terminal(keysyms::KEY_F7, true, false, true), None);
        assert_eq!(virtual_terminal(keysyms::KEY_F7, false, true, true), None);
    }

    #[test]
    fn leaves_virtual_terminal_keys_to_nested_clients() {
        assert_eq!(
            virtual_terminal(keysyms::KEY_XF86Switch_VT_3, false, false, false),
            None
        );
        assert_eq!(virtual_terminal(keysyms::KEY_F3, true, true, false), None);
    }
}
