//! A complete exchange has one deadline, including a full Unix listen backlog.
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::{ffi::OsStrExt, net::UnixStream};
use std::path::Path;
use std::time::{Duration, Instant};

pub(super) struct Stream {
    socket: UnixStream,
    deadline: Instant,
}

pub(super) fn connect(path: &Path, timeout: Duration) -> io::Result<Stream> {
    let deadline = Instant::now() + timeout;
    let path = path.as_os_str().as_bytes();
    // SAFETY: an all-zero sockaddr_un is valid to initialize before connect.
    let mut address: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    if path.is_empty() || path.len() >= address.sun_path.len() || path.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid activity socket path",
        ));
    }
    address.sun_family = libc::AF_UNIX as libc::sa_family_t;
    for (to, from) in address.sun_path.iter_mut().zip(path) {
        *to = *from as libc::c_char;
    }
    // SAFETY: socket creates a new descriptor; nonblocking connect cannot wait
    // indefinitely for room in a Unix listener's queue.
    let fd = unsafe {
        libc::socket(
            libc::AF_UNIX,
            libc::SOCK_STREAM | libc::SOCK_CLOEXEC | libc::SOCK_NONBLOCK,
            0,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: this function takes sole ownership of the descriptor just created.
    let socket = unsafe { UnixStream::from_raw_fd(fd) };
    let length = std::mem::offset_of!(libc::sockaddr_un, sun_path) + path.len() + 1;
    // SAFETY: address contains a checked, NUL-terminated pathname of length bytes.
    if unsafe {
        libc::connect(
            socket.as_raw_fd(),
            (&raw const address).cast(),
            length as libc::socklen_t,
        )
    } != 0
    {
        // Unix connect returns EAGAIN for a full backlog. Activity publishing is
        // best effort, so fail promptly instead of delaying the wrapped command.
        return Err(io::Error::last_os_error());
    }
    socket.set_nonblocking(false)?;
    Ok(Stream { socket, deadline })
}

impl Stream {
    fn remaining(&self) -> io::Result<Duration> {
        self.deadline
            .checked_duration_since(Instant::now())
            .filter(|remaining| !remaining.is_zero())
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::TimedOut,
                    "activity publication deadline expired",
                )
            })
    }
}
impl Read for Stream {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        self.socket.set_read_timeout(Some(self.remaining()?))?;
        self.socket.read(bytes)
    }
}
impl Write for Stream {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.socket.set_write_timeout(Some(self.remaining()?))?;
        self.socket.write(bytes)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.socket.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixListener;
    use std::sync::mpsc;

    #[test]
    fn trickled_response_expires_and_closes_the_socket_without_a_detached_reader() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("broker.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let (done, closed) = mpsc::channel();
        let server = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            let _: ferese_ipc::Request = ferese_ipc::read_frame(&mut socket).unwrap();
            socket.write_all(&1000_u32.to_be_bytes()).unwrap();
            for _ in 0..50 {
                if socket.write_all(b" ").is_err() {
                    done.send(()).unwrap();
                    return;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        });
        {
            let mut stream = connect(&path, Duration::from_millis(100)).unwrap();
            assert!(
                super::super::super::request(&mut stream, "activity-get", serde_json::json!({}))
                    .is_err()
            );
        }
        closed
            .recv_timeout(Duration::from_millis(400))
            .expect("timed-out socket remained open");
        server.join().unwrap();
    }

    #[test]
    fn full_listener_backlog_does_not_block_connect() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("broker.sock");
        let listener = UnixListener::bind(&path).unwrap();
        // SAFETY: the listener owns this socket; reducing its backlog does not
        // affect the descriptor or the sockaddr used by subsequent clients.
        assert_eq!(unsafe { libc::listen(listener.as_raw_fd(), 0) }, 0);
        let _first = connect(&path, Duration::from_secs(1)).unwrap();
        let started = Instant::now();
        assert!(connect(&path, Duration::from_secs(1)).is_err());
        assert!(started.elapsed() < Duration::from_millis(200));
    }
}
