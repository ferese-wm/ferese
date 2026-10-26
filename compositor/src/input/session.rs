//! Retire physical input ownership when the backend can no longer deliver releases.
use smithay::backend::input::{ButtonState, InputTime, KeyState};
use smithay::input::keyboard::KeyboardSource;
use smithay::input::pointer::{ButtonEvent, MotionEvent, PointerHandle};
use smithay::utils::SERIAL_COUNTER;

use crate::Ferese;

impl Ferese {
    /// Track buttons submitted to Smithay, including compositor-owned grabs.
    /// Consumed and captured presses do not acquire local pointer ownership.
    pub(super) fn send_pointer_button(
        &mut self,
        pointer: &PointerHandle<Self>,
        event: &ButtonEvent,
    ) {
        match event.state {
            ButtonState::Pressed => {
                self.pointer_buttons.insert(event.button);
            }
            ButtonState::Released => {
                self.pointer_buttons.remove(&event.button);
            }
        }
        pointer.button(self, event);
    }

    pub(crate) fn reset_session_input(&mut self) {
        let time = InputTime::now();
        let mut devices = self.take_physical_holds();
        self.cancel_window_interaction();
        self.set_binding_mode("default");
        self.cancel_focus_cycle();
        self.swipe = Default::default();
        self.finish_workspace_swipe(None);
        self.finish_focus_swipe(None);
        self.input_capture.disable_all();
        for kind in [
            super::Gesture::Swipe,
            super::Gesture::Pinch,
            super::Gesture::Hold,
        ] {
            self.cancel_input_gesture(kind, time);
        }
        for device in &mut devices {
            self.release_device_keys(device.id, std::mem::take(&mut device.keys), time);
        }

        if let Some(keyboard) = self.seat.get_keyboard() {
            let mut held = keyboard.pressed_keys();
            held.extend(self.binding_mode_pressed.iter().copied());
            for key in held {
                // release_source forwards intercepted keys too. Use its source
                // accounting with our existing press ownership filter instead.
                keyboard.input_from_source::<(), _>(
                    KeyboardSource::MAIN,
                    self,
                    key,
                    KeyState::Released,
                    SERIAL_COUNTER.next_serial(),
                    time,
                    |state, _, _| state.filter_key_release(key, time),
                );
            }
            keyboard.unset_grab(self);
            // A consumed modifier release still changes the shared XKB state.
            keyboard.advertise_modifier_state(self);
            let remaining = keyboard.pressed_keys();
            self.binding_mode_pressed
                .retain(|key| remaining.contains(key));
            self.intercepted_keys.retain(|key| remaining.contains(key));
        } else {
            self.binding_mode_pressed.clear();
            self.intercepted_keys.clear();
        }
        self.binding_mode_repeats.clear();

        if let Some(pointer) = self.seat.get_pointer() {
            for button in std::mem::take(&mut self.pointer_buttons) {
                pointer.button(
                    self,
                    &ButtonEvent {
                        button,
                        state: ButtonState::Released,
                        serial: SERIAL_COUNTER.next_serial(),
                        time,
                    },
                );
                // A delayed release after resume still belongs to this press.
                // A fresh press clears this suppression in the input route.
                self.consumed_pointer_buttons.insert(button);
            }
            pointer.unset_grab(self, SERIAL_COUNTER.next_serial(), time);
            // Leaving cancels only gestures actually begun on the old surface
            // and deactivates its pointer constraint before libinput suspends.
            pointer.motion(
                self,
                None,
                &MotionEvent {
                    location: pointer.current_location(),
                    serial: SERIAL_COUNTER.next_serial(),
                    time,
                },
            );
            pointer.frame(self);
        }
        self.pointer_scroll = Default::default();
        self.locked_pointer_hint = None;
        self.pending_layer_focus = None;
        if let Some(touch) = self.seat.get_touch() {
            touch.unset_grab(self);
            // Mark every delivered contact in this cancellation batch even if
            // its last hardware frame has already been flushed.
            for contact in devices
                .into_iter()
                .flat_map(|device| device.contacts.into_values())
            {
                if !self.consumed_touch_slots.contains(&contact.slot) {
                    touch.motion(
                        self,
                        None,
                        &smithay::input::touch::MotionEvent {
                            slot: contact.slot,
                            location: contact.position,
                            time,
                        },
                    );
                }
            }
            touch.cancel(self);
        }
        self.consumed_touch_slots.clear();
    }
}

#[cfg(test)]
pub(in crate::input) mod tests;
