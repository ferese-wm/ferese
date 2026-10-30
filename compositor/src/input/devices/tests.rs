use super::*;

mod integration;

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
struct TestDevice(u32);

impl Device for TestDevice {
    fn id(&self) -> String {
        "event-test".into()
    }
    fn name(&self) -> String {
        format!("test device {}", self.0)
    }
    fn has_capability(&self, _: smithay::backend::input::DeviceCapability) -> bool {
        true
    }
    fn usb_id(&self) -> Option<(u32, u32)> {
        None
    }
    fn syspath(&self) -> Option<std::path::PathBuf> {
        None
    }
}

#[test]
fn neutral_input_requires_released_keys_buttons_contacts_and_gestures() {
    let mut devices = PhysicalInput::default();
    let id = devices.add(TestDevice(1));
    assert!(devices.is_neutral());
    let key = Keycode::new(42);
    devices.key(id, key, KeyState::Pressed);
    assert!(!devices.is_neutral());
    devices.key(id, key, KeyState::Released);
    assert!(devices.is_neutral());
    devices.button(id, 0x110, ButtonState::Pressed);
    assert!(!devices.is_neutral());
    devices.button(id, 0x110, ButtonState::Released);
    assert!(devices.is_neutral());
    devices
        .begin_contact(id, Some(0).into(), (1., 1.).into())
        .unwrap();
    assert!(!devices.is_neutral());
    devices.end_contact(id, Some(0).into()).unwrap();
    assert!(devices.is_neutral());
    for kind in [Gesture::Swipe, Gesture::Pinch, Gesture::Hold] {
        devices.set_gesture_owner(kind, Some(id));
        assert!(!devices.is_neutral());
        devices.set_gesture_owner(kind, None);
        assert!(devices.is_neutral());
    }
}

#[test]
fn replaced_handles_do_not_reuse_keyboard_source_or_accept_stale_events() {
    let mut devices = PhysicalInput::default();
    let old = devices.add(TestDevice(1));
    devices.remove(&TestDevice(1)).unwrap();
    let replacement = devices.add(TestDevice(2));
    assert_ne!(old, replacement);
    assert!(devices.resolve(TestDevice(1)).is_none());
    assert!(devices.remove(&TestDevice(1)).is_none());
    assert_eq!(devices.resolve(TestDevice(2)), Some(replacement));
}

#[test]
fn buttons_and_touch_slots_have_independent_device_ownership() {
    let mut devices = PhysicalInput::default();
    let first = devices.add(TestDevice(1));
    let second = devices.add(TestDevice(2));
    assert!(devices.button(first, 0x110, ButtonState::Pressed));
    assert!(!devices.button(second, 0x110, ButtonState::Pressed));
    let a = devices
        .begin_contact(first, Some(0).into(), (1., 1.).into())
        .unwrap();
    let b = devices
        .begin_contact(second, Some(0).into(), (2., 2.).into())
        .unwrap();
    assert_ne!(a, b);
    let removed = devices.remove(&TestDevice(1)).unwrap();
    assert_eq!(removed.contacts.len(), 1);
    assert!(devices.button_held(0x110));
    assert_eq!(devices.contact(second, Some(0).into(), None), Some(b));
    assert!(devices.button(second, 0x110, ButtonState::Released));
    assert!(!devices.button(second, 0x110, ButtonState::Released));
}
