mod animation;
mod apps;
pub(super) mod files;
mod icons;
mod launch;
mod sources;
#[cfg(test)]
mod tests;
mod thumbnails;
mod view;
mod work;

use super::{
    Anchor, Border, Color, ContentFit, Duration, EffectsBinding, Element, Event, FereseShell,
    IcedMargin, IcedOutput, Instant, KeyboardInteractivity, Layer, Length, PlatformSpecific,
    SctkLayerSurfaceSettings, ShellControl, Subscription, Task, bar_icon, button,
    color_with_opacity, container, destroy_layer_surface, ferese_surface_effects_v1, image, motion,
    native_wayland_surface, row, system_modal, theme, wayland, window, wl_surface,
};
use cosmic::iced::keyboard::Key;
use cosmic::iced::keyboard::key::Named;
use sources::{Category, Row, Source, Target};
use std::cell::RefCell;
use std::sync::Arc;

#[derive(Clone, Debug)]
pub(super) enum Message {
    Dismiss(window::Id),
    Input(window::Id, String),
    Browse(window::Id, Category),
    ActivateRow(window::Id, sources::ResultId),
    Accept(bool),
    RefreshThumbnails(window::Id),
    RefreshIndex(window::Id),
    Closed(window::Id),
    AppsScrolled(window::Id, usize),
    Loaded(window::Id, Arc<apps::Apps>),
    Results(window::Id, u64, Vec<Row>, files::Status),
    Launched(window::Id, String, Result<(), String>),
    Finished(Result<(), String>),
}

pub(super) struct Launcher {
    pub id: window::Id,
    pub(super) output: String,
    input: cosmic::widget::Id,
    query: String,
    category: Option<Category>,
    alternatives: bool,
    category_focused: bool,
    ghost_visible: bool,
    selected: usize,
    selection_missing: bool,
    viewport: (u32, u32),
    rows: Vec<Row>,
    apps: Option<Arc<apps::Apps>>,
    revision: u64,
    running: bool,
    activating: bool,
    after_close: Option<Target>,
    error: Option<String>,
    file_status: files::Status,
    preferred_result: Option<sources::ResultId>,
    closing: bool,
    keyboard_nav: bool,
    animation: RefCell<animation::Animation>,
    thumbnails: thumbnails::Thumbnails,
    effects: Option<EffectsBinding>,
    regions: motion::Regions,
    refresh: Instant,
    lifetime: work::Cancellation,
    search_cancel: work::Cancellation,
    file_icons: Arc<std::sync::Mutex<icons::FileIcons>>,
    apps_first_row: usize,
    windows: sources::Windows,
    apps_scroll_id: cosmic::widget::Id,
}
impl Launcher {
    fn status_message(&self) -> Option<&str> {
        self.error.as_deref().or_else(|| {
            if self.activating {
                Some("Opening…")
            } else if self.selection_missing {
                Some("That result is no longer available. Choose another result.")
            } else if self.running || self.apps.is_none() {
                Some("Searching…")
            } else {
                self.file_status.message().or_else(|| {
                    (!self.query.is_empty() && self.rows.is_empty()).then_some("No matches")
                })
            }
        })
    }
    fn selected_row(&self) -> Option<&Row> {
        (!self.selection_missing)
            .then(|| self.rows.get(self.selected))
            .flatten()
    }
    fn restore_selection(&mut self, selected: Option<sources::ResultId>) {
        if let Some(id) = selected {
            match self.rows.iter().position(|row| row.id == id) {
                Some(index) => self.selected = index,
                None => self.selection_missing = true,
            }
        }
        self.selected = self.selected.min(self.rows.len().saturating_sub(1));
    }
    fn grid_columns(&self) -> usize {
        ((self.viewport.0.saturating_sub(32) / 160) as usize).clamp(1, 4)
    }
    fn grid_rows(&self) -> usize {
        ((self.viewport.1.saturating_sub(80) / 108) as usize).clamp(1, 3)
    }
    fn list_rows(&self) -> usize {
        ((self.viewport.1.saturating_sub(88) / 60) as usize).clamp(1, 5)
    }
    fn apps_browse(&self) -> bool {
        self.query.is_empty() && self.category == Some(Category::Apps)
    }
    fn target_height(&self) -> f64 {
        let height = if self.query.is_empty() && self.category.is_none() {
            72.
        } else if self.query.is_empty() && self.category == Some(Category::Windows) {
            220.
        } else if self.apps_browse() {
            80. + self
                .rows
                .len()
                .div_ceil(self.grid_columns())
                .clamp(1, self.grid_rows()) as f64
                * 108.
        } else if self.alternatives || (self.query.is_empty() && self.category.is_some()) {
            88. + self.rows.len().clamp(1, self.list_rows()) as f64 * 60.
        } else {
            72.
        };
        height.min(f64::from(self.viewport.1))
    }
    fn changed(&mut self) {
        self.search_cancel.cancel();
        self.apps_first_row = 0;
        self.revision = self.revision.wrapping_add(1);
        self.error = None;
        self.file_status = files::Status::Ready;
        self.preferred_result = None;
        self.selected = 0;
        self.selection_missing = false;
        self.category_focused = false;
        self.rows.clear();
    }
    fn escape(&mut self) -> bool {
        if !self.query.is_empty() {
            self.query.clear();
            self.alternatives = false;
            self.changed();
            false
        } else if self.category.take().is_some() {
            self.alternatives = false;
            self.changed();
            false
        } else {
            true
        }
    }
}
impl FereseShell {
    pub(super) fn attach_launcher_effects(
        &mut self,
        id: window::Id,
        surface: &wl_surface::WlSurface,
    ) {
        let Some(launcher) = self.launcher.as_mut().filter(|launcher| launcher.id == id) else {
            return;
        };
        if launcher.effects.is_none() {
            match EffectsBinding::attach_role(&mut self.effects_context, surface, None, 0.) {
                Ok(binding) => launcher.effects = Some(binding),
                Err(error) => eprintln!("ferese-shell: launcher material unavailable: {error}"),
            }
        }
    }
    pub(super) fn toggle_launcher(&mut self, name: String) -> Task<super::Message> {
        if self.launcher.is_some() {
            return self.close_launcher();
        }
        let Some(output) = self
            .outputs
            .iter()
            .find(|o| o.name.as_deref() == Some(&name))
            .or_else(|| self.outputs.first())
        else {
            return Task::none();
        };
        let name = output.name.clone().unwrap_or(name);
        let wl_output = output.output.clone();
        let (output_width, output_height) = output.size.unwrap_or((1920, 1080));
        // Reserve the largest browse view once, avoiding buffer reallocations during springs.
        let width = 720u32.min(output_width.max(1) as u32);
        let height = 404u32.min((output_height - 32).max(1) as u32);
        let top = ((output_height as f32 * 0.3) as i32)
            .min(output_height - height as i32 - 16)
            .max(0);
        let dismissed = Task::batch([self.destroy_menu(), self.destroy_system_modal(true)]);
        let id = window::Id::unique();
        let lifetime = work::Cancellation::default();
        let load_cancel = lifetime.clone();
        self.launcher = Some(Launcher {
            id,
            output: name,
            input: cosmic::widget::Id::unique(),
            query: String::new(),
            category: None,
            alternatives: false,
            category_focused: false,
            ghost_visible: true,
            selected: 0,
            selection_missing: false,
            viewport: (width, height),
            rows: Vec::new(),
            apps: None,
            revision: 0,
            running: false,
            activating: false,
            after_close: None,
            error: None,
            file_status: files::Status::Ready,
            preferred_result: None,
            closing: false,
            keyboard_nav: self.core.keyboard_nav(),
            animation: RefCell::new(animation::Animation::new(
                self.render.motion,
                Instant::now(),
            )),
            thumbnails: Default::default(),
            effects: None,
            regions: Default::default(),
            refresh: Instant::now(),
            lifetime,
            search_cancel: Default::default(),
            file_icons: Default::default(),
            apps_first_row: 0,
            windows: Default::default(),
            apps_scroll_id: cosmic::widget::Id::unique(),
        });
        self.core.set_keyboard_nav(false);
        let surface = cosmic::surface::action::app_layer_shell::<Self>(
            |_| Default::default(),
            move |_| SctkLayerSurfaceSettings {
                id,
                layer: Layer::Overlay,
                keyboard_interactivity: KeyboardInteractivity::Exclusive,
                anchor: Anchor::TOP,
                output: IcedOutput::Output(wl_output.clone()),
                namespace: "ferese-launcher".into(),
                exclusive_zone: -1,
                size: Some((Some(width), Some(height))),
                margin: IcedMargin {
                    top,
                    ..Default::default()
                },
                ..Default::default()
            },
            Some(Box::new(move |app| app.view_launcher(id))),
        );
        Task::batch([
            dismissed,
            cosmic::task::message(cosmic::Action::Surface(surface)),
            Task::perform(
                async move {
                    tokio::task::spawn_blocking(move || apps::Apps::load(&load_cancel))
                        .await
                        .map(Arc::new)
                        .unwrap_or_else(|_| Arc::new(apps::Apps::empty()))
                },
                move |apps| {
                    cosmic::Action::App(super::Message::Launcher(Message::Loaded(id, apps)))
                },
            ),
        ])
    }
    pub(super) fn destroy_launcher(&mut self) -> Task<super::Message> {
        let Some(launcher) = self.launcher.take() else {
            return Task::none();
        };
        launcher.lifetime.cancel();
        launcher.search_cancel.cancel();
        self.core.set_keyboard_nav(launcher.keyboard_nav);
        destroy_layer_surface(launcher.id)
    }
    fn close_launcher(&mut self) -> Task<super::Message> {
        let Some(launcher) = &mut self.launcher else {
            return Task::none();
        };
        launcher.lifetime.cancel();
        launcher.search_cancel.cancel();
        launcher.closing = true;
        let height = launcher.target_height();
        launcher
            .animation
            .get_mut()
            .retarget(height, true, self.render.motion, Instant::now());
        if !self.render.motion.motion_enabled() {
            return self.finish_launcher();
        }
        Task::none()
    }
    fn finish_launcher(&mut self) -> Task<super::Message> {
        let target = self.launcher.as_mut().and_then(|l| l.after_close.take());
        let close = self.destroy_launcher();
        if let Some(target) = target {
            let terminal = self.config.terminal.clone();
            close.chain(Task::perform(
                async move {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    tokio::task::spawn_blocking(move || launch::run(target, &terminal))
                        .await
                        .unwrap_or_else(|e| Err(e.to_string()))
                },
                |result| cosmic::Action::App(super::Message::Launcher(Message::Finished(result))),
            ))
        } else {
            close
        }
    }
    pub(super) fn launcher_subscription(&self) -> Subscription<super::Message> {
        let Some(launcher) = &self.launcher else {
            return Subscription::none();
        };
        if launcher.closing {
            return Subscription::none();
        }
        let id = launcher.id;
        let mut subscriptions = Vec::new();
        if launcher.category == Some(Category::Windows) && launcher.query.is_empty() {
            subscriptions.push(
                cosmic::iced::time::every(Duration::from_millis(500))
                    .with(id)
                    .map(|(id, _)| super::Message::Launcher(Message::RefreshThumbnails(id))),
            );
        }
        if matches!(launcher.file_status, files::Status::Indexing) {
            subscriptions.push(
                cosmic::iced::time::every(Duration::from_secs(1))
                    .with(id)
                    .map(|(id, _)| super::Message::Launcher(Message::RefreshIndex(id))),
            );
        }
        Subscription::batch(subscriptions)
    }
    pub(super) fn refresh_launcher(&mut self) -> Task<super::Message> {
        self.merge_launcher_windows(false)
    }
    fn merge_launcher_windows(&mut self, force: bool) -> Task<super::Message> {
        let Some(launcher) = &mut self.launcher else {
            return Task::none();
        };
        if launcher.closing
            || launcher.activating
            || (launcher.query.is_empty() && launcher.category.is_none())
            || !launcher
                .category
                .is_none_or(|category| category == Category::Windows)
        {
            return Task::none();
        }
        let changed =
            launcher
                .windows
                .sync(&self.snapshot, launcher.apps.as_deref(), &launcher.query);
        if !changed && !force {
            return Task::none();
        }
        let selected = launcher.selected_row().map(|row| row.id.clone());
        launcher
            .rows
            .retain(|row| !matches!(row.target, Target::Window(_)));
        launcher.rows.extend(launcher.windows.rows().cloned());
        sources::rank(&mut launcher.rows);
        if launcher.category.is_none() {
            launcher.rows.truncate(100);
        }
        launcher.restore_selection(selected);
        // Request the first visible thumbnails with the result change. Subsequent
        // refreshes have their own deadline rather than riding animation frames.
        if launcher.category == Some(Category::Windows)
            && launcher.query.is_empty()
            && let Some(control) = &self.control
        {
            launcher
                .thumbnails
                .refresh(&launcher.rows, launcher.selected, control, Instant::now());
        }
        Task::none()
    }
    fn query_launcher(&mut self) -> Task<super::Message> {
        let Some(launcher) = &mut self.launcher else {
            return Task::none();
        };
        if launcher.running
            || launcher.activating
            || launcher.closing
            || (launcher.query.is_empty() && launcher.category.is_none())
        {
            return Task::none();
        }
        let Some(apps) = launcher.apps.clone() else {
            return Task::none();
        };
        launcher.running = true;
        launcher.search_cancel = work::Cancellation::default();
        let cancel = launcher.search_cancel.clone();
        let file_icons = launcher.file_icons.clone();
        let (id, revision, query, category) = (
            launcher.id,
            launcher.revision,
            launcher.query.clone(),
            launcher.category,
        );
        let files = self.launcher_files.clone();
        Task::perform(
            async move {
                tokio::task::spawn_blocking(move || {
                    let mut rows = Vec::new();
                    if !cancel.cancelled() && category.is_none_or(|c| c == Category::Apps) {
                        rows.extend(apps.search(&query, &cancel));
                    }
                    if !cancel.cancelled() && category.is_none_or(|c| c == Category::Files) {
                        rows.extend(files.search(&query, &cancel, &file_icons));
                    }
                    if !cancel.cancelled() && category.is_none() {
                        rows.extend(sources::Actions.query(&query));
                    }
                    if cancel.cancelled() {
                        return (Vec::new(), files::Status::Ready);
                    }
                    sources::rank(&mut rows);
                    if category.is_none() {
                        rows.truncate(100);
                    }
                    (
                        rows,
                        if category.is_none_or(|c| c == Category::Files) {
                            files.status()
                        } else {
                            files::Status::Ready
                        },
                    )
                })
                .await
                .unwrap_or_else(|e| (Vec::new(), files::Status::Unavailable(e.to_string())))
            },
            move |(rows, status)| {
                cosmic::Action::App(super::Message::Launcher(Message::Results(
                    id, revision, rows, status,
                )))
            },
        )
    }
    pub(super) fn update_launcher(&mut self, message: Message) -> Task<super::Message> {
        if let Message::Finished(result) = message {
            if let Err(error) = result {
                self.notifications
                    .service_error("Launcher action failed", &error);
            }
            return Task::none();
        }
        if let Message::Launched(id, target, result) = message {
            if let Some(launcher) = &mut self.launcher
                && launcher.id == id
                && launcher.activating
                && !launcher.closing
            {
                launcher.activating = false;
                return match result {
                    Ok(()) => self.close_launcher(),
                    Err(error) => {
                        launcher.error = Some(format!("Could not open {target}: {error}"));
                        cosmic::widget::text_input::focus(launcher.input.clone())
                    }
                };
            }
            if let Err(error) = result {
                self.notifications
                    .service_error(&format!("Could not open {target}"), &error);
            }
            return Task::none();
        }
        let Some(launcher) = &mut self.launcher else {
            return Task::none();
        };
        match message {
            Message::Dismiss(id) if id == launcher.id => self.close_launcher(),
            Message::AppsScrolled(id, first) if id == launcher.id && launcher.apps_browse() => {
                launcher.apps_first_row = first.min(
                    launcher
                        .rows
                        .len()
                        .div_ceil(launcher.grid_columns())
                        .saturating_sub(1),
                );
                Task::none()
            }
            Message::Input(id, query)
                if id == launcher.id && !launcher.closing && !launcher.activating =>
            {
                launcher.ghost_visible =
                    launcher.query.starts_with(&query) || query.starts_with(&launcher.query);
                launcher.query = query;
                launcher.alternatives = false;
                launcher.changed();
                self.query_launcher()
            }
            Message::Browse(id, category)
                if id == launcher.id && !launcher.closing && !launcher.activating =>
            {
                launcher.category = Some(category);
                launcher.query.clear();
                launcher.alternatives = false;
                launcher.changed();
                launcher.thumbnails = Default::default();
                let focus = cosmic::widget::text_input::focus(launcher.input.clone());
                Task::batch([focus, self.query_launcher()])
            }
            Message::ActivateRow(id, row)
                if id == launcher.id && !launcher.closing && !launcher.activating =>
            {
                let Some(index) = launcher
                    .rows
                    .iter()
                    .position(|candidate| candidate.id == row)
                else {
                    return Task::none();
                };
                launcher.selected = index;
                launcher.selection_missing = false;
                self.activate_launcher(false)
            }
            Message::Accept(alt) => self.activate_launcher(alt),
            Message::Loaded(id, apps) if id == launcher.id => {
                launcher.apps = Some(apps);
                self.query_launcher()
            }
            Message::Results(id, revision, rows, status)
                if id == launcher.id && !launcher.activating =>
            {
                launcher.running = false;
                if revision != launcher.revision {
                    return self.query_launcher();
                }
                let selected = launcher
                    .preferred_result
                    .take()
                    .or_else(|| launcher.selected_row().map(|row| row.id.clone()));
                launcher.rows = rows;
                launcher.file_status = status;
                let task = self.merge_launcher_windows(true);
                let launcher = self
                    .launcher
                    .as_mut()
                    .expect("merging launcher windows keeps the active launcher alive");
                launcher.restore_selection(selected);
                task
            }
            Message::Closed(id)
                if id == launcher.id
                    && launcher.closing
                    && !launcher
                        .animation
                        .borrow()
                        .sample(Instant::now())
                        .progress
                        .is_animating() =>
            {
                self.finish_launcher()
            }
            Message::RefreshThumbnails(id) if id == launcher.id && !launcher.closing => {
                if launcher.query.is_empty()
                    && launcher.category == Some(Category::Windows)
                    && let Some(control) = &self.control
                {
                    launcher.thumbnails.refresh(
                        &launcher.rows,
                        launcher.selected,
                        control,
                        Instant::now(),
                    );
                }
                Task::none()
            }
            Message::RefreshIndex(id) if id == launcher.id && !launcher.closing => {
                let now = Instant::now();
                if now.duration_since(launcher.refresh) >= Duration::from_secs(1)
                    && matches!(launcher.file_status, files::Status::Indexing)
                {
                    launcher.refresh = now;
                    return self.query_launcher();
                }
                Task::none()
            }
            _ => Task::none(),
        }
    }
    pub(super) fn launcher_event(
        &mut self,
        event: &Event,
        id: window::Id,
    ) -> Option<Task<super::Message>> {
        let launcher = self.launcher.as_mut().filter(|l| l.id == id)?;
        if let Event::Window(window::Event::Opened { .. }) = event {
            return Some(Task::batch([
                cosmic::widget::text_input::focus(launcher.input.clone()),
                window::run(id, native_wayland_surface).map(move |surface| {
                    cosmic::Action::App(super::Message::NativeSurface(id, surface))
                }),
            ]));
        }
        if matches!(
            event,
            Event::PlatformSpecific(PlatformSpecific::Wayland(wayland::Event::Layer(
                wayland::LayerEvent::Unfocused,
                ..
            )))
        ) {
            return Some(self.destroy_launcher());
        }
        if let Event::Window(window::Event::Closed) = event {
            return Some(self.destroy_launcher());
        }
        if launcher.closing {
            return Some(Task::none());
        }
        if matches!(
            event,
            Event::Mouse(cosmic::iced::mouse::Event::ButtonPressed(_))
        ) {
            launcher.ghost_visible = false;
        }

        let Event::Keyboard(cosmic::iced::keyboard::Event::KeyPressed { key, modifiers, .. }) =
            event
        else {
            return None;
        };
        if launcher.activating {
            return Some(if *key == Key::Named(Named::Escape) {
                self.close_launcher()
            } else {
                Task::none()
            });
        }
        let selection_navigation = matches!(key, Key::Named(Named::ArrowDown | Named::ArrowUp))
            || (launcher.query.is_empty()
                && matches!(key, Key::Named(Named::ArrowLeft | Named::ArrowRight)));
        if selection_navigation && launcher.selection_missing && !launcher.rows.is_empty() {
            launcher.selected = 0;
            launcher.selection_missing = false;
            return Some(Task::none());
        }
        if launcher.query.is_empty() && launcher.category.is_none() {
            match key {
                Key::Named(Named::ArrowDown | Named::ArrowRight | Named::Tab) => {
                    launcher.selected = if launcher.category_focused {
                        (launcher.selected + 1) % 3
                    } else {
                        0
                    };
                    launcher.category_focused = true;
                    return Some(cosmic::widget::text_input::focus(launcher.input.clone()));
                }
                Key::Named(Named::ArrowLeft | Named::ArrowUp) => {
                    launcher.selected = (launcher.selected + 2) % 3;
                    launcher.category_focused = true;
                    return Some(cosmic::widget::text_input::focus(launcher.input.clone()));
                }
                _ => {}
            }
        }
        if launcher.apps_browse() {
            let columns = launcher.grid_columns();
            let visible_rows = launcher.grid_rows();
            let next = match key {
                Key::Named(Named::ArrowLeft) => Some(launcher.selected.saturating_sub(1)),
                Key::Named(Named::ArrowRight) => Some(launcher.selected + 1),
                Key::Named(Named::ArrowUp) => Some(launcher.selected.saturating_sub(columns)),
                Key::Named(Named::ArrowDown) => Some(launcher.selected + columns),
                _ => None,
            };
            if let Some(next) = next {
                launcher.selected = next.min(launcher.rows.len().saturating_sub(1));
                let rows = launcher.rows.len().div_ceil(columns);
                launcher.apps_first_row =
                    (launcher.selected / columns).saturating_sub(visible_rows - 1);
                return Some(cosmic::iced::widget::scrollable::snap_to(
                    launcher.apps_scroll_id.clone(),
                    cosmic::iced::widget::scrollable::RelativeOffset {
                        x: None,
                        y: Some(if rows > visible_rows {
                            (launcher.selected / columns).saturating_sub(visible_rows - 1) as f32
                                / (rows - visible_rows) as f32
                        } else {
                            0.
                        }),
                    },
                ));
            }
        }
        Some(match key {
            Key::Named(Named::Escape) => {
                if launcher.escape() {
                    self.close_launcher()
                } else {
                    let focus = cosmic::widget::text_input::focus(launcher.input.clone());
                    Task::batch([focus, self.query_launcher()])
                }
            }
            Key::Named(Named::ArrowDown) => {
                if launcher.alternatives || launcher.query.is_empty() {
                    launcher.selected =
                        (launcher.selected + 1).min(launcher.rows.len().saturating_sub(1));
                }
                launcher.alternatives = true;
                Task::none()
            }
            Key::Named(Named::ArrowUp) => {
                launcher.selected = launcher.selected.saturating_sub(1);
                Task::none()
            }
            Key::Named(Named::ArrowRight)
                if launcher.query.is_empty() && launcher.category == Some(Category::Windows) =>
            {
                launcher.selected =
                    (launcher.selected + 1).min(launcher.rows.len().saturating_sub(1));
                Task::none()
            }
            Key::Named(Named::ArrowLeft)
                if launcher.query.is_empty() && launcher.category == Some(Category::Windows) =>
            {
                launcher.selected = launcher.selected.saturating_sub(1);
                Task::none()
            }
            Key::Named(Named::Tab) if !launcher.query.is_empty() => {
                if let Some(row) = launcher.selected_row() {
                    let preferred = row.id.clone();
                    launcher.query = row.title.clone();
                    launcher.changed();
                    launcher.preferred_result = Some(preferred);
                }
                let focus = cosmic::widget::text_input::focus(launcher.input.clone());
                let cursor = cosmic::widget::text_input::move_cursor_to_end(launcher.input.clone());
                Task::batch([focus, cursor, self.query_launcher()])
            }
            Key::Named(Named::ArrowLeft | Named::ArrowRight | Named::Home) => {
                launcher.ghost_visible = false;
                Task::none()
            }
            Key::Named(Named::End) => {
                launcher.ghost_visible = true;
                Task::none()
            }
            Key::Named(Named::Tab) => cosmic::widget::text_input::focus(launcher.input.clone()),
            Key::Named(Named::Enter) => self.activate_launcher(modifiers.alt()),
            _ => Task::none(),
        })
    }
    fn activate_launcher(&mut self, alt: bool) -> Task<super::Message> {
        let Some(launcher) = &mut self.launcher else {
            return Task::none();
        };
        if launcher.closing || launcher.running || launcher.activating || launcher.selection_missing
        {
            return Task::none();
        }
        if launcher.query.is_empty() && launcher.category.is_none() && launcher.category_focused {
            let category =
                [Category::Apps, Category::Files, Category::Windows][launcher.selected.min(2)];
            let id = launcher.id;
            return self.update_launcher(Message::Browse(id, category));
        }
        let Some(row) = launcher.rows.get(launcher.selected) else {
            return Task::none();
        };
        if row.requires_alt() && !alt {
            launcher.error = Some("Use Alt+Enter to confirm".into());
            return Task::none();
        }
        let target = row.target.clone();
        let label = row.title.clone();
        let id = launcher.id;
        let terminal = self.config.terminal.clone();
        if matches!(target, Target::Action(sources::Action::Screenshot)) {
            launcher.after_close = Some(target);
            return self.close_launcher();
        }
        if let Target::Window(window) = target {
            if let Some(control) = &self.control {
                if alt {
                    if let Some(workspace) = self
                        .snapshot
                        .outputs
                        .iter()
                        .find(|o| o.name == launcher.output)
                        .map(|o| o.active_workspace)
                    {
                        control.move_window_to_workspace(window, workspace);
                    }
                } else {
                    control.activate_window(window);
                }
            }
            return self.close_launcher();
        }
        use sources::Action;
        if let Target::Action(action) = target {
            let power = match action {
                Action::Poweroff => Some(system_modal::PowerAction::Poweroff),
                Action::Reboot => Some(system_modal::PowerAction::Reboot),
                Action::Suspend => Some(system_modal::PowerAction::Suspend),
                _ => None,
            };
            if let Some(power) = power {
                let close = self.destroy_launcher();
                let open = self.open_system_modal(power, None);
                return Task::batch([close, open]);
            }
        }
        launcher.error = None;
        launcher.activating = true;
        Task::perform(
            async move {
                tokio::task::spawn_blocking(move || launch::run(target, &terminal))
                    .await
                    .unwrap_or_else(|e| Err(e.to_string()))
            },
            move |result| {
                cosmic::Action::App(super::Message::Launcher(Message::Launched(
                    id,
                    label.clone(),
                    result,
                )))
            },
        )
    }
}
