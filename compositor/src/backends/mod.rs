pub(crate) mod direct;
mod power;

use std::ffi::OsString;

use smithay::reexports::calloop::EventLoop;

use crate::Ferese;
use crate::private_client::ClientCapabilities;
use crate::session_environment::SessionPolicy;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BackendKind {
    Nested,
    Drm,
}

#[derive(Debug, Eq, PartialEq)]
pub struct LaunchConfig {
    pub backend: BackendKind,
    pub(crate) session_policy: SessionPolicy,
    pub client: Vec<OsString>,
    pub client_capabilities: ClientCapabilities,
}

impl LaunchConfig {
    pub fn from_environment() -> Result<Self, String> {
        let graphical_session = std::env::var_os("WAYLAND_DISPLAY").is_some() || std::env::var_os("DISPLAY").is_some();

        Self::parse(std::env::args_os().skip(1), graphical_session)
    }

    fn parse(args: impl IntoIterator<Item = OsString>, graphical_session: bool) -> Result<Self, String> {
        let mut args = args.into_iter().peekable();
        let mut requested_backend = None;
        let mut client = Vec::new();
        let mut client_capabilities = ClientCapabilities::default();

        while let Some(argument) = args.next() {
            if argument == "--" {
                client.extend(args);
                break;
            }
            if argument == "--backend" {
                let value = args
                    .next()
                    .ok_or_else(|| "--backend requires auto, nested, or drm".to_owned())?;
                requested_backend = Some(parse_backend(&value)?);
                continue;
            }
            if argument == "--grant-effects" {
                client_capabilities.insert(ClientCapabilities::EFFECTS);
                continue;
            }
            if argument == "--grant-shell-control" {
                client_capabilities.insert(ClientCapabilities::SHELL_CONTROL);
                continue;
            }
            if let Some(value) = argument
                .to_str()
                .and_then(|argument| argument.strip_prefix("--backend="))
            {
                requested_backend = Some(parse_backend(&OsString::from(value))?);
                continue;
            }

            client.push(argument);
            client.extend(args);
            break;
        }

        let backend = requested_backend.flatten().unwrap_or(if graphical_session {
            BackendKind::Nested
        } else {
            BackendKind::Drm
        });

        Ok(Self {
            backend,
            session_policy: match backend {
                BackendKind::Drm => SessionPolicy::Desktop,
                BackendKind::Nested => SessionPolicy::Embedded,
            },
            client,
            client_capabilities,
        })
    }
}

pub fn init(
    backend: BackendKind,
    event_loop: &mut EventLoop<Ferese>,
    state: &mut Ferese,
) -> Result<(), Box<dyn std::error::Error>> {
    match backend {
        BackendKind::Nested => crate::winit::init(event_loop, state),
        BackendKind::Drm => direct::init(event_loop, state),
    }
}

fn parse_backend(value: &OsString) -> Result<Option<BackendKind>, String> {
    match value.to_str() {
        Some("auto") => Ok(None),
        Some("nested") => Ok(Some(BackendKind::Nested)),
        Some("drm") => Ok(Some(BackendKind::Drm)),
        _ => Err(format!("unknown backend {:?}; expected auto, nested, or drm", value)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_selects_nested_inside_a_graphical_session() {
        let config = LaunchConfig::parse([OsString::from("foot")], true).unwrap();

        assert_eq!(config.backend, BackendKind::Nested);
        assert_eq!(config.session_policy, SessionPolicy::Embedded);
        assert_eq!(config.client, [OsString::from("foot")]);
        assert!(config.client_capabilities.is_empty());
    }

    #[test]
    fn auto_selects_drm_without_a_graphical_session() {
        let config = LaunchConfig::parse(Vec::<OsString>::new(), false).unwrap();

        assert_eq!(config.backend, BackendKind::Drm);
        assert_eq!(config.session_policy, SessionPolicy::Desktop);
        assert!(config.client.is_empty());
        assert!(config.client_capabilities.is_empty());
    }

    #[test]
    fn explicit_backend_and_separator_are_consumed() {
        let config = LaunchConfig::parse(
            [
                OsString::from("--backend=drm"),
                OsString::from("--"),
                OsString::from("foot"),
                OsString::from("--title=Ferese"),
            ],
            true,
        )
        .unwrap();

        assert_eq!(config.backend, BackendKind::Drm);
        assert_eq!(
            config.client,
            [OsString::from("foot"), OsString::from("--title=Ferese")]
        );
        assert!(config.client_capabilities.is_empty());
    }

    #[test]
    fn explicitly_grants_effects_to_the_launched_private_client() {
        let config = LaunchConfig::parse(
            [
                OsString::from("--grant-effects"),
                OsString::from("ferese-effects-probe"),
            ],
            true,
        )
        .unwrap();

        assert_eq!(config.client, [OsString::from("ferese-effects-probe")]);
        assert!(config.client_capabilities.contains(ClientCapabilities::EFFECTS));
    }

    #[test]
    fn explicitly_grants_shell_control_to_the_launched_private_client() {
        let config = LaunchConfig::parse(
            [OsString::from("--grant-shell-control"), OsString::from("ferese-shell")],
            true,
        )
        .unwrap();

        assert_eq!(config.client, [OsString::from("ferese-shell")]);
        assert!(config.client_capabilities.contains(ClientCapabilities::SHELL_CONTROL));
        assert!(!config.client_capabilities.contains(ClientCapabilities::EFFECTS));
    }

    #[test]
    fn rejects_an_unknown_backend() {
        let error = LaunchConfig::parse([OsString::from("--backend"), OsString::from("other")], true).unwrap_err();

        assert!(error.contains("unknown backend"));
    }

    #[test]
    fn explicit_backend_selects_the_default_session_policy() {
        for (backend, graphical, policy) in [
            ("--backend=drm", true, SessionPolicy::Desktop),
            ("--backend=nested", false, SessionPolicy::Embedded),
        ] {
            let config = LaunchConfig::parse([OsString::from(backend)], graphical).unwrap();
            assert_eq!(config.session_policy, policy);
        }
    }
}
