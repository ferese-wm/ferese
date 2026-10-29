use std::collections::{HashMap, VecDeque};
use std::time::Instant;

use serde::Deserialize;
use serde_json::{Value, json};
use smithay::input::keyboard::{Keycode, ModifiersState};

use crate::config::{Binding, InputSettings};

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Shortcut {
    pub id: String,
    pub trigger: String,
    #[serde(skip)]
    num_required: bool,
}

struct Registration {
    shortcuts: Vec<(Shortcut, Binding)>,
    held: HashMap<Keycode, String>,
    events: VecDeque<Value>,
    // Portal timestamps use milliseconds. Keep the input clock's u64 base;
    // Wayland's u32 millisecond timestamps wrap after about 49 days.
    latest: (u64, Instant),
    closed: bool,
}

impl Registration {
    fn timestamp(&self) -> u64 {
        self.latest
            .0
            .saturating_add(self.latest.1.elapsed().as_millis() as u64)
    }

    fn release_all(&mut self) {
        let timestamp = self.timestamp();
        let id_iter = self.held.drain().map(|(_, id)| id);
        for id in id_iter {
            self.events
                .push_back(json!({"id":id,"active":false,"timestamp":timestamp}));
        }
    }
}

#[derive(Default)]
pub(crate) struct PortalShortcuts(HashMap<u64, Registration>);

impl PortalShortcuts {
    pub(crate) fn register(
        &mut self,
        owner: u64,
        shortcuts: Vec<Shortcut>,
        configured: &[Binding],
        input: &InputSettings,
    ) -> Result<(), String> {
        if shortcuts.len() > 64 || (!self.0.contains_key(&owner) && self.0.len() >= 16) {
            return Err("Shortcut limit reached".into());
        }
        let keymap = crate::config::physical_keymap(input).map_err(|error| error.to_string())?;
        if self
            .0
            .get(&owner)
            .is_some_and(|registration| registration.closed)
        {
            return Err("Shortcut session has ended".into());
        }
        let mut parsed: Vec<(Shortcut, Binding)> = Vec::new();
        for mut shortcut in shortcuts {
            shortcut.num_required = shortcut
                .trigger
                .split('+')
                .any(|part| part.trim().eq_ignore_ascii_case("num"));
            if shortcut.id.is_empty()
                || shortcut.id.len() > 256
                || shortcut.id.chars().any(char::is_control)
            {
                return Err("Invalid shortcut identifier".into());
            }
            let binding =
                Binding::portal_trigger(&shortcut.trigger).map_err(|error| error.to_string())?;
            if configured
                .iter()
                .any(|other| binding.conflicts(other, &keymap))
                || parsed
                    .iter()
                    .any(|(other, key)| other.id == shortcut.id || binding.conflicts(key, &keymap))
                || self.0.iter().any(|(connection, registration)| {
                    *connection != owner
                        && registration
                            .shortcuts
                            .iter()
                            .any(|(_, other)| binding.conflicts(other, &keymap))
                })
            {
                return Err(format!("Shortcut {} is already in use", shortcut.trigger));
            }
            parsed.push((shortcut, binding));
        }
        let registration = self.0.entry(owner).or_insert_with(|| Registration {
            shortcuts: Vec::new(),
            held: HashMap::new(),
            events: VecDeque::new(),
            latest: (0, Instant::now()),
            closed: false,
        });
        registration.release_all();
        registration.shortcuts = parsed;
        Ok(())
    }

    pub(crate) fn remove(&mut self, owner: u64) {
        self.0.remove(&owner);
    }

    pub(crate) fn reconcile(&mut self, configured: &[Binding], input: &InputSettings) {
        let Ok(keymap) = crate::config::physical_keymap(input) else {
            for registration in self.0.values_mut() {
                registration.closed = true;
            }
            return;
        };
        for registration in self.0.values_mut() {
            registration.shortcuts.retain(|(_, binding)| {
                !configured.iter().any(|key| binding.conflicts(key, &keymap))
            });
            let active_ids = registration
                .shortcuts
                .iter()
                .map(|(shortcut, _)| shortcut.id.as_str())
                .collect::<Vec<_>>();
            let timestamp = registration.timestamp();
            registration.held.retain(|_, id| {
                if active_ids.contains(&id.as_str()) {
                    true
                } else {
                    registration
                        .events
                        .push_back(json!({"id":id,"active":false,"timestamp":timestamp}));
                    false
                }
            });
        }
    }

    pub(crate) fn poll(&mut self, owner: u64, locked: bool) -> Value {
        let Some(registration) = self.0.get_mut(&owner) else {
            return json!({"closed":true});
        };
        if registration.closed {
            return json!({"closed":true});
        }
        if locked {
            registration.release_all();
        }
        json!({
            "shortcuts": registration.shortcuts.iter().map(|(shortcut, _)| json!({"id":shortcut.id,"trigger":shortcut.trigger})).collect::<Vec<_>>(),
            "events": registration.events.drain(..).collect::<Vec<_>>()
        })
    }

    pub(crate) fn press(
        &mut self,
        key: Keycode,
        symbols: &[u32],
        modifiers: &ModifiersState,
        timestamp: u64,
    ) -> bool {
        for registration in self.0.values_mut() {
            let Some((shortcut, _)) = registration.shortcuts.iter().find(|(shortcut, binding)| {
                (!shortcut.num_required || modifiers.num_lock)
                    && binding.matches(
                        key,
                        symbols,
                        modifiers.logo,
                        modifiers.ctrl,
                        modifiers.alt,
                        modifiers.shift,
                    )
            }) else {
                continue;
            };
            if !registration.held.contains_key(&key) {
                if registration.events.len() >= 256 {
                    registration.closed = true;
                    registration.shortcuts.clear();
                    registration.held.clear();
                    registration.events.clear();
                    return false;
                }
                registration.latest = (timestamp, Instant::now());
                registration.held.insert(key, shortcut.id.clone());
                registration
                    .events
                    .push_back(json!({"id":shortcut.id,"active":true,"timestamp":timestamp}));
            }
            return true;
        }
        false
    }

    pub(crate) fn release(&mut self, key: Keycode, timestamp: u64) {
        for registration in self.0.values_mut() {
            if let Some(id) = registration.held.remove(&key) {
                registration.latest = (timestamp, Instant::now());
                registration
                    .events
                    .push_back(json!({"id":id,"active":false,"timestamp":timestamp}));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use smithay::backend::input::InputTime;
    use smithay::input::keyboard::keysyms;

    use super::*;

    fn shortcut(id: &str, trigger: &str) -> Shortcut {
        Shortcut {
            id: id.into(),
            trigger: trigger.into(),
            num_required: false,
        }
    }

    #[test]
    fn registrations_are_atomic_and_connection_scoped() {
        let input = crate::config::Config::parse_source("")
            .unwrap()
            .input_settings()
            .unwrap();
        let configured = vec![Binding::portal_trigger("Super+q").unwrap()];
        let mut shortcuts = PortalShortcuts::default();
        shortcuts
            .register(1, vec![shortcut("one", "Ctrl+Alt+a")], &configured, &input)
            .unwrap();
        assert!(
            shortcuts
                .register(2, vec![shortcut("two", "Ctrl+Alt+a")], &configured, &input)
                .is_err()
        );
        assert!(
            shortcuts
                .register(1, vec![shortcut("one", "Super+q")], &configured, &input)
                .is_err()
        );
        assert_eq!(
            shortcuts.poll(1, false)["shortcuts"][0]["trigger"],
            "Ctrl+Alt+a"
        );
        shortcuts.remove(1);
        assert_eq!(shortcuts.poll(1, false)["closed"], true);
        shortcuts
            .register(2, vec![shortcut("two", "Ctrl+Alt+a")], &configured, &input)
            .unwrap();
    }

    #[test]
    fn releases_follow_the_pressed_key_and_lock_releases_held_shortcuts() {
        let mut shortcuts = PortalShortcuts::default();
        shortcuts
            .register(
                1,
                vec![shortcut("one", "Ctrl+Alt+a")],
                &[],
                &crate::config::Config::parse_source("")
                    .unwrap()
                    .input_settings()
                    .unwrap(),
            )
            .unwrap();
        let key = Keycode::new(38);
        let modifiers = ModifiersState {
            ctrl: true,
            alt: true,
            ..ModifiersState::default()
        };
        assert!(shortcuts.press(key, &[keysyms::KEY_a], &modifiers, 10));
        assert!(shortcuts.press(key, &[keysyms::KEY_a], &modifiers, 11));
        shortcuts.release(key, 12);
        let result = shortcuts.poll(1, false);
        assert_eq!(result["events"].as_array().unwrap().len(), 2);
        assert_eq!(result["events"][1]["active"], false);
        assert!(shortcuts.press(key, &[keysyms::KEY_a], &modifiers, 13));
        assert_eq!(shortcuts.poll(1, true)["events"][1]["active"], false);
    }

    #[test]
    fn configuration_changes_revoke_conflicting_shortcuts() {
        let mut shortcuts = PortalShortcuts::default();
        let input = crate::config::Config::parse_source("")
            .unwrap()
            .input_settings()
            .unwrap();
        shortcuts
            .register(1, vec![shortcut("one", "Ctrl+Alt+a")], &[], &input)
            .unwrap();
        shortcuts.reconcile(&[Binding::portal_trigger("Ctrl+Alt+a").unwrap()], &input);
        assert!(
            shortcuts.poll(1, false)["shortcuts"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        assert!(Binding::portal_trigger("Ctrl+Alt+Escape").is_err());
        assert!(Binding::portal_trigger("Ctrl+Alt+F3").is_err());
    }

    #[test]
    fn queue_overflow_closes_the_session_instead_of_leaving_held_keys() {
        let mut shortcuts = PortalShortcuts::default();
        let input = crate::config::Config::parse_source("")
            .unwrap()
            .input_settings()
            .unwrap();
        shortcuts
            .register(1, vec![shortcut("one", "Ctrl+Alt+a")], &[], &input)
            .unwrap();
        let key = Keycode::new(38);
        let modifiers = ModifiersState {
            ctrl: true,
            alt: true,
            ..ModifiersState::default()
        };
        for timestamp in 0..200 {
            shortcuts.press(key, &[keysyms::KEY_a], &modifiers, timestamp * 2);
            shortcuts.release(key, timestamp * 2 + 1);
        }
        assert_eq!(shortcuts.poll(1, false)["closed"], true);
        assert!(!shortcuts.press(key, &[keysyms::KEY_a], &modifiers, 500));
    }

    #[test]
    fn physical_conflicts_include_secondary_layouts_and_num_modifier_is_honored() {
        let config = crate::config::Config::parse_source(
            "binding keys=\"Ctrl+AD01\" match=\"physical\" action=\"none\"\n",
        )
        .unwrap();
        let mut input = config.input_settings().unwrap();
        input.xkb_layout = "us,fr".into();
        let configured = config.bindings(&input).unwrap();
        let mut shortcuts = PortalShortcuts::default();
        assert!(
            shortcuts
                .register(1, vec![shortcut("one", "Ctrl+a")], &configured, &input)
                .is_err()
        );
        shortcuts
            .register(1, vec![shortcut("one", "NUM+Ctrl+z")], &[], &input)
            .unwrap();
        let key = Keycode::new(52);
        let mut modifiers = ModifiersState {
            ctrl: true,
            ..ModifiersState::default()
        };
        assert!(!shortcuts.press(key, &[keysyms::KEY_z], &modifiers, 1));
        modifiers.num_lock = true;
        assert!(shortcuts.press(key, &[keysyms::KEY_z], &modifiers, 2));
    }

    #[test]
    fn input_and_session_release_timestamps_use_64_bit_milliseconds() {
        use crate::input::binding_mode_tests::key_at;

        if !crate::startup_tests::private_runtime(
            "portal_shortcuts::tests::input_and_session_release_timestamps_use_64_bit_milliseconds",
        ) {
            return;
        }
        let mut events = smithay::reexports::calloop::EventLoop::try_new().unwrap();
        let mut state = crate::startup_tests::state(&mut events);
        state
            .portal_shortcuts
            .register(1, vec![shortcut("one", "F10")], &[], &state.input_settings)
            .unwrap();
        // Longer than a 32-bit Wayland millisecond timestamp. The portal uses
        // u64 time and must preserve this clock's base and millisecond deltas.
        let press = (u64::from(u32::MAX) + 1) * 1000 + 123_000;
        let release = press + 456_789;
        key_at(&mut state, 76, true, InputTime::from_micros(press));
        key_at(&mut state, 76, false, InputTime::from_micros(release));
        let result = state.portal_shortcuts.poll(1, false);
        assert_eq!(result["events"][0]["timestamp"], press / 1000);
        assert_eq!(result["events"][1]["timestamp"], release / 1000);
        assert_eq!(
            result["events"][1]["timestamp"].as_u64().unwrap()
                - result["events"][0]["timestamp"].as_u64().unwrap(),
            456
        );

        key_at(&mut state, 76, true, InputTime::now());
        state.portal_shortcuts.poll(1, false);
        let before = InputTime::now().micros() / 1000;
        state.reset_session_input();
        let after = InputTime::now().micros() / 1000;
        let result = state.portal_shortcuts.poll(1, false);
        let timestamp = result["events"][0]["timestamp"].as_u64().unwrap();
        assert_eq!(result["events"][0]["active"], false);
        assert!((before..=after).contains(&timestamp));
    }
}
