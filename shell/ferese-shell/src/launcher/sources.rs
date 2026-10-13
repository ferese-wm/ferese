use crate::control::ShellSnapshot;
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Category {
    Apps,
    Files,
    Windows,
}
impl Category {
    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Apps => "Apps",
            Self::Files => "Files",
            Self::Windows => "Windows",
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Action {
    Light,
    Dark,
    Auto,
    Screenshot,
    Lock,
    Suspend,
    Poweroff,
    Reboot,
    Logout,
}
impl Action {
    pub(super) fn destructive(self) -> bool {
        matches!(self, Self::Poweroff | Self::Reboot | Self::Logout)
    }
}
#[derive(Clone, Debug)]
pub(crate) enum Target {
    App { id: String, desktop: PathBuf },
    File(Arc<Path>),
    Window(u64),
    Action(Action),
}
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum ResultId {
    App(Arc<str>),
    File(Arc<Path>),
    Window(u64),
    Action(Action),
}
impl From<&str> for ResultId {
    fn from(value: &str) -> Self {
        Self::App(value.into())
    }
}
#[derive(Clone, Debug)]
pub(crate) struct Row {
    pub id: ResultId,
    pub title: String,
    pub icon: Option<cosmic::widget::icon::Handle>,
    pub subtitle: String,
    pub target: Target,
    pub score: f64,
}
impl Row {
    pub(super) fn category(&self) -> Option<Category> {
        match self.target {
            Target::App { .. } => Some(Category::Apps),
            Target::File(_) => Some(Category::Files),
            Target::Window(_) => Some(Category::Windows),
            Target::Action(_) => None,
        }
    }
    pub(super) fn requires_alt(&self) -> bool {
        matches!(self.target, Target::Action(action) if action.destructive())
    }
}
pub(super) trait Source {
    fn query(&self, query: &str) -> Vec<Row>;
}

// Normalize the query once, and reuse normalized catalog names across keystrokes.
pub(super) struct Matcher(String);
impl Matcher {
    pub(super) fn new(query: &str) -> Self {
        Self(query.trim().to_lowercase())
    }
    pub(super) fn score(&self, text: &str) -> Option<f64> {
        if self.0.is_empty() {
            return Some(0.);
        }
        self.normalized(&text.to_lowercase())
    }
    pub(super) fn normalized(&self, text: &str) -> Option<f64> {
        let query = &self.0;
        if query.is_empty() {
            return Some(0.);
        }
        if text == query {
            return Some(1000.);
        }
        if text.starts_with(query) {
            return Some(800. - text.chars().count() as f64 * 0.1);
        }
        if let Some(offset) = text.find(query) {
            return Some(600. - offset as f64);
        }
        let mut remaining = query.chars();
        let mut next = remaining.next()?;
        for ch in text.chars() {
            if ch == next {
                match remaining.next() {
                    Some(ch) => next = ch,
                    None => return Some(300. - text.chars().count() as f64 * 0.1),
                }
            }
        }
        None
    }
}
#[cfg(test)]
pub(super) fn match_score(query: &str, text: &str) -> Option<f64> {
    Matcher::new(query).score(text)
}
pub(super) fn rank(rows: &mut Vec<Row>) {
    // Group duplicate identities without allocating cloned IDs or a hash table.
    rows.sort_unstable_by(|a, b| {
        a.id.cmp(&b.id)
            .then_with(|| b.score.total_cmp(&a.score))
            .then_with(|| a.title.cmp(&b.title))
    });
    rows.dedup_by(|a, b| a.id == b.id);
    rows.sort_unstable_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            .then_with(|| a.title.cmp(&b.title))
            .then_with(|| a.id.cmp(&b.id))
    });
}
#[derive(Default)]
pub(super) struct Windows {
    entries: std::collections::HashMap<u64, WindowEntry>,
    workspaces: std::collections::HashMap<u64, String>,
    workspace_ids: std::collections::HashSet<u64>,
    query: String,
    apps_ready: bool,
}
struct WindowEntry {
    title: String,
    app_id: String,
    workspace: u64,
    workspace_name: String,
    result: Option<Row>,
    icon: Option<cosmic::widget::icon::Handle>,
    seen: bool,
}
impl Windows {
    pub(super) fn sync(&mut self, snapshot: &ShellSnapshot, apps: Option<&super::apps::Apps>, query: &str) -> bool {
        let query_changed = self.query != query || self.apps_ready != apps.is_some();
        if self.query != query {
            self.query.clear();
            self.query.push_str(query);
        }
        self.apps_ready = apps.is_some();
        let matcher = Matcher::new(query);
        self.workspace_ids.clear();
        for workspace in &snapshot.workspaces {
            self.workspace_ids.insert(workspace.id);
            self.workspaces
                .entry(workspace.id)
                .or_default()
                .clone_from(&workspace.name);
        }
        self.workspaces.retain(|id, _| self.workspace_ids.contains(id));
        for entry in self.entries.values_mut() {
            entry.seen = false;
        }
        let mut changed = query_changed;
        for window in &snapshot.windows {
            let name = self.workspaces.get(&window.workspace).map_or("?", String::as_str);
            if let Some(entry) = self.entries.get_mut(&window.id) {
                entry.seen = true;
                if !query_changed
                    && entry.title == window.title
                    && entry.app_id == window.app_id
                    && entry.workspace == window.workspace
                    && entry.workspace_name == name
                {
                    continue;
                }
            }
            let title = if window.title.is_empty() {
                &window.app_id
            } else {
                &window.title
            };
            let score = matcher
                .score(title)
                .into_iter()
                .chain(matcher.score(&window.app_id))
                .max_by(f64::total_cmp);
            let icon = apps.and_then(|apps| apps.icon(&window.app_id));
            let result = score.map(|score| Row {
                id: ResultId::Window(window.id),
                title: title.clone(),
                icon: icon.clone(),
                subtitle: format!("Window · Workspace {name}"),
                target: Target::Window(window.id),
                score,
            });
            self.entries.insert(
                window.id,
                WindowEntry {
                    title: window.title.clone(),
                    app_id: window.app_id.clone(),
                    workspace: window.workspace,
                    workspace_name: name.into(),
                    result,
                    icon,
                    seen: true,
                },
            );
            changed = true;
        }
        self.entries.retain(|_, entry| {
            if !entry.seen {
                changed = true;
            }
            entry.seen
        });
        changed
    }
    pub(super) fn rows(&self) -> impl Iterator<Item = &Row> {
        self.entries.values().filter_map(|entry| entry.result.as_ref())
    }
}
impl Source for Windows {
    fn query(&self, query: &str) -> Vec<Row> {
        if self.query == query {
            return self.rows().cloned().collect();
        }
        let matcher = Matcher::new(query);
        self.entries
            .iter()
            .filter_map(|(id, entry)| {
                let title = if entry.title.is_empty() {
                    &entry.app_id
                } else {
                    &entry.title
                };
                let score = matcher
                    .score(title)
                    .into_iter()
                    .chain(matcher.score(&entry.app_id))
                    .max_by(f64::total_cmp)?;
                Some(Row {
                    id: ResultId::Window(*id),
                    title: title.clone(),
                    icon: entry.icon.clone(),
                    subtitle: format!("Window · Workspace {}", entry.workspace_name),
                    target: Target::Window(*id),
                    score,
                })
            })
            .collect()
    }
}
pub(super) struct Actions;
impl Source for Actions {
    fn query(&self, query: &str) -> Vec<Row> {
        let matcher = Matcher::new(query);
        [
            ("Light theme", Action::Light),
            ("Dark theme", Action::Dark),
            ("Automatic theme", Action::Auto),
            ("Take screenshot", Action::Screenshot),
            ("Lock screen", Action::Lock),
            ("Suspend", Action::Suspend),
            ("Power off", Action::Poweroff),
            ("Restart", Action::Reboot),
            ("Log out", Action::Logout),
        ]
        .into_iter()
        .filter_map(|(title, action)| {
            let score = matcher.score(title)?;
            Some(Row {
                id: ResultId::Action(action),
                icon: None,
                title: title.into(),
                subtitle: "Action".into(),
                target: Target::Action(action),
                score,
            })
        })
        .collect()
    }
}
