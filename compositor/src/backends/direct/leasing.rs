//! Lease only non-desktop connectors, with resource and session ownership kept
//! separate from desktop output policy. Protocol state outlives unplugged GPUs
//! so late client destructors remain safe.

mod allocation;

use std::collections::{HashMap, HashSet};
use std::io;

use smithay::backend::drm::{DrmDevice, DrmNode};
use smithay::reexports::drm::control::{Device as ControlDevice, connector, crtc};
use smithay::wayland::drm_lease::{
    DrmLease, DrmLeaseBuilder, DrmLeaseHandler, DrmLeaseRequest, DrmLeaseState, LeaseRejected,
};

use super::{DirectBackendState, DirectDevice};
use crate::Ferese;
use allocation::{PrimaryPlanes, Request};

#[derive(Default)]
pub(super) struct State {
    devices: HashMap<DrmNode, ProtocolDevice>,
    locked: bool,
}

struct ProtocolDevice {
    protocol: DrmLeaseState,
    connectors: HashSet<connector::Handle>,
    fingerprints: HashMap<connector::Handle, u64>,
    retired: bool,
    suspended: bool,
}

/// Missing non-desktop properties have the kernel's normal desktop meaning.
/// Other query errors propagate, rather than silently putting an HMD on the desktop.
pub(super) fn non_desktop(drm: &DrmDevice, connector: &connector::Info) -> io::Result<bool> {
    let properties = drm.get_properties(connector.handle())?;
    for (handle, value) in properties.iter() {
        if drm.get_property(*handle)?.name().to_bytes() == b"non-desktop" {
            return Ok(*value != 0);
        }
    }
    Ok(false)
}

pub(super) fn crtc_is_leased(device: &DirectDevice, crtc: crtc::Handle) -> bool {
    device
        .active_leases
        .iter()
        .any(|lease| lease.crtcs().any(|leased| *leased == crtc))
}

fn routes(
    device: &DirectDevice,
    connectors: &[connector::Info],
) -> io::Result<(Vec<Request>, Vec<PrimaryPlanes>)> {
    let drm = &device.drm;
    let resources = drm.resource_handles()?;
    let mut requests = Vec::with_capacity(connectors.len());
    let mut free_crtcs = Vec::new();
    for connector in connectors {
        let mut compatible = Vec::new();
        for encoder in connector.encoders() {
            let encoder = drm.get_encoder(*encoder)?;
            for crtc in resources.filter_crtcs(encoder.possible_crtcs()) {
                if !device.outputs.contains_key(&crtc)
                    && !crtc_is_leased(device, crtc)
                    && !compatible.contains(&crtc)
                {
                    compatible.push(crtc);
                    if !free_crtcs.contains(&crtc) {
                        free_crtcs.push(crtc);
                    }
                }
            }
        }
        compatible.sort_by_key(|crtc| u32::from(*crtc));
        requests.push(Request {
            connector: connector.handle(),
            crtcs: compatible,
        });
    }
    free_crtcs.sort_by_key(|crtc| u32::from(*crtc));
    let mut primary = Vec::with_capacity(free_crtcs.len());
    for crtc in free_crtcs {
        let planes = drm.planes(&crtc).map_err(io::Error::other)?;
        let planes = planes
            .primary
            .into_iter()
            .filter_map(|plane| {
                // The temporary claim checks the shared plane pool. It is dropped
                // before planning; final claims are acquired together below.
                drm.claim_plane(plane.handle, crtc)
                    .map(|_claim| plane.handle)
            })
            .collect();
        primary.push(PrimaryPlanes { crtc, planes });
    }
    Ok((requests, primary))
}

fn inventory(device: &DirectDevice) -> io::Result<Vec<connector::Info>> {
    let inventory = device
        .inventory
        .as_ref()
        .ok_or_else(|| io::Error::other("connector inventory is unavailable"))?;
    let mut connectors = inventory
        .connectors
        .iter()
        .filter(|connector| {
            connector.info.state() == connector::State::Connected
                && !connector.desktop
                && !device
                    .outputs
                    .values()
                    .any(|output| output.connector == connector.info.handle())
        })
        .map(|connector| connector.info.clone())
        .collect::<Vec<_>>();
    connectors.sort_by_key(|connector| u32::from(connector.handle()));
    Ok(connectors)
}

fn fingerprint(drm: &DrmDevice, connector: &connector::Info) -> u64 {
    super::connector_edid(drm, connector).map_or(0, |edid| super::stable_hash(&edid))
}

/// Retire disconnected or replaced heads before desktop assignment. A headset
/// becoming a desktop connector must release its leased CRTC in this same pass.
pub(super) fn prune(state: &mut Ferese) {
    let Some(backend) = state.direct_backend.as_mut() else {
        return;
    };
    for (node, device) in &mut backend.devices {
        if !device.drm.is_active() {
            continue;
        }
        let Some(entry) = backend
            .leasing
            .devices
            .get_mut(node)
            .filter(|entry| !entry.retired)
        else {
            continue;
        };
        let current = inventory(device)
            .map(|connectors| {
                connectors
                    .into_iter()
                    .map(|connector| (connector.handle(), fingerprint(&device.drm, &connector)))
                    .collect::<HashMap<_, _>>()
            })
            .unwrap_or_default();
        let removed = entry
            .connectors
            .iter()
            .copied()
            .filter(|connector| current.get(connector) != entry.fingerprints.get(connector))
            .collect::<HashSet<_>>();
        device.active_leases.retain(|lease| {
            !lease
                .connectors()
                .any(|connector| removed.contains(connector))
        });
        for connector in removed {
            entry.protocol.withdraw_connector(connector);
            entry.connectors.remove(&connector);
            entry.fingerprints.remove(&connector);
        }
    }
}

/// Called after topology/output transactions, and on lease ownership changes.
/// Kernel enumeration is deliberately absent from the ordinary dispatch path.
pub(super) fn refresh(state: &mut Ferese) {
    prune(state);
    let locked = state.session_lock.active();
    let display = state.display_handle.clone();
    let Some(backend) = state.direct_backend.as_mut() else {
        return;
    };
    backend.leasing.locked = locked;
    if !backend.active || locked {
        suspend(backend);
        return;
    }
    for (node, device) in &mut backend.devices {
        if !device.drm.is_active() {
            if let Some(entry) = backend.leasing.devices.get_mut(node) {
                entry.protocol.suspend();
                entry.suspended = true;
            }
            continue;
        }
        let connectors = match inventory(device) {
            Ok(connectors) => connectors,
            Err(error) => {
                tracing::warn!(%error, ?node, "could not refresh non-desktop connectors");
                device.active_leases.clear();
                if let Some(entry) = backend.leasing.devices.get_mut(node) {
                    entry.protocol.suspend();
                    entry.suspended = true;
                }
                continue;
            }
        };
        let mut eligible = Vec::new();
        let connected = connectors
            .iter()
            .map(connector::Info::handle)
            .collect::<HashSet<_>>();
        // Revoke disconnected multi-head leases before computing which resources
        // have become available to surviving non-desktop connectors.
        device.active_leases.retain(|lease| {
            lease
                .connectors()
                .all(|connector| connected.contains(connector))
        });
        for connector in connectors {
            let leased = device.active_leases.iter().any(|lease| {
                lease
                    .connectors()
                    .any(|leased| *leased == connector.handle())
            });
            let available = leased
                || routes(device, std::slice::from_ref(&connector)).is_ok_and(
                    |(requests, primary)| allocation::allocate(&requests, &primary).is_some(),
                );
            if available {
                eligible.push(connector);
            }
        }
        // Avoid claiming support on nested sessions, inaccessible DRM nodes,
        // or a GPU without any leasable non-desktop connector.
        let replace = backend
            .leasing
            .devices
            .get(node)
            .is_none_or(|entry| entry.retired);
        if replace && !eligible.is_empty() {
            match DrmLeaseState::new_with_filter::<Ferese, _>(
                &display,
                node,
                crate::handlers::security_context::unrestricted_client,
            ) {
                Ok(protocol) => {
                    backend.leasing.devices.insert(
                        *node,
                        ProtocolDevice {
                            protocol,
                            connectors: HashSet::new(),
                            fingerprints: HashMap::new(),
                            retired: false,
                            suspended: false,
                        },
                    );
                }
                Err(error) => {
                    tracing::debug!(%error, ?node, "DRM leasing unavailable");
                    continue;
                }
            }
        }
        let Some(entry) = backend
            .leasing
            .devices
            .get_mut(node)
            .filter(|entry| !entry.retired)
        else {
            continue;
        };
        let desired = eligible
            .iter()
            .map(connector::Info::handle)
            .collect::<HashSet<_>>();
        let removed = entry
            .connectors
            .difference(&desired)
            .copied()
            .collect::<Vec<_>>();
        for connector in removed {
            // Withdrawal also revokes protocol-owned references; release our
            // plane claims before desktop policy may reuse those resources.
            device
                .active_leases
                .retain(|lease| !lease.connectors().any(|leased| *leased == connector));
            entry.protocol.withdraw_connector(connector);
            entry.connectors.remove(&connector);
            entry.fingerprints.remove(&connector);
        }
        if entry.suspended {
            entry.protocol.resume::<Ferese>();
            entry.suspended = false;
        }
        for connector in eligible {
            if entry.connectors.insert(connector.handle()) {
                entry
                    .fingerprints
                    .insert(connector.handle(), fingerprint(&device.drm, &connector));
                let name = connector.to_string();
                entry.protocol.add_connector::<Ferese>(
                    connector.handle(),
                    name.clone(),
                    format!("Non-desktop display {name}"),
                );
            }
        }
    }
}

pub(super) fn suspend(backend: &mut DirectBackendState) {
    for device in backend.devices.values_mut() {
        device.active_leases.clear();
    }
    for entry in backend
        .leasing
        .devices
        .values_mut()
        .filter(|entry| !entry.retired)
    {
        if !entry.suspended {
            entry.protocol.suspend();
            entry.suspended = true;
        }
    }
}

pub(super) fn retire(backend: &mut DirectBackendState, node: DrmNode) {
    if let Some(entry) = backend.leasing.devices.get_mut(&node) {
        entry.protocol.disable_global::<Ferese>();
        for connector in entry.connectors.drain() {
            entry.protocol.withdraw_connector(connector);
        }
        entry.protocol.suspend();
        entry.suspended = true;
        entry.retired = true;
    }
}

pub(crate) fn poll_lock(state: &mut Ferese) {
    let locked = state.session_lock.active();
    if state
        .direct_backend
        .as_ref()
        .is_some_and(|backend| backend.leasing.locked != locked)
    {
        refresh(state);
    }
}

impl DrmLeaseHandler for Ferese {
    fn drm_lease_state(&mut self, node: DrmNode) -> &mut DrmLeaseState {
        &mut self
            .direct_backend
            .as_mut()
            .expect("lease globals exist only on the direct backend")
            .leasing
            .devices
            .get_mut(&node)
            .expect("lease protocol tombstones outlive their client resources")
            .protocol
    }

    fn lease_request(
        &mut self,
        node: DrmNode,
        request: DrmLeaseRequest,
    ) -> Result<DrmLeaseBuilder, LeaseRejected> {
        if self.session_lock.active() {
            return Err(LeaseRejected::default());
        }
        let backend = self
            .direct_backend
            .as_ref()
            .filter(|backend| {
                backend.active && !backend.topology.reconciling && !backend.lid.pending()
            })
            .ok_or_else(LeaseRejected::default)?;
        let entry = backend
            .leasing
            .devices
            .get(&node)
            .filter(|entry| !entry.retired && !entry.suspended)
            .ok_or_else(LeaseRejected::default)?;
        let device = backend
            .devices
            .get(&node)
            .filter(|device| device.drm.is_active())
            .ok_or_else(LeaseRejected::default)?;
        let mut unique = HashSet::new();
        let mut connectors = Vec::new();
        for connector in request.connectors {
            if !unique.insert(connector)
                || !entry.connectors.contains(&connector)
                || device
                    .outputs
                    .values()
                    .any(|output| output.connector == connector)
                || device
                    .active_leases
                    .iter()
                    .any(|lease| lease.connectors().any(|leased| *leased == connector))
            {
                return Err(LeaseRejected::default());
            }
            let info = device
                .drm
                .get_connector(connector, true)
                .map_err(LeaseRejected::with_cause)?;
            if info.state() != connector::State::Connected
                || !non_desktop(&device.drm, &info).map_err(LeaseRejected::with_cause)?
                || entry.fingerprints.get(&connector) != Some(&fingerprint(&device.drm, &info))
            {
                return Err(LeaseRejected::default());
            }
            connectors.push(info);
        }
        let (requests, primary) = routes(device, &connectors).map_err(LeaseRejected::with_cause)?;
        let allocations =
            allocation::allocate(&requests, &primary).ok_or_else(LeaseRejected::default)?;
        let mut builder = DrmLeaseBuilder::new(&device.drm);
        // Hold all primary claims before any optional cursor claim. Failure
        // drops the builder and every provisional claim without issuing a lease.
        for allocation in &allocations {
            let claim = device
                .drm
                .claim_plane(allocation.primary, allocation.crtc)
                .ok_or_else(LeaseRejected::default)?;
            builder.add_connector(allocation.connector);
            builder.add_crtc(allocation.crtc);
            builder.add_plane(allocation.primary, claim);
        }
        for allocation in allocations {
            let planes = device
                .drm
                .planes(&allocation.crtc)
                .map_err(LeaseRejected::with_cause)?;
            if let Some((cursor, claim)) = planes.cursor.into_iter().find_map(|plane| {
                device
                    .drm
                    .claim_plane(plane.handle, allocation.crtc)
                    .map(|claim| (plane.handle, claim))
            }) {
                builder.add_plane(cursor, claim);
            }
        }
        Ok(builder)
    }

    fn new_active_lease(&mut self, node: DrmNode, lease: DrmLease) {
        if let Some(device) = self
            .direct_backend
            .as_mut()
            .and_then(|backend| backend.devices.get_mut(&node))
        {
            device.active_leases.push(lease);
        }
        refresh(self);
    }

    fn lease_destroyed(&mut self, node: DrmNode, lease_id: u32) {
        if let Some(device) = self
            .direct_backend
            .as_mut()
            .and_then(|backend| backend.devices.get_mut(&node))
        {
            device.active_leases.retain(|lease| lease.id() != lease_id);
        }
        refresh(self);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{ClientState, dispatch_test_client, test_client_request};
    use smithay::reexports::calloop::EventLoop;
    use std::io::Read;
    use std::os::unix::net::UnixStream;
    use std::sync::Arc;

    #[test]
    fn nested_sessions_never_advertise_leases_or_accept_forged_requests() {
        if !crate::startup_tests::private_runtime(
            "backends::direct::leasing::tests::nested_sessions_never_advertise_leases_or_accept_forged_requests",
        ) {
            return;
        }
        let mut events = EventLoop::try_new().unwrap();
        let mut state = crate::startup_tests::state(&mut events);
        if let Some(node) = std::fs::read_dir("/dev/dri")
            .ok()
            .into_iter()
            .flatten()
            .filter_map(Result::ok)
            .find_map(|entry| DrmNode::from_path(entry.path()).ok())
        {
            assert!(
                state
                    .lease_request(
                        node,
                        DrmLeaseRequest {
                            connectors: Vec::new()
                        }
                    )
                    .is_err()
            );
        }
        let (server, mut wire) = UnixStream::pair().unwrap();
        state
            .display_handle
            .insert_client(server, Arc::new(ClientState::default()))
            .unwrap();
        test_client_request(&mut wire, 1, 1, &[2], None);
        dispatch_test_client(&mut events, &mut state);
        wire.set_nonblocking(true).unwrap();
        let mut bytes = Vec::new();
        let _ = wire.read_to_end(&mut bytes);
        assert!(
            !bytes
                .windows(b"wp_drm_lease_device_v1".len())
                .any(|window| window == b"wp_drm_lease_device_v1")
        );
    }
}
