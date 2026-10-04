use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde_json::Value as Json;
use tokio::sync::Notify;

#[derive(Clone)]
pub(crate) struct Bridge(Arc<BridgeInner>);

struct Connection {
    stream: UnixStream,
    id: u64,
}

struct BridgeInner {
    connection: std::sync::Mutex<Connection>,
    shutdown: UnixStream,
    closed: AtomicBool,
    changed: Notify,
    timeout: Duration,
}

impl Bridge {
    pub(crate) fn connect() -> Result<Self, String> {
        let path = std::env::var_os("XDG_RUNTIME_DIR")
            .map(std::path::PathBuf::from)
            .ok_or("Missing runtime directory")?
            .join("ferese/control.sock");
        let stream = UnixStream::connect(path).map_err(|error| error.to_string())?;
        Self::from_stream(stream, Duration::from_secs(2))
    }

    fn from_stream(stream: UnixStream, timeout: Duration) -> Result<Self, String> {
        stream
            .set_read_timeout(Some(timeout))
            .map_err(|error| error.to_string())?;
        stream
            .set_write_timeout(Some(timeout))
            .map_err(|error| error.to_string())?;
        let shutdown = stream.try_clone().map_err(|error| error.to_string())?;
        Ok(Self(Arc::new(BridgeInner {
            connection: std::sync::Mutex::new(Connection { stream, id: 0 }),
            shutdown,
            closed: AtomicBool::new(false),
            changed: Notify::new(),
            timeout,
        })))
    }

    #[cfg(test)]
    pub(crate) fn test_pair() -> (Self, UnixStream) {
        let (stream, peer) = UnixStream::pair().unwrap();
        (Self::from_stream(stream, Duration::from_secs(1)).unwrap(), peer)
    }

    pub(crate) async fn call(&self, command: &'static str, args: Json) -> Result<Json, String> {
        self.request(command, args, false).await
    }

    pub(crate) async fn wait(&self, since: u64) -> Result<Json, String> {
        self.request("session-watch", serde_json::json!({"since": since}), true)
            .await
    }

    async fn request(&self, command: &'static str, args: Json, wait: bool) -> Result<Json, String> {
        let bridge = self.clone();
        match tokio::task::spawn_blocking(move || bridge.request_sync(command, args, wait)).await {
            Ok(result) => result,
            Err(error) => {
                self.close();
                Err(error.to_string())
            }
        }
    }

    fn request_sync(&self, command: &'static str, args: Json, wait: bool) -> Result<Json, String> {
        let mut connection = match self.0.connection.lock() {
            Ok(connection) => connection,
            Err(_) => {
                self.close();
                return Err("Compositor connection failed".into());
            }
        };

        let response = (|| {
            if self.is_closed() {
                return Err("Compositor connection is closed".to_owned());
            }

            connection.id = connection.id.checked_add(1).ok_or("Compositor request IDs exhausted")?;
            let request = ferese_ipc::Request {
                version: ferese_ipc::VERSION,
                id: connection.id,
                kind: "command".into(),
                command: command.into(),
                args,
            };
            connection
                .stream
                .set_read_timeout(if wait { None } else { Some(self.0.timeout) })
                .map_err(|error| error.to_string())?;
            ferese_ipc::write_frame(&mut connection.stream, &request).map_err(|error| error.to_string())?;
            let value: Json = ferese_ipc::read_frame(&mut connection.stream).map_err(|error| error.to_string())?;
            let has_result = value.get("result").is_some();
            let response: ferese_ipc::Response = serde_json::from_value(value).map_err(|error| error.to_string())?;
            if response.id != request.id || response.version != ferese_ipc::VERSION {
                return Err("Unexpected compositor response ID or protocol version".into());
            }

            if has_result == response.error.is_some() {
                return Err("Invalid compositor response outcome".into());
            }

            Ok(response)
        })();

        match response {
            Ok(response) => match response.error {
                Some(error) => Err(error.message),
                None => Ok(response.result.unwrap_or(Json::Null)),
            },
            Err(error) => {
                // Poison the stream before releasing the request lock so a
                // queued caller cannot read a late or partial response.
                self.close();
                Err(error)
            }
        }
    }

    pub(crate) async fn capture_watch(&self, session: u64) -> Result<Json, String> {
        self.request("input-capture-watch", serde_json::json!({"session":session}), true)
            .await
    }

    pub(crate) fn is_closed(&self) -> bool {
        self.0.closed.load(Ordering::SeqCst)
    }

    pub(crate) async fn terminated(&self) {
        let changed = self.0.changed.notified();
        tokio::pin!(changed);
        changed.as_mut().enable();
        if !self.is_closed() {
            changed.await;
        }
    }

    pub(crate) async fn closed(&self) {
        let Ok(reader) = self.0.shutdown.try_clone() else {
            return;
        };
        let Ok(reader) = tokio::io::unix::AsyncFd::new(reader) else {
            return;
        };
        loop {
            let readable = tokio::select! {
                _ = self.terminated() => return,
                ready = reader.readable() => ready,
            };
            let Ok(mut ready) = readable else {
                return;
            };
            match ready.try_io(|reader| {
                use std::os::fd::AsRawFd;
                let mut byte = 0u8;
                // Per-call nonblocking peek leaves the shared descriptor flags
                // and response bytes untouched for the request reader.
                let result = unsafe {
                    libc::recv(
                        reader.get_ref().as_raw_fd(),
                        (&mut byte as *mut u8).cast(),
                        1,
                        libc::MSG_PEEK | libc::MSG_DONTWAIT,
                    )
                };
                if result < 0 {
                    Err(std::io::Error::last_os_error())
                } else {
                    Ok(result)
                }
            }) {
                Ok(_) => return,
                Err(_) => continue,
            }
        }
    }

    pub(crate) fn close(&self) {
        self.0.closed.store(true, Ordering::SeqCst);
        let _ = self.0.shutdown.shutdown(std::net::Shutdown::Both);
        self.0.changed.notify_waiters();
    }
}

#[cfg(test)]
mod tests;
