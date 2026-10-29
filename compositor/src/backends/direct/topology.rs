//! Desired device identities survive temporary loss of their DRM resources.
use std::io;
use std::path::PathBuf;

use super::{
    DrmNode, Duration, Ferese, HashMap, HashSet, Instant, OutputMode, Path, RegistrationToken,
    Scale, Session, TimeoutAction, Timer, all_gpus, cancel_output_timers, drm_device_key, lid,
    monotonic_now, open_device, output_transform, refresh_connected_info, refresh_monitor_policy,
    remember_applied_configuration, remove_device, rescan_device, sync_battery_timer, transaction,
    validate_desired_outputs,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Availability<Node = DrmNode> {
    Ready(Node),
    Degraded(Node),
    Missing,
    Unavailable,
}

pub(super) struct DeviceState<Node = DrmNode> {
    pub availability: Availability<Node>,
    identity: Option<PathBuf>,
    retry: Retry,
}

impl<Node> Default for DeviceState<Node> {
    fn default() -> Self {
        Self {
            availability: Availability::Missing,
            identity: None,
            retry: Retry::default(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ReconcileReason {
    Configuration,
    Hotplug,
    Resume,
    Retry,
}

#[derive(Default)]
struct Retry {
    failures: u32,
    deadline: Option<Instant>,
}

impl Retry {
    fn failed(&mut self, now: Instant) {
        self.failures = self.failures.saturating_add(1);
        self.deadline =
            Some(now + Duration::from_secs(1u64 << self.failures.saturating_sub(1).min(5)));
    }

    fn due(&self, now: Instant) -> bool {
        self.deadline.is_none_or(|deadline| deadline <= now)
    }
}

type ObservedDevice = (PathBuf, Option<PathBuf>);

struct DeviceWork<Node> {
    paths: Vec<PathBuf>,
    deferred: Vec<Node>,
}

pub(super) fn device_identity(path: &Path) -> Option<PathBuf> {
    // The parent hardware identity survives cardN renumbering after unplug.
    std::fs::canonicalize(
        Path::new("/sys/class/drm")
            .join(path.file_name()?)
            .join("device"),
    )
    .ok()
}

impl<Node> DeviceState<Node> {
    fn current_path<'a>(
        &self,
        original: &Path,
        observed: &'a [ObservedDevice],
    ) -> Option<&'a Path> {
        observed
            .iter()
            .find(|(path, identity)| match &self.identity {
                Some(expected) => identity.as_ref() == Some(expected),
                None => path == original,
            })
            .map(|(path, _)| path.as_path())
    }

    pub fn failed(&mut self, now: Instant) {
        self.availability = Availability::Unavailable;
        self.retry.failed(now);
    }

    pub fn ready(&mut self, node: Node) {
        self.availability = Availability::Ready(node);
        self.retry = Retry::default();
    }

    pub fn missing(&mut self) {
        self.availability = Availability::Missing;
        self.retry = Retry::default();
    }

    fn degraded(&mut self, node: Node, now: Instant) {
        self.failed(now);
        self.availability = Availability::Degraded(node);
    }
}

pub(super) struct Topology<Node = DrmNode> {
    // Hardware paths are stable keys; use the card path only when sysfs identity
    // is unavailable. A reused card number must not replace another GPU's slot.
    pub devices: HashMap<PathBuf, DeviceState<Node>>,
    discovery_retry: Retry,
    pub invalidated: HashSet<Node>,
    pub reconciling: bool,
    pub dirty: bool,
    pub layout_dirty: bool,
    pub remember_applied: bool,
    pub retry_timer: Option<RegistrationToken>,
    pub settle_timer: Option<RegistrationToken>,
    pub coalescer: crate::output_policy::Debouncer,
}

impl<Node> Default for Topology<Node> {
    fn default() -> Self {
        Self {
            devices: HashMap::new(),
            discovery_retry: Retry::default(),
            invalidated: HashSet::new(),
            reconciling: false,
            dirty: false,
            layout_dirty: false,
            remember_applied: false,
            retry_timer: None,
            settle_timer: None,
            coalescer: Default::default(),
        }
    }
}

// Node identity is opaque to reconciliation; tests use integers without opening
// DRM devices or depending on which nodes happen to exist in the host's sysfs.
impl<Node: Copy + Eq + std::hash::Hash> Topology<Node> {
    fn observe_discovery(
        &mut self,
        observed: io::Result<Vec<ObservedDevice>>,
        now: Instant,
    ) -> io::Result<Vec<ObservedDevice>> {
        match observed {
            Ok(observed) => {
                self.discovery_retry = Retry::default();
                self.observe_devices(&observed);
                Ok(observed)
            }
            Err(error) => {
                self.discovery_retry.failed(now);
                self.dirty = true;
                Err(error)
            }
        }
    }

    fn work(&self, reason: ReconcileReason, now: Instant) -> DeviceWork<Node> {
        let mut paths = Vec::new();
        let mut deferred = Vec::new();
        for (path, device) in &self.devices {
            if reason == ReconcileReason::Retry && !device.retry.due(now) {
                if let Availability::Ready(node) | Availability::Degraded(node) =
                    device.availability
                {
                    deferred.push(node);
                }
            } else {
                paths.push(path.clone());
            }
        }
        paths.sort();
        DeviceWork { paths, deferred }
    }

    // Partial recovery must not disable a working display based on a profile
    // whose destination GPU is still unavailable. Apply the complete policy
    // once every participating device can be reconciled again.
    fn hold_healthy_outputs(
        &self,
        reason: ReconcileReason,
        now: Instant,
        ready: &mut Vec<(PathBuf, Node)>,
        deferred: &mut Vec<Node>,
    ) {
        if reason != ReconcileReason::Retry
            || (deferred.is_empty() && !self.devices.values().any(|device| !device.retry.due(now)))
        {
            return;
        }
        ready.retain(|(path, node)| {
            let healthy = self.devices.get(path).is_some_and(|device| {
                device.availability == Availability::Ready(*node) && device.retry.deadline.is_none()
            });
            if healthy {
                deferred.push(*node);
            }
            !healthy
        });
    }

    pub fn observe_devices(&mut self, observed: &[ObservedDevice]) {
        for (path, identity) in observed {
            let key = identity.as_ref().unwrap_or(path);
            let fallback = if identity.is_some() {
                self.devices.remove(path)
            } else {
                None
            };
            if self.devices.contains_key(key) {
                continue;
            }
            // Preserve a slot first discovered before its sysfs identity was
            // available, including its live DRM resources and retry state.
            let mut slot = fallback.unwrap_or_default();
            slot.identity = identity.clone();
            self.devices.insert(key.clone(), slot);
        }
    }

    fn retire_changed_devices(
        &mut self,
        observed: &[ObservedDevice],
        node_for_path: impl Fn(&Path) -> Option<Node>,
    ) -> HashSet<Node> {
        let mut retired = HashSet::new();
        for (key, slot) in &mut self.devices {
            if let Availability::Ready(previous) | Availability::Degraded(previous) =
                slot.availability
                && slot
                    .current_path(key, observed)
                    .is_none_or(|path| node_for_path(path).is_some_and(|node| node != previous))
            {
                retired.insert(previous);
                slot.missing();
            }
        }
        retired
    }

    pub fn request(&mut self, active: bool) -> bool {
        self.dirty = true;
        active && !self.reconciling
    }

    pub fn retry_deadline(&self, active: bool) -> Option<Instant> {
        active
            .then(|| {
                // Device work cannot proceed while discovery is unknown. An
                // expired device deadline must not spin the discovery retry.
                self.discovery_retry.deadline.or_else(|| {
                    self.devices
                        .values()
                        .filter_map(|device| device.retry.deadline)
                        .min()
                })
            })
            .flatten()
    }
}

fn slot_mut<'a>(state: &'a mut Ferese, path: &Path) -> &'a mut DeviceState {
    state
        .direct_backend
        .as_mut()
        .expect("DRM callbacks require an installed direct backend")
        .topology
        .devices
        .get_mut(path)
        .expect("the discovery path must have a topology slot")
}

/// The only entry point for applying the current output configuration to DRM.
pub(super) fn reconcile_outputs(state: &mut Ferese, reason: ReconcileReason) {
    let Some(backend) = state.direct_backend.as_mut() else {
        return;
    };
    if !backend
        .topology
        .request(backend.active && !backend.lid.pending())
    {
        return;
    }

    if let Some(token) = backend.topology.settle_timer.take() {
        state.loop_handle.remove(token);
    }
    backend.topology.coalescer.clear();
    backend.topology.reconciling = true;
    backend.topology.dirty = false;
    let previous_error = backend.output_error.take();
    if reason == ReconcileReason::Resume {
        for clock in backend.presentation.values_mut() {
            clock.reset_timing();
        }
    }
    if let Some(token) = backend.topology.retry_timer.take() {
        state.loop_handle.remove(token);
    }
    let mut invalidated = std::mem::take(&mut backend.topology.invalidated);
    let observed = all_gpus(backend.session.seat()).map(|paths| {
        paths
            .into_iter()
            .map(|path| {
                let identity = device_identity(&path);
                (path, identity)
            })
            .collect::<Vec<_>>()
    });
    let automatic = state
        .output_profiles
        .iter()
        .any(|profile| profile.outputs.iter().any(|output| output.auto_refresh));
    backend.low_power = automatic
        && super::super::power::low_power(
            backend.low_power,
            super::super::power::battery_percent(),
        );

    let observed = backend.topology.observe_discovery(observed, Instant::now());
    if let Ok(observed) = &observed {
        // Retire every obsolete node before opening any replacement. Otherwise
        // a later missing slot could remove a new GPU that reused its dev_t.
        invalidated.extend(
            backend
                .topology
                .retire_changed_devices(observed, |path| DrmNode::from_path(path).ok()),
        );
    }
    state.begin_desktop_transition();
    // Explicit device invalidations are authoritative even if enumeration
    // fails. A failed scan alone must never retire a working device.
    for node in invalidated {
        remove_device(state, node);
    }
    let observed = match observed {
        Ok(observed) => observed,
        Err(error) => {
            tracing::warn!(%error, "could not enumerate DRM topology; retaining live devices");
            state
                .direct_backend
                .as_mut()
                .expect("direct backend exists")
                .output_error = Some(error.to_string());
            let deferred = state
                .direct_backend
                .as_ref()
                .expect("direct backend exists")
                .devices
                .keys()
                .copied()
                .collect::<Vec<_>>();
            finish_reconciliation(state, &deferred);
            return;
        }
    };
    let DeviceWork {
        paths,
        mut deferred,
    } = state
        .direct_backend
        .as_ref()
        .expect("direct backend exists")
        .topology
        .work(reason, Instant::now());
    let mut ready = Vec::new();

    for path in paths {
        let slot = &state
            .direct_backend
            .as_ref()
            .expect("DRM callbacks require an installed direct backend")
            .topology
            .devices[&path];
        let refresh = matches!(reason, ReconcileReason::Hotplug | ReconcileReason::Resume)
            || slot.retry.deadline.is_some();
        let current_path = slot.current_path(&path, &observed);
        let previous = match slot.availability {
            Availability::Ready(node) | Availability::Degraded(node) => Some(node),
            _ => None,
        };
        let existing = match current_path.map(DrmNode::from_path).transpose() {
            Ok(node) => node.filter(|node| {
                state
                    .direct_backend
                    .as_ref()
                    .expect("direct backend exists")
                    .devices
                    .contains_key(node)
            }),
            Err(error) => {
                tracing::warn!(%error, ?path, "could not inspect DRM node; retaining live resources");
                if let Some(node) = previous {
                    slot_mut(state, &path).degraded(node, Instant::now());
                    deferred.push(node);
                } else {
                    slot_mut(state, &path).failed(Instant::now());
                }
                state
                    .direct_backend
                    .as_mut()
                    .expect("direct backend exists")
                    .output_error = Some(error.to_string());
                continue;
            }
        };
        let previous = previous.or(existing);
        if current_path.is_none() {
            if let Some(node) = previous {
                remove_device(state, node);
            }
            slot_mut(state, &path).missing();
            continue;
        }

        if let Some(node) = previous.filter(|node| Some(*node) != existing) {
            remove_device(state, node);
        }
        let node = match existing {
            Some(node) => node,
            None => match open_device(
                state,
                current_path.expect("device discovery found a path before attempting to open it"),
            ) {
                Ok(node) => node,
                Err(error) => {
                    tracing::warn!(%error, ?path, device_path = ?current_path, "DRM device unavailable; will retry");
                    slot_mut(state, &path).failed(Instant::now());
                    continue;
                }
            },
        };

        let mut failed_outputs = Vec::new();
        let reactivate = existing.is_some()
            && (reason == ReconcileReason::Resume
                || !state
                    .direct_backend
                    .as_ref()
                    .expect("direct backend exists")
                    .devices[&node]
                    .drm
                    .is_active());
        let activation = if reactivate {
            let device = state
                .direct_backend
                .as_mut()
                .expect("DRM callbacks require an installed direct backend")
                .devices
                .get_mut(&node)
                .expect("the selected DRM device must remain registered during this operation");
            device.drm.activate(true).map(|()| {
                for (crtc, output) in &mut device.outputs {
                    cancel_output_timers(&state.loop_handle, output);
                    output.surface.reset_buffers();
                    output.dmabuf_feedback.invalidate();
                    output.primary_commit = None;
                    output.capture_texture = Default::default();
                    output.frame_pending = false;
                    output.lock_frame_pending = false;
                    output.power.suspended();
                    state.session_lock.output_added(&output.output);
                    state.display_presentation.remove_output(&output.output);
                    state
                        .desktop_transition
                        .as_mut()
                        .expect("desktop transition must be started before publication or retirement")
                        .redraw
                        .insert(output.output.clone());

                    if let Err(error) = output.surface.clear().and_then(|()| output.surface.reset_state()) {
                        tracing::warn!(%error, ?node, ?crtc, "output reset failed; recreating output");
                        failed_outputs.push(*crtc);
                    }
                }
            })
        } else {
            Ok(())
        };

        if let Err(error) = activation {
            tracing::warn!(%error, ?node, "DRM reactivation failed; retiring unavailable outputs");
            remove_device(state, node);
            slot_mut(state, &path).failed(Instant::now());
            continue;
        }

        for crtc in failed_outputs {
            let mut output = state
                .direct_backend
                .as_mut()
                .expect("DRM callbacks require an installed direct backend")
                .devices
                .get_mut(&node)
                .expect("the selected DRM device must remain registered during this operation")
                .outputs
                .remove(&crtc)
                .expect("the selected CRTC must remain registered during this operation");
            cancel_output_timers(&state.loop_handle, &mut output);
            if let Some(global) = output.global.take() {
                state.display_handle.disable_global::<Ferese>(global);
            }
            state.unregister_output(&output.output);
            state
                .direct_backend
                .as_mut()
                .expect("DRM callbacks require an installed direct backend")
                .presentation
                .remove(&(node, crtc));
        }

        let device = state
            .direct_backend
            .as_mut()
            .expect("reconciliation requires a direct backend")
            .devices
            .get_mut(&node)
            .expect("device remains registered");
        if (refresh || reactivate || device.inventory.is_none())
            && let Err(error) = super::refresh_connector_inventory(device)
        {
            tracing::warn!(%error, ?node, "connector discovery failed; retaining the previous inventory");
            slot_mut(state, &path).degraded(node, Instant::now());
            deferred.push(node);
            state
                .direct_backend
                .as_mut()
                .expect("reconciliation requires a direct backend")
                .output_error = Some(error.to_string());
            continue;
        }
        ready.push((path, node));
    }

    state
        .direct_backend
        .as_ref()
        .expect("reconciliation requires a direct backend")
        .topology
        .hold_healthy_outputs(reason, Instant::now(), &mut ready, &mut deferred);

    let known_good = state
        .direct_backend
        .as_ref()
        .expect("DRM callbacks require an installed direct backend")
        .applied_outputs
        .clone();
    // A background retry may advance independently, but a fresh request must
    // not partially apply while another device's inventory is unknown.
    let blocked = reason != ReconcileReason::Retry && !deferred.is_empty();
    super::leasing::prune(state);
    if let Err(error) = refresh_monitor_policy(state).and_then(|()| {
        transaction::retire_disconnected(state);
        if blocked {
            return Err(state
                .direct_backend
                .as_ref()
                .expect("reconciliation requires a direct backend")
                .output_error
                .clone()
                .unwrap_or_else(|| "connector discovery is incomplete".into()));
        }
        validate_desired_outputs(state, &deferred)
    }) {
        tracing::warn!(%error, "monitor policy validation failed; retaining applied outputs");
        state
            .direct_backend
            .as_mut()
            .expect("DRM callbacks require an installed direct backend")
            .output_error = Some(error);
        if !blocked {
            for (path, node) in &ready {
                slot_mut(state, path).degraded(*node, Instant::now());
            }
        }
    } else {
        // Activate destinations before disabling former owners, including across GPUs.
        ready.sort_by_key(|(_, node)| {
            let backend = state
                .direct_backend
                .as_ref()
                .expect("DRM callbacks require an installed direct backend");
            let prefix = format!("{}/", drm_device_key(&backend.devices[node].drm));
            let desired = backend
                .desired_outputs
                .outputs
                .iter()
                .filter(|output| output.settings.enabled && output.key.starts_with(&prefix));
            let logical_sources = desired
                .clone()
                .filter(|output| output.mirror_source.is_none())
                .count();
            std::cmp::Reverse((logical_sources, desired.count()))
        });
        let mut succeeded = Vec::new();
        for (path, node) in &ready {
            let complete = rescan_device(state, *node);
            let entry = slot_mut(state, path);
            if complete == Ok(true) {
                entry.ready(*node);
                succeeded.push(*node);
            } else {
                entry.degraded(*node, Instant::now());
                // Device boundaries are not cross-GPU atomic. Compensate already
                // applied devices using their last actual hardware settings.
                if !known_good.outputs.is_empty() {
                    let backend = state
                        .direct_backend
                        .as_mut()
                        .expect("DRM callbacks require an installed direct backend");
                    let requested = backend.desired_outputs.clone();
                    backend.desired_outputs =
                        crate::output_policy::restore_configuration(&known_good, &backend.monitors);
                    for previous in succeeded {
                        let _ = rescan_device(state, previous);
                    }
                    state
                        .direct_backend
                        .as_mut()
                        .expect("DRM callbacks require an installed direct backend")
                        .desired_outputs = requested;
                }
                break;
            }
        }
        if state
            .direct_backend
            .as_ref()
            .expect("DRM callbacks require an installed direct backend")
            .output_error
            .is_none()
            && deferred.is_empty()
        {
            remember_applied_configuration(state);
        }
    }

    if !deferred.is_empty()
        && state
            .direct_backend
            .as_ref()
            .expect("direct backend exists")
            .output_error
            .is_none()
    {
        state
            .direct_backend
            .as_mut()
            .expect("direct backend exists")
            .output_error = previous_error;
    }
    transaction::recover_last_output(state, &deferred);
    finish_reconciliation(state, &deferred);
}

fn finish_reconciliation(state: &mut Ferese, deferred: &[DrmNode]) {
    let mut desktop_changes = publish_runtime_desktop(state);
    if std::mem::take(
        &mut state
            .direct_backend
            .as_mut()
            .expect("DRM callbacks require an installed direct backend")
            .topology
            .layout_dirty,
    ) {
        desktop_changes.layout = true;
        desktop_changes.shell = true;
        desktop_changes
            .outputs
            .extend(state.space.outputs().cloned());
    }
    let backend = state
        .direct_backend
        .as_mut()
        .expect("DRM callbacks require an installed direct backend");
    backend.topology.reconciling = false;
    let retry = backend.topology.retry_deadline(backend.active);
    if std::mem::take(&mut backend.topology.remember_applied) {
        remember_applied_configuration(state);
    }
    refresh_connected_info(state, deferred);
    if let Some(deadline) = retry {
        match state
            .loop_handle
            .insert_source(Timer::from_deadline(deadline), |_, _, state| {
                if let Some(backend) = state.direct_backend.as_mut() {
                    backend.topology.retry_timer = None;
                }
                reconcile_outputs(state, ReconcileReason::Retry);
                TimeoutAction::Drop
            }) {
            Ok(token) => {
                state
                    .direct_backend
                    .as_mut()
                    .expect("DRM callbacks require an installed direct backend")
                    .topology
                    .retry_timer = Some(token)
            }
            Err(error) => tracing::warn!(%error, "could not arm DRM recovery deadline"),
        }
    }

    sync_battery_timer(state);
    state.finish_desktop_transition(desktop_changes);
    super::leasing::refresh(state);
    state.notify_monitor_state();
    lid::apply_suspend_policy(state);
}

/// Publish only hardware present after application, compensation and recovery.
fn publish_runtime_desktop(state: &mut Ferese) -> crate::state::DesktopChanges {
    let mut outputs = state
        .direct_backend
        .as_ref()
        .expect("DRM callbacks require an installed direct backend")
        .devices
        .values()
        .flat_map(|device| device.outputs.values())
        .filter(|output| output.mirror_source.is_none())
        .map(|output| {
            (
                output.identity.clone(),
                output.output.clone(),
                output.mode,
                output.settings.clone(),
            )
        })
        .collect::<Vec<_>>();
    outputs.sort_by(|left, right| left.0.cmp(&right.0));
    let mut right_edge = 0;
    let runtime = outputs
        .into_iter()
        .map(|(identity, output, mode, settings)| {
            let position = settings.position.unwrap_or_else(|| {
                state
                    .space
                    .output_geometry(&output)
                    .map(|geometry| [geometry.loc.x, geometry.loc.y])
                    .unwrap_or([right_edge, 0])
            });
            let mode = OutputMode::from(mode);
            let transform = output_transform(settings.transform);
            let width =
                (transform.transform_size(mode.size).w as f64 / settings.scale).ceil() as i32;
            right_edge = right_edge.max(position[0].saturating_add(width));
            crate::state::DesktopOutput {
                output,
                identity,
                mode,
                transform,
                scale: Scale::Fractional(settings.scale),
                position: (position[0], position[1]).into(),
            }
        })
        .collect();
    let changes = state
        .publish_desktop(runtime)
        .expect("validated usable output inventory");
    for device in state
        .direct_backend
        .as_mut()
        .expect("DRM callbacks require an installed direct backend")
        .devices
        .values_mut()
    {
        for output in device.outputs.values_mut() {
            if output.mirror_source.is_none() {
                if output.global.is_none() {
                    output.global =
                        Some(output.output.create_global::<Ferese>(&state.display_handle));
                }
            } else {
                // Mirrors are physical scanouts, not workspace-owning outputs.
                output.output.change_current_state(
                    Some(OutputMode::from(output.mode)),
                    Some(output_transform(output.settings.transform)),
                    Some(Scale::Fractional(output.settings.scale)),
                    None,
                );
            }
            output
                .surface
                .set_output_mode_source((&output.output).into());
        }
    }
    changes
}

const SETTLE: Duration = Duration::from_millis(150);

/// Trailing-edge coalescing; inactive events retain dirtiness without arming a timer.
pub(super) fn hotplug(state: &mut Ferese) {
    let Some(backend) = state.direct_backend.as_mut() else {
        return;
    };
    backend.topology.dirty = true;
    if let Some(token) = backend.topology.settle_timer.take() {
        state.loop_handle.remove(token);
    }
    let deadline = backend
        .topology
        .coalescer
        .request(monotonic_now(), backend.active && !backend.lid.pending());
    if deadline.is_none() {
        return;
    }
    match state
        .loop_handle
        .insert_source(Timer::from_duration(SETTLE), |_, _, state| {
            if let Some(backend) = state.direct_backend.as_mut() {
                backend.topology.settle_timer = None;
                if !backend.topology.coalescer.take_due(monotonic_now()) {
                    return TimeoutAction::Drop;
                }
            }
            reconcile_outputs(state, ReconcileReason::Hotplug);
            TimeoutAction::Drop
        }) {
        Ok(token) => {
            state
                .direct_backend
                .as_mut()
                .expect("DRM callbacks require an installed direct backend")
                .topology
                .settle_timer = Some(token)
        }
        Err(error) => {
            tracing::warn!(%error, "could not debounce output events");
            reconcile_outputs(state, ReconcileReason::Hotplug);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_discovery_keeps_live_devices_and_pending_invalidations() {
        let now = Instant::now();
        let card = PathBuf::from("/dev/dri/card0");
        let mut topology = Topology::<u64>::default();
        let mut device = DeviceState::default();
        device.ready(7);
        topology.devices.insert(card.clone(), device);
        topology.invalidated.insert(9);

        let error = topology
            .observe_discovery(Err(io::Error::other("udev unavailable")), now)
            .unwrap_err();
        assert_eq!(error.to_string(), "udev unavailable");
        assert_eq!(topology.devices[&card].availability, Availability::Ready(7));
        assert_eq!(topology.invalidated, HashSet::from([9]));
        assert!(topology.dirty);
        assert_eq!(
            topology.retry_deadline(true),
            Some(now + Duration::from_secs(1))
        );
        assert_eq!(topology.retry_deadline(false), None);

        // Device deadlines cannot cause a busy loop while global discovery is
        // unavailable, and a successful scan restores the normal retry policy.
        topology
            .devices
            .get_mut(&card)
            .unwrap()
            .degraded(7, now - Duration::from_secs(2));
        assert_eq!(
            topology.retry_deadline(true),
            Some(now + Duration::from_secs(1))
        );
        topology
            .observe_discovery(
                Err(io::Error::other("still unavailable")),
                now + Duration::from_secs(1),
            )
            .unwrap_err();
        assert_eq!(
            topology.retry_deadline(true),
            Some(now + Duration::from_secs(3))
        );
        let observed = vec![(card.clone(), None)];
        assert_eq!(
            topology
                .observe_discovery(Ok(observed.clone()), now + Duration::from_secs(3))
                .unwrap(),
            observed
        );
        assert_eq!(
            topology.devices[&card].availability,
            Availability::Degraded(7)
        );
        assert_eq!(
            topology.retry_deadline(true),
            Some(now - Duration::from_secs(1))
        );
    }

    #[test]
    fn retry_work_executes_only_due_devices_and_explicit_changes_override_backoff() {
        let now = Instant::now();
        let mut topology = Topology::<u64>::default();
        let fast = PathBuf::from("/dev/dri/card0");
        let slow = PathBuf::from("/dev/dri/card1");
        let healthy = PathBuf::from("/dev/dri/card2");
        let mut a = DeviceState::default();
        a.failed(now);
        let mut b = DeviceState::default();
        for _ in 0..6 {
            b.degraded(11, now);
        }
        let mut c = DeviceState::default();
        c.ready(12);
        topology
            .devices
            .extend([(fast.clone(), a), (slow.clone(), b), (healthy.clone(), c)]);

        let work = topology.work(ReconcileReason::Retry, now + Duration::from_secs(1));
        assert_eq!(work.paths, vec![fast.clone(), healthy.clone()]);
        assert_eq!(work.deferred, vec![11]);
        assert_eq!(
            topology.devices[&slow].retry.deadline,
            Some(now + Duration::from_secs(32))
        );
        let work = topology.work(ReconcileReason::Retry, now + Duration::from_secs(32));
        assert_eq!(
            work.paths,
            vec![fast.clone(), slow.clone(), healthy.clone()]
        );
        assert!(work.deferred.is_empty());
        for reason in [
            ReconcileReason::Configuration,
            ReconcileReason::Hotplug,
            ReconcileReason::Resume,
        ] {
            let work = topology.work(reason, now);
            assert_eq!(
                work.paths,
                vec![fast.clone(), slow.clone(), healthy.clone()]
            );
            assert!(work.deferred.is_empty());
        }
        // An unopened GPU must also stay out of the actual execution list.
        topology.devices.get_mut(&slow).unwrap().failed(now);
        let work = topology.work(ReconcileReason::Retry, now + Duration::from_secs(2));
        assert_eq!(work.paths, vec![fast, healthy]);
        assert!(work.deferred.is_empty());
    }

    #[test]
    fn partial_recovery_holds_working_displays_until_the_destination_gpu_is_ready() {
        let now = Instant::now();
        let recovering = PathBuf::from("/dev/dri/card0");
        let waiting = PathBuf::from("/dev/dri/card1");
        let healthy = PathBuf::from("/dev/dri/card2");
        let mut topology = Topology::<u64>::default();
        let mut a = DeviceState::default();
        a.degraded(10, now - Duration::from_secs(2));
        let mut b = DeviceState::default();
        b.failed(now);
        let mut c = DeviceState::default();
        c.ready(12);
        topology
            .devices
            .extend([(recovering.clone(), a), (waiting, b), (healthy.clone(), c)]);
        let ready = vec![(recovering.clone(), 10), (healthy.clone(), 12)];
        let mut selected = ready.clone();
        let mut deferred = Vec::new();
        topology.hold_healthy_outputs(ReconcileReason::Retry, now, &mut selected, &mut deferred);
        assert_eq!(selected, vec![(recovering, 10)]);
        assert_eq!(deferred, vec![12]);
        selected = ready.clone();
        deferred.clear();
        topology.hold_healthy_outputs(
            ReconcileReason::Retry,
            now + Duration::from_secs(1),
            &mut selected,
            &mut deferred,
        );
        assert_eq!(selected, ready);
        assert!(deferred.is_empty());
        topology.hold_healthy_outputs(
            ReconcileReason::Configuration,
            now,
            &mut selected,
            &mut deferred,
        );
        assert_eq!(selected, ready);
        assert!(deferred.is_empty());
    }

    #[test]
    fn failed_node_inspection_does_not_prove_device_removal() {
        let card = PathBuf::from("/dev/dri/card0");
        let mut topology = Topology::<u64>::default();
        let mut device = DeviceState::default();
        device.ready(7);
        topology.devices.insert(card.clone(), device);
        assert!(
            topology
                .retire_changed_devices(&[(card, None)], |_| None)
                .is_empty()
        );
        assert_eq!(
            topology.devices.values().next().unwrap().availability,
            Availability::Ready(7)
        );
        assert_eq!(
            topology.retire_changed_devices(&[], |_| None),
            HashSet::from([7])
        );
    }

    #[test]
    fn replacement_gpu_at_the_same_card_path_gets_its_own_slot() {
        let card = PathBuf::from("/dev/dri/card0");
        let a = PathBuf::from("/sys/devices/gpu-a");
        let b = PathBuf::from("/sys/devices/gpu-b");
        let node = 1;
        let mut topology = Topology::<u64>::default();
        topology.observe_devices(&[(card.clone(), Some(a.clone()))]);
        topology.devices.get_mut(&a).unwrap().ready(node);
        // A policy failure keeps the device open, so it must retain ownership
        // of its node until replacement retirement runs.
        topology
            .devices
            .get_mut(&a)
            .unwrap()
            .degraded(node, Instant::now());
        let observed = [(card.clone(), Some(b.clone()))];
        topology.observe_devices(&observed);
        assert_eq!(
            topology.retire_changed_devices(&observed, |_| Some(node)),
            HashSet::from([node])
        );
        assert_eq!(topology.devices[&a].availability, Availability::Missing);
        assert_eq!(topology.devices[&a].current_path(&a, &observed), None);
        assert_eq!(
            topology.devices[&b].current_path(&b, &observed),
            Some(card.as_path())
        );
        topology.devices.get_mut(&b).unwrap().ready(node);
        assert!(
            topology
                .retire_changed_devices(&observed, |_| Some(node))
                .is_empty()
        );
    }

    #[test]
    fn renumbered_and_returning_gpus_keep_independent_slots() {
        let a = PathBuf::from("/sys/devices/gpu-a");
        let b = PathBuf::from("/sys/devices/gpu-b");
        let card0 = PathBuf::from("/dev/dri/card0");
        let card2 = PathBuf::from("/dev/dri/card2");
        let node0 = 1;
        let node2 = 2;
        let mut topology = Topology::<u64>::default();
        topology.observe_devices(&[(card0.clone(), Some(a.clone()))]);
        topology.devices.get_mut(&a).unwrap().ready(node0);
        let observed = [
            (card0.clone(), Some(b.clone())),
            (card2.clone(), Some(a.clone())),
        ];
        for _ in 0..2 {
            topology.observe_devices(&observed);
        }
        assert_eq!(topology.devices.len(), 2);
        assert_eq!(
            topology.devices[&a].current_path(&a, &observed),
            Some(card2.as_path())
        );
        assert_eq!(
            topology.devices[&b].current_path(&b, &observed),
            Some(card0.as_path())
        );
        assert_eq!(
            topology.retire_changed_devices(&observed, |path| Some(if path == card0 {
                node0
            } else {
                node2
            })),
            HashSet::from([node0])
        );
    }

    #[test]
    fn discovering_sysfs_identity_preserves_the_existing_slot() {
        let card = PathBuf::from("/dev/dri/card0");
        let identity = PathBuf::from("/sys/devices/gpu");
        let node = 1;
        let mut topology = Topology::<u64>::default();
        topology.observe_devices(&[(card.clone(), None)]);
        topology.devices.get_mut(&card).unwrap().ready(node);
        topology.observe_devices(&[(card.clone(), Some(identity.clone()))]);
        assert_eq!(topology.devices.len(), 1);
        assert_eq!(
            topology.devices[&identity].availability,
            Availability::Ready(node)
        );
    }

    #[test]
    fn temporary_identity_loss_reconciles_through_the_card_path() {
        let card = PathBuf::from("/dev/dri/card0");
        let identity = PathBuf::from("/sys/devices/gpu");
        let known = [(card.clone(), Some(identity.clone()))];
        let unknown = [(card.clone(), None)];
        let mut topology = Topology::<u64>::default();
        topology.observe_devices(&known);
        topology.devices.get_mut(&identity).unwrap().ready(1);

        topology.observe_devices(&unknown);
        assert_eq!(
            topology.retire_changed_devices(&unknown, |_| Some(1)),
            HashSet::from([1])
        );
        assert_eq!(
            topology.devices[&identity].availability,
            Availability::Missing
        );
        assert_eq!(
            topology.devices[&card].current_path(&card, &unknown),
            Some(card.as_path())
        );
        // Reconciliation reopens the node using its card-path fallback.
        topology.devices.get_mut(&card).unwrap().ready(1);

        topology.observe_devices(&known);
        assert_eq!(topology.devices.len(), 1);
        assert!(!topology.devices.contains_key(&card));
        assert_eq!(
            topology.devices[&identity].current_path(&identity, &known),
            Some(card.as_path())
        );
        // The reopened node is still valid and can be reused for this identity.
        assert!(
            topology
                .retire_changed_devices(&known, |_| Some(1))
                .is_empty()
        );
        topology.devices.get_mut(&identity).unwrap().ready(1);
        assert!(
            topology
                .retire_changed_devices(&known, |_| Some(1))
                .is_empty()
        );
    }

    #[test]
    fn swapping_device_numbers_retires_both_before_reopening_either() {
        let a = PathBuf::from("/sys/devices/gpu-a");
        let b = PathBuf::from("/sys/devices/gpu-b");
        let card0 = PathBuf::from("/dev/dri/card0");
        let card1 = PathBuf::from("/dev/dri/card1");
        let mut topology = Topology::<u64>::default();
        topology.observe_devices(&[
            (card0.clone(), Some(a.clone())),
            (card1.clone(), Some(b.clone())),
        ]);
        topology.devices.get_mut(&a).unwrap().ready(0);
        topology.devices.get_mut(&b).unwrap().ready(1);
        let swapped = [
            (card0.clone(), Some(b.clone())),
            (card1.clone(), Some(a.clone())),
        ];
        let node_for_path = |path: &Path| Some(u64::from(path == card1));
        topology.observe_devices(&swapped);
        assert_eq!(
            topology.retire_changed_devices(&swapped, node_for_path),
            HashSet::from([0, 1])
        );
        assert_eq!(topology.devices[&a].availability, Availability::Missing);
        assert_eq!(topology.devices[&b].availability, Availability::Missing);
        topology.devices.get_mut(&a).unwrap().ready(1);
        topology.devices.get_mut(&b).unwrap().ready(0);
        assert!(
            topology
                .retire_changed_devices(&swapped, node_for_path)
                .is_empty()
        );
    }

    #[test]
    fn inactive_changes_coalesce_and_reconciliation_cannot_reenter() {
        let mut topology = Topology::<u64>::default();
        assert!(!topology.request(false));
        assert!(!topology.request(false));
        assert!(topology.dirty);
        assert!(topology.request(true));
        topology.reconciling = true;
        assert!(!topology.request(true));
    }

    #[test]
    fn replug_matches_hardware_even_when_card_numbers_change() {
        let original = Path::new("/dev/dri/card0");
        let identity = PathBuf::from("/sys/devices/pci0000:00/0000:00:02.0");
        let mut slot = DeviceState::<u64> {
            identity: Some(identity.clone()),
            ..DeviceState::default()
        };
        slot.failed(Instant::now());
        slot.missing();
        let observed = vec![
            (
                PathBuf::from("/dev/dri/card0"),
                Some(PathBuf::from("/sys/devices/other-gpu")),
            ),
            (PathBuf::from("/dev/dri/card2"), Some(identity)),
        ];
        assert_eq!(
            slot.current_path(original, &observed),
            Some(Path::new("/dev/dri/card2"))
        );
        assert!(slot.current_path(original, &observed[..1]).is_none());
        assert!(slot.current_path(original, &[]).is_none());
    }

    #[test]
    fn unavailable_devices_retry_independently_with_capped_backoff() {
        let now = Instant::now();
        let mut topology = Topology::<u64>::default();
        topology
            .devices
            .insert("/dev/dri/card0".into(), DeviceState::default());
        let healthy = 1;
        let mut second = DeviceState::default();
        second.ready(healthy);
        topology.devices.insert("/dev/dri/card1".into(), second);
        let device = topology
            .devices
            .get_mut(Path::new("/dev/dri/card0"))
            .unwrap();
        device.failed(now);
        assert_eq!(device.retry.deadline, Some(now + Duration::from_secs(1)));
        for _ in 0..20 {
            device.failed(now);
        }
        assert_eq!(device.retry.deadline, Some(now + Duration::from_secs(32)));
        assert_eq!(
            topology.devices[Path::new("/dev/dri/card1")].availability,
            Availability::Ready(healthy)
        );
        assert!(topology.retry_deadline(false).is_none());
        assert_eq!(
            topology.retry_deadline(true),
            Some(now + Duration::from_secs(32))
        );
        topology
            .devices
            .get_mut(Path::new("/dev/dri/card0"))
            .unwrap()
            .missing();
        assert!(topology.retry_deadline(true).is_none());
    }
}
