//! Kernel battery/backlight events, with a cancellation fd and no polling timer.
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use super::{PollState, wake_status};

pub(super) struct Monitor {
    _stop: UnixStream,
    alive: Arc<AtomicBool>,
}

fn relevant(message: &[u8]) -> bool {
    message
        .split(|byte| *byte == 0)
        .any(|field| field == b"SUBSYSTEM=backlight" || field == b"SUBSYSTEM=power_supply")
}

impl Monitor {
    pub fn alive(&self) -> bool {
        self.alive.load(Ordering::Acquire)
    }

    pub fn start(wake: Arc<PollState>) -> std::io::Result<Self> {
        // SAFETY: socket has no borrowed pointer arguments; successful fd is owned below.
        let fd = unsafe {
            libc::socket(
                libc::AF_NETLINK,
                libc::SOCK_DGRAM | libc::SOCK_CLOEXEC,
                libc::NETLINK_KOBJECT_UEVENT,
            )
        };
        if fd < 0 {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: the successful socket call returned a unique owned descriptor.
        let fd = unsafe { OwnedFd::from_raw_fd(fd) };
        // SAFETY: zero is a valid initialization for sockaddr_nl including padding.
        let mut address: libc::sockaddr_nl = unsafe { std::mem::zeroed() };
        address.nl_family = libc::AF_NETLINK as u16;
        address.nl_groups = 1;
        // SAFETY: the address pointer and its length describe the initialized struct.
        if unsafe {
            libc::bind(
                fd.as_raw_fd(),
                (&raw const address).cast(),
                std::mem::size_of_val(&address) as _,
            )
        } < 0
        {
            return Err(std::io::Error::last_os_error());
        }
        let (stop, cancelled) = UnixStream::pair()?;
        let alive = Arc::new(AtomicBool::new(true));
        let running = alive.clone();
        std::thread::Builder::new()
            .name("ferese-hardware-events".into())
            .spawn(move || {
                let mut descriptors = [
                    libc::pollfd {
                        fd: fd.as_raw_fd(),
                        events: libc::POLLIN,
                        revents: 0,
                    },
                    libc::pollfd {
                        fd: cancelled.as_raw_fd(),
                        events: libc::POLLIN,
                        revents: 0,
                    },
                ];
                let mut buffer = [0u8; 65536];
                loop {
                    // SAFETY: both descriptors remain owned by this thread; poll writes within the array.
                    let result = unsafe { libc::poll(descriptors.as_mut_ptr(), 2, -1) };
                    if result < 0
                        && std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted
                    {
                        continue;
                    }
                    if result < 0
                        || descriptors[1].revents != 0
                        || descriptors[0].revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL)
                            != 0
                    {
                        break;
                    }
                    let mut length = std::mem::size_of_val(&address) as libc::socklen_t;
                    // SAFETY: the owned fd, writable byte buffer and sockaddr storage are valid for their lengths.
                    let count = unsafe {
                        libc::recvfrom(
                            fd.as_raw_fd(),
                            buffer.as_mut_ptr().cast(),
                            buffer.len(),
                            0,
                            (&raw mut address).cast(),
                            &mut length,
                        )
                    };
                    if count < 0 {
                        break;
                    }
                    if address.nl_pid == 0 && relevant(&buffer[..count as usize]) {
                        wake_status(&wake);
                    }
                }
                running.store(false, Ordering::Release);
                wake_status(&wake);
            })?;
        Ok(Self { _stop: stop, alive })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_battery_and_backlight_events_refresh_status() {
        assert!(relevant(b"change@/devices/panel\0SUBSYSTEM=backlight\0"));
        assert!(relevant(b"SUBSYSTEM=power_supply\0ACTION=change\0"));
        assert!(!relevant(b"SUBSYSTEM=input\0"));
        assert!(!relevant(b"OTHER=SUBSYSTEM=backlight\0"));
    }
}
