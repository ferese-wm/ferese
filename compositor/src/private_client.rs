use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::process::Command;
use std::sync::Arc;

use crate::Ferese;
use crate::state::ClientState;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct ClientCapabilities(u8);

impl ClientCapabilities {
    pub(crate) const EFFECTS: Self = Self(1 << 0);
    pub(crate) const SHELL_CONTROL: Self = Self(1 << 1);
    pub(crate) const WINDOW_CAPTURE: Self = Self(1 << 2);

    pub(crate) fn contains(self, capability: Self) -> bool {
        self.0 & capability.0 == capability.0
    }

    pub(crate) fn insert(&mut self, capability: Self) {
        self.0 |= capability.0;
    }

    pub(crate) fn is_empty(self) -> bool {
        self.0 == 0
    }
}

pub(crate) fn prepare_command(
    state: &mut Ferese,
    command: &mut Command,
    capabilities: ClientCapabilities,
) -> io::Result<Vec<UnixStream>> {
    let split_shell_control = capabilities.contains(ClientCapabilities::EFFECTS)
        && capabilities.contains(ClientCapabilities::SHELL_CONTROL);
    let primary_capabilities = if split_shell_control {
        ClientCapabilities::EFFECTS
    } else {
        capabilities
    };
    let primary = insert_private_client(state, primary_capabilities)?;
    let mut clients = vec![primary];

    command.env_remove("WAYLAND_DISPLAY");
    // Shell-launched applications must use the public socket, not inherit the
    // private shell/effects connection or accidentally connect to the host.
    command.env("FERESE_PUBLIC_WAYLAND_DISPLAY", &state.socket_name);
    command.env_remove("FERESE_SHELL_CONTROL_SOCKET");
    command.env("WAYLAND_SOCKET", clients[0].as_raw_fd().to_string());

    if split_shell_control {
        let control = insert_private_client(state, ClientCapabilities::SHELL_CONTROL)?;

        command.env(
            "FERESE_SHELL_CONTROL_SOCKET",
            control.as_raw_fd().to_string(),
        );
        clients.push(control);
    }

    let inherited_fds = clients.iter().map(AsRawFd::as_raw_fd).collect::<Vec<_>>();
    // SAFETY: fcntl(F_SETFD) is async-signal-safe. The closure only clears
    // CLOEXEC on sockets already owned by this Command's parent process.
    unsafe {
        command.pre_exec(move || {
            for fd in &inherited_fds {
                if libc::fcntl(*fd, libc::F_SETFD, 0) == -1 {
                    return Err(io::Error::last_os_error());
                }
            }

            Ok(())
        });
    }

    Ok(clients)
}

fn insert_private_client(
    state: &mut Ferese,
    capabilities: ClientCapabilities,
) -> io::Result<UnixStream> {
    let (server, client) = UnixStream::pair()?;

    state.display_handle.insert_client(
        server,
        Arc::new(ClientState {
            capabilities,
            ..ClientState::default()
        }),
    )?;

    Ok(client)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capabilities_are_explicit_and_independent() {
        let mut capabilities = ClientCapabilities::default();
        assert!(capabilities.is_empty());
        assert!(!capabilities.contains(ClientCapabilities::EFFECTS));
        assert!(!capabilities.contains(ClientCapabilities::SHELL_CONTROL));

        capabilities.insert(ClientCapabilities::EFFECTS);
        assert!(!capabilities.is_empty());
        assert!(capabilities.contains(ClientCapabilities::EFFECTS));
        assert!(!capabilities.contains(ClientCapabilities::SHELL_CONTROL));

        capabilities.insert(ClientCapabilities::SHELL_CONTROL);
        assert!(capabilities.contains(ClientCapabilities::EFFECTS));
        assert!(capabilities.contains(ClientCapabilities::SHELL_CONTROL));
    }
}
