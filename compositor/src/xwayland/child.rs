use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, Stdio};

use crate::session_environment::SessionEnvironment;

use super::sockets::Reservation;

#[cfg(test)]
pub(crate) mod test_hook {
    use std::cell::{Cell, RefCell};

    type InChild = Box<dyn FnMut() -> std::io::Result<()> + Send + Sync>;

    thread_local! {
        static IN_CHILD: RefCell<Option<InChild>> = const { RefCell::new(None) };
        /// Group signals this thread's owners actually sent.
        ///
        /// Kept per thread so parallel tests cannot inflate it: an owner whose
        /// leader was reaped must send none, and a test that measures before
        /// and after a shutdown is measuring that owner alone.
        static GROUP_SIGNALS: Cell<usize> = const { Cell::new(0) };
    }

    pub(crate) fn record_group_signal() {
        GROUP_SIGNALS.with(|count| count.set(count.get() + 1));
    }

    pub(crate) fn group_signals() -> usize {
        GROUP_SIGNALS.with(Cell::get)
    }

    /// Arrange an action that must happen in *this* child only, after fork.
    ///
    /// Close-on-exec must be cleared here: the parent is shared with parallel
    /// spawns, so a parent-side change would race them.
    pub(crate) fn set_in_child(hook: impl FnMut() -> std::io::Result<()> + Send + Sync + 'static) {
        IN_CHILD.with(|slot| *slot.borrow_mut() = Some(Box::new(hook)));
    }

    pub(crate) fn take_in_child() -> Option<InChild> {
        IN_CHILD.with(|slot| slot.borrow_mut().take())
    }
}

// Use the public Wayland socket: Satellite passes its environment to Xwayland.
pub(crate) fn spawn_satellite(
    executable: &Path,
    environment: &SessionEnvironment,
    reservation: &Reservation,
    authority: &Path,
    notify_socket: Option<&Path>,
) -> io::Result<Child> {
    if reservation.listeners().is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "cannot launch Satellite without reserved listening sockets",
        ));
    }

    satellite_command(executable, environment, reservation, authority, notify_socket).spawn()
}

fn satellite_command(
    executable: &Path,
    environment: &SessionEnvironment,
    reservation: &Reservation,
    authority: &Path,
    notify_socket: Option<&Path>,
) -> std::process::Command {
    let inherited: Vec<_> = reservation.listeners().iter().map(AsRawFd::as_raw_fd).collect();
    if inherited.is_empty() {
        panic!("cannot build a Satellite command without reserved listening sockets");
    }

    let mut command = crate::process::command(executable);
    environment.apply_public(&mut command);
    command
        .arg(reservation.display_name())
        .arg("-auth")
        .arg(authority)
        .args(["-nolisten", "tcp"])
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .process_group(0);

    for name in [
        "NOTIFY_SOCKET",
        "WATCHDOG_PID",
        "WATCHDOG_USEC",
        "LISTEN_PID",
        "LISTEN_FDS",
        "LISTEN_FDNAMES",
    ] {
        command.env_remove(name);
    }
    if let Some(path) = notify_socket {
        command.env("NOTIFY_SOCKET", path);
    }

    for fd in &inherited {
        command.arg("-listenfd").arg(fd.to_string());
    }

    // PR_SET_PDEATHSIG follows the spawning thread; spawn from the compositor thread.
    let expected_parent = unsafe { libc::getpid() };
    // SAFETY: this child-only hook performs only raw, non-allocating
    // Linux/POSIX operations on prevalidated descriptors. All formatting and
    // allocation occurred above, before fork.
    unsafe {
        command.pre_exec(move || {
            for fd in &inherited {
                let flags = libc::fcntl(*fd, libc::F_GETFD);
                if flags == -1 || libc::fcntl(*fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) == -1 {
                    return Err(io::Error::last_os_error());
                }
            }

            if libc::prctl(
                libc::PR_SET_PDEATHSIG,
                libc::SIGTERM as libc::c_ulong,
                0 as libc::c_ulong,
                0 as libc::c_ulong,
                0 as libc::c_ulong,
            ) == -1
            {
                return Err(io::Error::last_os_error());
            }

            if libc::getppid() != expected_parent {
                return Err(io::Error::from_raw_os_error(libc::ESRCH));
            }

            Ok(())
        });
    }

    // One-shot, thread-local, and consumed by exactly this spawn: the hook
    // changes descriptors in the forked child, never in the parent.
    #[cfg(test)]
    if let Some(hook) = test_hook::take_in_child() {
        // SAFETY: same contract as the hook above: a closure that runs in the
        // forked child before exec, using only prevalidated descriptors and
        // no allocation.
        unsafe {
            command.pre_exec(hook);
        }
    }

    command
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Observation {
    Running,
    Exited,
    Failed(String),
}

pub(crate) struct OwnedProcessGroup {
    pid: u32,
    child: Option<Child>,
    reaped: bool,
    cleanup_error: Option<String>,
}

impl OwnedProcessGroup {
    pub fn adopt(child: Child) -> Self {
        let pid = child.id();
        Self {
            pid,
            child: Some(child),
            reaped: false,
            cleanup_error: None,
        }
    }

    #[cfg(test)]
    fn from_command(mut command: std::process::Command) -> std::io::Result<Self> {
        command.process_group(0);
        Ok(Self::adopt(command.spawn()?))
    }

    pub fn pid(&self) -> u32 {
        self.pid
    }

    pub fn observe(&mut self) -> Observation {
        if self.reaped {
            return Observation::Exited;
        }
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        // SAFETY: `info` is a live, correctly aligned local; P_PID with our own
        // pid, and WNOWAIT|WNOHANG leave the status claimable by `wait`.
        let result = unsafe {
            libc::waitid(
                libc::P_PID,
                self.pid,
                std::ptr::from_mut(&mut info),
                libc::WEXITED | libc::WNOWAIT | libc::WNOHANG,
            )
        };
        if result == -1 {
            // ECHILD means someone else already reaped the leader, so the
            // process-group identity can no longer be trusted.
            return Observation::Failed(format!(
                "cannot observe X11 service {}: {}",
                self.pid,
                io::Error::last_os_error()
            ));
        }
        // si_pid == 0 with WNOHANG means no state change is pending.
        // SAFETY: `info` was filled in by the waitid call above.
        if unsafe { info.si_pid() } == 0 {
            Observation::Running
        } else {
            Observation::Exited
        }
    }

    /// Ask the whole group to stop, but only while this owner still holds the
    /// unreaped leader that names it.
    ///
    /// Once the leader is reaped the group number identifies nothing this
    /// owner controls: it may already belong to an unrelated process group.
    /// Cleanup in that state resumes by *waiting* for the group, never by
    /// signalling it, so a stale number can never kill a stranger.
    pub fn signal_group(&self, signal: i32) {
        if self.reaped {
            // The group number is stale: it may already name an unrelated
            // process group, so resume by waiting and never by signalling.
            return;
        }
        #[cfg(test)]
        test_hook::record_group_signal();
        signal_group(self.pid, signal);
    }

    pub fn group_is_empty(&self) -> bool {
        #[cfg(test)]
        if super::test_hooks::force_group_not_empty() {
            return false;
        }

        // SAFETY: `kill` with signal 0 performs error checking only. A negative
        // pid addresses the process group led by `self.pid`.
        let outcome = unsafe { libc::kill(-(self.pid as i32), 0) };
        if outcome == 0 {
            return false;
        }
        // ESRCH is the only "nothing left" answer; anything else (notably
        // EPERM) means the group still exists and must not be assumed gone.
        std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
    }

    pub fn is_reaped(&self) -> bool {
        self.reaped
    }

    pub fn terminate(&mut self, grace: std::time::Duration) -> Result<Option<std::process::ExitStatus>, String> {
        self.signal_group(libc::SIGTERM);

        let deadline = std::time::Instant::now() + grace;
        loop {
            match self.observe() {
                Observation::Exited => break,
                Observation::Running => {
                    if std::time::Instant::now() >= deadline {
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
                Observation::Failed(error) => return Err(error),
            }
        }

        self.signal_group(libc::SIGKILL);
        // A zombie still counts as a group member, so reap before asking
        // whether a descendant is left.
        let status = self.reap()?;

        let deadline = std::time::Instant::now() + grace;
        while !self.group_is_empty() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        if !self.group_is_empty() {
            // A survivor may still hold the X11 listeners, so report it.
            return Err("the X11 service group survived SIGKILL".to_owned());
        }
        Ok(status)
    }

    pub fn reap(&mut self) -> Result<Option<std::process::ExitStatus>, String> {
        if self.reaped {
            return Ok(None);
        }
        let Some(child) = self.child.as_mut() else {
            self.reaped = true;
            return Ok(None);
        };
        match child.wait() {
            Ok(status) => {
                self.reaped = true;
                Ok(Some(status))
            }
            Err(error) => Err(format!("cannot reap X11 service {}: {error}", self.pid)),
        }
    }

    pub fn cleanup_error(&self) -> Option<&str> {
        self.cleanup_error.as_deref()
    }

    pub fn note_cleanup_error(&mut self, error: String) {
        self.cleanup_error = Some(error);
    }
}

impl Drop for OwnedProcessGroup {
    fn drop(&mut self) {
        if self.reaped {
            return;
        }

        // Last resort: the manager must already have reached a completed
        // cleanup transition, so a process is never silently forgotten.
        self.signal_group(libc::SIGKILL);
        let _ = self.reap();
    }
}

/// Requires an unreaped group leader owned by the caller.
pub(crate) fn signal_group(pid: u32, signal: i32) {
    // SAFETY: negative pid targets the process group led by pid.
    unsafe {
        libc::kill(-(pid as i32), signal);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SessionEnvironment;
    use std::ffi::OsString;
    use std::time::Duration;

    fn environment() -> SessionEnvironment {
        SessionEnvironment {
            control_socket: std::path::PathBuf::from("/run/user/1000/ferese/control.sock"),
            policy: crate::SessionPolicy::Desktop,
            wayland_display: OsString::from("wayland-7"),
            x11: None,
        }
    }

    #[test]
    fn refusing_to_launch_without_owned_listeners() {
        let mut reservation = Reservation::allocate().unwrap();
        reservation.close_listeners();

        let error = spawn_satellite(
            Path::new("/nonexistent/xwayland-satellite"),
            &environment(),
            &reservation,
            Path::new("/nonexistent/auth"),
            None,
        )
        .unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert!(error.to_string().contains("without reserved listening sockets"));
    }

    #[test]
    fn a_missing_executable_is_reported_not_panicked() {
        let reservation = Reservation::allocate().unwrap();
        let error = spawn_satellite(
            Path::new("/nonexistent/xwayland-satellite"),
            &environment(),
            &reservation,
            Path::new("/nonexistent/auth"),
            None,
        )
        .unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::NotFound);
    }

    #[test]
    fn a_stopped_group_reports_itself_empty() {
        let mut command = std::process::Command::new("/bin/sh");
        command.arg("-c").arg("exit 0");
        let mut process = OwnedProcessGroup::from_command(command).expect("spawn /bin/sh");

        assert!(!process.group_is_empty());
        process.signal_group(libc::SIGTERM);
        let status = process.terminate(Duration::from_secs(5)).expect("terminate");

        assert!(status.is_some());
        assert!(process.group_is_empty(), "a reaped group has no members left");
        assert_eq!(process.cleanup_error(), None);
    }

    #[test]
    fn a_descendant_that_ignores_sigterm_is_still_killed() {
        // A leader that exits immediately while a descendant keeps running is
        // the crash shape that leaks X11 listeners into the next generation.
        let mut command = std::process::Command::new("/bin/sh");
        command.arg("-c").arg("sleep 300 & exit 0");
        let mut process = OwnedProcessGroup::from_command(command).expect("spawn /bin/sh");

        // The leader is gone, yet the group still has a member.
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while process.observe() == Observation::Running && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(process.observe(), Observation::Exited);
        assert!(
            !process.group_is_empty(),
            "the surviving descendant keeps the group non-empty"
        );

        process.terminate(Duration::from_secs(5)).expect("terminate");

        assert!(
            process.group_is_empty(),
            "SIGKILL must reclaim a descendant that ignored SIGTERM"
        );
    }

    #[test]
    fn reaping_reports_a_failure_instead_of_panicking() {
        let mut process = OwnedProcessGroup {
            pid: 0,
            child: None,
            reaped: false,
            cleanup_error: None,
        };

        // An owner with no child has nothing left to reap.
        assert_eq!(process.reap().expect("reap"), None);
        assert!(process.reaped);
        // Reaping twice is harmless, so cleanup paths stay simple.
        assert_eq!(process.reap().expect("reap again"), None);
    }

    #[test]
    fn a_reaped_owner_resumes_shutdown_without_signalling_a_stale_group() {
        let mut command = std::process::Command::new("/bin/sh");
        command.arg("-c").arg("exit 0");
        let mut process = OwnedProcessGroup::from_command(command).expect("spawn /bin/sh");
        process.reap().expect("reap the leader");
        assert!(process.is_reaped(), "the leader is reaped");

        // A shutdown that resumes here still owns the cleanup record, but the
        // group number may already name another process group: it must be
        // waited for, never signalled.
        let before = test_hook::group_signals();
        let status = process
            .terminate(Duration::from_secs(5))
            .expect("terminating a reaped owner must not fail");

        assert_eq!(status, None, "there is no leader left to report a status for");
        assert_eq!(
            test_hook::group_signals(),
            before,
            "a reaped owner must resume by waiting, not by signalling its stale group number"
        );
        assert!(process.group_is_empty(), "the group really was reclaimed");
    }

    #[test]
    fn the_command_line_matches_the_verified_cli_contract() {
        let reservation = Reservation::allocate().unwrap();
        let notify = std::env::temp_dir().join("ferese-notify-test");

        let command = satellite_command(
            Path::new("xwayland-satellite"),
            &environment(),
            &reservation,
            Path::new("/run/user/1000/ferese-xauth-abc"),
            Some(notify.as_path()),
        );

        let args: Vec<_> = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();

        assert_eq!(
            args.first().map(String::as_str),
            Some(reservation.display_name().as_str())
        );

        let mut listenfds = Vec::new();
        let mut index = 0;
        while index < args.len() {
            match args[index].as_str() {
                "-listenfd" => {
                    listenfds.push(args[index + 1].clone());
                    index += 2;
                }
                "-auth" => {
                    assert_eq!(args[index + 1], "/run/user/1000/ferese-xauth-abc");
                    index += 2;
                }
                "-nolisten" => {
                    assert_eq!(args[index + 1], "tcp");
                    index += 2;
                }
                _ => index += 1,
            }
        }

        assert!(args.windows(2).any(|pair| pair == ["-nolisten", "tcp"]));
        assert!(!args.iter().any(|arg| arg == "-ac"));

        assert!(!args.iter().any(|arg| arg == "-displayfd"));
        assert_eq!(listenfds.len(), reservation.listeners().len());
    }

    #[test]
    fn inherited_socket_activation_state_is_cleared() {
        let reservation = Reservation::allocate().unwrap();
        let command = satellite_command(
            Path::new("xwayland-satellite"),
            &environment(),
            &reservation,
            Path::new("/auth"),
            None,
        );

        for (key, value) in command.get_envs() {
            let key = key.to_string_lossy();
            if matches!(
                key.as_ref(),
                "LISTEN_PID" | "LISTEN_FDS" | "LISTEN_FDNAMES" | "WATCHDOG_PID" | "WATCHDOG_USEC" | "NOTIFY_SOCKET"
            ) {
                assert_eq!(value, None, "{key} must not be inherited from the launching service");
            }
            if key.as_ref() == "WAYLAND_SOCKET" {
                assert_eq!(
                    value, None,
                    "Satellite must connect by display name, not an inherited socket"
                );
            }
        }
    }

    #[test]
    fn a_private_notify_socket_replaces_the_host_one() {
        let reservation = Reservation::allocate().unwrap();
        let private = std::env::temp_dir().join("ferese-notify-private");
        let command = satellite_command(
            Path::new("xwayland-satellite"),
            &environment(),
            &reservation,
            Path::new("/auth"),
            Some(private.as_path()),
        );

        let notify = command
            .get_envs()
            .find(|(key, _)| key.to_string_lossy() == "NOTIFY_SOCKET")
            .and_then(|(_, value)| value)
            .expect("private NOTIFY_SOCKET must be set");

        assert_eq!(notify, private);
    }
}
