//! Drain a child's pipe without waiting for inherited descriptors indefinitely.
use std::io::{self, Read};
use std::os::fd::AsRawFd;
use std::process::ChildStdout;
use std::sync::{Arc, Mutex};
use std::time::Instant;

pub(super) type Deadline = Arc<Mutex<Option<Instant>>>;
pub(super) struct Output {
    pub pipe: ChildStdout,
    pub deadline: Deadline,
}
impl Read for Output {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        if bytes.is_empty() {
            return Ok(0);
        }
        loop {
            if self
                .deadline
                .lock()
                .unwrap()
                .is_some_and(|deadline| Instant::now() >= deadline)
            {
                // EOF lets BufReader return its final unterminated line too.
                return Ok(0);
            }
            let mut fd = libc::pollfd {
                fd: self.pipe.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            // The pipe has exactly one reader. Once readable (or hung up), a
            // pipe read returns available bytes (or EOF) without filling bytes.
            let result = unsafe { libc::poll(&mut fd, 1, 20) };
            if result > 0 {
                return self.pipe.read(bytes);
            }
            if result < 0 {
                let error = io::Error::last_os_error();
                if error.kind() != io::ErrorKind::Interrupted {
                    return Err(error);
                }
            }
        }
    }
}
