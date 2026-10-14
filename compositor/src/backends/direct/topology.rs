//! Desired device identities survive temporary loss of their DRM resources.
use std::path::PathBuf;

use super::*;

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
    failures: u32,
    retry_at: Option<Instant>,
}

impl<Node> Default for DeviceState<Node> {
    fn default() -> Self {
        Self {
            availability: Availability::Missing,
            identity: None,
            failures: 0,
            retry_at: None,
        }
    }
}

pub(super) fn device_identity(path: &Path) -> Option<PathBuf> {
    // The parent hardware identity survives cardN renumbering after unplug.
    std::fs::canonicalize(Path::new("/sys/class/drm").join(path.file_name()?).join("device")).ok()
}

impl<Node> DeviceState<Node> {
    fn current_path<'a>(&self, original: &Path, observed: &'a [(PathBuf, Option<PathBuf>)]) -> Option<&'a Path> {
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
        self.failures = self.failures.saturating_add(1);
        self.retry_at = Some(now + Duration::from_secs(1u64 << self.failures.saturating_sub(1).min(5)));
    }

    pub fn ready(&mut self, node: Node) {
        self.availability = Availability::Ready(node);
        self.failures = 0;
        self.retry_at = None;
    }

    pub fn missing(&mut self) {
        self.availability = Availability::Missing;
        self.failures = 0;
        self.retry_at = None;
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
    pub fn observe_devices(&mut self, observed: &[(PathBuf, Option<PathBuf>)]) {
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
        observed: &[(PathBuf, Option<PathBuf>)],
        node_for_path: impl Fn(&Path) -> Option<Node>,
    ) -> HashSet<Node> {
        let mut retired = HashSet::new();
        for (key, slot) in &mut self.devices {
            if let Availability::Ready(previous) | Availability::Degraded(previous) = slot.availability
                && slot.current_path(key, observed).and_then(&node_for_path) != Some(previous)
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
            .then(|| self.devices.values().filter_map(|device| device.retry_at).min())
            .flatten()
    }
}

fn slot_mut<'a>(state: &'a mut Ferese, path: &Path) -> &'a mut DeviceState {
    state
        .direct_backend
        .as_mut()
        .unwrap()
        .topology
        .devices
        .get_mut(path)
        .unwrap()
}

/// The only entry point for applying the current output configuration to DRM.
pub(super) fn reconcile_outputs(state: &mut Ferese, reactivate: bool) {
    let Some(backend) = state.direct_backend.as_mut() else {
        return;
    };
    if !backend.topology.request(backend.active && !backend.lid.pending()) {
        return;
    }

    if let Some(token) = backend.topology.settle_timer.take() {
        state.loop_handle.remove(token);
    }
    backend.topology.coalescer.clear();
    backend.topology.reconciling = true;
    backend.topology.dirty = false;
    backend.output_error = None;
    if reactivate {
        backend
            .presentation
            .values_mut()
            .for_each(PresentationClock::reset_timing);
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
    backend.low_power =
        automatic && super::super::power::low_power(backend.low_power, super::super::power::battery_percent());

    if let Ok(observed) = &observed {
        backend.topology.observe_devices(observed);
        // Retire every obsolete node before opening any replacement. Otherwise
        // a later missing slot could remove a new GPU that reused its dev_t.
        invalidated.extend(
            backend
                .topology
                .retire_changed_devices(observed, |path| DrmNode::from_path(path).ok()),
        );
    }
    let mut paths = backend.topology.devices.keys().cloned().collect::<Vec<_>>();
    paths.sort();
    let mut ready = Vec::new();
    state.begin_desktop_transition();

    for node in invalidated {
        remove_device(state, node);
    }

    for path in paths {
        let slot = &state.direct_backend.as_ref().unwrap().topology.devices[&path];
        let current_path = observed
            .as_ref()
            .ok()
            .and_then(|devices| slot.current_path(&path, devices));
        let existing = current_path
            .and_then(|path| DrmNode::from_path(path).ok())
            .filter(|node| state.direct_backend.as_ref().unwrap().devices.contains_key(node));
        let previous = match slot.availability {
            Availability::Ready(node) | Availability::Degraded(node) => Some(node),
            _ => existing,
        };
        let found = observed.as_ref().map(|_| current_path.is_some());
        if !matches!(found, Ok(true)) {
            if let Some(node) = previous {
                remove_device(state, node);
            }
            let entry = slot_mut(state, &path);
            match found {
                Ok(false) => entry.missing(),
                Err(error) => {
                    tracing::warn!(%error, ?path, "could not enumerate DRM topology");
                    entry.failed(Instant::now());
                }
                Ok(true) => unreachable!(),
            }
            continue;
        }

        if let Some(node) = previous.filter(|node| Some(*node) != existing) {
            remove_device(state, node);
        }
        let node = match existing {
            Some(node) => node,
            None => match open_device(state, current_path.unwrap()) {
                Ok(node) => node,
                Err(error) => {
                    tracing::warn!(%error, ?path, "DRM device unavailable; will retry");
                    slot_mut(state, &path).failed(Instant::now());
                    continue;
                }
            },
        };

        let mut failed_outputs = Vec::new();
        let activation = if reactivate && existing.is_some() {
            let device = state.direct_backend.as_mut().unwrap().devices.get_mut(&node).unwrap();
            device.drm.activate(true).map(|()| {
                for (crtc, output) in &mut device.outputs {
                    cancel_output_timers(&state.loop_handle, output);
                    output.surface.reset_buffers();
                    output.dmabuf_feedback.invalidate();
                    output.primary_commit = None;
                    output.capture_texture = None;
                    output.frame_pending = false;
                    output.lock_frame_pending = false;
                    output.power.suspended();
                    state.session_lock.output_added(&output.output);
                    state.display_presentation.remove_output(&output.output);
                    state
                        .desktop_transition
                        .as_mut()
                        .unwrap()
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
                .unwrap()
                .devices
                .get_mut(&node)
                .unwrap()
                .outputs
                .remove(&crtc)
                .unwrap();
            cancel_output_timers(&state.loop_handle, &mut output);
            if let Some(global) = output.global.take() {
                state.display_handle.disable_global::<Ferese>(global);
            }
            state.unregister_output(&output.output);
            state
                .direct_backend
                .as_mut()
                .unwrap()
                .presentation
                .remove(&(node, crtc));
        }

        ready.push((path, node));
    }

    let known_good = state.direct_backend.as_ref().unwrap().applied_outputs.clone();
    if let Err(error) = refresh_monitor_policy(state).and_then(|()| {
        transaction::retire_disconnected(state);
        validate_desired_outputs(state)
    }) {
        tracing::warn!(%error, "monitor policy validation failed; retaining applied outputs");
        state.direct_backend.as_mut().unwrap().output_error = Some(error);
        for (path, node) in &ready {
            slot_mut(state, path).degraded(*node, Instant::now());
        }
    } else {
        // Activate destinations before disabling former owners, including across GPUs.
        ready.sort_by_key(|(_, node)| {
            let backend = state.direct_backend.as_ref().unwrap();
            let prefix = format!("{}/", drm_device_key(&backend.devices[node].drm));
            let desired = backend
                .desired_outputs
                .outputs
                .iter()
                .filter(|output| output.settings.enabled && output.key.starts_with(&prefix));
            let logical_sources = desired.clone().filter(|output| output.mirror_source.is_none()).count();
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
                    let backend = state.direct_backend.as_mut().unwrap();
                    let requested = backend.desired_outputs.clone();
                    backend.desired_outputs =
                        crate::output_policy::restore_configuration(&known_good, &backend.monitors);
                    for previous in succeeded {
                        let _ = rescan_device(state, previous);
                    }
                    state.direct_backend.as_mut().unwrap().desired_outputs = requested;
                }
                break;
            }
        }
        if state.direct_backend.as_ref().unwrap().output_error.is_none() {
            remember_applied_configuration(state);
        }
    }

    transaction::recover_last_output(state);
    let mut desktop_changes = publish_runtime_desktop(state);
    if std::mem::take(&mut state.direct_backend.as_mut().unwrap().topology.layout_dirty) {
        desktop_changes.layout = true;
        desktop_changes.shell = true;
        desktop_changes.outputs.extend(state.space.outputs().cloned());
    }
    let backend = state.direct_backend.as_mut().unwrap();
    backend.topology.reconciling = false;
    let retry = backend.topology.retry_deadline(backend.active);
    if std::mem::take(&mut backend.topology.remember_applied) {
        remember_applied_configuration(state);
    }
    refresh_connected_info(state);
    if let Some(deadline) = retry {
        match state
            .loop_handle
            .insert_source(Timer::from_deadline(deadline), |_, _, state| {
                if let Some(backend) = state.direct_backend.as_mut() {
                    backend.topology.retry_timer = None;
                }
                reconcile_outputs(state, false);
                TimeoutAction::Drop
            }) {
            Ok(token) => state.direct_backend.as_mut().unwrap().topology.retry_timer = Some(token),
            Err(error) => tracing::warn!(%error, "could not arm DRM recovery deadline"),
        }
    }

    sync_battery_timer(state);
    state.finish_desktop_transition(desktop_changes);
    state.notify_monitor_state();
    lid::apply_suspend_policy(state);
}

/// Publish only hardware present after application, compensation and recovery.
fn publish_runtime_desktop(state: &mut Ferese) -> crate::state::DesktopChanges {
    let mut outputs = state
        .direct_backend
        .as_ref()
        .unwrap()
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
            let width = (transform.transform_size(mode.size).w as f64 / settings.scale).ceil() as i32;
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
    for device in state.direct_backend.as_mut().unwrap().devices.values_mut() {
        for output in device.outputs.values_mut() {
            if output.mirror_source.is_none() {
                if output.global.is_none() {
                    output.global = Some(output.output.create_global::<Ferese>(&state.display_handle));
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
            output.surface.set_output_mode_source((&output.output).into());
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
            reconcile_outputs(state, false);
            TimeoutAction::Drop
        }) {
        Ok(token) => state.direct_backend.as_mut().unwrap().topology.settle_timer = Some(token),
        Err(error) => {
            tracing::warn!(%error, "could not debounce output events");
            reconcile_outputs(state, false);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        topology.devices.get_mut(&a).unwrap().degraded(node, Instant::now());
        let observed = [(card.clone(), Some(b.clone()))];
        topology.observe_devices(&observed);
        assert_eq!(
            topology.retire_changed_devices(&observed, |_| Some(node)),
            HashSet::from([node])
        );
        assert_eq!(topology.devices[&a].availability, Availability::Missing);
        assert_eq!(topology.devices[&a].current_path(&a, &observed), None);
        assert_eq!(topology.devices[&b].current_path(&b, &observed), Some(card.as_path()));
        topology.devices.get_mut(&b).unwrap().ready(node);
        assert!(topology.retire_changed_devices(&observed, |_| Some(node)).is_empty());
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
        let observed = [(card0.clone(), Some(b.clone())), (card2.clone(), Some(a.clone()))];
        for _ in 0..2 {
            topology.observe_devices(&observed);
        }
        assert_eq!(topology.devices.len(), 2);
        assert_eq!(topology.devices[&a].current_path(&a, &observed), Some(card2.as_path()));
        assert_eq!(topology.devices[&b].current_path(&b, &observed), Some(card0.as_path()));
        assert_eq!(
            topology.retire_changed_devices(&observed, |path| Some(if path == card0 { node0 } else { node2 })),
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
        assert_eq!(topology.devices[&identity].availability, Availability::Ready(node));
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
        assert_eq!(topology.devices[&identity].availability, Availability::Missing);
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
        assert!(topology.retire_changed_devices(&known, |_| Some(1)).is_empty());
        topology.devices.get_mut(&identity).unwrap().ready(1);
        assert!(topology.retire_changed_devices(&known, |_| Some(1)).is_empty());
    }

    #[test]
    fn swapping_device_numbers_retires_both_before_reopening_either() {
        let a = PathBuf::from("/sys/devices/gpu-a");
        let b = PathBuf::from("/sys/devices/gpu-b");
        let card0 = PathBuf::from("/dev/dri/card0");
        let card1 = PathBuf::from("/dev/dri/card1");
        let mut topology = Topology::<u64>::default();
        topology.observe_devices(&[(card0.clone(), Some(a.clone())), (card1.clone(), Some(b.clone()))]);
        topology.devices.get_mut(&a).unwrap().ready(0);
        topology.devices.get_mut(&b).unwrap().ready(1);
        let swapped = [(card0.clone(), Some(b.clone())), (card1.clone(), Some(a.clone()))];
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
        assert!(topology.retire_changed_devices(&swapped, node_for_path).is_empty());
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
        topology.devices.insert("/dev/dri/card0".into(), DeviceState::default());
        let healthy = 1;
        let mut second = DeviceState::default();
        second.ready(healthy);
        topology.devices.insert("/dev/dri/card1".into(), second);
        let device = topology.devices.get_mut(Path::new("/dev/dri/card0")).unwrap();
        device.failed(now);
        assert_eq!(device.retry_at, Some(now + Duration::from_secs(1)));
        for _ in 0..20 {
            device.failed(now);
        }
        assert_eq!(device.retry_at, Some(now + Duration::from_secs(32)));
        assert_eq!(
            topology.devices[Path::new("/dev/dri/card1")].availability,
            Availability::Ready(healthy)
        );
        assert!(topology.retry_deadline(false).is_none());
        assert_eq!(topology.retry_deadline(true), Some(now + Duration::from_secs(32)));
        topology.devices.get_mut(Path::new("/dev/dri/card0")).unwrap().missing();
        assert!(topology.retry_deadline(true).is_none());
    }
}
