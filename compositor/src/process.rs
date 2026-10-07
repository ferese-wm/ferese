use std::cell::RefCell;
use std::ffi::OsStr;
use std::io;
use std::os::fd::{FromRawFd, OwnedFd};
use std::os::unix::process::CommandExt;
use std::process::{Child, Command};
use std::rc::Rc;
use std::time::Duration;

use calloop::generic::Generic;
use calloop::timer::{TimeoutAction, Timer};
use calloop::{Interest, LoopHandle, Mode, PostAction};
use tracing::{info, warn};

use crate::Ferese;
use crate::private_client::{self, ClientCapabilities};

mod supervisor;
pub(crate) use supervisor::Supervisor;

pub(crate) fn command(program: impl AsRef<OsStr>) -> Command {
    let mut command = Command::new(program);
    // Calloop blocks these on the compositor thread; fork/exec inherits that
    // mask. Leave unrelated signal masks alone, including those of the launcher.
    // SAFETY: only async-signal-safe signal-set operations run between fork and exec.
    unsafe {
        command.pre_exec(|| {
            let mut mask = std::mem::zeroed();
            libc::sigemptyset(&mut mask);
            libc::sigaddset(&mut mask, libc::SIGINT);
            libc::sigaddset(&mut mask, libc::SIGTERM);
            if libc::sigprocmask(libc::SIG_UNBLOCK, &mask, std::ptr::null_mut()) == -1 {
                return Err(io::Error::last_os_error());
            }

            Ok(())
        });
    }

    command
}

pub(crate) fn spawn_client<S: AsRef<OsStr>>(
    state: &mut Ferese,
    args: impl IntoIterator<Item = S>,
    capabilities: ClientCapabilities,
) -> Option<Child> {
    spawn_client_inner(state, args, capabilities, None)
}

fn spawn_client_inner<S: AsRef<OsStr>>(
    state: &mut Ferese,
    args: impl IntoIterator<Item = S>,
    capabilities: ClientCapabilities,
    supervised_restart: Option<bool>,
) -> Option<Child> {
    let mut args = args.into_iter();
    let Some(program) = args.next() else {
        info!("no client requested; pass one after `--`, for example `-- foot`");
        return None;
    };

    let program = program.as_ref();
    let mut command = command(program);
    command.args(args);
    if let Some(restarted) = supervised_restart {
        command.process_group(0);
        command.env("FERESE_CLIENT_RESTART", if restarted { "1" } else { "0" });
    }
    // Workers may already exist. Change only this child's environment.
    // Apply before granting the child a private Wayland connection.
    state.session_environment.apply_public(&mut command);
    command.env("FERESE_COMPOSITOR_WALLPAPER", "1");
    let private_connection = if capabilities.is_empty() {
        None
    } else {
        match private_client::prepare_command(state, &mut command, capabilities) {
            Ok(connection) => Some(connection),
            Err(error) => {
                warn!(program = ?program, %error, "failed to create private Wayland connection");
                return None;
            }
        }
    };

    let child = match command.spawn() {
        Ok(child) => {
            info!(program = ?program, pid = child.id(), "spawned Wayland client");
            Some(child)
        }
        Err(error) => {
            warn!(program = ?program, %error, "failed to spawn Wayland client");
            None
        }
    };
    drop(private_connection);
    child
}

pub(crate) fn pidfd(pid: u32) -> io::Result<OwnedFd> {
    #[cfg(test)]
    if crate::xwayland::test_hooks::PIDFD_UNAVAILABLE.load(std::sync::atomic::Ordering::SeqCst) {
        return Err(io::Error::from_raw_os_error(libc::ENOSYS));
    }

    // SAFETY: pidfd_open has no pointer arguments and returns a new owned fd.
    let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }

    // SAFETY: a successful syscall returned a unique valid descriptor.
    Ok(unsafe { OwnedFd::from_raw_fd(fd as i32) })
}

fn reap(child: &RefCell<Option<Child>>) -> bool {
    let mut child = child.borrow_mut();
    let Some(process) = child.as_mut() else {
        return true;
    };

    match process.try_wait() {
        Ok(Some(status)) => {
            info!(pid = process.id(), %status, "Wayland client exited");
            *child = None;
            true
        }
        Ok(None) => false,
        Err(error) => {
            warn!(%error, "cannot reap Wayland client");
            false
        }
    }
}

pub(crate) fn watch_client_exit(handle: &LoopHandle<'static, Ferese>, child: Child) -> Rc<RefCell<Option<Child>>> {
    let pid = child.id();
    let owner = Rc::new(RefCell::new(Some(child)));
    let watched = owner.clone();
    let result = pidfd(pid).and_then(|fd| {
        handle
            .insert_source(Generic::new(fd, Interest::READ, Mode::Level), move |_, _, _| {
                Ok(if reap(&watched) {
                    PostAction::Remove
                } else {
                    PostAction::Continue
                })
            })
            .map_err(io::Error::other)
    });

    if let Err(error) = result {
        warn!(%error, pid, "could not watch client exit with pidfd; polling instead");
        let watched = owner.clone();
        if let Err(error) = handle.insert_source(Timer::from_duration(Duration::from_millis(250)), move |_, _, _| {
            if reap(&watched) {
                TimeoutAction::Drop
            } else {
                TimeoutAction::ToDuration(Duration::from_millis(250))
            }
        }) {
            warn!(%error, pid, "could not schedule client exit polling; stopping child");
            if let Some(mut child) = owner.borrow_mut().take() {
                terminate_child(&mut child);
            }
        }
    }

    owner
}

pub(crate) fn terminate_child(child: &mut Child) {
    if child.try_wait().ok().flatten().is_some() {
        return;
    }

    // SAFETY: the process ID comes from the live Child owned by Ferese.
    unsafe {
        libc::kill(child.id() as i32, libc::SIGTERM);
    }

    for _ in 0..20 {
        if child.try_wait().ok().flatten().is_some() {
            return;
        }

        std::thread::sleep(Duration::from_millis(10));
    }

    let _ = child.kill();
    let _ = child.wait();
}
