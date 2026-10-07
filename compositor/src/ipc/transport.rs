//! Bound complete frames, including peers that trickle bytes before each timeout.
use std::io::{self, Read, Write};
use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

use ferese_ipc::{FrameError, Request};

pub(super) const FRAME_TIMEOUT: Duration = Duration::from_secs(5);
pub(super) const IDLE_TIMEOUT: Duration = Duration::from_secs(30);
const WRITE_TIMEOUT: Duration = Duration::from_secs(2);

struct DeadlineIo<'a> {
    stream: &'a UnixStream,
    deadline: Instant,
}

impl DeadlineIo<'_> {
    fn remaining(&self) -> io::Result<Duration> {
        self.deadline
            .checked_duration_since(Instant::now())
            .filter(|duration| !duration.is_zero())
            .ok_or_else(|| io::Error::new(io::ErrorKind::TimedOut, "IPC frame deadline expired"))
    }
}

impl Read for DeadlineIo<'_> {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        self.stream.set_read_timeout(Some(self.remaining()?))?;
        self.stream.read(bytes)
    }
}

impl Write for DeadlineIo<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.stream.set_write_timeout(Some(self.remaining()?))?;
        self.stream.write(bytes)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.stream.flush()
    }
}

pub(super) fn read_request(stream: &UnixStream, idle: Option<Duration>) -> Result<Request, FrameError> {
    read_request_with_budget(stream, idle, FRAME_TIMEOUT)
}

fn read_request_with_budget(
    mut stream: &UnixStream,
    idle: Option<Duration>,
    budget: Duration,
) -> Result<Request, FrameError> {
    stream.set_read_timeout(idle)?;
    let mut first = [0];
    stream.read_exact(&mut first)?;
    let reader = DeadlineIo {
        stream,
        deadline: Instant::now() + budget,
    };
    ferese_ipc::read_frame(&mut first.as_slice().chain(reader))
}

pub(super) fn write_response(stream: &UnixStream, value: &impl serde::Serialize) -> Result<(), FrameError> {
    // Tests may install a tighter socket timeout.
    let previous = stream.write_timeout()?;
    let budget = previous.unwrap_or(WRITE_TIMEOUT).min(WRITE_TIMEOUT);
    let result = ferese_ipc::write_frame(
        &mut DeadlineIo {
            stream,
            deadline: Instant::now() + budget,
        },
        value,
    );
    stream.set_write_timeout(previous)?;
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn silent_and_partial_clients_expire_but_lease_idle_is_allowed() {
        let (mut client, server) = UnixStream::pair().unwrap();
        let short = Duration::from_millis(40);
        assert!(read_request_with_budget(&server, Some(short), short).is_err());
        client.write_all(&[0, 0]).unwrap();
        assert!(read_request_with_budget(&server, Some(short), short).is_err());

        let (mut client, server) = UnixStream::pair().unwrap();
        let worker = std::thread::spawn(move || read_request_with_budget(&server, None, short).unwrap());
        std::thread::sleep(short * 3);
        let request = Request {
            version: 1,
            id: 9,
            kind: "command".into(),
            command: "get-outputs".into(),
            args: serde_json::json!({}),
        };
        ferese_ipc::write_frame(&mut client, &request).unwrap();
        assert_eq!(worker.join().unwrap().id, 9);
    }

    #[test]
    fn trickling_does_not_extend_a_frame_deadline() {
        let (mut client, server) = UnixStream::pair().unwrap();
        let worker = std::thread::spawn(move || {
            for byte in [0, 0, 0, 100, b'{', b'"', b'a'] {
                if client.write_all(&[byte]).is_err() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(30));
            }
        });
        let error =
            read_request_with_budget(&server, Some(Duration::from_secs(1)), Duration::from_millis(70)).unwrap_err();
        assert!(
            matches!(error, FrameError::Io(error) if matches!(error.kind(), io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock))
        );
        drop(server);
        worker.join().unwrap();
    }
}
