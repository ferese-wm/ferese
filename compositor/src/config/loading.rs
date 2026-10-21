use std::io::{self, Read};
use std::path::Path;

use ferese_config::Document;
use ferese_config::theme::Candidate;

use super::Config;
use crate::RuntimeConfig;

pub(crate) fn read_source(path: &Path) -> Result<String, String> {
    let mut source = String::new();
    std::fs::File::open(path)
        .map_err(|e| format!("{}: {e}", path.display()))?
        .take(60 * 1024 + 1)
        .read_to_string(&mut source)
        .map_err(|e| e.to_string())?;
    if source.len() > 60 * 1024 {
        return Err(format!("{} exceeds 60 KiB", path.display()));
    }
    Ok(source)
}

pub(crate) fn prepare(
    source: &str,
    directory: &Path,
) -> Result<(Config, RuntimeConfig, Candidate), String> {
    let document = Document::parse(source).map_err(|e| e.to_string())?;
    prepare_document(&document, directory)
}

pub(crate) struct StartupConfig {
    pub(crate) source: Option<String>,
    pub(crate) runtime: RuntimeConfig,
    pub(crate) theme: Candidate,
    pub(crate) error: Option<String>,
}

pub(crate) fn startup(path: Option<&Path>) -> Result<StartupConfig, String> {
    let directory = path.and_then(Path::parent).unwrap_or(Path::new("."));
    let source = path.map_or(Ok(None), |path| match std::fs::symlink_metadata(path) {
        Ok(_) => read_source(path).map(Some),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!("{}: {error}", path.display())),
    });
    let prepared = source.and_then(|source| {
        let (_, runtime, theme) = prepare(source.as_deref().unwrap_or(""), directory)?;
        Ok(StartupConfig {
            source,
            runtime,
            theme,
            error: None,
        })
    });
    match prepared {
        Ok(config) => Ok(config),
        Err(error) => {
            let (_, runtime, theme) = prepare("", directory)?;
            Ok(StartupConfig {
                source: Some(String::new()),
                runtime,
                theme,
                error: Some(error),
            })
        }
    }
}

pub(crate) fn prepare_document(
    document: &Document,
    directory: &Path,
) -> Result<(Config, RuntimeConfig, Candidate), String> {
    let candidate = crate::theme::resolve_candidate(document, directory)?;
    let config: Config =
        serde_json::from_value(document.with_theme(&candidate.theme).value().clone())
            .map_err(|e| e.to_string())?;
    let mut runtime = config.runtime_config().map_err(|e| e.to_string())?;
    runtime.theme_settings =
        Config::resolved_theme_settings(&candidate.theme).map_err(|e| e.to_string())?;
    Ok((config, runtime, candidate))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_startup_uses_defaults_and_keeps_the_authored_file() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.kdl");
        let (_, defaults, default_theme) = prepare("", directory.path()).unwrap();
        for source in [
            "panel main { surface solid; group-surface none; }",
            "panel main { geometry { height 1; }; }",
            "animations { speed 0; }",
            "theme { file \"missing-theme.kdl\"; }",
            "panel main {",
        ] {
            std::fs::write(&path, source).unwrap();
            let config = startup(Some(&path)).unwrap();
            assert!(config.error.is_some(), "{source}");
            assert_eq!(config.source.as_deref(), Some(""));
            assert_eq!(config.runtime.panel_style, defaults.panel_style);
            assert_eq!(config.runtime.layout_mode, defaults.layout_mode);
            assert_eq!(config.theme.theme, default_theme.theme);
            assert_eq!(std::fs::read_to_string(&path).unwrap(), source);
            assert!(
                prepare(source, directory.path()).is_err(),
                "strict validation must still reject {source}"
            );
        }
    }

    #[test]
    fn startup_distinguishes_missing_valid_and_unreadable_files() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.kdl");
        let missing = startup(Some(&path)).unwrap();
        assert!(missing.error.is_none());
        assert!(missing.source.is_none());
        assert!(!path.exists());

        let source = "// keep me\npanel main { style floating; geometry { height 42; }; }";
        std::fs::write(&path, source).unwrap();
        let valid = startup(Some(&path)).unwrap();
        assert!(valid.error.is_none());
        assert_eq!(valid.source.as_deref(), Some(source));
        assert_eq!(
            valid.runtime.panel_style,
            ferese_panel::PanelStyle::Floating
        );
        assert_eq!(
            Document::parse(valid.source.as_deref().unwrap())
                .unwrap()
                .get("panels.0.geometry.height"),
            Some(&serde_json::json!(42))
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), source);

        std::fs::write(&path, [0xff]).unwrap();
        assert!(startup(Some(&path)).unwrap().error.is_some());
        assert_eq!(std::fs::read(&path).unwrap(), [0xff]);
        assert!(startup(Some(directory.path())).unwrap().error.is_some());
        std::fs::remove_file(&path).unwrap();
        std::os::unix::fs::symlink(directory.path().join("missing"), &path).unwrap();
        assert!(startup(Some(&path)).unwrap().error.is_some());
        assert!(path.is_symlink());
    }
}
