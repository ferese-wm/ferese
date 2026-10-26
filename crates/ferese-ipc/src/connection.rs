use crate::{Request, Response, VERSION, exchange_frames};
use serde_json::Value;
use std::io;
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::Duration;

const FRAME_TIMEOUT: Duration = Duration::from_secs(2);

pub fn socket_path() -> io::Result<PathBuf> {
    crate::socket::resolve(None)
}

pub struct Connection {
    stream: UnixStream,
    id: u64,
}

impl Connection {
    pub fn connect() -> io::Result<Self> {
        Self::connect_to(socket_path()?)
    }

    pub fn connect_to(path: impl AsRef<std::path::Path>) -> io::Result<Self> {
        let stream = UnixStream::connect(path)?;
        stream.set_write_timeout(Some(FRAME_TIMEOUT))?;
        stream.set_read_timeout(Some(FRAME_TIMEOUT))?;
        Ok(Self { stream, id: 0 })
    }

    pub fn cancellation(&self) -> io::Result<Cancellation> {
        self.stream.try_clone().map(Cancellation)
    }

    pub fn call(&mut self, command: &str, args: Value) -> Result<Value, String> {
        self.call_with_budget(command, args, FRAME_TIMEOUT)
    }

    fn call_with_budget(
        &mut self,
        command: &str,
        args: Value,
        budget: Duration,
    ) -> Result<Value, String> {
        self.id = self.id.wrapping_add(1);
        let request = Request {
            version: VERSION,
            id: self.id,
            kind: "command".into(),
            command: command.into(),
            args,
        };
        let response = exchange_frames::<_, Response>(&self.stream, &request, budget);
        let response = match response {
            Ok(response) => response,
            Err(error) => {
                let _ = self.stream.shutdown(Shutdown::Both);
                return Err(error.to_string());
            }
        };
        if response.id != self.id || response.version != VERSION {
            let _ = self.stream.shutdown(Shutdown::Both);
            return Err("Unexpected IPC response".into());
        }
        match (response.result, response.error) {
            (Some(result), None) => Ok(result),
            (None, Some(error)) => Err(error.message),
            _ => {
                let _ = self.stream.shutdown(Shutdown::Both);
                Err("Invalid IPC response outcome".into())
            }
        }
    }

    /// Wait for a server-side change; dropping the cancellation handle interrupts it.
    pub fn wait(&mut self, command: &str, args: Value) -> Result<Value, String> {
        self.stream
            .set_read_timeout(None)
            .map_err(|e| e.to_string())?;
        let result = self.call(command, args);
        self.stream
            .set_read_timeout(Some(FRAME_TIMEOUT))
            .map_err(|e| e.to_string())?;
        result
    }
}

pub struct Cancellation(UnixStream);

impl Drop for Cancellation {
    fn drop(&mut self) {
        let _ = self.0.shutdown(Shutdown::Both);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{read_frame, write_frame};
    use serde_json::json;
    use std::io::{Read, Write};
    use std::sync::mpsc;

    #[test]
    fn cancelling_a_blocked_watch_releases_the_client_and_server() {
        let (stream, mut server) = UnixStream::pair().unwrap();
        let mut connection = Connection { stream, id: 0 };
        let cancellation = connection.cancellation().unwrap();
        let (send, receive) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            send.send(connection.wait("activity-watch", json!({"since": 7})))
                .unwrap();
        });
        server
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let request: Request = read_frame(&mut server).unwrap();
        assert_eq!(request.command, "activity-watch");
        assert_eq!(request.args, json!({"since": 7}));
        drop(cancellation);
        assert!(
            receive
                .recv_timeout(Duration::from_secs(2))
                .unwrap()
                .is_err()
        );
        assert!(read_frame::<Request>(&mut server).is_err());
        worker.join().unwrap();
    }

    #[test]
    fn response_identity_is_checked_before_returning_values() {
        for wrong_version in [false, true] {
            let (stream, mut server) = UnixStream::pair().unwrap();
            server
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let worker = std::thread::spawn(move || {
                let request: Request = read_frame(&mut server).unwrap();
                let mut response = Response::success(request.id, json!({}));
                if wrong_version {
                    response.version += 1;
                } else {
                    response.id += 1;
                }
                write_frame(&mut server, &response).unwrap();
            });
            let error = Connection { stream, id: 0 }
                .call("activity-get", json!({}))
                .unwrap_err();
            assert_eq!(error, "Unexpected IPC response");
            worker.join().unwrap();
        }
    }

    #[test]
    fn trickled_response_cannot_extend_a_call_or_started_watch() {
        for watch in [false, true] {
            let (stream, mut server) = UnixStream::pair().unwrap();
            server.set_read_timeout(Some(FRAME_TIMEOUT)).unwrap();
            let budget = Duration::from_millis(90);
            stream.set_read_timeout((!watch).then_some(budget)).unwrap();
            let worker = std::thread::spawn(move || {
                let request: Request = read_frame(&mut server).unwrap();
                if watch {
                    // An idle watch must outlive its partial-frame budget.
                    std::thread::sleep(budget * 3);
                }
                let mut frame = Vec::new();
                write_frame(&mut frame, &Response::success(request.id, json!({}))).unwrap();
                for byte in frame {
                    if server.write_all(&[byte]).is_err() {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(25));
                }
            });
            let mut connection = Connection { stream, id: 0 };
            assert!(
                connection
                    .call_with_budget("activity-watch", json!({"since": 7}), budget)
                    .is_err(),
                "a trickled partial response must expire (watch={watch})"
            );
            worker.join().unwrap();
        }
    }

    #[test]
    fn completed_calls_restore_socket_timeouts_for_the_next_frame() {
        let (stream, mut server) = UnixStream::pair().unwrap();
        let timeout = Some(Duration::from_secs(1));
        stream.set_read_timeout(timeout).unwrap();
        stream.set_write_timeout(timeout).unwrap();
        server.set_read_timeout(Some(FRAME_TIMEOUT)).unwrap();
        let worker = std::thread::spawn(move || {
            for _ in 0..2 {
                let request: Request = read_frame(&mut server).unwrap();
                write_frame(&mut server, &Response::success(request.id, json!({}))).unwrap();
            }
        });
        let mut connection = Connection { stream, id: 0 };
        for _ in 0..2 {
            connection.call("get-outputs", json!({})).unwrap();
            assert_eq!(connection.stream.read_timeout().unwrap(), timeout);
            assert_eq!(connection.stream.write_timeout().unwrap(), timeout);
        }
        worker.join().unwrap();
    }

    #[test]
    fn null_results_and_valid_errors_allow_the_next_request() {
        let (stream, mut server) = UnixStream::pair().unwrap();
        stream.set_read_timeout(Some(FRAME_TIMEOUT)).unwrap();
        server.set_read_timeout(Some(FRAME_TIMEOUT)).unwrap();
        let worker = std::thread::spawn(move || {
            let request: Request = read_frame(&mut server).unwrap();
            write_frame(
                &mut server,
                &Response::error(request.id, "denied", "Denied"),
            )
            .unwrap();
            let request: Request = read_frame(&mut server).unwrap();
            assert_eq!(request.command, "get-focused-window");
            write_frame(&mut server, &Response::success(request.id, Value::Null)).unwrap();
            let request: Request = read_frame(&mut server).unwrap();
            write_frame(&mut server, &Response::success(request.id, json!([]))).unwrap();
        });
        let mut connection = Connection { stream, id: 0 };
        assert_eq!(connection.call("focus", json!({})).unwrap_err(), "Denied");
        assert_eq!(
            connection.call("get-focused-window", json!({})).unwrap(),
            Value::Null
        );
        assert_eq!(
            connection.call("get-outputs", json!({})).unwrap(),
            json!([])
        );
        worker.join().unwrap();
    }

    #[test]
    fn missing_or_conflicting_outcomes_close_the_connection() {
        for conflicting in [false, true] {
            let (stream, mut server) = UnixStream::pair().unwrap();
            stream.set_read_timeout(Some(FRAME_TIMEOUT)).unwrap();
            server.set_read_timeout(Some(FRAME_TIMEOUT)).unwrap();
            let worker = std::thread::spawn(move || {
                let request: Request = read_frame(&mut server).unwrap();
                let mut response = Response::success(request.id, Value::Null);
                if conflicting {
                    response.error = Response::error(request.id, "failed", "Failed").error;
                } else {
                    response.result = None;
                }
                write_frame(&mut server, &response).unwrap();
                assert_eq!(server.read(&mut [0]).unwrap(), 0);
            });
            let mut connection = Connection { stream, id: 0 };
            assert_eq!(
                connection
                    .call("get-focused-window", json!({}))
                    .unwrap_err(),
                "Invalid IPC response outcome"
            );
            assert!(connection.call("get-outputs", json!({})).is_err());
            worker.join().unwrap();
        }
    }
}
