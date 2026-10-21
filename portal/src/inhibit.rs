use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use tokio::sync::{Mutex, Notify};
use zbus::Connection;
use zbus::message::Header;
use zbus::object_server::SignalEmitter;
use zbus::zvariant::{OwnedFd, OwnedObjectPath, Value};

use crate::backend::{Cancel, Options, authorize};
use crate::bridge::Bridge;
use crate::desktop::Requests;

const PATH: &str = "/org/freedesktop/portal/desktop";

struct Monitor {
    owner: String,
    cancel: Arc<Cancel>,
    last: Mutex<Option<(bool, u32, u64, u32)>>,
    bridge: Bridge,
    native_owner: u64,
}

#[derive(Clone)]
pub(crate) struct Inhibit {
    requests: Requests,
    monitors: Arc<Mutex<HashMap<String, Arc<Monitor>>>>,
    changed: Arc<Notify>,
}

impl Default for Inhibit {
    fn default() -> Self {
        Self {
            requests: Requests::with_limit(24),
            monitors: Default::default(),
            changed: Default::default(),
        }
    }
}

impl Inhibit {
    pub(crate) async fn revoke_stale(&self, connection: &Connection, owner: Option<&str>) {
        self.requests.revoke_stale(owner).await;
        let stale = self
            .monitors
            .lock()
            .await
            .iter()
            .filter(|(_, monitor)| Some(monitor.owner.as_str()) != owner)
            .map(|(path, _)| path.clone())
            .collect::<Vec<_>>();
        for path in stale {
            self.end(connection, &path).await;
        }
    }

    async fn end(&self, connection: &Connection, path: &str) {
        if let Some(monitor) = self.monitors.lock().await.remove(path) {
            let _state = monitor.last.lock().await;
            monitor.cancel.stop();
            monitor.bridge.close();
            if let Ok(emitter) = SignalEmitter::new(connection, path) {
                let _ = MonitorObject::closed(&emitter).await;
            }
            let _ = connection
                .object_server()
                .remove::<MonitorObject, _>(path)
                .await;
            self.changed.notify_one();
        }
    }

    pub(crate) async fn watch(self, connection: Connection) {
        let mut bridge: Option<Bridge> = None;
        let mut since = None;
        loop {
            if self.monitors.lock().await.is_empty() {
                if let Some(bridge) = bridge.take() {
                    bridge.close();
                }
                since = None;
                self.changed.notified().await;
                continue;
            }
            if bridge.is_none() {
                bridge = Bridge::connect().ok();
            }
            let result = if let Some(active) = &bridge {
                tokio::select! {
                    biased;
                    _ = self.changed.notified() => { active.close(); bridge = None; since = None; continue; }
                    state = async {
                        match since { Some(revision) => active.wait(revision).await, None => active.call("get-session-state", serde_json::json!({})).await }
                    } => state,
                }
            } else {
                Err("Session monitor is unavailable".into())
            };
            let monitors = self
                .monitors
                .lock()
                .await
                .iter()
                .map(|(path, monitor)| (path.clone(), monitor.clone()))
                .collect::<Vec<_>>();
            match result {
                Ok(state) => {
                    since = state["revision"].as_u64();
                    for (path, monitor) in monitors {
                        if !state["monitor-owners"].as_array().is_some_and(|owners| {
                            owners
                                .iter()
                                .any(|owner| owner.as_u64() == Some(monitor.native_owner))
                        }) {
                            self.end(&connection, &path).await;
                            continue;
                        }
                        let mut last = monitor.last.lock().await;
                        if monitor.cancel.stopped.load(Ordering::SeqCst) {
                            continue;
                        }
                        let transitions = match *last {
                            None => vec![state.clone()],
                            Some((_, _, revision, _)) => {
                                let events =
                                    state["transitions"].as_array().cloned().unwrap_or_default();
                                if events
                                    .first()
                                    .and_then(|event| event["revision"].as_u64())
                                    .is_some_and(|first| first > revision.saturating_add(1))
                                {
                                    drop(last);
                                    self.end(&connection, &path).await;
                                    continue;
                                }
                                events
                                    .into_iter()
                                    .filter(|event| {
                                        event["revision"]
                                            .as_u64()
                                            .is_some_and(|next| next > revision)
                                    })
                                    .collect()
                            }
                        };
                        for event in transitions {
                            let locked = event["screensaver-active"].as_bool().unwrap_or(false);
                            let phase = event["session-state"].as_u64().unwrap_or(1) as u32;
                            let revision = event["revision"].as_u64().unwrap_or(0);
                            let token = event["query-token"].as_u64().unwrap_or(0) as u32;
                            let changed = last.as_ref().is_none_or(|old| {
                                old.0 != locked || old.1 != phase || (phase == 2 && old.3 != token)
                            });
                            if changed {
                                let values = HashMap::from([
                                    (
                                        "screensaver-active".into(),
                                        Value::from(locked).try_into().unwrap(),
                                    ),
                                    (
                                        "session-state".into(),
                                        Value::from(phase).try_into().unwrap(),
                                    ),
                                ]);
                                if let Ok(emitter) = SignalEmitter::new(&connection, PATH)
                                    && Self::state_changed(
                                        &emitter,
                                        OwnedObjectPath::try_from(path.clone()).unwrap(),
                                        values,
                                    )
                                    .await
                                    .is_err()
                                {
                                    break;
                                }
                            }
                            *last = Some((locked, phase, revision, token));
                        }
                    }
                }
                Err(error) => {
                    eprintln!("ferese-portal: {error}");
                    for (path, _) in monitors {
                        self.end(&connection, &path).await;
                    }
                    if let Some(bridge) = bridge.take() {
                        bridge.close();
                    }
                    since = None;
                }
            }
        }
    }

    pub(crate) async fn watch_logind(self) {
        use futures_util::StreamExt;
        loop {
            let result: Result<(), String> = async {
                let connection = Connection::system()
                    .await
                    .map_err(|error| error.to_string())?;
                let proxy = zbus::Proxy::new(
                    &connection,
                    "org.freedesktop.login1",
                    "/org/freedesktop/login1",
                    "org.freedesktop.login1.Manager",
                )
                .await
                .map_err(|error| error.to_string())?;
                let mut signals = proxy
                    .receive_signal("PrepareForShutdown")
                    .await
                    .map_err(|error| error.to_string())?;
                while let Some(message) = signals.next().await {
                    let (ending,): (bool,) = message
                        .body()
                        .deserialize()
                        .map_err(|error| error.to_string())?;
                    if let Ok(bridge) = Bridge::connect() {
                        let _ = bridge
                            .call(
                                "logind-session-ending",
                                serde_json::json!({"ending": ending}),
                            )
                            .await;
                        bridge.close();
                    }
                }
                Ok(())
            }
            .await;
            if let Err(error) = result {
                eprintln!("ferese-portal: logind monitor unavailable: {error}");
            }
            tokio::time::sleep(Duration::from_secs(10)).await;
        }
    }
}

fn flags_what(flags: u32) -> zbus::fdo::Result<String> {
    if flags == 0 || flags & !15 != 0 {
        return Err(zbus::fdo::Error::InvalidArgs(
            "Invalid inhibition flags".into(),
        ));
    }
    let mut what = Vec::new();
    if flags & 1 != 0 {
        what.push("shutdown");
    }
    if flags & 4 != 0 {
        what.push("sleep");
    }
    if flags & 8 != 0 {
        what.push("idle");
    }
    Ok(what.join(":"))
}

async fn login_inhibitor(what: &str, app: &str, reason: &str) -> Result<Option<OwnedFd>, String> {
    if what.is_empty() {
        return Ok(None);
    }
    tokio::time::timeout(Duration::from_secs(5), async {
        let connection = Connection::system()
            .await
            .map_err(|error| error.to_string())?;
        let proxy = zbus::Proxy::new(
            &connection,
            "org.freedesktop.login1",
            "/org/freedesktop/login1",
            "org.freedesktop.login1.Manager",
        )
        .await
        .map_err(|error| error.to_string())?;
        let fd: OwnedFd = proxy
            .call("Inhibit", &(what, app, reason, "block"))
            .await
            .map_err(|error| error.to_string())?;
        Ok(Some(fd))
    })
    .await
    .map_err(|_| "Session inhibition timed out".to_owned())?
}

#[zbus::interface(name = "org.freedesktop.impl.portal.Inhibit")]
impl Inhibit {
    #[allow(
        clippy::too_many_arguments,
        reason = "Signature is defined by the portal D-Bus interface"
    )]
    async fn inhibit(
        &self,
        handle: OwnedObjectPath,
        app_id: String,
        _window: String,
        flags: u32,
        options: Options,
        #[zbus(connection)] connection: &Connection,
        #[zbus(header)] header: Header<'_>,
    ) -> zbus::fdo::Result<()> {
        authorize(connection, &header).await?;
        let what = flags_what(flags)?;
        let reason = options
            .get("reason")
            .map(|value| <&str>::try_from(value).map(str::to_owned))
            .transpose()
            .map_err(|_| zbus::fdo::Error::InvalidArgs("Invalid inhibition reason".into()))?
            .unwrap_or_else(|| "Application requested session inhibition".into());
        if app_id.len() > 512
            || reason.len() > 1024
            || app_id.chars().chain(reason.chars()).any(char::is_control)
        {
            return Err(zbus::fdo::Error::InvalidArgs(
                "Invalid application or reason".into(),
            ));
        }
        let cancel = self.requests.begin(connection, &header, &handle).await?;
        let bridge = match Bridge::connect() {
            Ok(bridge) => bridge,
            Err(error) => {
                self.requests.end(connection, &handle).await;
                return Err(zbus::fdo::Error::Failed(error));
            }
        };
        let acquired = tokio::select! {
            biased;
            _ = cancel.wait() => Err("Inhibition was cancelled".to_owned()),
            result = async {
                let fd = login_inhibitor(&what, &app_id, &reason).await?;
                bridge.call("portal-inhibit", serde_json::json!({"flags": flags, "app": app_id, "reason": reason})).await?;
                Ok(fd)
            } => result,
        };
        match acquired {
            Ok(fd) if !cancel.stopped.load(Ordering::SeqCst) => {
                let requests = self.requests.clone();
                let connection = connection.clone();
                tokio::spawn(async move {
                    tokio::select! { _ = cancel.wait() => {}, _ = bridge.closed() => {} }
                    cancel.stop();
                    bridge.close();
                    drop(fd);
                    requests.end(&connection, &handle).await;
                });
                Ok(())
            }
            result => {
                bridge.close();
                self.requests.end(connection, &handle).await;
                Err(zbus::fdo::Error::Failed(
                    result
                        .err()
                        .unwrap_or_else(|| "Inhibition was cancelled".into()),
                ))
            }
        }
    }

    async fn create_monitor(
        &self,
        handle: OwnedObjectPath,
        session_handle: OwnedObjectPath,
        app_id: String,
        _window: String,
        #[zbus(connection)] connection: &Connection,
        #[zbus(header)] header: Header<'_>,
    ) -> zbus::fdo::Result<u32> {
        let owner = authorize(connection, &header).await?;
        if !session_handle
            .as_str()
            .starts_with(&format!("{PATH}/session/"))
            || session_handle.as_str().len() > 512
            || app_id.len() > 512
        {
            return Err(zbus::fdo::Error::InvalidArgs(
                "Invalid session monitor".into(),
            ));
        }
        let cancel = self.requests.begin(connection, &header, &handle).await?;
        let mut monitors = self.monitors.lock().await;
        let result = if monitors.len() >= 8
            || monitors.contains_key(session_handle.as_str())
            || cancel.stopped.load(Ordering::SeqCst)
        {
            Ok(2)
        } else {
            async {
                let bridge = Bridge::connect().map_err(zbus::fdo::Error::Failed)?;
                let result = tokio::select! {
                    biased;
                    _ = cancel.wait() => Err("Monitor creation was cancelled".into()),
                    result = bridge.call("portal-monitor-register", serde_json::json!({})) => result,
                };
                let native = match result {
                    Ok(native) => native,
                    Err(error) => {
                        bridge.close();
                        return Err(zbus::fdo::Error::Failed(error));
                    }
                };
                let Some(native_owner) = native["monitor-owner"].as_u64() else {
                    bridge.close();
                    return Err(zbus::fdo::Error::Failed("Missing monitor owner".into()));
                };
                let exported = connection
                    .object_server()
                    .at(
                        &session_handle,
                        MonitorObject {
                            backend: self.clone(),
                            path: session_handle.to_string(),
                            owner: owner.clone(),
                            cancel: cancel.clone(),
                        },
                    )
                    .await;
                match exported {
                    Ok(true) if !cancel.stopped.load(Ordering::SeqCst) => {
                        monitors.insert(
                            session_handle.to_string(),
                            Arc::new(Monitor {
                                owner,
                                cancel: cancel.clone(),
                                last: Mutex::new(None),
                                bridge,
                                native_owner,
                            }),
                        );
                        self.changed.notify_one();
                        Ok(0)
                    }
                    result => {
                        bridge.close();
                        if matches!(result, Ok(true)) {
                            let _ = connection
                                .object_server()
                                .remove::<MonitorObject, _>(&session_handle)
                                .await;
                        }
                        result
                            .map(|_| if cancel.stopped.load(Ordering::SeqCst) { 1 } else { 2 })
                            .map_err(Into::into)
                    }
                }
            }
            .await
        };
        drop(monitors);
        self.requests.end(connection, &handle).await;
        result
    }

    async fn query_end_response(
        &self,
        session_handle: OwnedObjectPath,
        #[zbus(connection)] connection: &Connection,
        #[zbus(header)] header: Header<'_>,
    ) -> zbus::fdo::Result<()> {
        let owner = authorize(connection, &header).await?;
        let monitor = self
            .monitors
            .lock()
            .await
            .get(session_handle.as_str())
            .filter(|monitor| monitor.owner == owner)
            .cloned()
            .ok_or_else(|| zbus::fdo::Error::AccessDenied("Not your session monitor".into()))?;
        let last = monitor.last.lock().await;
        if monitor.cancel.stopped.load(Ordering::SeqCst) {
            return Err(zbus::fdo::Error::AccessDenied(
                "Session monitor closed".into(),
            ));
        }
        if let Some((_, 2, _, token)) = *last {
            monitor
                .bridge
                .call("portal-monitor-ack", serde_json::json!({"token": token}))
                .await
                .map_err(zbus::fdo::Error::Failed)?;
        }
        Ok(())
    }

    #[zbus(signal)]
    async fn state_changed(
        emitter: &SignalEmitter<'_>,
        session_handle: OwnedObjectPath,
        state: Options,
    ) -> zbus::Result<()>;
}

struct MonitorObject {
    backend: Inhibit,
    path: String,
    owner: String,
    cancel: Arc<Cancel>,
}

#[zbus::interface(name = "org.freedesktop.impl.portal.Session")]
impl MonitorObject {
    #[zbus(property, name = "version")]
    fn version(&self) -> u32 {
        1
    }

    async fn close(
        &self,
        #[zbus(connection)] connection: &Connection,
        #[zbus(header)] header: Header<'_>,
    ) -> zbus::fdo::Result<()> {
        if authorize(connection, &header).await? != self.owner {
            return Err(zbus::fdo::Error::AccessDenied(
                "Not your session monitor".into(),
            ));
        }
        self.cancel.stop();
        self.backend.end(connection, &self.path).await;
        Ok(())
    }

    #[zbus(signal)]
    async fn closed(emitter: &SignalEmitter<'_>) -> zbus::Result<()>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logind_flags_are_validated_and_mapped_independently() {
        assert_eq!(flags_what(1 | 4 | 8).unwrap(), "shutdown:sleep:idle");
        assert_eq!(flags_what(2).unwrap(), "");
        assert!(flags_what(0).is_err());
        assert!(flags_what(16).is_err());
    }
}
