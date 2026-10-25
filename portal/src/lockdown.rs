use std::borrow::Cow;
use std::collections::HashMap;
use std::io::Read;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::RwLock;
use zbus::Connection;
use zbus::message::Header;
use zbus::object_server::SignalEmitter;
use zbus::zvariant::Value;

const PATH: &str = "/org/freedesktop/portal/desktop";
const KEYS: [&str; 7] = [
    "disable-printing",
    "disable-save-to-disk",
    "disable-application-handlers",
    "disable-location",
    "disable-camera",
    "disable-microphone",
    "disable-sound-output",
];

#[derive(Clone)]
pub(crate) struct Lockdown(Arc<RwLock<Policy>>);

struct Policy {
    values: [bool; 7],
    source: Option<String>,
}

impl Lockdown {
    pub(crate) fn new() -> Self {
        Self(Arc::new(RwLock::new(Policy {
            values: load().unwrap_or_default(),
            source: None,
        })))
    }

    pub(crate) async fn watch(self, connection: Connection) {
        let emitter = SignalEmitter::new(&connection, PATH)
            .expect("the portal exports signals at a valid static object path");
        loop {
            tokio::time::sleep(Duration::from_secs(1)).await;
            let mut current = self.0.write().await;
            let Some(path) = ferese_config::config_path() else {
                continue;
            };
            let Some(next) = load_changed(&path, &mut current.source) else {
                continue;
            };
            if current.values == next {
                continue;
            }
            let changed = KEYS
                .iter()
                .enumerate()
                .filter(|(index, _)| current.values[*index] != next[*index])
                .map(|(index, key)| (*key, Value::from(next[index])))
                .collect::<HashMap<_, _>>();
            current.values = next;
            drop(current);
            let _ = zbus::fdo::Properties::properties_changed(
                &emitter,
                "org.freedesktop.impl.portal.Lockdown"
                    .try_into()
                    .expect("the Lockdown interface has a valid static D-Bus name"),
                changed,
                Cow::Borrowed(&[]),
            )
            .await;
        }
    }

    async fn set(
        &self,
        index: usize,
        value: bool,
        connection: &Connection,
        header: Option<Header<'_>>,
    ) -> zbus::fdo::Result<()> {
        let header =
            header.ok_or_else(|| zbus::fdo::Error::AccessDenied("Missing policy caller".into()))?;
        crate::backend::authorize(connection, &header).await?;
        let mut current = self.0.write().await;
        store(index, value).map_err(zbus::fdo::Error::Failed)?;
        current.values[index] = value;
        // Restoring a previously observed file must undo this write too.
        current.source = None;
        Ok(())
    }
}

#[zbus::interface(name = "org.freedesktop.impl.portal.Lockdown")]
impl Lockdown {
    #[zbus(property, name = "disable-printing")]
    async fn disable_printing(&self) -> bool {
        self.0.read().await.values[0]
    }

    #[zbus(property, name = "disable-printing")]
    async fn set_disable_printing(
        &self,
        value: bool,
        #[zbus(connection)] connection: &Connection,
        #[zbus(header)] header: Option<Header<'_>>,
    ) -> zbus::fdo::Result<()> {
        self.set(0, value, connection, header).await
    }

    #[zbus(property, name = "disable-save-to-disk")]
    async fn disable_save_to_disk(&self) -> bool {
        self.0.read().await.values[1]
    }

    #[zbus(property, name = "disable-save-to-disk")]
    async fn set_disable_save_to_disk(
        &self,
        value: bool,
        #[zbus(connection)] connection: &Connection,
        #[zbus(header)] header: Option<Header<'_>>,
    ) -> zbus::fdo::Result<()> {
        self.set(1, value, connection, header).await
    }

    #[zbus(property, name = "disable-application-handlers")]
    async fn disable_application_handlers(&self) -> bool {
        self.0.read().await.values[2]
    }

    #[zbus(property, name = "disable-application-handlers")]
    async fn set_disable_application_handlers(
        &self,
        value: bool,
        #[zbus(connection)] connection: &Connection,
        #[zbus(header)] header: Option<Header<'_>>,
    ) -> zbus::fdo::Result<()> {
        self.set(2, value, connection, header).await
    }

    #[zbus(property, name = "disable-location")]
    async fn disable_location(&self) -> bool {
        self.0.read().await.values[3]
    }

    #[zbus(property, name = "disable-location")]
    async fn set_disable_location(
        &self,
        value: bool,
        #[zbus(connection)] connection: &Connection,
        #[zbus(header)] header: Option<Header<'_>>,
    ) -> zbus::fdo::Result<()> {
        self.set(3, value, connection, header).await
    }

    #[zbus(property, name = "disable-camera")]
    async fn disable_camera(&self) -> bool {
        self.0.read().await.values[4]
    }

    #[zbus(property, name = "disable-camera")]
    async fn set_disable_camera(
        &self,
        value: bool,
        #[zbus(connection)] connection: &Connection,
        #[zbus(header)] header: Option<Header<'_>>,
    ) -> zbus::fdo::Result<()> {
        self.set(4, value, connection, header).await
    }

    #[zbus(property, name = "disable-microphone")]
    async fn disable_microphone(&self) -> bool {
        self.0.read().await.values[5]
    }

    #[zbus(property, name = "disable-microphone")]
    async fn set_disable_microphone(
        &self,
        value: bool,
        #[zbus(connection)] connection: &Connection,
        #[zbus(header)] header: Option<Header<'_>>,
    ) -> zbus::fdo::Result<()> {
        self.set(5, value, connection, header).await
    }

    #[zbus(property, name = "disable-sound-output")]
    async fn disable_sound_output(&self) -> bool {
        self.0.read().await.values[6]
    }

    #[zbus(property, name = "disable-sound-output")]
    async fn set_disable_sound_output(
        &self,
        value: bool,
        #[zbus(connection)] connection: &Connection,
        #[zbus(header)] header: Option<Header<'_>>,
    ) -> zbus::fdo::Result<()> {
        self.set(6, value, connection, header).await
    }
}

fn values(document: &ferese_config::Document) -> Option<[bool; 7]> {
    for path in ["portals", "portals.lockdown"] {
        if document.get(path).is_some_and(|value| !value.is_object()) {
            return None;
        }
    }
    let mut values = [false; 7];
    for (index, key) in KEYS.iter().enumerate() {
        if let Some(value) = document.get(&format!("portals.lockdown.{}", key.replace('-', "_"))) {
            values[index] = value.as_bool()?;
        }
    }
    Some(values)
}

fn load() -> Option<[bool; 7]> {
    load_path(&ferese_config::config_path()?)
}

fn load_path(path: &Path) -> Option<[bool; 7]> {
    values(&ferese_config::Document::parse(&read_source(path)?).ok()?)
}

fn load_changed(path: &Path, previous: &mut Option<String>) -> Option<[bool; 7]> {
    let source = read_source(path)?;
    if previous.as_ref() == Some(&source) {
        return None;
    }
    // Compare contents so atomic saves and edits with unchanged timestamps work.
    // Remember invalid edits too, while retaining the last valid policy.
    *previous = Some(source);
    values(&ferese_config::Document::parse(previous.as_deref()?).ok()?)
}

fn read_source(path: &Path) -> Option<String> {
    let mut source = String::new();
    match std::fs::File::open(path) {
        Ok(file) => {
            file.take((ferese_config::file::LIMIT + 1) as u64)
                .read_to_string(&mut source)
                .ok()?;
            if source.len() > ferese_config::file::LIMIT {
                return None;
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return None,
    }
    Some(source)
}

fn store(index: usize, value: bool) -> Result<(), String> {
    let path = ferese_config::config_path().ok_or("Missing config directory")?;
    store_path(&path, index, value)
}

fn store_path(path: &Path, index: usize, value: bool) -> Result<(), String> {
    let file = ferese_config::file::ConfigFile::open(path)?;
    let mut document =
        ferese_config::Document::parse(file.source()).map_err(|error| error.to_string())?;
    if values(&document).is_none() {
        return Err("Invalid lockdown policy".into());
    }
    document
        .set(
            &format!("portals.lockdown.{}", KEYS[index].replace('-', "_")),
            value.into(),
        )
        .map_err(|error| error.to_string())?;
    file.replace(&document.to_string(), None, |_| Ok(()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn polling_reloads_only_changed_contents_and_recovers_after_invalid_edits() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.kdl");
        let mut source = None;
        assert_eq!(load_changed(&path, &mut source), Some([false; 7]));
        assert_eq!(load_changed(&path, &mut source), None);

        store_path(&path, 4, true).unwrap();
        assert!(load_changed(&path, &mut source).unwrap()[4]);
        assert_eq!(load_changed(&path, &mut source), None);

        for invalid in ["portals {", "portals { lockdown { disable-camera 1; }; }"] {
            std::fs::write(&path, invalid).unwrap();
            assert_eq!(load_changed(&path, &mut source), None);
            assert_eq!(source.as_deref(), Some(invalid));
            assert_eq!(load_changed(&path, &mut source), None);
        }

        // Replacing the file must recover even after a malformed edit.
        let replacement = directory.path().join("replacement.kdl");
        std::fs::write(
            &replacement,
            "portals { lockdown { disable-camera #true; }; }",
        )
        .unwrap();
        std::fs::rename(replacement, &path).unwrap();
        assert!(load_changed(&path, &mut source).unwrap()[4]);
        std::fs::remove_file(&path).unwrap();
        assert_eq!(load_changed(&path, &mut source), Some([false; 7]));
        store_path(&path, 4, true).unwrap();
        assert!(load_changed(&path, &mut source).unwrap()[4]);
    }

    #[test]
    fn polling_detects_same_size_edits_and_retries_failed_reads() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.kdl");
        let mut source = None;
        std::fs::write(&path, "portals { lockdown { disable-camera #true ; }; }").unwrap();
        assert!(load_changed(&path, &mut source).unwrap()[4]);
        let modified = std::fs::metadata(&path).unwrap().modified().unwrap();
        std::fs::write(&path, "portals { lockdown { disable-camera #false; }; }").unwrap();
        std::fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(modified)
            .unwrap();
        assert!(!load_changed(&path, &mut source).unwrap()[4]);

        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        let previous = source.clone();
        assert_eq!(load_changed(&path, &mut source), None);
        assert_eq!(source, previous);
        std::fs::remove_dir(&path).unwrap();
        store_path(&path, 4, true).unwrap();
        assert!(load_changed(&path, &mut source).unwrap()[4]);
    }

    #[test]
    fn policy_save_respects_other_config_writers_and_preserves_their_changes() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.kdl");
        let settings = ferese_config::file::ConfigFile::open(&path).unwrap();
        assert!(
            store_path(&path, 4, true)
                .unwrap_err()
                .contains("another app")
        );
        assert!(!path.exists());
        settings
            .replace("theme \"light\"\n", None, |_| Ok(()))
            .unwrap();

        store_path(&path, 4, true).unwrap();
        let source = std::fs::read_to_string(&path).unwrap();
        let document = ferese_config::Document::parse(&source).unwrap();
        assert_eq!(
            document.get("theme").and_then(|value| value.as_str()),
            Some("light")
        );
        assert!(load_path(&path).unwrap()[4]);
    }

    #[test]
    fn oversized_policy_files_and_saves_leave_config_untouched() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.kdl");
        for size in [ferese_config::file::LIMIT, ferese_config::file::LIMIT + 1] {
            let source = format!("//{}\n", "x".repeat(size - 3));
            std::fs::write(&path, &source).unwrap();
            assert_eq!(
                load_path(&path).is_some(),
                size == ferese_config::file::LIMIT
            );
            assert!(store_path(&path, 4, true).unwrap_err().contains("60 KiB"));
            assert_eq!(std::fs::read_to_string(&path).unwrap(), source);
        }
    }

    #[test]
    fn invalid_policy_is_not_overwritten() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.kdl");
        let source = "portals { lockdown { disable-camera \"yes\"; }; }";
        std::fs::write(&path, source).unwrap();
        assert_eq!(
            store_path(&path, 4, true).unwrap_err(),
            "Invalid lockdown policy"
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), source);
        assert!(ferese_config::file::ConfigFile::open(&path).is_ok());
    }

    #[test]
    fn default_policy_is_unrestricted_and_wrong_types_are_rejected() {
        let empty = ferese_config::Document::parse("").unwrap();
        assert_eq!(values(&empty), Some([false; 7]));
        let enabled = ferese_config::Document::parse(
            "portals { lockdown { disable-camera #true; disable-save-to-disk #true; }; }",
        )
        .unwrap();
        let values = values(&enabled).unwrap();
        assert!(values[1] && values[4]);
        assert!(!values[0]);
        let invalid =
            ferese_config::Document::parse("portals { lockdown { disable-camera \"yes\"; }; }")
                .unwrap();
        assert!(super::values(&invalid).is_none());
        for source in ["portals #false", "portals { lockdown \"bad\"; }"] {
            let invalid = ferese_config::Document::parse(source).unwrap();
            assert!(super::values(&invalid).is_none());
        }
    }
}
