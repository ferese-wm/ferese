use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use ferese_ipc::events::Window;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;
use tokio::sync::RwLock;
use zbus::Connection;
use zbus::message::Header;
use zbus::object_server::SignalEmitter;
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};

use crate::backend::{Options, authorize};
use crate::desktop::{Requests, consent, reply};

const PATH: &str = "/org/freedesktop/portal/desktop";

#[derive(Clone)]
pub(crate) struct Usb(pub(crate) Requests);

#[zbus::interface(name = "org.freedesktop.impl.portal.Usb")]
impl Usb {
    #[zbus(property, name = "version")]
    fn version(&self) -> u32 {
        1
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "Signature is defined by the portal D-Bus interface"
    )]
    async fn acquire_devices(
        &self,
        handle: OwnedObjectPath,
        parent_window: String,
        app_id: String,
        devices: Vec<(String, Options, Options)>,
        _options: Options,
        #[zbus(connection)] connection: &Connection,
        #[zbus(header)] header: Header<'_>,
    ) -> zbus::fdo::Result<(u32, Options)> {
        let cancel = self.0.begin(connection, &header, &handle).await?;
        let result = tokio::select! {
            biased;
            _ = cancel.wait() => (1, Options::new()),
            result = usb_consent(&app_id, &parent_window, devices) => reply(result),
        };
        self.0.end(connection, &handle).await;
        Ok(result)
    }
}

fn usb_label(info: &Options, id: &str) -> String {
    let properties = info
        .get("properties")
        .and_then(|value| value.try_clone().ok())
        .and_then(|value| HashMap::<String, OwnedValue>::try_from(value).ok())
        .unwrap_or_default();
    let property = |keys: &[&str]| {
        keys.iter().find_map(|key| {
            properties
                .get(*key)
                .and_then(|value| <&str>::try_from(value).ok())
        })
    };
    let vendor = property(&["ID_VENDOR_FROM_DATABASE", "ID_VENDOR", "ID_VENDOR_ID"]);
    let model = property(&["ID_MODEL_FROM_DATABASE", "ID_MODEL", "ID_MODEL_ID"]);
    let label = [vendor, model]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" ");
    let label = if label.is_empty() { id } else { &label };
    label
        .chars()
        .filter(|character| !character.is_control())
        .take(256)
        .collect()
}

async fn usb_consent(
    app: &str,
    parent: &str,
    devices: Vec<(String, Options, Options)>,
) -> Result<Option<Options>, String> {
    if devices.is_empty() || devices.len() > 32 {
        return Err("Invalid USB device count".into());
    }
    let mut ids = HashSet::new();
    let mut selected = Vec::new();
    let mut description = "Allow access to these USB devices?\n".to_owned();
    for (id, info, options) in devices {
        if id.is_empty() || id.len() > 512 || !ids.insert(id.clone()) {
            return Err("Invalid or duplicate USB device identifier".into());
        }
        let writable = options
            .get("writable")
            .map(bool::try_from)
            .transpose()
            .map_err(|_| "Invalid USB access mode")?
            .unwrap_or(false);
        let label = usb_label(&info, &id);
        description.push_str(&format!(
            "\n{} — {}",
            label,
            if writable {
                "read and write"
            } else {
                "read only"
            }
        ));
        selected.push((
            id,
            HashMap::from([("writable".to_owned(), OwnedValue::from(writable))]),
        ));
    }
    if !consent(
        app,
        parent,
        "USB device access",
        &description,
        "Allow access",
        None,
    )
    .await?
    {
        return Ok(None);
    }
    Ok(Some(HashMap::from([(
        "devices".into(),
        Value::from(selected)
            .try_to_owned()
            .map_err(|error| error.to_string())?,
    )])))
}

#[derive(Clone)]
pub(crate) struct Background {
    requests: Requests,
    states: Arc<RwLock<HashMap<String, u32>>>,
}

impl Background {
    pub(crate) fn new(requests: Requests) -> Self {
        Self {
            requests,
            states: Default::default(),
        }
    }

    pub(crate) async fn watch(self, connection: Connection) {
        let emitter = SignalEmitter::new(&connection, PATH)
            .expect("the portal exports signals at a valid static object path");
        loop {
            let result = async {
                let path = ferese_ipc::socket::resolve(None).map_err(|error| error.to_string())?;
                let stream = UnixStream::connect(path)
                    .await
                    .map_err(|error| error.to_string())?;
                let mut events = BackgroundEvents::subscribe(stream).await?;
                loop {
                    let next = match events.next().await {
                        Ok(next) => next,
                        Err(error) => break Err::<(), String>(error),
                    };
                    if let Some(next) = next {
                        let mut previous = self.states.write().await;
                        if *previous != next {
                            *previous = next;
                            drop(previous);
                            let _ = Self::running_applications_changed(&emitter).await;
                        }
                    }
                }
            }
            .await;
            // An interrupted generation is no longer authoritative. Clear it
            // before waiting to reconnect, and signal only an actual change.
            let had_apps = {
                let mut states = self.states.write().await;
                let had_apps = !states.is_empty();
                states.clear();
                had_apps
            };
            if had_apps {
                let _ = Self::running_applications_changed(&emitter).await;
            }
            if let Err(error) = result {
                eprintln!("ferese portal: background event stream: {error}");
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    }
}

// Tokio owns the socket so cancelling the watch task also closes an idle stream.
struct BackgroundEvents {
    stream: UnixStream,
    windows: Vec<Window>,
    changed: bool,
}

impl BackgroundEvents {
    async fn subscribe(mut stream: UnixStream) -> Result<Self, String> {
        let request = ferese_ipc::Request {
            version: ferese_ipc::VERSION,
            id: 1,
            kind: "command".into(),
            command: "event-stream".into(),
            args: serde_json::json!({"version": ferese_ipc::events::VERSION}),
        };
        let mut bytes = Vec::new();
        ferese_ipc::write_frame(&mut bytes, &request).map_err(|error| error.to_string())?;
        tokio::time::timeout(Duration::from_secs(2), async {
            stream
                .write_all(&bytes)
                .await
                .map_err(|error| error.to_string())?;
            let response: ferese_ipc::Response = read_event_frame(&mut stream).await?;
            if response.version != ferese_ipc::VERSION || response.id != request.id {
                return Err("Unexpected event subscription response".into());
            }
            if let Some(error) = response.error {
                return Err(error.message);
            }
            if response
                .result
                .as_ref()
                .and_then(|value| value["version"].as_u64())
                != Some(u64::from(ferese_ipc::events::VERSION))
            {
                return Err("Unsupported event stream version".into());
            }
            Ok(())
        })
        .await
        .map_err(|_| "Event subscription timed out".to_owned())??;
        Ok(Self {
            stream,
            windows: Vec::new(),
            changed: false,
        })
    }

    async fn next(&mut self) -> Result<Option<HashMap<String, u32>>, String> {
        use ferese_ipc::events::{Change, Event};
        let event: Event = read_event_frame(&mut self.stream).await?;
        event.validate()?;
        match event.change {
            Change::Snapshot { desktop } => {
                self.windows = desktop.windows;
                self.changed = true;
            }
            Change::WindowsChanged { windows } => {
                self.windows = windows;
                self.changed = true;
            }
            Change::FocusChanged { focus } => {
                let focused = focus["window"].as_u64();
                for window in &mut self.windows {
                    window.focused = focused == Some(window.id);
                }
                self.changed = true;
            }
            _ => {}
        }
        if event.last && std::mem::take(&mut self.changed) {
            Ok(Some(app_states(&self.windows)))
        } else {
            Ok(None)
        }
    }
}

async fn read_event_frame<T: serde::de::DeserializeOwned>(
    stream: &mut UnixStream,
) -> Result<T, String> {
    let size = stream.read_u32().await.map_err(|error| error.to_string())? as usize;
    if size > ferese_ipc::MAX_PAYLOAD_SIZE {
        return Err("Oversized IPC event frame".into());
    }
    let mut bytes = vec![0; size];
    stream
        .read_exact(&mut bytes)
        .await
        .map_err(|error| error.to_string())?;
    serde_json::from_slice(&bytes).map_err(|error| error.to_string())
}

#[zbus::interface(name = "org.freedesktop.impl.portal.Background")]
impl Background {
    async fn get_app_state(
        &self,
        #[zbus(connection)] connection: &Connection,
        #[zbus(header)] header: Header<'_>,
    ) -> zbus::fdo::Result<Options> {
        authorize(connection, &header).await?;
        Ok(self
            .states
            .read()
            .await
            .iter()
            .map(|(app, state)| (app.clone(), (*state).into()))
            .collect())
    }

    async fn notify_background(
        &self,
        handle: OwnedObjectPath,
        app_id: String,
        name: String,
        #[zbus(connection)] connection: &Connection,
        #[zbus(header)] header: Header<'_>,
    ) -> zbus::fdo::Result<(u32, Options)> {
        let cancel = self.requests.begin(connection, &header, &handle).await?;
        let result = tokio::select! {
            biased;
            _ = cancel.wait() => (1, Options::new()),
            result = background_consent(&app_id, &name) => reply(result),
        };
        self.requests.end(connection, &handle).await;
        Ok(result)
    }

    async fn enable_autostart(
        &self,
        app_id: String,
        enable: bool,
        commandline: Vec<String>,
        flags: u32,
        #[zbus(connection)] connection: &Connection,
        #[zbus(header)] header: Header<'_>,
    ) -> zbus::fdo::Result<bool> {
        authorize(connection, &header).await?;
        let directory = dirs::config_dir()
            .ok_or_else(|| zbus::fdo::Error::Failed("Missing config directory".into()))?
            .join("autostart");
        set_autostart(&directory, &app_id, enable, &commandline, flags)
            .map_err(zbus::fdo::Error::InvalidArgs)
    }

    #[zbus(signal)]
    async fn running_applications_changed(emitter: &SignalEmitter<'_>) -> zbus::Result<()>;
}

async fn background_consent(app: &str, name: &str) -> Result<Option<Options>, String> {
    if name.len() > 512 {
        return Err("Background application name is too long".into());
    }
    let allow = consent(
        app,
        "",
        "Background activity",
        &format!("{name} wants to keep running after its windows close."),
        "Allow this time",
        None,
    )
    .await?;
    Ok(Some(HashMap::from([(
        "result".into(),
        OwnedValue::from(if allow { 2u32 } else { 0u32 }),
    )])))
}

fn app_states(windows: &[Window]) -> HashMap<String, u32> {
    let mut apps: HashMap<String, u32> = HashMap::new();
    for window in windows {
        let app = window.app_id.as_str();
        if app.is_empty() {
            continue;
        }
        let app = app.strip_suffix(".desktop").unwrap_or(app).to_owned();
        let state = if window.focused { 2 } else { 1 };
        apps.entry(app)
            .and_modify(|old| *old = (*old).max(state))
            .or_insert(state);
    }
    apps
}

fn set_autostart(
    directory: &Path,
    app: &str,
    enable: bool,
    argv: &[String],
    flags: u32,
) -> Result<bool, String> {
    if app.is_empty()
        || app.len() > 255
        || !app
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
        || app.starts_with('.')
        || flags & !1 != 0
    {
        return Err("Invalid autostart application or flags".into());
    }
    let path = directory.join(format!("{app}.desktop"));
    let previous = read_managed_entry(&path)?;
    if previous
        .as_ref()
        .is_some_and(|source| !source.lines().any(|line| line == "X-Ferese-Portal=true"))
    {
        return Err("Existing autostart entry is not managed by Ferese".into());
    }
    if !enable {
        match std::fs::remove_file(path) {
            Ok(()) => (),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
            Err(error) => return Err(error.to_string()),
        }
        return Ok(true);
    }
    if argv.len() > 128
        || (argv.first().is_none_or(|argument| argument.is_empty()) && flags & 1 == 0)
    {
        return Err("Invalid autostart command".into());
    }
    let exec = argv
        .iter()
        .map(|argument| desktop_argument(argument))
        .collect::<Result<Vec<_>, _>>()?
        .join(" ");
    let entry = format!(
        "[Desktop Entry]\nType=Application\nName={app}\nExec={exec}\nDBusActivatable={}\nX-Ferese-Portal=true\n",
        flags & 1 != 0
    );
    std::fs::create_dir_all(directory).map_err(|error| error.to_string())?;
    let mut file = tempfile::NamedTempFile::new_in(directory).map_err(|error| error.to_string())?;
    std::io::Write::write_all(&mut file, entry.as_bytes()).map_err(|error| error.to_string())?;
    if read_managed_entry(&path)? != previous {
        return Err("Autostart entry changed; please try again".into());
    }
    file.persist(path).map_err(|error| error.to_string())?;
    Ok(true)
}

fn read_managed_entry(path: &Path) -> Result<Option<String>, String> {
    use std::io::Read;
    match std::fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Ok(metadata) if metadata.is_file() && metadata.len() <= 1024 * 1024 => (),
        Ok(_) => return Err("Existing autostart entry is not a regular file".into()),
        Err(error) => return Err(error.to_string()),
    }
    let mut source = String::new();
    std::fs::File::open(path)
        .map_err(|error| error.to_string())?
        .take(1024 * 1024 + 1)
        .read_to_string(&mut source)
        .map_err(|error| error.to_string())?;
    if source.len() > 1024 * 1024 {
        return Err("Existing autostart entry is too large".into());
    }
    Ok(Some(source))
}

fn desktop_argument(argument: &str) -> Result<String, String> {
    if argument.len() > 4096 || argument.chars().any(|character| character.is_control()) {
        return Err("Invalid autostart argument".into());
    }
    let mut result = "\"".to_owned();
    for character in argument.chars() {
        match character {
            '%' => result.push_str("%%"),
            '\\' => result.push_str("\\\\\\\\"),
            '$' | '`' | '"' => {
                result.push_str("\\\\");
                result.push(character);
            }
            _ => result.push(character),
        }
    }
    result.push('"');
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn window(id: u64, app: &str, focused: bool, workspace: u64) -> Window {
        Window {
            id,
            app_id: app.into(),
            title: format!("Window {id}"),
            focused,
            workspace,
            floating: false,
            fullscreen: false,
        }
    }

    async fn send_frame(peer: &mut UnixStream, value: &impl serde::Serialize) {
        let mut bytes = Vec::new();
        ferese_ipc::write_frame(&mut bytes, value).unwrap();
        peer.write_all(&bytes).await.unwrap();
    }

    #[tokio::test]
    async fn background_events_apply_complete_generations_and_close_on_drop() {
        use ferese_ipc::events::{Change, Event};
        let (client, mut peer) = UnixStream::pair().unwrap();
        let server = tokio::spawn(async move {
            let request: ferese_ipc::Request = read_event_frame(&mut peer).await.unwrap();
            assert_eq!(request.command, "event-stream");
            assert_eq!(
                request.args,
                serde_json::json!({"version": ferese_ipc::events::VERSION})
            );
            send_frame(
                &mut peer,
                &ferese_ipc::Response::success(request.id, request.args),
            )
            .await;
            peer
        });
        let mut events = BackgroundEvents::subscribe(client).await.unwrap();
        let mut peer = server.await.unwrap();
        send_frame(
            &mut peer,
            &Event {
                version: ferese_ipc::events::VERSION,
                generation: 0,
                last: true,
                change: Change::Snapshot {
                    desktop: Box::new(ferese_ipc::events::Snapshot {
                        outputs: serde_json::json!([]).into(),
                        workspaces: serde_json::json!([]),
                        windows: vec![window(1, "one.desktop", true, 1)],
                        focus: serde_json::json!({"window":1}),
                        config: Default::default(),
                        theme: serde_json::Value::Null.into(),
                        lock: ferese_ipc::events::LockState {
                            phase: "unlocked".into(),
                            sleeping: false,
                        },
                    }),
                },
            },
        )
        .await;
        assert_eq!(
            events.next().await.unwrap().unwrap(),
            HashMap::from([("one".into(), 2)])
        );
        let windows = vec![
            window(1, "one.desktop", true, 1),
            window(2, "two", false, 2),
        ];
        send_frame(
            &mut peer,
            &Event {
                version: ferese_ipc::events::VERSION,
                generation: 1,
                last: false,
                change: Change::WindowsChanged { windows },
            },
        )
        .await;
        assert!(events.next().await.unwrap().is_none());
        send_frame(
            &mut peer,
            &Event {
                version: ferese_ipc::events::VERSION,
                generation: 1,
                last: true,
                change: Change::FocusChanged {
                    focus: serde_json::json!({"window":2}),
                },
            },
        )
        .await;
        assert_eq!(
            events.next().await.unwrap().unwrap(),
            HashMap::from([("one".into(), 1), ("two".into(), 2)])
        );
        // No command is sent while waiting for a change on an idle desktop.
        assert!(
            tokio::time::timeout(Duration::from_millis(20), peer.read_u32())
                .await
                .is_err()
        );
        send_frame(
            &mut peer,
            &Event {
                version: ferese_ipc::events::VERSION,
                generation: 2,
                last: true,
                change: Change::WindowsChanged {
                    windows: Vec::new(),
                },
            },
        )
        .await;
        assert!(events.next().await.unwrap().unwrap().is_empty());
        drop(events);
        assert_eq!(
            peer.read_u8().await.unwrap_err().kind(),
            std::io::ErrorKind::UnexpectedEof
        );
    }

    #[tokio::test]
    async fn background_events_reject_oversized_frames_and_disconnects() {
        let (stream, mut peer) = UnixStream::pair().unwrap();
        let mut events = BackgroundEvents {
            stream,
            windows: Vec::new(),
            changed: false,
        };
        peer.write_u32((ferese_ipc::MAX_PAYLOAD_SIZE + 1) as u32)
            .await
            .unwrap();
        assert!(events.next().await.unwrap_err().contains("Oversized"));
        drop(peer);
        assert!(events.next().await.is_err());
    }

    #[test]
    fn application_state_groups_windows_and_keeps_inactive_workspaces_running() {
        let windows = vec![
            window(1, "org.test.App.desktop", false, 2),
            window(2, "org.test.App", true, 1),
            window(3, "org.test.Other", false, 3),
            window(4, "", false, 1),
        ];
        assert_eq!(
            app_states(&windows),
            HashMap::from([("org.test.App".into(), 2), ("org.test.Other".into(), 1)])
        );
        assert!(app_states(&[]).is_empty());
    }

    #[test]
    fn usb_labels_use_vendor_and_model_from_udev_properties() {
        let properties = HashMap::from([
            (
                "ID_VENDOR_FROM_DATABASE".to_owned(),
                Value::from("Example Vendor").try_to_owned().unwrap(),
            ),
            (
                "ID_MODEL".to_owned(),
                Value::from("Gamepad").try_to_owned().unwrap(),
            ),
        ]);
        let info = HashMap::from([(
            "properties".into(),
            Value::from(properties).try_to_owned().unwrap(),
        )]);
        assert_eq!(usb_label(&info, "opaque-id"), "Example Vendor Gamepad");
        assert_eq!(usb_label(&Options::new(), "fallback-id"), "fallback-id");
    }

    #[test]
    fn autostart_preserves_existing_links_directories_and_invalid_text() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("org.test.App.desktop");
        std::os::unix::fs::symlink("missing", &path).unwrap();
        assert!(set_autostart(dir.path(), "org.test.App", false, &[], 0).is_err());
        assert!(path.symlink_metadata().unwrap().file_type().is_symlink());
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        assert!(set_autostart(dir.path(), "org.test.App", false, &[], 0).is_err());
        std::fs::remove_dir(&path).unwrap();
        std::fs::write(&path, [255u8]).unwrap();
        assert!(set_autostart(dir.path(), "org.test.App", false, &[], 0).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), [255]);
    }

    #[test]
    fn autostart_is_scoped_to_managed_files_and_literal_argv() {
        let dir = tempfile::tempdir().unwrap();
        let argv = vec!["program".into(), "a $x `y` 50% \\\"".into()];
        assert!(set_autostart(dir.path(), "org.test.App", true, &argv, 0).unwrap());
        let path = dir.path().join("org.test.App.desktop");
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("50%%"));
        assert!(text.contains("X-Ferese-Portal=true"));
        assert!(set_autostart(dir.path(), "org.test.App", false, &[], 0).unwrap());
        assert!(!path.exists());
        std::fs::write(&path, "[Desktop Entry]\nName=My custom file\n").unwrap();
        assert!(set_autostart(dir.path(), "org.test.App", true, &argv, 0).is_err());
        for app in ["../escape", ".hidden", "bad/name", "bad\nname"] {
            assert!(set_autostart(dir.path(), app, true, &argv, 0).is_err());
        }
        assert!(desktop_argument("bad\nargument").is_err());
    }
}
