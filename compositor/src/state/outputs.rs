use super::*;

impl Ferese {
    pub(crate) fn defer_output_redraw(&mut self, output: Output) {
        if !self.output_redraw_pending.contains(&output) {
            self.output_redraw_pending.push(output);
        }
    }

    pub(crate) fn output_by_identity(&self, identity: &str) -> Option<&Output> {
        self.output_identity_ids
            .get(identity)
            .and_then(|id| self.outputs_by_id.get(id))
    }

    pub(crate) fn persistent_output_id(&self, identity: &str) -> Option<OutputId> {
        self.output_identity_ids.get(identity).copied()
    }

    pub(crate) fn ensure_output_identity(&mut self, identity: &str) -> OutputId {
        *self.output_identity_ids.entry(identity.to_owned()).or_insert_with(|| {
            let id = OutputId(self.next_output_id);
            self.next_output_id = self.next_output_id.checked_add(1).expect("output ID space exhausted");
            id
        })
    }

    pub fn register_output(&mut self, output: &Output, identity: String) {
        assert!(
            self.desktop_transition.is_none(),
            "backend must publish its complete inventory"
        );
        let Some(geometry) = self.space.output_geometry(output) else {
            tracing::error!(output = %output.name(), "cannot register an unmapped output");
            return;
        };
        let mut runtime = self.current_desktop_outputs();
        runtime.retain(|entry| entry.output != *output);
        runtime.push(DesktopOutput {
            output: output.clone(),
            identity,
            mode: output.current_mode().expect("mapped output has a mode"),
            transform: output.current_transform(),
            scale: output.current_scale(),
            position: geometry.loc,
        });
        self.begin_desktop_transition();
        let changes = self
            .publish_desktop(runtime)
            .expect("valid registered output inventory");
        self.finish_desktop_transition(changes);
    }

    pub(crate) fn current_desktop_outputs(&self) -> Vec<DesktopOutput> {
        self.output_ids
            .iter()
            .filter_map(|(output, id)| {
                Some(DesktopOutput {
                    output: output.clone(),
                    identity: self
                        .output_identity_ids
                        .iter()
                        .find(|(_, candidate)| *candidate == id)?
                        .0
                        .clone(),
                    mode: output.current_mode()?,
                    transform: output.current_transform(),
                    scale: output.current_scale(),
                    position: self.space.output_geometry(output)?.loc,
                })
            })
            .collect()
    }

    /// Resource invalidation is immediate; ownership and focus wait for the
    /// complete usable inventory, including when the final output disappears.
    pub fn unregister_output(&mut self, output: &Output) {
        let standalone = self.desktop_transition.is_none();
        if standalone {
            self.begin_desktop_transition();
        }
        self.retire_output_resources(output);
        if standalone {
            let runtime = self
                .current_desktop_outputs()
                .into_iter()
                .filter(|entry| entry.output != *output)
                .collect();
            let changes = self.publish_desktop(runtime).expect("valid surviving output inventory");
            self.finish_desktop_transition(changes);
        }
    }

    pub(super) fn retire_output_resources(&mut self, output: &Output) {
        if let Some(transition) = self.desktop_transition.as_mut()
            && !transition.retired.insert(output.clone())
        {
            return;
        }
        self.display_presentation.remove_output(output);
        self.output_redraw_pending.retain(|pending| pending != output);
        if let Some(id) = self.output_ids.get(output) {
            self.render.remove_output(*id);
            self.capture_render.remove_output(*id);
        }
        self.pending_screencopies.retain(|capture| {
            if capture.output == *output {
                capture.fail();
                false
            } else {
                true
            }
        });
        self.session_lock.output_removed(output);
    }

    pub(super) fn activate_output_workspace(&mut self, output: OutputId, workspace: ferese_core::WorkspaceId) {
        if let Err(error) = self.output_workspaces.focus_output(output) {
            tracing::error!(%error, "failed to focus output");
            return;
        }

        match self.workspaces.activate(workspace) {
            Ok(focus) => self.focused_window = focus,
            Err(error) => tracing::error!(%error, "failed to activate output workspace"),
        }
    }

    pub(crate) fn focused_output(&self) -> Option<&Output> {
        let focused = self.output_workspaces.focused_output()?;
        self.outputs_by_id.get(&focused)
    }

    pub(crate) fn restore_output_focus(&mut self) {
        if let Some(output) = self.output_workspaces.focused_output()
            && let Some(workspace) = self.output_workspaces.active_workspace(output)
        {
            self.activate_output_workspace(output, workspace);
            self.restore_keyboard_focus();
        }
    }

    pub(crate) fn output_id(&self, output: &Output) -> Option<OutputId> {
        self.output_ids.get(output).copied()
    }

    pub(crate) fn focus_output_at(&mut self, position: Point<f64, Logical>) {
        let Some(output) = self.space.output_under(position).next().cloned() else {
            return;
        };
        let Some(output_id) = self.output_ids.get(&output).copied() else {
            return;
        };
        let Some(workspace) = self.output_workspaces.active_workspace(output_id) else {
            return;
        };
        if self.output_workspaces.focused_output() == Some(output_id) && self.workspaces.active_id() == workspace {
            return;
        }

        self.activate_output_workspace(output_id, workspace);
        self.relayout();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use smithay::backend::input::{ButtonState, Event, InputBackend, InputEvent, PointerButtonEvent, UnusedEvent};
    use smithay::backend::winit::WinitVirtualDevice;
    use smithay::input::pointer::MotionEvent;
    use smithay::utils::SERIAL_COUNTER;

    #[derive(Debug)]
    struct TestInput;

    impl InputBackend for TestInput {
        type Device = WinitVirtualDevice;
        type PointerButtonEvent = TestButton;
        type KeyboardKeyEvent = UnusedEvent;
        type PointerAxisEvent = UnusedEvent;
        type PointerMotionEvent = UnusedEvent;
        type PointerMotionAbsoluteEvent = UnusedEvent;
        type GestureSwipeBeginEvent = UnusedEvent;
        type GestureSwipeUpdateEvent = UnusedEvent;
        type GestureSwipeEndEvent = UnusedEvent;
        type GesturePinchBeginEvent = UnusedEvent;
        type GesturePinchUpdateEvent = UnusedEvent;
        type GesturePinchEndEvent = UnusedEvent;
        type GestureHoldBeginEvent = UnusedEvent;
        type GestureHoldEndEvent = UnusedEvent;
        type TouchDownEvent = UnusedEvent;
        type TouchUpEvent = UnusedEvent;
        type TouchMotionEvent = UnusedEvent;
        type TouchCancelEvent = UnusedEvent;
        type TouchFrameEvent = UnusedEvent;
        type TabletToolAxisEvent = UnusedEvent;
        type TabletToolProximityEvent = UnusedEvent;
        type TabletToolTipEvent = UnusedEvent;
        type TabletToolButtonEvent = UnusedEvent;
        type SwitchToggleEvent = UnusedEvent;
        type SpecialEvent = UnusedEvent;
    }

    #[derive(Debug)]
    struct TestButton(ButtonState);

    impl Event<TestInput> for TestButton {
        fn time(&self) -> u64 {
            1_000
        }
        fn device(&self) -> WinitVirtualDevice {
            WinitVirtualDevice
        }
    }

    impl PointerButtonEvent<TestInput> for TestButton {
        fn button_code(&self) -> u32 {
            0x110
        }
        fn state(&self) -> ButtonState {
            self.0
        }
    }

    fn click(state: &mut Ferese, position: Point<f64, Logical>) {
        let pointer = state.seat.get_pointer().unwrap();
        pointer.motion(
            state,
            state.surface_under(position),
            &MotionEvent {
                location: position,
                serial: SERIAL_COUNTER.next_serial(),
                time: 1,
            },
        );
        pointer.frame(state);

        for button in [ButtonState::Pressed, ButtonState::Released] {
            state.process_input_event::<TestInput>(InputEvent::PointerButton {
                event: TestButton(button),
            });
        }
    }

    #[test]
    #[ignore = "requires a private XDG_RUNTIME_DIR and permission to bind test sockets"]
    fn overlapping_outputs_agree_on_pointer_focus_and_overview_selection() {
        let runtime = std::path::PathBuf::from(std::env::var_os("XDG_RUNTIME_DIR").unwrap());
        assert!(runtime.starts_with(std::env::temp_dir()));

        let mut event_loop = EventLoop::try_new().unwrap();
        let config = crate::config::Config::default().runtime_config().unwrap();
        let mut state = Ferese::new(&mut event_loop, Display::new().unwrap(), config).unwrap();
        let mut outputs = Vec::new();

        for name in ["first", "second"] {
            let output = Output::new(
                name.into(),
                smithay::output::PhysicalProperties {
                    size: (0, 0).into(),
                    subpixel: smithay::output::Subpixel::Unknown,
                    make: "test".into(),
                    model: "test".into(),
                },
            );
            output.change_current_state(
                Some(smithay::output::Mode {
                    size: (1920, 1080).into(),
                    refresh: 60_000,
                }),
                Some(smithay::utils::Transform::Normal),
                Some(smithay::output::Scale::Fractional(1.75)),
                Some((0, 0).into()),
            );
            state.space.map_output(&output, (0, 0));
            state.register_output(&output, name.into());
            outputs.push(output);
        }

        let position = Point::from((300.0, 300.0));
        let expected_output = state.space.output_under(position).next().unwrap().clone();
        assert_eq!(expected_output, outputs[1]);
        let expected_workspace = state.workspace_under_pointer(position).unwrap();

        state.focus_output_at(position);
        assert_eq!(state.workspaces.active_id(), expected_workspace);
        assert_eq!(
            state.output_workspaces.focused_output(),
            state.output_id(&expected_output)
        );

        state
            .workspaces
            .insert_window(WindowId(1), Axis::Horizontal, 0.5)
            .unwrap();
        state.relayout();
        state.set_overview_active(true);
        let card = state
            .overview_workspace_cards(&expected_output)
            .into_iter()
            .find(|card| card.workspace != expected_workspace)
            .expect("occupied workspace has an empty successor");
        let position = Point::from((
            card.rect.x + card.rect.width / 2.0,
            card.rect.y + card.rect.height / 2.0,
        ));
        assert_eq!(state.overview_strip_at(position), Some(expected_output.clone()));
        click(&mut state, position);
        assert_eq!(state.workspaces.active_id(), card.workspace);
        state.windows.records.insert(
            WindowId(1),
            super::super::window_registry::WindowRecord {
                geometry: Some(WindowGeometry::new(Rect::new(0.0, 0.0, 800.0, 600.0), None)),
                ..Default::default()
            },
        );
        let occupied = state
            .overview_workspace_cards(&expected_output)
            .into_iter()
            .find(|card| card.workspace == expected_workspace)
            .unwrap();
        let preview = occupied.windows[0].1;
        let image_point = Point::from((preview.x + preview.width / 2.0, preview.y + preview.height / 2.0));
        let label_point = Point::from((
            occupied.rect.x + occupied.rect.width / 2.0,
            occupied.rect.y + occupied.rect.height - 8.0,
        ));

        for position in [image_point, label_point] {
            state.activate_managed_workspace(card.workspace);
            click(&mut state, position);
            assert_eq!(state.workspaces.active_id(), expected_workspace);
            assert!(state.overview.is_active());
        }

        assert_eq!(
            state.output_workspaces.focused_output(),
            state.output_id(&expected_output)
        );
    }
}
