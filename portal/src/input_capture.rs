use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use serde_json::json;
use tokio::sync::Mutex;
use zbus::Connection;
use zbus::message::Header;
use zbus::object_server::SignalEmitter;
use zbus::zvariant::{OwnedFd, OwnedObjectPath, Value};

use crate::backend::{Cancel, Options, authorize};
use crate::bridge::Bridge;
use crate::desktop::Requests;
use crate::eis::Worker;

const PATH: &str = "/org/freedesktop/portal/desktop";

#[derive(Default)]
struct State {
    starting: bool,
    started: bool,
    id: u64,
    capabilities: u32,
    keymap: String,
    bridge: Option<Bridge>,
    watch: Option<Bridge>,
    worker: Option<Worker>,
}

struct Session {
    owner: String,
    app: String,
    cancel: Arc<Cancel>,
    state: Mutex<State>,
}

#[derive(Clone, Default)]
pub(crate) struct InputCapture {
    sessions: Arc<Mutex<HashMap<String, Arc<Session>>>>,
    requests: Requests,
}

impl InputCapture {
    pub(crate) fn new(requests: Requests) -> Self {
        Self {
            requests,
            ..Self::default()
        }
    }

    async fn session(
        &self,
        connection: &Connection,
        header: &Header<'_>,
        path: &OwnedObjectPath,
        app: &str,
    ) -> zbus::fdo::Result<Arc<Session>> {
        let owner = authorize(connection, header).await?;
        self.sessions
            .lock()
            .await
            .get(path.as_str())
            .filter(|session| session.owner == owner && session.app == app)
            .cloned()
            .ok_or_else(|| invalid("Unknown input capture session"))
    }

    async fn end(&self, connection: &Connection, path: &str) {
        let session = self.sessions.lock().await.remove(path);
        if let Some(session) = session {
            session.cancel.stop();
            let state = session.state.lock().await;
            if let Some(worker) = &state.worker {
                worker.stop();
            }
            if let Some(bridge) = &state.bridge {
                bridge.close();
            }
            if let Some(watch) = &state.watch {
                watch.close();
            }
            if let Ok(emitter) = SignalEmitter::new(connection, path) {
                let _ = SessionObject::closed(&emitter).await;
            }
            let connection = connection.clone();
            let path = path.to_owned();
            tokio::spawn(async move {
                let _ = connection.object_server().remove::<SessionObject, _>(path).await;
            });
        }
    }

    pub(crate) async fn revoke_stale(&self, connection: &Connection, owner: Option<&str>) {
        let stale = self
            .sessions
            .lock()
            .await
            .iter()
            .filter_map(|(path, session)| (Some(session.owner.as_str()) != owner).then_some(path.clone()))
            .collect::<Vec<_>>();
        for path in stale {
            self.end(connection, &path).await;
        }
    }

    async fn finish_native_call(
        &self,
        connection: &Connection,
        path: &OwnedObjectPath,
        bridge: &Bridge,
        result: Result<serde_json::Value, String>,
    ) -> zbus::fdo::Result<serde_json::Value> {
        if bridge.is_closed() {
            self.end(connection, path.as_str()).await;
        }

        result.map_err(failed)
    }

    async fn native(session: &Session) -> zbus::fdo::Result<(Bridge, u64)> {
        let state = session.state.lock().await;
        if !state.started || session.cancel.stopped.load(Ordering::SeqCst) {
            return Err(invalid("Start the session first"));
        }
        Ok((
            state
                .bridge
                .clone()
                .ok_or_else(|| invalid("Missing capture connection"))?,
            state.id,
        ))
    }

    async fn watch(
        self,
        connection: Connection,
        path: OwnedObjectPath,
        session: Arc<Session>,
        bridge: Bridge,
        id: u64,
    ) {
        let control = session.state.lock().await.bridge.clone();
        let Some(control) = control else {
            self.end(&connection, path.as_str()).await;
            return;
        };

        loop {
            let response = tokio::select! {
                _ = session.cancel.wait() => break,
                _ = control.terminated() => break,
                result = bridge.capture_watch(id) => result,
            };
            let Ok(response) = response else {
                break;
            };
            let Some(events) = response["events"].as_array() else {
                break;
            };
            let state = session.state.lock().await;
            if session.cancel.stopped.load(Ordering::SeqCst) {
                break;
            }
            let transport = state.worker.as_ref().map(|worker| worker.send(events.clone()));
            if transport.is_some_and(|result| result.is_err()) {
                break;
            }
            let mut closed = false;
            let Ok(emitter) = SignalEmitter::new(&connection, PATH) else {
                break;
            };
            for event in events {
                let result = match event["type"].as_str() {
                    Some("activated") => {
                        let mut options = Options::new();
                        options.insert(
                            "activation_id".into(),
                            (event["activation_id"].as_u64().unwrap_or(0) as u32).into(),
                        );
                        options.insert(
                            "barrier_id".into(),
                            (event["barrier_id"].as_u64().unwrap_or(0) as u32).into(),
                        );
                        if let Some(position) = event["cursor_position"]
                            .as_array()
                            .filter(|position| position.len() == 2)
                            && let Ok(position) =
                                Value::from((position[0].as_f64().unwrap_or(0.0), position[1].as_f64().unwrap_or(0.0)))
                                    .try_to_owned()
                        {
                            options.insert("cursor_position".into(), position);
                        }
                        Self::activated(&emitter, path.clone(), options).await
                    }
                    Some("deactivated") => {
                        Self::deactivated(
                            &emitter,
                            path.clone(),
                            Options::from([(
                                "activation_id".into(),
                                (event["activation_id"].as_u64().unwrap_or(0) as u32).into(),
                            )]),
                        )
                        .await
                    }
                    Some("disabled") => Self::disabled(&emitter, path.clone(), Options::new()).await,
                    Some("zones") => Self::zones_changed(&emitter, path.clone(), Options::new()).await,
                    Some("closed") => {
                        closed = true;
                        Ok(())
                    }
                    _ => Ok(()),
                };
                if result.is_err() {
                    closed = true;
                }
            }
            drop(state);
            if closed {
                break;
            }
        }
        self.end(&connection, path.as_str()).await;
    }
}

fn invalid(message: &str) -> zbus::fdo::Error {
    zbus::fdo::Error::InvalidArgs(message.into())
}

fn failed(error: String) -> zbus::fdo::Error {
    zbus::fdo::Error::Failed(error)
}

fn capabilities(options: &Options) -> zbus::fdo::Result<u32> {
    let requested = options
        .get("capabilities")
        .ok_or_else(|| invalid("Missing requested capabilities"))?;
    let requested = u32::try_from(requested).map_err(|_| invalid("Invalid requested capabilities"))?;
    if requested == 0 || requested & 3 == 0 {
        return Err(invalid("No supported capabilities requested"));
    }
    Ok(requested & 3)
}

#[zbus::interface(name = "org.freedesktop.impl.portal.InputCapture")]
impl InputCapture {
    #[zbus(property, name = "version")]
    fn version(&self) -> u32 {
        2
    }

    #[zbus(property)]
    fn supported_capabilities(&self) -> u32 {
        3
    }

    async fn create_session2(
        &self,
        session_handle: OwnedObjectPath,
        app_id: String,
        _options: Options,
        #[zbus(connection)] connection: &Connection,
        #[zbus(header)] header: Header<'_>,
    ) -> zbus::fdo::Result<Options> {
        let owner = authorize(connection, &header).await?;
        if !session_handle.as_str().starts_with(&format!("{PATH}/session/"))
            || session_handle.as_str().len() > 512
            || app_id.len() > 512
        {
            return Err(invalid("Invalid session"));
        }
        let mut sessions = self.sessions.lock().await;
        if sessions.len() >= 8 || sessions.contains_key(session_handle.as_str()) {
            return Err(invalid("Session limit reached or duplicate session"));
        }
        if !connection
            .object_server()
            .at(
                &session_handle,
                SessionObject {
                    backend: self.clone(),
                    path: session_handle.to_string(),
                    owner: owner.clone(),
                },
            )
            .await?
        {
            return Err(invalid("Session object already exists"));
        }
        sessions.insert(
            session_handle.to_string(),
            Arc::new(Session {
                owner,
                app: app_id,
                cancel: Arc::default(),
                state: Mutex::default(),
            }),
        );
        Ok(Options::new())
    }

    #[allow(clippy::too_many_arguments)]
    async fn create_session(
        &self,
        handle: OwnedObjectPath,
        session_handle: OwnedObjectPath,
        app_id: String,
        parent_window: String,
        options: Options,
        #[zbus(connection)] connection: &Connection,
        #[zbus(header)] header: Header<'_>,
    ) -> zbus::fdo::Result<(u32, Options)> {
        capabilities(&options)?;
        self.create_session2(
            session_handle.clone(),
            app_id.clone(),
            Options::new(),
            connection,
            header.clone(),
        )
        .await?;
        let result = self
            .start(
                handle,
                session_handle.clone(),
                app_id,
                parent_window,
                options,
                connection,
                header,
            )
            .await;
        let (response, mut results) = match result {
            Ok(result) => result,
            Err(error) => {
                self.end(connection, session_handle.as_str()).await;
                return Err(error);
            }
        };
        if response == 0 {
            results.insert(
                "session_id".into(),
                Value::from(session_handle.to_string())
                    .try_to_owned()
                    .map_err(|e| failed(e.to_string()))?,
            );
        }
        Ok((response, results))
    }

    #[allow(clippy::too_many_arguments)]
    async fn start(
        &self,
        handle: OwnedObjectPath,
        session_handle: OwnedObjectPath,
        app_id: String,
        parent_window: String,
        options: Options,
        #[zbus(connection)] connection: &Connection,
        #[zbus(header)] header: Header<'_>,
    ) -> zbus::fdo::Result<(u32, Options)> {
        let session = self.session(connection, &header, &session_handle, &app_id).await?;
        let caps = capabilities(&options)?;
        crate::restore::persist_mode(&options)?;
        let cancel = self.requests.begin(connection, &header, &handle).await?;
        {
            let mut state = session.state.lock().await;
            if state.starting || state.started {
                self.requests.end(connection, &handle).await;
                return Err(invalid("Input capture can only be started once"));
            }
            state.starting = true;
        }
        let title = match caps {
            1 => "Share keyboard input?",
            2 => "Share pointer input?",
            _ => "Share keyboard and pointer?",
        };
        let consent = crate::desktop::consent(
            &app_id,
            &parent_window,
            title,
            "Allow input capture at the screen edges this app configures. Captured input is sent to this app until it releases control. Press Ctrl+Alt+Escape to return control to Ferese.",
            "Allow input capture",
            None,
        );
        let approved = tokio::select! {
            _ = cancel.wait() => Ok(false),
            _ = session.cancel.wait() => Ok(false),
            result = consent => result,
        };
        let result = if approved == Ok(true)
            && !cancel.stopped.load(Ordering::SeqCst)
            && !session.cancel.stopped.load(Ordering::SeqCst)
        {
            async {
                let bridge = Bridge::connect()?;
                let reply = bridge
                    .call("input-capture-register", json!({"capabilities":caps}))
                    .await?;
                let id = reply["session"].as_u64().ok_or("Missing capture session ID")?;
                let keymap = reply["keymap"].as_str().ok_or("Missing keyboard map")?.to_owned();
                let watch = Bridge::connect()?;
                let mut state = session.state.lock().await;
                if cancel.stopped.load(Ordering::SeqCst) || session.cancel.stopped.load(Ordering::SeqCst) {
                    bridge.close();
                    return Err("Input capture approval was cancelled".to_owned());
                }
                state.started = true;
                state.starting = false;
                state.id = id;
                state.capabilities = caps;
                state.keymap = keymap;
                state.bridge = Some(bridge);
                state.watch = Some(watch.clone());
                tokio::spawn(self.clone().watch(
                    connection.clone(),
                    session_handle.clone(),
                    session.clone(),
                    watch,
                    id,
                ));
                Ok(Options::from([
                    ("capabilities".into(), caps.into()),
                    ("clipboard_enabled".into(), false.into()),
                    ("persist_mode".into(), 0u32.into()),
                ]))
            }
            .await
        } else {
            Err(approved.err().unwrap_or_else(|| "Cancelled".into()))
        };
        self.requests.end(connection, &handle).await;
        match result {
            Ok(results) => Ok((0, results)),
            Err(error) => {
                let response = if error == "Cancelled"
                    || cancel.stopped.load(Ordering::SeqCst)
                    || session.cancel.stopped.load(Ordering::SeqCst)
                {
                    1
                } else {
                    eprintln!("ferese input capture: {error}");
                    2
                };
                self.end(connection, session_handle.as_str()).await;
                Ok((response, Options::new()))
            }
        }
    }

    async fn get_zones(
        &self,
        handle: OwnedObjectPath,
        session_handle: OwnedObjectPath,
        app_id: String,
        _options: Options,
        #[zbus(connection)] connection: &Connection,
        #[zbus(header)] header: Header<'_>,
    ) -> zbus::fdo::Result<(u32, Options)> {
        let session = self.session(connection, &header, &session_handle, &app_id).await?;
        let (bridge, id) = Self::native(&session).await?;
        let cancel = self.requests.begin(connection, &header, &handle).await?;
        let result = tokio::select! {
            biased;
            _ = cancel.wait() => None,
            _ = session.cancel.wait() => None,
            result = bridge.call("input-capture-zones", json!({"session":id})) => Some(result),
        };
        self.requests.end(connection, &handle).await;
        if cancel.stopped.load(Ordering::SeqCst) || session.cancel.stopped.load(Ordering::SeqCst) || result.is_none() {
            return Ok((1, Options::new()));
        }
        let result = self
            .finish_native_call(connection, &session_handle, &bridge, result.unwrap())
            .await?;
        let zones: Vec<(u32, u32, i32, i32)> =
            serde_json::from_value(result["zones"].clone()).map_err(|_| invalid("Invalid native capture zones"))?;
        Ok((
            0,
            Options::from([
                (
                    "zones".into(),
                    Value::from(zones).try_to_owned().map_err(|e| failed(e.to_string()))?,
                ),
                (
                    "zone_set".into(),
                    (result["zone_set"].as_u64().unwrap_or(0) as u32).into(),
                ),
            ]),
        ))
    }

    #[allow(clippy::too_many_arguments)]
    async fn set_pointer_barriers(
        &self,
        handle: OwnedObjectPath,
        session_handle: OwnedObjectPath,
        app_id: String,
        _options: Options,
        barriers: Vec<Options>,
        zone_set: u32,
        #[zbus(connection)] connection: &Connection,
        #[zbus(header)] header: Header<'_>,
    ) -> zbus::fdo::Result<(u32, Options)> {
        let session = self.session(connection, &header, &session_handle, &app_id).await?;
        let (bridge, id) = Self::native(&session).await?;
        if barriers.len() > 64 {
            return Err(invalid("Too many pointer barriers"));
        }
        let barriers = barriers
            .into_iter()
            .map(|barrier| {
                let id = u32::try_from(barrier.get("barrier_id").ok_or_else(|| invalid("Missing barrier ID"))?)
                    .map_err(|_| invalid("Invalid barrier ID"))?;
                let position = barrier
                    .get("position")
                    .ok_or_else(|| invalid("Missing barrier position"))?
                    .try_clone()
                    .map_err(|_| invalid("Invalid position"))?;
                let position: (i32, i32, i32, i32) =
                    position.try_into().map_err(|_| invalid("Invalid barrier position"))?;
                Ok(json!({"id":id,"position":[position.0,position.1,position.2,position.3]}))
            })
            .collect::<zbus::fdo::Result<Vec<_>>>()?;
        let cancel = self.requests.begin(connection, &header, &handle).await?;
        let result = tokio::select! {
            biased;
            _ = cancel.wait() => None,
            _ = session.cancel.wait() => None,
            result = bridge.call("input-capture-barriers", json!({"session":id,"zone_set":zone_set,"barriers":barriers})) => Some(result),
        };
        self.requests.end(connection, &handle).await;
        if cancel.stopped.load(Ordering::SeqCst) || session.cancel.stopped.load(Ordering::SeqCst) || result.is_none() {
            self.end(connection, session_handle.as_str()).await;
            return Ok((1, Options::new()));
        }
        let result = self
            .finish_native_call(connection, &session_handle, &bridge, result.unwrap())
            .await?;
        let failed_barriers: Vec<u32> = serde_json::from_value(result["failed_barriers"].clone())
            .map_err(|_| invalid("Invalid native barrier response"))?;
        Ok((
            0,
            Options::from([(
                "failed_barriers".into(),
                Value::from(failed_barriers)
                    .try_to_owned()
                    .map_err(|e| failed(e.to_string()))?,
            )]),
        ))
    }

    #[zbus(name = "ConnectToEIS")]
    async fn connect_to_eis(
        &self,
        session_handle: OwnedObjectPath,
        app_id: String,
        _options: Options,
        #[zbus(connection)] connection: &Connection,
        #[zbus(header)] header: Header<'_>,
    ) -> zbus::fdo::Result<OwnedFd> {
        let session = self.session(connection, &header, &session_handle, &app_id).await?;
        Self::native(&session).await?;
        let mut state = session.state.lock().await;
        if state.worker.is_some() {
            return Err(invalid("EIS has already been connected"));
        }
        let capabilities = state.capabilities;
        let keymap = state.keymap.clone();
        let cancel = session.cancel.clone();
        let (worker, fd) = tokio::task::spawn_blocking(move || Worker::start(capabilities, &keymap, cancel))
            .await
            .map_err(|e| failed(e.to_string()))?
            .map_err(failed)?;
        state.worker = Some(worker);
        Ok(fd.into())
    }

    async fn enable(
        &self,
        session_handle: OwnedObjectPath,
        app_id: String,
        _options: Options,
        #[zbus(connection)] connection: &Connection,
        #[zbus(header)] header: Header<'_>,
    ) -> zbus::fdo::Result<(u32, Options)> {
        let session = self.session(connection, &header, &session_handle, &app_id).await?;
        let (bridge, id) = Self::native(&session).await?;
        let ready = session
            .state
            .lock()
            .await
            .worker
            .as_ref()
            .map(|worker| worker.ready.clone())
            .ok_or_else(|| invalid("Connect to EIS before enabling input capture"))?;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while !ready.load(Ordering::SeqCst) {
            if session.cancel.stopped.load(Ordering::SeqCst) || tokio::time::Instant::now() >= deadline {
                return Err(invalid("EIS receiver has not bound an input device"));
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let result = bridge.call("input-capture-enable", json!({"session":id})).await;
        self.finish_native_call(connection, &session_handle, &bridge, result)
            .await?;
        Ok((0, Options::new()))
    }

    async fn disable(
        &self,
        session_handle: OwnedObjectPath,
        app_id: String,
        _options: Options,
        #[zbus(connection)] connection: &Connection,
        #[zbus(header)] header: Header<'_>,
    ) -> zbus::fdo::Result<(u32, Options)> {
        let session = self.session(connection, &header, &session_handle, &app_id).await?;
        let (bridge, id) = Self::native(&session).await?;
        let result = bridge.call("input-capture-disable", json!({"session":id})).await;
        self.finish_native_call(connection, &session_handle, &bridge, result)
            .await?;
        Ok((0, Options::new()))
    }

    async fn release(
        &self,
        session_handle: OwnedObjectPath,
        app_id: String,
        options: Options,
        #[zbus(connection)] connection: &Connection,
        #[zbus(header)] header: Header<'_>,
    ) -> zbus::fdo::Result<(u32, Options)> {
        let session = self.session(connection, &header, &session_handle, &app_id).await?;
        let (bridge, id) = Self::native(&session).await?;
        let activation = options
            .get("activation_id")
            .map(u32::try_from)
            .transpose()
            .map_err(|_| invalid("Invalid activation ID"))?;
        let position = options
            .get("cursor_position")
            .map(|value| {
                let position: (f64, f64) = value
                    .try_clone()
                    .map_err(|_| invalid("Invalid cursor position"))?
                    .try_into()
                    .map_err(|_| invalid("Invalid cursor position"))?;
                if !position.0.is_finite() || !position.1.is_finite() {
                    return Err(invalid("Invalid cursor position"));
                }
                Ok(position)
            })
            .transpose()?;
        let result = bridge
            .call(
                "input-capture-release",
                json!({"session":id,"activation_id":activation,"cursor_position":position}),
            )
            .await;
        self.finish_native_call(connection, &session_handle, &bridge, result)
            .await?;
        Ok((0, Options::new()))
    }

    #[zbus(signal)]
    async fn activated(
        emitter: &SignalEmitter<'_>,
        session_handle: OwnedObjectPath,
        options: Options,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn deactivated(
        emitter: &SignalEmitter<'_>,
        session_handle: OwnedObjectPath,
        options: Options,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn disabled(
        emitter: &SignalEmitter<'_>,
        session_handle: OwnedObjectPath,
        options: Options,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn zones_changed(
        emitter: &SignalEmitter<'_>,
        session_handle: OwnedObjectPath,
        options: Options,
    ) -> zbus::Result<()>;
}

struct SessionObject {
    backend: InputCapture,
    path: String,
    owner: String,
}

#[zbus::interface(name = "org.freedesktop.impl.portal.Session")]
impl SessionObject {
    #[zbus(property, name = "version")]
    fn version(&self) -> u32 {
        1
    }

    async fn close(
        &self,
        #[zbus(connection)] connection: &Connection,
        #[zbus(header)] header: Header<'_>,
    ) -> zbus::fdo::Result<()> {
        if header.sender().map(|owner| owner.as_str()) != Some(self.owner.as_str()) {
            return Err(zbus::fdo::Error::AccessDenied("Wrong session owner".into()));
        }
        self.backend.end(connection, &self.path).await;
        Ok(())
    }

    #[zbus(signal)]
    async fn closed(emitter: &SignalEmitter<'_>) -> zbus::Result<()>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn broken_control_transport_ends_capture_but_valid_rejection_does_not() {
        use std::process::Stdio;
        use tokio::io::{AsyncBufReadExt, BufReader};

        let mut bus = tokio::process::Command::new("dbus-daemon")
            .args(["--session", "--nofork", "--print-address=1"])
            .stdout(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let mut address = String::new();
        BufReader::new(bus.stdout.take().unwrap())
            .read_line(&mut address)
            .await
            .unwrap();
        let connection = zbus::connection::Builder::address(address.trim())
            .unwrap()
            .build()
            .await
            .unwrap();

        for transport_failure in [false, true] {
            let capture = InputCapture::default();
            let path: OwnedObjectPath = "/org/freedesktop/portal/desktop/session/test/capture"
                .try_into()
                .unwrap();
            let (control, mut peer) = Bridge::test_pair();
            let (watch, _peer) = Bridge::test_pair();
            let session = Arc::new(Session {
                owner: connection.unique_name().unwrap().to_string(),
                app: "test".into(),
                cancel: Arc::default(),
                state: Mutex::new(State {
                    started: true,
                    bridge: Some(control.clone()),
                    watch: Some(watch.clone()),
                    ..State::default()
                }),
            });
            capture.sessions.lock().await.insert(path.to_string(), session.clone());
            let response = std::thread::spawn(move || {
                let request: ferese_ipc::Request = ferese_ipc::read_frame(&mut peer).unwrap();
                if !transport_failure {
                    ferese_ipc::write_frame(&mut peer, &ferese_ipc::Response::error(request.id, "denied", "Denied"))
                        .unwrap();
                }
            });
            let result = control.call("input-capture-disable", json!({})).await;
            assert!(
                capture
                    .finish_native_call(&connection, &path, &control, result)
                    .await
                    .is_err()
            );
            response.join().unwrap();
            assert_eq!(session.cancel.stopped.load(Ordering::SeqCst), transport_failure);
            assert_eq!(watch.is_closed(), transport_failure);
            assert_eq!(
                capture.sessions.lock().await.contains_key(path.as_str()),
                !transport_failure
            );
            capture.end(&connection, path.as_str()).await;
        }
        bus.kill().await.unwrap();
        bus.wait().await.unwrap();
    }

    #[test]
    fn requested_capabilities_are_required_and_supported_subset_is_returned() {
        assert!(capabilities(&Options::new()).is_err());
        for requested in [0u32, 4, 8] {
            assert!(capabilities(&Options::from([("capabilities".into(), requested.into())])).is_err());
        }
        for (requested, supported) in [(1u32, 1), (2, 2), (3, 3), (7, 3), (9, 1)] {
            assert_eq!(
                capabilities(&Options::from([("capabilities".into(), requested.into())])).unwrap(),
                supported
            );
        }
    }
}
