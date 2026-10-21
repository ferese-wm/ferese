use smithay::reexports::calloop::timer::{TimeoutAction, Timer};
use std::io;
use std::time::{Duration, Instant};

use smithay::backend::drm::{DrmDevice, DrmDeviceFd};
use smithay::output::Output;
use smithay::reexports::drm::control::{Device as _, crtc};
use smithay::reexports::wayland_protocols_wlr::gamma_control::v1::server::zwlr_gamma_control_v1::ZwlrGammaControlV1;

use crate::Ferese;
use crate::gamma::Ramp;
use ferese_config::night_light::Settings;
use ferese_ipc::night_light::Status;

const TRANSITION_DURATION: Duration = Duration::from_secs(1);
const TRANSITION_INTERVAL: Duration = Duration::from_millis(16);

struct Transition {
    from: Ramp,
    started: Instant,
    duration: Duration,
}

// A single owner determines both the desired ramp and who loses control on failure.
enum Source {
    Original,
    Client {
        control: ZwlrGammaControlV1,
        ramp: Ramp,
    },
    NightLight {
        temperature: u16,
        ramp: Ramp,
    },
}

#[derive(Default)]
pub(super) enum State {
    #[default]
    Unprobed,
    Unsupported,
    Ready(Box<Gamma>),
}

impl State {
    fn probe(&mut self, drm: &DrmDevice, crtc: crtc::Handle) -> Option<&mut Gamma> {
        if matches!(self, Self::Unprobed) {
            *self = match Gamma::new(drm, crtc) {
                Ok(gamma) => Self::Ready(Box::new(gamma)),
                Err(error) => {
                    tracing::debug!(%error, ?crtc, "output gamma unavailable");
                    Self::Unsupported
                }
            };
        }
        self.controller_mut()
    }

    fn controller(&self) -> Option<&Gamma> {
        match self {
            Self::Ready(gamma) => Some(gamma),
            _ => None,
        }
    }

    fn controller_mut(&mut self) -> Option<&mut Gamma> {
        match self {
            Self::Ready(gamma) => Some(gamma),
            _ => None,
        }
    }

    pub(super) fn suspend(&mut self) {
        if let Some(gamma) = self.controller_mut() {
            gamma.suspend();
        }
    }

    pub(super) fn retry_probe(&mut self) {
        if matches!(self, Self::Unsupported) {
            *self = Self::Unprobed;
        }
    }
}

pub(super) struct Gamma {
    fd: DrmDeviceFd,
    crtc: crtc::Handle,
    original: Ramp,
    current: Ramp,
    transition: Option<Transition>,
    source: Source,
    suspended: bool,
    modified: bool,
    failed: bool,
}

impl Gamma {
    fn new(drm: &DrmDevice, crtc: crtc::Handle) -> io::Result<Self> {
        let mut original = Ramp::zeroed(drm.get_crtc(crtc)?.gamma_length() as usize)?;
        let (red, green, blue) = original.channels_mut();
        drm.get_gamma(crtc, red, green, blue)?;
        Ok(Self {
            fd: drm.device_fd().clone(),
            crtc,
            current: original.clone(),
            transition: None,
            original,
            source: Source::Original,
            suspended: false,
            modified: false,
            failed: false,
        })
    }

    fn write(&self, ramp: &Ramp) -> io::Result<()> {
        if self.fd.get_crtc(self.crtc)?.gamma_length() as usize != ramp.size() {
            return Err(io::ErrorKind::InvalidData.into());
        }
        let (red, green, blue) = ramp.channels();
        self.fd.set_gamma(self.crtc, red, green, blue)
    }

    fn restore(&mut self) {
        if self.suspended || !self.modified {
            return;
        }
        match self.write(&self.original) {
            Ok(()) => {
                self.modified = false;
                self.current = self.original.clone();
            }
            Err(error) => tracing::warn!(%error, ?self.crtc, "could not restore output gamma"),
        }
    }

    fn replace_source(&mut self, source: Source) {
        if let Source::Client { control, .. } = std::mem::replace(&mut self.source, source) {
            control.failed();
        }
    }

    fn configure(&mut self, settings: Settings, duration: Duration, now: Instant) {
        if settings.enabled {
            if matches!(self.source, Source::NightLight { temperature, .. } if temperature == settings.temperature)
            {
                return;
            }
            self.replace_source(Source::NightLight {
                temperature: settings.temperature,
                ramp: self.original.at_temperature(settings.temperature),
            });
        } else if matches!(self.source, Source::NightLight { .. }) {
            self.source = Source::Original;
        } else {
            return;
        }
        let target = match &self.source {
            Source::Original => &self.original,
            Source::NightLight { ramp, .. } | Source::Client { ramp, .. } => ramp,
        };
        self.transition =
            (!duration.is_zero() && !self.suspended && self.current != *target).then(|| {
                Transition {
                    from: self.current.clone(),
                    started: now,
                    duration,
                }
            });
    }

    fn owns(&self, owner: &ZwlrGammaControlV1) -> bool {
        matches!(&self.source, Source::Client { control, .. } if control == owner)
    }

    pub(super) fn suspend(&mut self) {
        self.transition = None;
        self.restore();
        self.suspended = true;
    }

    fn resume(&mut self, now: Instant) {
        self.suspended = false;
        let ramp = match &self.source {
            Source::Original if !self.modified && self.transition.is_none() => {
                self.failed = false;
                return;
            }
            Source::Original => &self.original,
            Source::Client { ramp, .. } | Source::NightLight { ramp, .. } => ramp,
        };
        let animated;
        let ramp = if let Some(transition) = &self.transition {
            let progress = now
                .saturating_duration_since(transition.started)
                .as_secs_f64()
                / transition.duration.as_secs_f64();
            if progress >= 1. {
                self.transition = None;
                ramp
            } else {
                let eased = progress * progress * (3. - 2. * progress);
                animated = transition.from.interpolate(ramp, eased);
                &animated
            }
        } else {
            ramp
        };
        // Even a failed driver call may need the original table restored.
        self.modified |= ramp != &self.original;
        match self.write(ramp) {
            Ok(()) => {
                self.current = ramp.clone();
                self.modified = self.current != self.original;
                self.failed = false;
            }
            Err(error) => {
                self.transition = None;
                self.failed = true;
                tracing::warn!(%error, ?self.crtc, "could not apply output gamma");
                if matches!(self.source, Source::Client { .. }) {
                    self.replace_source(Source::Original);
                }
                self.restore();
            }
        }
    }
}

impl Drop for Gamma {
    fn drop(&mut self) {
        self.replace_source(Source::Original);
        self.restore();
    }
}

pub(crate) fn acquire(
    state: &mut Ferese,
    target: &Output,
    owner: &ZwlrGammaControlV1,
) -> Option<usize> {
    // Reconcile a just-reloaded configuration before assigning ownership.
    sync_night_light(state);
    if state.night_light.enabled {
        return None;
    }
    let duration = state.animation_duration(TRANSITION_DURATION);
    let backend = state
        .direct_backend
        .as_mut()
        .filter(|backend| backend.active)?;
    for device in backend
        .devices
        .values_mut()
        .filter(|device| device.drm.is_active())
    {
        for (crtc, output) in &mut device.outputs {
            if &output.output != target || output.global.is_none() {
                continue;
            }
            let gamma = output.gamma.probe(&device.drm, *crtc)?;
            gamma.configure(state.night_light, duration, Instant::now());
            if !matches!(gamma.source, Source::Original) {
                return None;
            }
            gamma.transition = None;
            gamma.source = Source::Client {
                control: owner.clone(),
                ramp: gamma.original.clone(),
            };
            return Some(gamma.original.size());
        }
    }
    None
}

pub(crate) fn sync_night_light(state: &mut Ferese) {
    if !state.gamma_dirty {
        return;
    }
    if !state
        .direct_backend
        .as_ref()
        .is_some_and(|backend| backend.active)
    {
        return;
    }
    state.gamma_dirty = false;
    if advance_night_light(state, true) && !state.gamma_timer_pending {
        let result = state.loop_handle.insert_source(
            Timer::from_duration(TRANSITION_INTERVAL),
            |_, _, state| {
                if advance_night_light(state, false) {
                    TimeoutAction::ToDuration(TRANSITION_INTERVAL)
                } else {
                    state.gamma_timer_pending = false;
                    TimeoutAction::Drop
                }
            },
        );
        match result {
            Ok(_) => state.gamma_timer_pending = true,
            Err(error) => {
                tracing::warn!(%error, "could not schedule night-light transition");
                if let Some(backend) = &mut state.direct_backend {
                    for device in backend
                        .devices
                        .values_mut()
                        .filter(|device| device.drm.is_active())
                    {
                        for output in device
                            .outputs
                            .values_mut()
                            .filter(|output| !output.power.needs_wake())
                        {
                            if let Some(gamma) = output.gamma.controller_mut() {
                                gamma.transition = None;
                                gamma.resume(Instant::now());
                            }
                        }
                    }
                }
            }
        }
    }
}

fn advance_night_light(state: &mut Ferese, force: bool) -> bool {
    let duration = state.animation_duration(TRANSITION_DURATION);
    let now = Instant::now();
    let Some(backend) = state
        .direct_backend
        .as_mut()
        .filter(|backend| backend.active)
    else {
        return false;
    };
    let mut pending = false;
    for device in backend
        .devices
        .values_mut()
        .filter(|device| device.drm.is_active())
    {
        for (crtc, output) in &mut device.outputs {
            let ready = !output.power.needs_wake();
            let gamma = if ready {
                output.gamma.probe(&device.drm, *crtc)
            } else {
                output.gamma.controller_mut()
            };
            if let Some(gamma) = gamma {
                gamma.configure(state.night_light, duration, now);
                let update = force || gamma.transition.is_some();
                if duration.is_zero() {
                    gamma.transition = None;
                }
                if ready && update {
                    gamma.resume(now);
                    pending |= gamma.transition.is_some();
                }
            }
        }
    }
    pending
}

pub(crate) fn night_light_status(state: &Ferese) -> Status {
    let mut status = Status {
        enabled: state.night_light.enabled,
        temperature: state.night_light.temperature,
        ..Default::default()
    };
    if let Some(backend) = &state.direct_backend {
        for device in backend.devices.values() {
            for output in device.outputs.values() {
                if let Some(gamma) = output.gamma.controller() {
                    status.supported_outputs += 1;
                    status.failed_outputs += usize::from(gamma.failed);
                    status.active_outputs += usize::from(
                        backend.active
                            && !output.power.needs_wake()
                            && matches!(gamma.source, Source::NightLight { .. })
                            && !gamma.failed
                            && !gamma.suspended,
                    );
                }
            }
        }
    }
    status
}

fn owned<'a>(
    state: &'a mut Ferese,
    target: &Output,
    owner: &ZwlrGammaControlV1,
) -> Option<(&'a mut Gamma, bool)> {
    let duration = state.animation_duration(TRANSITION_DURATION);
    let backend = state.direct_backend.as_mut()?;
    for device in backend.devices.values_mut() {
        for output in device.outputs.values_mut() {
            if &output.output == target {
                let ready = backend.active && device.drm.is_active() && !output.power.needs_wake();
                let gamma = output.gamma.controller_mut()?;
                gamma.configure(state.night_light, duration, Instant::now());
                return gamma.owns(owner).then_some((gamma, ready));
            }
        }
    }
    None
}

pub(crate) fn size(
    state: &mut Ferese,
    target: &Output,
    owner: &ZwlrGammaControlV1,
) -> Option<usize> {
    owned(state, target, owner).map(|(gamma, _)| gamma.original.size())
}

pub(crate) fn set(state: &mut Ferese, target: &Output, owner: &ZwlrGammaControlV1, ramp: Ramp) {
    if let Some((gamma, ready)) = owned(state, target, owner) {
        gamma.transition = None;
        gamma.source = Source::Client {
            control: owner.clone(),
            ramp,
        };
        if ready {
            gamma.resume(Instant::now());
        }
    }
}

pub(crate) fn release(state: &mut Ferese, target: &Output, owner: &ZwlrGammaControlV1) {
    if let Some((gamma, ready)) = owned(state, target, owner) {
        gamma.transition = None;
        gamma.source = Source::Original;
        if ready {
            gamma.resume(Instant::now());
        }
    }
}
