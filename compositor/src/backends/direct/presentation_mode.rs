//! Capability checks shared by profile validation and real output transactions.
use std::io;

use smithay::backend::drm::DrmDevice;
use smithay::reexports::drm::control::{Device, ResourceHandle, connector, crtc};

fn property_value<H: ResourceHandle>(
    drm: &DrmDevice,
    handle: H,
    name: &[u8],
) -> io::Result<Option<u64>> {
    for (property, value) in drm.get_properties(handle)? {
        if drm.get_property(property)?.name().to_bytes() == name {
            return Ok(Some(value));
        }
    }
    Ok(None)
}

pub(super) fn capable(
    drm: &DrmDevice,
    connector: connector::Handle,
    crtc: crtc::Handle,
) -> io::Result<bool> {
    if !drm.is_atomic() {
        return Ok(false);
    }
    Ok(property_value(drm, connector, b"vrr_capable")? == Some(1)
        && property_value(drm, crtc, b"VRR_ENABLED")?.is_some())
}

pub(super) fn connector_capable(drm: &DrmDevice, connector: &connector::Info) -> io::Result<bool> {
    if !drm.is_atomic() || property_value(drm, connector.handle(), b"vrr_capable")? != Some(1) {
        return Ok(false);
    }
    let resources = drm.resource_handles()?;
    for encoder in connector.encoders() {
        let encoder = drm.get_encoder(*encoder)?;
        for crtc in resources.filter_crtcs(encoder.possible_crtcs()) {
            if capable(drm, connector.handle(), crtc)? {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

pub(super) fn validate(
    requested: bool,
    supported: bool,
    mirrored: bool,
) -> Result<(), &'static str> {
    if requested && !supported {
        return Err("adaptive sync is unavailable on this connector/CRTC");
    }
    if requested && mirrored {
        return Err("adaptive sync requires an independent output; disable mirroring first");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn variable_refresh_is_opt_in_requires_capability_and_rejects_mirror_cadences() {
        assert!(validate(false, false, true).is_ok());
        assert!(validate(true, false, false).is_err());
        assert!(validate(true, true, true).is_err());
        assert!(validate(true, true, false).is_ok());
    }
}
