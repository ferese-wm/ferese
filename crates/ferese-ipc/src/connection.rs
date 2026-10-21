use crate::{Request, Response, VERSION, read_frame, write_frame};
use serde_json::Value;
use std::io;
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::Duration;

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
        stream.set_write_timeout(Some(Duration::from_secs(2)))?;
        stream.set_read_timeout(Some(Duration::from_secs(2)))?;
        Ok(Self { stream, id: 0 })
    }

    pub fn cancellation(&self) -> io::Result<Cancellation> {
        self.stream.try_clone().map(Cancellation)
    }

    pub fn call(&mut self, command: &str, args: Value) -> Result<Value, String> {
        self.id = self.id.wrapping_add(1);
        let request = Request {
            version: VERSION,
            id: self.id,
            kind: "command".into(),
            command: command.into(),
            args,
        };
        let response = write_frame(&mut self.stream, &request)
            .and_then(|()| read_frame::<Response>(&mut self.stream));
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
        if let Some(error) = response.error {
            return Err(error.message);
        }
        response.result.ok_or_else(|| "Missing IPC result".into())
    }

    /// Wait for a server-side change; dropping the cancellation handle interrupts it.
    pub fn wait(&mut self, command: &str, args: Value) -> Result<Value, String> {
        self.stream
            .set_read_timeout(None)
            .map_err(|e| e.to_string())?;
        let result = self.call(command, args);
        self.stream
            .set_read_timeout(Some(Duration::from_secs(2)))
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
    use serde_json::json;
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
}
