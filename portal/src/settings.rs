use std::collections::HashMap;
use std::io::Read;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use ferese_config::Document;
use ferese_config::theme::{Appearance as ThemeAppearance, ResolvedTheme};
use tokio::sync::RwLock;
use zbus::Connection;
use zbus::object_server::SignalEmitter;
use zbus::zvariant::{OwnedValue, Value};

const APPEARANCE: &str = "org.freedesktop.appearance";
const PATH: &str = "/org/freedesktop/portal/desktop";
type Values = HashMap<String, HashMap<String, OwnedValue>>;

#[derive(Clone, Copy, Debug, PartialEq)]
struct Appearance {
    scheme: u32,
    accent: (f64, f64, f64),
    reduced_motion: u32,
    contrast: u32,
}

impl Appearance {
    fn from_resolved(theme: &ResolvedTheme) -> Self {
        let accent = ferese_config::theme::rgba(&theme.tokens.colors.accent).expect("validated accent");
        Self {
            scheme: if theme.appearance == ThemeAppearance::Light {
                2
            } else {
                1
            },
            accent: (accent[0], accent[1], accent[2]),
            reduced_motion: u32::from(theme.reduced_motion),
            contrast: u32::from(theme.accessibility.increase_contrast),
        }
    }

    fn values(self) -> HashMap<String, OwnedValue> {
        HashMap::from([
            ("color-scheme".into(), self.scheme.into()),
            ("accent-color".into(), Value::from(self.accent).try_to_owned().unwrap()),
            ("contrast".into(), self.contrast.into()),
            ("reduced-motion".into(), self.reduced_motion.into()),
        ])
    }
}

#[derive(Clone)]
pub(crate) struct Settings(Arc<RwLock<Appearance>>);

impl Settings {
    pub(crate) fn new() -> Self {
        let theme = ferese_ipc::Connection::connect()
            .ok()
            .and_then(|mut connection| connection.get(ferese_config::families::builtins).ok())
            .map(|snapshot| snapshot.theme);
        Self(Arc::new(RwLock::new(initial_appearance(theme.as_ref(), load_config))))
    }

    async fn update_appearance(&self, next: Appearance, emitter: &SignalEmitter<'_>) {
        let mut current = self.0.write().await;
        if *current == next {
            return;
        }
        let previous = current.values();
        *current = next;
        drop(current);
        for (key, value) in next.values() {
            if previous.get(&key) != Some(&value) {
                let _ = Self::setting_changed(emitter, APPEARANCE, &key, value).await;
            }
        }
    }

    pub(crate) async fn watch(self, connection: Connection) {
        let emitter = SignalEmitter::new(&connection, PATH).unwrap();
        loop {
            let connection = tokio::task::spawn_blocking(ferese_ipc::Connection::connect).await;
            if let Ok(Ok(mut connection)) = connection
                && let Ok(cancellation) = connection.cancellation()
            {
                let (send, mut receive) = tokio::sync::mpsc::channel(1);
                tokio::task::spawn_blocking(move || {
                    let Ok(mut snapshot) = connection.get(ferese_config::families::builtins) else {
                        return;
                    };
                    loop {
                        let revision = snapshot.revision;
                        if send.blocking_send(snapshot).is_err() {
                            return;
                        }
                        match connection.watch(revision, ferese_config::families::builtins) {
                            Ok(next) => snapshot = next,
                            Err(_) => return,
                        }
                    }
                });
                while let Some(snapshot) = receive.recv().await {
                    self.update_appearance(Appearance::from_resolved(&snapshot.theme), &emitter)
                        .await;
                }
                drop(cancellation);
            }
            // Standalone startup and compositor disconnects use the same resolver.
            // Rejected edits retain the last valid appearance, just as the owner does.
            if let Ok(Some(next)) = tokio::task::spawn_blocking(load_config).await {
                self.update_appearance(next, &emitter).await;
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    }
}

#[zbus::interface(name = "org.freedesktop.impl.portal.Settings")]
impl Settings {
    #[zbus(property, name = "version")]
    fn version(&self) -> u32 {
        1
    }

    async fn read_all(&self, namespaces: Vec<String>) -> Values {
        if matches_namespace(&namespaces, APPEARANCE) {
            HashMap::from([(APPEARANCE.into(), self.0.read().await.values())])
        } else {
            HashMap::new()
        }
    }

    async fn read(&self, namespace: &str, key: &str) -> zbus::fdo::Result<OwnedValue> {
        if namespace == APPEARANCE
            && let Some(value) = self.0.read().await.values().remove(key)
        {
            return Ok(value);
        }
        Err(zbus::fdo::Error::InvalidArgs("Unknown setting".into()))
    }

    #[zbus(signal)]
    async fn setting_changed(
        emitter: &SignalEmitter<'_>,
        namespace: &str,
        key: &str,
        value: OwnedValue,
    ) -> zbus::Result<()>;
}

fn matches_namespace(filters: &[String], namespace: &str) -> bool {
    filters.is_empty()
        || filters.iter().any(|filter| {
            filter.is_empty()
                || filter == namespace
                || filter.strip_suffix(".*").is_some_and(|prefix| {
                    namespace
                        .strip_prefix(prefix)
                        .is_some_and(|suffix| suffix.starts_with('.'))
                })
        })
}

fn initial_appearance(theme: Option<&ResolvedTheme>, load: impl FnOnce() -> Option<Appearance>) -> Appearance {
    theme
        .map(Appearance::from_resolved)
        .or_else(load)
        .unwrap_or_else(|| Appearance::from_resolved(&ferese_config::theme::default_theme()))
}

fn read_source(path: &Path) -> Result<String, String> {
    let mut source = String::new();
    std::fs::File::open(path)
        .map_err(|error| error.to_string())?
        .take(60 * 1024 + 1)
        .read_to_string(&mut source)
        .map_err(|error| error.to_string())?;
    if source.len() > 60 * 1024 {
        return Err(format!("{} exceeds 60 KiB", path.display()));
    }
    Ok(source)
}

fn load_config() -> Option<Appearance> {
    load_path(&ferese_config::config_path()?).ok()
}

fn load_path(path: &Path) -> Result<Appearance, String> {
    let source = match read_source(path) {
        Ok(source) => source,
        Err(_) if !path.try_exists().map_err(|error| error.to_string())? => {
            return Ok(Appearance::from_resolved(&ferese_config::theme::default_theme()));
        }
        Err(error) => return Err(error),
    };
    let document = Document::parse(&source).map_err(|error| error.to_string())?;
    let resolved = ferese_config::theme::resolve(
        &document,
        path.parent().unwrap_or_else(|| Path::new(".")),
        jiff::Timestamp::now(),
        read_source,
    )?;
    Ok(Appearance::from_resolved(&resolved.theme))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn standalone_appearance_resolves_disk_config_and_imports() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("config.kdl");
        let imported = root.path().join("colors.kdl");
        std::fs::write(&imported, r##"colors { accent "#ff8000"; }"##).unwrap();
        let source = r#"theme { mode "light"; file "colors.kdl"; accessibility { increase-contrast #true; }; }; animations { reduced-motion #true; }"#;
        std::fs::write(&path, source).unwrap();
        let appearance = initial_appearance(None, || load_path(&path).ok());
        let document = Document::parse(source).unwrap();
        let expected =
            ferese_config::theme::resolve(&document, root.path(), jiff::Timestamp::now(), read_source).unwrap();
        assert_eq!(appearance, Appearance::from_resolved(&expected.theme));
        assert_eq!(appearance.scheme, 2);
        assert_eq!(appearance.reduced_motion, 1);
        assert_eq!(appearance.contrast, 1);
        assert_ne!(
            appearance.accent,
            Appearance::from_resolved(&ferese_config::theme::default_theme()).accent
        );

        std::fs::write(&path, r#"theme { mode "dark"; }; animations { enabled #false; }"#).unwrap();
        let next = load_path(&path).unwrap();
        assert_eq!(next.scheme, 1);
        assert_eq!(next.reduced_motion, 1);
        assert_eq!(next.contrast, 0);
    }

    #[test]
    fn ipc_appearance_has_priority_over_disk_fallback() {
        let theme = ResolvedTheme {
            appearance: ThemeAppearance::Light,
            ..ferese_config::theme::default_theme()
        };
        assert_eq!(
            initial_appearance(Some(&theme), || panic!("disk fallback read with live IPC")),
            Appearance::from_resolved(&theme)
        );
    }

    #[test]
    fn disk_fallback_rejects_invalid_config_and_uses_defaults_when_absent() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("config.kdl");
        let default = Appearance::from_resolved(&ferese_config::theme::default_theme());
        assert_eq!(load_path(&path).unwrap(), default);
        for source in [
            r#"theme { broken"#,
            r#"theme { mode "invalid"; }"#,
            r#"theme { colors { accent "invalid"; }; }"#,
            r#"theme { file "missing.kdl"; }"#,
        ] {
            std::fs::write(&path, source).unwrap();
            assert!(load_path(&path).is_err(), "{source}");
            assert_eq!(initial_appearance(None, || load_path(&path).ok()), default);
        }
        std::fs::write(&path, " ".repeat(60 * 1024 + 1)).unwrap();
        assert!(load_path(&path).is_err());
    }

    #[test]
    fn namespaces_support_only_trailing_section_wildcards() {
        for filter in ["", "org.freedesktop.appearance", "org.freedesktop.*", "org.*"] {
            assert!(matches_namespace(&[filter.into()], APPEARANCE));
        }
        for filter in [
            "org.freedesktop*",
            "org.*.appearance",
            "org.freedesktop.appearance.*",
            "other",
        ] {
            assert!(!matches_namespace(&[filter.into()], APPEARANCE));
        }
        assert!(matches_namespace(&[], APPEARANCE));
    }

    #[test]
    fn reports_ferese_theme_and_reduced_motion() {
        let mut resolved = ResolvedTheme {
            appearance: ThemeAppearance::Light,
            ..ferese_config::theme::default_theme()
        };
        resolved.tokens.colors.accent = "#ff8000".into();
        resolved.reduced_motion = true;
        resolved.accessibility.increase_contrast = true;
        let appearance = Appearance::from_resolved(&resolved);
        assert_eq!(appearance.scheme, 2);
        assert_eq!(appearance.reduced_motion, 1);
        assert_eq!(appearance.accent.0, 1.);
        assert_eq!(appearance.accent.2, 0.);
        assert_eq!(
            u32::try_from(appearance.values().remove("contrast").unwrap()).unwrap(),
            1
        );
    }
}
