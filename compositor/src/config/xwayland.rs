use std::path::PathBuf;

use serde::Deserialize;

/// When the managed X11 service starts.
///
/// `on-demand` waits for a listener connection or an explicit retry request.
/// `eager` starts the same managed service immediately after the event loop
/// begins dispatching; it must not use a different allocation or environment
/// path.
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum XwaylandStartup {
    #[default]
    OnDemand,
    Eager,
}

/// Public configuration for the managed `xwayland-satellite` integration.
///
/// Deliberately small. Startup timeout, retry policy, and shutdown grace stay
/// internal constants until a real deployment needs to tune them. The field is
/// `path` rather than `command` because Ferese's shared KDL conversion treats a
/// field named `command` as an array, and arbitrary argument passthrough would
/// conflict with the Ferese-owned display, auth, and listen arguments.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct XwaylandConfig {
    pub enabled: bool,
    pub startup: XwaylandStartup,
    pub path: PathBuf,
}

impl Default for XwaylandConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            startup: XwaylandStartup::OnDemand,
            path: PathBuf::from("xwayland-satellite"),
        }
    }
}

impl XwaylandConfig {
    pub fn validate(&self) -> Result<(), String> {
        if self.enabled && self.path.as_os_str().is_empty() {
            return Err("xwayland.path must name an executable".into());
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_enable_on_demand_with_satellite_on_path() {
        let config = XwaylandConfig::default();

        assert!(config.enabled);
        assert_eq!(config.startup, XwaylandStartup::OnDemand);
        assert_eq!(config.path, PathBuf::from("xwayland-satellite"));
        assert_eq!(config.validate(), Ok(()));
    }

    #[test]
    fn disabled_config_does_not_require_a_path() {
        let config = XwaylandConfig {
            enabled: false,
            path: PathBuf::new(),
            ..XwaylandConfig::default()
        };

        assert_eq!(config.validate(), Ok(()));
    }

    #[test]
    fn enabled_config_rejects_an_empty_path() {
        let config = XwaylandConfig {
            enabled: true,
            path: PathBuf::new(),
            ..XwaylandConfig::default()
        };

        assert_eq!(
            config.validate(),
            Err("xwayland.path must name an executable".to_owned())
        );
    }

    #[test]
    fn startup_mode_accepts_kebab_case() {
        #[derive(Deserialize)]
        struct Wrapper {
            startup: XwaylandStartup,
        }

        assert_eq!(
            serde_json::from_str::<Wrapper>(r#"{"startup":"eager"}"#)
                .unwrap()
                .startup,
            XwaylandStartup::Eager
        );
        assert_eq!(
            serde_json::from_str::<Wrapper>(r#"{"startup":"on-demand"}"#)
                .unwrap()
                .startup,
            XwaylandStartup::OnDemand
        );
        assert!(serde_json::from_str::<Wrapper>(r#"{"startup":"eagerly"}"#).is_err());
    }
}
