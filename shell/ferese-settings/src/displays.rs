use serde_json::{Value, json};

use crate::store::{Edit, set};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Mode {
    pub width: u32,
    pub height: u32,
    pub refresh: u32,
}

impl Mode {
    fn from_value(value: &Value) -> Option<Self> {
        Some(Self {
            width: value["width"].as_u64()?.try_into().ok()?,
            height: value["height"].as_u64()?.try_into().ok()?,
            refresh: value["refresh_millihertz"].as_u64()?.try_into().ok()?,
        })
    }

    pub fn config(self) -> String {
        format!("{}x{}@{:.3}", self.width, self.height, f64::from(self.refresh) / 1000.)
    }

    pub fn label(self) -> String {
        if self.refresh.is_multiple_of(1000) {
            format!("{} Hz", self.refresh / 1000)
        } else {
            format!("{:.3} Hz", f64::from(self.refresh) / 1000.)
        }
    }
}

#[derive(Clone, Debug)]
pub struct Display {
    pub connector: String,
    pub identity: String,
    pub profile: Option<String>,
    pub logical_width: Option<i32>,
    pub focused: bool,
    pub current: Option<Mode>,
    pub modes: Vec<Mode>,
}

pub fn load() -> Result<Vec<Display>, String> {
    let mut connection = ferese_ipc::theme::Connection::connect().map_err(|error| error.to_string())?;
    let value = connection.call("get-outputs", json!({}))?;
    let values = value.as_array().ok_or("Invalid display response")?;
    Ok(values
        .iter()
        .filter_map(|value| {
            Some(Display {
                connector: value["connector"].as_str()?.to_owned(),
                identity: value["identity"].as_str().unwrap_or("").to_owned(),
                profile: value["profile"].as_str().map(str::to_owned),
                logical_width: value["width"]
                    .as_i64()
                    .and_then(|width| width.try_into().ok())
                    .filter(|width| *width > 0),
                focused: value["focused"].as_bool().unwrap_or(false),
                current: Mode::from_value(&value["current_mode"]),
                modes: value["available_modes"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Mode::from_value)
                    .collect(),
            })
        })
        .collect())
}

pub fn choices(display: &Display, configured: &str) -> Vec<Mode> {
    let size = configured
        .split('@')
        .next()
        .and_then(|size| size.split_once('x'))
        .and_then(|(width, height)| Some((width.parse::<u32>().ok()?, height.parse::<u32>().ok()?)))
        .or_else(|| display.current.map(|mode| (mode.width, mode.height)));
    let mut modes = display
        .modes
        .iter()
        .copied()
        .filter(|mode| Some((mode.width, mode.height)) == size && mode.refresh > 0)
        .collect::<Vec<_>>();
    modes.sort_by_key(|mode| mode.refresh);
    modes.dedup();
    modes
}

pub fn edits(prefix: &str, mode: Mode, automatic: bool) -> Vec<Edit> {
    vec![
        set(&format!("{prefix}.mode"), mode.config()),
        set(&format!("{prefix}.auto_refresh"), automatic),
    ]
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Target {
    pub key: String,
    pub matcher: String,
    pub title: String,
    pub profile: Option<String>,
    pub prefix: Option<String>,
    pub active: bool,
    pub connected: bool,
}

impl std::fmt::Display for Target {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} · {}{}",
            self.title,
            self.profile.as_deref().unwrap_or("Automatic"),
            if self.connected { "" } else { " (offline)" }
        )
    }
}

pub(super) fn targets(snapshot: &crate::store::Snapshot, displays: &[Display]) -> Vec<Target> {
    let mut targets = Vec::new();
    for profile in 0..snapshot.records("output_profiles") {
        let prefix = format!("output_profiles.{profile}");
        let name = snapshot.string(&format!("{prefix}.name"), "Display profile");
        for output in 0..snapshot.records(&format!("{prefix}.outputs")) {
            let prefix = format!("{prefix}.outputs.{output}");
            let matcher = snapshot.string(&format!("{prefix}.match"), "");
            let live = displays
                .iter()
                .find(|d| d.connector == matcher || d.identity == matcher);
            targets.push(Target {
                key: format!("{name}\0{matcher}"),
                title: live.map(|d| d.connector.clone()).unwrap_or_else(|| matcher.clone()),
                matcher,
                profile: Some(name.clone()),
                prefix: Some(prefix),
                active: live.is_some_and(|d| d.profile.as_deref() == Some(&name)),
                connected: live.is_some(),
            });
        }
    }
    for display in displays {
        if !targets
            .iter()
            .any(|t| t.matcher == display.connector || t.matcher == display.identity)
        {
            targets.push(Target {
                key: format!("automatic\0{}", display.connector),
                matcher: display.connector.clone(),
                title: display.connector.clone(),
                profile: None,
                prefix: None,
                active: true,
                connected: true,
            });
        }
    }
    targets
}

pub(super) fn selected<'a>(targets: &'a [Target], key: Option<&str>) -> Option<&'a Target> {
    targets
        .iter()
        .find(|t| Some(t.key.as_str()) == key)
        .or_else(|| targets.iter().find(|t| t.active))
        .or_else(|| targets.iter().find(|t| t.connected))
        .or_else(|| targets.first())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Resolution {
    pub width: u32,
    pub height: u32,
}

impl std::fmt::Display for Resolution {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} × {}", self.width, self.height)
    }
}

pub(super) fn resolutions(display: &Display) -> Vec<Resolution> {
    let mut values: Vec<_> = display
        .modes
        .iter()
        .filter(|m| m.width > 0 && m.height > 0)
        .map(|m| Resolution {
            width: m.width,
            height: m.height,
        })
        .collect();
    values.sort_by_key(|r| {
        (
            std::cmp::Reverse(u64::from(r.width) * u64::from(r.height)),
            r.width,
            r.height,
        )
    });
    values.dedup();
    values
}

pub(super) fn resolution(configured: &str, display: &Display) -> Option<Resolution> {
    configured
        .split('@')
        .next()
        .and_then(|v| v.split_once('x'))
        .and_then(|(w, h)| {
            Some(Resolution {
                width: w.parse().ok()?,
                height: h.parse().ok()?,
            })
        })
        .or_else(|| {
            display.current.map(|m| Resolution {
                width: m.width,
                height: m.height,
            })
        })
}

pub(super) fn change_resolution(
    display: &Display,
    configured: &str,
    resolution: Resolution,
    automatic: bool,
) -> Option<(Mode, bool)> {
    let modes = choices(display, &format!("{}x{}", resolution.width, resolution.height));
    let auto = automatic && modes.iter().any(|m| (59_000..=61_000).contains(&m.refresh));
    let previous = configured
        .split('@')
        .nth(1)
        .and_then(|r| r.parse::<f64>().ok())
        .map(|r| r * 1000.)
        .or_else(|| display.current.map(|m| f64::from(m.refresh)));
    let mode = if auto {
        modes.last()
    } else {
        modes
            .iter()
            .find(|m| previous.is_some_and(|r| (r - f64::from(m.refresh)).abs() < 1.))
            .or_else(|| modes.last())
    }?;
    Some((*mode, auto))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Refresh {
    Manual(Mode),
    Automatic(Mode),
}

impl std::fmt::Display for Refresh {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Manual(mode) => f.write_str(&mode.label()),
            Self::Automatic(_) => f.write_str("Auto (battery)"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Snapshot;

    #[test]
    fn refresh_choices_preserve_resolution_and_fractional_rates() {
        let display = Display {
            connector: "eDP-1".into(),
            identity: "panel".into(),
            profile: None,
            logical_width: Some(1440),
            focused: true,
            current: Some(Mode {
                width: 2880,
                height: 1800,
                refresh: 120000,
            }),
            modes: vec![
                Mode {
                    width: 2880,
                    height: 1800,
                    refresh: 120000,
                },
                Mode {
                    width: 2880,
                    height: 1800,
                    refresh: 59940,
                },
                Mode {
                    width: 1920,
                    height: 1080,
                    refresh: 60000,
                },
                Mode {
                    width: 2880,
                    height: 1800,
                    refresh: 59940,
                },
            ],
        };
        let choices = choices(&display, "2880x1800@120");
        assert_eq!(choices.len(), 2);
        assert_eq!(choices[0].label(), "59.940 Hz");
        assert_eq!(choices[0].config(), "2880x1800@59.940");
        assert_eq!(choices[1].label(), "120 Hz");
    }

    #[test]
    fn manual_choice_disables_auto_and_auto_restores_high_rate() {
        let mut snapshot = Snapshot::parse(
            "output-profile laptop { output eDP-1 mode=\"2880x1800@120\" scale=1.75 auto-refresh=#true; }".into(),
        )
        .unwrap();
        let prefix = "output_profiles.0.outputs.0";
        for edit in edits(
            prefix,
            Mode {
                width: 2880,
                height: 1800,
                refresh: 60000,
            },
            false,
        ) {
            snapshot.edit(&edit).unwrap();
        }
        assert!(!snapshot.boolean(&format!("{prefix}.auto_refresh"), true));
        assert_eq!(snapshot.string(&format!("{prefix}.mode"), ""), "2880x1800@60.000");
        assert_eq!(snapshot.number(&format!("{prefix}.scale"), 1.), 1.75);
        for edit in edits(
            prefix,
            Mode {
                width: 2880,
                height: 1800,
                refresh: 120000,
            },
            true,
        ) {
            snapshot.edit(&edit).unwrap();
        }
        assert!(snapshot.boolean(&format!("{prefix}.auto_refresh"), false));
        assert_eq!(snapshot.string(&format!("{prefix}.mode"), ""), "2880x1800@120.000");
    }

    #[test]
    fn selected_display_prefers_active_profile_and_survives_profile_reordering() {
        let snapshot =
            Snapshot::parse("output-profile dock { output HDMI-A-1; }; output-profile laptop { output eDP-1; }".into())
                .unwrap();
        let live = vec![Display {
            connector: "eDP-1".into(),
            identity: "panel".into(),
            profile: Some("laptop".into()),
            logical_width: Some(1440),
            focused: true,
            current: None,
            modes: vec![],
        }];
        let values = targets(&snapshot, &live);
        assert_eq!(selected(&values, None).unwrap().profile.as_deref(), Some("laptop"));
        let dock = selected(&values, Some("dock\0HDMI-A-1")).unwrap();
        assert!(!dock.connected);
        let key = dock.key.clone();
        let reordered =
            Snapshot::parse("output-profile laptop { output eDP-1; }; output-profile dock { output HDMI-A-1; }".into())
                .unwrap();
        let values = targets(&reordered, &live);
        assert_eq!(
            selected(&values, Some(&key)).unwrap().prefix.as_deref(),
            Some("output_profiles.1.outputs.0")
        );
    }

    #[test]
    fn automatically_managed_outputs_are_still_visible() {
        let snapshot = Snapshot::parse(String::new()).unwrap();
        let live = vec![Display {
            connector: "eDP-1".into(),
            identity: "panel".into(),
            profile: None,
            logical_width: Some(1440),
            focused: true,
            current: None,
            modes: vec![],
        }];
        let values = targets(&snapshot, &live);
        assert_eq!(values.len(), 1);
        assert!(selected(&values, None).unwrap().prefix.is_none());
    }

    #[test]
    fn resolution_changes_preserve_refresh_and_only_keep_valid_auto_policy() {
        let mode = |width, height, refresh| Mode { width, height, refresh };
        let display = Display {
            connector: "eDP-1".into(),
            identity: "panel".into(),
            profile: None,
            logical_width: Some(1440),
            focused: true,
            current: Some(mode(2880, 1800, 60000)),
            modes: vec![
                mode(2880, 1800, 60000),
                mode(2880, 1800, 120000),
                mode(2560, 1440, 90000),
            ],
        };
        assert_eq!(resolutions(&display).len(), 2);
        let high = Resolution {
            width: 2880,
            height: 1800,
        };
        assert_eq!(
            change_resolution(&display, "2880x1800@120", high, false),
            Some((mode(2880, 1800, 120000), false))
        );
        assert_eq!(
            change_resolution(&display, "2880x1800@120", high, true),
            Some((mode(2880, 1800, 120000), true))
        );
        let other = Resolution {
            width: 2560,
            height: 1440,
        };
        assert_eq!(
            change_resolution(&display, "2880x1800@120", other, true),
            Some((mode(2560, 1440, 90000), false))
        );
        assert!(change_resolution(&display, "", Resolution { width: 1, height: 1 }, true).is_none());
    }
}
