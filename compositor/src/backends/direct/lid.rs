//! Resume reads are asynchronous; newer switch events win over old replies.
use std::sync::mpsc;

use super::lid_policy::LidPolicy;
use super::{
    Duration, Error, Ferese, RegistrationToken, TimeoutAction, Timer, cancel_output_timers,
    reconcile_outputs,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Probe {
    serial: u64,
    revision: u64,
    reactivate: bool,
    wake: bool,
}

#[derive(Default)]
pub(super) struct LidState {
    value: Option<bool>,
    revision: u64,
    serial: u64,
    pending: Option<Probe>,
}

impl LidState {
    pub fn closed(&self) -> bool {
        self.value == Some(true)
    }
    pub fn pending(&self) -> bool {
        self.pending.is_some()
    }

    pub fn observe(&mut self, closed: bool) -> bool {
        let changed = self.value != Some(closed);
        self.value = Some(closed);
        self.revision = self.revision.wrapping_add(1);
        changed
    }

    fn begin(&mut self, reactivate: bool, wake: bool) -> Probe {
        self.serial = self.serial.wrapping_add(1);
        let probe = Probe {
            serial: self.serial,
            revision: self.revision,
            reactivate: reactivate || self.pending.is_some_and(|pending| pending.reactivate),
            wake: wake || self.pending.is_some_and(|pending| pending.wake),
        };
        self.pending = Some(probe);
        probe
    }

    fn complete(&mut self, probe: Probe, value: Option<bool>) -> Option<bool> {
        if self.pending != Some(probe) {
            return None;
        }

        self.pending = None;
        if probe.revision == self.revision
            && let Some(value) = value
        {
            self.observe(value);
        }
        Some(probe.reactivate)
    }

    pub fn pause(&mut self) {
        self.pending = None;
    }
}

pub(super) struct Reader {
    requests: mpsc::Sender<Probe>,
    policy: LidPolicy,
    pub deadline: Option<RegistrationToken>,
}

pub(super) fn read_logind(connection: &zbus::blocking::Connection) -> zbus::Result<bool> {
    // A direct Get avoids retaining a property cache across suspend.
    let value: zbus::zvariant::OwnedValue = connection
        .call_method(
            Some("org.freedesktop.login1"),
            "/org/freedesktop/login1",
            Some("org.freedesktop.DBus.Properties"),
            "Get",
            &("org.freedesktop.login1.Manager", "LidClosed"),
        )?
        .body()
        .deserialize()?;
    bool::try_from(value).map_err(Into::into)
}

fn read_acpi() -> Option<bool> {
    let mut state = None;
    let entry_iter = std::fs::read_dir("/proc/acpi/button/lid").ok()?.flatten();
    for entry in entry_iter {
        let Ok(text) = std::fs::read_to_string(entry.path().join("state")) else {
            continue;
        };
        match text.split_whitespace().last() {
            Some("closed") => return Some(true),
            Some("open") => state = Some(false),
            _ => {}
        }
    }
    state
}

impl Reader {
    pub fn set_active(&mut self, active: bool) {
        self.policy.set_active(active);
    }

    pub fn start(state: &Ferese) -> Result<Self, Box<dyn Error>> {
        let policy = LidPolicy::try_new()?;
        let (requests, receiver) = mpsc::channel::<Probe>();
        let (replies, events) = smithay::reexports::calloop::channel::channel();
        state.loop_handle.insert_source(events, |event, _, state| {
            if let smithay::reexports::calloop::channel::Event::Msg((probe, value)) = event {
                complete_refresh(state, probe, value);
            }
        })?;

        std::thread::Builder::new()
            .name("ferese-lid-reader".into())
            .spawn(move || {
                while let Ok(mut probe) = receiver.recv() {
                    for newer in receiver.try_iter() {
                        probe = newer;
                    }
                    let value = zbus::blocking::connection::Builder::system()
                        .and_then(|builder| builder.method_timeout(Duration::from_secs(2)).build())
                        .and_then(|connection| read_logind(&connection))
                        .ok()
                        .or_else(read_acpi);
                    if replies.send((probe, value)).is_err() {
                        break;
                    }
                }
            })?;

        let mut reader = Self {
            requests,
            policy,
            deadline: None,
        };
        reader.set_active(true);
        Ok(reader)
    }
}

pub(super) fn request_refresh(state: &mut Ferese, reactivate: bool) {
    request(state, reactivate, true);
}

/// Listener recovery reconciles hardware without resetting idle activity.
pub(super) fn request_reconcile(state: &mut Ferese) {
    if let Some(backend) = state.direct_backend.as_mut() {
        backend.lid_reader.policy.reconnect();
    }
    request(state, false, false);
}

fn request(state: &mut Ferese, reactivate: bool, wake: bool) {
    let Some(backend) = state.direct_backend.as_mut() else {
        return;
    };
    if !backend.active {
        backend.topology.dirty = true;
        return;
    }

    if let Some(token) = backend.lid_reader.deadline.take() {
        state.loop_handle.remove(token);
    }
    // A timer that fires while readiness is gated would otherwise leave a
    // stale registration token on an unchanged output.
    for device in backend.devices.values_mut() {
        for output in device.outputs.values_mut() {
            cancel_output_timers(&state.loop_handle, output);
        }
    }
    backend.lid_reader.policy.request_suspend(false);
    let probe = backend.lid.begin(reactivate, wake);
    let sent = backend.lid_reader.requests.send(probe).is_ok();
    let timer = state.loop_handle.insert_source(
        Timer::from_duration(Duration::from_secs(3)),
        move |_, _, state| {
            if let Some(backend) = state.direct_backend.as_mut() {
                backend.lid_reader.deadline = None;
            }
            tracing::warn!("lid refresh deadline elapsed; retaining the latest observed state");
            complete_refresh(state, probe, None);
            TimeoutAction::Drop
        },
    );

    if sent && let Ok(token) = timer {
        state
            .direct_backend
            .as_mut()
            .expect("DRM callbacks require an installed direct backend")
            .lid_reader
            .deadline = Some(token);
    } else {
        if let Ok(token) = timer {
            state.loop_handle.remove(token);
        }
        complete_refresh(state, probe, None);
    }
}

fn complete_refresh(state: &mut Ferese, probe: Probe, value: Option<bool>) {
    let Some(backend) = state.direct_backend.as_mut() else {
        return;
    };
    let Some(reactivate) = backend.lid.complete(probe, value) else {
        return;
    };
    if let Some(token) = backend.lid_reader.deadline.take() {
        state.loop_handle.remove(token);
    }
    if !backend.active {
        return;
    }

    // Waking a locked session may request redraws. Hold them until the fresh
    // device/connector snapshot has been reconciled.
    backend.lid_reader.policy.request_suspend(false);
    if probe.wake {
        backend.topology.reconciling = true;
        state.reset_animation_clock();
        state.lock_input_activity();
        state
            .direct_backend
            .as_mut()
            .expect("DRM callbacks require an installed direct backend")
            .topology
            .reconciling = false;
    }

    let reason = if reactivate {
        super::ReconcileReason::Resume
    } else {
        super::ReconcileReason::Configuration
    };
    reconcile_outputs(state, reason);
}

/// Only logind requests suspend. Inactive events wait for fresh resume topology.
pub(super) fn apply_suspend_policy(state: &mut Ferese) {
    let Some(backend) = state.direct_backend.as_mut() else {
        return;
    };
    let requested = backend.active && backend.desired_outputs.suspend && backend.lid.closed();
    backend.lid_reader.policy.request_suspend(requested);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires dbus-daemon and private test sockets"]
    fn logind_reads_are_fresh_without_property_change_signals() {
        use std::io::{BufRead, BufReader};
        use std::process::{Command, Stdio};
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};

        struct Bus(std::process::Child);
        impl Drop for Bus {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        struct Manager(Arc<AtomicBool>);
        #[zbus::interface(name = "org.freedesktop.login1.Manager")]
        impl Manager {
            #[zbus(property)]
            fn lid_closed(&self) -> bool {
                self.0.load(Ordering::Relaxed)
            }
        }

        let mut bus = Bus(Command::new("dbus-daemon")
            .args(["--session", "--nofork", "--print-address=1"])
            .stdout(Stdio::piped())
            .spawn()
            .unwrap());
        let mut address = String::new();
        BufReader::new(bus.0.stdout.take().unwrap())
            .read_line(&mut address)
            .unwrap();
        let closed = Arc::new(AtomicBool::new(true));
        let server = zbus::blocking::connection::Builder::address(address.trim())
            .unwrap()
            .name("org.freedesktop.login1")
            .unwrap()
            .serve_at("/org/freedesktop/login1", Manager(closed.clone()))
            .unwrap()
            .build()
            .unwrap();
        let client = zbus::blocking::connection::Builder::address(address.trim())
            .unwrap()
            .method_timeout(Duration::from_secs(1))
            .build()
            .unwrap();
        assert!(read_logind(&client).unwrap());
        closed.store(false, Ordering::Relaxed);
        assert!(!read_logind(&client).unwrap());
        server.release_name("org.freedesktop.login1").unwrap();
        assert!(
            read_logind(&client).is_err(),
            "missing logind must not masquerade as an open lid"
        );
    }

    #[test]
    fn newer_input_wins_over_a_delayed_resume_read() {
        let mut lid = LidState::default();
        lid.observe(true);
        let probe = lid.begin(true, true);
        lid.observe(false);
        assert_eq!(lid.complete(probe, Some(true)), Some(true));
        assert!(!lid.closed());
    }

    #[test]
    fn failed_reads_preserve_known_state_and_unknown_is_not_an_open_observation() {
        let mut lid = LidState::default();
        let probe = lid.begin(false, true);
        lid.complete(probe, None);
        assert_eq!(lid.value, None);
        lid.observe(true);
        let probe = lid.begin(true, true);
        lid.complete(probe, None);
        assert_eq!(lid.value, Some(true));
    }

    #[test]
    fn pause_and_newer_resume_invalidate_old_replies() {
        let mut lid = LidState::default();
        let old = lid.begin(true, true);
        lid.pause();
        assert_eq!(lid.complete(old, Some(true)), None);
        let current = lid.begin(true, true);
        assert_eq!(lid.complete(old, Some(true)), None);
        assert!(lid.pending());
        assert_eq!(lid.complete(current, Some(false)), Some(true));
        assert!(!lid.pending());
    }

    #[test]
    fn listener_recovery_does_not_request_idle_wake_or_reactivation() {
        let mut lid = LidState::default();
        let recovery = lid.begin(false, false);
        assert!(!recovery.wake);
        assert_eq!(lid.complete(recovery, Some(false)), Some(false));
    }

    #[test]
    fn listener_recovery_preserves_a_pending_real_wake() {
        let mut lid = LidState::default();
        let resume = lid.begin(true, true);
        let recovery = lid.begin(false, false);
        assert!(recovery.wake);
        assert_eq!(lid.complete(resume, Some(false)), None);
        assert_eq!(lid.complete(recovery, Some(false)), Some(true));
    }

    #[test]
    fn system_wake_cannot_cancel_pending_session_reactivation() {
        let mut lid = LidState::default();
        lid.begin(true, true);
        let probe = lid.begin(false, true);
        assert_eq!(lid.complete(probe, Some(false)), Some(true));
    }
}
