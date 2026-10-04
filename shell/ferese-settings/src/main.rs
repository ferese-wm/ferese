mod connections;
mod form_controls;
mod navigation;
mod pages;
mod theme_controls;

use theme_controls::*;

mod displays;
mod displays_ui;
mod fonts;
mod schema;
mod store;
mod visuals;
mod wallpaper_controls;
mod watch;

use std::collections::HashMap;
use std::path::PathBuf;

use cosmic::app::{Core, Settings, Task};
use cosmic::iced::{Alignment, Length};
use cosmic::widget::{button, column, container, row, scrollable, slider, text_input};
use cosmic::{ApplicationExt, Element, widget};
use ferese_theme::gallery;
use schema::{Field, Kind, Page};
use store::{Edit, Snapshot, set};

fn main() -> cosmic::iced::Result {
    let mut args = std::env::args_os().skip(1);
    let mut path = store::config_path();
    let mut initial_page = None;
    while let Some(arg) = args.next() {
        match arg.to_str() {
            Some("--config") => {
                let Some(value) = args.next() else {
                    eprintln!("--config requires a path");
                    return Ok(());
                };
                path = PathBuf::from(value);
            }
            Some("--page") => {
                initial_page = match args.next().as_deref().and_then(|s| s.to_str()) {
                    Some("connections" | "wifi") => Some(InitialPage::Connections(connections::Tab::Wifi)),
                    Some("bluetooth") => Some(InitialPage::Connections(connections::Tab::Bluetooth)),
                    Some("wallpaper") => Some(InitialPage::Wallpaper),
                    _ => {
                        eprintln!("--page accepts connections, wifi, bluetooth, or wallpaper");
                        return Ok(());
                    }
                };
            }
            _ => {
                eprintln!("Usage: ferese-settings [--config PATH] [--page connections|wifi|bluetooth|wallpaper]");
                return Ok(());
            }
        }
    }
    let path = path.canonicalize().unwrap_or(path);
    fonts::families();
    let initial = Snapshot::read(&path);
    let theme = visuals::native_theme(initial.as_ref().ok());

    cosmic::app::run::<App>(
        Settings::default()
            .theme(theme)
            .is_daemon(false)
            .antialiasing(true)
            .default_text_size(14.),
        (path, initial, initial_page),
    )
}

#[derive(Clone, Copy)]
enum InitialPage {
    Connections(connections::Tab),
    Wallpaper,
}

#[derive(Clone, Debug)]
enum Message {
    Connections(connections::Input),
    ConnectionsReady(Result<std::sync::Arc<connections::Client>, String>),
    ConnectionsRefreshed(Box<connections::Snapshot>),
    ConnectionFinished(Result<(), String>),
    ConnectionsLeft,
    SelectDisplay(String),
    DisplayResolution(String, displays::Resolution),
    RefreshDisplays,
    DisplaysLoaded(Result<Vec<displays::Display>, String>),
    RefreshRate(String, displays::Mode, bool),
    ThemeChanged(Box<ferese_ipc::theme::Snapshot>),
    DragWindow,
    Page(Page),
    PagePresented(Page),
    RowVisibility(&'static str, usize, bool),
    Search(String),
    Change(Edit),
    SelectFont(String, String),
    Draft(String, String),
    Commit(Field),
    Range(Field, f64),
    Release(Field),
    Saved(Result<(Snapshot, bool), String>),
    Reload,
    ExternalConfig(Result<Snapshot, String>),
    Undo,
    Family(String, Option<ferese_config::theme::Appearance>),
    SplitThemes(bool),
    AutoAppearance(bool),
    AutoDetails(bool),
    AdvancedTheme(bool),
    ThemeFileTarget(usize),
    PickThemeFile(&'static str),
    ThemeFilePicked(&'static str, Result<Option<String>, String>),
    ImportTheme,
    ThemeImported(Result<Option<(String, String)>, String>),
    ExpireUndo(u64),
    PickWallpaper,
    WallpaperTarget(wallpaper_controls::Target),
    UseDefaultWallpaper(ferese_config::theme::Appearance),
    RestoreDefaultWallpapers,
    DefaultWallpaperThumbnail(ferese_config::theme::Appearance, Result<widget::image::Handle, String>),
    PreviewLock,
    LockPreviewStarted(Result<(), String>),
    AddNote,
    NoteAction(String, widget::text_editor::Action),
    SaveNote(String, u64),
    WallpaperPicked(wallpaper_controls::Target, Result<Option<String>, String>),
    NewCommand(String),
    AddCommand,
    AddSwipe(&'static str),
    Remove(String, usize),
    Thumbnail(String, Result<widget::image::Handle, String>),
}

struct App {
    connections: connections::State,
    displays: Vec<displays::Display>,
    display_selection: Option<String>,
    display_error: Option<String>,
    note_editors: HashMap<String, NoteEditor>,
    core: Core,
    path: PathBuf,
    current: Snapshot,
    draft: Snapshot,
    pending: Vec<Edit>,
    saving: bool,
    undo: Option<String>,
    saving_previous: String,
    error: Option<String>,
    status: String,
    page: Page,
    profile_pages: bool,
    page_transition: Option<std::time::Instant>,
    visible_rows: std::collections::HashSet<(&'static str, usize)>,
    hidden_binding_rows: std::collections::HashSet<usize>,
    search: String,
    inputs: HashMap<String, String>,
    ranges: HashMap<String, f64>,
    new_command: String,
    font: cosmic::font::Font,
    native_palette: visuals::Palette,
    family_ids: std::sync::Arc<Vec<String>>,
    gallery_focus: Option<(String, Option<ferese_config::theme::Appearance>)>,
    resolved: ferese_ipc::theme::Snapshot,
    undo_revision: u64,
    auto_details: bool,
    advanced_theme: bool,
    theme_file_target: usize,
    wallpaper: wallpaper_controls::State,
}

fn initial_visible_rows() -> std::collections::HashSet<(&'static str, usize)> {
    // Populate the first viewport before sensors report precise visibility.
    // This avoids a blank first frame when a page opens.
    (0..4)
        .map(|index| ("bindings", index))
        .chain(
            ["theme", "theme-light", "theme-dark"]
                .into_iter()
                .flat_map(|list| (0..3).map(move |index| (list, index))),
        )
        .collect()
}

impl Drop for App {
    fn drop(&mut self) {
        if let Some(client) = &self.connections.client {
            client.shutdown();
        }
    }
}

struct NoteEditor {
    content: widget::text_editor::Content<cosmic::Renderer>,
    revision: u64,
    dirty: bool,
}

impl cosmic::Application for App {
    type Executor = cosmic::executor::Default;
    type Flags = (PathBuf, Result<Snapshot, String>, Option<InitialPage>);
    type Message = Message;
    const APP_ID: &'static str = "dev.ferese.Settings";

    fn core(&self) -> &Core {
        &self.core
    }

    fn core_mut(&mut self) -> &mut Core {
        &mut self.core
    }

    fn init(mut core: Core, (path, initial, initial_page): Self::Flags) -> (Self, Task<Message>) {
        core.window.show_headerbar = false;

        let status = if path == store::config_path() {
            "Changes save automatically"
        } else {
            "Preview config · changes do not update your desktop"
        }
        .into();
        let error = initial.as_ref().err().cloned();
        let current = initial.unwrap_or_else(|_| Snapshot::parse(String::new()).unwrap());
        let resolved = ferese_theme_client::service::current();
        let font = ferese_theme::font(Some(&resolved.presented.tokens.typography.font_family));
        let native_palette = visuals::Palette::from_resolved(&resolved.presented);
        let mut app = Self {
            connections: connections::State {
                tab: match initial_page {
                    Some(InitialPage::Connections(tab)) => tab,
                    _ => connections::Tab::default(),
                },
                ..connections::State::default()
            },
            displays: vec![],
            display_selection: None,
            display_error: None,
            note_editors: HashMap::new(),
            core,
            path,
            draft: current.clone(),
            current,
            pending: vec![],
            saving: false,
            undo: None,
            saving_previous: String::new(),
            error,
            status,
            page: match initial_page {
                Some(InitialPage::Connections(_)) => Page::Connections,
                Some(InitialPage::Wallpaper) => Page::Wallpaper,
                None => Page::Appearance,
            },
            profile_pages: std::env::var_os("FERESE_PROFILE_SETTINGS").is_some(),
            page_transition: None,
            search: String::new(),
            visible_rows: initial_visible_rows(),
            hidden_binding_rows: Default::default(),
            inputs: HashMap::new(),
            ranges: HashMap::new(),
            new_command: String::new(),
            font,
            native_palette,
            gallery_focus: None,
            family_ids: std::sync::Arc::new(resolved.families.iter().map(|family| family.id.clone()).collect()),
            resolved,
            undo_revision: 0,
            auto_details: false,
            advanced_theme: false,
            theme_file_target: 0,
            wallpaper: Default::default(),
        };
        let task = app
            .core
            .main_window_id()
            .map(|id| app.set_window_title("Ferese Settings".into(), id))
            .unwrap_or_else(Task::none);

        app.sync_notes();
        let connections = if app.page == Page::Connections {
            app.refresh_connections()
        } else {
            Task::none()
        };
        let thumbnails = app.load_thumbnail();
        (app, Task::batch([task, connections, thumbnails]))
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Connections(input) => return self.connections_input(input),
            Message::ConnectionsReady(result) => return self.connections_ready(result),
            Message::ConnectionsRefreshed(snapshot) => self.connections_refreshed(*snapshot),
            Message::ConnectionFinished(result) => return self.connection_finished(result),
            Message::ConnectionsLeft => {}
            Message::SelectDisplay(key) => self.display_selection = Some(key),
            Message::DisplayResolution(prefix, size) => {
                let matcher = self.draft.string(&format!("{prefix}.match"), "");
                let configured = self.draft.string(&format!("{prefix}.mode"), "");
                let automatic = self.draft.boolean(&format!("{prefix}.auto_refresh"), false);
                if let Some(display) = self
                    .displays
                    .iter()
                    .find(|d| d.connector == matcher || d.identity == matcher)
                    && let Some((mode, automatic)) = displays::change_resolution(display, &configured, size, automatic)
                {
                    self.inputs.remove(&format!("{prefix}.mode"));
                    return self.edit_many(displays::edits(&prefix, mode, automatic));
                }
            }
            Message::RefreshDisplays => return self.load_displays(),
            Message::DisplaysLoaded(result) => match result {
                Ok(displays) => {
                    self.displays = displays;
                    self.display_error = None;
                }
                Err(error) => self.display_error = Some(error),
            },
            Message::RefreshRate(prefix, mode, automatic) => {
                self.inputs.remove(&format!("{prefix}.mode"));
                return self.edit_many(displays::edits(&prefix, mode, automatic));
            }
            Message::ThemeChanged(snapshot) => {
                let wallpaper_changed =
                    self.resolved.presented.tokens.background.path != snapshot.presented.tokens.background.path;
                let font_changed = self.resolved.presented.tokens.typography.font_family
                    != snapshot.presented.tokens.typography.font_family;
                if self.resolved.families != snapshot.families {
                    self.family_ids =
                        std::sync::Arc::new(snapshot.families.iter().map(|family| family.id.clone()).collect());
                }
                self.resolved = *snapshot;
                if font_changed {
                    self.font = ferese_theme::font(Some(&self.resolved.presented.tokens.typography.font_family));
                }

                let theme = self.update_theme();
                return if wallpaper_changed {
                    Task::batch([theme, self.load_thumbnail()])
                } else {
                    theme
                };
            }
            Message::NoteAction(id, action) => {
                if let Some(editor) = self.note_editors.get_mut(&id) {
                    let edited = action.is_edit();
                    editor.content.perform(action);

                    if edited {
                        editor.dirty = true;
                        editor.revision = editor.revision.wrapping_add(1);
                        let revision = editor.revision;
                        return cosmic::task::future(async move {
                            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                            Message::SaveNote(id, revision)
                        });
                    }
                }
            }
            Message::SaveNote(id, revision) => {
                let Some(index) = self.note_index(&id) else {
                    return Task::none();
                };

                if let Some(editor) = self.note_editors.get_mut(&id)
                    && editor.dirty
                    && editor.revision == revision
                {
                    let value = editor.content.text();
                    editor.dirty = false;
                    return self.change(set(&format!("desktop_widgets.notes.{index}.text"), value));
                }
            }
            Message::AddNote if !self.saving && self.draft.records("desktop_widgets.notes") < 32 => {
                let id = format!(
                    "note-{}",
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_nanos()
                );
                let task = self.change(Edit::Add(
                    "desktop_widgets.notes".into(),
                    vec![
                        ("id".into(), id.into()),
                        ("title".into(), "Note".into()),
                        ("text".into(), "".into()),
                    ],
                ));

                self.sync_notes();
                return task;
            }
            Message::SelectFont(path, family) => {
                self.inputs.remove(&path);
                return self.change(set(&path, family));
            }
            Message::DragWindow => return self.core.drag(None),
            Message::ExternalConfig(result) => match result {
                Ok(snapshot) if snapshot.source == self.current.source || snapshot.source == self.draft.source => {}
                Ok(snapshot) => {
                    if self.saving
                        || self.note_editors.values().any(|editor| editor.dirty)
                        || !self.inputs.is_empty()
                        || !self.ranges.is_empty()
                        || !self.pending.is_empty()
                    {
                        self.status = "Config changed externally · Reload when your edits are finished".into();
                    } else {
                        self.current = snapshot.clone();
                        self.draft = snapshot;
                        self.sync_notes();
                        self.font = ferese_theme::font(Some(&self.resolved.presented.tokens.typography.font_family));
                        self.undo = None;
                        self.error = None;
                        self.status = "Updated from your config".into();

                        return Task::batch([self.update_theme(), self.load_thumbnail()]);
                    }
                }
                Err(error) => self.error = Some(format!("Config reload failed: {error}")),
            },
            Message::PreviewLock => {
                let path = self.path.clone();
                return Task::perform(
                    async move {
                        let binary = std::env::current_exe()
                            .ok()
                            .and_then(|p| p.parent().map(|p| p.join("ferese-lock")))
                            .filter(|p| p.is_file())
                            .unwrap_or_else(|| "ferese-lock".into());
                        std::process::Command::new(binary)
                            .arg("--preview")
                            .arg("--config")
                            .arg(path)
                            .spawn()
                            .map(|mut child| {
                                std::thread::spawn(move || {
                                    let _ = child.wait();
                                });
                            })
                            .map_err(|error| format!("Could not open the lock preview: {error}"))
                    },
                    |result| cosmic::Action::App(Message::LockPreviewStarted(result)),
                );
            }
            Message::LockPreviewStarted(Err(error)) => {
                self.error = Some(error);
            }
            Message::Page(page) => {
                if self.page == page && self.search.is_empty() {
                    return Task::none();
                }

                self.page_transition = self.profile_pages.then(std::time::Instant::now);
                let leaving = self.page == Page::Connections && page != Page::Connections;
                self.page = page;
                self.search.clear();
                self.visible_rows = initial_visible_rows();
                self.hidden_binding_rows.clear();
                self.gallery_focus = None;
                let reset = cosmic::iced::widget::scrollable::snap_to(
                    widget::Id::new("settings-content"),
                    cosmic::iced::widget::scrollable::RelativeOffset::START.into(),
                );
                if leaving {
                    let cleanup = self.leave_connections();
                    let load = match page {
                        Page::Displays => self.load_displays(),
                        Page::Wallpaper => self.load_thumbnail(),
                        _ => Task::none(),
                    };
                    return Task::batch([reset, cleanup, load]);
                }
                if page == Page::Connections {
                    return Task::batch([reset, self.refresh_connections()]);
                }
                if page == Page::Displays {
                    return Task::batch([reset, self.load_displays()]);
                }
                if page == Page::Wallpaper {
                    return Task::batch([reset, self.load_thumbnail()]);
                }

                return reset;
            }
            Message::PagePresented(page) => {
                if self.page == page
                    && let Some(started) = self.page_transition.take()
                {
                    eprintln!(
                        "settings page={page:?} first_redraw_us={}",
                        started.elapsed().as_micros()
                    );
                }
            }
            Message::RowVisibility(list, index, visible) => {
                if visible {
                    self.visible_rows.insert((list, index));
                    if list == "bindings" {
                        self.hidden_binding_rows.remove(&index);
                    }
                    if let Some((id, appearance)) = &self.gallery_focus
                        && navigation::gallery_list(*appearance) == list
                        && self
                            .family_ids
                            .iter()
                            .position(|family| family == id)
                            .is_some_and(|position| position / 3 == index)
                    {
                        let (id, appearance) = self.gallery_focus.take().unwrap();
                        return cosmic::iced::advanced::widget::operate(
                            cosmic::iced::advanced::widget::operation::focusable::focus(gallery_id(&id, appearance)),
                        )
                        .map(cosmic::Action::App);
                    }
                } else {
                    let dirty = list == "bindings"
                        && self
                            .inputs
                            .keys()
                            .any(|path| path.starts_with(&format!("bindings.{index}.")));
                    if dirty {
                        // Retain the input widget until its pending edit commits.
                        self.hidden_binding_rows.insert(index);
                    } else {
                        self.visible_rows.remove(&(list, index));
                    }
                }
            }
            Message::Search(query) => self.search = query,
            Message::Draft(path, value) => {
                self.inputs.insert(path, value);
            }
            Message::Range(field, value) => {
                self.ranges.insert(field.path, value);
            }
            Message::Release(field) => {
                if let Some(value) = self.ranges.remove(&field.path) {
                    let integer = matches!(field.kind, Kind::Range { integer: true, .. });
                    return self.change(if integer {
                        set(&field.path, value.round() as i64)
                    } else {
                        set(&field.path, value)
                    });
                }
            }
            Message::Commit(field) => {
                if let Some(value) = self.inputs.remove(&field.path) {
                    if field.path.starts_with("bindings.")
                        && let Some(index) = field
                            .path
                            .split('.')
                            .nth(1)
                            .and_then(|index| index.parse::<usize>().ok())
                        && !self
                            .inputs
                            .keys()
                            .any(|path| path.starts_with(&format!("bindings.{index}.")))
                        && self.hidden_binding_rows.remove(&index)
                    {
                        self.visible_rows.remove(&("bindings", index));
                    }
                    let edit = if matches!(field.kind, Kind::Text { argv: true, .. }) {
                        match shlex::split(&value).filter(|v| !v.is_empty()) {
                            Some(args) => set(
                                &field.path,
                                args.into_iter().map(serde_json::Value::from).collect::<Vec<_>>(),
                            ),
                            None => {
                                self.error = Some("Enter a program and arguments with balanced quotes. Commands are not run through a shell.".into());
                                return Task::none();
                            }
                        }
                    } else {
                        set(&field.path, value)
                    };
                    return self.change(edit);
                }
            }
            Message::Change(edit) => return self.change(edit),
            Message::Saved(result) => {
                self.saving = false;
                match result {
                    Ok((snapshot, live)) => {
                        self.undo = Some(std::mem::take(&mut self.saving_previous));
                        self.current = snapshot;
                        self.font = ferese_theme::font(Some(&self.resolved.presented.tokens.typography.font_family));
                        self.draft = self.current.clone();

                        for edit in &self.pending {
                            if let Err(error) = self.draft.edit(edit) {
                                self.error = Some(error);
                            }
                        }

                        self.sync_notes();
                        self.status = if live {
                            "Saved · desktop updated"
                        } else if self.path != store::config_path() {
                            "Saved to preview config · desktop unchanged"
                        } else {
                            "Saved · could not confirm desktop reload"
                        }
                        .into();

                        self.undo_revision = self.undo_revision.wrapping_add(1);
                        let revision = self.undo_revision;
                        let expire = cosmic::task::future(async move {
                            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                            Message::ExpireUndo(revision)
                        });
                        return Task::batch([self.update_theme(), self.flush(), self.load_thumbnail(), expire]);
                    }
                    Err(error) => {
                        self.error = Some(error);
                        self.pending.clear();
                        self.draft = self.current.clone();
                        self.status = "Not saved".into();
                    }
                }
            }
            Message::Reload if !self.saving => match Snapshot::read(&self.path) {
                Ok(snapshot) => {
                    self.current = snapshot.clone();
                    self.draft = snapshot;
                    self.note_editors.clear();
                    self.sync_notes();
                    self.font = ferese_theme::font(Some(&self.resolved.presented.tokens.typography.font_family));
                    self.pending.clear();
                    self.inputs.clear();
                    self.ranges.clear();
                    self.error = None;
                    self.undo = None;
                    self.status = "Reloaded from your config".into();

                    return Task::batch([self.update_theme(), self.load_thumbnail()]);
                }
                Err(error) => self.error = Some(error),
            },
            Message::Undo if !self.saving => {
                if let Some(source) = self.undo.take() {
                    return self.start_save(source);
                }
            }
            Message::Family(id, appearance) => {
                let reveal = if let Some(index) = self.family_ids.iter().position(|family| family == &id) {
                    let list = navigation::gallery_list(appearance);
                    if !self.visible_rows.insert((list, index / 3)) {
                        self.gallery_focus = None;
                    } else {
                        // Focus after the sensor observes the newly mounted row.
                        self.gallery_focus = Some((id.clone(), appearance));
                    }
                    cosmic::iced::advanced::widget::operate(navigation::RevealRow::new(list, index / 3))
                        .map(cosmic::Action::App)
                } else {
                    Task::none()
                };
                let edits = visuals::choose_family(&id, appearance);
                let focus = if self.gallery_focus.is_none() {
                    cosmic::iced::advanced::widget::operate(
                        cosmic::iced::advanced::widget::operation::focusable::focus(gallery_id(&id, appearance)),
                    )
                    .map(cosmic::Action::App)
                } else {
                    Task::none()
                };
                return Task::batch([self.edit_many(edits), reveal, focus]);
            }
            Message::AutoDetails(enabled) => self.auto_details = enabled,
            Message::AdvancedTheme(enabled) => self.advanced_theme = enabled,
            Message::AutoAppearance(enabled) => {
                let mode = if enabled {
                    "auto"
                } else if self.resolved.theme.appearance == ferese_config::theme::Appearance::Light {
                    "light"
                } else {
                    "dark"
                };
                return self.change(set("theme.mode", mode));
            }
            Message::ThemeFileTarget(index) => self.theme_file_target = index.min(2),
            Message::PickThemeFile(path) => {
                return cosmic::task::future(async move {
                    let result = tokio::task::spawn_blocking(|| {
                        store::pick_file("Choose theme overrides", "KDL themes | *.kdl")
                    })
                    .await
                    .unwrap_or_else(|e| Err(e.to_string()));
                    Message::ThemeFilePicked(path, result)
                });
            }
            Message::ThemeFilePicked(path, Ok(Some(file))) => return self.change(set(path, file)),
            Message::ThemeFilePicked(_, Err(error)) => self.error = Some(error),
            Message::SplitThemes(enabled) => {
                return self.edit_many(visuals::toggle_split(
                    &self.draft,
                    enabled,
                    self.resolved.theme.appearance,
                ));
            }
            Message::ImportTheme => {
                let source = self.draft.source.clone();
                let directory = self.path.parent().map(ToOwned::to_owned);
                return cosmic::task::future(async move {
                    let result = tokio::task::spawn_blocking(move || -> Result<_, String> {
                        let Some(path) = store::pick_file("Import theme", "KDL themes | *.kdl")? else {
                            return Ok(None);
                        };
                        let mut snapshot = Snapshot::parse(source)?;
                        let stem = PathBuf::from(&path)
                            .file_stem()
                            .unwrap_or_default()
                            .to_string_lossy()
                            .to_string();
                        let stem: String = stem
                            .to_lowercase()
                            .chars()
                            .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
                            .collect();
                        let mut id = format!("custom-{stem}");
                        let mut suffix = 2;
                        while snapshot.item(&format!("theme.custom_themes.{id}")).is_some() {
                            id = format!("custom-{stem}-{suffix}");
                            suffix += 1;
                        }
                        snapshot.edit(&set(&format!("theme.custom_themes.{id}.file"), path.as_str()))?;
                        let mut connection = ferese_ipc::theme::Connection::connect().map_err(|e| e.to_string())?;
                        let value = connection.call(
                            "theme-preview",
                            serde_json::json!({"source": snapshot.source, "directory": directory}),
                        )?;
                        let _: Vec<ferese_config::families::Family> =
                            serde_json::from_value(value["families"].clone()).map_err(|e| e.to_string())?;
                        Ok(Some((id, path)))
                    })
                    .await
                    .unwrap_or_else(|e| Err(e.to_string()));
                    Message::ThemeImported(result)
                });
            }
            Message::ThemeImported(Ok(Some((id, path)))) => {
                return self.change(set(&format!("theme.custom_themes.{id}.file"), path));
            }
            Message::ThemeImported(Err(error)) => self.error = Some(error),
            Message::ExpireUndo(revision) if revision == self.undo_revision => self.undo = None,
            Message::PickWallpaper => {
                let target = self.wallpaper.target;
                return cosmic::task::future(async move {
                    let result = tokio::task::spawn_blocking(|| {
                        store::pick_file(
                            "Choose a wallpaper",
                            "Images | *.png *.jpg *.jpeg *.webp *.PNG *.JPG *.JPEG *.WEBP",
                        )
                    })
                    .await
                    .unwrap_or_else(|e| Err(e.to_string()));
                    Message::WallpaperPicked(target, result)
                });
            }
            Message::WallpaperPicked(target, Ok(Some(path))) => {
                return self.apply_wallpaper_edits(wallpaper_controls::edits(target, "path", path));
            }
            Message::WallpaperPicked(_, Err(error)) => self.error = Some(error),
            Message::Thumbnail(path, result) => {
                self.wallpaper_thumbnail(path, result);
                return self.load_thumbnail();
            }
            Message::DefaultWallpaperThumbnail(appearance, result) => {
                self.default_wallpaper_thumbnail(appearance, result);
            }
            Message::WallpaperTarget(target) => self.wallpaper.target = target,
            Message::UseDefaultWallpaper(appearance) => {
                return self.apply_wallpaper_edits(wallpaper_controls::edits(
                    self.wallpaper.target,
                    "path",
                    ferese_config::default_wallpaper_for(appearance),
                ));
            }
            Message::RestoreDefaultWallpapers => {
                return self.apply_wallpaper_edits(wallpaper_controls::restore_defaults());
            }
            Message::NewCommand(value) => self.new_command = value,
            Message::AddCommand if !self.saving => match shlex::split(&self.new_command).filter(|a| !a.is_empty()) {
                Some(args) => {
                    self.new_command.clear();
                    return self.change(Edit::Add(
                        "autostart".into(),
                        vec![
                            (
                                "command".into(),
                                args.into_iter().map(serde_json::Value::from).collect::<Vec<_>>().into(),
                            ),
                            ("enabled".into(), true.into()),
                            ("restart".into(), true.into()),
                        ],
                    ));
                }
                None => self.error = Some("Enter a program and arguments with balanced quotes.".into()),
            },
            Message::AddSwipe(keys) if !self.saving => {
                let (action, argument) = match keys {
                    "Swipe3Up" => ("workspace-next", None),
                    "Swipe3Down" => ("workspace-previous", None),
                    "Swipe3Left" => ("focus", Some("right")),
                    _ => ("focus", Some("left")),
                };
                let mut fields = vec![("keys".into(), keys.into()), ("action".into(), action.into())];

                if let Some(argument) = argument {
                    fields.push(("argument".into(), argument.into()));
                }

                return self.change(Edit::Add("bindings".into(), fields));
            }
            Message::Remove(table, index) if !self.saving => {
                self.inputs.clear();
                let task = self.change(Edit::Remove(table, index));
                self.sync_notes();
                return task;
            }
            _ => {}
        }
        Task::none()
    }

    fn subscription(&self) -> cosmic::iced::Subscription<Message> {
        cosmic::iced::Subscription::batch([
            cosmic::iced::Subscription::run_with(self.path.clone(), |path| watch::changes(path)),
            ferese_theme_client::service::subscription().map(|snapshot| Message::ThemeChanged(Box::new(snapshot))),
            if self.page == Page::Displays {
                cosmic::iced::time::every(std::time::Duration::from_secs(2)).map(|_| Message::RefreshDisplays)
            } else {
                cosmic::iced::Subscription::none()
            },
            if self.page == Page::Connections {
                cosmic::iced::time::every(std::time::Duration::from_secs(3))
                    .map(|_| Message::Connections(connections::Input::Refresh))
            } else {
                cosmic::iced::Subscription::none()
            },
            if self.connections.busy == Some("Pairing…") {
                cosmic::iced::time::every(std::time::Duration::from_millis(250))
                    .map(|_| Message::Connections(connections::Input::PollPrompt))
            } else {
                cosmic::iced::Subscription::none()
            },
        ])
    }

    fn view(&self) -> Element<'_, Message> {
        self.page_view()
    }
}

impl App {
    fn update_theme(&mut self) -> Task<Message> {
        // Widgets use presented colors directly; COSMIC's structural theme only
        // changes when the resolved destination changes, not on every fade tick.
        let palette = visuals::Palette::from_resolved(&self.resolved.theme);
        if palette == self.native_palette {
            return Task::none();
        }
        self.native_palette = palette;
        cosmic::command::set_theme(palette.native_theme())
    }

    fn note_index(&self, id: &str) -> Option<usize> {
        (0..self.draft.records("desktop_widgets.notes"))
            .find(|index| self.draft.string(&format!("desktop_widgets.notes.{index}.id"), "note") == id)
    }

    fn sync_notes(&mut self) {
        let mut ids = Vec::new();

        for index in 0..self.draft.records("desktop_widgets.notes") {
            let prefix = format!("desktop_widgets.notes.{index}");
            let id = self.draft.string(&format!("{prefix}.id"), "note");
            let text = self.draft.string(&format!("{prefix}.text"), "");
            let editor = self.note_editors.entry(id.clone()).or_insert_with(|| NoteEditor {
                content: widget::text_editor::Content::with_text(&text),
                revision: 0,
                dirty: false,
            });
            if !editor.dirty && editor.content.text() != text {
                editor.content = widget::text_editor::Content::with_text(&text);
            }
            ids.push(id);
        }

        self.note_editors.retain(|id, _| ids.contains(id));
    }

    fn edit_many(&mut self, edits: Vec<Edit>) -> Task<Message> {
        for edit in edits {
            if let Err(error) = self.draft.edit(&edit) {
                self.error = Some(error);
                return Task::none();
            }
            self.pending.push(edit);
        }
        self.flush()
    }

    fn load_displays(&self) -> Task<Message> {
        cosmic::task::future(async {
            Message::DisplaysLoaded(
                tokio::task::spawn_blocking(displays::load)
                    .await
                    .unwrap_or_else(|error| Err(error.to_string())),
            )
        })
    }

    fn change(&mut self, edit: Edit) -> Task<Message> {
        if let Edit::Set(path, value) = &edit
            && matches!(path.as_str(), "theme.background.path" | "theme.background.mode")
        {
            return self.apply_wallpaper_edits(wallpaper_controls::edits(
                wallpaper_controls::Target::Both,
                path.rsplit('.').next().unwrap(),
                value.clone(),
            ));
        }

        match self.draft.edit(&edit) {
            Ok(()) => {
                // When a gradient exists, changing Accent also changes its
                // leading stop; otherwise the visible focus border would stay
                // on the old palette despite the control saying it changed.
                if let Edit::Set(path, value) = &edit
                    && path == "theme.colors.accent"
                    && self.draft.item("theme.focus_ring.gradient").is_some()
                {
                    let gradient = set("theme.focus_ring.gradient.from", value.clone());
                    if self.draft.edit(&gradient).is_ok() {
                        self.pending.push(gradient);
                    }
                }

                self.pending.push(edit);
                self.error = None;
                self.flush()
            }
            Err(error) => {
                self.error = Some(error);
                Task::none()
            }
        }
    }

    fn flush(&mut self) -> Task<Message> {
        if self.saving || self.pending.is_empty() {
            return Task::none();
        }
        self.pending.clear();
        if self.draft.source == self.current.source {
            return Task::none();
        }
        self.start_save(self.draft.source.clone())
    }

    fn start_save(&mut self, desired: String) -> Task<Message> {
        self.saving = true;
        self.saving_previous = self.current.source.clone();
        let expected = self.current.source.clone();
        let path = self.path.clone();
        cosmic::task::future(async move {
            // Parsing XKB, disk fsync, and IPC must never block native UI events.
            let (send, receive) = cosmic::iced::futures::channel::oneshot::channel();
            std::thread::spawn(move || {
                let result = store::save(&path, &expected, &desired).map(|snapshot| {
                    let live = path == store::config_path() && store::reload_running();
                    (snapshot, live)
                });
                let _ = send.send(result);
            });
            Message::Saved(
                receive
                    .await
                    .unwrap_or_else(|_| Err("Settings worker stopped unexpectedly.".into())),
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use cosmic::Application;

    use super::*;
    fn app() -> App {
        App::init(
            Core::default(),
            (
                PathBuf::from("/unused/settings-test.kdl"),
                Snapshot::parse(String::new()),
                None,
            ),
        )
        .0
    }

    fn shortcuts(count: usize) -> App {
        let mut app = app();
        app.page = Page::Shortcuts;
        let source = (0..count)
            .map(|index| format!("binding \"Super+F{index}\" \"none\"\n"))
            .collect::<String>();
        app.draft = Snapshot::parse(source).unwrap();
        assert_eq!(app.draft.records("bindings"), count);
        app
    }

    fn tree_nodes(tree: &cosmic::iced::advanced::widget::Tree) -> usize {
        1 + tree.children.iter().map(tree_nodes).sum::<usize>()
    }

    #[test]
    fn wallpaper_preview_uses_presented_state_and_tracks_mode_changes() {
        let mut app = app();
        app.path = store::config_path();
        app.current = Snapshot::parse("theme { background { path \"/saved.png\"; }; }".into()).unwrap();
        app.resolved.presented.tokens.background.path = Some("/active.png".into());
        assert_eq!(app.active_wallpaper_path(), "/active.png");

        app.page = Page::Wallpaper;
        let mut snapshot = app.resolved.clone();
        snapshot.presented.tokens.background.path =
            Some(ferese_config::default_wallpaper_for(ferese_config::theme::Appearance::Light).into());
        let task = app.update(Message::ThemeChanged(Box::new(snapshot)));
        assert!(task.units() > 0);
        assert_eq!(
            app.active_wallpaper_path(),
            ferese_config::default_wallpaper_for(ferese_config::theme::Appearance::Light)
        );
    }

    #[test]
    fn wallpaper_preview_config_preserves_its_mode_and_relative_paths() {
        let mut app = app();
        app.path = PathBuf::from("/preview/config.kdl");
        app.current = Snapshot::parse("theme { mode \"light\"; background { path \"shared.png\"; }; light { background { path \"light.png\"; }; }; }".into()).unwrap();
        assert_eq!(app.active_wallpaper_path(), "/preview/light.png");
        app.wallpaper.target = wallpaper_controls::Target::Dark;
        assert_eq!(app.active_wallpaper_path(), "/preview/light.png");
    }

    #[test]
    fn choosing_a_default_clears_old_input_and_keeps_the_other_mode() {
        let mut app = app();
        app.saving = true;
        app.wallpaper.target = wallpaper_controls::Target::Light;
        app.draft = Snapshot::parse("theme { dark { background { path \"/custom-dark.png\"; }; }; }".into()).unwrap();
        app.inputs
            .insert("theme.light.background.path".into(), "/unfinished.png".into());
        let _ = app.update(Message::UseDefaultWallpaper(ferese_config::theme::Appearance::Light));
        assert!(!app.inputs.contains_key("theme.light.background.path"));
        assert_eq!(
            app.draft.string("theme.light.background.path", ""),
            ferese_config::default_wallpaper_for(ferese_config::theme::Appearance::Light)
        );
        assert_eq!(app.draft.string("theme.dark.background.path", ""), "/custom-dark.png");
    }

    #[test]
    fn wallpaper_picker_keeps_its_target_when_selection_changes() {
        let mut app = app();
        app.saving = true;
        app.wallpaper.target = wallpaper_controls::Target::Dark;
        let _ = app.update(Message::WallpaperPicked(
            wallpaper_controls::Target::Light,
            Ok(Some("/chosen.png".into())),
        ));
        assert_eq!(app.draft.string("theme.light.background.path", ""), "/chosen.png");
        assert!(app.draft.item("theme.dark.background.path").is_none());
    }

    #[test]
    fn shortcuts_build_controls_only_for_mounted_rows() {
        let mut app = shortcuts(100);
        app.visible_rows.extend((0..4).map(|index| ("bindings", index)));
        let visible = tree_nodes(&cosmic::iced::advanced::widget::Tree::new(app.page_view().as_widget()));
        app.visible_rows.extend((0..100).map(|index| ("bindings", index)));
        let eager = tree_nodes(&cosmic::iced::advanced::widget::Tree::new(app.page_view().as_widget()));
        assert!(eager > visible + 96 * 10, "visible={visible}, eager={eager}");
    }

    #[test]
    fn virtual_rows_keep_unsaved_text_mounted_until_unfocus() {
        let mut app = shortcuts(10);
        let _ = app.update(Message::RowVisibility("bindings", 5, true));
        let _ = app.update(Message::Draft("bindings.5.argument".into(), "my command".into()));
        let _ = app.update(Message::RowVisibility("bindings", 5, false));
        assert!(app.visible_rows.contains(&("bindings", 5)));
        assert_eq!(app.inputs["bindings.5.argument"], "my command");
        let _ = app.update(Message::Commit(schema::text("bindings.5.argument", "Argument", "", "")));
        assert!(!app.visible_rows.contains(&("bindings", 5)));
        assert_eq!(app.draft.string("bindings.5.argument", ""), "my command");
    }

    #[test]
    fn navigation_resets_scroll_but_reselecting_the_same_page_does_not() {
        let mut app = app();
        app.visible_rows.insert(("theme", 2));
        let task = app.update(Message::Page(Page::Shortcuts));
        assert!(task.units() > 0);
        assert_eq!(app.visible_rows, initial_visible_rows());
        assert_eq!(app.update(Message::Page(Page::Shortcuts)).units(), 0);
    }

    #[test]
    fn offscreen_theme_selection_defers_focus_until_the_row_is_mounted() {
        let mut app = app();
        let id = app.family_ids.last().unwrap().clone();
        let row = (app.family_ids.len() - 1) / 3;
        app.visible_rows.remove(&("theme", row));
        let _ = app.update(Message::Family(id.clone(), None));
        assert_eq!(app.gallery_focus, Some((id, None)));
        assert!(app.visible_rows.contains(&("theme", row)));
        let task = app.update(Message::RowVisibility("theme", row, true));
        assert!(task.units() > 0);
        assert!(app.gallery_focus.is_none());
    }

    #[test]
    fn animated_palette_does_not_rebuild_native_theme_or_family_navigation() {
        let mut app = app();
        app.native_palette = visuals::Palette::from_resolved(&app.resolved.theme);
        let palette = app.native_palette;
        let ids = app.family_ids.clone();
        let mut snapshot = app.resolved.clone();
        snapshot.presented.tokens.colors.surface_base = "#123456".into();
        let task = app.update(Message::ThemeChanged(Box::new(snapshot)));
        assert_eq!(task.units(), 0);
        assert_eq!(app.native_palette, palette);
        assert!(std::sync::Arc::ptr_eq(&ids, &app.family_ids));
    }

    #[test]
    #[ignore = "release timing sample; run with --ignored --nocapture"]
    fn profile_shortcuts_page_construction() {
        for count in [50, 200, 500] {
            let mut app = shortcuts(count);
            for mounted in [4, count] {
                app.visible_rows = (0..mounted).map(|index| ("bindings", index)).collect();
                let mut samples = Vec::new();
                for _ in 0..100 {
                    let started = std::time::Instant::now();
                    let view = app.page_view();
                    std::hint::black_box(cosmic::iced::advanced::widget::Tree::new(view.as_widget()));
                    samples.push(started.elapsed());
                }
                samples.sort_unstable();
                eprintln!(
                    "bindings={count} mounted={mounted} construction_median_us={} p95_us={}",
                    samples[50].as_micros(),
                    samples[95].as_micros()
                );
            }
        }
    }

    #[test]
    #[ignore = "release scroll/render sample; requires both renderer backends"]
    fn profile_dense_pages_on_both_renderers() {
        use cosmic::iced::advanced::renderer::{Headless, Renderer as _};
        use cosmic::iced::advanced::{Layout, Shell, clipboard, layout, mouse, renderer, widget::Tree};
        use cosmic::iced::{Color, Event, Font, Pixels, Point, Rectangle, Size};
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let bounds = Rectangle::with_size(Size::new(1000.0, 780.0));
        let scale = 1.25;
        let size = Size::new(1250, 975);

        for backend in ["wgpu", "tiny-skia"] {
            let mut renderer = runtime
                .block_on(<cosmic::Renderer as Headless>::new(
                    Font::default(),
                    Pixels(14.0),
                    Some(backend),
                ))
                .expect("requested renderer");
            eprintln!("renderer={}", Headless::name(&renderer));

            for page in [Page::Shortcuts, Page::Appearance] {
                let mut app = shortcuts(500);
                app.page = page;
                let theme = app.native_palette.native_theme();
                let mut tree: Option<Tree> = None;
                let mut samples = Vec::new();

                for frame in 0..48 {
                    let mut messages = Vec::new();
                    let mut clipboard = clipboard::Null;
                    let mut shell = Shell::new(&mut messages);
                    let cursor = mouse::Cursor::Available(Point::new(700.0, 450.0));
                    let started = std::time::Instant::now();
                    let mut view = app.page_view();
                    let tree = tree.get_or_insert_with(|| Tree::new(view.as_widget()));
                    tree.diff(view.as_widget_mut());
                    let node =
                        view.as_widget_mut()
                            .layout(tree, &renderer, &layout::Limits::new(Size::ZERO, bounds.size()));
                    view.as_widget_mut().update(
                        tree,
                        &Event::Mouse(mouse::Event::WheelScrolled {
                            delta: mouse::ScrollDelta::Lines {
                                x: 0.0,
                                y: if frame < 24 { -1.0 } else { 1.0 },
                            },
                        }),
                        Layout::new(&node),
                        cursor,
                        &renderer,
                        &mut clipboard,
                        &mut shell,
                        &bounds,
                    );
                    let layout_elapsed = started.elapsed();
                    renderer.reset(bounds);
                    let draw_started = std::time::Instant::now();
                    view.as_widget().draw(
                        tree,
                        &mut renderer,
                        &theme,
                        &renderer::Style {
                            text_color: Color::WHITE,
                            icon_color: Color::WHITE,
                            scale_factor: scale as f64,
                        },
                        Layout::new(&node),
                        cursor,
                        &bounds,
                    );
                    let draw_elapsed = draw_started.elapsed();
                    drop(view);
                    let render_started = std::time::Instant::now();
                    let pixels = Headless::screenshot(&mut renderer, size, scale, theme.cosmic().bg_color().into());
                    std::hint::black_box(pixels);
                    let render_elapsed = render_started.elapsed();

                    for message in messages {
                        let _ = app.update(message);
                    }

                    if frame >= 8 {
                        samples.push((
                            layout_elapsed.as_micros(),
                            draw_elapsed.as_micros(),
                            render_elapsed.as_micros(),
                        ));
                    }
                }

                for (stage, index) in [("layout", 0), ("draw_queue", 1), ("render_and_readback", 2)] {
                    let mut times = samples
                        .iter()
                        .map(|sample| match index {
                            0 => sample.0,
                            1 => sample.1,
                            _ => sample.2,
                        })
                        .collect::<Vec<_>>();
                    times.sort_unstable();
                    eprintln!(
                        "backend={backend} page={page:?} stage={stage} median_us={} p95_us={}",
                        times[times.len() / 2],
                        times[times.len() * 95 / 100]
                    );
                }
            }
        }
    }

    #[test]
    fn disabling_auto_keeps_the_effective_appearance() {
        for appearance in [
            ferese_config::theme::Appearance::Light,
            ferese_config::theme::Appearance::Dark,
        ] {
            let mut app = app();
            app.resolved.theme.appearance = appearance;
            let _ = app.update(Message::AutoAppearance(false));
            assert_eq!(
                app.draft.string("theme.mode", ""),
                if appearance == ferese_config::theme::Appearance::Light {
                    "light"
                } else {
                    "dark"
                }
            );
            let _ = app.update(Message::AutoAppearance(true));
            assert_eq!(app.draft.string("theme.mode", ""), "auto");
        }
    }

    #[test]
    fn file_picker_applies_to_the_scope_it_was_opened_for_and_cancel_keeps_it() {
        let mut app = app();
        let _ = app.update(Message::ThemeFileTarget(2));
        let _ = app.update(Message::ThemeFilePicked(
            "theme.light.file",
            Ok(Some("/tmp/light.kdl".into())),
        ));
        assert_eq!(app.draft.string("theme.light.file", ""), "/tmp/light.kdl");
        assert!(app.draft.item("theme.dark.file").is_none());
        let _ = app.update(Message::ThemeFilePicked("theme.light.file", Ok(None)));
        assert_eq!(app.draft.string("theme.light.file", ""), "/tmp/light.kdl");
    }

    #[test]
    fn notes_save_only_the_latest_revision_and_preserve_multiline_text() {
        let mut app = app();
        app.draft
            .edit(&Edit::Add(
                "desktop_widgets.notes".into(),
                vec![("id".into(), "test".into()), ("text".into(), "first\nsecond".into())],
            ))
            .unwrap();
        app.sync_notes();
        let editor = app.note_editors.get_mut("test").unwrap();
        editor.dirty = true;
        editor.revision = 2;
        let _ = app.update(Message::SaveNote("test".into(), 1));
        assert!(!app.saving);
        assert!(app.note_editors["test"].dirty);
        let _ = app.update(Message::SaveNote("test".into(), 2));
        assert!(app.saving);
        assert!(!app.note_editors["test"].dirty);
        assert_eq!(app.draft.string("desktop_widgets.notes.0.text", ""), "first\nsecond");
    }

    #[test]
    fn external_config_does_not_replace_an_unsaved_note() {
        let mut app = app();
        app.draft
            .edit(&Edit::Add(
                "desktop_widgets.notes".into(),
                vec![("id".into(), "test".into())],
            ))
            .unwrap();
        app.sync_notes();
        app.note_editors.get_mut("test").unwrap().dirty = true;
        let _ = app.update(Message::ExternalConfig(Snapshot::parse(
            "animations {\n    speed 0.5\n}\n".into(),
        )));
        assert!(app.note_editors.contains_key("test"));
        assert!(app.status.contains("externally"));
    }

    #[test]
    fn rapid_edits_are_serialized_and_not_lost_when_a_save_finishes() {
        let mut app = app();
        let _ = app.change(set("animations.speed", 0.75));
        assert!(app.saving);
        let completed = app.draft.clone();
        let _ = app.change(set("layout.inner_gap", 6.));
        assert_eq!(app.pending.len(), 1);
        let _ = app.update(Message::Saved(Ok((completed.clone(), true))));
        assert!(app.saving);
        assert!(app.pending.is_empty());
        assert_eq!(app.saving_previous, completed.source);
        assert_eq!(app.draft.number("layout.inner_gap", 0.), 6.);
        assert_eq!(app.draft.number("animations.speed", 0.), 0.75);
    }

    #[test]
    fn external_config_refreshes_idle_settings_but_preserves_active_edits() {
        let mut app = app();
        let external = Snapshot::parse("animations {\n    speed 0.5\n}\n".into()).unwrap();
        let _ = app.update(Message::ExternalConfig(Ok(external.clone())));
        assert_eq!(app.current.source, external.source);
        assert_eq!(app.draft.number("animations.speed", 0.), 0.5);
        app.inputs.insert("theme.colors.accent".into(), "#ffffff".into());
        let newer = Snapshot::parse("animations {\n    speed 0.8\n}\n".into()).unwrap();
        let _ = app.update(Message::ExternalConfig(Ok(newer)));
        assert_eq!(app.current.source, external.source);
        assert_eq!(app.inputs["theme.colors.accent"], "#ffffff");
        assert!(app.status.contains("externally"));
        let _ = app.update(Message::ExternalConfig(Err("invalid KDL".into())));
        assert_eq!(app.current.source, external.source);
        assert!(app.error.as_ref().unwrap().contains("invalid KDL"));
    }

    #[test]
    fn settings_has_no_native_headerbar() {
        assert!(!app().core.window.show_headerbar);
    }

    #[test]
    fn rejected_save_restores_committed_values_without_claiming_success() {
        let mut app = app();
        let _ = app.change(set("animations.speed", -1.));
        let _ = app.change(set("layout.inner_gap", 6.));
        let _ = app.update(Message::Saved(Err("invalid speed".into())));
        assert!(!app.saving);
        assert!(app.pending.is_empty());
        assert_eq!(app.current.source, app.draft.source);
        assert_eq!(app.status, "Not saved");
        assert_eq!(app.error.as_deref(), Some("invalid speed"));
    }
}
