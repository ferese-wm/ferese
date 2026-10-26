//! Capability checks shared by profile validation and real output transactions.
use std::io;
use std::time::Duration;

use smithay::backend::drm::DrmDevice;
use smithay::reexports::drm::control::{Device, ResourceHandle, connector, crtc};
use smithay::reexports::drm::{Device as BasicDevice, DriverCapability};
use smithay::reexports::wayland_protocols::wp::presentation_time::server::wp_presentation_feedback::Kind;
use smithay::wayland::presentation::Refresh;

use super::DirectOutput;
use crate::Ferese;

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

pub(super) fn tearing_capable(drm: &DrmDevice) -> bool {
    // Ferese's initial policy requires primary direct scanout, which Smithay
    // supports only through atomic KMS. Do not expose a legacy-only hint global.
    drm.is_atomic()
        && drm
            .get_driver_capability(DriverCapability::AtomicASyncPageFlip)
            .is_ok_and(|value| value != 0)
}

#[derive(Clone, Copy, Default)]
pub(super) struct TearingConditions {
    pub allowed: bool,
    pub supported: bool,
    pub client_hint: bool,
    pub fullscreen: bool,
    pub animating: bool,
    pub locked: bool,
    pub mirrored: bool,
    pub capturing: bool,
    pub variable_refresh: bool,
}

impl TearingConditions {
    pub fn eligible(self) -> bool {
        self.allowed
            && self.supported
            && self.client_hint
            && self.fullscreen
            && !self.animating
            && !self.locked
            && !self.mirrored
            && !self.capturing
            && !self.variable_refresh
    }
}

pub(super) fn tearing_candidate(state: &Ferese, output: &DirectOutput) -> bool {
    let surface = crate::tearing_control::fullscreen_surface(state, &output.output);
    let mirrored = output.mirror_source.is_some()
        || state.direct_backend.as_ref().is_some_and(|backend| {
            backend.connected_outputs.iter().any(|info| {
                info.enabled && info.mirror_source.as_deref() == Some(output.identity.as_str())
            })
        });
    TearingConditions {
        allowed: output.settings.allow_tearing,
        supported: output.tearing_supported,
        client_hint: surface
            .as_ref()
            .is_some_and(crate::tearing_control::surface_allows_tearing),
        fullscreen: surface.is_some() && !state.overview.is_presenting(),
        animating: state.output_has_animations(&output.output)
            || state.output_has_pending_visual_changes(&output.output),
        locked: state.session_lock.active(),
        mirrored,
        capturing: [false, true]
            .into_iter()
            .any(|cursor| state.has_pending_screencopy(&output.output, cursor)),
        variable_refresh: output.settings.adaptive_sync,
    }
    .eligible()
}

pub(super) fn feedback_flags(tearing: bool) -> Kind {
    if tearing {
        Kind::empty()
    } else {
        Kind::Vsync | Kind::HwClock | Kind::HwCompletion
    }
}

pub(super) fn feedback_refresh(variable: bool, tearing: bool, interval: Duration) -> Refresh {
    if tearing {
        Refresh::Unknown
    } else if variable {
        Refresh::variable(interval)
    } else {
        Refresh::fixed(interval)
    }
}

pub(super) fn valid_timestamp(
    timestamp: Duration,
    now: Duration,
    interval: Duration,
    previous: Option<Duration>,
) -> bool {
    !timestamp.is_zero()
        && timestamp <= now.saturating_add(interval)
        && previous.is_none_or(|previous| timestamp >= previous)
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

    #[test]
    fn every_tearing_requirement_is_independent_and_default_is_synchronized() {
        assert!(!TearingConditions::default().eligible());
        let eligible = TearingConditions {
            allowed: true,
            supported: true,
            client_hint: true,
            fullscreen: true,
            ..TearingConditions::default()
        };
        assert!(eligible.eligible());
        let conditions = [
            TearingConditions {
                allowed: false,
                ..eligible
            },
            TearingConditions {
                supported: false,
                ..eligible
            },
            TearingConditions {
                client_hint: false,
                ..eligible
            },
            TearingConditions {
                fullscreen: false,
                ..eligible
            },
            TearingConditions {
                animating: true,
                ..eligible
            },
            TearingConditions {
                locked: true,
                ..eligible
            },
            TearingConditions {
                mirrored: true,
                ..eligible
            },
            TearingConditions {
                capturing: true,
                ..eligible
            },
            TearingConditions {
                variable_refresh: true,
                ..eligible
            },
        ];
        assert!(
            conditions
                .into_iter()
                .all(|condition| !condition.eligible())
        );
        assert!(feedback_flags(false).contains(Kind::Vsync));
        assert!(
            feedback_flags(true).is_empty(),
            "async/fallback timestamps must not promise physical vblank facts"
        );
        let interval = Duration::from_millis(10);
        assert_eq!(
            feedback_refresh(false, false, interval),
            Refresh::Fixed(interval)
        );
        assert_eq!(
            feedback_refresh(true, false, interval),
            Refresh::Variable(interval)
        );
        assert_eq!(feedback_refresh(false, true, interval), Refresh::Unknown);
    }

    #[test]
    fn feedback_clock_rejects_zero_regression_and_future_clock_domains() {
        let now = Duration::from_secs(100);
        let interval = Duration::from_millis(10);
        assert!(valid_timestamp(now, now, interval, None));
        assert!(valid_timestamp(now + interval, now, interval, Some(now)));
        assert!(!valid_timestamp(Duration::ZERO, now, interval, None));
        assert!(!valid_timestamp(now - interval, now, interval, Some(now)));
        assert!(!valid_timestamp(
            now + interval + Duration::from_nanos(1),
            now,
            interval,
            None
        ));
    }
}
