use super::{Contact, DeviceId, Gesture, Held};
use crate::Ferese;
use smithay::backend::input::{Device, Event, InputBackend, InputEvent, InputTime, KeyState};
use smithay::input::keyboard::{FilterResult, Keycode};
use smithay::input::pointer::{
    ButtonEvent, GestureHoldEndEvent, GesturePinchEndEvent, GestureSwipeEndEvent,
};
use smithay::input::touch::UpEvent;
use smithay::utils::SERIAL_COUNTER;
use std::collections::HashMap;

impl Ferese {
    pub(crate) fn add_input_device<D: Device + 'static>(&mut self, device: D) {
        self.physical_input.add(device);
    }

    pub(crate) fn remove_input_device<D: Device + 'static>(&mut self, device: &D) {
        let Some(held) = self.physical_input.remove(device) else {
            return;
        };
        let time = InputTime::now();
        self.pointer_scroll.remove_device(&device.id());
        for kind in [Gesture::Swipe, Gesture::Pinch, Gesture::Hold] {
            if self.physical_input.gesture_owner(kind) == Some(held.id) {
                self.cancel_input_gesture(kind, time);
            }
        }
        let buttons = held
            .buttons
            .iter()
            .copied()
            .filter(|button| !self.physical_input.button_held(*button))
            .collect::<Vec<_>>();
        if self
            .seat
            .get_pointer()
            .and_then(|pointer| pointer.grab_start_data())
            .is_some_and(|start| buttons.contains(&start.button))
        {
            self.cancel_window_interaction();
        }
        self.release_device_keys(held.id, held.keys, time);
        for button in buttons {
            if self.consumed_pointer_buttons.remove(&button) {
                continue;
            }
            if self.input_capture.captures(2) {
                self.input_capture
                    .push(serde_json::json!({"type":"button", "button":button, "pressed":false}));
            } else if self.pointer_buttons.contains(&button)
                && let Some(pointer) = self.seat.get_pointer()
            {
                self.send_pointer_button(
                    &pointer,
                    &ButtonEvent {
                        button,
                        state: smithay::backend::input::ButtonState::Released,
                        serial: SERIAL_COUNTER.next_serial(),
                        time,
                    },
                );
                pointer.frame(self);
            }
        }
        self.retire_touch_contacts(held.contacts, time);
    }

    pub(crate) fn filter_key_release(&mut self, key: Keycode, time: InputTime) -> FilterResult<()> {
        self.binding_mode_pressed.remove(&key);
        self.binding_mode_repeats.remove(&key);
        self.portal_shortcuts.release(key, time.micros() / 1000);
        if self.intercepted_keys.remove(&key) {
            return FilterResult::Intercept(());
        }
        if self.input_capture.captures(1) {
            self.input_capture.push(serde_json::json!({
                "type":"key", "key":key.raw().saturating_sub(8), "pressed":false
            }));
        }
        FilterResult::Forward
    }

    pub(in crate::input) fn release_device_keys(
        &mut self,
        id: DeviceId,
        keys: impl IntoIterator<Item = Keycode>,
        time: InputTime,
    ) {
        let Some(keyboard) = self.seat.get_keyboard() else {
            return;
        };
        for key in keys {
            keyboard.input_from_source::<(), _>(
                id.0,
                self,
                key,
                KeyState::Released,
                SERIAL_COUNTER.next_serial(),
                time,
                |state, modifiers, _| {
                    if !modifiers.alt {
                        state.cancel_focus_cycle();
                    }
                    state.filter_key_release(key, time)
                },
            );
        }
        keyboard.advertise_modifier_state(self);
    }

    pub(in crate::input) fn retire_touch_contacts(
        &mut self,
        contacts: HashMap<smithay::backend::input::TouchSlot, Contact>,
        time: InputTime,
    ) {
        if contacts.is_empty() {
            return;
        }
        self.pending_layer_focus = None;
        let Some(touch) = self.seat.get_touch() else {
            return;
        };
        // wl_touch.cancel cancels a client's entire sequence. Retire only this
        // device's delivered contacts so another touchscreen remains usable.
        for contact in contacts.into_values() {
            if !self.consumed_touch_slots.remove(&contact.slot) {
                touch.up(
                    self,
                    &UpEvent {
                        slot: contact.slot,
                        serial: SERIAL_COUNTER.next_serial(),
                        time,
                    },
                );
            }
        }
        touch.frame(self);
    }

    pub(in crate::input) fn begin_input_gesture(
        &mut self,
        device: DeviceId,
        kind: Gesture,
        time: InputTime,
    ) {
        if self.physical_input.gesture_owner(kind).is_some() {
            self.cancel_input_gesture(kind, time);
        }
        self.physical_input.set_gesture_owner(kind, Some(device));
    }

    pub(in crate::input) fn end_input_gesture(&mut self, device: DeviceId, kind: Gesture) -> bool {
        if self.physical_input.gesture_owner(kind) != Some(device) {
            return false;
        }
        self.physical_input.set_gesture_owner(kind, None);
        true
    }

    pub(in crate::input) fn cancel_input_gesture(&mut self, kind: Gesture, time: InputTime) {
        self.physical_input.set_gesture_owner(kind, None);
        let serial = SERIAL_COUNTER.next_serial();
        if matches!(kind, Gesture::Swipe) {
            self.swipe = Default::default();
            self.finish_workspace_swipe(None);
            self.finish_focus_swipe(None);
        }
        if let Some(pointer) = self.seat.get_pointer() {
            match kind {
                Gesture::Swipe => pointer.gesture_swipe_end(
                    self,
                    &GestureSwipeEndEvent {
                        serial,
                        time,
                        cancelled: true,
                    },
                ),
                Gesture::Pinch => pointer.gesture_pinch_end(
                    self,
                    &GesturePinchEndEvent {
                        serial,
                        time,
                        cancelled: true,
                    },
                ),
                Gesture::Hold => pointer.gesture_hold_end(
                    self,
                    &GestureHoldEndEvent {
                        serial,
                        time,
                        cancelled: true,
                    },
                ),
            }
        }
    }

    pub(in crate::input) fn input_device<I: InputBackend>(
        &mut self,
        event: &InputEvent<I>,
    ) -> Option<DeviceId>
    where
        I::Device: 'static,
    {
        let device = match event {
            InputEvent::Keyboard { event } => event.device(),
            InputEvent::PointerButton { event } => event.device(),
            InputEvent::PointerMotion { event } => event.device(),
            InputEvent::PointerMotionAbsolute { event } => event.device(),
            InputEvent::PointerAxis { event } => event.device(),
            InputEvent::GestureSwipeBegin { event } => event.device(),
            InputEvent::GestureSwipeUpdate { event } => event.device(),
            InputEvent::GestureSwipeEnd { event } => event.device(),
            InputEvent::GesturePinchBegin { event } => event.device(),
            InputEvent::GesturePinchUpdate { event } => event.device(),
            InputEvent::GesturePinchEnd { event } => event.device(),
            InputEvent::GestureHoldBegin { event } => event.device(),
            InputEvent::GestureHoldEnd { event } => event.device(),
            InputEvent::TouchDown { event } => event.device(),
            InputEvent::TouchMotion { event } => event.device(),
            InputEvent::TouchUp { event } => event.device(),
            InputEvent::TouchCancel { event } => event.device(),
            InputEvent::TouchFrame { event } => event.device(),
            InputEvent::TabletToolAxis { event } => event.device(),
            InputEvent::TabletToolProximity { event } => event.device(),
            InputEvent::TabletToolTip { event } => event.device(),
            InputEvent::TabletToolButton { event } => event.device(),
            InputEvent::SwitchToggle { event } => event.device(),
            _ => return None,
        };
        self.physical_input.resolve(device)
    }

    pub(in crate::input) fn take_physical_holds(&mut self) -> Vec<Held> {
        self.physical_input.take_all()
    }
}
