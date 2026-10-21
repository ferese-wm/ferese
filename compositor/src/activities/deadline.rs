//! Activity deadlines share CLOCK_BOOTTIME with the published countdowns. A
//! monotonic calloop timer would otherwise keep waiting for time spent asleep.
use smithay::reexports::calloop::generic::Generic;
use smithay::reexports::calloop::{Interest, LoopHandle, Mode, PostAction, RegistrationToken};
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

pub(crate) struct Deadline {
    fd: OwnedFd,
    armed: std::cell::Cell<Option<u64>>,
    handle: LoopHandle<'static, crate::Ferese>,
    token: RegistrationToken,
}
impl Deadline {
    pub fn new(
        handle: LoopHandle<'static, crate::Ferese>,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        // SAFETY: timerfd_create returns a new owned descriptor or -1.
        let fd = unsafe {
            libc::timerfd_create(libc::CLOCK_BOOTTIME, libc::TFD_CLOEXEC | libc::TFD_NONBLOCK)
        };
        if fd < 0 {
            return Err(io::Error::last_os_error().into());
        }
        let fd = unsafe { OwnedFd::from_raw_fd(fd) };
        let token = handle.insert_source(
            Generic::new(fd.try_clone()?, Interest::READ, Mode::Level),
            |_, fd, state| {
                let mut expirations = 0_u64;
                // SAFETY: expirations is a writable eight-byte timerfd counter.
                let result =
                    unsafe { libc::read(fd.as_raw_fd(), (&raw mut expirations).cast(), 8) };
                if result < 0 {
                    let error = io::Error::last_os_error();
                    if error.kind() != io::ErrorKind::WouldBlock {
                        return Err(error);
                    }
                } else {
                    state.activities.tick();
                }
                Ok(PostAction::Continue)
            },
        )?;
        Ok(Self {
            fd,
            armed: std::cell::Cell::new(None),
            handle,
            token,
        })
    }
    pub fn update(&self, deadline: Option<u64>) -> io::Result<()> {
        if self.armed.get() == deadline {
            return Ok(());
        }
        let ms = deadline.unwrap_or(0);
        let spec = libc::itimerspec {
            it_interval: libc::timespec {
                tv_sec: 0,
                tv_nsec: 0,
            },
            it_value: libc::timespec {
                tv_sec: (ms / 1_000) as _,
                tv_nsec: ((ms % 1_000) * 1_000_000) as _,
            },
        };
        // SAFETY: spec is initialized; null old_value is permitted.
        if unsafe {
            libc::timerfd_settime(
                self.fd.as_raw_fd(),
                libc::TFD_TIMER_ABSTIME,
                &spec,
                std::ptr::null_mut(),
            )
        } < 0
        {
            Err(io::Error::last_os_error())
        } else {
            self.armed.set(deadline);
            Ok(())
        }
    }
}
impl Drop for Deadline {
    fn drop(&mut self) {
        self.handle.remove(self.token);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn deadline_uses_boottime_and_disarming_leaves_no_periodic_wakeups() {
        let events = smithay::reexports::calloop::EventLoop::<crate::Ferese>::try_new().unwrap();
        let deadline = Deadline::new(events.handle()).unwrap();
        let info =
            std::fs::read_to_string(format!("/proc/self/fdinfo/{}", deadline.fd.as_raw_fd()))
                .unwrap();
        assert!(
            info.lines()
                .any(|line| line.split_whitespace().collect::<Vec<_>>() == ["clockid:", "7"]),
            "{info}"
        );
        deadline
            .update(Some(ferese_ipc::activities::now_ms() + 20))
            .unwrap();
        let mut pollfd = libc::pollfd {
            fd: deadline.fd.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: pollfd is a valid array of one descriptor.
        assert_eq!(unsafe { libc::poll(&raw mut pollfd, 1, 500) }, 1);
        let mut counter = 0_u64;
        assert_eq!(
            unsafe { libc::read(deadline.fd.as_raw_fd(), (&raw mut counter).cast(), 8) },
            8
        );
        assert_eq!(counter, 1);
        deadline.update(None).unwrap();
        assert_eq!(unsafe { libc::poll(&raw mut pollfd, 1, 0) }, 0);
    }
}
