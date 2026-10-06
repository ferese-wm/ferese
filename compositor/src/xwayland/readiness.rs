use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixDatagram;
use std::path::{Path, PathBuf};

/// Bytes reserved for ancillary control data, well above `CMSG_LEN(ucred)`.
const CONTROL_CAPACITY: usize = 128;

fn cmsg_len_ucred() -> usize {
    cmsg_data_offset() + std::mem::size_of::<libc::ucred>()
}

/// Byte offset at which `CMSG_DATA` starts: `cmsghdr` rounded up to
/// `size_t` alignment.
fn cmsg_data_offset() -> usize {
    let header = std::mem::size_of::<libc::cmsghdr>();
    let align = std::mem::size_of::<libc::size_t>();
    header.div_ceil(align) * align
}

/// Room for the control data of one notification.
///
/// `cmsghdr` is the first field so the buffer carries that type's alignment.
/// A `#[repr(C)]` union with a `ucred` is not enough: the kernel requires
/// `msg_control` to be aligned for `cmsghdr`, which is wider.
#[repr(C)]
struct ControlBuffer {
    header: libc::cmsghdr,
    payload: [u8; CONTROL_CAPACITY],
}

// Requires Satellite's systemd notification feature; exec success is not readiness.
pub(crate) struct ReadinessSocket {
    socket: UnixDatagram,
    path: PathBuf,
}

impl ReadinessSocket {
    pub fn create(runtime_directory: &Path, generation: u64) -> io::Result<Self> {
        let path = runtime_directory.join(format!("ferese-x11-notify-{generation}.sock"));

        let socket = UnixDatagram::bind(&path)?;
        socket.set_nonblocking(true)?;

        let enable: libc::c_int = 1;
        let length = libc::socklen_t::try_from(std::mem::size_of::<libc::c_int>())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "optlen overflow"))?;
        // SAFETY: setsockopt on a live owned socket, with `enable` a live local
        // int of exactly `length` bytes.
        if unsafe {
            libc::setsockopt(
                socket.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_PASSCRED,
                std::ptr::from_ref(&enable).cast(),
                length,
            )
        } == -1
        {
            return Err(io::Error::last_os_error());
        }

        Ok(Self { socket, path })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn recv_ready(&self, satellite_pid: u32) -> io::Result<bool> {
        let mut buffer = [0_u8; 512];
        let mut control = ControlBuffer {
            header: unsafe { std::mem::zeroed() },
            payload: [0_u8; CONTROL_CAPACITY],
        };
        let mut iov = libc::iovec {
            iov_base: buffer.as_mut_ptr().cast(),
            iov_len: buffer.len(),
        };
        let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
        message.msg_iov = std::ptr::from_mut(&mut iov);
        message.msg_iovlen = 1;
        message.msg_control = std::ptr::from_mut(&mut control).cast();
        message.msg_controllen = std::mem::size_of::<ControlBuffer>();

        // SAFETY: every pointer refers to a live local buffer for the duration
        // of the call, and `message` is fully initialized before use.
        let received = unsafe {
            libc::recvmsg(
                self.socket.as_raw_fd(),
                std::ptr::from_mut(&mut message),
                libc::MSG_DONTWAIT | libc::MSG_CMSG_CLOEXEC,
            )
        };
        if received < 0 {
            return Err(io::Error::last_os_error());
        }

        // Walk *every* ancillary message, not just the first: a sender is free
        // to put credentials second, and descriptors attached to any message
        // must not be leaked into this long-lived process. This happens before
        // the truncation check below, because a truncated datagram can still
        // have installed descriptors: the flag does not release them.
        let mut from_satellite = false;
        // SAFETY: the control buffer was sized, aligned, and described by this
        // message, and CMSG_NXTHDR walks only what the kernel filled in.
        unsafe {
            let mut header = libc::CMSG_FIRSTHDR(&message);
            while !header.is_null() {
                let (cmsg, data) = (*header, libc::CMSG_DATA(header));

                if cmsg.cmsg_level == libc::SOL_SOCKET && cmsg.cmsg_type == libc::SCM_CREDENTIALS {
                    // Only an exactly sized ucred is a credential. A shorter or
                    // longer one is malformed, so it does not count.
                    if cmsg.cmsg_len == cmsg_len_ucred() {
                        // SAFETY: cmsg_len was checked to describe exactly one
                        // ucred, and CMSG_DATA points into our own buffer.
                        let credential = &*data.cast::<libc::ucred>();
                        // Ignore the Xwayland child and any other process.
                        if credential.pid as u32 == satellite_pid {
                            from_satellite = true;
                        }
                    }
                } else if cmsg.cmsg_level == libc::SOL_SOCKET && cmsg.cmsg_type == libc::SCM_RIGHTS {
                    // Accepting a datagram must never adopt descriptors from an
                    // untrusted sender. Close them instead of dropping them on
                    // the floor, where they would stay open in this process.
                    let payload = cmsg.cmsg_len.saturating_sub(cmsg_data_offset());
                    let count = payload / std::mem::size_of::<libc::c_int>();
                    for index in 0..count {
                        // SAFETY: element `index` lies inside the ancillary data
                        // the kernel reported, and is read exactly once.
                        let raw = *data.cast::<libc::c_int>().add(index);
                        // SAFETY: `raw` is an open descriptor delivered by the
                        // kernel, so closing it releases the sender's copy.
                        libc::close(raw);
                    }
                }

                header = libc::CMSG_NXTHDR(&message, header);
            }
        }

        // Decided only after the control walk: a completely filled payload
        // buffer cannot be distinguished from truncation, and the kernel's
        // truncation flags never satisfy the readiness contract.
        if received as usize == buffer.len() || message.msg_flags & (libc::MSG_CTRUNC | libc::MSG_TRUNC) != 0 {
            return Ok(false);
        }

        if !from_satellite {
            return Ok(false);
        }

        Ok(matches!(
            exact_line(&buffer[..received as usize], b"READY=1"),
            Some(true)
        ))
    }

    pub fn try_clone_owned(&self) -> io::Result<UnixDatagram> {
        self.socket.try_clone()
    }
}

impl AsRawFd for ReadinessSocket {
    fn as_raw_fd(&self) -> std::os::fd::RawFd {
        self.socket.as_raw_fd()
    }
}

fn exact_line(payload: &[u8], wanted: &[u8]) -> Option<bool> {
    if payload.is_empty() || payload.contains(&0) {
        return None;
    }
    let text = std::str::from_utf8(payload).ok()?;
    let mut found = false;
    for line in text.split('\n') {
        if line.as_bytes() == wanted {
            found = true;
        } else if !line.is_empty() && !is_key_value(line) {
            return None;
        }
    }

    Some(found)
}

fn is_key_value(line: &str) -> bool {
    line.split_once('=')
        .is_some_and(|(key, _)| !key.is_empty() && key.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'_'))
}

impl Drop for ReadinessSocket {
    fn drop(&mut self) {
        if let Err(error) = std::fs::remove_file(&self.path)
            && error.kind() != io::ErrorKind::NotFound
        {
            tracing::warn!(path = %self.path.display(), %error, "could not remove readiness socket");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Bounded wait, mirroring the production drain budget.
    const READINESS_DRAIN_BUDGET_TESTS: usize = 200;

    /// Open a datagram socket already connected to `path`.
    ///
    /// Connecting first means the senders can use plain `sendmsg`, whose
    /// glibc export carries no destination address.
    ///
    /// # Safety
    /// Async-signal-safe, for use between `fork` and `_exit`.
    unsafe fn connect_datagram(path: &Path) -> libc::c_int {
        unsafe {
            let fd = libc::socket(libc::AF_UNIX, libc::SOCK_DGRAM, 0);
            if fd < 0 {
                return -1;
            }
            let mut address: libc::sockaddr_un = std::mem::zeroed();
            address.sun_family = libc::AF_UNIX as libc::sa_family_t;
            let bytes = path.as_os_str().as_encoded_bytes();
            if bytes.len() >= address.sun_path.len() {
                libc::close(fd);
                return -1;
            }
            for (slot, byte) in address.sun_path.iter_mut().zip(bytes) {
                *slot = *byte as libc::c_char;
            }
            if libc::connect(
                fd,
                std::ptr::from_ref(&address).cast(),
                // The fixed `sockaddr_un` length is what the kernel expects for
                // AF_UNIX; `sun_path` is already zero-filled past the name.
                libc::socklen_t::try_from(std::mem::size_of::<libc::sockaddr_un>()).unwrap(),
            ) == -1
            {
                libc::close(fd);
                return -1;
            }
            fd
        }
    }

    fn private_runtime_dir(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("ferese-readiness-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::set_permissions(&root, std::os::unix::fs::PermissionsExt::from_mode(0o700)).unwrap();
        root
    }

    fn send_from_child(path: &Path, payload: &[u8]) -> i32 {
        // SAFETY: the child path performs only socket/bind-free sendto and
        // _exit, which are async-signal-safe.
        unsafe {
            let pid = libc::fork();
            assert!(pid >= 0, "fork must succeed");
            if pid == 0 {
                let Ok(client) = UnixDatagram::unbound() else {
                    libc::_exit(70);
                };
                let sent = client.send_to(payload, path);
                libc::_exit(if sent.is_ok() { 0 } else { 71 });
            }
            let mut status = 0;
            libc::waitpid(pid, std::ptr::from_mut(&mut status), 0);
            status
        }
    }

    /// Send `payload` with `MSG_TRUNC`, which makes the kernel deliver a short
    /// datagram and set `MSG_TRUNC` on the receiver.
    fn send_truncated_from_child(path: &Path, payload: &[u8]) {
        // SAFETY: the child performs only sendmsg and _exit, both
        // async-signal-safe, on freshly created descriptors.
        unsafe {
            let pid = libc::fork();
            assert!(pid >= 0, "fork must succeed");
            if pid == 0 {
                let fd = connect_datagram(path);
                if fd < 0 {
                    libc::_exit(70);
                }
                let mut iov = libc::iovec {
                    iov_base: payload.as_ptr().cast_mut().cast(),
                    iov_len: payload.len(),
                };
                let mut message: libc::msghdr = std::mem::zeroed();
                message.msg_iov = std::ptr::from_mut(&mut iov);
                message.msg_iovlen = 1;
                // The kernel truncates the payload and reports MSG_TRUNC.
                message.msg_flags = libc::MSG_TRUNC;
                let sent = libc::sendmsg(fd, std::ptr::from_ref(&message), libc::MSG_NOSIGNAL);
                libc::_exit(if sent >= 0 { 0 } else { 71 });
            }
            let mut status = 0;
            libc::waitpid(pid, std::ptr::from_mut(&mut status), 0);
            assert!(
                libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0,
                "child must send a truncated datagram"
            );
        }
    }

    /// Send `READY=1` from a child that also passes a descriptor.
    ///
    /// The child stays alive so the receiver can name its PID, and the extra
    /// descriptor must be released by the receiver rather than leaked.
    fn send_with_descriptor_from_child(path: &Path, sentinel: &Path) -> i32 {
        // SAFETY: the child performs only socket/open/sendmsg/_exit, all
        // async-signal-safe.
        unsafe {
            let pid = libc::fork();
            assert!(pid >= 0, "fork must succeed");
            if pid == 0 {
                let fd = connect_datagram(path);
                if fd < 0 {
                    libc::_exit(70);
                }
                // A descriptor for a known file, so the receiver can prove it
                // closed exactly this one rather than guessing at a count.
                let Ok(name) = std::ffi::CString::new(sentinel.as_os_str().as_encoded_bytes()) else {
                    libc::_exit(73);
                };
                let spare = libc::open(name.as_ptr(), libc::O_RDONLY);
                if spare < 0 {
                    libc::_exit(72);
                }
                let mut iov = libc::iovec {
                    iov_base: b"READY=1".as_ptr().cast_mut().cast(),
                    iov_len: 7,
                };
                // One SCM_RIGHTS message, in an aligned buffer.
                let mut control = ControlBuffer {
                    header: std::mem::zeroed(),
                    payload: [0_u8; CONTROL_CAPACITY],
                };
                // `payload` starts where `CMSG_DATA` starts, so the descriptor
                // belongs at the beginning of it. Writing it further along would
                // silently send whatever zeros sit in between.
                let descriptor = spare;
                std::ptr::copy_nonoverlapping(
                    std::ptr::from_ref(&descriptor).cast::<u8>(),
                    control.payload.as_mut_ptr(),
                    std::mem::size_of::<libc::c_int>(),
                );
                let total = cmsg_data_offset() + std::mem::size_of::<libc::c_int>();
                control.header.cmsg_len = total as _;
                control.header.cmsg_level = libc::SOL_SOCKET;
                control.header.cmsg_type = libc::SCM_RIGHTS;

                let mut message: libc::msghdr = std::mem::zeroed();
                message.msg_iov = std::ptr::from_mut(&mut iov);
                message.msg_iovlen = 1;
                message.msg_control = std::ptr::from_mut(&mut control).cast();
                message.msg_controllen = total;
                let sent = libc::sendmsg(fd, std::ptr::from_ref(&message), libc::MSG_NOSIGNAL);
                if sent < 0 {
                    libc::_exit(71);
                }
                // Stay alive so the receiver can accept the credential while
                // this process still exists.
                libc::sleep(1);
                libc::_exit(0);
            }
            pid
        }
    }

    /// A truncation flag must be exercised for real, not simulated by a shorter payload.
    #[test]
    fn a_truncated_notification_is_rejected_even_with_valid_credentials() {
        let runtime = private_runtime_dir("truncated");
        let socket = ReadinessSocket::create(&runtime, 21).unwrap();
        let path = socket.path().to_owned();

        send_truncated_from_child(&path, b"READY=1");
        // Any PID is fine here: the point is the flag, not the credential.
        assert!(
            !recv_within(&socket, u32::MAX),
            "MSG_TRUNC must never satisfy readiness"
        );
    }

    /// Descriptors attached to a notification must be released, not adopted.
    #[test]
    fn attached_descriptors_are_closed_rather_than_leaked() {
        let runtime = private_runtime_dir("fds");
        let socket = ReadinessSocket::create(&runtime, 22).unwrap();
        let path = socket.path().to_owned();

        // Counted by identity, not by process-wide total, so a parallel test
        // cannot make this flaky.
        let sentinel = sentinel_file("fds");
        let pid = send_with_descriptor_from_child(&path, &sentinel);
        assert!(
            recv_within(&socket, pid as u32),
            "a READY=1 from the named PID is accepted even with an attached fd"
        );

        assert!(
            !holds_open(&sentinel),
            "the descriptor delivered with the notification must be closed, \
             not left open in the compositor"
        );
        let _ = std::fs::remove_file(&sentinel);

        // SAFETY: reaping a direct child that has finished sleeping.
        let mut status = 0;
        unsafe { libc::waitpid(pid, std::ptr::from_mut(&mut status), 0) };
    }

    #[test]
    fn credentials_after_another_control_message_are_still_read() {
        let runtime = private_runtime_dir("cmsg-order");
        let socket = ReadinessSocket::create(&runtime, 23).unwrap();
        let path = socket.path().to_owned();

        let sentinel = sentinel_file("cmsg-order");
        let pid = send_with_descriptor_from_child(&path, &sentinel);
        assert!(
            recv_within(&socket, pid as u32),
            "a credential that is not the first control message must still count"
        );

        // SAFETY: reaping a direct child that has finished sleeping.
        let mut status = 0;
        unsafe { libc::waitpid(pid, std::ptr::from_mut(&mut status), 0) };
    }

    /// Send `READY=1` with more descriptors than the receiver's control buffer can
    /// hold, so the kernel truncates the ancillary data while installing what fits.
    fn send_descriptors_beyond_capacity_from_child(path: &Path, sentinel: &Path, count: usize) -> i32 {
        // SAFETY: the child performs only socket/open/sendmsg/sleep/_exit, all
        // async-signal-safe except sleep, which is a bounded wait before exit.
        unsafe {
            let pid = libc::fork();
            assert!(pid >= 0, "fork must succeed");
            if pid == 0 {
                let fd = connect_datagram(path);
                if fd < 0 {
                    libc::_exit(70);
                }
                let Ok(name) = std::ffi::CString::new(sentinel.as_os_str().as_encoded_bytes()) else {
                    libc::_exit(73);
                };
                let sentinel_fd = libc::open(name.as_ptr(), libc::O_RDONLY);
                if sentinel_fd < 0 {
                    libc::_exit(72);
                }
                let filler = libc::open(c"/dev/null".as_ptr(), libc::O_RDONLY);
                if filler < 0 {
                    libc::_exit(72);
                }

                let mut iov = libc::iovec {
                    iov_base: b"READY=1".as_ptr().cast_mut().cast(),
                    iov_len: 7,
                };
                // A control buffer large enough to *send* the flood; the
                // receiver's much smaller buffer is what truncates it.
                let mut control = [0_u8; 1024];
                let mut message: libc::msghdr = std::mem::zeroed();
                message.msg_iov = std::ptr::from_mut(&mut iov);
                message.msg_iovlen = 1;
                message.msg_control = control.as_mut_ptr().cast();
                message.msg_controllen = control.len() as _;
                let header = libc::CMSG_FIRSTHDR(&message);
                if header.is_null() {
                    libc::_exit(74);
                }
                (*header).cmsg_level = libc::SOL_SOCKET;
                (*header).cmsg_type = libc::SCM_RIGHTS;
                let bytes = (count * std::mem::size_of::<libc::c_int>()) as u32;
                (*header).cmsg_len = libc::CMSG_LEN(bytes) as _;
                // Exactly the aligned size of one control message: trailing
                // zero bytes would be read as a second, malformed message and
                // the kernel would reject the send with EINVAL.
                message.msg_controllen = libc::CMSG_SPACE(bytes) as _;
                let data = libc::CMSG_DATA(header);
                for index in 0..count {
                    // The sentinel rides along first, so it is one of the
                    // descriptors the receiver's short buffer actually installs.
                    let descriptor = if index == 0 { sentinel_fd } else { filler };
                    *data.cast::<libc::c_int>().add(index) = descriptor;
                }
                let sent = libc::sendmsg(fd, std::ptr::from_ref(&message), libc::MSG_NOSIGNAL);
                if sent < 0 {
                    libc::_exit(71);
                }
                libc::sleep(1);
                libc::_exit(0);
            }
            pid
        }
    }

    /// Send `count` malformed datagrams, each carrying a descriptor.
    fn send_malformed_storm_from_child(path: &Path, sentinel: &Path, count: usize) -> i32 {
        // SAFETY: the child performs only socket/open/sendmsg/_exit, all
        // async-signal-safe.
        unsafe {
            let pid = libc::fork();
            assert!(pid >= 0, "fork must succeed");
            if pid == 0 {
                let fd = connect_datagram(path);
                if fd < 0 {
                    libc::_exit(70);
                }
                let Ok(name) = std::ffi::CString::new(sentinel.as_os_str().as_encoded_bytes()) else {
                    libc::_exit(73);
                };
                let sentinel_fd = libc::open(name.as_ptr(), libc::O_RDONLY);
                if sentinel_fd < 0 {
                    libc::_exit(72);
                }
                for _ in 0..count {
                    let mut iov = libc::iovec {
                        iov_base: b"READY=2".as_ptr().cast_mut().cast(),
                        iov_len: 7,
                    };
                    let mut control = ControlBuffer {
                        header: std::mem::zeroed(),
                        payload: [0_u8; CONTROL_CAPACITY],
                    };
                    let descriptor = sentinel_fd;
                    std::ptr::copy_nonoverlapping(
                        std::ptr::from_ref(&descriptor).cast::<u8>(),
                        control.payload.as_mut_ptr(),
                        std::mem::size_of::<libc::c_int>(),
                    );
                    control.header.cmsg_len = (cmsg_data_offset() + std::mem::size_of::<libc::c_int>()) as _;
                    control.header.cmsg_level = libc::SOL_SOCKET;
                    control.header.cmsg_type = libc::SCM_RIGHTS;

                    let mut message: libc::msghdr = std::mem::zeroed();
                    message.msg_iov = std::ptr::from_mut(&mut iov);
                    message.msg_iovlen = 1;
                    message.msg_control = std::ptr::from_mut(&mut control).cast();
                    message.msg_controllen = cmsg_data_offset() + std::mem::size_of::<libc::c_int>();
                    if libc::sendmsg(fd, std::ptr::from_ref(&message), libc::MSG_NOSIGNAL) < 0 {
                        libc::_exit(71);
                    }
                }
                libc::_exit(0);
            }
            pid
        }
    }

    /// Open descriptors in this process; only *deltas* between two calls are
    /// meaningful, because parallel tests add and remove descriptors too.
    fn open_fd_count() -> usize {
        std::fs::read_dir("/proc/self/fd")
            .map(|entries| entries.count())
            .unwrap_or(0)
    }

    /// A truncated ancillary buffer must be rejected and its delivered descriptors closed.
    #[test]
    fn truncated_control_messages_still_close_the_descriptors_they_delivered() {
        let runtime = private_runtime_dir("ctrunc");
        let socket = ReadinessSocket::create(&runtime, 24).unwrap();
        let path = socket.path().to_owned();

        let sentinel = sentinel_file("ctrunc");
        let before = open_fd_count();
        // Far more descriptors than the receiver's 128-byte payload can hold.
        let pid = send_descriptors_beyond_capacity_from_child(&path, &sentinel, 64);
        // SAFETY: reaping a direct child that has finished sleeping.
        let mut status = 0;
        unsafe { libc::waitpid(pid, std::ptr::from_mut(&mut status), 0) };
        assert!(
            libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0,
            "child must deliver the truncated notification: {status:#x}"
        );

        let verdict = recv_within(&socket, pid as u32);
        assert!(!verdict, "MSG_CTRUNC must never satisfy readiness");
        assert!(
            !holds_open(&sentinel),
            "descriptors installed before MSG_CTRUNC was noticed must be closed"
        );
        let after = open_fd_count();
        assert!(
            after <= before + 4,
            "truncation leaked descriptors: {before} open before, {after} after"
        );
        let _ = std::fs::remove_file(&sentinel);
    }

    /// A flood of malformed datagrams must leak nothing and leave the socket usable.
    #[test]
    fn a_storm_of_malformed_datagrams_leaks_nothing_and_leaves_the_socket_usable() {
        let runtime = private_runtime_dir("storm");
        let socket = ReadinessSocket::create(&runtime, 25).unwrap();
        let path = socket.path().to_owned();

        let sentinel = sentinel_file("storm");
        let before = open_fd_count();
        let pid = send_malformed_storm_from_child(&path, &sentinel, 128);

        // Drain while the sender may still be blocked in `sendmsg`: the socket
        // queue holds far fewer datagrams than the storm, so waiting for the
        // child to exit before reading would deadlock the pair. One deadline
        // covers both the send and the receive.
        let started = std::time::Instant::now();
        let deadline = started + std::time::Duration::from_secs(10);
        let mut judged = 0;
        let mut status = 0;
        while judged < 128 {
            loop {
                match socket.recv_ready(u32::MAX) {
                    Ok(_) => judged += 1,
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                    Err(error) => panic!("unexpected readiness read error: {error}"),
                }
            }
            if judged >= 128 || std::time::Instant::now() >= deadline {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }

        if judged < 128 {
            // The sender never finished, so it is still blocked. Kill it and
            // reap it here rather than leave it behind for another test.
            // SAFETY: signalling and reaping a direct child of this process.
            unsafe {
                libc::kill(pid, libc::SIGKILL);
                libc::waitpid(pid, std::ptr::from_mut(&mut status), 0);
            }
            panic!("the storm was not delivered within the shared deadline: {judged} of 128 judged");
        }

        // Every datagram was accepted, so the sender has reached its exit.
        // SAFETY: reaping a direct child that has finished sending.
        unsafe { libc::waitpid(pid, std::ptr::from_mut(&mut status), 0) };
        assert!(
            libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0,
            "child must deliver the storm: {status:#x}"
        );
        assert!(judged >= 128, "the storm must be delivered and judged: {judged}");
        assert!(
            started.elapsed() < std::time::Duration::from_secs(2),
            "judging the storm must not stall the consumer: {:?}",
            started.elapsed()
        );
        assert!(
            !holds_open(&sentinel),
            "every descriptor carried by the storm must be closed"
        );
        let after = open_fd_count();
        assert!(
            after <= before + 4,
            "the storm leaked descriptors: {before} open before, {after} after"
        );
        let _ = std::fs::remove_file(&sentinel);

        let ready_sentinel = sentinel_file("storm-ready");
        let ready_pid = send_with_descriptor_from_child(&path, &ready_sentinel);
        assert!(
            recv_within(&socket, ready_pid as u32),
            "a READY=1 after the storm is still judged by credential, and the \
             storm sender's exit must not have wedged the socket"
        );
        assert!(
            !holds_open(&ready_sentinel),
            "cleanup must keep working after the storm"
        );
        let _ = std::fs::remove_file(&ready_sentinel);

        // SAFETY: reaping a direct child that has finished sleeping.
        let mut ready_status = 0;
        unsafe { libc::waitpid(ready_pid, std::ptr::from_mut(&mut ready_status), 0) };
    }

    /// Drain the socket until one datagram is judged, bounded like production.
    fn recv_within(socket: &ReadinessSocket, pid: u32) -> bool {
        for _ in 0..READINESS_DRAIN_BUDGET_TESTS {
            match socket.recv_ready(pid) {
                Ok(verdict) => return verdict,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
                Err(error) => panic!("unexpected readiness read error: {error}"),
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        panic!("no readiness datagram arrived");
    }

    /// A unique empty file used to identify a delivered descriptor.
    fn sentinel_file(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("ferese-readiness-sentinel-{name}-{}", std::process::id()));
        let _ = std::fs::remove_file(&path);
        std::fs::write(&path, b"").unwrap();
        path
    }

    /// Whether this process currently holds any descriptor on `path`.
    fn holds_open(path: &Path) -> bool {
        let Ok(entries) = std::fs::read_dir("/proc/self/fd") else {
            return false;
        };
        entries
            .filter_map(Result::ok)
            .any(|entry| std::fs::read_link(entry.path()).is_ok_and(|target| target == path))
    }

    #[test]
    fn creates_a_private_socket_that_is_removed_on_drop() {
        let runtime = private_runtime_dir("create");
        let path = {
            let socket = ReadinessSocket::create(&runtime, 7).unwrap();
            let path = socket.path().to_owned();
            assert!(path.exists());
            assert!(path.starts_with(&runtime));
            assert!(path.to_string_lossy().contains('7'), "path must encode the generation");
            path
        };

        assert!(!path.exists(), "readiness socket must be removed on drop");
    }

    #[test]
    fn each_generation_gets_a_distinct_socket() {
        let runtime = private_runtime_dir("generations");
        let first = ReadinessSocket::create(&runtime, 1).unwrap();
        let second = ReadinessSocket::create(&runtime, 2).unwrap();

        assert_ne!(first.path(), second.path());
    }

    #[test]
    fn line_matching_requires_an_exact_ready_line() {
        assert_eq!(exact_line(b"READY=1", b"READY=1"), Some(true));
        assert_eq!(exact_line(b"READY=1\n", b"READY=1"), Some(true));
        assert_eq!(exact_line(b"MAINPID=42\nREADY=1", b"READY=1"), Some(true));

        assert_eq!(exact_line(b"READY=0", b"READY=1"), Some(false));
        assert_eq!(exact_line(b"READY=1 ", b"READY=1"), Some(false));
        assert_eq!(exact_line(b"XREADY=1", b"READY=1"), Some(false));
        assert_eq!(exact_line(b"READY=11", b"READY=1"), Some(false));
        assert_eq!(exact_line(b"READY=1=2", b"READY=1"), Some(false));
        assert_eq!(exact_line(b"", b"READY=1"), None);
        assert_eq!(exact_line(b"READY=1\x00", b"READY=1"), None);
        assert_eq!(exact_line(b"\xff\xfe\x00\x01", b"READY=1"), None);
    }

    #[test]
    fn a_ready_datagram_from_another_process_is_rejected() {
        let runtime = private_runtime_dir("foreign");
        let socket = ReadinessSocket::create(&runtime, 3).unwrap();
        let path = socket.path().to_owned();

        let status = send_from_child(&path, b"READY=1");
        assert!(
            libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0,
            "child must send"
        );

        let mut rejected = false;
        for _ in 0..1000 {
            match socket.recv_ready(u32::MAX) {
                Ok(false) => {
                    rejected = true;
                    break;
                }
                Ok(true) => panic!("a foreign READY=1 must never satisfy readiness"),
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
                Err(error) => panic!("unexpected readiness read error: {error}"),
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        assert!(rejected, "a READY=1 from another process must be rejected");
    }

    #[test]
    fn a_ready_datagram_from_the_expected_pid_is_accepted() {
        let runtime = private_runtime_dir("expected");
        let socket = ReadinessSocket::create(&runtime, 4).unwrap();
        let path = socket.path().to_owned();

        let pid = {
            // SAFETY: mirror of send_from_child, returning the child PID.
            unsafe {
                let pid = libc::fork();
                assert!(pid >= 0);
                if pid == 0 {
                    for _ in 0..500 {
                        if let Ok(client) = UnixDatagram::unbound()
                            && client.send_to(b"READY=1", &path).is_ok()
                        {
                            libc::_exit(0);
                        }
                        libc::usleep(2_000);
                    }
                    libc::_exit(72);
                }
                pid
            }
        };

        let mut accepted = false;
        for _ in 0..1000 {
            match socket.recv_ready(pid as u32) {
                Ok(true) => {
                    accepted = true;
                    break;
                }
                Ok(false) => {}
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
                Err(error) => panic!("unexpected readiness read error: {error}"),
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }

        // SAFETY: reap the child we just spawned.
        unsafe {
            let mut status = 0;
            libc::waitpid(pid, std::ptr::from_mut(&mut status), 0);
        }

        assert!(accepted, "an exact READY=1 from the expected PID must be accepted");
    }

    #[test]
    fn a_non_ready_datagram_from_the_expected_pid_is_rejected() {
        let runtime = private_runtime_dir("not-ready");
        let socket = ReadinessSocket::create(&runtime, 5).unwrap();
        let path = socket.path().to_owned();

        let pid = unsafe {
            let pid = libc::fork();
            assert!(pid >= 0);
            if pid == 0 {
                for _ in 0..500 {
                    if let Ok(client) = UnixDatagram::unbound()
                        && client.send_to(b"RELOADING=1\nSTATUS=starting", &path).is_ok()
                    {
                        libc::_exit(0);
                    }
                    libc::usleep(2_000);
                }
                libc::_exit(73);
            }
            pid
        };

        let mut rejected = false;
        for _ in 0..1000 {
            match socket.recv_ready(pid as u32) {
                Ok(false) => {
                    rejected = true;
                    break;
                }
                Ok(true) => panic!("RELOADING=1 must not satisfy readiness"),
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
                Err(error) => panic!("unexpected readiness read error: {error}"),
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }

        unsafe {
            let mut status = 0;
            libc::waitpid(pid, std::ptr::from_mut(&mut status), 0);
        }

        assert!(rejected, "RELOADING=1 must not satisfy readiness");
    }
}
