use std::ffi::OsString;
use std::path::PathBuf;
use std::process::Command;

/// The X11 endpoint this session owns, advertised to applications.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct X11Environment {
    pub display: OsString,
    pub authority: PathBuf,
}

/// The immutable child session environment, built once after display
/// reservation and authority-file creation succeed.
///
/// This is deliberately separate from the compositor's own process
/// environment: nested backend initialization needs the *host* `DISPLAY` or
/// `WAYLAND_DISPLAY`, while children need the *Ferese* endpoints. Ferese has
/// worker threads by the time children are spawned, so process-global
/// `set_var` is not an option.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SessionEnvironment {
    pub wayland_display: OsString,
    pub x11: Option<X11Environment>,
}

impl SessionEnvironment {
    /// Build the public child environment.
    ///
    /// Apply this *before* adding a deliberately privileged private
    /// connection, because applying it afterwards would erase the private
    /// socket those clients intentionally receive.
    pub fn apply_public(&self, command: &mut Command) {
        command
            .env("WAYLAND_DISPLAY", &self.wayland_display)
            .env("FERESE_PUBLIC_WAYLAND_DISPLAY", &self.wayland_display)
            .env_remove("WAYLAND_SOCKET")
            .env_remove("FERESE_SHELL_CONTROL_SOCKET")
            .env_remove("DISPLAY")
            .env_remove("XAUTHORITY");

        // When X11 is unavailable, DISPLAY/XAUTHORITY stay removed. Never
        // publish a fake working DISPLAY.
        if let Some(x11) = &self.x11 {
            command.env("DISPLAY", &x11.display).env("XAUTHORITY", &x11.authority);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn environment(x11: Option<X11Environment>) -> SessionEnvironment {
        SessionEnvironment {
            wayland_display: OsString::from("wayland-7"),
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

        // Removed, never set to a placeholder.
        assert_eq!(value_of(&command, "DISPLAY"), Some(None));
        assert_eq!(value_of(&command, "XAUTHORITY"), Some(None));
    }

    #[test]
    fn the_host_private_socket_is_always_cleared() {
        let mut command = Command::new("true");
        environment(None).apply_public(&mut command);

        // A host WAYLAND_SOCKET inherited by a child would win over
        // WAYLAND_DISPLAY in wl_display_connect and misroute the client.
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

        // The child's DISPLAY must be the Ferese endpoint, never the host's.
        assert_eq!(value_of(&command, "DISPLAY"), Some(Some(":7".to_owned())));
    }
}
