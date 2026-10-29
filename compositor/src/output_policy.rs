//! Pure monitor selection. DRM handles and side effects never enter this module.
use std::collections::{BTreeSet, HashSet};

use crate::config::{LidPolicy, OutputLayout, OutputProfile, OutputSettings, OutputTransform};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Monitor {
    /// Qualified hardware connector (GPU identity + connector), never a runtime Output handle.
    pub key: String,
    pub connector: String,
    pub identity: String,
    pub aliases: Vec<String>,
    pub internal: bool,
    pub usable: bool,
}

impl Monitor {
    pub fn matches(&self, selector: &str) -> bool {
        selector == self.connector
            || selector == self.identity
            || self.aliases.iter().any(|alias| alias == selector)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct DesiredOutput {
    pub key: String,
    pub settings: OutputSettings,
    /// A mirror target has no workspace ownership of its own.
    pub mirror_source: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct DesiredOutputConfiguration {
    pub profile: Option<String>,
    pub outputs: Vec<DesiredOutput>,
    pub suspend: bool,
}

/// Overrides expire on a *material* topology/lid change, independent of event ordering.
#[derive(Clone, Debug, Default)]
pub(crate) struct ManualOverride {
    pub configuration: Option<DesiredOutputConfiguration>,
    pub profile: Option<String>,
    pub internal: Option<bool>,
    pub layout: Option<OutputLayout>,
    topology: Option<(BTreeSet<String>, bool)>,
}

impl ManualOverride {
    pub fn observe(&mut self, monitors: &[Monitor], lid_closed: bool, profiles: &[OutputProfile]) {
        let topology = (
            monitors
                .iter()
                .map(|monitor| format!("{}={}", monitor.key, monitor.identity))
                .collect(),
            lid_closed,
        );
        if self.topology.as_ref().is_some_and(|old| old != &topology) {
            self.configuration = None;
            self.profile = None;
            self.internal = None;
            self.layout = None;
        }
        if self.profile.as_ref().is_some_and(|name| {
            !profiles.iter().any(|profile| {
                profile.name == *name && profile_matches(profile, monitors, lid_closed)
            })
        }) {
            self.profile = None;
        }
        self.topology = Some(topology);
    }
}

fn profile_matches(profile: &OutputProfile, monitors: &[Monitor], lid_closed: bool) -> bool {
    let mut matched = HashSet::new();
    if profile.outputs.iter().any(|settings| {
        monitors
            .iter()
            .filter(|monitor| monitor.matches(&settings.matcher))
            .any(|monitor| !matched.insert(&monitor.key))
    }) {
        return false;
    }
    profile
        .lid_closed
        .is_none_or(|required| required == lid_closed)
        && profile
            .outputs
            .iter()
            .filter(|settings| settings.required)
            .all(|settings| {
                // Ambiguous selectors cannot silently configure two monitors as one.
                monitors
                    .iter()
                    .filter(|monitor| monitor.matches(&settings.matcher))
                    .count()
                    == 1
            })
}

pub(crate) fn defaults(matcher: String) -> OutputSettings {
    OutputSettings {
        auto_refresh: false,
        adaptive_sync: false,
        allow_tearing: false,
        matcher,
        required: true,
        enabled: true,
        mode: None,
        scale: 1.0,
        transform: OutputTransform::Normal,
        position: None,
    }
}

/// Most required monitors wins; equal specificity retains config order.
/// Unlisted outputs keep working in extend profiles. Exclusive layouts deliberately filter them.
pub(crate) fn select_profile(
    monitors: &[Monitor],
    lid_closed: bool,
    profiles: &[OutputProfile],
    manual: &ManualOverride,
) -> DesiredOutputConfiguration {
    if let Some(configuration) = &manual.configuration {
        return configuration.clone();
    }
    let selected = manual
        .profile
        .as_ref()
        .and_then(|name| {
            profiles.iter().find(|profile| {
                profile.name == *name && profile_matches(profile, monitors, lid_closed)
            })
        })
        .or_else(|| {
            profiles
                .iter()
                .enumerate()
                .filter(|(_, profile)| profile_matches(profile, monitors, lid_closed))
                .max_by_key(|(index, profile)| {
                    (
                        profile
                            .outputs
                            .iter()
                            .filter(|output| output.required)
                            .count(),
                        std::cmp::Reverse(*index),
                    )
                })
                .map(|(_, profile)| profile)
        });
    let layout = manual
        .layout
        .unwrap_or_else(|| selected.map_or(OutputLayout::Extend, |profile| profile.layout));
    let manage_lid = selected.is_none_or(|profile| profile.lid_policy == LidPolicy::DockOrSuspend);
    let external = monitors
        .iter()
        .any(|monitor| monitor.usable && !monitor.internal);
    let mut outputs = monitors
        .iter()
        .map(|monitor| {
            let mut settings = selected
                .and_then(|profile| {
                    profile
                        .outputs
                        .iter()
                        .find(|settings| monitor.matches(&settings.matcher))
                })
                .cloned()
                .unwrap_or_else(|| defaults(monitor.identity.clone()));
            if manual.layout.is_some() {
                settings.enabled = true;
            }
            settings.enabled &= monitor.usable;
            settings.enabled &= match layout {
                OutputLayout::InternalOnly => monitor.internal,
                OutputLayout::ExternalOnly => !monitor.internal,
                OutputLayout::Extend | OutputLayout::Mirror => true,
            };
            if monitor.internal {
                if let Some(enabled) = manual.internal {
                    settings.enabled = enabled && monitor.usable;
                } else if manage_lid && lid_closed && external {
                    settings.enabled = false;
                }
            }
            DesiredOutput {
                key: monitor.key.clone(),
                settings,
                mirror_source: None,
            }
        })
        .collect::<Vec<_>>();
    if !outputs.iter().any(|output| output.settings.enabled) {
        // Policy never disables the last usable display, even for an inapplicable exclusive layout.
        if let Some((index, _)) = monitors
            .iter()
            .enumerate()
            .filter(|(_, monitor)| monitor.usable)
            .min_by_key(|(_, monitor)| (!monitor.internal, &monitor.key))
        {
            outputs[index].settings = defaults(monitors[index].identity.clone());
        }
    }
    if layout == OutputLayout::Mirror {
        let source = selected
            .and_then(|profile| profile.mirror_source.as_ref())
            .and_then(|selector| {
                monitors
                    .iter()
                    .zip(&outputs)
                    .find(|(monitor, output)| monitor.matches(selector) && output.settings.enabled)
                    .map(|(_, output)| output.key.clone())
            })
            .or_else(|| {
                outputs
                    .iter()
                    .filter(|output| output.settings.enabled)
                    .map(|output| output.key.clone())
                    .min()
            });
        if let Some(source) = source {
            for output in &mut outputs {
                if output.settings.enabled && output.key != source {
                    output.mirror_source = Some(source.clone());
                }
            }
        }
    }
    DesiredOutputConfiguration {
        profile: selected.map(|profile| profile.name.clone()),
        outputs,
        suspend: manage_lid && lid_closed && !external,
    }
}

/// Restore actual settings only for surviving hardware; new monitors remain safe fallbacks.
pub(crate) fn restore_configuration(
    previous: &DesiredOutputConfiguration,
    monitors: &[Monitor],
) -> DesiredOutputConfiguration {
    let mut restored = previous.clone();
    restored
        .outputs
        .retain(|output| monitors.iter().any(|monitor| monitor.key == output.key));
    for monitor in monitors {
        if !restored
            .outputs
            .iter()
            .any(|output| output.key == monitor.key)
        {
            let mut settings = defaults(monitor.identity.clone());
            settings.enabled = false;
            restored.outputs.push(DesiredOutput {
                key: monitor.key.clone(),
                settings,
                mirror_source: None,
            });
        }
    }
    let source_keys = restored
        .outputs
        .iter()
        .filter(|output| output.settings.enabled)
        .map(|output| output.key.clone())
        .collect::<HashSet<_>>();
    for output in &mut restored.outputs {
        if output
            .mirror_source
            .as_ref()
            .is_some_and(|source| !source_keys.contains(source))
        {
            output.mirror_source = None;
        }
    }
    if !restored
        .outputs
        .iter()
        .any(|output| output.settings.enabled)
    {
        return select_profile(monitors, false, &[], &ManualOverride::default());
    }
    restored
}

/// A diff carries only affected connectors; removing a connector is an explicit disable.
pub(crate) fn changed_outputs<'a>(
    old: &'a DesiredOutputConfiguration,
    new: &'a DesiredOutputConfiguration,
) -> Vec<&'a str> {
    let mut keys = HashSet::new();
    old.outputs
        .iter()
        .chain(&new.outputs)
        .filter_map(|output| {
            let key = output.key.as_str();
            if !keys.insert(key) {
                return None;
            }
            let previous = old.outputs.iter().find(|candidate| candidate.key == key);
            let next = new.outputs.iter().find(|candidate| candidate.key == key);
            let same = match (previous, next) {
                (Some(previous), Some(next)) => {
                    previous.settings.same_configuration(&next.settings)
                        && previous.mirror_source == next.mirror_source
                }
                (None, None) => true,
                _ => false,
            };
            (!same).then_some(key)
        })
        .collect()
}

#[derive(Default)]
pub(crate) struct Debouncer {
    deadline: Option<std::time::Duration>,
}

impl Debouncer {
    pub fn request(
        &mut self,
        now: std::time::Duration,
        active: bool,
    ) -> Option<std::time::Duration> {
        self.deadline = active.then(|| now + std::time::Duration::from_millis(150));
        self.deadline
    }

    pub fn clear(&mut self) {
        self.deadline = None;
    }

    pub fn take_due(&mut self, now: std::time::Duration) -> bool {
        if self.deadline.is_some_and(|deadline| deadline <= now) {
            self.deadline = None;
            true
        } else {
            false
        }
    }
}

pub(crate) struct Placement {
    pub key: String,
    pub size: (i32, i32),
    pub previous: Option<[i32; 2]>,
}

/// Explicit positions win. Preserve an automatic output's old position when
/// it still fits; append new/conflicting outputs in stable hardware-key order.
pub(crate) fn arrange_positions(
    desired: &mut DesiredOutputConfiguration,
    placements: &[Placement],
) -> Result<(), String> {
    let mut occupied = Vec::new();
    let mut automatic = Vec::new();
    let output_iter = desired.outputs.iter().enumerate();
    for (index, output) in output_iter {
        if !output.settings.enabled || output.mirror_source.is_some() {
            continue;
        }
        let placement = placements
            .iter()
            .find(|placement| placement.key == output.key)
            .ok_or("missing output dimensions")?;
        if let Some(position) = output.settings.position {
            occupied.push((position[0], position[1], placement.size.0, placement.size.1));
        } else {
            automatic.push((index, placement));
        }
    }
    automatic.sort_by(|(_, a), (_, b)| {
        (a.previous.is_none(), &a.key).cmp(&(b.previous.is_none(), &b.key))
    });
    for (index, placement) in automatic {
        let fits = |position: [i32; 2]| {
            let (x, y, w, h) = (
                position[0] as i64,
                position[1] as i64,
                placement.size.0 as i64,
                placement.size.1 as i64,
            );
            !occupied.iter().any(|&(ox, oy, ow, oh)| {
                x < ox as i64 + ow as i64
                    && x + w > ox as i64
                    && y < oy as i64 + oh as i64
                    && y + h > oy as i64
            })
        };
        let previous = placement.previous.filter(|position| fits(*position));
        let position = if let Some(previous) = previous {
            previous
        } else {
            let right = occupied
                .iter()
                .map(|&(x, _, w, _)| x as i64 + w as i64)
                .max()
                .unwrap_or(0)
                .max(0);
            let x = i32::try_from(right).map_err(|_| "automatic output position overflows")?;
            x.checked_add(placement.size.0)
                .ok_or("automatic output geometry overflows")?;
            [x, 0]
        };
        desired.outputs[index].settings.position = Some(position);
        occupied.push((position[0], position[1], placement.size.0, placement.size.1));
    }
    let source_positions = desired
        .outputs
        .iter()
        .filter(|output| output.mirror_source.is_none())
        .map(|output| (output.key.clone(), output.settings.position))
        .collect::<std::collections::HashMap<_, _>>();
    for output in &mut desired.outputs {
        if let Some(source) = &output.mirror_source {
            output.settings.position = source_positions.get(source).copied().flatten();
        }
    }
    Ok(())
}

#[cfg(test)]
mod placement_tests {
    use super::*;

    #[test]
    fn automatic_placement_is_stable_and_only_moves_overlapping_neighbors() {
        let monitors = [
            Monitor {
                key: "a".into(),
                connector: "a".into(),
                identity: "a".into(),
                aliases: vec![],
                internal: true,
                usable: true,
            },
            Monitor {
                key: "b".into(),
                connector: "b".into(),
                identity: "b".into(),
                aliases: vec![],
                internal: false,
                usable: true,
            },
        ];
        let mut desired = select_profile(&monitors, false, &[], &ManualOverride::default());
        let placements = [
            Placement {
                key: "b".into(),
                size: (100, 100),
                previous: Some([100, 0]),
            },
            Placement {
                key: "a".into(),
                size: (100, 100),
                previous: Some([0, 0]),
            },
        ];
        arrange_positions(&mut desired, &placements).unwrap();
        assert_eq!(desired.outputs[0].settings.position, Some([0, 0]));
        assert_eq!(desired.outputs[1].settings.position, Some([100, 0]));
        let mut expanded = select_profile(&monitors, false, &[], &ManualOverride::default());
        let placements = [
            Placement {
                key: "a".into(),
                size: (150, 100),
                previous: Some([0, 0]),
            },
            Placement {
                key: "b".into(),
                size: (100, 100),
                previous: Some([100, 0]),
            },
        ];
        arrange_positions(&mut expanded, &placements).unwrap();
        assert_eq!(expanded.outputs[0].settings.position, Some([0, 0]));
        assert_eq!(expanded.outputs[1].settings.position, Some([150, 0]));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn monitor(name: &str, internal: bool) -> Monitor {
        Monitor {
            key: name.into(),
            connector: name.into(),
            identity: format!("edid:{name}"),
            aliases: vec![],
            internal,
            usable: true,
        }
    }

    fn profile(name: &str, outputs: &[&str], layout: OutputLayout) -> OutputProfile {
        OutputProfile {
            name: name.into(),
            outputs: outputs
                .iter()
                .map(|name| defaults((*name).into()))
                .collect(),
            layout,
            confirm_timeout: 15,
            lid_policy: LidPolicy::DockOrSuspend,
            lid_closed: None,
            mirror_source: None,
        }
    }

    #[test]
    fn manual_layouts_reuse_profiles_and_expire_on_monitor_changes() {
        let connected = [monitor("eDP-1", true), monitor("DP-1", false)];
        let mut settings = defaults("DP-1".into());
        settings.enabled = false;
        settings.scale = 1.5;
        let mut mobile = profile("mobile", &["eDP-1"], OutputLayout::InternalOnly);
        mobile.outputs.push(settings);
        let profiles = [mobile];
        for layout in [
            OutputLayout::InternalOnly,
            OutputLayout::ExternalOnly,
            OutputLayout::Extend,
            OutputLayout::Mirror,
        ] {
            let mut manual = ManualOverride {
                layout: Some(layout),
                ..ManualOverride::default()
            };
            manual.observe(&connected, false, &profiles);
            let desired = select_profile(&connected, false, &profiles, &manual);
            assert_eq!(
                desired.outputs[0].settings.enabled,
                layout != OutputLayout::ExternalOnly
            );
            assert_eq!(
                desired.outputs[1].settings.enabled,
                layout != OutputLayout::InternalOnly
            );
            assert_eq!(
                desired.outputs[1].settings.scale, 1.5,
                "manual layout must preserve configured scale"
            );
            assert_eq!(
                desired
                    .outputs
                    .iter()
                    .filter(|output| output.mirror_source.is_some())
                    .count(),
                usize::from(layout == OutputLayout::Mirror)
            );
            manual.observe(&connected[..1], false, &profiles);
            assert_eq!(manual.layout, None);
            assert!(
                select_profile(&connected[..1], false, &profiles, &manual).outputs[0]
                    .settings
                    .enabled
            );
        }
    }

    #[test]
    fn specificity_and_config_order() {
        let monitors = [monitor("eDP-1", true), monitor("DP-1", false)];
        let profiles = [
            profile("mobile", &["eDP-1"], OutputLayout::Extend),
            profile("desk", &["eDP-1", "DP-1"], OutputLayout::Extend),
            profile("tie", &["DP-1", "eDP-1"], OutputLayout::Extend),
        ];
        assert_eq!(
            select_profile(&monitors, false, &profiles, &ManualOverride::default())
                .profile
                .as_deref(),
            Some("desk")
        );
    }

    #[test]
    fn layouts_unknown_outputs_and_safe_fallback() {
        let monitors = [
            monitor("eDP-1", true),
            monitor("DP-1", false),
            monitor("HDMI-A-1", false),
        ];
        for (layout, count) in [
            (OutputLayout::InternalOnly, 1),
            (OutputLayout::ExternalOnly, 2),
            (OutputLayout::Extend, 3),
            (OutputLayout::Mirror, 3),
        ] {
            let desired = select_profile(
                &monitors,
                false,
                &[profile("test", &["eDP-1"], layout)],
                &ManualOverride::default(),
            );
            assert_eq!(
                desired
                    .outputs
                    .iter()
                    .filter(|output| output.settings.enabled)
                    .count(),
                count
            );
            assert_eq!(
                desired
                    .outputs
                    .iter()
                    .filter(|output| output.mirror_source.is_some())
                    .count(),
                if layout == OutputLayout::Mirror { 2 } else { 0 }
            );
        }
        let missing = [profile("absent", &["DP-9"], OutputLayout::ExternalOnly)];
        let fallback = select_profile(&monitors, false, &missing, &ManualOverride::default());
        assert_eq!(fallback.profile, None);
        assert!(
            fallback
                .outputs
                .iter()
                .all(|output| output.settings.enabled)
        );
        let internal = &monitors[..1];
        let mut disabled = profile("disabled", &["eDP-1"], OutputLayout::Extend);
        disabled.outputs[0].enabled = false;
        assert!(
            select_profile(internal, false, &[disabled], &ManualOverride::default()).outputs[0]
                .settings
                .enabled
        );
    }

    #[test]
    fn override_expires_only_on_material_change_or_invalid_reload() {
        let monitors = [monitor("eDP-1", true), monitor("DP-1", false)];
        let profiles = [profile("mobile", &["eDP-1"], OutputLayout::InternalOnly)];
        let mut manual = ManualOverride::default();
        manual.observe(&monitors, false, &profiles);
        manual.profile = Some("mobile".into());
        manual.observe(&monitors, false, &profiles);
        assert_eq!(manual.profile.as_deref(), Some("mobile"));
        manual.observe(&monitors, true, &profiles);
        assert_eq!(manual.profile, None);
        manual.profile = Some("mobile".into());
        manual.observe(&monitors, true, &[]);
        assert_eq!(manual.profile, None);
    }

    #[test]
    fn lid_dock_undock_and_internal_toggle_never_disable_last_output() {
        let monitors = [monitor("eDP-1", true), monitor("DP-1", false)];
        let docked = select_profile(&monitors, true, &[], &ManualOverride::default());
        assert!(!docked.suspend);
        assert!(!docked.outputs[0].settings.enabled);
        assert!(docked.outputs[1].settings.enabled);
        let manual = ManualOverride {
            internal: Some(false),
            ..ManualOverride::default()
        };
        let undocked = select_profile(&monitors[..1], true, &[], &manual);
        assert!(undocked.suspend);
        assert!(undocked.outputs[0].settings.enabled);
    }

    #[test]
    fn desired_diff_is_local() {
        let monitors = [monitor("eDP-1", true), monitor("DP-1", false)];
        let old = select_profile(&monitors, false, &[], &ManualOverride::default());
        assert!(changed_outputs(&old, &old).is_empty());
        let mut new = old.clone();
        new.outputs[1].settings.scale = 1.5;
        assert_eq!(changed_outputs(&old, &new), vec!["DP-1"]);
    }
    #[test]
    fn mirror_removal_and_extend_restore_single_ownership_plan() {
        let monitors = [monitor("eDP-1", true), monitor("DP-1", false)];
        let mut mirror = profile("mirror", &["eDP-1", "DP-1"], OutputLayout::Mirror);
        mirror.mirror_source = Some("eDP-1".into());
        let old = select_profile(
            &monitors,
            false,
            &[mirror.clone()],
            &ManualOverride::default(),
        );
        assert_eq!(old.outputs[1].mirror_source.as_deref(), Some("eDP-1"));
        let removed = select_profile(&monitors[..1], false, &[mirror], &ManualOverride::default());
        assert!(removed.outputs[0].mirror_source.is_none());
        assert!(removed.outputs[0].settings.enabled);
        let extended = select_profile(&monitors, false, &[], &ManualOverride::default());
        assert!(
            extended
                .outputs
                .iter()
                .all(|output| output.mirror_source.is_none())
        );
        assert_eq!(changed_outputs(&old, &extended), vec!["DP-1"]);
    }

    #[test]
    fn optional_outputs_and_lid_specific_profiles() {
        let monitors = [monitor("eDP-1", true)];
        let mut mobile = profile("mobile", &["eDP-1", "DP-1"], OutputLayout::Extend);
        mobile.outputs[1].required = false;
        mobile.lid_closed = Some(false);
        assert_eq!(
            select_profile(
                &monitors,
                false,
                &[mobile.clone()],
                &ManualOverride::default()
            )
            .profile
            .as_deref(),
            Some("mobile")
        );
        assert_eq!(
            select_profile(&monitors, true, &[mobile], &ManualOverride::default()).profile,
            None
        );
    }

    #[test]
    fn swapping_monitor_on_same_connector_expires_override() {
        let mut monitors = [monitor("eDP-1", true), monitor("DP-1", false)];
        let profiles = [profile("mobile", &["eDP-1"], OutputLayout::InternalOnly)];
        let mut manual = ManualOverride::default();
        manual.observe(&monitors, false, &profiles);
        manual.profile = Some("mobile".into());
        monitors[1].identity = "edid:new-monitor".into();
        manual.observe(&monitors, false, &profiles);
        assert!(manual.profile.is_none());
    }

    #[test]
    fn restoration_survives_disconnect_and_keeps_new_monitor_as_emergency_fallback() {
        let monitors = [monitor("eDP-1", true), monitor("DP-1", false)];
        let previous = select_profile(&monitors, false, &[], &ManualOverride::default());
        let restored = restore_configuration(&previous, &monitors[..1]);
        assert_eq!(restored.outputs.len(), 1);
        assert!(restored.outputs[0].settings.enabled);
        let unknown = [monitor("HDMI-A-2", false)];
        assert!(
            restore_configuration(&previous, &unknown).outputs[0]
                .settings
                .enabled
        );
    }
}

#[cfg(test)]
mod debounce_tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn a_dock_burst_produces_one_reconciliation_after_last_event() {
        let mut debounce = Debouncer::default();
        for time in [0, 40, 110] {
            debounce.request(Duration::from_millis(time), true);
        }
        assert!(!debounce.take_due(Duration::from_millis(150)));
        assert!(!debounce.take_due(Duration::from_millis(259)));
        assert!(debounce.take_due(Duration::from_millis(260)));
        assert!(!debounce.take_due(Duration::from_millis(400)));
    }

    #[test]
    fn inactivity_cancels_deadlines_and_resume_uses_fresh_state() {
        let mut debounce = Debouncer::default();
        debounce.request(Duration::ZERO, true);
        assert_eq!(debounce.request(Duration::from_millis(30), false), None);
        assert!(!debounce.take_due(Duration::from_secs(10)));
        debounce.request(Duration::from_secs(10), true);
        assert!(debounce.take_due(Duration::from_millis(10150)));
    }
}
