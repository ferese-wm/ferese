use ferese_core::{OutputId, WorkspaceId};
use ferese_layout::WindowId;
use ferese_protocols::shell::v1::server::ferese_shell_manager_v1::FereseShellManagerV1;
use ferese_protocols::shell::v1::server::ferese_shell_v1::FereseShellV1;
use ferese_protocols::shell::v1::server::{ferese_shell_manager_v1, ferese_shell_v1};
use smithay::reexports::wayland_server::{Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource};

use crate::Ferese;
use crate::private_client::ClientCapabilities;
use crate::state::ClientState;

pub(crate) fn init_global(display: &DisplayHandle) {
    display.create_global::<Ferese, FereseShellManagerV1, _>(8, ());
}

pub(crate) fn config_chunks(source: &str) -> Vec<&str> {
    let mut remaining = source;
    let mut chunks = Vec::new();
    while !remaining.is_empty() {
        let mut boundary = remaining.len().min(1024);
        while !remaining.is_char_boundary(boundary) {
            boundary -= 1;
        }
        chunks.push(&remaining[..boundary]);
        remaining = &remaining[boundary..];
    }
    chunks
}

pub(crate) fn send_shell_config(shell: &FereseShellV1, source: &str) {
    shell.config_begin();
    for chunk in config_chunks(source) {
        shell.config_chunk(chunk.to_owned());
    }
    shell.config_end();
}

impl GlobalDispatch<FereseShellManagerV1, ()> for Ferese {
    fn bind(
        _state: &mut Self,
        _display: &DisplayHandle,
        _client: &Client,
        resource: New<FereseShellManagerV1>,
        _global_data: &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        data_init.init(resource, ());
    }

    fn can_view(client: Client, _global_data: &()) -> bool {
        client
            .get_data::<ClientState>()
            .is_some_and(|state| state.capabilities.contains(ClientCapabilities::SHELL_CONTROL))
    }
}

impl Dispatch<FereseShellManagerV1, ()> for Ferese {
    fn request(
        state: &mut Self,
        _client: &Client,
        _manager: &FereseShellManagerV1,
        request: ferese_shell_manager_v1::Request,
        _data: &(),
        _display: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            ferese_shell_manager_v1::Request::GetShell { id } => {
                let shell = data_init.init(id, ());

                shell.capabilities(1);
                if shell.version() >= 2
                    && let Some(source) = &state.config_source
                {
                    send_shell_config(&shell, source);
                }
                shell.overview_state(u32::from(state.overview.is_active()));
                state.send_shell_snapshot(&shell);
                state.shell_resources.push(shell.downgrade());
            }
            ferese_shell_manager_v1::Request::Destroy => {}
            _ => unreachable!(),
        }
    }
}

impl Dispatch<FereseShellV1, ()> for Ferese {
    fn request(
        state: &mut Self,
        _client: &Client,
        shell: &FereseShellV1,
        request: ferese_shell_v1::Request,
        _data: &(),
        _display: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        if state.session_lock.active() {
            return;
        }
        match request {
            ferese_shell_v1::Request::ActivateWindow { window_hi, window_lo } => state.handle_shell_window_request(
                shell,
                ferese_shell_v1::FailedRequest::ActivateWindow,
                join_id(window_hi, window_lo),
                ShellWindowAction::Activate,
            ),
            ferese_shell_v1::Request::CloseWindow { window_hi, window_lo } => state.handle_shell_window_request(
                shell,
                ferese_shell_v1::FailedRequest::CloseWindow,
                join_id(window_hi, window_lo),
                ShellWindowAction::Close,
            ),
            ferese_shell_v1::Request::ActivateWorkspace {
                workspace_hi,
                workspace_lo,
            } => {
                let workspace = WorkspaceId(join_u64(workspace_hi, workspace_lo));

                if !state.activate_managed_workspace(workspace) {
                    send_request_failed(shell, ferese_shell_v1::FailedRequest::ActivateWorkspace, workspace.0);
                }
            }
            ferese_shell_v1::Request::MoveWindowToWorkspace {
                window_hi,
                window_lo,
                workspace_hi,
                workspace_lo,
            } => {
                let window = join_id(window_hi, window_lo);
                let workspace = WorkspaceId(join_u64(workspace_hi, workspace_lo));
                if !state.move_managed_window_to_workspace(window, workspace) {
                    send_request_failed(shell, ferese_shell_v1::FailedRequest::MoveWindowToWorkspace, window.0);
                }
            }
            ferese_shell_v1::Request::ConfirmLogout { serial } => {
                let snapshot = state.portal_session.snapshot();
                let token = snapshot["query-token"].as_u64().unwrap_or(0) as u32;
                let revision = snapshot["inhibitor-revision"].as_u64().unwrap_or(0) as u32;
                state.confirm_portal_logout(shell, serial, token, revision, false);
            }
            ferese_shell_v1::Request::ConfirmLogoutWithInhibitors {
                serial,
                inhibitor_revision,
                query_token,
                force,
            } => {
                if force <= 1 {
                    state.confirm_portal_logout(shell, serial, query_token, inhibitor_revision, force == 1);
                }
            }
            ferese_shell_v1::Request::CancelLogout { serial } => {
                if state.logout_owner.as_ref() == Some(&shell.id()) && state.pending_logout == Some(serial) {
                    state.cancel_logout_confirmation();
                }
            }
            ferese_shell_v1::Request::EnterOverview => state.set_overview_active(true),
            ferese_shell_v1::Request::ExitOverview => state.set_overview_active(false),
            ferese_shell_v1::Request::SelectOverviewWindow { window_hi, window_lo } => {
                let id = join_id(window_hi, window_lo);

                if !state.select_overview_window(id) {
                    send_request_failed(shell, ferese_shell_v1::FailedRequest::SelectOverviewWindow, id.0);
                }
            }
            ferese_shell_v1::Request::Destroy => {
                if state.logout_owner.as_ref() == Some(&shell.id()) {
                    state.cancel_logout_confirmation();
                }
            }
            _ => unreachable!(),
        }
    }

    fn destroyed(
        state: &mut Self,
        _client: smithay::reexports::wayland_server::backend::ClientId,
        shell: &FereseShellV1,
        _data: &(),
    ) {
        if state.logout_owner.as_ref() == Some(&shell.id()) {
            state.cancel_logout_confirmation();
        }
    }
}

fn logout_confirmation_matches(pending: Option<u32>, query: Option<u32>, serial: u32, token: u32) -> bool {
    pending == Some(serial) && query == Some(token)
}

fn consume_logout_confirmation(pending: &mut Option<u32>, serial: u32) -> bool {
    if *pending == Some(serial) {
        *pending = None;
        true
    } else {
        false
    }
}

impl Ferese {
    fn confirm_portal_logout(&mut self, shell: &FereseShellV1, serial: u32, token: u32, revision: u32, force: bool) {
        if self.logout_owner.as_ref() != Some(&shell.id())
            || !logout_confirmation_matches(self.pending_logout, self.logout_query, serial, token)
        {
            return;
        }
        match self.portal_session.commit_end(token, revision, force) {
            Ok(()) => {
                consume_logout_confirmation(&mut self.pending_logout, serial);
                self.logout_owner = None;
                self.logout_query = None;
                self.end_portal_session();
            }
            Err(error) => {
                tracing::debug!(%error, "logout requires a new confirmation");
                self.cancel_logout_confirmation();
                self.request_logout_confirmation();
            }
        }
    }

    pub(crate) fn notify_monitor_state(&self) {
        for shell in self
            .shell_resources
            .iter()
            .filter_map(|shell| shell.upgrade().ok())
            .filter(|shell| shell.version() >= 6)
        {
            shell.monitor_state_changed();
        }
    }

    pub(crate) fn toggle_display_mode(&mut self) -> bool {
        if self.session_lock.active() {
            return false;
        }
        let Some(shell) = self
            .shell_resources
            .iter()
            .filter_map(|shell| shell.upgrade().ok())
            .find(|shell| shell.version() >= 6)
        else {
            tracing::warn!("display mode chooser requires the updated Ferese shell");
            return false;
        };
        shell.toggle_display_mode(self.focused_output().map_or_else(String::new, |output| output.name()));
        true
    }

    pub(crate) fn toggle_keybinding_guide(&mut self) -> bool {
        if self.session_lock.active() {
            return false;
        }
        let Some(shell) = self
            .shell_resources
            .iter()
            .filter_map(|shell| shell.upgrade().ok())
            .find(|shell| shell.version() >= 5)
        else {
            tracing::warn!("shortcut hint requires the updated Ferese shell");
            return false;
        };
        let output = self.focused_output().map_or_else(String::new, |output| output.name());
        shell.toggle_keybinding_guide(output);
        true
    }

    pub(crate) fn request_logout_confirmation(&mut self) {
        if self.session_lock.active() {
            return;
        }
        let shells = self
            .shell_resources
            .iter()
            .filter_map(|shell| shell.upgrade().ok())
            .filter(|shell| shell.version() >= 3)
            .collect::<Vec<_>>();
        if shells.is_empty() {
            tracing::warn!("logout confirmation requires the updated Ferese shell");
            return;
        }
        self.cancel_logout_confirmation();
        let serial = u32::from(smithay::utils::SERIAL_COUNTER.next_serial());
        let shell = &shells[0];
        self.logout_query = Some(self.portal_session.begin_query());
        self.pending_logout = Some(serial);
        self.logout_owner = Some(shell.id());
        let output_name = self.focused_output().map_or_else(String::new, |output| output.name());
        shell.logout_requested(serial, output_name);
    }

    pub(crate) fn cancel_logout_confirmation(&mut self) {
        if let Some(token) = self.logout_query.take() {
            self.portal_session.cancel_query(token);
        }
        if let Some(serial) = self.pending_logout.take() {
            for shell in self.shell_resources.iter().filter_map(|shell| shell.upgrade().ok()) {
                if self.logout_owner.as_ref() == Some(&shell.id()) && shell.version() >= 3 {
                    shell.logout_cancelled(serial);
                }
            }
        }
        self.logout_owner = None;
    }

    pub(crate) fn send_shell_snapshots(&mut self) {
        if let Some(transition) = self.desktop_transition.as_mut() {
            transition.shell_pending = true;
            return;
        }
        if self
            .direct_backend
            .as_ref()
            .is_some_and(|backend| backend.reconciling())
        {
            return;
        }

        self.shell_resources.retain(|resource| resource.upgrade().is_ok());
        if self.shell_resources.is_empty() {
            self.last_shell_snapshot = None;
            return;
        }
        if self
            .last_shell_snapshot
            .as_ref()
            .is_some_and(|snapshot| self.shell_snapshot_matches(snapshot))
        {
            return;
        }

        let snapshot = self.shell_snapshot();
        for resource in &self.shell_resources {
            if let Ok(shell) = resource.upgrade() {
                self.shell_snapshot_serial = self.shell_snapshot_serial.wrapping_add(1);
                snapshot.send(&shell, self.shell_snapshot_serial);
            }
        }
        self.last_shell_snapshot = Some(snapshot);
    }

    fn shell_snapshot_matches(&self, snapshot: &ShellSnapshot) -> bool {
        if snapshot.workspace_preview_revision != self.workspace_preview_revision {
            return false;
        }
        let mut outputs = self.output_workspaces.connected_outputs();

        for previous in &snapshot.outputs {
            if outputs.next() != Some(previous.id)
                || self.output_names.get(&previous.id) != Some(&previous.name)
                || self.output_workspaces.active_workspace(previous.id) != Some(previous.active_workspace)
                || self
                    .output_ids
                    .iter()
                    .find(|(_, id)| **id == previous.id)
                    .and_then(|(output, _)| self.output_bounds_for(output))
                    != previous.bounds
                || (self.output_workspaces.focused_output() == Some(previous.id)) != previous.focused
            {
                return false;
            }
        }

        if outputs.next().is_some() {
            return false;
        }

        let mut workspaces = self.output_workspaces.workspace_views_iter(&self.workspaces);

        for previous in &snapshot.workspaces {
            let Some(view) = workspaces.next() else {
                return false;
            };

            if previous.id != view.id
                || previous.output != Some(view.output)
                || previous.index != view.index
                || previous.window_count != view.window_count as u32
                || previous.visible != view.visible
                || previous.focused != view.focused
            {
                return false;
            }
        }

        if workspaces.next().is_some() {
            return false;
        }

        let mut windows = self.windows.ordered_ids().filter_map(|id| self.shell_window_data(id));

        for previous in &snapshot.windows {
            let Some((id, workspace, app_id, title, state)) = windows.next() else {
                return false;
            };

            if previous.id != id
                || previous.workspace != workspace
                || previous.app_id != app_id
                || previous.title != title
                || previous.state != state
                || previous.source_geometry != self.shell_window_geometry(id)
            {
                return false;
            }
        }

        windows.next().is_none()
    }

    fn shell_snapshot(&self) -> ShellSnapshot {
        ShellSnapshot {
            workspace_preview_revision: self.workspace_preview_revision,
            outputs: self.output_snapshots(),
            workspaces: self.workspace_snapshots(),
            windows: self.managed_window_snapshots(),
        }
    }

    fn send_shell_snapshot(&mut self, shell: &FereseShellV1) {
        if let Some(transition) = self.desktop_transition.as_mut() {
            transition.shell_pending = true;
            self.last_shell_snapshot = None;
            return;
        }
        // New subscribers always receive a complete snapshot. Do not update the
        // broadcast cache here: existing subscribers may still need this state.
        self.shell_snapshot_serial = self.shell_snapshot_serial.wrapping_add(1);
        self.shell_snapshot().send(shell, self.shell_snapshot_serial);
    }

    fn output_snapshots(&self) -> Vec<OutputSnapshot> {
        let focused = self.output_workspaces.focused_output();
        let mut outputs = self
            .output_ids
            .iter()
            .filter_map(|(output, id)| {
                Some(OutputSnapshot {
                    id: *id,
                    name: output.name(),
                    active_workspace: self.output_workspaces.active_workspace(*id)?,
                    focused: focused == Some(*id),
                    bounds: self.output_bounds_for(output),
                })
            })
            .collect::<Vec<_>>();

        outputs.sort_by_key(|output| output.id.0);
        outputs
    }

    fn workspace_snapshots(&self) -> Vec<WorkspaceSnapshot> {
        self.output_workspaces
            .workspace_views_iter(&self.workspaces)
            .map(|view| WorkspaceSnapshot {
                id: view.id,
                output: Some(view.output),
                name: view.index.to_string(),
                index: view.index,
                window_count: view.window_count as u32,
                visible: view.visible,
                focused: view.focused,
            })
            .collect()
    }

    fn shell_window_data(&self, id: WindowId) -> Option<(WindowId, u64, &str, &str, ferese_shell_v1::WindowState)> {
        let workspace = self.workspaces.workspace_for_window(id)?;
        let record = self.windows.record(id)?;
        self.windows.window(id)?.toplevel()?;
        let mut state = ferese_shell_v1::WindowState::empty();

        if record.maximized {
            state |= ferese_shell_v1::WindowState::Maximized;
        }
        if self.focused_window == Some(id) {
            state |= ferese_shell_v1::WindowState::Focused;
        }

        if self
            .workspaces
            .workspace(workspace)
            .is_some_and(|workspace| workspace.fullscreen == Some(id))
        {
            state |= ferese_shell_v1::WindowState::Fullscreen;
        }

        if matches!(
            self.workspaces.placement(id),
            Some(ferese_core::WindowPlacement::Floating { .. })
        ) {
            state |= ferese_shell_v1::WindowState::Floating;
        }

        Some((id, workspace.0, &record.app_id, &record.title, state))
    }

    fn shell_window_geometry(&self, id: WindowId) -> Option<ferese_layout::Rect> {
        let rect = self.windows.geometry(&id)?.visual.target;
        (rect.width > 0. && rect.height > 0.).then_some(rect)
    }

    fn managed_window_snapshots(&self) -> Vec<ManagedWindowSnapshot> {
        let previews = self.workspace_preview_rects();
        self.windows
            .ordered_ids()
            .filter_map(|id| self.shell_window_data(id))
            .map(|(id, workspace, app_id, title, state)| ManagedWindowSnapshot {
                id,
                workspace,
                app_id: app_id.to_owned(),
                title: title.to_owned(),
                state,
                source_geometry: self.shell_window_geometry(id),
                geometry: previews.get(&id).copied().or_else(|| self.shell_window_geometry(id)),
            })
            .collect()
    }

    fn handle_shell_window_request(
        &mut self,
        shell: &FereseShellV1,
        request: ferese_shell_v1::FailedRequest,
        id: WindowId,
        action: ShellWindowAction,
    ) {
        let succeeded = match action {
            ShellWindowAction::Activate => self.activate_managed_window(id),
            ShellWindowAction::Close => self.close_managed_window(id),
        };

        if !succeeded {
            send_request_failed(shell, request, id.0);
        }
    }

    pub(crate) fn notify_overview_state(&mut self) {
        let active = self.overview.is_active();

        self.shell_resources.retain(|resource| {
            resource.upgrade().is_ok_and(|shell| {
                shell.overview_state(u32::from(active));
                true
            })
        });
    }
}

#[derive(Clone, Copy)]
enum ShellWindowAction {
    Activate,
    Close,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ShellSnapshot {
    workspace_preview_revision: u64,
    outputs: Vec<OutputSnapshot>,
    workspaces: Vec<WorkspaceSnapshot>,
    windows: Vec<ManagedWindowSnapshot>,
}

impl ShellSnapshot {
    fn send(&self, shell: &FereseShellV1, serial: u32) {
        shell.snapshot_begin(serial);
        for output in &self.outputs {
            let (output_hi, output_lo) = split_id(output.id.0);
            let (workspace_hi, workspace_lo) = split_id(output.active_workspace.0);

            shell.output(
                output_hi,
                output_lo,
                output.name.clone(),
                workspace_hi,
                workspace_lo,
                u32::from(output.focused),
            );
        }
        for workspace in &self.workspaces {
            let (workspace_hi, workspace_lo) = split_id(workspace.id.0);
            let (output_hi, output_lo) = workspace.output.map(|output| split_id(output.0)).unwrap_or_default();

            shell.workspace(
                workspace_hi,
                workspace_lo,
                output_hi,
                output_lo,
                workspace.name.clone(),
                workspace.index,
                workspace.window_count,
                u32::from(workspace.visible),
                u32::from(workspace.focused),
            );
        }
        for snapshot in &self.windows {
            let (window_hi, window_lo) = split_id(snapshot.id.0);
            let (workspace_hi, workspace_lo) = split_id(snapshot.workspace);

            shell.window(
                window_hi,
                window_lo,
                workspace_hi,
                workspace_lo,
                snapshot.app_id.clone(),
                snapshot.title.clone(),
                snapshot.state,
            );
            if shell.version() >= 8
                && let Some(rect) = snapshot.geometry
            {
                shell.window_geometry(window_hi, window_lo, rect.x, rect.y, rect.width, rect.height);
            }
        }
        shell.snapshot_end(serial);
    }
}

#[derive(Clone, Debug, PartialEq)]
struct ManagedWindowSnapshot {
    id: WindowId,
    workspace: u64,
    app_id: String,
    title: String,
    state: ferese_shell_v1::WindowState,
    geometry: Option<ferese_layout::Rect>,
    source_geometry: Option<ferese_layout::Rect>,
}

#[derive(Clone, Debug, PartialEq)]
struct OutputSnapshot {
    bounds: Option<ferese_layout::Rect>,
    id: OutputId,
    name: String,
    active_workspace: WorkspaceId,
    focused: bool,
}

#[derive(Clone, Debug, PartialEq)]
struct WorkspaceSnapshot {
    id: WorkspaceId,
    output: Option<OutputId>,
    name: String,
    index: u32,
    window_count: u32,
    visible: bool,
    focused: bool,
}

fn split_id(id: u64) -> (u32, u32) {
    ((id >> 32) as u32, id as u32)
}

fn join_id(hi: u32, lo: u32) -> WindowId {
    WindowId(join_u64(hi, lo))
}

fn join_u64(hi: u32, lo: u32) -> u64 {
    (u64::from(hi) << 32) | u64::from(lo)
}

fn send_request_failed(shell: &FereseShellV1, request: ferese_shell_v1::FailedRequest, id: u64) {
    let (object_hi, object_lo) = split_id(id);

    shell.request_failed(
        request,
        object_hi,
        object_lo,
        "unknown or unavailable shell object".to_owned(),
    );
}

#[cfg(test)]
mod tests {
    #[test]
    #[ignore = "requires a private XDG_RUNTIME_DIR and permission to bind test sockets"]
    fn snapshot_precheck_detects_membership_selection_and_output_changes() {
        use smithay::output::{Output, PhysicalProperties, Subpixel};
        use smithay::reexports::calloop::EventLoop;
        use smithay::reexports::wayland_server::Display;

        use super::*;

        let runtime = std::path::PathBuf::from(std::env::var_os("XDG_RUNTIME_DIR").unwrap());
        assert!(runtime.starts_with(std::env::temp_dir()));
        let mut event_loop = EventLoop::try_new().unwrap();
        let config = crate::config::Config::default().runtime_config().unwrap();
        let mut state = Ferese::new(&mut event_loop, Display::new().unwrap(), config).unwrap();
        let output = Output::new(
            "snapshot-test".into(),
            PhysicalProperties {
                size: (0, 0).into(),
                subpixel: Subpixel::Unknown,
                make: "test".into(),
                model: "test".into(),
            },
        );
        output.change_current_state(
            Some(smithay::output::Mode {
                size: (800, 600).into(),
                refresh: 60_000,
            }),
            Some(smithay::utils::Transform::Normal),
            None,
            Some((0, 0).into()),
        );
        state.space.map_output(&output, (0, 0));
        state.register_output(&output, "snapshot-test".into());
        let original = state.shell_snapshot();

        for _ in 0..100 {
            assert!(state.shell_snapshot_matches(&original));
        }

        state
            .workspaces
            .insert_window(WindowId(99), ferese_layout::Axis::Horizontal, 0.5)
            .unwrap();
        assert!(!state.shell_snapshot_matches(&original));
        let occupied = state.shell_snapshot();
        assert!(state.shell_snapshot_matches(&occupied));
        let output_id = state.output_ids[&output];
        let next = state.workspaces.create_workspace();
        state.output_workspaces.assign_workspace(output_id, next).unwrap();
        assert!(!state.shell_snapshot_matches(&occupied));
        let assigned = state.shell_snapshot();
        state.output_workspaces.switch_workspace(output_id, next).unwrap();
        assert!(!state.shell_snapshot_matches(&assigned));
        let selected = state.shell_snapshot();
        assert!(state.shell_snapshot_matches(&selected));
        assert_eq!(state.output_workspaces.active_workspace(output_id), Some(next));
        assert_eq!(state.workspaces.workspace(next).unwrap().layout.window_ids().count(), 0);
        let hidden_before = state.workspace_preview_rects()[&WindowId(99)];
        state
            .reload_config_source("layout { inner-gap 30; outer-gap 40; smart-gaps #false; }".into())
            .unwrap();
        assert!(
            !state.shell_snapshot_matches(&selected),
            "hidden previews must invalidate even with no visible window geometry to change"
        );
        assert_ne!(state.workspace_preview_rects()[&WindowId(99)], hidden_before);
        let refreshed = state.shell_snapshot();
        assert!(state.shell_snapshot_matches(&refreshed));
        state
            .reload_config_source(
                "layout { inner-gap 30; outer-gap 40; smart-gaps #false; }; input { repeat-rate 35; }".into(),
            )
            .unwrap();
        assert_eq!(
            state.workspace_preview_revision, refreshed.workspace_preview_revision,
            "unrelated settings do not invalidate layouts"
        );
        state.unregister_output(&output);
        assert!(!state.shell_snapshot_matches(&refreshed));
    }

    #[test]
    fn logout_serial_cannot_authorize_a_different_session_query() {
        assert!(super::logout_confirmation_matches(Some(42), Some(9), 42, 9));
        assert!(!super::logout_confirmation_matches(Some(42), Some(9), 42, 10));
        assert!(!super::logout_confirmation_matches(None, Some(9), 42, 9));
    }

    #[test]
    fn logout_requires_the_current_confirmation_and_can_be_cancelled() {
        let mut pending = Some(42);
        assert!(!super::consume_logout_confirmation(&mut pending, 41));
        assert_eq!(pending, Some(42));
        assert!(super::consume_logout_confirmation(&mut pending, 42));
        assert_eq!(pending, None);
        assert!(!super::consume_logout_confirmation(&mut pending, 42));
    }

    use super::*;

    #[test]
    fn snapshot_deduplication_preserves_shell_visible_changes() {
        let original = ShellSnapshot {
            workspace_preview_revision: 0,
            outputs: vec![OutputSnapshot {
                bounds: None,
                id: OutputId(1),
                name: "display".into(),
                active_workspace: WorkspaceId(1),
                focused: true,
            }],
            workspaces: vec![WorkspaceSnapshot {
                id: WorkspaceId(1),
                output: Some(OutputId(1)),
                name: "1".into(),
                index: 1,
                window_count: 1,
                visible: true,
                focused: true,
            }],
            windows: vec![ManagedWindowSnapshot {
                id: WindowId(1),
                workspace: 1,
                app_id: "terminal".into(),
                title: "shell".into(),
                state: ferese_shell_v1::WindowState::Focused,
                geometry: None,
                source_geometry: None,
            }],
        };
        assert_eq!(original, original.clone());
        let changes: &[fn(&mut ShellSnapshot)] = &[
            |s| s.workspace_preview_revision += 1,
            |s| s.outputs[0].focused = false,
            |s| s.outputs[0].active_workspace = WorkspaceId(2),
            |s| s.outputs.clear(),
            |s| s.workspaces[0].output = None,
            |s| s.workspaces[0].visible = false,
            |s| s.workspaces[0].focused = false,
            |s| s.workspaces[0].index = 2,
            |s| s.workspaces[0].window_count = 0,
            |s| s.workspaces[0].name = "renamed".into(),
            |s| s.windows[0].workspace = 2,
            |s| s.windows[0].title = "new title".into(),
            |s| s.windows[0].app_id = "new app".into(),
            |s| s.windows[0].state |= ferese_shell_v1::WindowState::Fullscreen,
            |s| s.windows.clear(),
        ];
        for change in changes {
            let mut updated = original.clone();
            change(&mut updated);
            assert_ne!(original, updated);
        }
    }

    #[test]
    fn config_chunks_are_bounded_and_preserve_unicode() {
        let source = "abc🌲é".repeat(9000);
        let chunks = config_chunks(&source);
        assert!(chunks.iter().all(|chunk| !chunk.is_empty() && chunk.len() <= 1024));
        assert_eq!(chunks.concat(), source);
        assert!(config_chunks("").is_empty());
    }

    #[test]
    fn window_ids_round_trip_through_protocol_words() {
        for id in [0, 1, u32::MAX as u64, u64::from(u32::MAX) + 1, u64::MAX] {
            let (hi, lo) = split_id(id);

            assert_eq!(join_id(hi, lo), WindowId(id));
        }
    }
}
