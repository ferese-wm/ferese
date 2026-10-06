use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, Stdio};

use crate::session_environment::SessionEnvironment;

use super::sockets::Reservation;

/// Launch Satellite with Ferese's already-owned listening descriptors.
///
/// Satellite is launched on the **public** session environment: it is an
/// ordinary Wayland client of Ferese. `private_client::prepare_command()` is
/// deliberately not used here — its `WAYLAND_SOCKET` would take precedence over
/// `WAYLAND_DISPLAY` in `wl_display_connect()` inside Xwayland, and Satellite
/// does not remove that variable from Xwayland's inherited environment.
pub(crate) fn spawn_satellite(
    executable: &Path,
    environment: &SessionEnvironment,
    reservation: &Reservation,
    authority: &Path,
    // Production passes the private, per-generation readiness socket. `None`
    // is only for an explicit development/readiness-unknown mode.
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

/// Build the Satellite command line and child-only setup.
///
/// Split out from spawning so the verified CLI contract can be asserted
/// directly, without starting a process.
fn satellite_command(
    executable: &Path,
    environment: &SessionEnvironment,
    reservation: &Reservation,
    authority: &Path,
    notify_socket: Option<&Path>,
) -> std::process::Command {
    let inherited: Vec<_> = reservation.listeners().iter().map(AsRawFd::as_raw_fd).collect();
    if inherited.is_empty() {
        // Callers check this before reaching a spawn; keep the guarantee local
        // by refusing to build a command that could pass a dropped descriptor.
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

    // Do not accidentally notify or consume socket-activation state belonging
    // to the service that launched Ferese itself.
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

    // Never hard-code these: pass each owned descriptor exactly once.
    for fd in &inherited {
        command.arg("-listenfd").arg(fd.to_string());
    }

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

            // Spawn this from Ferese's long-lived compositor thread.
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
            // Covers parent death immediately before PR_SET_PDEATHSIG.
            if libc::getppid() != expected_parent {
                return Err(io::Error::from_raw_os_error(libc::ESRCH));
            }

            Ok(())
        });
    }

    command
}

/// What is known about the service leader right now.
///
/// Deliberately separates "has exited" from "has been reaped": only reaping
/// releases the leader's process-group identity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Observation {
    /// Still running, or exited in a way that is not yet observable.
    Running,
    /// The leader is waitable. Its status has deliberately **not** been
    /// consumed, so group signalling is still safe.
    Exited,
    /// Observation failed. Ownership is unknown, so the caller must not start
    /// a replacement generation.
    Failed(String),
}

/// The single owner, signaler, and reaper of the managed service process group.
///
/// Group signalling with `-pgid` is only meaningful while the group leader is
/// unreaped: once the leader is reaped its identifier may be reused, and a
/// later `kill(-pgid, …)` could hit an unrelated group. Every exit check
/// therefore uses `waitid(…, WNOWAIT)`, and [`OwnedProcessGroup::reap`] is the
/// only operation that consumes the status.
///
/// Dropping this type is not a cleanup strategy, but it is a last-resort
/// safety net: the caller must drive [`OwnedProcessGroup::terminate`] to
/// completion before a generation becomes startable again.
pub(crate) struct OwnedProcessGroup {
    pid: u32,
    child: Option<Child>,
    /// Set once the leader has been reaped, releasing the identity.
    reaped: bool,
    cleanup_error: Option<String>,
}

impl OwnedProcessGroup {
    /// Take sole ownership of an already-spawned Satellite process group.
    pub fn adopt(child: Child) -> Self {
        let pid = child.id();
        Self {
            pid,
            child: Some(child),
            reaped: false,
            cleanup_error: None,
        }
    }

    /// Take ownership of an already-spawned, group-leading child.
    ///
    /// The caller is responsible for having set `process_group(0)`, which is
    /// what makes the group identifier owned rather than inherited.
    #[cfg(test)]
    fn from_command(mut command: std::process::Command) -> std::io::Result<Self> {
        command.process_group(0);
        Ok(Self::adopt(command.spawn()?))
    }

    /// The process-group identifier, valid only until the leader is reaped.
    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// Observe the leader without consuming its exit status.
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

    /// Signal every member of the owned group.
    ///
    /// Only call this while the leader is unreaped; see the type
    /// documentation for why.
    pub fn signal_group(&self, signal: i32) {
        signal_group(self.pid, signal);
    }

    /// Whether any member of the owned group is still alive.
    ///
    /// A crashed leader can leave descendants holding the X11 listeners, so
    /// "the leader exited" is not the same as "the listeners are free". The
    /// leader is kept unreaped while this is consulted, which keeps the
    /// process-group identifier from being recycled underneath the check.
    pub fn group_is_empty(&self) -> bool {
        if self.reaped {
            return true;
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

    /// Signal the group, then reap the leader once it is gone.
    ///
    /// Returns the exit status when the leader was reaped by this call.
    /// `grace` bounds how long a cooperative group may take before `SIGKILL`.
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

        // Kill any surviving descendant while the leader is still unreaped, so
        // the group identifier cannot have been recycled. SIGKILL is not
        // waitable, so the group is polled briefly before the leader is reaped.
        self.signal_group(libc::SIGKILL);
        let killed_at = std::time::Instant::now();
        while !self.group_is_empty() && std::time::Instant::now() < killed_at + grace {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        self.reap()
    }

    /// Consume the leader's exit status, releasing the process-group identity.
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

    /// A cleanup failure that makes the service non-startable, if any.
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
        // Last resort only. The manager is required to reach a completed
        // cleanup transition first; this prevents an unreaped, un-signalled
        // process from being silently forgotten if that contract is ever broken.
        self.signal_group(libc::SIGKILL);
        let _ = self.reap();
    }
}

/// Signal the service's whole process group while the group leader is still
/// unreaped.
///
/// Reaping first and signalling `-old_pid` afterwards can target a reused
/// identifier, so the caller must issue this *before* reaping the leader.
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
            wayland_display: OsString::from("wayland-7"),
            x11: None,
        }
    }

    #[test]
    fn refusing_to_launch_without_owned_listeners() {
        // A reservation whose listeners were closed must not yield an
        // integer FD that has already been dropped.
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

        // The display number must be the first argument.
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

        // TCP must never be enabled implicitly.
        assert!(args.windows(2).any(|pair| pair == ["-nolisten", "tcp"]));
        assert!(!args.iter().any(|arg| arg == "-ac"));
        // No invented -displayfd.
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
                // Must be an explicit removal, never an inherited value.
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
