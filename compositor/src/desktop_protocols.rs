//! Standard desktop discovery follows Ferese's managed window and output identities.

use std::collections::{HashMap, HashSet};

use ferese_core::{OutputId, WorkspaceId};
use ferese_layout::WindowId;
use smithay::output::Output;
use smithay::reexports::wayland_protocols::ext::workspace::v1::server::{
    ext_workspace_group_handle_v1::{self, ExtWorkspaceGroupHandleV1},
    ext_workspace_handle_v1::{self, ExtWorkspaceHandleV1},
    ext_workspace_manager_v1::{self, ExtWorkspaceManagerV1},
};
use smithay::reexports::wayland_server::backend::ClientId;
use smithay::reexports::wayland_server::protocol::wl_output::WlOutput;
use smithay::reexports::wayland_server::{
    Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource,
};
use smithay::wayland::foreign_toplevel_list::{
    ForeignToplevelHandle, ForeignToplevelListHandler, ForeignToplevelListState,
};

use crate::Ferese;

struct Toplevel {
    handle: ForeignToplevelHandle,
    revision: u64,
}

pub(crate) struct DesktopProtocols {
    foreign_list: ForeignToplevelListState,
    foreign_discovery_started: bool,
    toplevels: HashMap<WindowId, Toplevel>,
    workspace_managers: Vec<Manager>,
}

struct Manager {
    resource: ExtWorkspaceManagerV1,
    groups: HashMap<OutputId, Group>,
    workspaces: HashMap<WorkspaceId, Workspace>,
    pending: HashMap<OutputId, WorkspaceId>,
}

struct Group {
    resource: ExtWorkspaceGroupHandleV1,
    outputs: HashSet<WlOutput>,
}

struct Workspace {
    resource: ExtWorkspaceHandleV1,
    group: OutputId,
    index: u32,
    active: bool,
}

pub(crate) struct GroupData;

pub(crate) struct WorkspaceData {
    manager: ExtWorkspaceManagerV1,
    id: WorkspaceId,
}

struct View {
    id: WorkspaceId,
    output: OutputId,
    index: u32,
    active: bool,
}

impl DesktopProtocols {
    pub(crate) fn new(display: &DisplayHandle) -> Self {
        let foreign_list = ForeignToplevelListState::new_with_filter::<Ferese>(
            display,
            crate::handlers::security_context::unrestricted_client,
        );
        display.create_global::<Ferese, ExtWorkspaceManagerV1, _>(1, ());
        Self {
            foreign_list,
            foreign_discovery_started: false,
            toplevels: HashMap::new(),
            workspace_managers: Vec::new(),
        }
    }
}

fn sync_toplevels(state: &mut Ferese) {
    if !state.desktop_protocols.foreign_discovery_started {
        return;
    }
    let locked = state.session_lock.active();
    let visible = state
        .windows
        .ordered_ids()
        .filter(|id| {
            !locked
                && state.workspaces.workspace_for_window(*id).is_some()
                && state
                    .windows
                    .window(*id)
                    .is_some_and(|window| !state.capture_protected(window))
        })
        .collect::<HashSet<_>>();
    let desktop = &mut state.desktop_protocols;
    desktop.toplevels.retain(|id, toplevel| {
        if visible.contains(id) {
            true
        } else {
            desktop.foreign_list.remove_toplevel(&toplevel.handle);
            false
        }
    });
    for id in visible {
        let Some(record) = state.windows.record(id) else {
            continue;
        };
        if let Some(toplevel) = desktop.toplevels.get_mut(&id) {
            if toplevel.revision != record.metadata_revision {
                toplevel.handle.send_title(&record.title);
                toplevel.handle.send_app_id(&record.app_id);
                toplevel.handle.send_done();
                toplevel.revision = record.metadata_revision;
            }
        } else {
            let handle = desktop
                .foreign_list
                .new_toplevel::<Ferese>(&record.title, &record.app_id);
            desktop.toplevels.insert(
                id,
                Toplevel {
                    handle,
                    revision: record.metadata_revision,
                },
            );
        }
    }
}

impl ForeignToplevelListHandler for Ferese {
    fn foreign_toplevel_list_state(&mut self) -> &mut ForeignToplevelListState {
        // Binding can occur after another request in this same dispatch changed
        // metadata, mapping, or privacy. Never publish the previous batch's list.
        self.desktop_protocols.foreign_discovery_started = true;
        sync_toplevels(self);
        &mut self.desktop_protocols.foreign_list
    }
}

pub(crate) fn sync(state: &mut Ferese) {
    sync_toplevels(state);
    if state.desktop_protocols.workspace_managers.is_empty() {
        return;
    }
    let (views, outputs) = workspace_snapshot(state);
    for manager in &mut state.desktop_protocols.workspace_managers {
        publish_workspaces(&state.display_handle, manager, &views, &outputs);
    }
}

fn workspace_snapshot(state: &Ferese) -> (Vec<View>, Vec<(OutputId, Output)>) {
    if state.session_lock.active() {
        return (Vec::new(), Vec::new());
    }
    let views = state
        .output_workspaces
        .workspace_views_iter(&state.workspaces)
        .map(|view| View {
            id: view.id,
            output: view.output,
            index: view.index,
            active: view.visible,
        })
        .collect();
    let outputs = state
        .space
        .outputs()
        .filter_map(|output| state.output_id(output).map(|id| (id, output.clone())))
        .collect();
    (views, outputs)
}

fn publish_workspaces(
    display: &DisplayHandle,
    manager: &mut Manager,
    views: &[View],
    outputs: &[(OutputId, Output)],
) {
    let Some(client) = manager.resource.client() else {
        return;
    };
    let mut changed = false;
    // Leave old groups before their removed event, including when a workspace
    // migrates from an unplugged display to a surviving display.
    for (id, workspace) in &manager.workspaces {
        if views
            .iter()
            .find(|view| &view.id == id)
            .is_none_or(|view| view.output != workspace.group)
            && workspace.resource.is_alive()
            && let Some(group) = manager.groups.get(&workspace.group)
            && group.resource.is_alive()
        {
            group.resource.workspace_leave(&workspace.resource);
            changed = true;
        }
    }
    manager.workspaces.retain(|id, workspace| {
        let live = views.iter().any(|view| &view.id == id);
        if !live {
            workspace.resource.removed();
            changed = true;
        }
        live
    });
    manager.groups.retain(|id, group| {
        let live = outputs.iter().any(|(next, _)| next == id);
        if !live {
            for output in &group.outputs {
                group.resource.output_leave(output);
            }
            group.resource.removed();
            changed = true;
        }
        live
    });
    for (id, output) in outputs {
        if !manager.groups.contains_key(id) {
            let Ok(resource) = client
                .create_resource::<ExtWorkspaceGroupHandleV1, _, Ferese>(display, 1, GroupData)
            else {
                continue;
            };
            manager.resource.workspace_group(&resource);
            resource.capabilities(ext_workspace_group_handle_v1::GroupCapabilities::empty());
            manager.groups.insert(
                *id,
                Group {
                    resource,
                    outputs: HashSet::new(),
                },
            );
            changed = true;
        }
        let group = manager.groups.get_mut(id).expect("group was populated");
        if !group.resource.is_alive() {
            continue;
        }
        let bound = output.client_outputs(&client).collect::<HashSet<_>>();
        for output in group.outputs.difference(&bound) {
            group.resource.output_leave(output);
            changed = true;
        }
        for output in bound.difference(&group.outputs) {
            group.resource.output_enter(output);
            changed = true;
        }
        group.outputs = bound;
    }
    for view in views {
        let Some(group) = manager
            .groups
            .get(&view.output)
            .filter(|group| group.resource.is_alive())
        else {
            continue;
        };
        if !manager.workspaces.contains_key(&view.id) {
            let Ok(resource) = client.create_resource::<ExtWorkspaceHandleV1, _, Ferese>(
                display,
                1,
                WorkspaceData {
                    manager: manager.resource.clone(),
                    id: view.id,
                },
            ) else {
                continue;
            };
            manager.resource.workspace(&resource);
            // Ferese workspace IDs live for a session, while this protocol's id
            // event promises persistence. Temporary workspaces therefore omit it.
            resource.name(view.index.to_string());
            resource.coordinates(view.index.saturating_sub(1).to_ne_bytes().to_vec());
            resource.state(if view.active {
                ext_workspace_handle_v1::State::Active
            } else {
                ext_workspace_handle_v1::State::empty()
            });
            resource.capabilities(ext_workspace_handle_v1::WorkspaceCapabilities::Activate);
            group.resource.workspace_enter(&resource);
            manager.workspaces.insert(
                view.id,
                Workspace {
                    resource,
                    group: view.output,
                    index: view.index,
                    active: view.active,
                },
            );
            changed = true;
        } else {
            let workspace = manager
                .workspaces
                .get_mut(&view.id)
                .expect("workspace was populated");
            if !workspace.resource.is_alive() {
                continue;
            }
            if workspace.group != view.output {
                group.resource.workspace_enter(&workspace.resource);
                workspace.group = view.output;
                changed = true;
            }
            if workspace.index != view.index {
                workspace.resource.name(view.index.to_string());
                workspace
                    .resource
                    .coordinates(view.index.saturating_sub(1).to_ne_bytes().to_vec());
                workspace.index = view.index;
                changed = true;
            }
            if workspace.active != view.active {
                workspace.resource.state(if view.active {
                    ext_workspace_handle_v1::State::Active
                } else {
                    ext_workspace_handle_v1::State::empty()
                });
                workspace.active = view.active;
                changed = true;
            }
        }
    }
    manager.pending.retain(|output, id| {
        views
            .iter()
            .any(|view| &view.id == id && &view.output == output)
    });
    if changed {
        manager.resource.done();
    }
}

impl GlobalDispatch<ExtWorkspaceManagerV1, ()> for Ferese {
    fn bind(
        state: &mut Self,
        display: &DisplayHandle,
        _client: &Client,
        resource: New<ExtWorkspaceManagerV1>,
        _data: &(),
        init: &mut DataInit<'_, Self>,
    ) {
        let resource = init.init(resource, ());
        let mut manager = Manager {
            resource,
            groups: HashMap::new(),
            workspaces: HashMap::new(),
            pending: HashMap::new(),
        };
        let (views, outputs) = workspace_snapshot(state);
        publish_workspaces(display, &mut manager, &views, &outputs);
        if views.is_empty() {
            manager.resource.done();
        }
        state.desktop_protocols.workspace_managers.push(manager);
    }

    fn can_view(client: Client, _data: &()) -> bool {
        crate::handlers::security_context::unrestricted_client(&client)
    }
}

impl Dispatch<ExtWorkspaceManagerV1, ()> for Ferese {
    fn request(
        state: &mut Self,
        _client: &Client,
        resource: &ExtWorkspaceManagerV1,
        request: ext_workspace_manager_v1::Request,
        _data: &(),
        _display: &DisplayHandle,
        _init: &mut DataInit<'_, Self>,
    ) {
        match request {
            ext_workspace_manager_v1::Request::Commit => {
                let Some(manager) = state
                    .desktop_protocols
                    .workspace_managers
                    .iter_mut()
                    .find(|manager| &manager.resource == resource)
                else {
                    return;
                };
                let mut pending = std::mem::take(&mut manager.pending)
                    .into_iter()
                    .collect::<Vec<_>>();
                if state.session_lock.active() {
                    return;
                }
                pending.sort_by_key(|(output, _)| output.0);
                for (output, workspace) in pending {
                    if state.output_workspaces.output_for_workspace(workspace) != Some(output) {
                        continue;
                    }
                    // The existing action path owns layout, keyboard focus,
                    // viewport restoration, animations, and desktop IPC events.
                    state.activate_managed_workspace(workspace);
                }
                sync(state);
            }
            ext_workspace_manager_v1::Request::Stop => {
                state
                    .desktop_protocols
                    .workspace_managers
                    .retain(|manager| &manager.resource != resource);
                resource.finished();
            }
            _ => unreachable!(),
        }
    }

    fn destroyed(
        state: &mut Self,
        _client: ClientId,
        resource: &ExtWorkspaceManagerV1,
        _data: &(),
    ) {
        state
            .desktop_protocols
            .workspace_managers
            .retain(|manager| &manager.resource != resource);
    }
}

impl Dispatch<ExtWorkspaceGroupHandleV1, GroupData> for Ferese {
    fn request(
        _state: &mut Self,
        _client: &Client,
        _resource: &ExtWorkspaceGroupHandleV1,
        _request: ext_workspace_group_handle_v1::Request,
        _data: &GroupData,
        _display: &DisplayHandle,
        _init: &mut DataInit<'_, Self>,
    ) {
        // Ferese creates workspaces through its dynamic workspace policy. No
        // create capability is advertised, so that optional request is ignored.
    }
}

impl Dispatch<ExtWorkspaceHandleV1, WorkspaceData> for Ferese {
    fn request(
        state: &mut Self,
        _client: &Client,
        resource: &ExtWorkspaceHandleV1,
        request: ext_workspace_handle_v1::Request,
        data: &WorkspaceData,
        _display: &DisplayHandle,
        _init: &mut DataInit<'_, Self>,
    ) {
        if !matches!(request, ext_workspace_handle_v1::Request::Activate)
            || state.session_lock.active()
        {
            return;
        }
        let Some(manager) = state
            .desktop_protocols
            .workspace_managers
            .iter_mut()
            .find(|manager| manager.resource == data.manager)
        else {
            return;
        };
        let Some(workspace) = manager
            .workspaces
            .get(&data.id)
            .filter(|workspace| &workspace.resource == resource)
        else {
            return;
        };
        manager.pending.insert(workspace.group, data.id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{ClientState, dispatch_test_client, test_client_request as request};
    use smithay::output::{Mode, PhysicalProperties, Subpixel};
    use smithay::reexports::calloop::EventLoop;
    use smithay::wayland::security_context::SecurityContext;
    use std::io::Read;
    use std::os::unix::net::UnixStream;
    use std::sync::Arc;

    type Message = (u32, u16, Vec<u32>);

    fn messages(wire: &mut UnixStream) -> Vec<Message> {
        wire.set_nonblocking(true).unwrap();
        let mut bytes = Vec::new();
        let _ = wire.read_to_end(&mut bytes);
        wire.set_nonblocking(false).unwrap();
        let mut result = Vec::new();
        let mut offset = 0;
        while offset < bytes.len() {
            let object = u32::from_ne_bytes(bytes[offset..offset + 4].try_into().unwrap());
            let header = u32::from_ne_bytes(bytes[offset + 4..offset + 8].try_into().unwrap());
            let end = offset + (header >> 16) as usize;
            result.push((
                object,
                header as u16,
                bytes[offset + 8..end]
                    .chunks_exact(4)
                    .map(|word| u32::from_ne_bytes(word.try_into().unwrap()))
                    .collect(),
            ));
            offset = end;
        }
        result
    }

    fn output(state: &mut Ferese, name: &str, x: i32) -> Output {
        let output = Output::new(
            name.into(),
            PhysicalProperties {
                serial_number: String::new(),
                size: (300, 200).into(),
                subpixel: Subpixel::Unknown,
                make: "test".into(),
                model: "test".into(),
            },
        );
        output.change_current_state(
            Some(Mode {
                size: (800, 600).into(),
                refresh: 60_000,
            }),
            None,
            None,
            Some((x, 0).into()),
        );
        state.space.map_output(&output, (x, 0));
        state.register_output(&output, name.into());
        output
    }

    fn fixture() -> (EventLoop<'static, Ferese>, Ferese, Output) {
        let mut events = EventLoop::try_new().unwrap();
        let mut state = crate::startup_tests::state(&mut events);
        let output = output(&mut state, "TEST-1", 0);
        (events, state, output)
    }

    fn registry(
        events: &mut EventLoop<'static, Ferese>,
        state: &mut Ferese,
        sandboxed: bool,
    ) -> (UnixStream, Vec<(u32, String, u32)>) {
        let (creator_server, _creator_wire) = UnixStream::pair().unwrap();
        let creator = state
            .display_handle
            .insert_client(creator_server, Arc::new(ClientState::default()))
            .unwrap();
        let (server, mut wire) = UnixStream::pair().unwrap();
        state
            .display_handle
            .insert_client(
                server,
                Arc::new(ClientState {
                    security_context: sandboxed.then(|| SecurityContext {
                        sandbox_engine: Some("test".into()),
                        app_id: Some("test.app".into()),
                        instance_id: None,
                        creator_client_id: creator.id(),
                    }),
                    ..Default::default()
                }),
            )
            .unwrap();
        request(&mut wire, 1, 1, &[2], None);
        dispatch_test_client(events, state);
        let received = messages(&mut wire);
        let globals = received
            .into_iter()
            .filter_map(|(object, opcode, args)| {
                if object != 2 || opcode != 0 {
                    return None;
                }
                let length = args[1] as usize;
                let bytes = args[2..]
                    .iter()
                    .flat_map(|word| word.to_ne_bytes())
                    .collect::<Vec<_>>();
                let interface = String::from_utf8(bytes[..length - 1].to_vec()).unwrap();
                Some((args[0], interface, *args.last().unwrap()))
            })
            .collect();
        (wire, globals)
    }

    fn bind(
        events: &mut EventLoop<'static, Ferese>,
        state: &mut Ferese,
        interface: &str,
    ) -> UnixStream {
        let (mut wire, globals) = registry(events, state, false);
        let (name, _, version) = globals
            .iter()
            .find(|(_, name, _)| name == interface)
            .unwrap();
        let mut encoded = interface.as_bytes().to_vec();
        encoded.push(0);
        let length = encoded.len() as u32;
        encoded.resize(encoded.len().next_multiple_of(4), 0);
        let mut args = vec![*name, length];
        args.extend(
            encoded
                .chunks_exact(4)
                .map(|word| u32::from_ne_bytes(word.try_into().unwrap())),
        );
        args.extend([*version, 3]);
        request(&mut wire, 2, 0, &args, None);
        dispatch_test_client(events, state);
        wire
    }

    #[test]
    fn discovery_globals_are_filtered_from_sandbox_clients() {
        if !crate::startup_tests::private_runtime(
            "desktop_protocols::tests::discovery_globals_are_filtered_from_sandbox_clients",
        ) {
            return;
        }
        let (mut events, mut state, _output) = fixture();
        let (_wire, unrestricted) = registry(&mut events, &mut state, false);
        let (_sandbox, restricted) = registry(&mut events, &mut state, true);
        for interface in [
            "ext_foreign_toplevel_list_v1",
            "ext_workspace_manager_v1",
            "zwlr_output_manager_v1",
        ] {
            assert!(unrestricted.iter().any(|(_, name, _)| name == interface));
            assert!(!restricted.iter().any(|(_, name, _)| name == interface));
        }
    }

    #[test]
    fn foreign_toplevels_keep_identity_across_metadata_updates_and_close_on_unmap() {
        if !crate::startup_tests::private_runtime(
            "desktop_protocols::tests::foreign_toplevels_keep_identity_across_metadata_updates_and_close_on_unmap",
        ) {
            return;
        }
        let (mut events, mut state, _output) = fixture();
        let (window, mut app_wire) = crate::state::test_window(&mut state, &mut events, 0xff123456);
        let id = state.windows.ids()[&window];
        let mut wire = bind(&mut events, &mut state, "ext_foreign_toplevel_list_v1");
        let before = state.desktop_protocols.toplevels[&id].handle.identifier();
        let resource = messages(&mut wire)
            .iter()
            .find(|(object, opcode, _)| *object == 3 && *opcode == 0)
            .unwrap()
            .2[0];
        let title = b"New title\0";
        let mut encoded = title.to_vec();
        encoded.resize(encoded.len().next_multiple_of(4), 0);
        let mut args = vec![title.len() as u32];
        args.extend(
            encoded
                .chunks_exact(4)
                .map(|word| u32::from_ne_bytes(word.try_into().unwrap())),
        );
        request(&mut app_wire, 4, 2, &args, None);
        dispatch_test_client(&mut events, &mut state);
        sync(&mut state);
        state.display_handle.flush_clients().unwrap();
        assert_eq!(
            state.desktop_protocols.toplevels[&id].handle.identifier(),
            before
        );
        let updates = messages(&mut wire);
        assert!(
            updates
                .iter()
                .any(|(object, opcode, _)| *object == resource && *opcode == 2)
        );
        request(&mut app_wire, 2, 1, &[0, 0, 0], None); // wl_surface.attach(NULL)
        request(&mut app_wire, 2, 6, &[], None);
        dispatch_test_client(&mut events, &mut state);
        sync(&mut state);
        state.display_handle.flush_clients().unwrap();
        assert!(!state.desktop_protocols.toplevels.contains_key(&id));
        assert!(
            messages(&mut wire)
                .iter()
                .any(|(object, opcode, _)| *object == resource && *opcode == 0)
        );
    }

    #[test]
    fn privacy_rules_withdraw_foreign_handles_and_new_bindings_receive_no_title() {
        if !crate::startup_tests::private_runtime(
            "desktop_protocols::tests::privacy_rules_withdraw_foreign_handles_and_new_bindings_receive_no_title",
        ) {
            return;
        }
        let (mut events, mut state, _output) = fixture();
        let (window, _app) = crate::state::test_window_with_app_id(
            &mut state,
            &mut events,
            0xff123456,
            "private.app",
        );
        let id = state.windows.ids()[&window];
        let mut wire = bind(&mut events, &mut state, "ext_foreign_toplevel_list_v1");
        assert!(state.desktop_protocols.toplevels.contains_key(&id));
        let _ = messages(&mut wire);
        state
            .reload_config_source(
                "window-rule app-id=\"private.app\" block-out-from-screencasts=#true".into(),
            )
            .unwrap();
        sync(&mut state);
        assert!(!state.desktop_protocols.toplevels.contains_key(&id));
        let mut newcomer = bind(&mut events, &mut state, "ext_foreign_toplevel_list_v1");
        assert!(
            !messages(&mut newcomer)
                .iter()
                .any(|(object, opcode, _)| *object == 3 && *opcode == 0)
        );
    }

    #[test]
    fn workspace_activation_waits_for_commit_and_uses_stable_handles() {
        if !crate::startup_tests::private_runtime(
            "desktop_protocols::tests::workspace_activation_waits_for_commit_and_uses_stable_handles",
        ) {
            return;
        }
        let (mut events, mut state, _output) = fixture();
        let (_window, _app) = crate::state::test_window(&mut state, &mut events, 0xff123456);
        let mut wire = bind(&mut events, &mut state, "ext_workspace_manager_v1");
        let before = state.workspaces.active_id();
        let manager = &state.desktop_protocols.workspace_managers[0];
        let (target, target_resource) = manager
            .workspaces
            .iter()
            .find(|(_, workspace)| !workspace.active)
            .map(|(id, workspace)| (*id, Resource::id(&workspace.resource).protocol_id()))
            .unwrap();
        request(&mut wire, target_resource, 1, &[], None);
        dispatch_test_client(&mut events, &mut state);
        assert_eq!(state.workspaces.active_id(), before);
        request(&mut wire, 3, 0, &[], None);
        dispatch_test_client(&mut events, &mut state);
        assert_eq!(state.workspaces.active_id(), target);
        assert_eq!(
            Resource::id(
                &state.desktop_protocols.workspace_managers[0].workspaces[&target].resource
            )
            .protocol_id(),
            target_resource
        );
        let responses = messages(&mut wire);
        assert!(
            responses
                .iter()
                .any(|(object, opcode, _)| *object == 3 && *opcode == 2)
        );
    }

    #[test]
    fn stopping_workspace_manager_discards_pending_actions() {
        if !crate::startup_tests::private_runtime(
            "desktop_protocols::tests::stopping_workspace_manager_discards_pending_actions",
        ) {
            return;
        }
        let (mut events, mut state, _output) = fixture();
        let (_window, _app) = crate::state::test_window(&mut state, &mut events, 0xff123456);
        let mut wire = bind(&mut events, &mut state, "ext_workspace_manager_v1");
        let before = state.workspaces.active_id();
        let target = Resource::id(
            &state.desktop_protocols.workspace_managers[0]
                .workspaces
                .values()
                .find(|workspace| !workspace.active)
                .unwrap()
                .resource,
        )
        .protocol_id();
        request(&mut wire, target, 1, &[], None);
        request(&mut wire, 3, 1, &[], None);
        dispatch_test_client(&mut events, &mut state);
        assert!(state.desktop_protocols.workspace_managers.is_empty());
        assert_eq!(state.workspaces.active_id(), before);
        assert!(
            messages(&mut wire)
                .iter()
                .any(|(object, opcode, _)| *object == 3 && *opcode == 3)
        );
    }

    #[test]
    fn locking_withdraws_desktop_metadata_and_discards_pending_activation() {
        if !crate::startup_tests::private_runtime(
            "desktop_protocols::tests::locking_withdraws_desktop_metadata_and_discards_pending_activation",
        ) {
            return;
        }
        let (mut events, mut state, _output) = fixture();
        let (_window, _app) = crate::state::test_window(&mut state, &mut events, 0xff123456);
        let mut foreign = bind(&mut events, &mut state, "ext_foreign_toplevel_list_v1");
        let mut workspaces = bind(&mut events, &mut state, "ext_workspace_manager_v1");
        let before = state.workspaces.active_id();
        let target = Resource::id(
            &state.desktop_protocols.workspace_managers[0]
                .workspaces
                .values()
                .find(|workspace| !workspace.active)
                .unwrap()
                .resource,
        )
        .protocol_id();
        request(&mut workspaces, target, 1, &[], None);
        dispatch_test_client(&mut events, &mut state);
        let (server, mut locker) = UnixStream::pair().unwrap();
        let client = state
            .display_handle
            .insert_client(server, Arc::new(ClientState::default()))
            .unwrap();
        use smithay::reexports::wayland_protocols::ext::session_lock::v1::server::ext_session_lock_manager_v1::ExtSessionLockManagerV1;
        let manager = client
            .create_resource::<ExtSessionLockManagerV1, _, Ferese>(
                &state.display_handle,
                1,
                smithay::wayland::GlobalData,
            )
            .unwrap();
        request(&mut locker, manager.id().protocol_id(), 1, &[2], None);
        dispatch_test_client(&mut events, &mut state);
        assert!(state.session_lock.active());
        sync(&mut state);
        request(&mut workspaces, 3, 0, &[], None);
        dispatch_test_client(&mut events, &mut state);
        assert_eq!(state.workspaces.active_id(), before);
        assert!(state.desktop_protocols.toplevels.is_empty());
        assert!(
            state.desktop_protocols.workspace_managers[0]
                .workspaces
                .is_empty()
        );
        let _ = messages(&mut foreign);
        let mut newcomer = bind(&mut events, &mut state, "ext_foreign_toplevel_list_v1");
        assert!(
            !messages(&mut newcomer)
                .iter()
                .any(|(object, opcode, _)| *object == 3 && *opcode == 0)
        );
    }

    #[test]
    fn unplugging_a_group_preserves_migrated_workspace_handles() {
        if !crate::startup_tests::private_runtime(
            "desktop_protocols::tests::unplugging_a_group_preserves_migrated_workspace_handles",
        ) {
            return;
        }
        let (mut events, mut state, first) = fixture();
        let (_window, _app) = crate::state::test_window(&mut state, &mut events, 0xff123456);
        let workspace = state.workspaces.active_id();
        let second = output(&mut state, "TEST-2", 800);
        let mut wire = bind(&mut events, &mut state, "ext_workspace_manager_v1");
        let old = Resource::id(
            &state.desktop_protocols.workspace_managers[0].workspaces[&workspace].resource,
        );
        let old_group = state.output_id(&first).unwrap();
        let second_id = state.output_id(&second).unwrap();
        state.unregister_output(&first);
        sync(&mut state);
        state.display_handle.flush_clients().unwrap();
        let manager = &state.desktop_protocols.workspace_managers[0];
        assert!(!manager.groups.contains_key(&old_group));
        assert_eq!(manager.workspaces[&workspace].group, second_id);
        assert_eq!(Resource::id(&manager.workspaces[&workspace].resource), old);
        assert!(
            messages(&mut wire)
                .iter()
                .any(|(object, opcode, _)| *object == 3 && *opcode == 2)
        );
    }
}
