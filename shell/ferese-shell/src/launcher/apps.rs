use super::sources::{Matcher, ResultId, Row, Target};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

#[derive(Clone, Debug)]
pub(crate) struct Apps {
    rows: Vec<Row>,
    normalized_names: Vec<String>,
    by_id: HashMap<String, usize>,
}

#[derive(Default, Deserialize, Serialize)]
struct Usage {
    count: u64,
    last: u64,
}
fn state_path() -> Option<PathBuf> {
    std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/state")))
        .map(|path| path.join("ferese/launcher-apps.json"))
}
fn history() -> HashMap<String, Usage> {
    state_path()
        .and_then(|path| {
            use std::io::Read;
            let mut bytes = Vec::new();
            std::fs::File::open(path)
                .ok()?
                .take(1024 * 1024 + 1)
                .read_to_end(&mut bytes)
                .ok()?;
            (bytes.len() <= 1024 * 1024).then_some(bytes)
        })
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}
fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
impl Apps {
    fn entry(&self, app_id: &str) -> Option<&Row> {
        self.by_id
            .get(&app_id.trim_end_matches(".desktop").to_ascii_lowercase())
            .map(|index| &self.rows[*index])
    }
    pub(super) fn name(&self, app_id: &str) -> Option<&str> {
        self.entry(app_id).map(|row| row.title.as_str())
    }
    pub(super) fn icon(&self, app_id: &str) -> Option<cosmic::widget::icon::Handle> {
        self.entry(app_id)?.icon.clone()
    }
    pub(super) fn empty() -> Self {
        Self {
            rows: Vec::new(),
            normalized_names: Vec::new(),
            by_id: HashMap::new(),
        }
    }
    pub(super) fn load(cancel: &super::work::Cancellation) -> Self {
        use freedesktop_desktop_entry as fde;
        let locales = fde::get_languages_from_env();
        let desktops = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_else(|_| "Ferese".into());
        let desktops: Vec<_> = desktops.split(':').collect();
        let mut seen = HashSet::new();
        let usage = history();
        let now = now();
        let mut rows: Vec<Row> = fde::Iter::new(fde::default_paths())
            .entries(Some(&locales))
            .take_while(|_| !cancel.cancelled())
            .filter(|entry| seen.insert(entry.appid.clone()))
            .filter(|entry| !entry.hidden() && !entry.no_display())
            .filter(|entry| {
                entry
                    .only_show_in()
                    .is_none_or(|names| names.iter().any(|name| desktops.contains(name)))
            })
            .filter(|entry| {
                entry
                    .not_show_in()
                    .is_none_or(|names| !names.iter().any(|name| desktops.contains(name)))
            })
            .filter_map(|entry| {
                let title = entry.name(&locales)?.into_owned();
                let score = usage.get(&entry.appid).map_or(0., |usage| {
                    let age = now.saturating_sub(usage.last) as f64 / 86400.;
                    (usage.count as f64).ln_1p() * 8. + 40. / (1. + age)
                });
                Some(Row {
                    id: ResultId::App(entry.appid.as_str().into()),
                    icon: Some(super::icons::resolve(
                        entry.icon().unwrap_or("application-x-executable"),
                        "application-x-executable",
                    )),
                    title,
                    subtitle: "Application".into(),
                    target: Target::App {
                        id: entry.appid,
                        desktop: entry.path,
                    },
                    score,
                })
            })
            .collect();
        if cancel.cancelled() {
            return Self::empty();
        }
        distinguish_names(&mut rows);
        let normalized_names = rows.iter().map(|row| row.title.to_lowercase()).collect();
        let by_id = rows
            .iter()
            .enumerate()
            .filter_map(|(index, row)| match &row.target {
                Target::App { id, .. } => Some((id.to_ascii_lowercase(), index)),
                _ => None,
            })
            .collect();
        Self {
            rows,
            normalized_names,
            by_id,
        }
    }
}
impl Apps {
    pub(super) fn search(&self, query: &str, cancel: &super::work::Cancellation) -> Vec<Row> {
        let matcher = Matcher::new(query);
        self.rows
            .iter()
            .zip(&self.normalized_names)
            .take_while(|_| !cancel.cancelled())
            .filter_map(|(row, name)| {
                let score = matcher.normalized(name)?;
                let mut row = row.clone();
                row.score += score;
                Some(row)
            })
            .collect()
    }
}
pub(super) fn record(id: &str) -> Result<(), String> {
    let path = state_path().ok_or("No state directory")?;
    let parent = path.parent().ok_or("Invalid state directory")?;
    std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    let mut history = history();
    let usage = history.entry(id.into()).or_default();
    usage.count = usage.count.saturating_add(1);
    usage.last = now();
    let mut file = tempfile::NamedTempFile::new_in(parent).map_err(|e| e.to_string())?;
    serde_json::to_writer(file.as_file_mut(), &history).map_err(|e| e.to_string())?;
    file.persist(path).map_err(|e| e.to_string())?;
    Ok(())
}

// Preserve concise ordinary rows while identifying applications whose names collide.
fn distinguish_names(rows: &mut [Row]) {
    let mut counts = HashMap::new();
    for row in rows.iter() {
        *counts.entry(row.title.to_lowercase()).or_insert(0usize) += 1;
    }
    for row in rows {
        if counts[&row.title.to_lowercase()] > 1
            && let Target::App { id, .. } = &row.target
        {
            row.subtitle = id.clone();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duplicate_names_remain_distinct_and_display_their_application_ids() {
        let mut rows = [
            "org.first.Editor",
            "org.second.Editor",
            "org.unique.Terminal",
        ]
        .map(|id| Row {
            id: ResultId::App(id.into()),
            title: if id.ends_with("Editor") {
                "Editor"
            } else {
                "Terminal"
            }
            .into(),
            icon: None,
            subtitle: "Application".into(),
            target: Target::App {
                id: id.into(),
                desktop: format!("/applications/{id}.desktop").into(),
            },
            score: 0.,
        });
        distinguish_names(&mut rows);
        for row in &rows[..2] {
            let Target::App { id, desktop } = &row.target else {
                panic!("expected application")
            };
            assert_eq!(&row.subtitle, id);
            assert_eq!(row.id, ResultId::App(id.as_str().into()));
            assert_eq!(
                desktop,
                &PathBuf::from(format!("/applications/{id}.desktop"))
            );
        }
        assert_ne!(rows[0].subtitle, rows[1].subtitle);
        assert_eq!(rows[2].subtitle, "Application");
    }
}
