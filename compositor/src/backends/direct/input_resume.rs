//! Re-enumerate input after system sleep, even when libseat stayed active.
use std::cell::RefCell;
use std::collections::HashMap;
use std::os::fd::OwnedFd;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Instant;

use smithay::reexports::input::event::EventTrait;
use smithay::reexports::input::{Event, LibinputInterface, event};

use super::{
    Duration, Ferese, InputEvent, Libinput, LibinputInputBackend, TimeoutAction, Timer,
    process_libinput_event,
};

const RETRY_DELAYS: [Duration; 3] = [
    Duration::from_secs(1),
    Duration::from_secs(2),
    Duration::from_secs(4),
];
const BUSY_DELAY: Duration = Duration::from_millis(100);
const RETRY_LIFETIME: Duration = Duration::from_secs(10);

#[derive(Clone, Default)]
pub(super) struct OpenFailures(Rc<RefCell<HashMap<PathBuf, i32>>>);

impl OpenFailures {
    fn has_present_devices(&self) -> bool {
        let mut failed = self.0.borrow_mut();
        failed.retain(|path, _| path.try_exists().unwrap_or(true));
        !failed.is_empty()
    }
}

// libinput can return success after individual opens fail. Observe the actual
// session interface instead of inferring recovery from resume's return value.
pub(super) struct Access<I> {
    pub(super) inner: I,
    pub(super) failures: OpenFailures,
}

impl<I: LibinputInterface> LibinputInterface for Access<I> {
    fn open_restricted(&mut self, path: &Path, flags: i32) -> Result<OwnedFd, i32> {
        let result = self.inner.open_restricted(path, flags);
        let mut failures = self.failures.0.borrow_mut();
        match &result {
            Ok(_) => {
                failures.remove(path);
            }
            Err(error) => {
                failures.insert(path.to_owned(), *error);
            }
        }
        result
    }

    fn close_restricted(&mut self, fd: OwnedFd) {
        self.inner.close_restricted(fd);
    }
}

#[derive(Clone, Copy, Debug)]
pub(super) struct SleepOffset {
    lower: u64,
    upper: u64,
}

impl SleepOffset {
    pub(super) fn sample() -> Option<Self> {
        let before = super::monotonic_now().as_millis() as u64;
        let boot = ferese_ipc::activities::now_ms();
        let after = super::monotonic_now().as_millis() as u64;
        // Bracket the boot clock read so scheduling delays do not look like
        // suspend time. Millisecond rounding is covered by the tolerance below.
        (boot != 0).then_some(Self {
            lower: boot.saturating_sub(after),
            upper: boot.saturating_sub(before),
        })
    }

    fn advanced_to(self, current: Self) -> bool {
        current.lower > self.upper.saturating_add(5)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Failure {
    Monitor,
    Devices,
}

struct Retries {
    failure: Failure,
    attempts: usize,
    deadline: Instant,
}

impl Retries {
    fn new(failure: Failure, now: Instant) -> Self {
        Self {
            failure,
            attempts: 0,
            deadline: now + RETRY_LIFETIME,
        }
    }

    fn can_wait(&self, now: Instant) -> bool {
        self.failure == Failure::Monitor || now < self.deadline
    }

    fn failed(&mut self, failure: Failure, now: Instant) -> Option<Duration> {
        let previous = self.failure;
        self.failure = failure;
        if failure == Failure::Monitor {
            // No usable monitor remains to recover through. Keep trying at a
            // capped rate until monitoring returns or the seat pauses.
            return Some(RETRY_DELAYS[2]);
        }
        if previous == Failure::Monitor {
            self.attempts = 0;
            self.deadline = now + RETRY_LIFETIME;
        }
        self.attempts += 1;
        RETRY_DELAYS.get(self.attempts).copied()
    }
}

fn drain(
    context: &mut Libinput,
    switches: bool,
    dispatch: &mut impl FnMut(InputEvent<LibinputInputBackend>),
) {
    // suspend/resume queue lifecycle events without necessarily making the
    // poll fd readable. Consume them now so old handles are retired and new
    // devices are configured before the next hardware event.
    for event in context {
        let event = match event {
            Event::Device(event::DeviceEvent::Added(event)) => InputEvent::DeviceAdded {
                device: event.device(),
            },
            Event::Device(event::DeviceEvent::Removed(event)) => InputEvent::DeviceRemoved {
                device: event.device(),
            },
            Event::Switch(event::SwitchEvent::Toggle(event)) if switches => {
                InputEvent::SwitchToggle { event }
            }
            // Input queued before suspension must not become fresh presses or
            // gestures after wake. Only newly read switch state is forwarded.
            _ => continue,
        };
        dispatch(event);
    }
}

fn resume_context(
    context: &mut Libinput,
    failures: &OpenFailures,
    dispatch: &mut impl FnMut(InputEvent<LibinputInputBackend>),
) -> Result<(), Failure> {
    drain(context, false, dispatch);
    failures.0.borrow_mut().clear();
    let result = context.resume();
    drain(context, true, dispatch);
    result.map_err(|()| Failure::Monitor)?;
    if failures.has_present_devices() {
        Err(Failure::Devices)
    } else {
        Ok(())
    }
}

fn refresh_context(
    context: &mut Libinput,
    failures: &OpenFailures,
    dispatch: &mut impl FnMut(InputEvent<LibinputInputBackend>),
) -> Result<(), Failure> {
    // resume() alone is a no-op on an active udev context. Reopening the seat
    // discards stale external-mouse/lid state and enumerates current hardware.
    context.suspend();
    resume_context(context, failures, dispatch)
}

fn attempt(state: &mut Ferese, refresh: bool) -> Result<(), Failure> {
    let Some(backend) = state
        .direct_backend
        .as_ref()
        .filter(|backend| backend.active)
    else {
        return Ok(());
    };
    let mut context = backend.input_context.clone();
    let failures = backend.input_open_failures.clone();
    // This records a sleep already observed, not device-opening success. Once
    // this attempt starts, the retry owns incomplete recovery; a logind restart
    // must not bypass its held-input guard. A later sleep advances the offset.
    if let Some(backend) = state.direct_backend.as_mut() {
        backend.input_sleep_offset = SleepOffset::sample();
    }
    if refresh {
        state.reset_session_input();
    }
    let mut dispatch = |event| process_libinput_event(state, event);
    if refresh {
        refresh_context(&mut context, &failures, &mut dispatch)
    } else {
        resume_context(&mut context, &failures, &mut dispatch)
    }
}

fn retry(state: &mut Ferese, failure: Failure) {
    let Some(backend) = state.direct_backend.as_mut() else {
        return;
    };
    if backend.input_retry.is_some() {
        return;
    }
    tracing::warn!(
        ?failure,
        "input recovery incomplete; retrying device discovery"
    );
    let mut retries = Retries::new(failure, Instant::now());
    match state
        .loop_handle
        .insert_source(Timer::from_duration(RETRY_DELAYS[0]), move |_, _, state| {
            let active = state.direct_backend.as_ref().is_some_and(|backend| backend.active);
            if !active {
                clear_retry_marker(state);
                return TimeoutAction::Drop;
            }
            if retries.failure == Failure::Devices
                && state
                    .direct_backend
                    .as_ref()
                    .is_some_and(|backend| !backend.input_open_failures.has_present_devices())
            {
                clear_retry_marker(state);
                return TimeoutAction::Drop;
            }
            if !state.physical_input.is_neutral() {
                if retries.can_wait(Instant::now()) {
                    return TimeoutAction::ToDuration(BUSY_DELAY);
                }
                tracing::warn!("input recovery deferred while input remained held; stopping retries");
                clear_retry_marker(state);
                return TimeoutAction::Drop;
            }
            // A failed open does not suspend an otherwise active monitor, and
            // a failed resume can leave one partially initialized. Always
            // suspend before retrying rather than calling resume() again.
            let result = attempt(state, true);
            if let Err(failure) = result {
                if let Some(delay) = retries.failed(failure, Instant::now()) {
                    return TimeoutAction::ToDuration(delay);
                }
                if let Some(backend) = state.direct_backend.as_ref() {
                    tracing::error!(?failure, devices = ?backend.input_open_failures.0.borrow(), "input recovery retries exhausted");
                }
            }
            clear_retry_marker(state);
            TimeoutAction::Drop
        }) {
        Ok(token) => backend.input_retry = Some(token),
        Err(error) => tracing::error!(%error, "failed to schedule libinput recovery"),
    }
}

pub(super) fn resume(state: &mut Ferese) {
    cancel_retry(state);
    if let Err(failure) = attempt(state, false) {
        retry(state, failure);
    }
}

pub(super) fn refresh(state: &mut Ferese) {
    if !state
        .direct_backend
        .as_ref()
        .is_some_and(|backend| backend.active)
    {
        // A later seat activation owns reopening devices on an inactive VT.
        return;
    }
    cancel_retry(state);
    match attempt(state, true) {
        Ok(()) => tracing::info!("refreshed input devices after system resume"),
        Err(failure) => retry(state, failure),
    }
}

pub(super) fn reconcile(state: &mut Ferese) {
    let Some(backend) = state.direct_backend.as_ref() else {
        return;
    };
    let slept = match (backend.input_sleep_offset, SleepOffset::sample()) {
        (Some(previous), Some(current)) => previous.advanced_to(current),
        _ => true,
    };
    if slept {
        // A listener outage can hide PrepareForSleep(false). Reconcile input
        // after that missed sleep without counting it as fresh idle activity.
        refresh(state);
    }
}

fn clear_retry_marker(state: &mut Ferese) {
    if let Some(backend) = state.direct_backend.as_mut() {
        backend.input_retry = None;
    }
}

fn cancel_retry(state: &mut Ferese) {
    if let Some(token) = state
        .direct_backend
        .as_mut()
        .and_then(|backend| backend.input_retry.take())
    {
        state.loop_handle.remove(token);
    }
}

#[cfg(test)]
mod tests;
