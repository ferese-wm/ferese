use std::ffi::OsString;
use std::path::PathBuf;
use std::process::Command;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct X11Environment {
    pub display: OsString,
    pub authority: PathBuf,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SessionPolicy {
    Desktop,
    Embedded,
}

impl SessionPolicy {
    fn environment_value(self) -> &'static str {
        match self {
            Self::Desktop => "desktop",
            Self::Embedded => "embedded",
        }
    }

    pub(crate) fn allows_autostart(self, embedded_opt_in: bool) -> bool {
        self == Self::Desktop || embedded_opt_in
    }
}

// Child endpoints are separate from the host endpoints used by the nested backend.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SessionEnvironment {
    pub wayland_display: OsString,
    pub control_socket: PathBuf,
    pub policy: SessionPolicy,
    pub x11: Option<X11Environment>,
}

impl SessionEnvironment {
    /// Apply before private_client::prepare_command(), which overrides WAYLAND_SOCKET.
    pub fn apply_public(&self, command: &mut Command) {
        command
            .env("WAYLAND_DISPLAY", &self.wayland_display)
            .env("FERESE_PUBLIC_WAYLAND_DISPLAY", &self.wayland_display)
            .env("FERESE_SOCKET", &self.control_socket)
            .env("FERESE_SESSION_MODE", self.policy.environment_value())
            .env_remove("WAYLAND_SOCKET")
            .env_remove("FERESE_SHELL_CONTROL_SOCKET")
            .env_remove("DISPLAY")
            .env_remove("XAUTHORITY");

        if let Some(x11) = &self.x11 {
            command.env("DISPLAY", &x11.display).env("XAUTHORITY", &x11.authority);
        }
        if self.policy == SessionPolicy::Embedded {
            command.env("FERESE_SESSION_IMPORT_ENV", "0");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn environment(x11: Option<X11Environment>) -> SessionEnvironment {
        SessionEnvironment {
            wayland_display: OsString::from("wayland-7"),
            control_socket: PathBuf::from("/run/user/1000/ferese/instances/preview/control.sock"),
            policy: SessionPolicy::Desktop,
            x11,
        }
    }

    fn rendered(command: &Command) -> Vec<(String, Option<String>)> {
        command
            .get_envs()
            .map(|(key, value)| {
                (
                    key.to_string_lossy().into_owned(),
                    value.map(|value| value.to_string_lossy().into_owned()),
                )
            })
            .collect()
    }

    fn value_of(command: &Command, key: &str) -> Option<Option<String>> {
        rendered(command)
            .into_iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value)
    }

    #[test]
    fn x11_available_publishes_display_and_authority() {
        let mut command = Command::new("true");
        environment(Some(X11Environment {
            display: OsString::from(":7"),
            authority: PathBuf::from("/run/user/1000/ferese-xauth-abc"),
        }))
        .apply_public(&mut command);

        assert_eq!(value_of(&command, "DISPLAY"), Some(Some(":7".to_owned())));
        assert_eq!(
            value_of(&command, "XAUTHORITY"),
            Some(Some("/run/user/1000/ferese-xauth-abc".to_owned()))
        );
        assert_eq!(
            value_of(&command, "WAYLAND_DISPLAY"),
            Some(Some("wayland-7".to_owned()))
        );
    }

    #[test]
    fn x11_unavailable_removes_display_and_authority() {
        let mut command = Command::new("true");
        environment(None).apply_public(&mut command);

        assert_eq!(value_of(&command, "DISPLAY"), Some(None));
        assert_eq!(value_of(&command, "XAUTHORITY"), Some(None));
    }

    #[test]
    fn the_host_private_socket_is_always_cleared() {
        let mut command = Command::new("true");
        environment(None).apply_public(&mut command);

        assert_eq!(value_of(&command, "WAYLAND_SOCKET"), Some(None));
        assert_eq!(value_of(&command, "FERESE_SHELL_CONTROL_SOCKET"), Some(None));
    }

    #[test]
    fn the_public_socket_is_exported_for_shell_launched_apps() {
        let mut command = Command::new("true");
        environment(None).apply_public(&mut command);

        assert_eq!(
            value_of(&command, "FERESE_PUBLIC_WAYLAND_DISPLAY"),
            Some(Some("wayland-7".to_owned()))
        );
    }

    #[test]
    fn unrelated_variables_are_left_alone() {
        let mut command = Command::new("true");
        command.env("FERESE_COMPOSITOR_WALLPAPER", "1");
        environment(None).apply_public(&mut command);

        assert_eq!(
            value_of(&command, "FERESE_COMPOSITOR_WALLPAPER"),
            Some(Some("1".to_owned()))
        );
    }

    #[test]
    fn a_host_display_is_replaced_by_the_session_display() {
        let mut command = Command::new("true");
        command.env("DISPLAY", ":0");
        environment(Some(X11Environment {
            display: OsString::from(":7"),
            authority: PathBuf::from("/run/user/1000/auth"),
        }))
        .apply_public(&mut command);

        assert_eq!(value_of(&command, "DISPLAY"), Some(Some(":7".to_owned())));
    }

    #[test]
    fn a_host_control_socket_is_replaced_by_the_instance_socket() {
        let mut command = Command::new("true");
        command.env("FERESE_SOCKET", "/run/user/1000/ferese/control.sock");
        environment(None).apply_public(&mut command);
        assert_eq!(
            value_of(&command, "FERESE_SOCKET"),
            Some(Some("/run/user/1000/ferese/instances/preview/control.sock".into()))
        );
    }

    #[test]
    fn embedded_clients_cannot_import_their_environment_into_the_host_session() {
        let mut command = Command::new("true");
        command.env("FERESE_SESSION_IMPORT_ENV", "1");
        command.env("FERESE_SESSION_MODE", "desktop");
        let mut embedded = environment(None);
        embedded.policy = SessionPolicy::Embedded;
        embedded.apply_public(&mut command);
        assert_eq!(value_of(&command, "FERESE_SESSION_IMPORT_ENV"), Some(Some("0".into())));
        assert_eq!(value_of(&command, "FERESE_SESSION_MODE"), Some(Some("embedded".into())));
    }

    #[test]
    fn desktop_clients_preserve_the_launchers_activation_import_choice() {
        for choice in ["0", "1"] {
            let mut command = Command::new("true");
            command.env("FERESE_SESSION_IMPORT_ENV", choice);
            command.env("FERESE_SESSION_MODE", "embedded");
            environment(None).apply_public(&mut command);
            assert_eq!(
                value_of(&command, "FERESE_SESSION_IMPORT_ENV"),
                Some(Some(choice.into()))
            );
            assert_eq!(value_of(&command, "FERESE_SESSION_MODE"), Some(Some("desktop".into())));
        }
    }
}
