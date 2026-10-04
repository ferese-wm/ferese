mod bar;
mod desktop_widgets;
mod surfaces;

use bar::*;
use desktop_widgets::*;
use surfaces::*;

mod clock;
mod compositor_ipc;
mod config;
mod control;
mod display_mode;
mod keybinding_guide;
mod motion;
mod note_store;
mod notification_ui;
mod notifications;
mod recording;
mod renderer;
mod status;
mod status_ui;
mod system_modal;

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use cosmic::app::{Core, Settings, Task};
use cosmic::iced::event::{PlatformSpecific, wayland};
use cosmic::iced::platform_specific::runtime::wayland::layer_surface::{
    IcedMargin, IcedOutput, SctkLayerSurfaceSettings,
};
use cosmic::iced::platform_specific::shell::commands::layer_surface::{
    Anchor, KeyboardInteractivity, Layer, destroy_layer_surface, set_anchor, set_exclusive_zone, set_input_zone,
    set_margin, set_size,
};
use cosmic::iced::{
    Background, Border, Color, ContentFit, Event, Length, Limits, Subscription, alignment, event, window,
};
use cosmic::widget::{button, container, icon, image, row};
use cosmic::{Element, theme};
use ferese_protocols::effects::v1::client::ferese_effects_manager_v1::FereseEffectsManagerV1;
use ferese_protocols::effects::v1::client::ferese_surface_effects_v1;
use ferese_protocols::effects::v1::client::ferese_surface_effects_v1::FereseSurfaceEffectsV1;
use ferese_theme::calendar::{format_bar_time, stacked_digits as stacked_clock_digits};
use ferese_theme::icons::{accented as accented_icon, tinted as bar_icon};
use jiff::Zoned;
use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::{wl_output, wl_registry, wl_surface};
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle, delegate_noop};

use crate::config::{ShellConfig, ShellTheme, WallpaperMode};
use crate::control::{ShellControl, ShellSnapshot};

const APP_ID: &str = "dev.ferese.Shell";

// Older libcosmic backends expose native surfaces in frame events. Keep
// that compatibility path gated; current backends use Window::Opened and
// window::run to retrieve handles without updates at the display refresh rate.
static EFFECT_FRAME_PENDING: AtomicBool = AtomicBool::new(true);
static SHELL_FONT: std::sync::OnceLock<std::sync::RwLock<cosmic::font::Font>> = std::sync::OnceLock::new();

fn configured_font(family: Option<&str>) -> cosmic::font::Font {
    ferese_theme::font(family)
}

fn shell_font() -> cosmic::font::Font {
    *SHELL_FONT
        .get_or_init(|| std::sync::RwLock::new(cosmic::font::default()))
        .read()
        .unwrap()
}

fn text<'a>(
    content: impl Into<std::borrow::Cow<'a, str>> + 'a,
) -> cosmic::widget::Text<'a, cosmic::Theme, cosmic::Renderer> {
    ferese_theme::text(content, shell_font())
}

#[derive(Clone, Copy)]
struct BarMetrics {
    height: f32,
    text_size: u16,
    icon_size: u16,
    overview_icon_size: u16,
    control_height: f32,
    group_height: f32,
    group_item_height: f32,
}

impl From<ShellTheme> for BarMetrics {
    fn from(theme: ShellTheme) -> Self {
        let height = theme.bar_height;
        let control_height = (height - 4.0).max(21.0).min(height);
        // Leave two logical pixels inside the group border on each side,
        // including compact bars with 20 px icons.
        let group_height = control_height.max(24.0).min(height);

        Self {
            height,
            // Logical sizes: Wayland/iced applies each surface's output scale.
            // Compacting the bar must not also shrink its readable content.
            text_size: 14,
            icon_size: 20,
            overview_icon_size: 20,
            control_height,
            group_height,
            group_item_height: (group_height - 4.0).max(0.0),
        }
    }
}

fn main() -> cosmic::iced::Result {
    renderer::configure_shell();

    cosmic::iced::advanced::graphics::text::font_system()
        .write()
        .unwrap()
        .load_font(std::borrow::Cow::Borrowed(include_bytes!(
            "../../../assets/fonts/Comfortaa-Regular.otf"
        )));
    cosmic::iced::advanced::graphics::text::font_system()
        .write()
        .unwrap()
        .load_font(std::borrow::Cow::Borrowed(include_bytes!(
            "../../../assets/fonts/Cantarell-ExtraBold.otf"
        )));

    let mut config = config::load();
    motion::configure(config.animations, config.theme.material_radius);
    let compositor_wallpaper = std::env::var_os("FERESE_COMPOSITOR_WALLPAPER").is_some();
    if compositor_wallpaper {
        // The compositor owns one GPU image, shared across output renderers;
        // do not decode another copy or allocate full-screen software buffers.
        config.wallpaper.path = None;
    }
    let shell_font = configured_font(config.font_family.as_deref());
    let _ = SHELL_FONT.set(std::sync::RwLock::new(shell_font));
    // Decode alongside toolkit/GPU initialization, never during a UI draw.
    let wallpaper = config.wallpaper.path.clone().map(|path| {
        let (sender, receiver) = cosmic::iced::futures::channel::oneshot::channel();
        std::thread::spawn(move || {
            let result = cosmic::iced::advanced::graphics::image::load(&image::Handle::from_path(path))
                .map(|pixels| image::Handle::from_rgba(pixels.width(), pixels.height(), pixels.into_raw()))
                .map_err(|error| format!("{error:?}"));
            let _ = sender.send(result);
        });
        receiver
    });
    let mut settings = Settings::default()
        .no_main_window(true)
        .client_decorations(false)
        .transparent(true)
        .is_daemon(true);
    settings = settings
        .default_font(shell_font)
        .theme(config.theme.palette().native_theme());

    cosmic::app::run::<FereseShell>(settings, (config, wallpaper))
}

type WallpaperLoad = cosmic::iced::futures::channel::oneshot::Receiver<Result<image::Handle, String>>;

struct FereseShell {
    core: Core,
    bar_surface_id: window::Id,
    config: ShellConfig,
    wallpaper: Option<image::Handle>,
    control: Option<ShellControl>,
    snapshot: ShellSnapshot,
    overview_active: bool,
    clock: String,
    clock_service: clock::Service,
    desktop_clock: (String, String),
    outputs: Vec<OutputSurfaces>,
    notifications: notifications::Center,
    notification_surface: Option<notification_ui::NotificationSurface>,
    status_service: status::Service,
    status: status::Snapshot,
    recorder: recording::Recorder,
    calendar_offset: i32,
    status_error: Option<status::ActionError>,
    theme_error: Option<String>,
    menu: Option<status_ui::OpenMenu>,
    system_modal: Option<system_modal::SystemModal>,
    display_mode: display_mode::Model,
    guide_shown: bool,
    guide_load: keybinding_guide::LoadState,
    guide_attempts: u8,
    pending_power: Option<(window::Id, system_modal::PowerAction)>,
    note_editor: Option<DesktopNoteEditor>,
    note_drag: Option<NoteDrag>,
    note_pointer: std::collections::HashMap<window::Id, cosmic::iced::Point>,
    note_pending: Vec<note_store::Edit>,
    note_inflight: Vec<note_store::Edit>,
    note_saving: bool,
    note_error: Option<String>,
}

struct DesktopNoteEditor {
    id: String,
    content: cosmic::widget::text_editor::Content<cosmic::Renderer>,
    revision: u64,
}

struct NoteDrag {
    id: Option<String>,
    source: window::Id,
    overlay: window::Id,
    start: cosmic::iced::Point,
    origin: cosmic::iced::Point,
    position: cosmic::iced::Point,
    output_size: (i32, i32),
    obstacles: Vec<cosmic::iced::Rectangle>,
}

struct OutputSurfaces {
    output: wl_output::WlOutput,
    name: Option<String>,
    bar: window::Id,
    wallpaper: Option<window::Id>,
    clock: Option<window::Id>,
    notes: Vec<(String, window::Id)>,
    size: Option<(i32, i32)>,
    effects: Option<EffectsBinding>,
    hidden: bool,
}

#[derive(Clone, Debug)]
enum Message {
    ThemeChanged(Box<ferese_ipc::theme::Snapshot>),
    ThemeMode(ferese_config::theme::Mode),
    ThemeModeSet(Result<(), String>),
    BeginNoteEdit(String),
    NoteAction(String, cosmic::widget::text_editor::Action),
    SaveNote(String, u64),
    FinishNoteEdit,
    BeginNoteDrag(String, window::Id),
    BeginClockDrag(window::Id),
    NotesStored(Result<(), String>),
    WallpaperLoaded(Result<image::Handle, String>),
    Event(Event, window::Id),
    NativeSurface(window::Id, Result<(Connection, wl_surface::WlSurface), String>),
    ClockChanged(clock::Labels),
    RecorderEvent(recording::Event),
    RecorderElapsed,
    ControlReady,
    ActivateWorkspace(u64),
    ToggleOverview,
    StatusUpdated(status::Update),
    StartRecording,
    StopRecording,
    NotificationEvent(notifications::Event),
    NotificationTick,
    DismissNotification(u32),
    RemoveNotification(u32),
    InvokeNotification(u32, String),
    HoverNotification(u32, bool),
    ToggleNotificationHistory,
    ClearNotifications,
    ToggleNotificationGroup(String),
    RemoveNotificationGroup(String),
    AnimateMenu,
    OpenMenu(status_ui::Menu, cosmic::iced::Rectangle<i32>),
    OpenMenuOn(window::Id, status_ui::Menu, cosmic::iced::Rectangle<i32>),
    Control(status::Action),
    ShowGuide,
    GuideLoaded {
        generation: u64,
        manual: bool,
        output: Option<String>,
        result: Result<Vec<keybinding_guide::Entry>, String>,
    },
    OpenDisplays(Option<String>),
    DisplaysLoaded(u64, Result<display_mode::Inventory, String>),
    SelectDisplayMode(display_mode::Mode),
    ApplyDisplayMode,
    DisplayModeCompleted(u64, bool, Result<display_mode::Inventory, String>),
    KeepDisplayMode,
    RevertDisplayMode,
    SystemInhibitors(window::Id, Result<compositor_ipc::Approval, String>),
    ConfirmPower(status::Action),
    CancelPower,
    ExecutePower,
    AnimatePower,
    PowerCompleted(window::Id, Result<(), String>),
    CalendarMonth(i32),
    CalendarToday,
}

impl cosmic::Application for FereseShell {
    type Executor = cosmic::executor::Default;
    type Flags = (ShellConfig, Option<WallpaperLoad>);
    type Message = Message;

    const APP_ID: &'static str = APP_ID;

    fn core(&self) -> &Core {
        &self.core
    }

    fn core_mut(&mut self) -> &mut Core {
        &mut self.core
    }

    fn init(core: Core, (config, wallpaper): Self::Flags) -> (Self, Task<Self::Message>) {
        let bar_surface_id = window::Id::unique();
        let desktop_clock = config.desktop_widgets.clock.labels(&Zoned::now()).unwrap_or_default();
        let app = Self {
            core,
            bar_surface_id,
            notifications: notifications::Center::new(config.notifications.clone()),
            notification_surface: None,
            status_service: status::Service::start(config.status.settings_command.clone()),
            status: status::Snapshot::default(),
            recorder: recording::Recorder::default(),
            calendar_offset: 0,
            status_error: None,
            theme_error: None,
            menu: None,
            system_modal: None,
            display_mode: display_mode::Model::default(),
            guide_shown: false,
            guide_load: Default::default(),
            guide_attempts: 0,
            pending_power: None,
            note_editor: None,
            note_drag: None,
            note_pointer: Default::default(),
            note_pending: Vec::new(),
            note_inflight: Vec::new(),
            note_saving: false,
            note_error: None,
            clock_service: clock::Service::new(&config.desktop_widgets.clock),
            config,
            wallpaper: None,
            control: ShellControl::connect()
                .map_err(|error| {
                    eprintln!("ferese-shell: shell control unavailable: {error}");
                })
                .ok(),
            snapshot: ShellSnapshot::default(),
            overview_active: false,
            clock: current_time(),
            desktop_clock,
            outputs: Vec::new(),
        };
        let wallpaper_task = match wallpaper {
            Some(receiver) => cosmic::task::future(async move {
                cosmic::Action::App(Message::WallpaperLoaded(
                    receiver.await.unwrap_or_else(|error| Err(error.to_string())),
                ))
            }),
            None => Task::none(),
        };

        (app, wallpaper_task)
    }

    fn subscription(&self) -> Subscription<Self::Message> {
        Subscription::batch([
            self.control.as_ref().map_or_else(Subscription::none, |control| {
                control.subscription().map(|_| Message::ControlReady)
            }),
            ferese_theme_client::service::subscription().map(|snapshot| Message::ThemeChanged(Box::new(snapshot))),
            self.notifications.subscription().map(Message::NotificationEvent),
            self.notifications
                .tick_subscription()
                .map(|_| Message::NotificationTick),
            event::listen_with(|event, status, id| match &event {
                Event::Keyboard(cosmic::iced::keyboard::Event::KeyPressed {
                    key: cosmic::iced::keyboard::Key::Named(cosmic::iced::keyboard::key::Named::Enter),
                    ..
                }) if status == cosmic::iced::event::Status::Captured => None,
                Event::PlatformSpecific(PlatformSpecific::Wayland(wayland::Event::Frame(..))) => EFFECT_FRAME_PENDING
                    .load(Ordering::Relaxed)
                    .then_some(Message::Event(event, id)),
                Event::Keyboard(_)
                | Event::Mouse(cosmic::iced::mouse::Event::ButtonPressed(_))
                | Event::Window(window::Event::Opened { .. } | window::Event::Closed)
                | Event::PlatformSpecific(PlatformSpecific::Wayland(
                    wayland::Event::Popup(..) | wayland::Event::Layer(..) | wayland::Event::Output(..),
                )) => Some(Message::Event(event, id)),
                _ => None,
            }),
            self.clock_service
                .subscription(
                    self.outputs.iter().any(|output| !output.hidden),
                    self.outputs.iter().any(|output| {
                        output.clock.is_some() && self.config.desktop_widgets.clock.on_output(output.name.as_deref())
                    }),
                )
                .map(Message::ClockChanged),
            self.recorder.subscription().map(Message::RecorderEvent),
            self.recorder.elapsed_subscription().map(|_| Message::RecorderElapsed),
            self.status_service.subscription().map(Message::StatusUpdated),
            if self.config.desktop_widgets.clock.enabled
                || self
                    .config
                    .desktop_widgets
                    .notes
                    .iter()
                    .any(|note| note.enabled && note.interactive)
            {
                event::listen_with(|event, _, id| {
                    matches!(&event, Event::Mouse(_)).then_some(Message::Event(event, id))
                })
            } else {
                Subscription::none()
            },
        ])
    }

    fn update(&mut self, message: Self::Message) -> Task<Self::Message> {
        match message {
            Message::ThemeChanged(snapshot) => {
                let mut config = self.config.clone();
                config.apply_theme(&snapshot.presented);
                config.theme_mode = snapshot.mode;
                if snapshot.error != self.theme_error {
                    if let Some(error) = &snapshot.error {
                        self.notifications.service_error("Theme could not be loaded", error);
                    }
                    self.theme_error = snapshot.error;
                }
                self.apply_config(config)
            }
            Message::ThemeMode(mode) => cosmic::task::future(async move {
                let result = tokio::task::spawn_blocking(move || {
                    let mut connection = ferese_ipc::theme::Connection::connect().map_err(|e| e.to_string())?;
                    connection
                        .call("theme-set-mode", serde_json::json!({"mode": mode}))
                        .map(|_| ())
                })
                .await
                .unwrap_or_else(|e| Err(e.to_string()));
                cosmic::Action::App(Message::ThemeModeSet(result))
            }),
            Message::ThemeModeSet(result) => {
                if let Err(error) = result {
                    self.notifications
                        .service_error("Appearance could not be changed", &error);
                }
                Task::none()
            }
            Message::BeginNoteEdit(id) => {
                let mut tasks = vec![self.finish_note_edit()];

                if let Some(note) = self.config.desktop_widgets.notes.iter().find(|note| note.id == id) {
                    self.note_editor = Some(DesktopNoteEditor {
                        id,
                        content: cosmic::widget::text_editor::Content::with_text(&note.text),
                        revision: 0,
                    });
                }
                tasks.push(Task::none());
                Task::batch(tasks)
            }
            Message::FinishNoteEdit => self.finish_note_edit(),
            Message::NoteAction(id, action) => {
                if let Some(editor) = &mut self.note_editor
                    && editor.id == id
                {
                    let edited = action.is_edit();
                    editor.content.perform(action);

                    if edited {
                        editor.revision = editor.revision.wrapping_add(1);
                        let revision = editor.revision;
                        return cosmic::task::future(async move {
                            // iced executor is Tokio; do not spawn blocking sleep threads.
                            tokio::time::sleep(Duration::from_millis(500)).await;
                            Message::SaveNote(id, revision)
                        });
                    }
                }
                Task::none()
            }
            Message::SaveNote(id, revision) => {
                if let Some(editor) = &self.note_editor
                    && editor.id == id
                    && editor.revision == revision
                {
                    let value = editor.content.text();
                    self.set_note_text(&id, value)
                } else {
                    Task::none()
                }
            }
            Message::BeginNoteDrag(id, surface) => self.begin_note_drag(id, surface),
            Message::BeginClockDrag(surface) => self.begin_widget_drag(None, surface),
            Message::NotesStored(result) => {
                self.note_saving = false;
                match result {
                    Ok(()) => {
                        self.note_inflight.clear();
                        self.note_error = None;
                        self.flush_note_changes()
                    }
                    Err(error) => {
                        self.note_pending.splice(0..0, self.note_inflight.drain(..));
                        self.note_error = Some(error);
                        Task::none()
                    }
                }
            }
            Message::WallpaperLoaded(result) => {
                match result {
                    Ok(handle) => self.wallpaper = Some(handle),
                    Err(error) => eprintln!("ferese-shell: wallpaper unavailable: {error}"),
                }
                Task::none()
            }
            Message::NativeSurface(id, result) => {
                match result {
                    Ok((_connection, surface)) if self.outputs.iter().any(|entry| entry.bar == id) => {
                        self.attach_effects(id, &surface)
                    }
                    Ok((_connection, surface)) => {
                        self.attach_power_material(id, &surface);
                        if let Some(entry) = &mut self.notification_surface
                            && entry.id == id
                            && entry.effects.is_none()
                        {
                            match EffectsBinding::attach_role(&surface, None, 1.0) {
                                Ok(binding) => entry.effects = Some(binding),
                                Err(error) => eprintln!("ferese-shell: notification effects unavailable: {error}"),
                            }
                        }

                        if let Some(menu) = &mut self.menu
                            && menu.id == id
                        {
                            menu.prepare_surface(&surface);
                        }
                    }
                    Err(error) => eprintln!("ferese-shell: native surface unavailable: {error}"),
                }
                Task::none()
            }
            Message::StartRecording => {
                self.recorder.start();
                if self.recorder.busy() {
                    self.close_menu()
                } else {
                    Task::none()
                }
            }
            Message::StopRecording => {
                self.recorder.stop();
                Task::none()
            }
            Message::StatusUpdated(update) => {
                if update.generation >= self.status_service.generation {
                    self.status = update.snapshot;
                    self.status_error = update.error;
                }
                self.notifications.tick();

                if self.notifications.ready {
                    self.status.notifications = Some(status::Notifications {
                        count: self.notifications.unread(),
                        dnd: self.notifications.dnd,
                    });
                }
                self.sync_notification_surface()
            }
            Message::NotificationEvent(event) => {
                self.notifications.handle_event(event);
                if self.notifications.ready {
                    self.status.notifications = Some(status::Notifications {
                        count: self.notifications.unread(),
                        dnd: self.notifications.dnd,
                    });
                }
                self.sync_notification_surface()
            }
            Message::NotificationTick => {
                self.notifications.tick();
                self.sync_notification_surface()
            }
            Message::DismissNotification(id) => {
                self.notifications.close(id, 2);
                self.sync_notification_surface()
            }
            Message::RemoveNotification(id) => {
                self.notifications.dismiss(id);
                self.sync_notification_surface()
            }
            Message::InvokeNotification(id, action) => {
                self.notifications.invoke(id, action);
                self.sync_notification_surface()
            }
            Message::HoverNotification(id, hovered) => {
                self.notifications.hover(id, hovered);
                self.sync_notification_surface()
            }
            Message::ToggleNotificationHistory => self.toggle_notification_history(),
            Message::ToggleNotificationGroup(app) => {
                self.notifications.toggle_group(app);
                self.sync_notification_surface()
            }
            Message::RemoveNotificationGroup(app) => {
                self.notifications.dismiss_group(&app);
                self.sync_notification_surface()
            }
            Message::ClearNotifications => {
                self.notifications.clear();
                self.sync_notification_surface()
            }
            Message::AnimateMenu => self.animate_menu(),
            Message::OpenMenu(kind, anchor) => self.open_menu(kind, anchor),
            Message::OpenMenuOn(id, kind, anchor) => {
                if self.bar_surface_id != id {
                    let destroy = self.destroy_menu();
                    self.bar_surface_id = id;
                    let open = self.open_menu(kind, anchor);
                    Task::batch([destroy, open])
                } else {
                    self.open_menu(kind, anchor)
                }
            }
            Message::ShowGuide => {
                if self.guide_shown
                    || self.guide_load.loading
                    || self.guide_attempts >= 5
                    || !self.config.status.keybinding_guide
                    || self.outputs.is_empty()
                {
                    return Task::none();
                }
                self.guide_attempts += 1;
                self.load_guide(false, None)
            }
            Message::GuideLoaded {
                generation,
                manual,
                output,
                result,
            } => {
                if !self.guide_load.finish(generation) {
                    return Task::none();
                }
                match result {
                    Ok(entries)
                        if (manual || self.config.status.keybinding_guide)
                            && !self.outputs.is_empty()
                            && self.system_modal.is_none()
                            && self.pending_power.is_none() =>
                    {
                        self.guide_shown = true;
                        self.open_guide_on(entries, output.as_deref())
                    }
                    result => {
                        if let Err(error) = result {
                            eprintln!("ferese-shell: keybinding guide unavailable: {error}");
                        }
                        if !manual
                            && self.guide_attempts < 5
                            && self.config.status.keybinding_guide
                            && !self.guide_shown
                        {
                            Task::perform(
                                async {
                                    tokio::time::sleep(Duration::from_secs(2)).await;
                                },
                                |_| cosmic::Action::App(Message::ShowGuide),
                            )
                        } else {
                            Task::none()
                        }
                    }
                }
            }
            Message::SystemInhibitors(id, result) => self.set_system_inhibitors(id, result),
            Message::ConfirmPower(action) => {
                if let Some(action) = system_modal::PowerAction::from_status(action) {
                    return self.open_system_modal(action, None);
                }
                Task::none()
            }
            Message::OpenDisplays(output) => self.open_display_modal(output.as_deref()),
            Message::DisplaysLoaded(generation, result) => self.finish_display_inventory(generation, result),
            Message::SelectDisplayMode(mode) => self.select_display_mode(mode, true),
            Message::ApplyDisplayMode => self.apply_display_mode(),
            Message::DisplayModeCompleted(serial, close, result) => self.finish_display_mode(serial, close, result),
            Message::KeepDisplayMode => self.confirm_display_mode(true),
            Message::RevertDisplayMode => self.confirm_display_mode(false),
            Message::ExecutePower => self.execute_system_modal(),
            Message::AnimatePower => self.animate_system_modal(),
            Message::PowerCompleted(id, result) => self.finish_power_action(id, result),
            Message::CalendarMonth(delta) => {
                self.calendar_offset = (self.calendar_offset + delta).clamp(-1200, 1200);
                Task::none()
            }
            Message::CalendarToday => {
                self.calendar_offset = 0;
                Task::none()
            }
            Message::CancelPower => self.close_system_modal(),
            Message::Control(action) => {
                if let Some(action) = system_modal::PowerAction::from_status(action.clone()) {
                    return self.open_system_modal(action, None);
                }

                if let status::Action::PowerProfile(profile) = action {
                    self.status_error = self.status_service.send(status::Action::PowerProfile(profile)).err();

                    if self.status_error.is_none()
                        && let Some(profiles) = &mut self.status.power_profiles
                    {
                        profiles.active = profile.to_owned();
                    }
                    return Task::none();
                }
                if self.notifications.ready {
                    match action {
                        status::Action::Dnd(value) => {
                            self.notifications.dnd = value;
                            self.status.notifications = Some(status::Notifications {
                                count: self.notifications.unread(),
                                dnd: value,
                            });
                            return Task::none();
                        }
                        status::Action::Notifications => {
                            return Task::batch([self.close_menu(), self.toggle_notification_history()]);
                        }
                        _ => {}
                    }
                }

                self.status_error = self.status_service.send(action.clone()).err();

                if self.status_error.is_none() {
                    self.optimistic_status(&action);
                }

                if matches!(action, status::Action::Notifications | status::Action::Settings) {
                    return self.close_menu();
                }

                Task::none()
            }
            Message::Event(event, id) => self.handle_event(event, id),
            Message::ClockChanged(labels) => {
                self.clock = labels.bar;
                self.desktop_clock = labels.desktop;
                Task::none()
            }
            Message::RecorderEvent(event) => {
                self.recorder.handle(event);
                Task::none()
            }
            Message::RecorderElapsed => Task::none(),
            Message::ControlReady => {
                let mut reload_task = Task::none();
                if let Some(control) = &self.control {
                    let poll = control.poll();

                    if poll.disconnected {
                        return cosmic::iced::exit();
                    }

                    if let Some(active) = poll.overview_active {
                        self.overview_active = active;
                    }

                    if let Some(source) = poll.config {
                        reload_task = self.reload_config(source);
                    }

                    if let Some(snapshot) = poll.snapshot {
                        self.snapshot = snapshot;
                        let mut tasks = vec![reload_task];

                        for entry in &mut self.outputs {
                            let output = self
                                .snapshot
                                .outputs
                                .iter()
                                .find(|output| Some(output.name.as_str()) == entry.name.as_deref())
                                .map(|output| output.id);
                            let hidden = output_bar_hidden(&self.snapshot, output);

                            if entry.hidden == hidden {
                                continue;
                            }

                            entry.hidden = hidden;
                            if let Some(effects) = &entry.effects
                                && let Err(error) = effects.set_visible(!hidden)
                            {
                                eprintln!("ferese-shell: could not update panel material: {error}");
                            }
                            tasks.push(set_input_zone(entry.bar, if hidden { Some(Vec::new()) } else { None }));
                        }
                        if self.bar_hidden(self.bar_surface_id) {
                            tasks.push(self.destroy_menu());
                        }
                        reload_task = Task::batch(tasks);
                    }
                    for command in poll.commands {
                        let task = match command {
                            control::ControlCommand::ToggleDisplays(output) => self.open_display_modal(Some(&output)),
                            control::ControlCommand::MonitorsChanged => self.load_display_inventory(),
                            control::ControlCommand::ToggleGuide(output) => self.toggle_guide(output),
                            control::ControlCommand::CancelLogout(serial) => self.cancel_logout_modal(serial),
                            control::ControlCommand::Logout(serial, output) => {
                                self.open_system_modal(system_modal::PowerAction::Logout(serial), Some(&output))
                            }
                        };
                        reload_task = Task::batch([reload_task, task]);
                    }
                }
                reload_task
            }
            Message::ActivateWorkspace(id) => {
                if let Some(control) = &self.control {
                    control.activate_workspace(id);
                }
                Task::none()
            }
            Message::ToggleOverview => {
                self.overview_active = !self.overview_active;
                if let Some(control) = &self.control {
                    control.set_overview_active(self.overview_active);
                }
                Task::none()
            }
        }
    }

    fn style(&self) -> Option<cosmic::iced::theme::Style> {
        // Shell surfaces own their backgrounds. Never inherit an opaque
        // application clear color during startup or a system-theme change.
        Some(shell_surface_style(self.config.theme))
    }

    fn view(&self) -> Element<'_, Self::Message> {
        text("").into()
    }

    fn view_window(&self, _id: window::Id) -> Element<'_, Self::Message> {
        text("").into()
    }
}

impl FereseShell {
    fn reload_config(&mut self, source: String) -> Task<Message> {
        let config = match config::parse_source(&source) {
            Ok(config) => config,
            Err(error) => {
                eprintln!("ferese-shell: reload rejected; keeping current config: {error}");
                return Task::none();
            }
        };

        self.apply_config(config)
    }

    fn apply_config(&mut self, mut config: config::ShellConfig) -> Task<Message> {
        if std::env::var_os("FERESE_COMPOSITOR_WALLPAPER").is_some() {
            config.wallpaper.path = None;
        }

        if self.config.font_family != config.font_family {
            *SHELL_FONT
                .get_or_init(|| std::sync::RwLock::new(cosmic::font::default()))
                .write()
                .unwrap() = configured_font(config.font_family.as_deref());
        }
        motion::configure(config.animations, config.theme.material_radius);

        if self.config.animations != config.animations {
            self.notifications.update_motion_settings(config.animations);
            if let Some(menu) = &mut self.menu {
                menu.motion.update_settings(config.animations);
            }
            if let Some(modal) = &mut self.system_modal {
                modal.motion.update_settings(config.animations);
            }
        }
        self.status_service
            .update_settings(config.status.settings_command.clone());

        let old = self.config.theme;
        let old_clock = &self.config.desktop_widgets.clock;
        let old_notes = &self.config.desktop_widgets.notes;
        let new_notes = &config.desktop_widgets.notes;
        let notes_changed = old_notes.len() != new_notes.len()
            || old_notes.iter().zip(new_notes).any(|(old, new)| !old.same_surface(new));
        let new_clock = &config.desktop_widgets.clock;
        let clock_changed = old_clock.enabled != new_clock.enabled
            || old_clock.outputs != new_clock.outputs
            || old_clock.anchor != new_clock.anchor
            || old_clock.width != new_clock.width
            || old_clock.height != new_clock.height
            || old_clock.margin_x != new_clock.margin_x
            || old_clock.margin_y != new_clock.margin_y;
        let theme = config.theme;
        let geometry_changed = old.bar_height != theme.bar_height
            || old.bar_margin_top != theme.bar_margin_top
            || old.bar_margin_horizontal != theme.bar_margin_horizontal
            || old.bar_window_gap != theme.bar_window_gap;
        self.notifications.configure(config.notifications.clone());
        self.clock_service.configure(&config.desktop_widgets.clock);
        self.config = config;
        self.desktop_clock = self
            .config
            .desktop_widgets
            .clock
            .labels(&Zoned::now())
            .unwrap_or_default();
        let mut tasks = vec![if clock_changed {
            self.rebuild_clocks(true)
        } else {
            Task::none()
        }];

        if !self.guide_load.manual
            && !self.config.status.keybinding_guide
            && self.system_modal.as_ref().is_some_and(|modal| modal.is_guide())
        {
            tasks.push(self.destroy_system_modal(false));
        }

        if old.palette() != theme.palette() {
            tasks.push(cosmic::command::set_theme(theme.palette().native_theme()));
        }

        if clock_changed && self.note_drag.as_ref().is_some_and(|drag| drag.id.is_none()) {
            tasks.push(self.finish_note_drag(false));
        }

        if notes_changed {
            tasks.push(self.finish_note_drag(false));
            if self.note_editor.as_ref().is_some_and(|editor| {
                !self
                    .config
                    .desktop_widgets
                    .notes
                    .iter()
                    .any(|note| note.id == editor.id && note.interactive)
            }) {
                self.note_editor = None;
            }
            tasks.push(self.rebuild_notes(true));
        }

        for entry in self.outputs.iter().filter(|_| geometry_changed) {
            tasks.push(set_size(entry.bar, None, Some(theme.bar_height.round() as u32)));
            tasks.push(set_margin(
                entry.bar,
                theme.bar_margin_top,
                theme.bar_margin_horizontal,
                0,
                theme.bar_margin_horizontal,
            ));
            tasks.push(set_exclusive_zone(
                entry.bar,
                (theme.bar_height.round() as i32).saturating_add(theme.bar_window_gap),
            ));
        }

        Task::batch(tasks)
    }
}

fn shell_surface_style(theme: ShellTheme) -> cosmic::iced::theme::Style {
    theme.palette().application_style(Color::TRANSPARENT)
}

fn color([red, green, blue, alpha]: [u8; 4]) -> Color {
    Color::from_rgba8(red, green, blue, f32::from(alpha) / 255.0)
}

fn color_with_opacity(mut value: [u8; 4], opacity: f32) -> Color {
    value[3] = (f32::from(value[3]) * opacity).round() as u8;
    color(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control::{OutputSnapshot, WindowSnapshot};
    #[test]
    fn widget_drag_stops_at_edges_without_tunnelling_and_slides() {
        use cosmic::iced::{Point, Rectangle};
        let obstacles = [Rectangle {
            x: 100.,
            y: 0.,
            width: 40.,
            height: 100.,
        }];
        assert_eq!(
            super::avoid_widget_overlap(Point::new(0., 0.), Point::new(300., 0.), (20, 20), &obstacles),
            Point::new(80., 0.)
        );
        assert_eq!(
            super::avoid_widget_overlap(Point::new(80., 0.), Point::new(120., 50.), (20, 20), &obstacles),
            Point::new(80., 50.)
        );
        assert_eq!(
            super::avoid_widget_overlap(Point::new(160., 0.), Point::new(0., 0.), (20, 20), &obstacles),
            Point::new(140., 0.)
        );
        assert_eq!(
            super::avoid_widget_overlap(Point::new(100., 120.), Point::new(100., 0.), (20, 20), &obstacles),
            Point::new(100., 100.)
        );
        assert_eq!(
            super::avoid_widget_overlap(Point::new(0., 100.), Point::new(200., 100.), (20, 20), &obstacles),
            Point::new(200., 100.)
        );
    }

    #[test]
    fn note_drag_origin_and_bounds_use_logical_output_coordinates() {
        use cosmic::iced::Point;
        let mut note = ferese_config::desktop::StickyNote::default();
        assert_eq!(super::note_origin(&note, (1920, 1080)), Point::new(1552., 80.));
        note.anchor = ferese_config::desktop::Anchor::Center;
        assert_eq!(super::note_origin(&note, (1920, 1080)), Point::new(800., 420.));
        assert_eq!(
            super::clamp_note_position(Point::new(-20., 1200.), (1920, 1080), (320, 240)),
            Point::new(0., 840.)
        );
        assert_eq!(
            super::clamp_note_position(Point::new(20., 30.), (200, 100), (320, 240)),
            Point::ORIGIN
        );
    }

    #[test]
    fn desktop_clock_anchor_margins_match_only_anchored_edges() {
        use ferese_config::desktop::{Anchor as Position, Clock};
        let clock = Clock {
            anchor: Position::BottomRight,
            margin_x: 30,
            margin_y: 40,
            ..Clock::default()
        };
        let (anchor, margin) = super::clock_placement(&clock);
        assert_eq!(anchor, super::Anchor::BOTTOM | super::Anchor::RIGHT);
        assert_eq!((margin.top, margin.right, margin.bottom, margin.left), (0, 30, 40, 0));
        let (anchor, margin) = super::clock_placement(&Clock {
            anchor: Position::Center,
            ..clock
        });
        assert!(anchor.is_empty());
        assert_eq!((margin.top, margin.right, margin.bottom, margin.left), (0, 0, 0, 0));
    }

    #[test]
    fn shell_font_respects_configured_family() {
        assert_eq!(
            configured_font(Some("JetBrainsMono Nerd Font")),
            cosmic::font::Font::with_name("JetBrainsMono Nerd Font")
        );
        assert_eq!(configured_font(None), cosmic::font::default());
    }

    #[test]
    fn bar_decoration_does_not_follow_transitional_text_contrast() {
        let theme = config::ShellTheme::default();
        let contrasted = config::ShellTheme {
            text_primary: [0, 0, 0, 255],
            text_muted: [255, 255, 255, 255],
            ..theme
        };
        let before = bar_group_style(theme);
        let after = bar_group_style(contrasted);
        assert_eq!(before.border, after.border);
        assert_eq!(before.background, after.background);
        assert_eq!(before.border.color, color(theme.border));
        assert_eq!(
            workspace_selector_style(false, false, true, theme).background,
            workspace_selector_style(false, false, true, contrasted).background,
        );
    }

    #[test]
    fn workspace_selector_has_distinct_active_paint() {
        let theme = ShellTheme::default();
        let active = workspace_selector_style(true, false, false, theme);
        let occupied = workspace_selector_style(false, false, true, theme);
        let inactive = workspace_selector_style(false, false, false, theme);
        let remote = workspace_selector_style(false, true, false, theme);
        assert!(active.background.is_some());
        assert_eq!(
            active.background,
            Some(Background::Color(color_with_opacity(theme.accent, 0.16)))
        );
        assert!(occupied.background.is_some());
        assert!(inactive.background.is_none());
        assert_eq!(inactive.border.width, 0.0);
        assert_eq!(remote.border.width, 1.0);
        assert!(remote.background.is_none());
    }

    #[test]
    fn workspace_highlight_tracks_the_bars_output() {
        let output = control::OutputSnapshot {
            id: 1,
            name: "eDP-1".into(),
            active_workspace: 2,
            focused: false,
        };
        assert!(workspace_active_on_bar(2, Some(&output)));
        assert!(!workspace_active_on_bar(1, Some(&output)));
        assert!(!workspace_active_on_bar(2, None));
    }

    #[test]
    fn startup_surface_clear_is_transparent_and_bar_fallback_is_dark() {
        let theme = ShellTheme::default();
        assert_eq!(shell_surface_style(theme).background_color, Color::TRANSPARENT);
        let Some(Background::Color(background)) = bar_style(theme.for_bar(), false).background else {
            panic!("first frame needs a fallback fill before material attachment");
        };
        assert!(background.r < 0.15 && background.g < 0.15 && background.b < 0.20);
        assert!(background.a > 0.9);
        assert!(theme.bar_text_primary[..3].iter().all(|channel| *channel > 220));
    }

    #[test]
    fn bar_fallback_uses_the_matching_dark_palette_and_geometry() {
        let theme = ShellTheme::default().for_bar();
        let style = bar_style(theme, false);
        assert_eq!(style.background, Some(Background::Color(color(theme.bar_background))));
        assert_eq!(style.text_color, Some(color(theme.bar_text_primary)));
        assert_eq!(style.border.radius, theme.bar_radius.into());
        assert_eq!(style.shadow, cosmic::iced::Shadow::default());
    }

    #[test]
    fn bar_does_not_cover_the_selected_compositor_material() {
        let theme = ShellTheme::default().for_bar();
        let style = bar_style(theme, true);
        assert_eq!(style.background, None);
        assert_eq!(style.text_color, Some(color(theme.bar_text_primary)));
        assert_eq!(style.border.radius, theme.bar_radius.into());
        assert_eq!(style.shadow, cosmic::iced::Shadow::default());
    }

    #[test]
    fn compact_bar_preserves_logical_text_and_icon_sizes() {
        for height in [24.0, 26.0, 32.0, 38.0, 44.0] {
            let metrics = BarMetrics::from(ShellTheme {
                bar_height: height,
                ..ShellTheme::default()
            });
            assert_eq!(metrics.text_size, 14);
            assert_eq!(metrics.icon_size, 20);
            assert_eq!(metrics.overview_icon_size, 20);
            assert!(metrics.control_height >= f32::from(metrics.icon_size));
            assert!(metrics.control_height <= height);
        }
    }

    #[test]
    fn fullscreen_on_the_active_workspace_hides_the_bar() {
        let snapshot = snapshot_with_fullscreen_window(7);

        assert!(bar_hidden(&snapshot));
    }

    #[test]
    fn fullscreen_on_an_inactive_workspace_keeps_the_bar_visible() {
        let snapshot = snapshot_with_fullscreen_window(8);

        assert!(!bar_hidden(&snapshot));
    }

    #[test]
    fn fullscreen_visibility_is_local_to_each_monitor() {
        let mut snapshot = snapshot_with_fullscreen_window(7);
        snapshot.outputs.push(OutputSnapshot {
            id: 2,
            name: "external-test".to_owned(),
            active_workspace: 9,
            focused: false,
        });
        assert!(output_bar_hidden(&snapshot, Some(1)));
        assert!(!output_bar_hidden(&snapshot, Some(2)));
        assert!(!output_bar_hidden(&snapshot, None));
        snapshot.outputs[0].focused = false;
        snapshot.outputs[1].focused = true;
        assert!(output_bar_hidden(&snapshot, Some(1)));
        assert!(!output_bar_hidden(&snapshot, Some(2)));
    }

    #[test]
    fn bar_title_tracks_focus_and_the_outputs_active_workspace() {
        let mut snapshot = snapshot_with_fullscreen_window(7);
        assert_eq!(focused_bar_title(&snapshot, snapshot.outputs.first()), "Test");
        snapshot.windows[0].focused = false;
        assert_eq!(focused_bar_title(&snapshot, snapshot.outputs.first()), "");
        snapshot.windows[0].focused = true;
        snapshot.windows[0].workspace = 8;
        assert_eq!(focused_bar_title(&snapshot, snapshot.outputs.first()), "");
        snapshot.windows[0].workspace = 7;
        snapshot.windows[0].title.clear();
        assert_eq!(
            focused_bar_title(&snapshot, snapshot.outputs.first()),
            "dev.ferese.Test"
        );
        assert_eq!(focused_bar_title(&snapshot, None), "");
    }

    fn snapshot_with_fullscreen_window(workspace: u64) -> ShellSnapshot {
        ShellSnapshot {
            outputs: vec![OutputSnapshot {
                id: 1,
                name: "eDP-1".to_owned(),
                active_workspace: 7,
                focused: true,
            }],
            workspaces: Vec::new(),
            windows: vec![WindowSnapshot {
                workspace,
                app_id: "dev.ferese.Test".to_owned(),
                title: "Test".to_owned(),
                focused: true,
                fullscreen: true,
            }],
        }
    }
}
