mod auth;
mod child;
#[cfg(test)]
mod integration_tests;
mod lifecycle;
mod readiness;
mod sockets;

use std::ffi::OsString;
use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;
use std::time::Duration;

use calloop::generic::Generic;
use calloop::timer::{TimeoutAction, Timer};
use calloop::{Interest, LoopHandle, Mode, PostAction, RegistrationToken};
use tracing::{info, warn};

use crate::Ferese;
use crate::config::{XwaylandConfig, XwaylandStartup};

use ferese_ipc::xwayland::Readiness;

use self::auth::AuthorityFile;
use self::child::{Observation, OwnedProcessGroup};
use self::lifecycle::{AfterStop, Lifecycle, Phase};
use self::readiness::ReadinessSocket;
use self::sockets::Reservation;

const STARTUP_TIMEOUT: Duration = Duration::from_secs(10);
const READINESS_DRAIN_BUDGET: usize = 16;
const CHILD_POLL_INTERVAL: Duration = Duration::from_millis(250);
const STOP_TIMEOUT: Duration = Duration::from_secs(2);

type Loop = LoopHandle<'static, Ferese>;

#[cfg(test)]
pub(crate) mod test_hooks {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::time::Duration;

    pub(crate) static PIDFD_UNAVAILABLE: AtomicBool = AtomicBool::new(false);
    pub(crate) static FAIL_LISTENER_REGISTRATION_AFTER: AtomicUsize = AtomicUsize::new(usize::MAX);
    pub(crate) static FAIL_REBIND_AFTER: AtomicUsize = AtomicUsize::new(usize::MAX);
    pub(crate) static STARTUP_DEADLINE_MS: AtomicUsize = AtomicUsize::new(0);
    /// Pretend the previous service group can never be reclaimed, so a test can
    /// reach the unconfirmed-cleanup state without an unkillable survivor.
    pub(crate) static FORCE_GROUP_NOT_EMPTY: AtomicBool = AtomicBool::new(false);

    pub(crate) fn force_group_not_empty() -> bool {
        FORCE_GROUP_NOT_EMPTY.load(Ordering::SeqCst)
    }

    /// True when this call must fail instead of performing its operation.
    ///
    /// `usize::MAX` is idle; smaller values count allowed calls, then keep firing.
    pub(crate) fn fire(counter: &AtomicUsize) -> bool {
        loop {
            let current = counter.load(Ordering::SeqCst);
            if current == usize::MAX {
                return false;
            }
            if current == 0 {
                return true;
            }
            if counter
                .compare_exchange(current, current - 1, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok()
            {
                return false;
            }
        }
    }

    pub(crate) fn startup_deadline() -> Option<Duration> {
        match STARTUP_DEADLINE_MS.load(Ordering::SeqCst) {
            0 => None,
            milliseconds => Some(Duration::from_millis(milliseconds as u64)),
        }
    }

    // Directory the display allocator must use instead of `/tmp/.X11-unix`;
    // thread-local so parallel tests keep their own inputs.
    thread_local! {
        static ALLOCATOR_ROOT: std::cell::RefCell<Option<(std::path::PathBuf, std::path::PathBuf)>> =
            const { std::cell::RefCell::new(None) };
    }

    /// Point the display allocator at an owned directory for one test.
    pub(crate) fn allocator_root(path: std::path::PathBuf) {
        allocator_root_in(path.clone(), path);
    }

    /// Point the display allocator at explicit socket and lock directories.
    ///
    /// An X client resolves `DISPLAY` against `/tmp/.X11-unix`, so the live test names it.
    pub(crate) fn allocator_root_in(sockets: std::path::PathBuf, locks: std::path::PathBuf) {
        ALLOCATOR_ROOT.with(|slot| *slot.borrow_mut() = Some((sockets, locks)));
    }

    pub(crate) fn allocator_root_for_reservation() -> Option<(std::path::PathBuf, std::path::PathBuf)> {
        ALLOCATOR_ROOT.with(|slot| slot.borrow().clone())
    }

    /// Owned display-slot inputs for a test that never named a root: an unset
    /// root falls back to a private per-thread directory, never `/tmp/.X11-unix`.
    pub(crate) fn default_allocator_root() -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!(
            "ferese-x11-sockets-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::create_dir_all(&root);
        let _ = std::fs::set_permissions(&root, std::os::unix::fs::PermissionsExt::from_mode(0o700));
        root
    }

    pub(crate) fn reset() {
        PIDFD_UNAVAILABLE.store(false, Ordering::SeqCst);
        FAIL_LISTENER_REGISTRATION_AFTER.store(usize::MAX, Ordering::SeqCst);
        FAIL_REBIND_AFTER.store(usize::MAX, Ordering::SeqCst);
        STARTUP_DEADLINE_MS.store(0, Ordering::SeqCst);
        FORCE_GROUP_NOT_EMPTY.store(false, Ordering::SeqCst);
        ALLOCATOR_ROOT.with(|slot| *slot.borrow_mut() = None);
    }
}

#[cfg(test)]
fn startup_deadline() -> Duration {
    test_hooks::startup_deadline().unwrap_or(STARTUP_TIMEOUT)
}

#[cfg(not(test))]
fn startup_deadline() -> Duration {
    STARTUP_TIMEOUT
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct XwaylandStatus {
    pub enabled: bool,
    pub effective_startup: String,
    pub phase: &'static str,
    pub display: Option<String>,
    pub satellite_pid: Option<u32>,
    pub generation: Option<u64>,
    pub readiness: ferese_ipc::xwayland::Readiness,
    pub recent_failures: u32,
    pub last_error: Option<String>,
}

#[derive(Default)]
struct GenerationSources {
    readiness_socket: Option<ReadinessSocket>,
    readiness: Option<RegistrationToken>,
    startup_deadline: Option<RegistrationToken>,
    exit_watch: Option<RegistrationToken>,
    stop_poll: Option<RegistrationToken>,
}

impl GenerationSources {
    fn is_empty(&self) -> bool {
        self.readiness_socket.is_none()
            && self.readiness.is_none()
            && self.startup_deadline.is_none()
            && self.exit_watch.is_none()
            && self.stop_poll.is_none()
    }

    fn release(&mut self, loop_handle: &Loop) {
        self.readiness_socket = None;
        if let Some(token) = self.readiness.take() {
            loop_handle.remove(token);
        }
        if let Some(token) = self.startup_deadline.take() {
            loop_handle.remove(token);
        }
        if let Some(token) = self.exit_watch.take() {
            loop_handle.remove(token);
        }
        if let Some(token) = self.stop_poll.take() {
            loop_handle.remove(token);
        }
    }
}

pub(crate) struct XwaylandManager {
    lifecycle: Lifecycle,
    config: XwaylandConfig,
    reservation: Reservation,
    authority: AuthorityFile,
    notify_directory: tempfile::TempDir,
    display: String,
    process: Option<OwnedProcessGroup>,
    sources: GenerationSources,
    backoff_token: Option<RegistrationToken>,
    stop_kill_deadline: Option<std::time::Instant>,
    stop_kill_grace: Option<std::time::Instant>,
    listener_tokens: Vec<RegistrationToken>,
    last_error: Option<String>,
    readiness_verdict: Readiness,
}

impl XwaylandManager {
    pub fn status(&self) -> XwaylandStatus {
        XwaylandStatus {
            enabled: self.config.enabled,
            effective_startup: match self.config.startup {
                XwaylandStartup::OnDemand => "on-demand",
                XwaylandStartup::Eager => "eager",
            }
            .to_owned(),
            phase: phase_name(self.lifecycle.phase),
            display: Some(self.display.clone()),
            satellite_pid: self.process.as_ref().map(OwnedProcessGroup::pid),
            generation: self.lifecycle.phase.generation(),
            readiness: self.readiness_verdict,
            recent_failures: self.lifecycle.recent_failures() as u32,
            last_error: self.last_error.clone(),
        }
    }

    pub fn config(&self) -> &XwaylandConfig {
        &self.config
    }

    fn owns_generation(&self, generation: u64) -> bool {
        self.lifecycle.phase.generation() == Some(generation)
    }

    fn projected_state(&self) -> ferese_ipc::xwayland::State {
        use ferese_ipc::xwayland::State;
        match self.lifecycle.phase {
            Phase::Idle => State::Idle,
            // Exec succeeded but the service has not reported readiness, so it
            // is not running yet: children must not be told otherwise.
            Phase::Starting(_) | Phase::Spawned(_) => State::Starting,
            Phase::Running(_) => State::Running,
            // Cleanup owns the previous service; the endpoint is not usable.
            Phase::Stopping { .. } | Phase::Stopped => State::Stopped,
            Phase::Backoff { .. } => State::Backoff,
            // Failed and still failed: a parked cleanup refuses a retry until
            // the group it owns is reclaimed, which `last_error` explains.
            Phase::CleanupFailed { .. } | Phase::Failed => State::Failed,
        }
    }

    fn x11_environment(&self) -> crate::X11Environment {
        crate::X11Environment {
            display: OsString::from(&self.display),
            authority: self.authority.path().to_owned(),
        }
    }

    fn disable_listeners(&self, loop_handle: &Loop) {
        for token in &self.listener_tokens {
            let _ = loop_handle.disable(token);
        }
    }

    fn enable_listeners(&self, loop_handle: &Loop) {
        for token in &self.listener_tokens {
            let _ = loop_handle.enable(token);
        }
    }

    // Disabling a source leaves its listener clone open; removal releases it.
    fn remove_listener_sources(&mut self, loop_handle: &Loop) {
        for token in self.listener_tokens.drain(..) {
            loop_handle.remove(token);
        }
    }

    fn release_generation_sources(&mut self, loop_handle: &Loop) {
        self.sources.release(loop_handle);
    }

    fn remove_backoff_timer(&mut self, loop_handle: &Loop) {
        if let Some(token) = self.backoff_token.take() {
            loop_handle.remove(token);
        }
    }

    fn note_verified_readiness(&mut self) {
        self.readiness_verdict = Readiness::Verified;
    }

    fn finish_ready(&mut self, loop_handle: &Loop) {
        self.note_verified_readiness();

        if let Some(token) = self.sources.readiness.take() {
            loop_handle.remove(token);
        }

        if let Some(token) = self.sources.startup_deadline.take() {
            loop_handle.remove(token);
        }

        self.sources.readiness_socket = None;
        self.last_error = None;
    }

    fn install_exit_watch(&mut self, loop_handle: &Loop, generation: u64, pid: u32) -> Result<(), String> {
        if let Ok(fd) = crate::process::pidfd(pid) {
            match loop_handle.insert_source(Generic::new(fd, Interest::READ, Mode::Level), move |_, _, state| {
                let handle = state.loop_handle.clone();
                if let Some(manager) = state.xwayland.as_mut() {
                    manager.handle_exit(&handle, generation, pid);
                }
                Ok(PostAction::Remove)
            }) {
                Ok(token) => {
                    self.sources.exit_watch = Some(token);
                    return Ok(());
                }
                Err(error) => warn!(%error, pid, "cannot register the X11 service exit source"),
            }
        } else {
            warn!(pid, "pidfd unavailable; using a bounded poll to watch the X11 service");
        }

        self.install_exit_poll(loop_handle, generation, pid)
    }

    fn install_exit_poll(&mut self, loop_handle: &Loop, generation: u64, pid: u32) -> Result<(), String> {
        let timer = Timer::from_duration(CHILD_POLL_INTERVAL);
        let token = loop_handle
            .insert_source(timer, move |_, _, state| {
                let handle = state.loop_handle.clone();
                let observed = state
                    .xwayland
                    .as_mut()
                    .filter(|manager| manager.owns(generation, pid))
                    .and_then(|manager| manager.process.as_mut())
                    .map(|process| process.observe());
                match observed {
                    Some(Observation::Running) => return TimeoutAction::ToDuration(CHILD_POLL_INTERVAL),
                    Some(Observation::Exited) | Some(Observation::Failed(_)) | None => {}
                }
                if let Some(manager) = state.xwayland.as_mut() {
                    manager.handle_exit(&handle, generation, pid);
                }
                TimeoutAction::Drop
            })
            .map_err(|error| format!("cannot schedule the X11 service exit poll: {error}"))?;
        self.sources.exit_watch = Some(token);

        Ok(())
    }

    fn owns(&self, generation: u64, pid: u32) -> bool {
        self.lifecycle.phase.generation() == Some(generation)
            && self.process.as_ref().is_some_and(|process| process.pid() == pid)
    }

    fn handle_exit(&mut self, loop_handle: &Loop, generation: u64, pid: u32) {
        if !self.owns(generation, pid) {
            return;
        }

        let retryable = self.lifecycle.phase.is_running();
        let detail = "X11 service exited".to_owned();
        self.fail(loop_handle, generation, &detail, retryable);
    }

    fn fail(&mut self, loop_handle: &Loop, generation: u64, error: &str, retryable: bool) {
        self.last_error = Some(error.to_owned());
        // The verdict describes the generation that just ended.
        self.readiness_verdict = Readiness::NotAttempted;
        let Some(after_stop) =
            self.lifecycle
                .begin_stop_after_failure(generation, std::time::Instant::now(), retryable)
        else {
            return;
        };

        warn!(generation, %error, "X11 service generation failed");
        self.begin_stop(loop_handle, generation, after_stop);
    }

    fn begin_stop(&mut self, loop_handle: &Loop, generation: u64, after_stop: AfterStop) {
        if let Some(token) = self.sources.readiness.take() {
            loop_handle.remove(token);
        }

        if let Some(token) = self.sources.startup_deadline.take() {
            loop_handle.remove(token);
        }

        if let Some(token) = self.sources.exit_watch.take() {
            loop_handle.remove(token);
        }

        self.sources.readiness_socket = None;
        self.disable_listeners(loop_handle);
        debug_assert!(self.sources.is_empty(), "a stop must release every source it owns");

        let Some(process) = self.process.as_mut() else {
            // Nothing survived, so cleanup is already complete.
            self.complete_stop(loop_handle, generation, after_stop);
            return;
        };

        // Ask the whole group to exit, then bound how long a cooperative group
        // may take before SIGKILL. The leader stays unreaped throughout, so the
        // process-group identifier cannot be recycled under us.
        process.signal_group(libc::SIGTERM);
        self.stop_kill_deadline = Some(std::time::Instant::now() + STOP_TIMEOUT);

        match self.process.as_mut().map(OwnedProcessGroup::observe) {
            // The group, not just the leader, has to be gone.
            Some(Observation::Exited) | None => {
                self.wait_for_empty_group(loop_handle, generation, after_stop);
                if self.process.is_none() || !matches!(self.lifecycle.phase, Phase::Stopping { .. }) {
                    // Cleanup finished, or was parked as unconfirmed, inside
                    // the helper: either way no poll belongs to this stop.
                    return;
                }
            }
            Some(Observation::Failed(error)) => {
                self.note_cleanup_failure(loop_handle, generation, error);
                return;
            }
            Some(Observation::Running) => {}
        }

        self.schedule_stop_poll(loop_handle, generation, after_stop);
    }

    fn schedule_stop_poll(&mut self, loop_handle: &Loop, generation: u64, after_stop: AfterStop) {
        let timer = Timer::from_duration(CHILD_POLL_INTERVAL);
        match loop_handle.insert_source(timer, move |_, _, state| {
            let handle = state.loop_handle.clone();
            let Some(manager) = state.xwayland.as_mut() else {
                return TimeoutAction::Drop;
            };

            if !manager.owns(generation, manager.process.as_ref().map_or(0, OwnedProcessGroup::pid)) {
                return TimeoutAction::Drop;
            }

            let observed = manager.process.as_mut().map(OwnedProcessGroup::observe);
            match observed {
                Some(Observation::Running) => {
                    let overdue = manager
                        .stop_kill_deadline
                        .is_some_and(|deadline| std::time::Instant::now() >= deadline);
                    if overdue && let Some(process) = manager.process.as_mut() {
                        process.signal_group(libc::SIGKILL);
                    }
                    TimeoutAction::ToDuration(CHILD_POLL_INTERVAL)
                }
                Some(Observation::Exited) => {
                    manager.wait_for_empty_group(&handle, generation, after_stop);
                    match manager.sources.stop_poll {
                        Some(_) => TimeoutAction::ToDuration(CHILD_POLL_INTERVAL),
                        None => TimeoutAction::Drop,
                    }
                }
                Some(Observation::Failed(error)) => {
                    manager.note_cleanup_failure(&handle, generation, error);
                    TimeoutAction::Drop
                }
                None => {
                    manager.complete_stop(&handle, generation, after_stop);
                    TimeoutAction::Drop
                }
            }
        }) {
            Ok(token) => self.sources.stop_poll = Some(token),
            Err(error) => {
                // Without a poll nothing would ever reap this process. Fall back
                // to one bounded synchronous stop rather than forget it.
                warn!(%error, generation, "cannot schedule X11 cleanup; stopping synchronously");
                self.complete_stop(loop_handle, generation, after_stop);
            }
        }
    }

    fn wait_for_empty_group(&mut self, loop_handle: &Loop, generation: u64, after_stop: AfterStop) {
        let Some(process) = self.process.as_mut() else {
            self.complete_stop(loop_handle, generation, after_stop);
            return;
        };

        if !process.is_reaped() {
            // The leader still owns the group identifier, so signal first and
            // reap second. A zombie holds no descriptors, but it would keep the
            // group present for ever and must not be part of the emptiness test.
            process.signal_group(libc::SIGKILL);
            if let Err(error) = process.reap() {
                self.note_cleanup_failure(loop_handle, generation, error);
                return;
            }
        }

        if self.process.as_ref().is_some_and(OwnedProcessGroup::group_is_empty) {
            self.complete_stop(loop_handle, generation, after_stop);
            return;
        }

        // Only a survivor of SIGKILL gets here. Bound the wait, then report it
        // rather than wedge shutdown or pretend the group was reclaimed.
        let now = std::time::Instant::now();
        match self.stop_kill_grace {
            None => {
                warn!(generation, "the X11 service left survivors after SIGKILL; waiting");
                self.stop_kill_grace = Some(now + STOP_TIMEOUT);
            }
            Some(grace) if now >= grace => {
                let detail = "the X11 service group survived SIGKILL".to_owned();
                warn!(generation, %detail, "cannot reclaim the X11 service group; keeping it under ownership");
                self.last_error = Some(detail);
                // A deadline is not proof that the group is gone, so this must
                // not publish a stop that would drop the only handle on it.
                self.park_cleanup_failure(loop_handle, generation);
            }
            Some(_) => {}
        }
    }

    fn note_cleanup_failure(&mut self, loop_handle: &Loop, generation: u64, error: String) {
        let detail = format!("cannot clean up the X11 service: {error}");
        warn!(generation, %detail, "X11 cleanup is incomplete");
        self.last_error = Some(detail.clone());
        if let Some(process) = self.process.as_mut() {
            process.note_cleanup_error(detail);
        }

        // An incomplete cleanup must never leave the service startable, and it
        // must not be published as if the resources were gone.
        self.park_cleanup_failure(loop_handle, generation);
    }

    /// Keep an unconfirmed cleanup under ownership instead of publishing it.
    ///
    /// The stop is over as far as the event loop is concerned, but the record
    /// it was cleaning up is not: survivors may still hold the X11 listeners.
    /// The record stays owned, the endpoint stays closed, and every start
    /// stays refused until [`Self::confirm_cleanup`] proves the group is
    /// empty.
    fn park_cleanup_failure(&mut self, loop_handle: &Loop, generation: u64) {
        if let Some(token) = self.sources.stop_poll.take() {
            loop_handle.remove(token);
        }
        self.stop_kill_deadline = None;
        self.stop_kill_grace = None;

        // Reap here, while the process-group identity is still the one we own.
        if let Some(process) = self.process.as_mut()
            && let Err(error) = process.reap()
        {
            process.note_cleanup_error(error.clone());
            self.last_error = Some(error);
        }

        if !self.lifecycle.park_cleanup_failure(generation) {
            // Nothing current is waiting on this record, so it must not be
            // mistaken for the cleanup of whatever the lifecycle moved on to.
            warn!(
                generation,
                "an unconfirmed cleanup arrived for a generation that is no longer current"
            );
            return;
        }
        debug_assert!(
            self.process.is_some(),
            "a parked cleanup keeps the record that still owns the group"
        );

        // An open listening socket with no consumer would strand clients, and
        // the path must not exist while survivors may still own the old one.
        self.remove_listener_sources(loop_handle);
        self.reservation.close_listeners();
    }

    /// Prove a parked cleanup is actually finished, so a retry may proceed.
    ///
    /// Only an empty group counts. The deadline that parked the stop said
    /// nothing about the group, so the circuit reopens on evidence alone.
    fn confirm_cleanup(&mut self) -> Result<(), String> {
        let Phase::CleanupFailed { generation } = self.lifecycle.phase else {
            return Ok(());
        };

        let confirmed = match self.process.as_mut() {
            // Nothing is owned any more, so there is nothing left to reclaim.
            None => true,
            Some(process) => match process.reap() {
                Ok(_) => process.group_is_empty(),
                Err(error) => {
                    let error = format!("cannot reap the previous X11 service: {error}");
                    self.last_error = Some(error.clone());
                    return Err(error);
                }
            },
        };

        if !confirmed {
            let error = "the previous X11 service group still holds the X11 display; \
                 cleanup has not finished, so X11 cannot be retried yet"
                .to_owned();
            self.last_error = Some(error.clone());
            return Err(error);
        }

        self.process = None;
        if !self.lifecycle.confirm_cleanup(generation) {
            return Err("the X11 service left the unconfirmed cleanup state while it was checked".to_owned());
        }
        info!(
            generation,
            "reclaimed the previous X11 service group; retrying is allowed again"
        );

        Ok(())
    }

    fn complete_stop(&mut self, loop_handle: &Loop, generation: u64, after_stop: AfterStop) {
        if let Some(token) = self.sources.stop_poll.take() {
            loop_handle.remove(token);
        }

        self.stop_kill_deadline = None;
        self.stop_kill_grace = None;

        // Reap here, while the process-group identity is still the one we own.
        if let Some(process) = self.process.as_mut()
            && let Err(error) = process.reap()
        {
            process.note_cleanup_error(error.clone());
            self.last_error = Some(error);
        }
        // A leader that could not be reaped may still hold the display, so the
        // stop must not publish the outcome it was scheduled to publish.
        if self
            .process
            .as_ref()
            .is_some_and(|process| process.cleanup_error().is_some())
        {
            self.park_cleanup_failure(loop_handle, generation);
            return;
        }
        // Safe now: the leader has been reaped, so Drop has nothing to signal.
        self.process = None;

        let Some(published) = self.lifecycle.finish_stop(generation, std::time::Instant::now()) else {
            return;
        };
        debug_assert!(
            published == after_stop || published == AfterStop::Failed,
            "cleanup may only strengthen the stop result: requested {after_stop:?}, published {published:?}"
        );

        match published {
            AfterStop::Backoff => {
                let delay = self.lifecycle.backoff_delay();
                let timer = Timer::from_duration(delay);
                match loop_handle.insert_source(timer, move |_, _, state| {
                    let handle = state.loop_handle.clone();
                    if let Some(manager) = state.xwayland.as_mut() {
                        // Idle again: a still-queued connection can wake us.
                        if manager.lifecycle.finish_backoff(generation, std::time::Instant::now()) {
                            manager.enable_listeners(&handle);
                        }
                    }
                    TimeoutAction::Drop
                }) {
                    Ok(token) => self.backoff_token = Some(token),
                    Err(error) => warn!(%error, "cannot schedule the X11 backoff retry"),
                }
            }
            AfterStop::Failed => {
                // The circuit is open. An open listening socket with no consumer
                // would strand clients, so fail fast instead.
                warn!("X11 endpoint disabled after repeated failures; waiting for an explicit retry");
                self.remove_listener_sources(loop_handle);
                self.reservation.close_listeners();
            }
            AfterStop::Stopped => {}
        }
    }

    pub fn explicit_retry(&mut self, loop_handle: &Loop) -> Result<(), String> {
        // The previous group may still hold the display. Nothing below may run
        // until that is proven false, or a replacement would be handed an
        // endpoint an unknown survivor still owns.
        if matches!(self.lifecycle.phase, Phase::CleanupFailed { .. }) {
            self.confirm_cleanup()?;
        }

        // A precondition check, not a transition: preparation happens first so
        // a failure cannot leave the service in a state that rejects its own
        // retry or advertises listeners nobody is watching.
        if !self.lifecycle.can_explicit_retry() {
            return Err("X11 is not in a failed state, so there is nothing to retry".to_owned());
        }
        if self.process.is_some() {
            return Err("X11 is still stopping its previous service; retry once it has finished".to_owned());
        }

        // Rebind the **same** display so launchers that already learned the
        // display number keep working. Never silently switch displays.
        if let Err(error) = self.reservation.bind_same_display() {
            let error = format!("cannot rebind X11 display {}: {error}", self.display);
            // Still in `Failed`, so another explicit retry remains allowed.
            self.last_error = Some(error.clone());
            return Err(error);
        }

        // Re-register activation sources. On failure, close what was just bound
        // and stay in `Failed` so the endpoint keeps failing fast.
        self.remove_listener_sources(loop_handle);
        if let Err(error) = register_listeners(loop_handle, self) {
            let error = format!("cannot watch the X11 listeners: {error}");
            self.remove_listener_sources(loop_handle);
            self.reservation.close_listeners();
            self.last_error = Some(error.clone());
            return Err(error);
        }

        if !self.lifecycle.commit_explicit_retry() {
            return Err("X11 left the failed state while it was being prepared".to_owned());
        }
        self.last_error = None;
        info!(display = %self.display, "retrying the X11 service on the same display");

        Ok(())
    }
}

fn phase_name(phase: Phase) -> &'static str {
    match phase {
        Phase::Idle => "idle",
        Phase::Starting(_) => "starting",
        Phase::Spawned(_) => "starting",
        Phase::Running(_) => "running",
        Phase::Stopping { .. } => "stopping",
        Phase::Backoff { .. } => "backoff",
        Phase::CleanupFailed { .. } => "cleanup-failed",
        Phase::Failed => "failed",
        Phase::Stopped => "stopped",
    }
}

pub(crate) fn runtime_directory() -> Result<PathBuf, String> {
    let raw = std::env::var_os("XDG_RUNTIME_DIR")
        .filter(|value| !value.is_empty())
        .ok_or("XDG_RUNTIME_DIR is not set, so X11 has no private per-session directory")?;
    let path = PathBuf::from(raw);
    validate_runtime_directory(&path)?;

    Ok(path)
}

fn validate_runtime_directory(path: &std::path::Path) -> Result<(), String> {
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|error| format!("XDG_RUNTIME_DIR is unusable ({}): {error}", path.display()))?;
    let uid = unsafe { libc::geteuid() };
    if !metadata.is_dir() || metadata.file_type().is_symlink() || metadata.uid() != uid || metadata.mode() & 0o077 != 0
    {
        return Err(format!(
            "XDG_RUNTIME_DIR is not a private user-owned directory: {}",
            path.display()
        ));
    }

    Ok(())
}

pub fn initialize(state: &mut Ferese) {
    if !state.xwayland_config.enabled {
        info!("X11 support is disabled by configuration");
        return;
    }

    if let Err(error) = start_service(state) {
        warn!(%error, "X11 unavailable; continuing with a native Wayland desktop");
        state.x11_diagnostic = Some(error);
        return;
    }

    if state.xwayland_config.startup == XwaylandStartup::Eager {
        if let Err(error) = request_first_generation(state) {
            warn!(%error, "X11 could not be requested at startup; continuing with a native Wayland desktop");
            state.x11_diagnostic = Some(error);
        }
    } else {
        info!("X11 is available on demand; no X11 process has been started");
    }
}

fn start_service(state: &mut Ferese) -> Result<(), String> {
    let config = state.xwayland_config.clone();
    let loop_handle = state.loop_handle.clone();
    let mut manager = prepare(&config)?;

    state.session_environment.x11 = Some(manager.x11_environment());

    if let Err(error) = register_listeners(&loop_handle, &mut manager) {
        state.session_environment.x11 = None;
        return Err(format!("cannot watch the X11 listening sockets: {error}"));
    }

    let display_name = manager.display.clone();
    state.xwayland = Some(manager);
    state.x11_diagnostic = None;
    info!(display = %display_name, "reserved an X11 display for the session");

    Ok(())
}

fn request_first_generation(state: &mut Ferese) -> Result<u64, String> {
    request_start(state)
        .ok_or_else(|| "X11 could not be started yet; a previous attempt is still in progress".to_owned())
}

/// Create the private directory that holds one service instance's readiness
/// socket.
///
/// Production and the tests share this constructor so the mode is asserted
/// against the same code that ships: the directory is `0700` whatever the
/// session umask is, because another user must not be able to place a socket
/// where Ferese looks for notifications.
fn notification_directory(runtime_directory: &std::path::Path) -> Result<tempfile::TempDir, String> {
    tempfile::Builder::new()
        .prefix("ferese-x11-")
        .rand_bytes(16)
        .permissions(std::os::unix::fs::PermissionsExt::from_mode(0o700))
        .tempdir_in(runtime_directory)
        .map_err(|error| format!("cannot create the X11 notification directory: {error}"))
}

fn prepare(config: &XwaylandConfig) -> Result<XwaylandManager, String> {
    let runtime_directory = runtime_directory()?;
    let notify_directory = notification_directory(&runtime_directory)?;
    let reservation = Reservation::allocate().map_err(|error| format!("cannot reserve an X11 display: {error}"))?;
    let display = reservation.display_number();
    let authority = AuthorityFile::create(&runtime_directory, display)
        .map_err(|error| format!("cannot create an Xauthority file: {error}"))?;

    Ok(XwaylandManager {
        lifecycle: Lifecycle::default(),
        config: config.clone(),
        display: reservation.display_name(),
        reservation,
        authority,
        notify_directory,
        process: None,
        sources: GenerationSources::default(),
        backoff_token: None,
        stop_kill_deadline: None,
        stop_kill_grace: None,
        listener_tokens: Vec::new(),
        last_error: None,
        readiness_verdict: Readiness::NotAttempted,
    })
}

// Xwayland, not this callback, must accept the queued client connection.
fn register_listeners(loop_handle: &Loop, manager: &mut XwaylandManager) -> Result<(), String> {
    let mut inserted = Vec::new();

    for listener in manager.reservation.listeners() {
        #[cfg(test)]
        if test_hooks::fire(&test_hooks::FAIL_LISTENER_REGISTRATION_AFTER) {
            for token in inserted.drain(..) {
                loop_handle.remove(token);
            }
            return Err("injected X11 listener registration failure".to_owned());
        }

        let listener = listener.try_clone().map_err(|error| {
            for token in inserted.drain(..) {
                loop_handle.remove(token);
            }
            error.to_string()
        })?;
        let token = loop_handle
            .insert_source(Generic::new(listener, Interest::READ, Mode::Level), |_, _, state| {
                let handle = state.loop_handle.clone();

                let ticket = state.xwayland.as_mut().and_then(|manager| {
                    let ticket = manager.lifecycle.request_start();
                    if ticket.is_some() {
                        manager.disable_listeners(&handle);
                    }
                    ticket
                });

                if let Some(generation) = ticket {
                    handle.insert_idle(move |state| {
                        let current = state
                            .xwayland
                            .as_ref()
                            .is_some_and(|manager| manager.owns_generation(generation));
                        if current {
                            start_generation(state, generation);
                        }
                    });
                }

                Ok(PostAction::Disable)
            })
            .map_err(|error| {
                for token in inserted.drain(..) {
                    loop_handle.remove(token);
                }
                error.to_string()
            })?;

        inserted.push(token);
    }

    manager.listener_tokens.extend(inserted);

    Ok(())
}

pub fn retry(state: &mut Ferese) -> Result<(), String> {
    if !state.xwayland_config.enabled {
        return Err("X11 is disabled in the configuration; enable it and start a new session".to_owned());
    }

    if let Some(manager) = state.xwayland.as_mut() {
        let loop_handle = state.loop_handle.clone();
        manager.explicit_retry(&loop_handle)?;
    } else if let Err(error) = start_service(state) {
        state.x11_diagnostic = Some(error.clone());
        return Err(error);
    }

    request_first_generation(state)?;
    state.x11_diagnostic = None;
    Ok(())
}

pub fn status_snapshot(state: &Ferese) -> ferese_ipc::xwayland::Status {
    use ferese_ipc::xwayland::{State, Status};

    let Some(manager) = state.xwayland.as_ref() else {
        let disabled = !state.xwayland_config.enabled;
        return Status {
            enabled: state.xwayland_config.enabled,
            effective_startup: match state.xwayland_config.startup {
                XwaylandStartup::OnDemand => "on-demand",
                XwaylandStartup::Eager => "eager",
            }
            .to_owned(),
            state: if disabled { State::Disabled } else { State::Failed },
            display: None,
            satellite_pid: None,
            generation: None,
            readiness: Readiness::NotAttempted,
            recent_failures: 0,
            // There is no live service whose configuration could drift, so
            // nothing needs a restart to take effect.
            restart_required: false,
            last_error: if disabled {
                None
            } else {
                // Never report an empty failure: say what the user can do.
                Some(
                    state.x11_diagnostic.clone().unwrap_or_else(|| {
                        "the X11 service is not running; retry it or start a new session".to_owned()
                    }),
                )
            },
        };
    };

    let status = manager.status();
    Status {
        enabled: status.enabled,
        effective_startup: status.effective_startup,
        state: manager.projected_state(),
        display: status.display,
        satellite_pid: status.satellite_pid,
        generation: status.generation,
        readiness: status.readiness,
        recent_failures: status.recent_failures,
        restart_required: manager.config() != &state.xwayland_config,
        last_error: status.last_error,
    }
}

pub fn request_start(state: &mut Ferese) -> Option<u64> {
    let handle = state.loop_handle.clone();
    let ticket = state
        .xwayland
        .as_mut()
        .and_then(|manager| manager.lifecycle.request_start());

    if let Some(generation) = ticket {
        handle.insert_idle(move |state| start_generation(state, generation));
    }

    ticket
}

fn start_generation(state: &mut Ferese, generation: u64) {
    let loop_handle = state.loop_handle.clone();
    let Some(manager) = state.xwayland.as_mut() else {
        return;
    };
    if !manager.owns_generation(generation) || manager.lifecycle.phase != Phase::Starting(generation) {
        return;
    }

    manager.disable_listeners(&loop_handle);
    manager.release_generation_sources(&loop_handle);

    let socket = match ReadinessSocket::create(manager.notify_directory.path(), generation) {
        Ok(socket) => socket,
        Err(error) => {
            let error = format!("cannot create the X11 readiness socket: {error}");
            manager.fail(&loop_handle, generation, &error, false);
            return;
        }
    };
    let notify_path = socket.path().to_owned();
    manager.readiness_verdict = Readiness::Pending;

    let environment = state.session_environment.clone();
    let config_path = manager.config.path.clone();
    let authority = manager.authority.path().to_owned();

    let spawned = child::spawn_satellite(
        &config_path,
        &environment,
        &manager.reservation,
        authority.as_path(),
        Some(notify_path.as_path()),
    );

    let satellite = match spawned {
        Ok(child) => child,
        Err(error) => {
            let error = format!("cannot start {}: {error}", config_path.display());
            manager.fail(&loop_handle, generation, &error, false);
            return;
        }
    };

    if !manager.lifecycle.spawned(generation) {
        // The state machine moved on. Take ownership first so the process is
        // reaped through the same contract rather than leaked.
        let mut process = OwnedProcessGroup::adopt(satellite);
        let _ = process.terminate(STOP_TIMEOUT);
        return;
    }
    let pid = satellite.id();
    info!(pid, generation, "started the X11 service");

    // Publish ownership first, so any later failure has something to stop.
    manager.sources.readiness_socket = Some(socket);
    manager.process = Some(OwnedProcessGroup::adopt(satellite));

    // Acquire the generation's required sources transactionally. A source that
    // cannot be installed means the service is not observable, so the new
    // generation is stopped immediately instead of being published.
    if let Err(error) = install_readiness_source(manager, &loop_handle, generation, pid) {
        manager.fail(
            &loop_handle,
            generation,
            &format!("cannot watch the X11 readiness socket: {error}"),
            false,
        );
        return;
    }
    if let Err(error) = manager.install_exit_watch(&loop_handle, generation, pid) {
        manager.fail(
            &loop_handle,
            generation,
            &format!("cannot watch the X11 service process: {error}"),
            false,
        );
        return;
    }

    // Finite startup deadline. A timeout takes the same owned stop transition
    // as any other failure, so the process is stopped rather than abandoned.
    let deadline = startup_deadline();
    let timer = Timer::from_duration(deadline);
    match loop_handle.insert_source(timer, move |_, _, state| {
        let handle = state.loop_handle.clone();
        if let Some(manager) = state.xwayland.as_mut()
            && manager.owns_generation(generation)
            && manager.lifecycle.phase == Phase::Spawned(generation)
        {
            // The service executed but stayed silent: name the contract it broke.
            let error = format!(
                "the X11 service started but reported no readiness within {deadline:?}; \
                 the packaged xwayland-satellite may lack the readiness notification feature"
            );
            manager.fail(&handle, generation, &error, true);
        }
        TimeoutAction::Drop
    }) {
        Ok(token) => manager.sources.startup_deadline = Some(token),
        Err(error) => {
            let error = format!("cannot arm the X11 startup deadline: {error}");
            manager.fail(&loop_handle, generation, &error, false);
        }
    }
}

fn install_readiness_source(
    manager: &mut XwaylandManager,
    loop_handle: &Loop,
    generation: u64,
    pid: u32,
) -> Result<(), String> {
    let Some(socket) = manager.sources.readiness_socket.as_ref() else {
        return Err("the readiness endpoint is missing".to_owned());
    };
    let clone = socket
        .try_clone_owned()
        .map_err(|error| format!("cannot duplicate the readiness endpoint: {error}"))?;

    let token = loop_handle
        .insert_source(Generic::new(clone, Interest::READ, Mode::Level), move |_, _, state| {
            let handle = state.loop_handle.clone();
            let Some(manager) = state.xwayland.as_mut() else {
                return Ok(PostAction::Remove);
            };
            if !manager.owns_generation(generation) || manager.lifecycle.phase != Phase::Spawned(generation) {
                return Ok(PostAction::Remove);
            }

            for _ in 0..READINESS_DRAIN_BUDGET {
                let Some(socket) = manager.sources.readiness_socket.as_ref() else {
                    return Ok(PostAction::Remove);
                };
                match socket.recv_ready(pid) {
                    Ok(true) => {
                        info!(generation, "X11 service reported readiness");
                        if manager.lifecycle.ready(generation) {
                            manager.finish_ready(&handle);

                            manager.enable_listeners(&handle);
                        }
                        return Ok(PostAction::Remove);
                    }
                    // Ignore stale, foreign, or malformed notifications.
                    Ok(false) => continue,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        // Stay level-triggered for any remaining work.
                        return Ok(PostAction::Continue);
                    }
                    Err(error) => {
                        warn!(%error, "cannot read the X11 readiness notification");
                        return Ok(PostAction::Continue);
                    }
                }
            }

            Ok(PostAction::Continue)
        })
        .map_err(|error| format!("cannot register the readiness source: {error}"))?;
    manager.sources.readiness = Some(token);

    Ok(())
}

pub fn shutdown_after_loop(state: &mut Ferese) {
    let Some(mut manager) = state.xwayland.take() else {
        return;
    };
    let display_name = manager.display.clone();
    info!(display = %display_name, "stopping the managed X11 service");
    let loop_handle = state.loop_handle.clone();

    // Leave the event loop before the bounded sequence below, so it cannot race
    // a dispatch.
    manager.lifecycle.begin_shutdown_stop();
    manager.remove_listener_sources(&loop_handle);
    manager.release_generation_sources(&loop_handle);
    manager.remove_backoff_timer(&loop_handle);

    if let Some(mut process) = manager.process.take()
        && let Err(error) = process.terminate(STOP_TIMEOUT)
    {
        warn!(%error, "the X11 service could not be stopped cleanly");
        manager.last_error = Some(error);
    }

    manager.reservation.close_listeners();

    state.session_environment.x11 = None;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn the_startup_deadline_is_finite_and_bounded() {
        assert!(STARTUP_TIMEOUT > Duration::ZERO && STARTUP_TIMEOUT <= Duration::from_secs(30));
    }

    #[test]
    fn phases_have_stable_names_for_ipc() {
        assert_eq!(phase_name(Phase::Idle), "idle");
        assert_eq!(phase_name(Phase::Running(1)), "running");
        assert_eq!(
            phase_name(Phase::Backoff {
                generation: 1,
                until: std::time::Instant::now()
            }),
            "backoff"
        );
        assert_eq!(phase_name(Phase::Failed), "failed");
        assert_eq!(
            phase_name(Phase::CleanupFailed { generation: 1 }),
            "cleanup-failed",
            "an unconfirmed cleanup is reported distinctly from a finished stop"
        );

        assert_eq!(phase_name(Phase::Starting(1)), "starting");
        assert_eq!(phase_name(Phase::Spawned(1)), "starting");
    }

    #[test]
    fn a_confirmed_generation_reports_verified_readiness() {
        if std::env::var_os("XDG_RUNTIME_DIR").is_none() {
            return;
        }

        let mut manager = prepare(&XwaylandConfig::default()).expect("reserve a managed X11 service");
        assert_eq!(
            manager.status().readiness,
            Readiness::NotAttempted,
            "a fresh generation has not reported readiness yet"
        );

        manager.note_verified_readiness();

        assert_eq!(
            manager.status().readiness,
            Readiness::Verified,
            "confirmed readiness must be recorded, or status can never report it"
        );
    }

    /// Two managers must never share a notification endpoint, even across
    /// process lifetimes where a stale socket from an earlier instance is
    /// still on disk.
    #[test]
    fn notification_endpoints_are_private_per_instance() {
        let root = tempfile::tempdir().expect("a private runtime directory");
        // The production constructor, so the mode below is the shipped one and
        // not a second implementation that only the test believes in.
        let stale = notification_directory(root.path()).expect("the first instance directory");
        let stale_socket = ReadinessSocket::create(stale.path(), 1).expect("the first endpoint");

        let next = notification_directory(root.path()).expect("the second instance directory");
        let next_socket = ReadinessSocket::create(next.path(), 1).expect("the second endpoint");

        assert_ne!(stale.path(), next.path(), "each instance owns its own directory");
        assert_ne!(
            stale_socket.path(),
            next_socket.path(),
            "the same generation number in a different instance must not collide"
        );

        // The directory is private: another user must not be able to place a
        // socket where Ferese will look for notifications.
        let metadata = std::fs::metadata(next.path()).expect("directory metadata");
        let mode = std::os::unix::fs::MetadataExt::mode(&metadata);
        assert_eq!(
            mode & 0o777,
            0o700,
            "the notification directory must be private to the session, whatever the umask is"
        );
    }

    #[test]
    fn the_runtime_directory_must_be_private() {
        assert!(validate_runtime_directory(Path::new("/tmp")).is_err());

        assert!(validate_runtime_directory(Path::new("/dev/null")).is_err());
        assert!(validate_runtime_directory(Path::new("/nonexistent/ferese")).is_err());

        let link = std::env::temp_dir().join("ferese-xwayland-runtime-link");
        let _ = std::fs::remove_file(&link);
        std::os::unix::fs::symlink("/tmp", &link).expect("create a symlink to a valid directory");
        assert!(validate_runtime_directory(&link).is_err());
        let _ = std::fs::remove_file(&link);
    }
}
