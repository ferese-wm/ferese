//! Supervise the explicitly opted-in desktop helper. Each generation gets new
//! private Wayland connections; a dead generation's descriptors are never reused.
use super::*;
use calloop::RegistrationToken;
use std::ffi::OsString;
use std::time::Instant;

pub(crate) struct Supervisor {
    args: Vec<OsString>,
    capabilities: ClientCapabilities,
    child: Option<Child>,
    source: Option<RegistrationToken>,
    failures: u32,
    started: Instant,
    restarted: bool,
    stopped: bool,
}

impl Supervisor {
    pub(crate) fn start(
        state: &mut Ferese,
        args: Vec<OsString>,
        capabilities: ClientCapabilities,
    ) -> Rc<RefCell<Self>> {
        let owner = Rc::new(RefCell::new(Self {
            args,
            capabilities,
            child: None,
            source: None,
            failures: 0,
            started: Instant::now(),
            restarted: false,
            stopped: false,
        }));
        Self::launch(&owner, state);
        owner
    }

    fn launch(owner: &Rc<RefCell<Self>>, state: &mut Ferese) {
        let mut this = owner.borrow_mut();
        if this.stopped {
            return;
        }
        this.started = Instant::now();
        this.child = spawn_client_inner(state, &this.args, this.capabilities, Some(this.restarted));
        this.restarted |= this.child.is_some();
        let pid = this.child.as_ref().map(Child::id);
        drop(this);
        if let Some(pid) = pid {
            let weak = Rc::downgrade(owner);
            let watched = pidfd(pid).and_then(|fd| {
                state
                    .loop_handle
                    .insert_source(Generic::new(fd, Interest::READ, Mode::Level), move |_, _, state| {
                        if let Some(owner) = weak.upgrade() {
                            owner.borrow_mut().source = None;
                            Self::exited(&owner, state);
                        }
                        Ok(PostAction::Remove)
                    })
                    .map_err(io::Error::other)
            });
            if let Ok(token) = watched {
                owner.borrow_mut().source = Some(token);
                return;
            }
            // pidfd is unavailable on older kernels. Keep child ownership and
            // bounded polling instead of losing restart and shutdown tracking.
            let weak = Rc::downgrade(owner);
            let token = state
                .loop_handle
                .insert_source(Timer::from_duration(Duration::from_millis(250)), move |_, _, state| {
                    let Some(owner) = weak.upgrade() else {
                        return TimeoutAction::Drop;
                    };
                    let done = owner
                        .borrow_mut()
                        .child
                        .as_mut()
                        .is_none_or(|child| child.try_wait().ok().flatten().is_some());
                    if done {
                        owner.borrow_mut().source = None;
                        Self::exited(&owner, state);
                        TimeoutAction::Drop
                    } else {
                        TimeoutAction::ToDuration(Duration::from_millis(250))
                    }
                })
                .expect("register desktop child watcher");
            owner.borrow_mut().source = Some(token);
        } else {
            Self::retry(owner, state);
        }
    }

    fn exited(owner: &Rc<RefCell<Self>>, state: &mut Ferese) {
        let mut this = owner.borrow_mut();
        if let Some(mut child) = this.child.take() {
            // Every supervised generation owns a separate process group. Reap
            // abandoned shell/agent descendants even if the helper was killed.
            kill_group(&child, libc::SIGKILL);
            if let Ok(Some(status)) = child.try_wait() {
                warn!(pid = child.id(), %status, "desktop helper exited");
            }
        }
        if this.started.elapsed() >= Duration::from_secs(60) {
            this.failures = 0;
        }
        drop(this);
        Self::retry(owner, state);
    }

    fn retry(owner: &Rc<RefCell<Self>>, state: &mut Ferese) {
        let mut this = owner.borrow_mut();
        if this.stopped {
            return;
        }
        let delay = Duration::from_secs((1u64 << this.failures.min(5)).min(30));
        this.failures = this.failures.saturating_add(1);
        warn!(?delay, "desktop helper exited; scheduling recovery");
        let weak = Rc::downgrade(owner);
        this.source = Some(
            state
                .loop_handle
                .insert_source(Timer::from_duration(delay), move |_, _, state| {
                    if let Some(owner) = weak.upgrade() {
                        owner.borrow_mut().source = None;
                        Self::launch(&owner, state);
                    }
                    TimeoutAction::Drop
                })
                .expect("register desktop recovery timer"),
        );
    }

    pub(crate) fn stop(&mut self, handle: &LoopHandle<'static, Ferese>) {
        self.stopped = true;
        if let Some(source) = self.source.take() {
            handle.remove(source);
        }
        if let Some(mut child) = self.child.take() {
            // Let the helper run its session cleanup before removing descendants.
            terminate_child(&mut child);
            kill_group(&child, libc::SIGKILL);
        }
    }
}

fn kill_group(child: &Child, signal: i32) {
    // SAFETY: spawn_client_inner creates this child's private process group.
    unsafe {
        libc::kill(-(child.id() as i32), signal);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restarts_with_fresh_connections_and_cancels_pending_recovery_on_stop() {
        exercise_recovery(
            "process::supervisor::tests::restarts_with_fresh_connections_and_cancels_pending_recovery_on_stop",
            false,
        );
    }

    #[test]
    fn polling_recovers_when_pidfd_is_unavailable() {
        exercise_recovery(
            "process::supervisor::tests::polling_recovers_when_pidfd_is_unavailable",
            true,
        );
    }

    fn exercise_recovery(test: &str, polling: bool) {
        if !crate::startup_tests::private_runtime(test) {
            return;
        }
        crate::xwayland::test_hooks::PIDFD_UNAVAILABLE.store(polling, std::sync::atomic::Ordering::SeqCst);
        let mut events = calloop::EventLoop::try_new().unwrap();
        let mut state = crate::startup_tests::state(&mut events);
        let directory = tempfile::tempdir().unwrap();
        let record = directory.path().join("generations");
        let code = r#"import os, sys, time
with open(sys.argv[1], 'a') as output:
 output.write(str(os.fstat(int(os.environ['WAYLAND_SOCKET'])).st_ino) + ' ' + os.environ['FERESE_CLIENT_RESTART'] + '\n')
if os.environ['FERESE_CLIENT_RESTART'] == '1':
 time.sleep(60)
"#;
        let owner = Supervisor::start(
            &mut state,
            vec![
                "python3".into(),
                "-c".into(),
                code.into(),
                record.as_os_str().to_owned(),
            ],
            ClientCapabilities::EFFECTS,
        );
        let deadline = Instant::now() + Duration::from_secs(5);
        let rows = loop {
            events.dispatch(Duration::from_millis(20), &mut state).unwrap();
            let text = std::fs::read_to_string(&record).unwrap_or_default();
            let rows: Vec<_> = text.lines().map(str::to_owned).collect();
            if rows.len() == 2 {
                break rows;
            }
            assert!(Instant::now() < deadline, "supervised child did not restart: {text}");
        };
        assert!(rows[0].ends_with(" 0"));
        assert!(rows[1].ends_with(" 1"));
        assert_ne!(rows[0].split_whitespace().next(), rows[1].split_whitespace().next());
        let child_pid = owner.borrow().child.as_ref().unwrap().id();
        unsafe {
            libc::kill(child_pid as i32, libc::SIGKILL);
        }
        while owner.borrow().child.is_some() {
            events.dispatch(Duration::from_millis(20), &mut state).unwrap();
            assert!(Instant::now() < deadline);
        }
        assert!(owner.borrow().source.is_some(), "restart timer armed");
        owner.borrow_mut().stop(&state.loop_handle);
        let stopped = Instant::now();
        while stopped.elapsed() < Duration::from_millis(2200) {
            events.dispatch(Duration::from_millis(20), &mut state).unwrap();
        }
        assert_eq!(std::fs::read_to_string(record).unwrap().lines().count(), 2);
        assert!(owner.borrow().source.is_none());
    }

    #[test]
    fn stopping_a_live_helper_removes_its_descendants() {
        if !crate::startup_tests::private_runtime(
            "process::supervisor::tests::stopping_a_live_helper_removes_its_descendants",
        ) {
            return;
        }
        let mut events = calloop::EventLoop::try_new().unwrap();
        let mut state = crate::startup_tests::state(&mut events);
        let directory = tempfile::tempdir().unwrap();
        let record = directory.path().join("child-pid");
        let code = r#"import os, signal, subprocess, sys, time
signal.signal(signal.SIGTERM, signal.SIG_IGN)
child = subprocess.Popen(['sleep', '60'])
with open(sys.argv[1], 'w') as output:
 output.write(str(child.pid))
time.sleep(60)
"#;
        let owner = Supervisor::start(
            &mut state,
            vec![
                "python3".into(),
                "-c".into(),
                code.into(),
                record.as_os_str().to_owned(),
            ],
            ClientCapabilities::default(),
        );
        let deadline = Instant::now() + Duration::from_secs(5);
        let pid: u32 = loop {
            if let Ok(contents) = std::fs::read_to_string(&record)
                && let Ok(pid) = contents.parse()
            {
                break pid;
            }
            assert!(Instant::now() < deadline);
            events.dispatch(Duration::from_millis(20), &mut state).unwrap();
        };
        owner.borrow_mut().stop(&state.loop_handle);
        let status_path = format!("/proc/{pid}/status");
        loop {
            // An orphan may briefly remain a zombie until the process reaper runs.
            let running = std::fs::read_to_string(&status_path).is_ok_and(|status| {
                !status
                    .lines()
                    .any(|line| line.starts_with("State:") && line.contains("Z (zombie)"))
            });
            if !running {
                break;
            }
            assert!(Instant::now() < deadline, "helper descendant survived shutdown");
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(owner.borrow().child.is_none());
        assert!(owner.borrow().source.is_none());
    }
}
