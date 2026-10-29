use std::io::{self, Read, Write};
use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::{FrameError, read_frame, write_frame};

struct DeadlineIo<'socket> {
    stream: &'socket UnixStream,
    deadline: Instant,
}

impl<'socket> DeadlineIo<'socket> {
    fn try_new(stream: &'socket UnixStream, budget: Duration) -> io::Result<Self> {
        let deadline = Instant::now().checked_add(budget).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "IPC frame timeout is too large",
            )
        })?;
        Ok(Self { stream, deadline })
    }

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

/// Exchange one request and response with absolute frame deadlines.
///
/// Each configured socket timeout tightens `budget`. If the read timeout is
/// disabled, an idle watch may wait indefinitely for its first response byte;
/// `budget` then bounds the rest of that frame. Original timeouts are restored.
/// Callers must serialize access to shared socket clones, validate response
/// identity/outcome, and discard a connection after a framing or I/O error.
///
/// # Errors
/// Fails for invalid timeout budgets, expired deadlines, framing errors or socket I/O
/// errors, including failure to restore the original timeout configuration.
pub fn exchange_frames<T: Serialize, R: DeserializeOwned>(
    stream: &UnixStream,
    request: &T,
    budget: Duration,
) -> Result<R, FrameError> {
    let read_timeout = stream.read_timeout()?;
    let write_timeout = stream.write_timeout()?;
    let result = (|| {
        write_frame(
            &mut DeadlineIo::try_new(stream, write_timeout.unwrap_or(budget).min(budget))?,
            request,
        )?;
        read_with_budget(stream, read_timeout, budget)
    })();
    let restored = stream
        .set_read_timeout(read_timeout)
        .and_then(|()| stream.set_write_timeout(write_timeout));
    match result {
        Err(error) => Err(error),
        Ok(response) => {
            restored?;
            Ok(response)
        }
    }
}

/// Read one socket frame with an absolute deadline and restore its read timeout.
///
/// A configured read timeout tightens `budget`, including the first byte. With
/// no read timeout, an event subscription may idle indefinitely; once its first
/// byte arrives, the remaining frame must complete within `budget`. Discard the
/// connection on error, since a partial frame cannot be resumed safely.
///
/// # Errors
/// Fails for invalid timeout budgets, expired deadlines, framing errors or socket I/O
/// errors, including failure to restore the original read timeout.
pub fn read_socket_frame<R: DeserializeOwned>(
    stream: &UnixStream,
    budget: Duration,
) -> Result<R, FrameError> {
    let timeout = stream.read_timeout()?;
    let result = read_with_budget(stream, timeout, budget);
    let restored = stream.set_read_timeout(timeout);
    match result {
        Err(error) => Err(error),
        Ok(value) => {
            restored?;
            Ok(value)
        }
    }
}

fn read_with_budget<R: DeserializeOwned>(
    mut stream: &UnixStream,
    timeout: Option<Duration>,
    budget: Duration,
) -> Result<R, FrameError> {
    let mut first = [0];
    let reader = if let Some(timeout) = timeout {
        let mut reader = DeadlineIo::try_new(stream, timeout.min(budget))?;
        reader.read_exact(&mut first)?;
        reader
    } else {
        stream.read_exact(&mut first)?;
        DeadlineIo::try_new(stream, budget)?
    };
    read_frame(&mut first.as_slice().chain(reader))
}
