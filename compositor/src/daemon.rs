//! Session-owned services. Never grant shell/effects privileges to daemons.
use std::cell::RefCell;
use std::collections::HashMap;
use std::process::Child;
use std::rc::Rc;
use std::time::{Duration, Instant};

use smithay::reexports::calloop::generic::Generic;
use smithay::reexports::calloop::timer::{TimeoutAction, Timer};
use smithay::reexports::calloop::{Interest, Mode, PostAction, RegistrationToken};

use crate::config::DaemonConfig;
use crate::private_client::ClientCapabilities;
use crate::process::{pidfd, spawn_client, terminate_child};
use crate::{Ferese, SessionPolicy};

struct Service {
    config: DaemonConfig,
    child: Option<Child>,
    next_start: Instant,
    finished: bool,
}

pub(crate) struct Runner(
    Vec<Service>,
    HashMap<u32, RegistrationToken>,
    Option<(RegistrationToken, Instant)>,
);

impl Runner {
    pub fn start(state: &mut Ferese) -> Rc<RefCell<Self>> {
        let runner = Rc::new(RefCell::new(Self::new(
            &state.autostart,
            state.session_environment.policy,
        )));
        Self::refresh(&runner, state);
        runner
    }

    pub fn refresh(owner: &Rc<RefCell<Self>>, state: &mut Ferese) {
        let mut runner = owner.borrow_mut();
        runner.tick(state);
        let pids = runner
            .0
            .iter()
            .filter_map(|service| service.child.as_ref().map(Child::id))
            .collect::<Vec<_>>();
        runner.1.retain(|pid, token| {
            if pids.contains(pid) {
                true
            } else {
                state.loop_handle.remove(*token);
                false
            }
        });

        let mut watch_retry = None;
        for pid in pids {
            if runner.1.contains_key(&pid) {
                continue;
            }
            let weak = Rc::downgrade(owner);
            let source = pidfd(pid).map(|fd| Generic::new(fd, Interest::READ, Mode::Level));
            let result = source.and_then(|source| {
                state
                    .loop_handle
                    .insert_source(source, move |_, _, state| {
                        if let Some(owner) = weak.upgrade() {
                            owner.borrow_mut().1.remove(&pid);
                            Self::refresh(&owner, state);
                        }
                        Ok(PostAction::Remove)
                    })
                    .map_err(std::io::Error::other)
            });
            match result {
                Ok(token) => {
                    runner.1.insert(pid, token);
                }
                Err(error) => {
                    tracing::warn!(%error, pid, "could not watch daemon exit; retrying registration");
                    watch_retry = Some(Instant::now() + Duration::from_secs(5));
                }
            }
        }

        let deadline = runner
            .0
            .iter()
            .filter(|service| service.child.is_none() && !service.finished)
            .map(|service| service.next_start)
            .chain(watch_retry)
            .min();
        if runner.2.as_ref().map(|(_, deadline)| *deadline) == deadline {
            return;
        }
        if let Some((token, _)) = runner.2.take() {
            state.loop_handle.remove(token);
        }
        if let Some(deadline) = deadline {
            let weak = Rc::downgrade(owner);
            match state
                .loop_handle
                .insert_source(Timer::from_deadline(deadline), move |_, _, state| {
                    if let Some(owner) = weak.upgrade() {
                        owner.borrow_mut().2 = None;
                        Self::refresh(&owner, state);
                    }
                    TimeoutAction::Drop
                }) {
                Ok(token) => runner.2 = Some((token, deadline)),
                Err(error) => tracing::warn!(%error, "could not arm daemon restart deadline"),
            }
        }
    }

    pub fn reconcile(&mut self, configs: &[DaemonConfig], policy: SessionPolicy) {
        let mut previous = std::mem::take(&mut self.0);
        for config in configs
            .iter()
            .filter(|c| c.enabled && policy.allows_autostart(c.nested))
        {
            if self.0.iter().any(|service| service.config.command == config.command) {
                continue;
            }
            if let Some(index) = previous
                .iter()
                .position(|service| service.config.command == config.command)
            {
                let mut service = previous.remove(index);
                if config.restart && !service.config.restart {
                    service.finished = false;
                }
                service.config = config.clone();
                self.0.push(service);
            } else {
                self.0.push(Service {
                    config: config.clone(),
                    child: None,
                    next_start: Instant::now(),
                    finished: false,
                });
            }
        }

        // TERM/grace/reaping must not pause the compositor's event loop.
        for mut removed in previous {
            if let Some(mut child) = removed.child.take() {
                std::thread::spawn(move || terminate_child(&mut child));
            }
        }
    }

    pub fn new(configs: &[DaemonConfig], policy: SessionPolicy) -> Self {
        Self(
            configs
                .iter()
                .filter(|config| config.enabled && policy.allows_autostart(config.nested))
                .map(|config| Service {
                    config: config.clone(),
                    child: None,
                    next_start: Instant::now(),
                    finished: false,
                })
                .collect(),
            HashMap::new(),
            None,
        )
    }

    pub fn tick(&mut self, state: &mut Ferese) {
        self.reconcile(&state.autostart, state.session_environment.policy);
        let now = Instant::now();
        for service in &mut self.0 {
            if let Some(child) = &mut service.child {
                match child.try_wait() {
                    Ok(Some(status)) => {
                        tracing::info!(command = ?service.config.command, %status, "session daemon exited");
                        service.child = None;
                        service.finished = !service.config.restart;
                        service.next_start = now + Duration::from_secs(5);
                    }
                    Ok(None) => continue,
                    Err(error) => {
                        tracing::warn!(%error, "cannot reap session daemon");
                        continue;
                    }
                }
            }
            if !service.finished && now >= service.next_start {
                service.child = spawn_client(state, service.config.command.iter(), ClientCapabilities::default());
                service.next_start = now + Duration::from_secs(5);
                if service.child.is_none() && !service.config.restart {
                    service.finished = true;
                }
            }
        }
    }

    pub fn stop(&mut self) {
        for service in &mut self.0 {
            if let Some(mut child) = service.child.take() {
                terminate_child(&mut child);
            }
            service.finished = true;
        }
    }
}

impl Drop for Runner {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pidfd_wakes_the_loop_once_and_the_child_is_reaped() {
        let mut child = std::process::Command::new("/bin/sh")
            .args(["-c", "exit 7"])
            .spawn()
            .unwrap();
        let fd = pidfd(child.id()).unwrap();
        let mut events = smithay::reexports::calloop::EventLoop::<usize>::try_new().unwrap();
        events
            .handle()
            .insert_source(Generic::new(fd, Interest::READ, Mode::Level), move |_, _, count| {
                assert_eq!(child.wait().unwrap().code(), Some(7));
                *count += 1;
                Ok(PostAction::Remove)
            })
            .unwrap();
        let mut count = 0;
        events.dispatch(Some(Duration::from_secs(2)), &mut count).unwrap();
        assert_eq!(count, 1);
        events.dispatch(Some(Duration::ZERO), &mut count).unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn live_reconcile_keeps_existing_process_and_changes_policy_without_restart() {
        let mut config = DaemonConfig {
            command: vec!["/bin/sleep".into(), "30".into()],
            enabled: true,
            restart: false,
            nested: false,
        };
        let mut runner = Runner::new(&[config.clone()], SessionPolicy::Desktop);
        runner.0[0].child = Some(std::process::Command::new("/bin/sleep").arg("30").spawn().unwrap());
        let pid = runner.0[0].child.as_ref().unwrap().id();
        config.restart = true;
        runner.reconcile(&[config.clone()], SessionPolicy::Desktop);
        assert_eq!(runner.0[0].child.as_ref().unwrap().id(), pid);
        assert!(runner.0[0].config.restart);
        runner.reconcile(&[config.clone(), config.clone()], SessionPolicy::Desktop);
        assert_eq!(runner.0.len(), 1);
        config.enabled = false;
        runner.reconcile(&[config], SessionPolicy::Desktop);
        assert!(runner.0.is_empty());
        let deadline = Instant::now() + Duration::from_secs(2);
        while unsafe { libc::kill(pid as i32, 0) } == 0 {
            assert!(Instant::now() < deadline, "removed service was not stopped/reaped");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn previews_do_not_duplicate_session_daemons() {
        let configs = vec![DaemonConfig {
            command: vec!["awari".into()],
            enabled: true,
            restart: true,
            nested: false,
        }];
        assert!(Runner::new(&configs, SessionPolicy::Embedded).0.is_empty());
        let mut direct = Runner::new(&configs, SessionPolicy::Desktop);
        assert_eq!(direct.0.len(), 1);
        direct.stop();
        assert!(direct.0[0].finished);
    }

    #[test]
    fn disabled_login_items_never_start_under_either_session_policy() {
        let configs = vec![DaemonConfig {
            command: vec!["not-executed".into()],
            enabled: false,
            restart: true,
            nested: true,
        }];
        assert!(Runner::new(&configs, SessionPolicy::Embedded).0.is_empty());
        assert!(Runner::new(&configs, SessionPolicy::Desktop).0.is_empty());
    }

    #[test]
    fn embedded_autostart_requires_opt_in_on_start_and_reload() {
        let regular = DaemonConfig {
            command: vec!["regular".into()],
            enabled: true,
            restart: false,
            nested: false,
        };
        let embedded = DaemonConfig {
            command: vec!["embedded".into()],
            nested: true,
            ..regular.clone()
        };
        let configs = [regular, embedded.clone()];
        let mut runner = Runner::new(&configs, SessionPolicy::Embedded);
        assert_eq!(runner.0.len(), 1);
        assert_eq!(runner.0[0].config, embedded);
        runner.reconcile(&configs, SessionPolicy::Desktop);
        assert_eq!(runner.0.len(), 2);
        runner.reconcile(&configs, SessionPolicy::Embedded);
        assert_eq!(runner.0.len(), 1);
        assert_eq!(runner.0[0].config, embedded);
    }

    #[test]
    fn desktop_autostart_does_not_depend_on_a_live_drm_backend() {
        if !crate::startup_tests::private_runtime(
            "daemon::tests::desktop_autostart_does_not_depend_on_a_live_drm_backend",
        ) {
            return;
        }
        let mut events = smithay::reexports::calloop::EventLoop::try_new().unwrap();
        let mut state = crate::startup_tests::state(&mut events);
        assert!(state.direct_backend.is_none());
        assert_eq!(state.session_environment.policy, SessionPolicy::Desktop);
        state.autostart = vec![DaemonConfig {
            command: vec!["/bin/sleep".into(), "30".into()],
            enabled: true,
            restart: false,
            nested: false,
        }];
        let runner = Runner::start(&mut state);
        assert_eq!(runner.borrow().0.len(), 1);
        assert!(runner.borrow().0[0].child.is_some());
        runner.borrow_mut().stop();
        state.session_environment.policy = SessionPolicy::Embedded;
        Runner::refresh(&runner, &mut state);
        assert!(runner.borrow().0.is_empty());
    }
}
