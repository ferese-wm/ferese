use super::sources::{Matcher, ResultId, Row, Source, Target};
use fff_search::{
    FilePicker, FilePickerOptions, FuzzySearchOptions, PaginationArgs, QueryParser,
    SharedFilePicker, SharedFrecency,
};
use std::sync::{Arc, OnceLock};

#[derive(Clone, Debug, Default)]
pub(crate) enum Status {
    #[default]
    Ready,
    Indexing,
    Unavailable(String),
}
impl Status {
    pub(super) fn message(&self) -> Option<&str> {
        match self {
            Self::Ready => None,
            Self::Indexing => Some("Indexing files…"),
            Self::Unavailable(error) => Some(error),
        }
    }
}

#[derive(Clone, Default)]
pub(crate) struct Files(Arc<OnceLock<Result<SharedFilePicker, String>>>);
impl Files {
    fn picker(&self) -> Result<&SharedFilePicker, &str> {
        self.0
            .get_or_init(|| {
                let home =
                    std::env::var_os("HOME").ok_or_else(|| "No home directory".to_string())?;
                let home = std::fs::canonicalize(home).map_err(|e| e.to_string())?;
                if home.parent().is_none() {
                    return Err("The filesystem root cannot be indexed".into());
                }
                let base_path = home
                    .into_os_string()
                    .into_string()
                    .map_err(|_| "Home directory is not UTF-8".to_string())?;
                let shared = SharedFilePicker::default();
                FilePicker::new_with_shared_state(
                    shared.clone(),
                    SharedFrecency::default(),
                    FilePickerOptions {
                        base_path,
                        enable_home_dir_scanning: true,
                        ..Default::default()
                    },
                )
                .map_err(|error| error.to_string())?;
                Ok(shared)
            })
            .as_ref()
            .map_err(String::as_str)
    }
    pub(super) fn status(&self) -> Status {
        match self.0.get() {
            Some(Err(error)) => Status::Unavailable(format!("File search unavailable: {error}")),
            Some(Ok(picker)) if !picker.wait_for_scan(std::time::Duration::ZERO) => {
                Status::Indexing
            }
            _ => Status::Ready,
        }
    }
}
impl Files {
    pub(super) fn search(
        &self,
        query: &str,
        cancel: &super::work::Cancellation,
        icons: &std::sync::Mutex<super::icons::FileIcons>,
    ) -> Vec<Row> {
        if cancel.cancelled() {
            return Vec::new();
        }
        let Ok(shared) = self.picker() else {
            return Vec::new();
        };
        if cancel.cancelled() {
            return Vec::new();
        }
        let paths = {
            let Ok(guard) = shared.read() else {
                return Vec::new();
            };
            if cancel.cancelled() {
                return Vec::new();
            }
            let Some(picker) = guard.as_ref() else {
                return Vec::new();
            };
            let parser = QueryParser::default();
            let parsed = parser.parse(query);
            picker
                .fuzzy_search(
                    &parsed,
                    None,
                    FuzzySearchOptions {
                        max_threads: 2,
                        pagination: PaginationArgs {
                            offset: 0,
                            limit: 100,
                        },
                        ..Default::default()
                    },
                )
                .items
                .into_iter()
                .take_while(|_| !cancel.cancelled())
                .map(|item| item.absolute_path(picker, &picker.base_path))
                .collect::<Vec<_>>()
        };
        // Filesystem/icon work must not hold the picker's read lock.
        let matcher = Matcher::new(query);
        let mut icons = icons.lock().ok();
        paths
            .into_iter()
            .take_while(|_| !cancel.cancelled())
            .map(|path| {
                let title = path
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned();
                let icon = icons.as_mut().map(|icons| icons.file(&path));
                let subtitle = format!("File · {}", path.parent().unwrap_or(&path).display());
                let path: Arc<std::path::Path> = path.into();
                Row {
                    id: ResultId::File(path.clone()),
                    score: matcher.score(&title).unwrap_or(200.),
                    icon,
                    title,
                    subtitle,
                    target: Target::File(path),
                }
            })
            .collect()
    }
}
impl Source for Files {
    fn query(&self, query: &str) -> Vec<Row> {
        self.search(query, &Default::default(), &Default::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn index_is_lazy_and_queries_use_the_file_picker() {
        assert!(Files::default().0.get().is_none());
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("launcher-note.txt"), "note").unwrap();
        let shared = SharedFilePicker::default();
        FilePicker::new_with_shared_state(
            shared.clone(),
            SharedFrecency::default(),
            FilePickerOptions {
                base_path: directory.path().to_string_lossy().into_owned(),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(shared.wait_for_scan(std::time::Duration::from_secs(10)));
        let files = Files(Arc::new(OnceLock::from(Ok(shared))));
        let matches = files.query("launcher-note");
        assert!(matches.iter().any(|row| row.title == "launcher-note.txt"));
        assert!(files.query("no-such-file-xyz").is_empty());

        let icons = std::sync::Mutex::new(super::super::icons::FileIcons::default());
        std::thread::scope(|scope| {
            let failed = scope
                .spawn(|| {
                    let _guard = icons.lock().expect("fresh icon cache");
                    panic!("simulate an icon worker failure");
                })
                .join();
            assert!(failed.is_err());
        });
        let matches = files.search("launcher-note", &Default::default(), &icons);
        assert!(matches.iter().any(|row| row.title == "launcher-note.txt"));
        assert!(matches.iter().all(|row| row.icon.is_none()));
        assert!(
            icons.is_poisoned(),
            "search must not reuse the poisoned cache"
        );
    }
}
