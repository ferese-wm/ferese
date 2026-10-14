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

use super::*;
use cosmic::iced::keyboard::{Key, key::Named};
use ferese_animation::{AnimatedValue, SpringConfig};
use sources::{Category, Row, Source, Target};
use std::sync::Arc;

#[derive(Clone, Debug)]
pub(super) enum Message {
    Dismiss(window::Id),
    Input(window::Id, String),
    Browse(window::Id, Category),
    ActivateRow(window::Id, sources::ResultId),
    Accept(bool),
    Tick,
    AppsScrolled(window::Id, usize),
    Loaded(window::Id, Arc<apps::Apps>),
    Results(window::Id, u64, Vec<Row>, files::Status),
    Launched(window::Id, Result<(), String>),
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
    progress: AnimatedValue,
    height: AnimatedValue,
    tick: Instant,
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
        self.error.as_deref().or_else(|| self.file_status.message())
    }
    fn apps_browse(&self) -> bool {
        self.query.is_empty() && self.category == Some(Category::Apps)
    }
    fn target_height(&self) -> f64 {
        if self.query.is_empty() && self.category.is_none() {
            72.
        } else if self.query.is_empty() && self.category == Some(Category::Windows) {
            220.
        } else if self.apps_browse() {
            80. + self.rows.len().div_ceil(4).clamp(1, 3) as f64 * 108.
        } else if self.alternatives || (self.query.is_empty() && self.category.is_some()) {
            88. + self.rows.len().clamp(1, 5) as f64 * 60.
        } else {
            72.
        }
    }
    fn changed(&mut self) {
        self.search_cancel.cancel();
        self.apps_first_row = 0;
        self.revision = self.revision.wrapping_add(1);
        self.error = None;
        self.file_status = files::Status::Ready;
        self.preferred_result = None;
        self.selected = 0;
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
    pub(super) fn attach_launcher_effects(&mut self, id: window::Id, surface: &wl_surface::WlSurface) {
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
        let mut progress = AnimatedValue::new(0.);
        progress.set_target(1.);
        if !self.render.motion.motion_enabled() {
            progress.snap();
        }
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
            progress,
            height: AnimatedValue::new(72.),
            tick: Instant::now(),
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
                move |apps| cosmic::Action::App(super::Message::Launcher(Message::Loaded(id, apps))),
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
        launcher.progress.set_target(0.);
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
        let interval = if launcher.progress.is_animating() || launcher.height.needs_update(launcher.target_height()) {
            16
        } else if launcher.category == Some(Category::Windows) && launcher.query.is_empty() {
            500
        } else if matches!(launcher.file_status, files::Status::Indexing) {
            1000
        } else {
            return Subscription::none();
        };
        cosmic::iced::time::every(Duration::from_millis(interval)).map(|_| super::Message::Launcher(Message::Tick))
    }
    pub(super) fn refresh_launcher(&mut self) -> Task<super::Message> {
        self.merge_launcher_windows(false)
    }
    fn merge_launcher_windows(&mut self, force: bool) -> Task<super::Message> {
        let Some(launcher) = &mut self.launcher else {
            return Task::none();
        };
        if launcher.closing
            || (launcher.query.is_empty() && launcher.category.is_none())
            || !launcher.category.is_none_or(|category| category == Category::Windows)
        {
            return Task::none();
        }
        let changed = launcher
            .windows
            .sync(&self.snapshot, launcher.apps.as_deref(), &launcher.query);
        if !changed && !force {
            return Task::none();
        }
        let selected = launcher.rows.get(launcher.selected).map(|row| row.id.clone());
        launcher.rows.retain(|row| !matches!(row.target, Target::Window(_)));
        launcher.rows.extend(launcher.windows.rows().cloned());
        sources::rank(&mut launcher.rows);
        if launcher.category.is_none() {
            launcher.rows.truncate(100);
        }
        launcher.selected = selected
            .and_then(|id| launcher.rows.iter().position(|row| row.id == id))
            .unwrap_or(0);
        Task::none()
    }
    fn query_launcher(&mut self) -> Task<super::Message> {
        let Some(launcher) = &mut self.launcher else {
            return Task::none();
        };
        if launcher.running || launcher.closing || (launcher.query.is_empty() && launcher.category.is_none()) {
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
                cosmic::Action::App(super::Message::Launcher(Message::Results(id, revision, rows, status)))
            },
        )
    }
    pub(super) fn update_launcher(&mut self, message: Message) -> Task<super::Message> {
        if let Message::Finished(result) = message {
            if let Err(error) = result {
                self.notifications.service_error("Launcher action failed", &error);
            }
            return Task::none();
        }
        let Some(launcher) = &mut self.launcher else {
            return Task::none();
        };
        match message {
            Message::Dismiss(id) if id == launcher.id => self.close_launcher(),
            Message::AppsScrolled(id, first) if id == launcher.id && launcher.apps_browse() => {
                launcher.apps_first_row = first.min(launcher.rows.len().div_ceil(4).saturating_sub(1));
                Task::none()
            }
            Message::Input(id, query) if id == launcher.id && !launcher.closing => {
                launcher.ghost_visible = launcher.query.starts_with(&query) || query.starts_with(&launcher.query);
                launcher.query = query;
                launcher.alternatives = false;
                launcher.changed();
                self.query_launcher()
            }
            Message::Browse(id, category) if id == launcher.id && !launcher.closing => {
                launcher.category = Some(category);
                launcher.query.clear();
                launcher.alternatives = false;
                launcher.changed();
                launcher.thumbnails = Default::default();
                let focus = cosmic::widget::text_input::focus(launcher.input.clone());
                Task::batch([focus, self.query_launcher()])
            }
            Message::ActivateRow(id, row) if id == launcher.id && !launcher.closing => {
                let Some(index) = launcher.rows.iter().position(|candidate| candidate.id == row) else {
                    return Task::none();
                };
                launcher.selected = index;
                self.activate_launcher(false)
            }
            Message::Accept(alt) => self.activate_launcher(alt),
            Message::Loaded(id, apps) if id == launcher.id => {
                launcher.apps = Some(apps);
                self.query_launcher()
            }
            Message::Results(id, revision, rows, status) if id == launcher.id => {
                launcher.running = false;
                if revision != launcher.revision {
                    return self.query_launcher();
                }
                let selected = launcher
                    .preferred_result
                    .take()
                    .or_else(|| launcher.rows.get(launcher.selected).map(|row| row.id.clone()));
                launcher.rows = rows;
                launcher.file_status = status;
                let task = self.merge_launcher_windows(true);
                let launcher = self.launcher.as_mut().unwrap();
                launcher.selected = selected
                    .and_then(|id| launcher.rows.iter().position(|row| row.id == id))
                    .unwrap_or(0);
                task
            }
            Message::Launched(id, result) if id == launcher.id => match result {
                Ok(()) => self.close_launcher(),
                Err(error) => {
                    launcher.activating = false;
                    launcher.error = Some(error);
                    Task::none()
                }
            },
            Message::Tick => {
                let now = Instant::now();
                let delta = now
                    .duration_since(launcher.tick)
                    .min(Duration::from_millis(64))
                    .mul_f64(self.render.motion.speed);
                launcher.tick = now;
                let base = self.render.motion.spring_config().unwrap_or_default();
                let stiffness = base.stiffness * if launcher.closing { 1.6 } else { 1. };
                let spring = SpringConfig {
                    stiffness,
                    damping: 2. * (stiffness * base.mass).sqrt(),
                    position_tolerance: 0.001,
                    velocity_tolerance: 0.001,
                    crossing: ferese_animation::CrossingPolicy::NoCrossing,
                    ..base
                };
                launcher.height.set_target(launcher.target_height());
                if self.render.motion.motion_enabled() {
                    launcher.progress.advance(delta, spring);
                    launcher.height.advance(delta, spring);
                } else {
                    launcher.progress.snap();
                    launcher.height.snap();
                }
                if launcher.closing && !launcher.progress.is_animating() {
                    return self.finish_launcher();
                }
                if !launcher.closing && launcher.query.is_empty() && launcher.category == Some(Category::Windows) {
                    if let Some(control) = &self.control {
                        launcher
                            .thumbnails
                            .refresh(&launcher.rows, launcher.selected, control, now);
                    }
                }
                if !launcher.closing
                    && now.duration_since(launcher.refresh) >= Duration::from_secs(1)
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
    pub(super) fn launcher_event(&mut self, event: &Event, id: window::Id) -> Option<Task<super::Message>> {
        let launcher = self.launcher.as_mut().filter(|l| l.id == id)?;
        if let Event::Window(window::Event::Opened { .. }) = event {
            return Some(Task::batch([
                cosmic::widget::text_input::focus(launcher.input.clone()),
                window::run(id, native_wayland_surface)
                    .map(move |surface| cosmic::Action::App(super::Message::NativeSurface(id, surface))),
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
        if matches!(event, Event::Mouse(cosmic::iced::mouse::Event::ButtonPressed(_))) {
            launcher.ghost_visible = false;
        }

        let Event::Keyboard(cosmic::iced::keyboard::Event::KeyPressed { key, modifiers, .. }) = event else {
            return None;
        };
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
            let next = match key {
                Key::Named(Named::ArrowLeft) => Some(launcher.selected.saturating_sub(1)),
                Key::Named(Named::ArrowRight) => Some(launcher.selected + 1),
                Key::Named(Named::ArrowUp) => Some(launcher.selected.saturating_sub(4)),
                Key::Named(Named::ArrowDown) => Some(launcher.selected + 4),
                _ => None,
            };
            if let Some(next) = next {
                launcher.selected = next.min(launcher.rows.len().saturating_sub(1));
                let rows = launcher.rows.len().div_ceil(4);
                launcher.apps_first_row = (launcher.selected / 4).saturating_sub(2);
                return Some(cosmic::iced::widget::scrollable::snap_to(
                    launcher.apps_scroll_id.clone(),
                    cosmic::iced::widget::scrollable::RelativeOffset {
                        x: None,
                        y: Some(if rows > 3 {
                            (launcher.selected / 4).saturating_sub(2) as f32 / (rows - 3) as f32
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
                    launcher.selected = (launcher.selected + 1).min(launcher.rows.len().saturating_sub(1));
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
                launcher.selected = (launcher.selected + 1).min(launcher.rows.len().saturating_sub(1));
                Task::none()
            }
            Key::Named(Named::ArrowLeft)
                if launcher.query.is_empty() && launcher.category == Some(Category::Windows) =>
            {
                launcher.selected = launcher.selected.saturating_sub(1);
                Task::none()
            }
            Key::Named(Named::Tab) if !launcher.query.is_empty() => {
                if let Some(row) = launcher.rows.get(launcher.selected) {
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
        if launcher.closing || launcher.running || launcher.activating {
            return Task::none();
        }
        if launcher.query.is_empty() && launcher.category.is_none() && launcher.category_focused {
            let category = [Category::Apps, Category::Files, Category::Windows][launcher.selected.min(2)];
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
        launcher.activating = true;
        Task::perform(
            async move {
                tokio::task::spawn_blocking(move || launch::run(target, &terminal))
                    .await
                    .unwrap_or_else(|e| Err(e.to_string()))
            },
            move |result| cosmic::Action::App(super::Message::Launcher(Message::Launched(id, result))),
        )
    }
}
