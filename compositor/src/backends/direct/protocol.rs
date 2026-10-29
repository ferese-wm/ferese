//! Standard output requests enter the same validated hardware transaction as profiles.

use crate::Ferese;
use crate::config::OutputSettings;
use crate::output_policy::{DesiredOutput, DesiredOutputConfiguration};

/// TEST_ONLY never changes policy, confirmation timers, or the published desktop.
/// Applying a standard request persists until a native override or material
/// topology/lid change. A successful apply is already committed, as required by
/// wlr-output-management; it does not arm Ferese's interactive confirmation timer.
pub(crate) fn configure_outputs(
    state: &mut Ferese,
    settings: &[(String, OutputSettings)],
    test_only: bool,
) -> Result<(), String> {
    if state.session_lock.active() {
        return Err("outputs cannot be configured while the session is locked".into());
    }
    let backend = state
        .direct_backend
        .as_ref()
        .ok_or("direct backend unavailable")?;
    if !backend.active || backend.lid.pending() || backend.topology.reconciling {
        return Err("output session is inactive or reconciling".into());
    }
    if settings.len() != backend.monitors.len() {
        return Err("configuration must include every connected output".into());
    }
    let mut outputs = Vec::with_capacity(settings.len());
    for monitor in &backend.monitors {
        let mut matching = settings
            .iter()
            .filter(|(identity, _)| identity == &monitor.identity);
        let (_, settings) = matching
            .next()
            .ok_or("configuration omitted a connected output")?;
        if matching.next().is_some() {
            return Err("configuration repeats a connected output".into());
        }
        outputs.push(DesiredOutput {
            key: monitor.key.clone(),
            settings: settings.clone(),
            mirror_source: None,
        });
    }
    let mut desired = DesiredOutputConfiguration {
        profile: None,
        outputs,
        suspend: false,
    };
    super::resolve_output_positions(backend, &mut desired)?;
    if super::validate_output_configuration(backend, &desired, &[])? == 0 {
        return Err("configuration would disable every usable output".into());
    }
    if test_only {
        return Ok(());
    }
    let previous = backend.manual_outputs.clone();
    let known_good = backend.applied_outputs.clone();
    let backend = state
        .direct_backend
        .as_mut()
        .expect("validated direct backend exists");
    backend.manual_outputs.configuration = Some(desired);
    backend.manual_outputs.profile = None;
    backend.manual_outputs.internal = None;
    backend.manual_outputs.layout = None;
    super::reconcile_outputs(state, super::ReconcileReason::Configuration);
    let backend = state
        .direct_backend
        .as_mut()
        .expect("direct backend remains installed");
    if let Some(error) = backend.output_error.clone() {
        backend.manual_outputs = previous;
        backend.revert_outputs = Some(known_good);
        super::reconcile_outputs(state, super::ReconcileReason::Configuration);
        return Err(error);
    }
    // A pending native confirmation must not later undo a successful request.
    if let Some(timer) = backend.confirmation_timer.take() {
        state.loop_handle.remove(timer);
    }
    backend.confirmation = None;
    state.notify_monitor_state();
    Ok(())
}

/// wl_output/head names keep familiar connector labels unless two GPUs expose
/// that label. The suffix derives from the persistent GPU path, never cardN.
pub(super) fn output_name(
    connector: &str,
    identity: &str,
    monitors: &[crate::output_policy::Monitor],
) -> String {
    if monitors
        .iter()
        .filter(|monitor| monitor.connector == connector)
        .count()
        <= 1
    {
        return connector.to_owned();
    }
    let gpu = monitors
        .iter()
        .find(|monitor| monitor.identity == identity)
        .and_then(|monitor| monitor.key.rsplit_once('/').map(|(gpu, _)| gpu))
        .unwrap_or(identity);
    format!(
        "{connector}-gpu-{:016x}",
        super::stable_hash(gpu.as_bytes())
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::output_policy::Monitor;

    fn monitor(gpu: &str, connector: &str) -> Monitor {
        Monitor {
            key: format!("{gpu}/{connector}"),
            connector: connector.into(),
            identity: format!("{gpu}:{connector}"),
            aliases: Vec::new(),
            internal: false,
            usable: true,
        }
    }

    #[test]
    fn connector_names_are_unchanged_until_a_collision_and_qualified_by_stable_gpu() {
        let first = monitor("pci-0000-01-00", "HDMI-A-1");
        let second = monitor("pci-0000-02-00", "HDMI-A-1");
        assert_eq!(
            output_name(
                &first.connector,
                &first.identity,
                std::slice::from_ref(&first)
            ),
            "HDMI-A-1"
        );
        let monitors = [first.clone(), second.clone()];
        let first_name = output_name(&first.connector, &first.identity, &monitors);
        let second_name = output_name(&second.connector, &second.identity, &monitors);
        assert_ne!(first_name, second_name);
        assert_eq!(
            output_name(&first.connector, &first.identity, &[second, first.clone()]),
            first_name
        );
        assert!(
            first_name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        );
    }

    #[test]
    fn exact_protocol_policy_survives_noop_events_and_expires_on_lid_or_hardware_change() {
        let mut first = monitor("pci-0000-01-00", "HDMI-A-1");
        let mut manual = crate::output_policy::ManualOverride::default();
        manual.observe(std::slice::from_ref(&first), false, &[]);
        let mut desired =
            crate::output_policy::select_profile(std::slice::from_ref(&first), false, &[], &manual);
        desired.outputs[0].settings.scale = 1.75;
        manual.configuration = Some(desired);
        manual.observe(std::slice::from_ref(&first), false, &[]);
        assert_eq!(
            crate::output_policy::select_profile(std::slice::from_ref(&first), false, &[], &manual)
                .outputs[0]
                .settings
                .scale,
            1.75
        );
        manual.observe(std::slice::from_ref(&first), true, &[]);
        assert!(manual.configuration.is_none());
        manual.configuration = Some(DesiredOutputConfiguration::default());
        first.identity.push_str("replacement");
        manual.observe(&[first], true, &[]);
        assert!(manual.configuration.is_none());
    }
}
