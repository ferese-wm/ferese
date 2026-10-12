use super::*;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct BindingModeConfig {
    name: String,
    #[serde(default)]
    bindings: Vec<BindingConfig>,
}

/// Named keyboard maps share the ordinary binding index. The default map and
/// gestures remain owned by the compositor; a mode never mutates either.
#[derive(Clone, Default)]
pub(crate) struct BindingModes {
    maps: HashMap<String, BindingSet>,
    active: Option<String>,
}

impl BindingModes {
    pub(crate) fn active(&self) -> Option<&str> {
        self.active.as_deref()
    }

    pub(crate) fn set(&mut self, name: &str) -> bool {
        let next = if name == "default" {
            None
        } else if self.maps.contains_key(name) {
            Some(name.to_owned())
        } else {
            return false;
        };
        if next == self.active {
            return false;
        }
        self.active = next;
        true
    }

    pub(crate) fn bindings<'a>(&'a self, default: &'a BindingSet) -> &'a BindingSet {
        self.active
            .as_ref()
            .and_then(|name| self.maps.get(name))
            .unwrap_or(default)
    }
}

impl Config {
    pub(crate) fn binding_modes(
        &self,
        input: &InputSettings,
        default: &[Binding],
    ) -> Result<BindingModes, ConfigError> {
        let keymap = physical_keymap(input)?;
        let commands = self.binding_commands();
        let mut modes = BindingModes::default();
        for mode in &self.binding_modes {
            if mode.name.is_empty()
                || mode.name.len() > 48
                || mode.name == "default"
                || !mode
                    .name
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
                || modes.maps.contains_key(&mode.name)
            {
                return Err(ConfigError::Binding(format!(
                    "invalid or duplicate binding mode {:?}",
                    mode.name
                )));
            }
            let mut identities = HashSet::new();
            let mut bindings = Vec::new();
            for configured in &mode.bindings {
                let identity = binding_identity(configured, &keymap)?;
                if configured.disabled
                    || identity.match_mode == BindingMatch::Swipe
                    || !identities.insert(identity.clone())
                    || (identity.match_mode == BindingMatch::Keysym && identity.key == keysyms::KEY_Escape)
                    || (identity.match_mode == BindingMatch::Physical
                        && (0..keymap.num_layouts_for_key(Keycode::new(identity.key))).any(|layout| {
                            (0..keymap.num_levels_for_key(Keycode::new(identity.key), layout)).any(|level| {
                                keymap
                                    .key_get_syms_by_level(Keycode::new(identity.key), layout, level)
                                    .iter()
                                    .any(|symbol| symbol.raw() == keysyms::KEY_Escape)
                            })
                        }))
                {
                    return Err(ConfigError::Binding(format!(
                        "invalid, duplicate or reserved mode binding {:?}",
                        configured.keys
                    )));
                }
                bindings.push(parse_binding(configured, &commands, &keymap)?);
            }
            modes.maps.insert(mode.name.clone(), bindings.into());
        }
        for binding in default.iter().chain(modes.maps.values().flat_map(|map| map.iter())) {
            if let BindingAction::BindingMode(name) = &binding.action
                && name != "default"
                && !modes.maps.contains_key(name)
            {
                return Err(ConfigError::Binding(format!("unknown binding mode {name:?}")));
            }
        }
        Ok(modes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modes_use_the_binding_index_without_inheriting_default_shortcuts() {
        let config = Config::parse_source(
            r#"
            binding "Super+r" binding-mode resize
            binding-mode resize {
                binding h resize left
                binding t binding-mode launch
                binding AD02 resize up match="physical"
            }
            binding-mode launch { binding Return spawn terminal; }
        "#,
        )
        .unwrap();
        let runtime = config.runtime_config().unwrap();
        let default = BindingSet::from(runtime.bindings);
        let mut modes = runtime.binding_modes;
        assert!(modes.set("resize"));
        let selected = modes.bindings(&default);
        assert_eq!(
            selected.action_for_key(Keycode::new(43), &[keysyms::KEY_h], false, false, false, false),
            Some(&BindingAction::Resize(Direction::Left))
        );
        assert_eq!(
            selected.action_for_key(Keycode::new(25), &[], false, false, false, false),
            Some(&BindingAction::Resize(Direction::Up))
        );
        assert_eq!(
            selected.action_for_key(Keycode::new(43), &[keysyms::KEY_h], true, false, false, false),
            None
        );
        assert_eq!(
            selected.action_for_key(Keycode::new(36), &[keysyms::KEY_Return], true, false, false, false),
            None
        );
        assert!(!modes.set("missing"));
        assert_eq!(modes.active(), Some("resize"));
        assert!(modes.set("default"));
        assert!(matches!(
            modes.bindings(&default).action_for_key(
                Keycode::new(36),
                &[keysyms::KEY_Return],
                true,
                false,
                false,
                false
            ),
            Some(BindingAction::Spawn(_))
        ));
    }

    #[test]
    fn invalid_modes_and_targets_are_rejected_before_runtime_publication() {
        for source in [
            "binding-mode default {}",
            "binding-mode \"\" {}",
            "binding-mode \"two words\" {}",
            "binding-mode resize {}\nbinding-mode resize {}",
            "binding x binding-mode missing",
            "binding-mode resize { binding x binding-mode missing; }",
            "binding-mode resize { binding x close; binding x none; }",
            "binding-mode resize { binding Escape none; }",
            "binding-mode resize { binding Super+Escape none; }",
            "binding-mode resize { binding ESC none match=\"physical\"; }",
            "input { xkb-options \"caps:escape\"; }\nbinding-mode resize { binding CAPS none match=\"physical\"; }",
            "binding-mode resize { binding Swipe3Left none; }",
            "binding-mode resize { binding x disabled=#true; }",
            "binding-mode resize fallback=#true {}",
        ] {
            assert!(
                Config::parse_source(source)
                    .and_then(|config| config.runtime_config())
                    .is_err(),
                "{source}"
            );
        }
    }
}
