use std::error::Error;
use std::os::fd::{FromRawFd, RawFd};
use std::os::unix::net::UnixStream;
use std::sync::mpsc::{Receiver, Sender, TryRecvError};
use std::sync::{Arc, mpsc};
use std::thread;

use ferese_protocols::shell::v1::client::ferese_shell_manager_v1::FereseShellManagerV1;
use ferese_protocols::shell::v1::client::ferese_shell_v1;
use ferese_protocols::shell::v1::client::ferese_shell_v1::FereseShellV1;
use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::wl_registry;
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle, delegate_noop};

#[derive(Clone, Debug, Default)]
pub(crate) struct ShellSnapshot {
    pub(crate) outputs: Vec<OutputSnapshot>,
    pub(crate) workspaces: Vec<WorkspaceSnapshot>,
    pub(crate) windows: Vec<WindowSnapshot>,
}

impl ShellSnapshot {
    pub(crate) fn workspaces_for_output(&self, output: Option<u64>) -> impl Iterator<Item = &WorkspaceSnapshot> {
        self.workspaces
            .iter()
            .filter(move |workspace| output.is_some() && workspace.output == output)
    }
}

#[derive(Clone, Debug)]
pub(crate) struct OutputSnapshot {
    pub(crate) id: u64,
    pub(crate) name: String,
    pub(crate) active_workspace: u64,
    pub(crate) focused: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct WorkspaceSnapshot {
    pub(crate) id: u64,
    pub(crate) name: String,
    pub(crate) output: Option<u64>,
    pub(crate) index: u32,
    pub(crate) window_count: u32,
    pub(crate) visible: bool,
    pub(crate) focused: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct WindowSnapshot {
    #[cfg_attr(not(test), expect(dead_code, reason = "Retained for window-targeted shell views."))]
    pub(crate) id: u64,
    pub(crate) workspace: u64,
    pub(crate) app_id: String,
    pub(crate) title: String,
    pub(crate) focused: bool,
    #[cfg_attr(not(test), expect(dead_code, reason = "The current bar does not display urgency."))]
    pub(crate) urgent: bool,
    pub(crate) fullscreen: bool,
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "The current bar does not display window placement.")
    )]
    pub(crate) floating: bool,
}

pub(crate) struct ShellControl {
    connection: Connection,
    _manager: FereseShellManagerV1,
    shell: FereseShellV1,
    updates: Receiver<ControlUpdate>,
    wake: Wake,
}

pub(crate) struct ControlPoll {
    pub(crate) config: Option<String>,
    pub(crate) snapshot: Option<ShellSnapshot>,
    pub(crate) overview_active: Option<bool>,
    pub(crate) disconnected: bool,
    pub(crate) commands: Vec<ControlCommand>,
}

pub(crate) enum ControlCommand {
    Logout(u32, String),
    CancelLogout(u32),
    ToggleGuide(String),
    ToggleDisplays(String),
    MonitorsChanged,
}

#[derive(Clone)]
struct Wake(Arc<tokio::sync::Notify>);

impl std::hash::Hash for Wake {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        std::ptr::hash(Arc::as_ptr(&self.0), state);
    }
}

impl Wake {
    fn stream(&self) -> impl cosmic::iced::futures::Stream<Item = ()> + use<> {
        cosmic::iced::futures::stream::unfold(self.clone(), |wake| async move {
            wake.0.notified().await;
            Some(((), wake))
        })
    }
}

struct UpdateSender {
    sender: Sender<ControlUpdate>,
    wake: Wake,
}

impl UpdateSender {
    fn send(&self, update: ControlUpdate) -> Result<(), mpsc::SendError<ControlUpdate>> {
        self.sender.send(update)?;
        self.wake.0.notify_one();
        Ok(())
    }
}

enum ControlUpdate {
    Config(String),
    Snapshot(ShellSnapshot),
    OverviewState(bool),
    Disconnected,
    Logout(u32, String),
    LogoutCancelled(u32),
    ToggleGuide(String),
    ToggleDisplays(String),
    MonitorsChanged,
}

impl ShellControl {
    pub(crate) fn connect() -> Result<Self, Box<dyn Error>> {
        let connection = control_connection()?;
        let (globals, mut queue) = registry_queue_init::<ControlState>(&connection)?;
        let qh = queue.handle();
        let manager = globals.bind::<FereseShellManagerV1, _, _>(&qh, 4..=6, ())?;
        let shell = manager.get_shell(&qh, ());
        let (sender, updates) = mpsc::channel();
        let wake = Wake(Arc::new(tokio::sync::Notify::new()));
        let mut state = ControlState::new(UpdateSender {
            sender,
            wake: wake.clone(),
        });

        connection.flush()?;
        thread::Builder::new()
            .name("ferese-shell-control".to_owned())
            .spawn(move || {
                while queue.blocking_dispatch(&mut state).is_ok() {}
                let _ = state.sender.send(ControlUpdate::Disconnected);
            })?;

        Ok(Self {
            connection,
            _manager: manager,
            shell,
            updates,
            wake,
        })
    }

    pub(crate) fn subscription(&self) -> cosmic::iced::Subscription<()> {
        cosmic::iced::Subscription::run_with(self.wake.clone(), Wake::stream)
    }

    pub(crate) fn poll(&self) -> ControlPoll {
        let mut poll = ControlPoll {
            config: None,
            snapshot: None,
            overview_active: None,
            disconnected: false,
            commands: Vec::new(),
        };

        loop {
            match self.updates.try_recv() {
                Ok(ControlUpdate::Logout(serial, output)) => poll.commands.push(ControlCommand::Logout(serial, output)),
                Ok(ControlUpdate::ToggleDisplays(output)) => poll.commands.push(ControlCommand::ToggleDisplays(output)),
                Ok(ControlUpdate::MonitorsChanged) => poll.commands.push(ControlCommand::MonitorsChanged),
                Ok(ControlUpdate::ToggleGuide(output)) => poll.commands.push(ControlCommand::ToggleGuide(output)),
                Ok(ControlUpdate::LogoutCancelled(serial)) => poll.commands.push(ControlCommand::CancelLogout(serial)),
                Ok(ControlUpdate::Config(source)) => poll.config = Some(source),
                Ok(ControlUpdate::Snapshot(snapshot)) => poll.snapshot = Some(snapshot),
                Ok(ControlUpdate::OverviewState(active)) => poll.overview_active = Some(active),
                Ok(ControlUpdate::Disconnected) | Err(TryRecvError::Disconnected) => {
                    poll.disconnected = true;
                    return poll;
                }
                Err(TryRecvError::Empty) => return poll,
            }
        }
    }

    pub(crate) fn activate_workspace(&self, id: u64) {
        let (hi, lo) = split_id(id);

        self.shell.activate_workspace(hi, lo);
        let _ = self.connection.flush();
    }

    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "Window-targeting API; the current UI targets workspaces.")
    )]
    pub(crate) fn activate_window(&self, id: u64) {
        let (hi, lo) = split_id(id);
        self.shell.activate_window(hi, lo);
        let _ = self.connection.flush();
    }

    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "Window-targeting API; the current UI targets workspaces.")
    )]
    pub(crate) fn close_window(&self, id: u64) {
        let (hi, lo) = split_id(id);
        self.shell.close_window(hi, lo);
        let _ = self.connection.flush();
    }

    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "Window-targeting API; the current UI targets workspaces.")
    )]
    pub(crate) fn select_overview_window(&self, id: u64) {
        let (hi, lo) = split_id(id);
        self.shell.select_overview_window(hi, lo);
        let _ = self.connection.flush();
    }

    pub(crate) fn confirm_logout(&self, serial: u32, revision: u32, token: u32, force: bool) {
        self.shell
            .confirm_logout_with_inhibitors(serial, revision, token, u32::from(force));
        let _ = self.connection.flush();
    }

    pub(crate) fn cancel_logout(&self, serial: u32) {
        if self.shell.version() >= 3 {
            self.shell.cancel_logout(serial);
            let _ = self.connection.flush();
        }
    }

    pub(crate) fn set_overview_active(&self, active: bool) {
        if active {
            self.shell.enter_overview();
        } else {
            self.shell.exit_overview();
        }
        let _ = self.connection.flush();
    }
}

fn control_connection() -> Result<Connection, Box<dyn Error>> {
    let raw_fd =
        std::env::var_os("FERESE_SHELL_CONTROL_SOCKET").ok_or("Ferese did not provide a shell-control connection")?;
    let fd = raw_fd
        .to_str()
        .ok_or("FERESE_SHELL_CONTROL_SOCKET is not valid UTF-8")?
        .parse::<RawFd>()?;
    // SAFETY: Ferese passes ownership of this inherited descriptor to the
    // shell. This function is called once and consumes the descriptor.
    let socket = unsafe { UnixStream::from_raw_fd(fd) };
    // Rust's cloned stream is close-on-exec. Do not leak shell-control authority
    // into status helpers or applications launched from the menu.
    let connection_socket = socket.try_clone()?;
    drop(socket);
    Ok(Connection::from_socket(connection_socket)?)
}

struct ControlState {
    config: Option<String>,
    sender: UpdateSender,
    pending: ShellSnapshot,
    serial: Option<u32>,
}

impl ControlState {
    fn new(sender: UpdateSender) -> Self {
        Self {
            sender,
            config: None,
            pending: ShellSnapshot::default(),
            serial: None,
        }
    }
}

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for ControlState {
    fn event(
        _state: &mut Self,
        _registry: &wl_registry::WlRegistry,
        _event: wl_registry::Event,
        _data: &GlobalListContents,
        _connection: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<FereseShellV1, ()> for ControlState {
    fn event(
        state: &mut Self,
        _shell: &FereseShellV1,
        event: ferese_shell_v1::Event,
        _data: &(),
        _connection: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        match event {
            ferese_shell_v1::Event::ConfigBegin => state.config = Some(String::new()),
            ferese_shell_v1::Event::ConfigChunk { source } => {
                append_config_chunk(&mut state.config, &source);
            }
            ferese_shell_v1::Event::ConfigEnd => {
                if let Some(source) = state.config.take() {
                    let _ = state.sender.send(ControlUpdate::Config(source));
                }
            }
            ferese_shell_v1::Event::SnapshotBegin { serial } => {
                state.serial = Some(serial);
                state.pending = ShellSnapshot::default();
            }
            ferese_shell_v1::Event::Output {
                output_hi,
                output_lo,
                name,
                active_workspace_hi,
                active_workspace_lo,
                focused,
            } => state.pending.outputs.push(OutputSnapshot {
                id: join_id(output_hi, output_lo),
                name,
                active_workspace: join_id(active_workspace_hi, active_workspace_lo),
                focused: focused != 0,
            }),
            ferese_shell_v1::Event::Workspace {
                workspace_hi,
                workspace_lo,
                output_hi,
                output_lo,
                name,
                index,
                window_count,
                visible,
                focused,
            } => {
                let output = join_id(output_hi, output_lo);

                state.pending.workspaces.push(WorkspaceSnapshot {
                    id: join_id(workspace_hi, workspace_lo),
                    name,
                    output: (output != 0).then_some(output),
                    index,
                    window_count,
                    visible: visible != 0,
                    focused: focused != 0,
                });
            }
            ferese_shell_v1::Event::Window {
                window_hi,
                window_lo,
                workspace_hi,
                workspace_lo,
                app_id,
                title,
                state: window_state,
            } => {
                // WEnum::Unknown holds the entire bitfield, not just new bits.
                let flags = ferese_shell_v1::WindowState::from_bits_truncate(u32::from(window_state));

                state.pending.windows.push(WindowSnapshot {
                    id: join_id(window_hi, window_lo),
                    workspace: join_id(workspace_hi, workspace_lo),
                    app_id,
                    title,
                    focused: flags.contains(ferese_shell_v1::WindowState::Focused),
                    urgent: flags.contains(ferese_shell_v1::WindowState::Urgent),
                    fullscreen: flags.contains(ferese_shell_v1::WindowState::Fullscreen),
                    floating: flags.contains(ferese_shell_v1::WindowState::Floating),
                });
            }
            ferese_shell_v1::Event::Capabilities { .. } => {
                let _ = state.sender.send(ControlUpdate::MonitorsChanged);
            }
            ferese_shell_v1::Event::SnapshotEnd { serial } if state.serial.take() == Some(serial) => {
                let _ = state.sender.send(ControlUpdate::Snapshot(state.pending.clone()));
            }
            ferese_shell_v1::Event::LogoutRequested { serial, output_name } => {
                let _ = state.sender.send(ControlUpdate::Logout(serial, output_name));
            }
            ferese_shell_v1::Event::ToggleDisplayMode { output_name } => {
                let _ = state.sender.send(ControlUpdate::ToggleDisplays(output_name));
            }
            ferese_shell_v1::Event::MonitorStateChanged => {
                let _ = state.sender.send(ControlUpdate::MonitorsChanged);
            }
            ferese_shell_v1::Event::ToggleKeybindingGuide { output_name } => {
                let _ = state.sender.send(ControlUpdate::ToggleGuide(output_name));
            }
            ferese_shell_v1::Event::LogoutCancelled { serial } => {
                let _ = state.sender.send(ControlUpdate::LogoutCancelled(serial));
            }
            ferese_shell_v1::Event::OverviewState { active } => {
                let _ = state.sender.send(ControlUpdate::OverviewState(active != 0));
            }
            _ => {}
        }
    }
}

delegate_noop!(ControlState: ignore FereseShellManagerV1);

#[cfg(test)]
delegate_noop!(ControlState: ignore wl_registry::WlRegistry);

fn split_id(id: u64) -> (u32, u32) {
    ((id >> 32) as u32, id as u32)
}

fn join_id(hi: u32, lo: u32) -> u64 {
    (u64::from(hi) << 32) | u64::from(lo)
}

fn append_config_chunk(config: &mut Option<String>, source: &str) {
    if let Some(buffer) = config {
        if buffer.len() + source.len() > 60 * 1024 {
            *config = None;
        } else {
            buffer.push_str(source);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::Read;
    use std::time::Duration;

    use super::*;

    fn read_request(peer: &mut UnixStream) -> (u32, u16, Vec<u8>) {
        let mut header = [0; 8];
        peer.read_exact(&mut header).unwrap();
        let object = u32::from_ne_bytes(header[..4].try_into().unwrap());
        let size_opcode = u32::from_ne_bytes(header[4..].try_into().unwrap());
        let mut args = vec![0; (size_opcode >> 16) as usize - 8];
        peer.read_exact(&mut args).unwrap();
        (object, size_opcode as u16, args)
    }

    fn control_fixture() -> (
        ShellControl,
        ControlState,
        wayland_client::EventQueue<ControlState>,
        UnixStream,
    ) {
        let (socket, mut peer) = UnixStream::pair().unwrap();
        peer.set_read_timeout(Some(Duration::from_secs(1))).unwrap();
        let connection = Connection::from_socket(socket).unwrap();
        let queue = connection.new_event_queue::<ControlState>();
        let qh = queue.handle();
        let registry = connection.display().get_registry(&qh, ());
        let manager = registry.bind::<FereseShellManagerV1, _, _>(1, 6, &qh, ());
        let shell = manager.get_shell(&qh, ());
        let (sender, updates) = mpsc::channel();
        let wake = Wake(Arc::new(tokio::sync::Notify::new()));
        let state = ControlState::new(UpdateSender {
            sender,
            wake: wake.clone(),
        });
        connection.flush().unwrap();
        // Consume get_registry, bind and get_shell before checking UI requests.
        for _ in 0..3 {
            read_request(&mut peer);
        }
        let control = ShellControl {
            connection,
            _manager: manager,
            shell,
            updates,
            wake,
        };
        (control, state, queue, peer)
    }

    fn dispatch_event(
        control: &ShellControl,
        state: &mut ControlState,
        queue: &wayland_client::EventQueue<ControlState>,
        event: ferese_shell_v1::Event,
    ) {
        <ControlState as Dispatch<FereseShellV1, ()>>::event(
            state,
            &control.shell,
            event,
            &(),
            &control.connection,
            &queue.handle(),
        );
    }

    fn window_event(id: u64, flags: u32) -> ferese_shell_v1::Event {
        let (window_hi, window_lo) = split_id(id);
        ferese_shell_v1::Event::Window {
            window_hi,
            window_lo,
            workspace_hi: 0x1234_5678,
            workspace_lo: 0x9abc_def0,
            app_id: "dev.ferese.Test".into(),
            title: "Same title".into(),
            state: flags.into(),
        }
    }

    #[test]
    fn window_id_words_round_trip_without_truncation() {
        for (id, hi, lo) in [
            (0, 0, 0),
            (u64::from(u32::MAX), 0, u32::MAX),
            (1 << 32, 1, 0),
            (0x1234_5678_9abc_def0, 0x1234_5678, 0x9abc_def0),
            (u64::MAX, u32::MAX, u32::MAX),
        ] {
            assert_eq!(split_id(id), (hi, lo));
            assert_eq!(join_id(hi, lo), id);
        }
    }

    #[test]
    fn snapshot_retains_identity_and_floating_state_for_identical_window_metadata() {
        let (control, mut state, queue, _peer) = control_fixture();
        dispatch_event(
            &control,
            &mut state,
            &queue,
            ferese_shell_v1::Event::SnapshotBegin { serial: 7 },
        );
        let ids = [0x1234_5678_9abc_def0, 0xfedc_ba98_7654_3210];
        for (id, flags) in [(ids[0], 0), (ids[1], ferese_shell_v1::WindowState::Floating.bits())] {
            dispatch_event(&control, &mut state, &queue, window_event(id, flags));
        }
        assert!(control.poll().snapshot.is_none());
        dispatch_event(
            &control,
            &mut state,
            &queue,
            ferese_shell_v1::Event::SnapshotEnd { serial: 7 },
        );
        let snapshot = control.poll().snapshot.unwrap().clone();
        assert_eq!(snapshot.windows.iter().map(|window| window.id).collect::<Vec<_>>(), ids);
        let [tiled, floating] = snapshot.windows.as_slice() else {
            panic!("expected two windows")
        };
        assert_eq!(tiled.workspace, 0x1234_5678_9abc_def0);
        assert_eq!(tiled.workspace, floating.workspace);
        assert_eq!(tiled.app_id, floating.app_id);
        assert_eq!(tiled.title, floating.title);
        assert!(!tiled.floating);
        assert!(floating.floating);
        assert_ne!(tiled.id, floating.id);
    }

    #[test]
    fn known_window_flags_survive_unknown_future_bits() {
        use ferese_shell_v1::WindowState as Flags;
        let (control, mut state, queue, _peer) = control_fixture();
        let known = Flags::Focused | Flags::Urgent | Flags::Fullscreen | Flags::Floating;
        let future = 1 << 31;
        assert!(matches!(
            wayland_client::WEnum::<Flags>::from(known.bits() | future),
            wayland_client::WEnum::Unknown(_)
        ));
        let cases = [
            (known.bits(), [true, true, true, true]),
            (known.bits() | future, [true, true, true, true]),
            (Flags::Focused.bits() | future, [true, false, false, false]),
            (Flags::Urgent.bits() | future, [false, true, false, false]),
            (Flags::Fullscreen.bits() | future, [false, false, true, false]),
            (Flags::Floating.bits() | future, [false, false, false, true]),
            (future, [false; 4]),
            (0, [false; 4]),
        ];
        dispatch_event(
            &control,
            &mut state,
            &queue,
            ferese_shell_v1::Event::SnapshotBegin { serial: 1 },
        );
        for (index, (bits, _)) in cases.iter().enumerate() {
            dispatch_event(&control, &mut state, &queue, window_event(index as u64 + 1, *bits));
        }
        dispatch_event(
            &control,
            &mut state,
            &queue,
            ferese_shell_v1::Event::SnapshotEnd { serial: 1 },
        );
        let snapshot = control.poll().snapshot.unwrap();
        for (window, (_, expected)) in snapshot.windows.iter().zip(cases) {
            assert_eq!(
                [window.focused, window.urgent, window.fullscreen, window.floating],
                expected
            );
        }
    }

    #[test]
    fn window_helpers_send_the_exact_snapshot_id_for_each_action() {
        let (control, mut state, queue, mut peer) = control_fixture();
        dispatch_event(
            &control,
            &mut state,
            &queue,
            ferese_shell_v1::Event::SnapshotBegin { serial: 8 },
        );
        let ids = [0x1234_5678_9abc_def0, 0xfedc_ba98_7654_3210];
        for id in ids {
            dispatch_event(&control, &mut state, &queue, window_event(id, 0));
        }
        dispatch_event(
            &control,
            &mut state,
            &queue,
            ferese_shell_v1::Event::SnapshotEnd { serial: 8 },
        );
        for window in control.poll().snapshot.unwrap().windows {
            control.activate_window(window.id);
            control.close_window(window.id);
            control.select_overview_window(window.id);
            for opcode in [0, 1, 5] {
                let (object, actual_opcode, args) = read_request(&mut peer);
                assert_eq!(object, control.shell.id().protocol_id());
                assert_eq!(actual_opcode, opcode);
                assert_eq!(args.len(), 8);
                let hi = u32::from_ne_bytes(args[..4].try_into().unwrap());
                let lo = u32::from_ne_bytes(args[4..].try_into().unwrap());
                assert_eq!(join_id(hi, lo), window.id);
                assert_eq!((hi, lo), split_id(window.id));
            }
        }
    }

    #[test]
    fn config_transfer_is_bounded_and_requires_begin() {
        let mut config = None;
        append_config_chunk(&mut config, "ignored");
        assert!(config.is_none());
        config = Some(String::new());
        append_config_chunk(&mut config, "🌲");
        append_config_chunk(&mut config, "theme");
        assert_eq!(config.take().as_deref(), Some("🌲theme"));
        config = Some("a".repeat(60 * 1024));
        append_config_chunk(&mut config, "overflow");
        assert!(config.is_none());
        append_config_chunk(&mut config, "ignored after overflow");
        assert!(config.is_none());
    }
}

#[cfg(test)]
mod workspace_tests {
    use super::*;

    #[test]
    fn each_bar_uses_its_own_order_and_authoritative_occupancy() {
        let workspace = |id, output, index, window_count| WorkspaceSnapshot {
            id,
            output: Some(output),
            index,
            name: index.to_string(),
            window_count,
            visible: index == 1,
            focused: output == 1 && index == 1,
        };
        let snapshot = ShellSnapshot {
            workspaces: vec![workspace(7, 1, 1, 2), workspace(9, 1, 2, 0), workspace(3, 2, 1, 0)],
            ..Default::default()
        };

        let left = snapshot.workspaces_for_output(Some(1)).collect::<Vec<_>>();
        assert_eq!(
            left.iter().map(|workspace| workspace.id).collect::<Vec<_>>(),
            vec![7, 9]
        );
        assert_eq!(left[0].window_count, 2);
        assert_eq!(
            snapshot
                .workspaces_for_output(Some(2))
                .map(|workspace| workspace.id)
                .collect::<Vec<_>>(),
            vec![3]
        );
        assert_eq!(snapshot.workspaces_for_output(None).count(), 0);
        assert_eq!(snapshot.workspaces_for_output(Some(99)).count(), 0);
    }
}

#[cfg(test)]
mod wake_tests {
    use cosmic::iced::futures::{FutureExt, StreamExt};

    use super::*;

    #[test]
    fn control_updates_wake_without_a_clock_tick_even_before_subscription() {
        let wake = Wake(Arc::new(tokio::sync::Notify::new()));
        let (sender, receiver) = mpsc::channel();
        let sender = UpdateSender {
            sender,
            wake: wake.clone(),
        };
        sender.send(ControlUpdate::OverviewState(true)).ok().unwrap();
        let stream = wake.stream();
        cosmic::iced::futures::pin_mut!(stream);

        assert_eq!(stream.next().now_or_never(), Some(Some(())));
        assert!(matches!(receiver.try_recv(), Ok(ControlUpdate::OverviewState(true))));
        assert_eq!(stream.next().now_or_never(), None);

        sender.send(ControlUpdate::Logout(7, "display".into())).ok().unwrap();
        sender.send(ControlUpdate::LogoutCancelled(7)).ok().unwrap();
        sender.send(ControlUpdate::Disconnected).ok().unwrap();
        assert_eq!(stream.next().now_or_never(), Some(Some(())));
        assert!(matches!(receiver.try_recv(), Ok(ControlUpdate::Logout(7, _))));
        assert!(matches!(receiver.try_recv(), Ok(ControlUpdate::LogoutCancelled(7))));
        assert!(matches!(receiver.try_recv(), Ok(ControlUpdate::Disconnected)));
    }
}
