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

/// Finite startup deadline for one generation.
const STARTUP_TIMEOUT: Duration = Duration::from_secs(10);
/// Datagrams one readiness dispatch may consume before yielding the event loop.
const READINESS_DRAIN_BUDGET: usize = 16;

/// Interval used only while a child needs monitoring.
const CHILD_POLL_INTERVAL: Duration = Duration::from_millis(250);
/// Grace period for the cooperative service group to exit during teardown.
const STOP_TIMEOUT: Duration = Duration::from_secs(2);

type Loop = LoopHandle<'static, Ferese>;

/// A snapshot of the managed X11 service, for diagnostics and IPC.
///
/// This deliberately reports the **Satellite** PID, not an Xwayland PID:
/// Ferese owns the Satellite child and cannot cheaply obtain the Xwayland child
/// it starts, so reporting a guess would be misleading. The X11 cookie is never
/// included here or in any IPC response.
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
    /// False when the packaged Satellite cannot satisfy the readiness contract.
    pub readiness_contract: bool,
    pub last_error: Option<String>,
}

/// Every event source a single generation owns.
///
/// Keeping these together makes "release this generation" one operation. A
/// source that is required for correctness must be acquired before the process
/// is published as running; a failure to acquire one routes through the stop
/// path instead of silently leaving a live service unobserved.
#[derive(Default)]
struct GenerationSources {
    /// The endpoint, owned by the generation so it is closed with its sources.
    readiness_socket: Option<ReadinessSocket>,
    readiness: Option<RegistrationToken>,
    startup_deadline: Option<RegistrationToken>,
    /// Exactly one exit watcher is active: a pidfd source, or the bounded poll.
    exit_watch: Option<RegistrationToken>,
    /// Drives the asynchronous stop transition.
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

    /// Remove every source this generation owns.
    fn release(&mut self, loop_handle: &Loop) {
        // Closing the endpoint rejects any late datagram.
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

/// The single owner of the managed X11 service.
///
/// This type is the sole owner and reaper of its `Child`. It must never hand
/// that child to `process::watch_client_exit()` or `daemon::Runner`: two
/// reapers, separate restart decisions, or a discarded `Child` would undermine
/// the lifecycle.
///
/// Every helper takes a cloned `LoopHandle` rather than `&mut Ferese`, so event
/// callbacks can hold `&mut XwaylandManager` without a second state borrow.
pub(crate) struct XwaylandManager {
    lifecycle: Lifecycle,
    config: XwaylandConfig,
    reservation: Reservation,
    authority: AuthorityFile,
    /// This manager's private directory for readiness endpoints.
    ///
    /// Each process gets its own directory, so a stale socket left by an
    /// earlier compositor can never collide with a live generation, and two
    /// compositor instances never share a notification path.
    notify_directory: tempfile::TempDir,
    /// The advertised display name, stable for the whole session.
    display: String,
    /// The sole owner and reaper of the current generation's process group.
    process: Option<OwnedProcessGroup>,
    /// Every event source owned by the current generation, so cleanup is one
    /// operation rather than a set of independent optional fields.
    sources: GenerationSources,
    /// The backoff timer, owned by the backoff phase.
    backoff_token: Option<RegistrationToken>,
    /// When the in-flight stop escalates from SIGTERM to SIGKILL.
    stop_kill_deadline: Option<std::time::Instant>,
    /// When a stop that already sent SIGKILL gives up on the group.
    stop_kill_grace: Option<std::time::Instant>,
    listener_tokens: Vec<RegistrationToken>,
    last_error: Option<String>,
    readiness_contract: bool,
    /// Readiness verdict for the current generation, for status reporting.
    readiness_verdict: Readiness,
}

impl XwaylandManager {
    /// Diagnostics snapshot. See [`status_snapshot`] for the IPC projection.
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
            readiness_contract: self.readiness_contract,
            last_error: self.last_error.clone(),
        }
    }

    /// The configuration this live service was actually started with.
    pub fn config(&self) -> &XwaylandConfig {
        &self.config
    }

    fn owns_generation(&self, generation: u64) -> bool {
        self.lifecycle.phase.generation() == Some(generation)
    }

    /// Project the internal phase onto the public state.
    ///
    /// Matched exhaustively so a new phase cannot silently fall through to a
    /// misleading "failed" in status output.
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
            Phase::Failed => State::Failed,
        }
    }

    /// The advertised X11 endpoint for child processes.
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

    /// Remove every listener source, closing the descriptor clones they own.
    ///
    /// `disable()` stops events but does **not** close a descriptor held by a
    /// calloop source, so terminal-failure and final-cleanup paths must remove
    /// the sources.
    fn remove_listener_sources(&mut self, loop_handle: &Loop) {
        for token in self.listener_tokens.drain(..) {
            loop_handle.remove(token);
        }
    }

    /// Release every source owned by the current generation.
    fn release_generation_sources(&mut self, loop_handle: &Loop) {
        self.sources.release(loop_handle);
    }

    /// Release the backoff timer, if one is armed.
    fn remove_backoff_timer(&mut self, loop_handle: &Loop) {
        if let Some(token) = self.backoff_token.take() {
            loop_handle.remove(token);
        }
    }

    /// Record that the current generation satisfied the readiness contract.
    /// Without this the verdict stays `pending` and no caller can ever observe
    /// that readiness was verified.
    fn note_verified_readiness(&mut self) {
        self.readiness_verdict = Readiness::Verified;
    }

    fn finish_ready(&mut self, loop_handle: &Loop) {
        self.note_verified_readiness();
        // Readiness is settled: drop its endpoint and the startup deadline, but
        // keep the exit watch so a crash is still observed.
        if let Some(token) = self.sources.readiness.take() {
            loop_handle.remove(token);
        }
        if let Some(token) = self.sources.startup_deadline.take() {
            loop_handle.remove(token);
        }
        self.sources.readiness_socket = None;
        self.last_error = None;
    }

    /// Install the exit watcher for `generation`.
    ///
    /// A pidfd is preferred because it becomes readable on termination without
    /// reaping. If it cannot be created or registered, a bounded poll is used
    /// instead; either way the generation ends up with exactly one watcher.
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

    /// Bounded fallback watcher. Observes the leader without reaping it, so the
    /// process-group identity stays valid for signalling.
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

    /// True only for the currently owned `(generation, pid)`.
    ///
    /// A stale callback must not touch state or signal anything, and losing
    /// ownership must itself reject the callback.
    fn owns(&self, generation: u64, pid: u32) -> bool {
        self.lifecycle.phase.generation() == Some(generation)
            && self.process.as_ref().is_some_and(|process| process.pid() == pid)
    }

    /// The owned service process exited without being asked to.
    fn handle_exit(&mut self, loop_handle: &Loop, generation: u64, pid: u32) {
        if !self.owns(generation, pid) {
            return;
        }
        // A crash after readiness is retryable; an early failure usually is not.
        let retryable = self.lifecycle.phase.is_running();
        let detail = "X11 service exited".to_owned();
        self.fail(loop_handle, generation, &detail, retryable);
    }

    /// Record a failure and take ownership of stopping the generation.
    ///
    /// This never publishes `Backoff`, `Failed`, or `Idle` directly. The
    /// lifecycle moves to `Stopping`, which still owns the process, and only
    /// [`Self::complete_stop`] publishes the next phase once no live process can
    /// still hold the X11 listeners.
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

    /// Begin the single owned stop transition.
    ///
    /// Releases the generation's monitoring sources and keeps the process, so
    /// the stop poll can drive cleanup while the compositor stays dispatchable.
    /// Listeners stay disabled for the whole transition: a queued connection
    /// must not be able to start a replacement generation.
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
                if self.process.is_none() {
                    // Cleanup finished inside the helper.
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

    /// Watch a stop that is still in flight.
    ///
    /// Every in-flight stop needs a source. Without one nothing would ever
    /// escalate to `SIGKILL`, reap the leader, or publish the outcome, so a
    /// service whose leader was already gone would stay `Stopping` forever.
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

    /// Drive the tail of a stop where the leader is already gone.
    ///
    /// A crashed leader can leave descendants holding the X11 listeners. The
    /// replacement generation must not start until the whole group is gone, so
    /// the stop stays in flight: escalate to `SIGKILL` at the deadline, then
    /// give up and report rather than block shutdown forever.
    fn wait_for_empty_group(&mut self, loop_handle: &Loop, generation: u64, after_stop: AfterStop) {
        if self.process.as_ref().is_none_or(|process| process.group_is_empty()) {
            self.complete_stop(loop_handle, generation, after_stop);
            return;
        }

        let now = std::time::Instant::now();
        let Some(kill_deadline) = self.stop_kill_deadline else {
            self.complete_stop(loop_handle, generation, after_stop);
            return;
        };
        if now < kill_deadline {
            return;
        }

        match self.stop_kill_grace {
            None => {
                warn!(generation, "the X11 service left survivors; sending SIGKILL");
                if let Some(process) = self.process.as_ref() {
                    process.signal_group(libc::SIGKILL);
                }
                self.stop_kill_grace = Some(now + STOP_TIMEOUT);
            }
            Some(grace) if now >= grace => {
                // Reported, but never allowed to wedge the compositor: a
                // survivor holding the display will surface as a bind failure
                // on the next generation instead of hanging here.
                let detail = "the X11 service group survived SIGKILL".to_owned();
                warn!(generation, %detail, "cannot fully reclaim the X11 service group");
                self.last_error = Some(detail);
                self.complete_stop(loop_handle, generation, AfterStop::Failed);
            }
            Some(_) => {}
        }
    }

    /// Cleanup could not be observed, so the service must not become startable.
    fn note_cleanup_failure(&mut self, loop_handle: &Loop, generation: u64, error: String) {
        let detail = format!("cannot clean up the X11 service: {error}");
        warn!(generation, %detail, "X11 cleanup is incomplete");
        self.last_error = Some(detail.clone());
        if let Some(process) = self.process.as_mut() {
            process.note_cleanup_error(detail);
        }
        // An incomplete cleanup must never leave the service startable.
        self.complete_stop(loop_handle, generation, AfterStop::Failed);
    }

    /// Publish the phase that follows a completed stop.
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
        let cleanup_unconfirmed = self
            .process
            .as_ref()
            .is_some_and(|process| process.cleanup_error().is_some());
        // Safe now: the leader has been reaped, so Drop has nothing to signal.
        self.process = None;

        // A caller that could not confirm cleanup asks for `Failed`; honour it
        // instead of publishing whatever the stop was scheduled to do.
        if cleanup_unconfirmed || after_stop == AfterStop::Failed {
            self.lifecycle.escalate_stop_to_failed(generation);
        }

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

    /// Explicit user-requested retry from the failed phase.
    ///
    /// Rebinds the **same** display so launchers that already learned the
    /// display number keep working, and registers fresh sources.
    pub fn explicit_retry(&mut self, loop_handle: &Loop) -> Result<(), String> {
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

/// Stable, IPC-friendly name for a phase.
fn phase_name(phase: Phase) -> &'static str {
    match phase {
        Phase::Idle => "idle",
        Phase::Starting(_) => "starting",
        Phase::Spawned(_) => "starting",
        Phase::Running(_) => "running",
        Phase::Stopping { .. } => "stopping",
        Phase::Backoff { .. } => "backoff",
        Phase::Failed => "failed",
        Phase::Stopped => "stopped",
    }
}

/// Read and validate the session's private runtime directory.
///
/// `XDG_RUNTIME_DIR` must be a real, private, user-owned directory. Never fall
/// back to `/tmp`.
pub(crate) fn runtime_directory() -> Result<PathBuf, String> {
    let raw = std::env::var_os("XDG_RUNTIME_DIR")
        .filter(|value| !value.is_empty())
        .ok_or("XDG_RUNTIME_DIR is not set, so X11 has no private per-session directory")?;
    let path = PathBuf::from(raw);
    validate_runtime_directory(&path)?;
    Ok(path)
}

/// Reject a runtime directory that is not private and owned by this user.
///
/// Both the authority file and the readiness socket live here, so a shared or
/// world-writable directory would expose X11 credentials to other users.
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

/// Initialize the managed X11 service.
///
/// Reserves the display, creates the authority file, publishes the child
/// environment, and registers listener sources. This does **not** wait for a
/// Satellite -> Ferese Wayland handshake on this thread.
///
/// Optional-dependency failures become diagnostics rather than aborting the
/// desktop; only a violated compositor invariant is fatal.
pub fn initialize(state: &mut Ferese) -> Result<(), String> {
    if !state.xwayland_config.enabled {
        info!("X11 support is disabled by configuration");
        return Ok(());
    }

    match start_service(state) {
        Ok(()) => Ok(()),
        Err(error) => {
            // A missing optional dependency must leave a working native Wayland
            // desktop with a visible diagnostic, never a fake DISPLAY.
            warn!(%error, "X11 unavailable; continuing with a native Wayland desktop");
            state.x11_diagnostic = Some(error);
            Ok(())
        }
    }
}

/// Reserve the endpoints and register listener sources.
///
/// Shared by startup and by an explicit retry after an unavailable start.
fn start_service(state: &mut Ferese) -> Result<(), String> {
    let config = state.xwayland_config.clone();
    let loop_handle = state.loop_handle.clone();
    let mut manager = prepare(&config)?;

    // Publish the endpoint to children now that it genuinely exists. Never
    // advertise a fake DISPLAY.
    state.session_environment.x11 = Some(manager.x11_environment());

    if let Err(error) = register_listeners(&loop_handle, &mut manager) {
        // Withdraw the endpoint again: the listener sources never came up, so
        // the reservation is about to be dropped and advertising it would hand
        // children a DISPLAY that accepts nothing.
        state.session_environment.x11 = None;
        return Err(format!("cannot watch the X11 listening sockets: {error}"));
    }

    let display_name = manager.display.clone();
    let eager = config.startup == XwaylandStartup::Eager;
    state.xwayland = Some(manager);
    state.x11_diagnostic = None;
    info!(display = %display_name, "reserved an X11 display for the session");

    if eager {
        // Request the first generation from an idle callback, then enter the
        // event loop. This uses the same allocation and environment path.
        request_start(state);
    } else {
        info!("X11 is available on demand; no X11 process has been started");
    }

    Ok(())
}

/// Reserve the display and create the authority file.
fn prepare(config: &XwaylandConfig) -> Result<XwaylandManager, String> {
    let runtime_directory = runtime_directory()?;
    // Own a private directory before reserving anything, so a failure here
    // leaves no display reservation behind.
    let notify_directory = tempfile::Builder::new()
        .prefix("ferese-x11-")
        .rand_bytes(16)
        .tempdir_in(&runtime_directory)
        .map_err(|error| format!("cannot create the X11 notification directory: {error}"))?;
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
        // The packaged Satellite is expected to carry the notification feature.
        // A failing readiness check reports this distinctly at runtime.
        readiness_contract: true,
        readiness_verdict: Readiness::NotAttempted,
    })
}

/// Register a calloop source for each owned listening socket.
///
/// The callback must never call `accept()`: the application is already queued
/// at the kernel listening socket and Xwayland needs to accept that connection
/// after it starts.
fn register_listeners(loop_handle: &Loop, manager: &mut XwaylandManager) -> Result<(), String> {
    let mut inserted = Vec::new();

    for listener in manager.reservation.listeners() {
        // Roll back any source inserted so far if a later one fails, so a
        // partially registered endpoint never looks healthy.
        let listener = listener.try_clone().map_err(|error| {
            for token in inserted.drain(..) {
                loop_handle.remove(token);
            }
            error.to_string()
        })?;
        let token = loop_handle
            .insert_source(Generic::new(listener, Interest::READ, Mode::Level), |_, _, state| {
                let handle = state.loop_handle.clone();
                // Claim a generation ticket synchronously, so two sockets
                // becoming readable in one dispatch cannot start two services.
                let ticket = state.xwayland.as_mut().and_then(|manager| {
                    let ticket = manager.lifecycle.request_start();
                    if ticket.is_some() {
                        // Disable every trigger: a queued-but-unaccepted
                        // connection must not spin the loop while starting.
                        manager.disable_listeners(&handle);
                    }
                    ticket
                });

                if let Some(generation) = ticket {
                    handle.insert_idle(move |state| {
                        // Recheck the generation: an old queued idle callback
                        // must not start a later service.
                        let current = state
                            .xwayland
                            .as_ref()
                            .is_some_and(|manager| manager.owns_generation(generation));
                        if current {
                            start_generation(state, generation);
                        }
                    });
                }

                // Disable the trigger even when another callback already
                // claimed the generation.
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

/// Explicitly retry the managed X11 service.
///
/// Covers both failure shapes: an endpoint that failed after exhausting its
/// bounded retries, and a service that was never available at startup (for
/// example because `xwayland-satellite` was installed later). A disabled
/// configuration is never overridden, because the user asked for X11 to be off.
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

    // The user asked for X11 now, so verify it immediately instead of waiting
    // for the next application connection.
    if request_start(state).is_none() {
        return Err("X11 could not be started yet; a previous attempt is still in progress".to_owned());
    }
    state.x11_diagnostic = None;
    Ok(())
}

/// Report the managed X11 service for `feresectl xwayland status`.
///
/// Never includes the X11 cookie or authority-file contents. `restart_required`
/// is true when the configuration currently on disk differs from the one the
/// live service was started with, because the endpoints and child environment
/// are allocated once per session.
pub fn status_snapshot(state: &Ferese) -> ferese_ipc::xwayland::Status {
    use ferese_ipc::xwayland::{State, Status};

    let Some(manager) = state.xwayland.as_ref() else {
        // No manager: either disabled by configuration, or unavailable.
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

/// Request a generation from outside the listener path, such as eager startup
/// or an explicit retry.
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

/// Start one generation: spawn Satellite, watch it, and wait for validated
/// readiness.
fn start_generation(state: &mut Ferese, generation: u64) {
    let loop_handle = state.loop_handle.clone();
    let Some(manager) = state.xwayland.as_mut() else {
        return;
    };
    if !manager.owns_generation(generation) || manager.lifecycle.phase != Phase::Starting(generation) {
        return;
    }

    // Disable every trigger before spawning, so a queued connection cannot spin
    // the loop while the service comes up.
    manager.disable_listeners(&loop_handle);
    manager.release_generation_sources(&loop_handle);

    // A fresh per-generation notification endpoint. Registering it before the
    // spawn prevents a missed readiness datagram.
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
    let timer = Timer::from_duration(STARTUP_TIMEOUT);
    match loop_handle.insert_source(timer, move |_, _, state| {
        let handle = state.loop_handle.clone();
        if let Some(manager) = state.xwayland.as_mut()
            && manager.owns_generation(generation)
            && manager.lifecycle.phase == Phase::Spawned(generation)
        {
            let error = format!("X11 service did not report readiness within {STARTUP_TIMEOUT:?}");
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

/// Register the readiness source for `generation`.
///
/// Work per dispatch is bounded: a level-triggered socket that keeps receiving
/// data must not be able to consume the compositor's event loop indefinitely.
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

            // Drain, but only a bounded number of datagrams per dispatch.
            for _ in 0..READINESS_DRAIN_BUDGET {
                let Some(socket) = manager.sources.readiness_socket.as_ref() else {
                    return Ok(PostAction::Remove);
                };
                match socket.recv_ready(pid) {
                    Ok(true) => {
                        info!(generation, "X11 service reported readiness");
                        if manager.lifecycle.ready(generation) {
                            manager.finish_ready(&handle);
                            // Running: new clients connect directly.
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

/// Stop the service and release resources in ownership order.
///
/// Must be called after the event loop returns and before the sockets, auth
/// file, and Wayland display are dropped.
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

    // Bounded teardown of the whole owned group: signal, escalate to SIGKILL,
    // and only then reap the leader, so a descendant that ignores SIGTERM cannot
    // survive still holding the X11 listeners.
    if let Some(mut process) = manager.process.take()
        && let Err(error) = process.terminate(STOP_TIMEOUT)
    {
        warn!(%error, "the X11 service could not be stopped cleanly");
        manager.last_error = Some(error);
    }

    // Close descriptors, then let `manager` drop the owned socket path and
    // Xauthority file, then release the display-number lock.
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
        // Both pre-readiness phases report as starting, never as running.
        assert_eq!(phase_name(Phase::Starting(1)), "starting");
        assert_eq!(phase_name(Phase::Spawned(1)), "starting");
    }

    #[test]
    fn a_confirmed_generation_reports_verified_readiness() {
        // The verdict is what `feresectl xwayland status` reports, and it is the
        // only evidence that the readiness contract actually held. Preparing a
        // service needs a private runtime directory, which a desktop test
        // environment provides and a bare container may not.
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
        let stale = tempfile::Builder::new()
            .prefix("ferese-x11-")
            .rand_bytes(16)
            .tempdir_in(root.path())
            .expect("the first instance directory");
        let stale_socket = ReadinessSocket::create(stale.path(), 1).expect("the first endpoint");

        let next = tempfile::Builder::new()
            .prefix("ferese-x11-")
            .rand_bytes(16)
            .tempdir_in(root.path())
            .expect("the second instance directory");
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
            "the notification directory must be private to the session"
        );
    }

    #[test]
    fn the_runtime_directory_must_be_private() {
        // A real directory, but world-writable: it would expose the Xauthority
        // cookie and readiness socket to other users.
        assert!(validate_runtime_directory(Path::new("/tmp")).is_err());
        // Not a directory, and not present at all.
        assert!(validate_runtime_directory(Path::new("/dev/null")).is_err());
        assert!(validate_runtime_directory(Path::new("/nonexistent/ferese")).is_err());
        // A symlink is rejected even when it points at a valid directory.
        let link = std::env::temp_dir().join("ferese-xwayland-runtime-link");
        let _ = std::fs::remove_file(&link);
        std::os::unix::fs::symlink("/tmp", &link).expect("create a symlink to a valid directory");
        assert!(validate_runtime_directory(&link).is_err());
        let _ = std::fs::remove_file(&link);
    }
}
