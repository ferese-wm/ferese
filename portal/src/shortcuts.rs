use std::collections::HashMap;
use std::io::Read;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use serde_json::json;
use tokio::io::AsyncWriteExt;
use tokio::sync::Mutex;
use zbus::Connection;
use zbus::message::Header;
use zbus::object_server::SignalEmitter;
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};

use crate::backend::{Cancel, Options, authorize};
use crate::bridge::Bridge;
use crate::consent::{Prompt, ShortcutField};
use crate::desktop::Requests;

const PATH: &str = "/org/freedesktop/portal/desktop";
type Shortcuts = Vec<(String, Options)>;

struct Session {
    owner: String,
    app: String,
    cancel: Arc<Cancel>,
    state: Mutex<SessionState>,
}

struct SessionState {
    fields: Vec<ShortcutField>,
    bound: bool,
    busy: bool,
    bridge: Option<Bridge>,
    revision: u64,
}

#[derive(Clone, Default)]
pub(crate) struct GlobalShortcuts {
    sessions: Arc<Mutex<HashMap<String, Arc<Session>>>>,
    requests: Requests,
}

impl GlobalShortcuts {
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
    ) -> zbus::fdo::Result<Arc<Session>> {
        let owner = authorize(connection, header).await?;
        self.sessions
            .lock()
            .await
            .get(path.as_str())
            .filter(|session| session.owner == owner)
            .cloned()
            .ok_or_else(|| zbus::fdo::Error::AccessDenied("Not your shortcut session".into()))
    }

    pub(crate) async fn revoke_stale(&self, connection: &Connection, owner: Option<&str>) {
        let paths = self
            .sessions
            .lock()
            .await
            .iter()
            .filter(|(_, session)| Some(session.owner.as_str()) != owner)
            .map(|(path, _)| path.clone())
            .collect::<Vec<_>>();
        for path in paths {
            self.end(connection, &path).await;
        }
    }

    async fn end(&self, connection: &Connection, path: &str) {
        if let Some(session) = self.sessions.lock().await.remove(path) {
            let state = session.state.lock().await;
            session.cancel.stop();
            if let Some(bridge) = &state.bridge {
                bridge.close();
            }
            if let Ok(emitter) = SignalEmitter::new(connection, path) {
                let _ = SessionObject::closed(&emitter).await;
            }
            let _ = connection
                .object_server()
                .remove::<SessionObject, _>(path)
                .await;
        }
    }

    async fn configure(
        &self,
        connection: &Connection,
        path: &OwnedObjectPath,
        session: Arc<Session>,
        parent: &str,
        fields: Vec<ShortcutField>,
        cancel: Arc<Cancel>,
    ) -> Result<Option<Shortcuts>, String> {
        let (bridge, first, previous) = {
            let state = session.state.lock().await;
            let first = state.bridge.is_none();
            let bridge = match &state.bridge {
                Some(bridge) => bridge.clone(),
                None => Bridge::connect()?,
            };
            (bridge, first, state.fields.clone())
        };
        let mut fields = fields;
        let mut message = "Choose shortcuts for this application. Leave a shortcut empty to disable it. System shortcuts cannot be overridden.".to_owned();

        loop {
            let selected = tokio::select! {
                biased;
                _ = session.cancel.wait() => return Ok(None),
                _ = cancel.wait() => return Ok(None),
                selected = dialog(&session.app, parent, &message, fields.clone()) => selected?,
            };
            let Some(selected) = selected else {
                return Ok(None);
            };

            validate_selection(&fields, &selected)?;

            let registration = selected
                .iter()
                .filter(|field| !field.trigger.trim().is_empty())
                .map(|field| json!({"id":field.id,"trigger":field.trigger.trim()}))
                .collect::<Vec<_>>();
            match bridge
                .call("portal-shortcuts-register", json!(registration))
                .await
            {
                Ok(_) => {
                    let mut state = session.state.lock().await;

                    if session
                        .cancel
                        .stopped
                        .load(std::sync::atomic::Ordering::SeqCst)
                        || cancel.stopped.load(std::sync::atomic::Ordering::SeqCst)
                    {
                        if first
                            || session
                                .cancel
                                .stopped
                                .load(std::sync::atomic::Ordering::SeqCst)
                        {
                            bridge.close();
                        } else {
                            let previous = previous
                                .iter()
                                .filter(|field| !field.trigger.trim().is_empty())
                                .map(|field| json!({"id":field.id,"trigger":field.trigger}))
                                .collect::<Vec<_>>();

                            if bridge
                                .call("portal-shortcuts-register", json!(previous))
                                .await
                                .is_err()
                            {
                                bridge.close();
                            }
                        }

                        return Ok(None);
                    }

                    state.fields = selected.clone();
                    state.bound = true;
                    state.bridge = Some(bridge.clone());
                    state.revision = state.revision.wrapping_add(1);
                    drop(state);

                    if let Err(error) = save(&session.app, &selected) {
                        eprintln!("ferese shortcuts: could not save preferences: {error}");
                    }

                    if first {
                        let backend = self.clone();
                        let connection = connection.clone();
                        let path = path.clone();
                        tokio::spawn(async move {
                            backend.watch(connection, path, session, bridge).await;
                        });
                    }

                    return Ok(Some(results(&selected)));
                }
                Err(error) => {
                    message = format!("{error}\nChoose another shortcut.");
                    fields = selected;
                }
            }
        }
    }

    async fn watch(
        &self,
        connection: Connection,
        path: OwnedObjectPath,
        session: Arc<Session>,
        bridge: Bridge,
    ) {
        let emitter = SignalEmitter::new(&connection, PATH)
            .expect("the portal exports signals at a valid static object path");
        loop {
            tokio::select! {
                _ = session.cancel.wait() => break,
                _ = tokio::time::sleep(Duration::from_millis(50)) => {},
            }
            let revision = session.state.lock().await.revision;
            let value = match bridge.call("portal-shortcuts-poll", json!({})).await {
                Ok(value) if value["closed"] != true => value,
                _ => break,
            };
            let mut state = session.state.lock().await;
            if session
                .cancel
                .stopped
                .load(std::sync::atomic::Ordering::SeqCst)
            {
                break;
            }
            if let Some(active) = value["shortcuts"]
                .as_array()
                .filter(|_| !state.busy && state.revision == revision)
            {
                let mut changed = false;
                for field in &mut state.fields {
                    if !field.trigger.is_empty()
                        && !active.iter().any(|item| item["id"] == field.id)
                    {
                        field.trigger.clear();
                        changed = true;
                    }
                }
                if changed {
                    let _ = Self::shortcuts_changed(&emitter, path.clone(), results(&state.fields))
                        .await;
                }
            }
            for event in value["events"].as_array().into_iter().flatten() {
                let Some(id) = event["id"].as_str() else {
                    continue;
                };
                let timestamp = event["timestamp"].as_u64().unwrap_or(0);
                if event["active"] == true {
                    let _ = Self::activated(&emitter, path.clone(), id, timestamp, Options::new())
                        .await;
                } else {
                    let _ =
                        Self::deactivated(&emitter, path.clone(), id, timestamp, Options::new())
                            .await;
                }
            }
        }
        self.end(&connection, path.as_str()).await;
    }
}

#[zbus::interface(name = "org.freedesktop.impl.portal.GlobalShortcuts")]
impl GlobalShortcuts {
    #[zbus(property, name = "version")]
    fn version(&self) -> u32 {
        2
    }

    async fn create_session(
        &self,
        handle: OwnedObjectPath,
        session_handle: OwnedObjectPath,
        app_id: String,
        _options: Options,
        #[zbus(connection)] connection: &Connection,
        #[zbus(header)] header: Header<'_>,
    ) -> zbus::fdo::Result<(u32, Options)> {
        let owner = authorize(connection, &header).await?;
        if !session_handle
            .as_str()
            .starts_with(&format!("{PATH}/session/"))
            || session_handle.as_str().len() > 512
            || app_id.len() > 512
        {
            return Err(zbus::fdo::Error::InvalidArgs(
                "Invalid shortcut session".into(),
            ));
        }
        let cancel = self.requests.begin(connection, &header, &handle).await?;
        let mut sessions = self.sessions.lock().await;
        let result = if sessions.len() >= 8
            || sessions.contains_key(session_handle.as_str())
            || cancel.stopped.load(std::sync::atomic::Ordering::SeqCst)
        {
            Ok((2, Options::new()))
        } else {
            match connection
                .object_server()
                .at(
                    &session_handle,
                    SessionObject {
                        backend: self.clone(),
                        path: session_handle.to_string(),
                        owner: owner.clone(),
                    },
                )
                .await
            {
                Err(error) => Err(zbus::fdo::Error::Failed(error.to_string())),
                Ok(false) => Ok((2, Options::new())),
                Ok(true) if cancel.stopped.load(std::sync::atomic::Ordering::SeqCst) => {
                    let _ = connection
                        .object_server()
                        .remove::<SessionObject, _>(&session_handle)
                        .await;
                    Ok((1, Options::new()))
                }
                Ok(true) => {
                    let fields = load(&app_id).unwrap_or_default();
                    sessions.insert(
                        session_handle.to_string(),
                        Arc::new(Session {
                            owner,
                            app: app_id,
                            cancel: Arc::default(),
                            state: Mutex::new(SessionState {
                                fields,
                                bound: false,
                                busy: false,
                                bridge: None,
                                revision: 0,
                            }),
                        }),
                    );
                    Ok((0, Options::new()))
                }
            }
        };
        drop(sessions);
        self.requests.end(connection, &handle).await;
        result
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "Signature is defined by the portal D-Bus interface"
    )]
    async fn bind_shortcuts(
        &self,
        handle: OwnedObjectPath,
        session_handle: OwnedObjectPath,
        shortcuts: Shortcuts,
        parent_window: String,
        _options: Options,
        #[zbus(connection)] connection: &Connection,
        #[zbus(header)] header: Header<'_>,
    ) -> zbus::fdo::Result<(u32, Options)> {
        let session = self.session(connection, &header, &session_handle).await?;
        let mut fields = fields(shortcuts).map_err(zbus::fdo::Error::InvalidArgs)?;
        {
            let mut state = session.state.lock().await;
            if state.bound || state.busy {
                return Err(zbus::fdo::Error::InvalidArgs(
                    "Shortcuts already bound or being configured".into(),
                ));
            }
            for field in &mut fields {
                if let Some(saved) = state.fields.iter().find(|saved| saved.id == field.id) {
                    field.trigger = saved.trigger.clone();
                }
            }
            state.busy = true;
            state.revision = state.revision.wrapping_add(1);
        }
        let cancel = match self.requests.begin(connection, &header, &handle).await {
            Ok(cancel) => cancel,
            Err(error) => {
                session.state.lock().await.busy = false;
                return Err(error);
            }
        };
        let response = self
            .configure(
                connection,
                &session_handle,
                session.clone(),
                &parent_window,
                fields,
                cancel,
            )
            .await;
        session.state.lock().await.busy = false;
        self.requests.end(connection, &handle).await;
        Ok(reply(response))
    }

    async fn list_shortcuts(
        &self,
        handle: OwnedObjectPath,
        session_handle: OwnedObjectPath,
        #[zbus(connection)] connection: &Connection,
        #[zbus(header)] header: Header<'_>,
    ) -> zbus::fdo::Result<(u32, Options)> {
        let session = self.session(connection, &header, &session_handle).await?;
        self.requests.begin(connection, &header, &handle).await?;
        let result = results(&session.state.lock().await.fields);
        self.requests.end(connection, &handle).await;
        Ok((
            0,
            HashMap::from([(
                "shortcuts".into(),
                Value::from(result)
                    .try_into()
                    .expect("the D-Bus value uses an owned supported scalar or collection type"),
            )]),
        ))
    }

    async fn configure_shortcuts(
        &self,
        session_handle: OwnedObjectPath,
        parent_window: String,
        _options: Options,
        #[zbus(connection)] connection: &Connection,
        #[zbus(header)] header: Header<'_>,
    ) -> zbus::fdo::Result<()> {
        let session = self.session(connection, &header, &session_handle).await?;
        let fields = {
            let mut state = session.state.lock().await;
            if state.busy || !state.bound {
                return Err(zbus::fdo::Error::InvalidArgs(
                    "Shortcuts are not ready to configure".into(),
                ));
            }
            state.busy = true;
            state.revision = state.revision.wrapping_add(1);
            state.fields.clone()
        };
        let backend = self.clone();
        let connection = connection.clone();
        tokio::spawn(async move {
            let result = backend
                .configure(
                    &connection,
                    &session_handle,
                    session.clone(),
                    &parent_window,
                    fields,
                    Arc::default(),
                )
                .await;
            session.state.lock().await.busy = false;
            if let Ok(Some(shortcuts)) = result {
                let emitter = SignalEmitter::new(&connection, PATH)
                    .expect("the portal exports signals at a valid static object path");
                let _ = Self::shortcuts_changed(&emitter, session_handle, shortcuts).await;
            }
        });
        Ok(())
    }

    #[zbus(signal)]
    async fn activated(
        emitter: &SignalEmitter<'_>,
        session_handle: OwnedObjectPath,
        shortcut_id: &str,
        timestamp: u64,
        options: Options,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn deactivated(
        emitter: &SignalEmitter<'_>,
        session_handle: OwnedObjectPath,
        shortcut_id: &str,
        timestamp: u64,
        options: Options,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn shortcuts_changed(
        emitter: &SignalEmitter<'_>,
        session_handle: OwnedObjectPath,
        shortcuts: Shortcuts,
    ) -> zbus::Result<()>;
}

struct SessionObject {
    backend: GlobalShortcuts,
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
        if header.sender().map(|sender| sender.as_str()) != Some(self.owner.as_str()) {
            return Err(zbus::fdo::Error::AccessDenied(
                "Not your shortcut session".into(),
            ));
        }
        self.backend.end(connection, &self.path).await;
        Ok(())
    }

    #[zbus(signal)]
    async fn closed(emitter: &SignalEmitter<'_>) -> zbus::Result<()>;
}

fn fields(shortcuts: Shortcuts) -> Result<Vec<ShortcutField>, String> {
    if shortcuts.is_empty() || shortcuts.len() > 64 {
        return Err("Expected 1–64 shortcuts".into());
    }
    let mut fields: Vec<ShortcutField> = Vec::new();
    for (id, properties) in shortcuts {
        let description = properties
            .get("description")
            .and_then(|value| <&str>::try_from(value).ok())
            .ok_or("Missing shortcut description")?;
        let trigger = properties
            .get("preferred_trigger")
            .map(|value| <&str>::try_from(value).map(str::to_owned))
            .transpose()
            .map_err(|_| "Invalid shortcut trigger")?
            .unwrap_or_default();
        if id.is_empty()
            || id.len() > 256
            || description.is_empty()
            || description.len() > 1024
            || trigger.len() > 256
            || id.chars().chain(description.chars()).any(char::is_control)
            || fields.iter().any(|field| field.id == id)
        {
            return Err("Invalid or duplicate shortcut".into());
        }
        fields.push(ShortcutField {
            id,
            description: description.into(),
            trigger,
        });
    }
    Ok(fields)
}

fn validate_selection(
    requested: &[ShortcutField],
    selected: &[ShortcutField],
) -> Result<(), String> {
    if selected.len() != requested.len()
        || selected.iter().zip(requested).any(|(chosen, original)| {
            chosen.id != original.id
                || chosen.description != original.description
                || chosen.trigger.len() > 256
                || chosen.trigger.chars().any(char::is_control)
        })
    {
        return Err("Invalid shortcut selection".into());
    }
    Ok(())
}

fn results(fields: &[ShortcutField]) -> Shortcuts {
    fields
        .iter()
        .filter(|field| !field.trigger.trim().is_empty())
        .map(|field| {
            (
                field.id.clone(),
                HashMap::from([
                    (
                        "description".into(),
                        OwnedValue::from(zbus::zvariant::Str::from(field.description.as_str())),
                    ),
                    (
                        "trigger_description".into(),
                        OwnedValue::from(zbus::zvariant::Str::from(field.trigger.as_str())),
                    ),
                ]),
            )
        })
        .collect()
}

fn reply(response: Result<Option<Shortcuts>, String>) -> (u32, Options) {
    match response {
        Ok(Some(shortcuts)) => (
            0,
            HashMap::from([(
                "shortcuts".into(),
                Value::from(shortcuts)
                    .try_into()
                    .expect("the D-Bus value uses an owned supported scalar or collection type"),
            )]),
        ),
        Ok(None) => (1, Options::new()),
        Err(error) => {
            eprintln!("ferese shortcuts: {error}");
            (2, Options::new())
        }
    }
}

async fn dialog(
    app: &str,
    parent: &str,
    description: &str,
    fields: Vec<ShortcutField>,
) -> Result<Option<Vec<ShortcutField>>, String> {
    let prompt = Prompt {
        title: "Application shortcuts".into(),
        description: format!("{app}\n{description}"),
        accept: "Allow shortcuts".into(),
        parent: parent.into(),
        image: None,
        shortcuts: fields,
    };
    let mut child = crate::backend::child_command()?
        .arg("--consent")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|error| error.to_string())?;
    let mut input = child
        .stdin
        .take()
        .expect("the child was spawned with piped standard input");
    input
        .write_all(&serde_json::to_vec(&prompt).map_err(|error| error.to_string())?)
        .await
        .map_err(|error| error.to_string())?;
    drop(input);
    let output = tokio::time::timeout(Duration::from_secs(300), child.wait_with_output())
        .await
        .map_err(|_| "Shortcut configuration timed out")?
        .map_err(|error| error.to_string())?;
    if !output.status.success() || output.stdout.is_empty() {
        return Ok(None);
    }
    serde_json::from_slice(&output.stdout)
        .map(Some)
        .map_err(|error| error.to_string())
}

fn cache_path(app: &str) -> Option<std::path::PathBuf> {
    if app.is_empty()
        || app.len() > 200
        || !app
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
        || app.starts_with('.')
    {
        return None;
    }
    Some(
        dirs::data_local_dir()?
            .join("ferese/portal/shortcuts")
            .join(format!("{app}.json")),
    )
}

fn load(app: &str) -> Option<Vec<ShortcutField>> {
    use std::os::unix::fs::OpenOptionsExt;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW)
        .open(cache_path(app)?)
        .ok()?;
    if !file.metadata().ok()?.is_file() {
        return None;
    }
    let mut bytes = Vec::new();
    file.take(128 * 1024 + 1).read_to_end(&mut bytes).ok()?;
    if bytes.len() > 128 * 1024 {
        return None;
    }
    let saved: Vec<ShortcutField> = serde_json::from_slice(&bytes).ok()?;
    let shortcuts = saved
        .into_iter()
        .map(|field| {
            (
                field.id,
                HashMap::from([
                    (
                        "description".into(),
                        OwnedValue::from(zbus::zvariant::Str::from(field.description)),
                    ),
                    (
                        "preferred_trigger".into(),
                        OwnedValue::from(zbus::zvariant::Str::from(field.trigger)),
                    ),
                ]),
            )
        })
        .collect();
    fields(shortcuts).ok()
}

fn save(app: &str, fields: &[ShortcutField]) -> Result<(), String> {
    let Some(path) = cache_path(app) else {
        return Ok(());
    };
    let directory = path
        .parent()
        .expect("the portal configuration path identifies a file in a directory");
    std::fs::create_dir_all(directory).map_err(|error| error.to_string())?;
    let mut file = tempfile::NamedTempFile::new_in(directory).map_err(|error| error.to_string())?;
    std::io::Write::write_all(
        &mut file,
        &serde_json::to_vec(fields).map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    file.persist(path).map_err(|error| error.to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn choices_keep_requested_ids_and_descriptions_and_allow_disabling() {
        let request = vec![ShortcutField {
            id: "record".into(),
            description: "Record screen".into(),
            trigger: "Ctrl+Alt+r".into(),
        }];
        let mut selected = request.clone();
        selected[0].trigger.clear();
        assert!(validate_selection(&request, &selected).is_ok());
        assert!(results(&selected).is_empty());
        selected[0].id = "other".into();
        assert!(validate_selection(&request, &selected).is_err());
        let mut selected = request.clone();
        selected[0].trigger = "a\nb".into();
        assert!(validate_selection(&request, &selected).is_err());
    }

    #[test]
    fn duplicate_shortcuts_and_unsafe_cache_names_are_rejected() {
        let shortcut = (
            "record".into(),
            HashMap::from([(
                "description".into(),
                OwnedValue::from(zbus::zvariant::Str::from("Record screen")),
            )]),
        );
        assert!(fields(vec![shortcut.clone(), shortcut]).is_err());
        for app in ["../other", "/etc/file", ".hidden", "bad/name", ""] {
            assert!(cache_path(app).is_none());
        }
    }
}
