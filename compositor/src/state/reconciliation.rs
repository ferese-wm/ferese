//! Exceptional topology publication. Ordinary input/damage never constructs a plan.
use super::*;
use smithay::output::{Mode, Scale};
use smithay::utils::Transform;

/// Mutable resource bookkeeping lives outside the pure ownership plan.
#[derive(Default)]
pub(crate) struct DesktopTransition {
    pub retired: HashSet<Output>,
    pub redraw: HashSet<Output>,
    pub publishing: bool,
    pub shell_pending: bool,
}

/// The backend reports hardware that actually survived application/rollback.
pub(crate) struct DesktopOutput {
    pub output: Output,
    pub identity: String,
    pub mode: Mode,
    pub transform: Transform,
    pub scale: Scale,
    pub position: Point<i32, Logical>,
}

impl DesktopOutput {
    fn geometry(&self) -> OutputGeometry {
        let size = self
            .transform
            .transform_size(self.mode.size)
            .to_f64()
            .to_logical(self.scale.fractional_scale())
            .to_i32_ceil();
        OutputGeometry::new(self.position.x, self.position.y, size.w, size.h)
    }
}

#[derive(Default)]
pub(crate) struct DesktopChanges {
    pub layout: bool,
    pub focus: bool,
    pub shell: bool,
    pub outputs: HashSet<Output>,
}

impl Ferese {
    pub(crate) fn begin_desktop_transition(&mut self) {
        assert!(self.desktop_transition.is_none(), "nested desktop transition");
        self.desktop_transition = Some(DesktopTransition::default());
    }

    /// No event-loop dispatch occurs between planning and publication. Only the
    /// small ownership map is staged; layout and animation remain authoritative.
    pub(crate) fn publish_desktop(
        &mut self,
        runtime: Vec<DesktopOutput>,
    ) -> Result<DesktopChanges, ferese_core::OutputError> {
        assert!(self.desktop_transition.is_some());
        let inventory = runtime
            .iter()
            .map(|output| (self.ensure_output_identity(&output.identity), output.geometry()))
            .collect::<Vec<_>>();
        let workspaces = self.workspaces.iter().map(|workspace| workspace.id).collect::<Vec<_>>();
        let plan = self
            .output_workspaces
            .plan_desktop(&inventory, &workspaces, self.workspaces.next_workspace_id())?;
        let mut changes = DesktopChanges {
            layout: !plan.affected_outputs.is_empty(),
            focus: plan.focus_changed,
            shell: !plan.affected_outputs.is_empty(),
            outputs: self.desktop_transition.as_ref().unwrap().redraw.clone(),
        };
        let floats = self
            .windows
            .ids()
            .values()
            .filter_map(|id| {
                let workspace = self.workspaces.workspace_for_window(*id)?;
                let WindowPlacement::Floating { rect } = self.workspaces.placement(*id)? else {
                    return None;
                };
                let destination = plan.outputs.output_for_workspace(workspace)?;
                let new = plan.outputs.geometry(destination)?;
                let old_owner = self.output_workspaces.output_for_workspace(workspace);
                let old = old_owner.and_then(|owner| self.output_workspaces.geometry(owner));
                (old_owner != Some(destination) || old != Some(new)).then(|| {
                    let bounds = |g: OutputGeometry| Rect::new(g.x as f64, g.y as f64, g.width as f64, g.height as f64);
                    (*id, moved_floating_rect(rect, bounds(old.unwrap_or(new)), bounds(new)))
                })
            })
            .collect::<Vec<_>>();

        // All fallible planning precedes publication. Resource retirement may
        // already have invalidated unusable scanouts, but not desktop policy.
        for id in plan.created_workspaces {
            assert_eq!(self.workspaces.create_workspace(), id);
        }
        let surviving = runtime.iter().map(|output| output.output.clone()).collect::<Vec<_>>();
        let old_outputs = self.output_ids.keys().cloned().collect::<Vec<_>>();
        for output in old_outputs {
            if !surviving.contains(&output) {
                self.retire_output_resources(&output);
                self.space.unmap_output(&output);
            }
        }
        if self.output_ids.is_empty() && !runtime.is_empty() {
            self.reset_animation_clock();
        }
        self.output_ids.clear();
        self.outputs_by_id.clear();
        self.output_names.clear();
        for (output, (id, geometry)) in runtime.into_iter().zip(inventory) {
            let state_changed = output.output.current_mode() != Some(output.mode)
                || output.output.current_transform() != output.transform
                || output.output.current_scale().fractional_scale() != output.scale.fractional_scale()
                || self.space.output_geometry(&output.output).is_none_or(|old| {
                    old.loc != output.position || old.size.w != geometry.width || old.size.h != geometry.height
                });
            if state_changed {
                output.output.change_current_state(
                    Some(output.mode),
                    Some(output.transform),
                    Some(output.scale),
                    Some(output.position),
                );
                self.space.map_output(&output.output, output.position);
                changes.layout = true;
                changes.shell = true;
            }
            if state_changed || plan.affected_outputs.contains(&id) {
                changes.outputs.insert(output.output.clone());
            }
            if state_changed && self.session_lock.active() {
                self.session_lock.output_added(&output.output);
            }
            self.output_names.insert(id, output.output.name());
            self.outputs_by_id.insert(id, output.output.clone());
            self.output_ids.insert(output.output, id);
        }
        // Slides referencing evacuated workspaces cannot survive ownership
        // changes. Unaffected slide/world/viewport/resize owners remain intact.
        self.workspace_slides.retain(|id, slide| {
            plan.outputs.geometry(*id).is_some()
                && slide
                    .items
                    .iter()
                    .all(|item| plan.outputs.output_for_workspace(item.workspace) == Some(*id))
        });
        self.output_workspaces = plan.outputs;
        for (id, rect) in floats {
            self.workspaces
                .set_floating_rect(id, rect)
                .expect("planned floating window exists");
        }
        self.refresh_workspace_slide_offsets();
        if changes.focus {
            if let Some(output) = self.output_workspaces.focused_output()
                && let Some(workspace) = self.output_workspaces.active_workspace(output)
            {
                self.activate_output_workspace(output, workspace);
            } else {
                self.focused_window = None;
            }
        }
        Ok(changes)
    }

    pub(crate) fn finish_desktop_transition(&mut self, mut changes: DesktopChanges) {
        self.desktop_transition.as_mut().unwrap().publishing = true;
        if changes.layout {
            self.relayout_desktop();
        }
        if changes.focus || self.session_lock.active() {
            self.restore_keyboard_focus();
        }
        self.refresh_lock_outputs();
        self.refresh_idle_inhibition();
        let batch = self.desktop_transition.take().unwrap();
        changes.outputs.extend(batch.redraw);
        // Animation/layout helpers may request snapshots, but only this point
        // can emit the complete topology generation.
        if changes.shell || changes.layout || changes.focus || batch.shell_pending {
            self.send_shell_snapshots();
        }
        changes.outputs.retain(|output| {
            self.output_ids.contains_key(output)
                || self
                    .direct_backend
                    .as_ref()
                    .is_some_and(|backend| backend.physical_outputs().any(|physical| physical == output))
        });
        if !changes.outputs.is_empty() {
            crate::backends::direct::render_on(self, &changes.outputs.into_iter().collect::<Vec<_>>());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use smithay::output::{PhysicalProperties, Subpixel};
    use smithay::reexports::wayland_server::Resource;
    use std::io::{Read, Write};
    use std::os::unix::net::UnixStream;

    fn output(name: &str, x: i32, width: i32) -> DesktopOutput {
        DesktopOutput {
            output: Output::new(
                name.into(),
                PhysicalProperties {
                    size: (0, 0).into(),
                    subpixel: Subpixel::Unknown,
                    make: "test".into(),
                    model: "test".into(),
                },
            ),
            identity: name.into(),
            mode: Mode {
                size: (width, 800).into(),
                refresh: 60_000,
            },
            transform: Transform::Normal,
            scale: Scale::Fractional(1.0),
            position: (x, 0).into(),
        }
    }

    fn request(wire: &mut UnixStream, object: u32, opcode: u32, args: &[u32]) {
        for word in [object, (((args.len() + 2) * 4) as u32) << 16 | opcode]
            .into_iter()
            .chain(args.iter().copied())
        {
            wire.write_all(&word.to_ne_bytes()).unwrap();
        }
    }

    fn window(state: &mut Ferese, event_loop: &mut EventLoop<'static, Ferese>) -> (Window, UnixStream) {
        use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_wm_base::XdgWmBase;
        use smithay::reexports::wayland_server::protocol::wl_compositor::WlCompositor;
        use smithay::wayland::shell::xdg::XdgWmBaseUserData;
        let (server, mut wire) = UnixStream::pair().unwrap();
        let client = state
            .display_handle
            .insert_client(server, Arc::new(ClientState::default()))
            .unwrap();
        let compositor = client
            .create_resource::<WlCompositor, (), Ferese>(&state.display_handle, 6, ())
            .unwrap();
        let shell = client
            .create_resource::<XdgWmBase, XdgWmBaseUserData, Ferese>(&state.display_handle, 6, Default::default())
            .unwrap();
        request(&mut wire, compositor.id().protocol_id(), 0, &[2]);
        request(&mut wire, shell.id().protocol_id(), 2, &[3, 2]);
        request(&mut wire, 3, 1, &[4]);
        event_loop.dispatch(Duration::from_millis(10), state).unwrap();
        let window = state
            .space
            .elements()
            .find(|window| {
                window
                    .toplevel()
                    .unwrap()
                    .wl_surface()
                    .client()
                    .is_some_and(|owner| owner.id() == client.id())
            })
            .unwrap()
            .clone();
        (window, wire)
    }

    fn events(state: &mut Ferese, wire: &mut UnixStream) -> Vec<(u32, u32, Vec<u32>)> {
        state.display_handle.flush_clients().unwrap();
        wire.set_nonblocking(true).unwrap();
        let mut bytes = Vec::new();
        match wire.read_to_end(&mut bytes) {
            Ok(_) => {}
            Err(error) => assert_eq!(error.kind(), std::io::ErrorKind::WouldBlock),
        }
        let mut result = Vec::new();
        let mut offset = 0;
        while offset < bytes.len() {
            let word = |at| u32::from_ne_bytes(bytes[at..at + 4].try_into().unwrap());
            let header = word(offset + 4);
            let end = offset + (header >> 16) as usize;
            result.push((
                word(offset),
                header & 0xffff,
                (offset + 8..end).step_by(4).map(word).collect(),
            ));
            offset = end;
        }
        result
    }

    #[test]
    fn planned_geometry_matches_smithay_at_fractional_scale_and_rotation() {
        for transform in [Transform::Normal, Transform::_90, Transform::Flipped270] {
            let mut entry = output("geometry", -500, 1920);
            entry.mode.size.h = 1080;
            entry.scale = Scale::Fractional(1.75);
            entry.transform = transform;
            entry.output.change_current_state(
                Some(entry.mode),
                Some(transform),
                Some(entry.scale),
                Some(entry.position),
            );
            let mut space = Space::<Window>::default();
            space.map_output(&entry.output, entry.position);
            let actual = space.output_geometry(&entry.output).unwrap();
            assert_eq!(
                entry.geometry(),
                OutputGeometry::new(actual.loc.x, actual.loc.y, actual.size.w, actual.size.h)
            );
        }
    }

    #[test]
    fn topology_publication_protocol_and_readiness_regressions() {
        // The subprocess owns private sockets and config; never touches a live session.
        if std::env::var_os("FERESE_DESKTOP_TEST_CHILD").is_none() {
            let runtime = tempfile::tempdir().unwrap();
            let result = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "state::reconciliation::tests::topology_publication_protocol_and_readiness_regressions",
                    "--nocapture",
                ])
                .env("FERESE_DESKTOP_TEST_CHILD", "1")
                .env("XDG_RUNTIME_DIR", runtime.path())
                .env("XDG_CONFIG_HOME", runtime.path())
                .env("XDG_STATE_HOME", runtime.path())
                .output()
                .unwrap();
            assert!(
                result.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&result.stdout),
                String::from_utf8_lossy(&result.stderr)
            );
            return;
        }
        let mut event_loop = EventLoop::try_new().unwrap();
        let config = crate::config::Config::default().runtime_config().unwrap();
        let mut state = Ferese::new(&mut event_loop, Display::new().unwrap(), config).unwrap();
        let a = output("a", 0, 1200);
        let b = output("b", 1200, 800);
        let a_handle = a.output.clone();
        let b_handle = b.output.clone();
        state.begin_desktop_transition();
        let changes = state.publish_desktop(vec![a, b]).unwrap();
        state.finish_desktop_transition(changes);
        let a_id = state.output_ids[&a_handle];
        let b_id = state.output_ids[&b_handle];
        let b_workspace = state.output_workspaces.active_workspace(b_id).unwrap();

        // Real protocol resources let us observe exactly what clients receive.
        let (server, mut shell_wire) = UnixStream::pair().unwrap();
        let client = state
            .display_handle
            .insert_client(server, Arc::new(ClientState::default()))
            .unwrap();
        let shell = client
            .create_resource::<FereseShellV1, (), Ferese>(&state.display_handle, 6, ())
            .unwrap();
        state.shell_resources.push(shell.downgrade());
        state.send_shell_snapshots();
        events(&mut state, &mut shell_wire);
        let original_serial = state.shell_snapshot_serial;

        // Invalid planning leaves output metadata, ownership and shell unchanged.
        let original = state.output_workspaces.clone();
        state.begin_desktop_transition();
        let mut invalid = state.current_desktop_outputs();
        invalid[0].mode.size.w = 0;
        assert!(state.publish_desktop(invalid).is_err());
        assert_eq!(state.output_workspaces, original);
        assert_eq!(a_handle.current_mode().unwrap().size.w, 1200);
        assert!(events(&mut state, &mut shell_wire).is_empty());
        state.finish_desktop_transition(DesktopChanges::default());
        assert_eq!(state.shell_snapshot_serial, original_serial);

        // A real XDG toplevel needs a new size; it deliberately never acknowledges it.
        let (slow, _slow_wire) = window(&mut state, &mut event_loop);
        let slow_id = WindowId(101);
        let initial = Rect::new(1500.0, 500.0, 600.0, 600.0);
        state
            .workspaces
            .insert_floating_window(slow_id, b_workspace, initial, true)
            .unwrap();
        state.windows.register_window(slow.clone(), slow_id);
        let mut geometry = WindowGeometry::new(initial, None);
        geometry.client.committed_size = Some(ClientSize::from_rect(initial));
        state.windows.set_geometry(slow_id, geometry);
        state.output_workspaces.focus_output(b_id).unwrap();
        state.restore_output_focus();
        state.relayout();
        events(&mut state, &mut shell_wire);
        let before_serial = state.shell_snapshot_serial;
        let spare = state.workspaces.create_workspace();
        state.output_workspaces.assign_workspace(b_id, spare).unwrap();
        state.workspace_slides.insert(
            b_id,
            WorkspaceSlide::new(None, spare, b_workspace, SwipeDirection::Right),
        );

        state.begin_desktop_transition();
        state.unregister_output(&b_handle);
        state.unregister_output(&b_handle); // Duplicate retirement is harmless.
        assert_eq!(
            state.workspaces.placement(slow_id),
            Some(WindowPlacement::Floating { rect: initial })
        );
        assert_eq!(state.output_workspaces.output_for_workspace(b_workspace), Some(b_id));
        state.send_shell_snapshots();
        assert!(events(&mut state, &mut shell_wire).is_empty());
        let mut runtime = state.current_desktop_outputs();
        runtime.retain(|entry| entry.output == a_handle);
        runtime[0].mode.size = (500, 400).into();
        let changes = state.publish_desktop(runtime).unwrap();
        assert_eq!(state.output_workspaces.output_for_workspace(b_workspace), Some(a_id));
        assert_eq!(state.focused_window, Some(slow_id));
        assert!(!state.workspace_slides.contains_key(&b_id));
        state.send_shell_snapshots();
        assert!(events(&mut state, &mut shell_wire).is_empty());
        state.finish_desktop_transition(changes);
        assert_eq!(state.shell_snapshot_serial, before_serial + 1);
        let published = events(&mut state, &mut shell_wire);
        let shell_events = published
            .iter()
            .filter(|(id, _, _)| *id == shell.id().protocol_id())
            .collect::<Vec<_>>();
        assert_eq!(shell_events.iter().filter(|(_, opcode, _)| *opcode == 0).count(), 1);
        assert_eq!(shell_events.iter().filter(|(_, opcode, _)| *opcode == 4).count(), 1);
        assert_eq!(shell_events.iter().filter(|(_, opcode, _)| *opcode == 2).count(), 1);
        for (_, opcode, args) in &shell_events {
            if *opcode == 3 {
                assert_eq!(args[3], a_id.0 as u32);
                if args[1] == b_workspace.0 as u32 {
                    assert_eq!(&args[args.len() - 2..], &[1, 1]);
                }
            } else if *opcode == 2 {
                let active = 3 + args[2].div_ceil(4) as usize;
                assert_eq!(args[active + 1], b_workspace.0 as u32);
                assert_eq!(args[active + 2], 1);
            } else if *opcode == 1 {
                assert_eq!(args[1], slow_id.0 as u32);
                assert_eq!(args[3], b_workspace.0 as u32);
            }
        }
        let WindowPlacement::Floating { rect } = state.workspaces.placement(slow_id).unwrap() else {
            panic!()
        };
        assert!(rect.x >= 0.0 && rect.y >= 0.0 && rect.x + rect.width <= 500.0 && rect.y + rect.height <= 400.0);
        assert!(
            state.windows.transaction(&slow_id).is_some(),
            "topology commit must not await XDG acknowledgement"
        );
        assert_eq!(
            state.windows.geometry(&slow_id).unwrap().client.committed_size,
            Some(ClientSize::from_rect(initial))
        );

        // A separate workspace/output keeps progressing while the slow client waits.
        state.begin_desktop_transition();
        let mut runtime = state.current_desktop_outputs();
        runtime.push(output("c", 500, 800));
        let changes = state.publish_desktop(runtime).unwrap();
        state.finish_desktop_transition(changes);
        let c_id = state.persistent_output_id("c").unwrap();
        let c_workspace = state.output_workspaces.active_workspace(c_id).unwrap();
        let mut viewport = AnimatedValue::new(0.0);
        viewport.set_target(200.0);
        state
            .viewports
            .insert(c_workspace, ViewportPresentation::from_motion(viewport));
        let now = state.start_time.elapsed();
        state.advance_animations_at(Duration::from_millis(16), now + Duration::from_millis(16));
        assert!(state.viewports[&c_workspace].motion().current > 0.0);
        assert!(state.windows.transaction(&slow_id).is_some());
        state.advance_animations_at(Duration::from_millis(334), now + Duration::from_millis(350));
        assert!(state.windows.transaction(&slow_id).is_none());

        // A no-op/resume inventory keeps ownership and emits no duplicate generation.
        events(&mut state, &mut shell_wire);
        let before = state.output_workspaces.clone();
        for _ in 0..3 {
            state.begin_desktop_transition();
            let runtime = state.current_desktop_outputs();
            let changes = state.publish_desktop(runtime).unwrap();
            assert!(!changes.layout && !changes.focus && !changes.shell);
            assert!(changes.outputs.is_empty());
            state.finish_desktop_transition(changes);
        }
        assert_eq!(state.output_workspaces, before);
        assert!(events(&mut state, &mut shell_wire).is_empty());
        assert_eq!(state.workspaces.workspace_for_window(slow_id), Some(b_workspace));

        let unfocused = state.outputs_by_id[&c_id].clone();
        let focused = state.focused_window;
        state.unregister_output(&unfocused);
        assert_eq!(
            state.focused_window, focused,
            "removing a non-focused output preserves window focus"
        );

        // Lock protection remains authoritative through headless loss/recovery.
        use smithay::reexports::wayland_protocols::ext::session_lock::v1::server::ext_session_lock_manager_v1::ExtSessionLockManagerV1;
        let (server, mut lock_wire) = UnixStream::pair().unwrap();
        let client = state
            .display_handle
            .insert_client(server, Arc::new(ClientState::default()))
            .unwrap();
        let lock = client
            .create_resource::<ExtSessionLockManagerV1, (), Ferese>(&state.display_handle, 1, ())
            .unwrap();
        request(&mut lock_wire, lock.id().protocol_id(), 1, &[2]);
        event_loop.dispatch(Duration::from_millis(10), &mut state).unwrap();
        assert!(state.session_lock.active());
        state.begin_desktop_transition();
        for output in state.output_ids.keys().cloned().collect::<Vec<_>>() {
            state.unregister_output(&output);
        }
        let changes = state.publish_desktop(Vec::new()).unwrap();
        state.finish_desktop_transition(changes);
        assert!(state.output_ids.is_empty());
        assert!(state.space.outputs().next().is_none());
        assert!(state.output_workspaces.focused_output().is_none());
        assert!(state.focused_window.is_none());
        assert!(state.workspace_slides.is_empty());
        assert!(state.seat.get_keyboard().unwrap().current_focus().is_none());
        assert_eq!(state.workspaces.workspace_for_window(slow_id), Some(b_workspace));
        state.begin_desktop_transition();
        let changes = state.publish_desktop(vec![output("a", 0, 1200)]).unwrap();
        state.finish_desktop_transition(changes);
        assert_eq!(state.output_workspaces.output_for_workspace(b_workspace), Some(a_id));
        assert!(state.session_lock.active());
        assert!(state.surface_under((20.0, 20.0).into()).is_none());
    }
}
