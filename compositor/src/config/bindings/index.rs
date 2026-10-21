use super::*;

/// Immutable bindings and their keyboard index are replaced together on reload.
/// The ordered slice remains available for gestures and the shortcut guide.
#[derive(Clone)]
pub(crate) struct BindingSet {
    ordered: Vec<Binding>,
    keyboard: HashMap<BindingIdentity, usize>,
}

impl From<Vec<Binding>> for BindingSet {
    fn from(ordered: Vec<Binding>) -> Self {
        let mut keyboard = HashMap::with_capacity(ordered.len());

        for (index, binding) in ordered.iter().enumerate() {
            let identity = binding_identity_for_runtime(binding);

            if identity.match_mode != BindingMatch::Swipe {
                keyboard.entry(identity).or_insert(index);
            }
        }

        Self { ordered, keyboard }
    }
}

impl std::ops::Deref for BindingSet {
    type Target = [Binding];

    fn deref(&self) -> &Self::Target {
        &self.ordered
    }
}

impl BindingSet {
    pub(crate) fn binding_for_key(
        &self,
        keycode: Keycode,
        symbols: &[u32],
        logo: bool,
        ctrl: bool,
        alt: bool,
        shift: bool,
    ) -> Option<&Binding> {
        let modifiers = BindingModifiers {
            logo,
            ctrl,
            alt,
            shift,
        };
        let physical = self.keyboard.get(&BindingIdentity {
            modifiers,
            match_mode: BindingMatch::Physical,
            key: keycode.raw(),
        });
        let symbolic = symbols.iter().filter_map(|&key| {
            self.keyboard.get(&BindingIdentity {
                modifiers,
                match_mode: BindingMatch::Keysym,
                key,
            })
        });
        // Preserve list precedence across physical keys and all raw symbols;
        // neither hash iteration order nor symbol order determines the winner.
        physical
            .into_iter()
            .chain(symbolic)
            .min()
            .map(|&index| &self.ordered[index])
    }

    #[cfg(test)]
    pub(crate) fn action_for_key(
        &self,
        keycode: Keycode,
        symbols: &[u32],
        logo: bool,
        ctrl: bool,
        alt: bool,
        shift: bool,
    ) -> Option<&BindingAction> {
        self.binding_for_key(keycode, symbols, logo, ctrl, alt, shift)
            .map(|binding| &binding.action)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn binding_for(trigger: BindingTrigger, action: BindingAction) -> Binding {
        Binding {
            modifiers: BindingModifiers::default(),
            trigger,
            action,
        }
    }

    #[test]
    fn keyboard_index_preserves_order_across_symbols_and_physical_keys() {
        let code = Keycode::new(38);
        let bindings = vec![
            binding_for(BindingTrigger::Keysym(keysyms::KEY_b), BindingAction::Close),
            binding_for(
                BindingTrigger::Physical(code),
                BindingAction::ToggleFloating,
            ),
            binding_for(BindingTrigger::Keysym(keysyms::KEY_a), BindingAction::Exit),
            binding_for(BindingTrigger::Keysym(keysyms::KEY_b), BindingAction::None),
        ];

        for ordered in [bindings.clone(), bindings.into_iter().rev().collect()] {
            let index = BindingSet::from(ordered);

            for symbols in [
                vec![keysyms::KEY_a, keysyms::KEY_b],
                vec![keysyms::KEY_b, keysyms::KEY_a],
                vec![],
            ] {
                let expected = index
                    .iter()
                    .find(|binding| binding.matches(code, &symbols, false, false, false, false));
                assert_eq!(
                    index.action_for_key(code, &symbols, false, false, false, false),
                    expected.map(|binding| &binding.action)
                );
            }
        }
    }

    #[test]
    fn indexed_lookup_matches_linear_lookup_for_all_modifier_combinations() {
        let config = Config::default();
        let mut bindings = config.bindings(&config.input_settings().unwrap()).unwrap();
        bindings.extend([
            binding_for(
                BindingTrigger::Physical(Keycode::new(38)),
                BindingAction::None,
            ),
            binding_for(BindingTrigger::Keysym(keysyms::KEY_a), BindingAction::Close),
            binding_for(
                BindingTrigger::Swipe {
                    fingers: 3,
                    direction: crate::gestures::SwipeDirection::Left,
                },
                BindingAction::Focus(Direction::Left),
            ),
        ]);
        let index = BindingSet::from(bindings);

        for bits in 0..16 {
            let (logo, ctrl, alt, shift) =
                (bits & 1 != 0, bits & 2 != 0, bits & 4 != 0, bits & 8 != 0);

            for code in 8..100 {
                for symbols in [
                    vec![],
                    vec![keysyms::KEY_Return],
                    vec![keysyms::KEY_b, keysyms::KEY_a],
                    vec![keysyms::KEY_a, keysyms::KEY_Tab],
                    vec![keysyms::KEY_NoSymbol],
                ] {
                    let code = Keycode::new(code);
                    let expected = index
                        .iter()
                        .find(|binding| binding.matches(code, &symbols, logo, ctrl, alt, shift));
                    assert_eq!(
                        index.action_for_key(code, &symbols, logo, ctrl, alt, shift),
                        expected.map(|binding| &binding.action)
                    );
                }
            }
        }
    }

    #[test]
    fn replacing_bindings_discards_old_index_entries() {
        let code = Keycode::new(38);
        let mut bindings = BindingSet::from(vec![binding_for(
            BindingTrigger::Physical(code),
            BindingAction::Close,
        )]);
        assert_eq!(
            bindings.action_for_key(code, &[], false, false, false, false),
            Some(&BindingAction::Close)
        );
        bindings = BindingSet::from(vec![binding_for(
            BindingTrigger::Keysym(keysyms::KEY_b),
            BindingAction::None,
        )]);
        assert_eq!(
            bindings.action_for_key(code, &[], false, false, false, false),
            None
        );
        assert_eq!(
            bindings.action_for_key(code, &[keysyms::KEY_b], false, false, false, false),
            Some(&BindingAction::None)
        );
    }

    #[test]
    fn cloning_spawn_actions_shares_the_command_storage() {
        let action =
            BindingAction::Spawn(vec!["foot".into(), "--app-id".into(), "work".into()].into());
        let cloned = action.clone();
        let (BindingAction::Spawn(original), BindingAction::Spawn(cloned)) = (&action, cloned)
        else {
            unreachable!();
        };
        assert!(Arc::ptr_eq(original, &cloned));
        assert_eq!(original[0], "foot");
        assert_eq!(original.len(), 3);
    }
}
