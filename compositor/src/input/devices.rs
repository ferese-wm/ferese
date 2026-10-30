//! Physical device generations and the input transitions owned by each one.
use std::any::{Any, TypeId};
use std::collections::{HashMap, HashSet};

use smithay::backend::input::{ButtonState, Device, KeyState, TouchSlot};
use smithay::input::keyboard::{KeyboardSource, Keycode};
use smithay::utils::{Logical, Point};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct DeviceId(pub KeyboardSource);

#[derive(Clone, Copy)]
pub(super) enum Gesture {
    Swipe = 0,
    Pinch = 1,
    Hold = 2,
}

pub(super) enum KeyTransition {
    New,
    Repeat,
    Ignore,
}

pub(super) struct Contact {
    pub slot: TouchSlot,
    pub position: Point<f64, Logical>,
}

pub(super) struct Held {
    pub id: DeviceId,
    pub keys: HashSet<Keycode>,
    pub buttons: HashSet<u32>,
    pub contacts: HashMap<TouchSlot, Contact>,
}

struct Entry {
    device: Box<dyn Any>,
    held: Held,
}

#[derive(Default)]
pub(crate) struct PhysicalInput {
    // Exact device equality distinguishes a replacement from an old handle
    // even when both have the same sysname. Only live devices are retained.
    entries: Vec<Entry>,
    announced: HashSet<TypeId>,
    gestures: [Option<DeviceId>; 3],
    next_touch: u32,
}

impl PhysicalInput {
    fn find<D: Device + 'static>(&self, device: &D) -> Option<usize> {
        self.entries.iter().position(|entry| {
            entry
                .device
                .downcast_ref::<D>()
                .is_some_and(|registered| registered == device)
        })
    }

    fn insert<D: Device + 'static>(&mut self, device: D) -> DeviceId {
        if let Some(index) = self.find(&device) {
            return self.entries[index].held.id;
        }
        let id = DeviceId(KeyboardSource::new_auxiliary());
        self.entries.push(Entry {
            device: Box::new(device),
            held: Held {
                id,
                keys: HashSet::new(),
                buttons: HashSet::new(),
                contacts: HashMap::new(),
            },
        });
        id
    }

    pub(super) fn add<D: Device + 'static>(&mut self, device: D) -> DeviceId {
        self.announced.insert(TypeId::of::<D>());
        self.insert(device)
    }

    pub(super) fn resolve<D: Device + 'static>(&mut self, device: D) -> Option<DeviceId> {
        if let Some(index) = self.find(&device) {
            return Some(self.entries[index].held.id);
        }
        // Some synthetic backends have no lifecycle events. Once a backend
        // announces devices, unknown handles must wait for an explicit add.
        (!self.announced.contains(&TypeId::of::<D>())).then(|| self.insert(device))
    }

    pub(super) fn remove<D: Device + 'static>(&mut self, device: &D) -> Option<Held> {
        self.announced.insert(TypeId::of::<D>());
        let index = self.find(device)?;
        Some(self.entries.swap_remove(index).held)
    }

    fn held_mut(&mut self, id: DeviceId) -> &mut Held {
        &mut self
            .entries
            .iter_mut()
            .find(|entry| entry.held.id == id)
            .expect("input routing retains its registered device")
            .held
    }

    pub(super) fn key(&mut self, id: DeviceId, key: Keycode, state: KeyState) -> KeyTransition {
        let held = self.held_mut(id);
        match state {
            KeyState::Pressed if held.keys.insert(key) => KeyTransition::New,
            KeyState::Pressed => KeyTransition::Repeat,
            KeyState::Released if held.keys.remove(&key) => KeyTransition::New,
            KeyState::Released => KeyTransition::Ignore,
        }
    }

    pub(super) fn button(&mut self, id: DeviceId, button: u32, state: ButtonState) -> bool {
        let was_held = self.button_held(button);
        let held = self.held_mut(id);
        match state {
            ButtonState::Pressed => held.buttons.insert(button) && !was_held,
            ButtonState::Released => {
                if !held.buttons.remove(&button) {
                    return false;
                }
                !self.button_held(button)
            }
        }
    }

    pub(super) fn button_held(&self, button: u32) -> bool {
        self.entries
            .iter()
            .any(|entry| entry.held.buttons.contains(&button))
    }

    pub(super) fn begin_contact(
        &mut self,
        id: DeviceId,
        local: TouchSlot,
        position: Point<f64, Logical>,
    ) -> Option<TouchSlot> {
        if self.held_mut(id).contacts.contains_key(&local) {
            return None;
        }
        // Linux touch slots are per device; Wayland touch IDs are per seat.
        // Reuse a free ID after wrap without colliding with a live contact.
        let occupied: HashSet<_> = self
            .entries
            .iter()
            .flat_map(|entry| entry.held.contacts.values().map(|contact| contact.slot))
            .collect();
        if occupied.len() >= 1024 {
            return None;
        }
        let slot = loop {
            let slot = Some(self.next_touch).into();
            self.next_touch = self.next_touch.wrapping_add(1) & i32::MAX as u32;
            if !occupied.contains(&slot) {
                break slot;
            }
        };
        self.held_mut(id)
            .contacts
            .insert(local, Contact { slot, position });
        Some(slot)
    }

    pub(super) fn contact(
        &mut self,
        id: DeviceId,
        local: TouchSlot,
        position: Option<Point<f64, Logical>>,
    ) -> Option<TouchSlot> {
        let contact = self.held_mut(id).contacts.get_mut(&local)?;
        if let Some(position) = position {
            contact.position = position;
        }
        Some(contact.slot)
    }

    pub(super) fn end_contact(&mut self, id: DeviceId, local: TouchSlot) -> Option<Contact> {
        self.held_mut(id).contacts.remove(&local)
    }

    pub(super) fn take_contacts(&mut self, id: DeviceId) -> HashMap<TouchSlot, Contact> {
        std::mem::take(&mut self.held_mut(id).contacts)
    }

    pub(super) fn gesture_owner(&self, kind: Gesture) -> Option<DeviceId> {
        self.gestures[kind as usize]
    }

    pub(super) fn set_gesture_owner(&mut self, kind: Gesture, id: Option<DeviceId>) {
        self.gestures[kind as usize] = id;
    }

    pub(crate) fn is_neutral(&self) -> bool {
        self.gestures.iter().all(Option::is_none)
            && self.entries.iter().all(|entry| {
                entry.held.keys.is_empty()
                    && entry.held.buttons.is_empty()
                    && entry.held.contacts.is_empty()
            })
    }

    pub(super) fn take_all(&mut self) -> Vec<Held> {
        self.entries
            .iter_mut()
            .map(|entry| Held {
                id: entry.held.id,
                keys: std::mem::take(&mut entry.held.keys),
                buttons: std::mem::take(&mut entry.held.buttons),
                contacts: std::mem::take(&mut entry.held.contacts),
            })
            .collect()
    }
}

mod routing;

#[cfg(test)]
mod tests;
