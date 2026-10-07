use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

pub(super) fn call(command: &str) -> Result<Value, String> {
    request(command, json!({}))
}

#[derive(Debug)]
struct SessionConnection {
    stream: std::sync::Mutex<UnixStream>,
}

impl SessionConnection {
    fn connect() -> Result<Self, String> {
        let path = ferese_ipc::socket::resolve(None).map_err(|error| error.to_string())?;
        let stream = UnixStream::connect(path).map_err(|error| error.to_string())?;
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .map_err(|error| error.to_string())?;
        stream
            .set_write_timeout(Some(Duration::from_secs(2)))
            .map_err(|error| error.to_string())?;
        Ok(Self {
            stream: std::sync::Mutex::new(stream),
        })
    }

    fn request(&self, command: &str, args: Value) -> Result<Value, String> {
        let mut stream = self.stream.lock().map_err(|_| "Compositor connection failed")?;
        let request = ferese_ipc::Request {
            version: ferese_ipc::VERSION,
            id: 1,
            kind: "command".into(),
            command: command.into(),
            args,
        };
        ferese_ipc::write_frame(&mut *stream, &request).map_err(|error| error.to_string())?;
        let response: ferese_ipc::Response = ferese_ipc::read_frame(&mut *stream).map_err(|error| error.to_string())?;
        if let Some(error) = response.error {
            return Err(error.message);
        }
        response.result.ok_or("Missing compositor response".into())
    }
}

pub(super) fn request(command: &str, args: Value) -> Result<Value, String> {
    SessionConnection::connect()?.request(command, args)
}

#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub(super) enum Mode {
    #[default]
    Guide,
    Logout,
    Shutdown,
    Suspend,
}

#[derive(Clone, Debug, Default)]
pub(super) struct Approval {
    pub reasons: Vec<String>,
    pub revision: u32,
    pub token: u32,
    mode: Mode,
    external: Vec<String>,
    connection: Option<std::sync::Arc<SessionConnection>>,
}

impl Approval {
    pub fn force(&self) -> bool {
        !self.reasons.is_empty()
    }

    pub fn cancel(&self) {
        if self.mode == Mode::Shutdown {
            let _ = request("cancel-session-end", json!({"token": self.token}));
        }
    }

    pub fn prepare(&self) -> Result<(), String> {
        if matches!(self.mode, Mode::Shutdown | Mode::Suspend) && login_inhibitors(self.mode)? != self.external {
            return Err("Applications changed their inhibitors; review the confirmation again".into());
        }
        if self.mode == Mode::Shutdown {
            self.connection
                .as_ref()
                .ok_or("Missing session-ending lease")?
                .request(
                    "validate-session-end",
                    json!({"token": self.token, "inhibitor-revision": self.revision, "force": self.force()}),
                )?;
        } else if self.mode == Mode::Suspend {
            let state = call("get-session-state")?;
            if state["inhibitor-revision"].as_u64() != Some(u64::from(self.revision)) {
                return Err("Applications changed their inhibitors; review the confirmation again".into());
            }
        }
        Ok(())
    }
}

fn login_inhibitors(mode: Mode) -> Result<Vec<String>, String> {
    if !matches!(mode, Mode::Shutdown | Mode::Suspend) {
        return Ok(Vec::new());
    }
    let connection = zbus::blocking::connection::Builder::system()
        .map_err(|error| error.to_string())?
        .method_timeout(Duration::from_secs(2))
        .build()
        .map_err(|error| error.to_string())?;
    let proxy = zbus::blocking::Proxy::new(
        &connection,
        "org.freedesktop.login1",
        "/org/freedesktop/login1",
        "org.freedesktop.login1.Manager",
    )
    .map_err(|error| error.to_string())?;
    let inhibitors: Vec<(String, String, String, String, u32, u32)> =
        proxy.call("ListInhibitors", &()).map_err(|error| error.to_string())?;
    let what = if mode == Mode::Suspend { "sleep" } else { "shutdown" };
    let mut reasons = inhibitors
        .into_iter()
        .filter(|(flags, _, _, inhibition, _, _)| inhibition == "block" && flags.split(':').any(|flag| flag == what))
        .map(|(_, app, reason, _, _, _)| format!("{app}: {reason}"))
        .collect::<Vec<_>>();
    reasons.sort();
    reasons.dedup();
    Ok(reasons)
}

pub(super) fn inhibitors(mode: Mode) -> Result<Approval, String> {
    let connection = if mode == Mode::Shutdown {
        Some(std::sync::Arc::new(SessionConnection::connect()?))
    } else {
        None
    };
    let mut state = match &connection {
        Some(connection) => connection.request("begin-session-end", json!({}))?,
        None => call("get-session-state")?,
    };
    let token = state["query-token"]
        .as_u64()
        .and_then(|token| u32::try_from(token).ok())
        .unwrap_or(0);
    let result = (|| {
        if matches!(mode, Mode::Logout | Mode::Shutdown) {
            let deadline = Instant::now() + Duration::from_millis(1500);
            while state["query-ready"] != true {
                if state["session-state"] != 2
                    || state["query-token"].as_u64() != Some(u64::from(token))
                    || Instant::now() >= deadline
                {
                    return Err("Session-ending request was cancelled or timed out".into());
                }
                std::thread::sleep(Duration::from_millis(50));
                state = match &connection {
                    Some(connection) => connection.request("get-session-state", json!({}))?,
                    None => call("get-session-state")?,
                };
            }
        }
        let flags = if mode == Mode::Suspend { 4 } else { 1 };
        let entries = state["inhibitors"].as_array().ok_or("Invalid inhibitor list")?;
        let mut reasons = entries
            .iter()
            .filter(|entry| entry["flags"].as_u64().unwrap_or(0) & flags != 0)
            .map(|entry| {
                let app = entry["app"]
                    .as_str()
                    .filter(|app| !app.is_empty())
                    .unwrap_or("Application");
                let reason = entry["reason"].as_str().unwrap_or("Requested session inhibition");
                format!("{app}: {reason}")
            })
            .collect::<Vec<_>>();
        let external = login_inhibitors(mode)?;
        reasons.extend(external.iter().cloned());
        reasons.sort();
        reasons.dedup();
        Ok(Approval {
            reasons,
            revision: state["inhibitor-revision"]
                .as_u64()
                .and_then(|revision| u32::try_from(revision).ok())
                .ok_or("Missing inhibitor revision")?,
            token,
            mode,
            external,
            connection,
        })
    })();
    if result.is_err() && mode == Mode::Shutdown {
        let _ = request("cancel-session-end", json!({"token": token}));
    }
    result
}
