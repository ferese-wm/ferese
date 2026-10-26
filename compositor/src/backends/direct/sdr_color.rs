//! Preserve the SDR interpretation of Ferese's output buffers across VT owners.
//! Color metadata is tested and committed atomically, with owned copies of old
//! HDR blobs retained until the accompanying output transaction succeeds.

use std::io;

use smithay::backend::drm::{DrmDevice, DrmDeviceFd};
use smithay::reexports::drm::control::atomic::AtomicModeReq;
use smithay::reexports::drm::control::{
    AtomicCommitFlags, Device as ControlDevice, connector, property,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Assignment {
    connector: connector::Handle,
    property: property::Handle,
    value: u64,
}

struct Observed {
    property: property::Handle,
    current: u64,
    sdr: u64,
    blob: bool,
    writable: bool,
}

trait Hardware: Clone {
    fn properties(&self, connector: connector::Handle) -> io::Result<Vec<Observed>>;
    fn blob(&self, id: u64) -> io::Result<Vec<u8>>;
    fn retain_blob(&self, bytes: &mut [u8]) -> io::Result<u64>;
    fn destroy_blob(&self, id: u64);
    fn commit(&self, assignments: &[Assignment], test: bool) -> io::Result<()>;
}

impl Hardware for DrmDeviceFd {
    fn properties(&self, connector: connector::Handle) -> io::Result<Vec<Observed>> {
        let mut observed = Vec::new();
        for (handle, current) in self.get_properties(connector)?.iter() {
            let info = self.get_property(*handle)?;
            let (sdr, blob) = match info.name().to_bytes() {
                b"HDR_OUTPUT_METADATA" => {
                    if !matches!(info.value_type(), property::ValueType::Blob) {
                        return Err(io::Error::other(
                            "HDR_OUTPUT_METADATA is not a blob property",
                        ));
                    }
                    (0, true)
                }
                b"Colorspace" => {
                    let property::ValueType::Enum(values) = info.value_type() else {
                        return Err(io::Error::other("Colorspace is not an enum property"));
                    };
                    let default = values
                        .values()
                        .1
                        .iter()
                        .find(|value| value.name().to_bytes() == b"Default")
                        .ok_or_else(|| io::Error::other("Colorspace has no Default value"))?;
                    (default.value(), false)
                }
                _ => continue,
            };
            observed.push(Observed {
                property: *handle,
                current: *current,
                sdr,
                blob,
                writable: info.mutable() && info.atomic(),
            });
        }
        Ok(observed)
    }

    fn blob(&self, id: u64) -> io::Result<Vec<u8>> {
        self.get_property_blob(id)
    }
    fn retain_blob(&self, bytes: &mut [u8]) -> io::Result<u64> {
        self.create_property_blob_bytes(bytes)
    }
    fn destroy_blob(&self, id: u64) {
        let _ = self.destroy_property_blob(id);
    }
    fn commit(&self, assignments: &[Assignment], test: bool) -> io::Result<()> {
        let mut request = AtomicModeReq::new();
        append(&mut request, assignments);
        self.atomic_commit(
            if test {
                AtomicCommitFlags::TEST_ONLY | AtomicCommitFlags::ALLOW_MODESET
            } else {
                AtomicCommitFlags::ALLOW_MODESET
            },
            request,
        )
    }
}

fn append(request: &mut AtomicModeReq, assignments: &[Assignment]) {
    for assignment in assignments {
        request.add_raw_property(
            assignment.connector.into(),
            assignment.property,
            assignment.value,
        );
    }
}

pub(super) struct Plan {
    target: Vec<Assignment>,
    previous: Vec<Assignment>,
    blobs: Vec<(usize, Vec<u8>)>,
}

impl Plan {
    pub(super) fn capture(
        drm: &DrmDevice,
        connectors: impl IntoIterator<Item = connector::Handle>,
    ) -> io::Result<Self> {
        if !drm.is_active() {
            return Err(io::Error::other(
                "inactive DRM device cannot change color state",
            ));
        }
        let plan = Self::capture_with(drm.device_fd(), connectors)?;
        if !plan.is_empty() && !drm.is_atomic() {
            return Err(io::Error::other(
                "inherited HDR state requires atomic color reset",
            ));
        }
        Ok(plan)
    }

    fn capture_with(
        hardware: &impl Hardware,
        connectors: impl IntoIterator<Item = connector::Handle>,
    ) -> io::Result<Self> {
        let mut plan = Self {
            target: Vec::new(),
            previous: Vec::new(),
            blobs: Vec::new(),
        };
        let mut seen = std::collections::HashSet::new();
        for connector in connectors {
            if !seen.insert(connector) {
                continue;
            }
            for observed in hardware.properties(connector)? {
                if observed.current == observed.sdr {
                    continue;
                }
                if !observed.writable {
                    return Err(io::Error::other(
                        "inherited color property cannot be reset atomically",
                    ));
                }
                if observed.blob && observed.current != 0 {
                    plan.blobs
                        .push((plan.previous.len(), hardware.blob(observed.current)?));
                }
                plan.previous.push(Assignment {
                    connector,
                    property: observed.property,
                    value: observed.current,
                });
                plan.target.push(Assignment {
                    connector,
                    property: observed.property,
                    value: observed.sdr,
                });
            }
        }
        Ok(plan)
    }

    pub(super) fn is_empty(&self) -> bool {
        self.target.is_empty()
    }
    pub(super) fn extend_test(&self, request: &mut AtomicModeReq) {
        append(request, &self.target);
    }

    pub(super) fn apply(self, drm: &DrmDevice) -> io::Result<Applied> {
        self.apply_with(drm.device_fd().clone(), drm.is_active())
            .map(|state| Applied { state })
    }

    fn apply_with<D: Hardware>(self, hardware: D, active: bool) -> io::Result<AppliedState<D>> {
        if !active {
            return Err(io::Error::other(
                "inactive DRM device cannot change color state",
            ));
        }
        let mut state = AppliedState {
            hardware,
            previous: self.previous,
            blobs: Vec::new(),
        };
        if self.target.is_empty() {
            return Ok(state);
        }
        state.hardware.commit(&self.target, true)?;
        // Clearing HDR metadata may release the kernel's last reference to the
        // old blob. Copy its contents before the first real property commit.
        for (index, mut bytes) in self.blobs {
            let id = state.hardware.retain_blob(&mut bytes)?;
            state.blobs.push(id);
            state.previous[index].value = id;
        }
        state.hardware.commit(&self.target, false)?;
        Ok(state)
    }
}

pub(super) struct Applied {
    state: AppliedState<DrmDeviceFd>,
}
impl Applied {
    pub(super) fn restore(&self, drm: &DrmDevice) -> io::Result<()> {
        self.state.restore(drm.is_active())
    }
}

struct AppliedState<D: Hardware> {
    hardware: D,
    previous: Vec<Assignment>,
    blobs: Vec<u64>,
}
impl<D: Hardware> AppliedState<D> {
    fn restore(&self, active: bool) -> io::Result<()> {
        if self.previous.is_empty() {
            return Ok(());
        }
        if !active {
            return Err(io::Error::other(
                "inactive DRM device cannot restore color state",
            ));
        }
        self.hardware.commit(&self.previous, true)?;
        self.hardware.commit(&self.previous, false)
    }
}
impl<D: Hardware> Drop for AppliedState<D> {
    fn drop(&mut self) {
        for id in &self.blobs {
            self.hardware.destroy_blob(*id);
        }
    }
}

#[cfg(test)]
#[path = "sdr_color_tests.rs"]
mod tests;
