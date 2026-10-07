use super::*;

impl Ferese {
    pub(super) fn focus_candidates(&self) -> Vec<WindowId> {
        self.windows
            .ids()
            .iter()
            .filter_map(|(window, id)| {
                self.workspaces.workspace_for_window(*id)?;
                (window.toplevel().is_some()
                    && self.window_content_ready(window)
                    && self.windows.geometry(id).is_some()
                    && self.output_workspaces.focused_output().is_some())
                .then_some(*id)
            })
            .collect()
    }

    pub(crate) fn focus_preview_output(&self, id: WindowId) -> Option<OutputId> {
        self.workspaces
            .workspace_for_window(id)
            .and_then(|workspace| self.output_workspaces.output_for_workspace(workspace))
            .or_else(|| self.output_workspaces.focused_output())
    }

    pub(crate) fn focus_last_window(&mut self) {
        if self.session_lock.active() || self.input_capture.captures(1) {
            return;
        }
        self.cancel_focus_cycle();
        let target = self
            .focus_history
            .candidates(self.focused_window, self.focus_candidates())
            .into_iter()
            .find(|id| Some(*id) != self.focused_window);
        if let Some(id) = target {
            self.activate_managed_window(id);
        }
    }

    pub(crate) fn focus_floating(&mut self) {
        if self.session_lock.active() || self.input_capture.captures(1) {
            return;
        }
        let Some(current) = self.focused_window else {
            return;
        };
        let workspace = self.workspaces.active_id();
        if self.workspaces.workspace_for_window(current) != Some(workspace) {
            return;
        }
        let target = self
            .focus_history
            .candidates(self.focused_window, self.focus_candidates())
            .into_iter()
            .find(|id| {
                self.workspaces.workspace_for_window(*id) == Some(workspace)
                    && matches!(
                        (self.workspaces.placement(current), self.workspaces.placement(*id)),
                        (Some(WindowPlacement::Tiled), Some(WindowPlacement::Floating { .. }))
                            | (Some(WindowPlacement::Floating { .. }), Some(WindowPlacement::Tiled))
                    )
            });
        if let Some(id) = target {
            self.cancel_focus_cycle();
            self.activate_managed_window(id);
        }
    }

    pub(crate) fn cycle_focus(&mut self, reverse: bool, preview: bool) {
        if self.session_lock.active() || self.input_capture.captures(1) {
            return;
        }
        let available = self.focus_candidates();
        if !preview {
            self.cancel_focus_cycle();
            let order = self.focus_history.candidates(self.focused_window, &available);
            let mut cycle = ferese_core::FocusCycle::new(order, self.focused_window);
            if let Some(id) = cycle.advance(reverse, &available) {
                self.activate_managed_window(id);
            }
            return;
        }
        if self.focus_cycle.is_none() {
            let order = self.focus_history.candidates(self.focused_window, &available);
            if order.len() < 2 {
                return;
            }
            self.focus_cycle = Some(ferese_core::FocusCycle::new(order, self.focused_window));
            if self.overview.is_active() {
                self.retarget_overview();
            } else {
                self.set_overview_active(true);
            }
        }
        if let Some(id) = self
            .focus_cycle
            .as_mut()
            .and_then(|cycle| cycle.advance(reverse, &available))
        {
            let previous = self.overview.selection_state();
            self.overview.select_window(id);
            self.redraw_overview_selection(previous);
        } else {
            self.cancel_focus_cycle();
        }
    }

    pub(crate) fn finish_focus_cycle(&mut self) {
        let Some(mut cycle) = self.focus_cycle.take() else {
            return;
        };
        if !self.session_lock.active()
            && !self.input_capture.captures(1)
            && let Some(id) = cycle.reconcile(&self.focus_candidates())
        {
            self.activate_managed_window(id);
        }
        self.set_overview_active(false);
    }

    pub(crate) fn cancel_focus_cycle(&mut self) {
        if self.focus_cycle.take().is_some() {
            self.set_overview_active(false);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::capture_privacy::tests::window;
    use smithay::reexports::calloop::EventLoop;
    use std::os::unix::net::UnixStream;

    struct Fixture {
        events: EventLoop<'static, Ferese>,
        state: Ferese,
        ids: Vec<WindowId>,
        _clients: Vec<UnixStream>,
    }

    fn fixture(test: &str, floating: &[bool]) -> Option<Fixture> {
        if !crate::startup_tests::private_runtime(&format!("state::focus::tests::{test}")) {
            return None;
        }
        let mut events = EventLoop::try_new().unwrap();
        let mut state = crate::startup_tests::state(&mut events);
        state.animations_enabled = false;
        let output = Output::new(
            "focus".into(),
            smithay::output::PhysicalProperties {
                size: (0, 0).into(),
                subpixel: smithay::output::Subpixel::Unknown,
                make: "test".into(),
                model: "test".into(),
            },
        );
        output.change_current_state(
            Some(smithay::output::Mode {
                size: (800, 600).into(),
                refresh: 60_000,
            }),
            None,
            None,
            None,
        );
        state.space.map_output(&output, (0, 0));
        state.register_output(&output, "focus".into());
        let mut ids = Vec::new();
        let mut clients = Vec::new();
        for floating in floating {
            let (window, wire) = window(&mut state, &mut events, 0xff112233);
            let id = state.windows.ids()[&window];
            assert!(state.activate_managed_window(id));
            if *floating {
                state.toggle_focused_floating();
            }
            ids.push(id);
            clients.push(wire);
        }
        Some(Fixture {
            events,
            state,
            ids,
            _clients: clients,
        })
    }

    impl Fixture {
        fn focus(&mut self, index: usize) {
            assert!(self.state.activate_managed_window(self.ids[index]));
        }

        fn placements(&self) -> Vec<Option<WindowPlacement>> {
            self.ids.iter().map(|id| self.state.workspaces.placement(*id)).collect()
        }

        fn press(&mut self, expected: usize) {
            let workspace = self.state.workspaces.active_id();
            let placements = self.placements();
            self.state.focus_floating();
            self.assert_focus(expected);
            assert_eq!(self.state.workspaces.active_id(), workspace);
            assert_eq!(self.placements(), placements);
        }

        fn assert_focus(&self, expected: usize) {
            let id = self.ids[expected];
            assert_eq!(self.state.focused_window, Some(id));
            assert_eq!(self.state.workspaces.active().last_focused, Some(id));
            assert_eq!(
                self.state.seat.get_keyboard().unwrap().current_focus(),
                Some(
                    self.state
                        .windows
                        .window(id)
                        .unwrap()
                        .toplevel()
                        .unwrap()
                        .wl_surface()
                        .clone()
                )
            );
        }
    }

    #[test]
    fn tiled_focuses_most_recently_focused_float() {
        let Some(mut f) = fixture("tiled_focuses_most_recently_focused_float", &[false, true, true]) else {
            return;
        };
        f.focus(1);
        f.focus(0);
        f.press(1);
    }

    #[test]
    fn floating_focuses_most_recently_focused_tile() {
        let Some(mut f) = fixture("floating_focuses_most_recently_focused_tile", &[false, false, true]) else {
            return;
        };
        f.focus(0);
        f.focus(2);
        f.press(0);
    }

    #[test]
    fn multiple_floaters_do_not_cycle_or_wrap() {
        let Some(mut f) = fixture("multiple_floaters_do_not_cycle_or_wrap", &[false, true, true, true]) else {
            return;
        };
        f.focus(2);
        f.focus(0);
        for _ in 0..3 {
            f.press(2);
            f.press(0);
        }
    }

    #[test]
    fn no_floating_candidate_is_a_no_op() {
        let Some(mut f) = fixture("no_floating_candidate_is_a_no_op", &[false, false]) else {
            return;
        };
        f.focus(0);
        let history = f.state.focus_history.candidates(None, &f.ids);
        f.press(0);
        f.press(0);
        assert_eq!(f.state.focus_history.candidates(None, &f.ids), history);
    }

    #[test]
    fn no_tiled_candidate_is_a_no_op() {
        let Some(mut f) = fixture("no_tiled_candidate_is_a_no_op", &[true, true]) else {
            return;
        };
        f.focus(0);
        let history = f.state.focus_history.candidates(None, &f.ids);
        f.press(0);
        f.press(0);
        assert_eq!(f.state.focus_history.candidates(None, &f.ids), history);
    }

    #[test]
    fn candidates_on_other_workspaces_are_ignored_in_both_directions() {
        let Some(mut f) = fixture(
            "candidates_on_other_workspaces_are_ignored_in_both_directions",
            &[false, true, false, true],
        ) else {
            return;
        };
        for index in [2, 3] {
            f.focus(index);
            f.state.move_focused_to_workspace(2);
        }
        f.focus(1);
        f.focus(3);
        f.focus(0);
        f.press(1);
        f.focus(2);
        f.focus(1);
        f.press(0);
        f.focus(0);
        f.state.move_focused_to_workspace(2);
        f.focus(1);
        f.press(1);
        f.focus(3);
        f.state.move_focused_to_workspace(1);
        f.focus(2);
        f.press(2);
    }

    #[test]
    fn repeated_presses_follow_updated_focus_history_between_layers() {
        let Some(mut f) = fixture(
            "repeated_presses_follow_updated_focus_history_between_layers",
            &[false, false, true, true],
        ) else {
            return;
        };
        f.focus(0);
        f.focus(2);
        for _ in 0..3 {
            f.press(0);
            f.press(2);
        }
        f.focus(1);
        f.press(2);
        f.press(1);
        f.focus(3);
        f.press(1);
        f.press(3);
    }

    #[test]
    fn focus_floating_preserves_all_window_placements() {
        let Some(mut f) = fixture("focus_floating_preserves_all_window_placements", &[false, true, true]) else {
            return;
        };
        let placements = f.placements();
        f.focus(0);
        f.press(2);
        f.press(0);
        assert_eq!(f.placements(), placements);
    }

    #[test]
    fn ipc_focus_floating_uses_the_managed_activation_path() {
        use ferese_ipc::{Request, Response, VERSION, read_frame, write_frame};
        use std::sync::mpsc;
        use std::time::{Duration, Instant};
        let Some(mut f) = fixture("ipc_focus_floating_uses_the_managed_activation_path", &[false, true]) else {
            return;
        };
        f.focus(0);
        let workspace = f.state.workspaces.active_id();
        let placements = f.placements();
        let path = f.state.session_environment.control_socket.clone();
        let (send, receive) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            let mut socket = UnixStream::connect(path).unwrap();
            socket.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
            write_frame(
                &mut socket,
                &Request {
                    version: VERSION,
                    id: 1,
                    kind: "command".into(),
                    command: "focus-floating".into(),
                    args: serde_json::json!({}),
                },
            )
            .unwrap();
            send.send(read_frame::<Response>(&mut socket).unwrap()).unwrap();
        });
        let deadline = Instant::now() + Duration::from_secs(3);
        let response = loop {
            if let Ok(response) = receive.try_recv() {
                break response;
            }
            assert!(Instant::now() < deadline, "IPC focus action timed out");
            f.events.dispatch(Duration::from_millis(1), &mut f.state).unwrap();
        };
        worker.join().unwrap();
        assert!(response.error.is_none(), "{:?}", response.error);
        f.assert_focus(1);
        assert_eq!(f.state.workspaces.active_id(), workspace);
        assert_eq!(f.placements(), placements);
    }
}
