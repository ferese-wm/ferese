mod activities;
mod bar;
mod battery_indicator;
mod surfaces;

use bar::{bar_content, output_bar_hidden};
use surfaces::{EffectsBinding, EffectsContext, native_wayland_surface};

mod capture_status;
mod clock;
mod compositor_ipc;
mod config;
mod control;
mod display_mode;
mod ipc_subscription;
mod keybinding_guide;
mod launcher;
mod media;
mod motion;
mod notification_ui;
mod notifications;
mod panel;
mod panel_hub;
mod panel_layout;
mod panel_runtime;
mod recording;
mod renderer;
mod rendering;
mod status;
mod status_ui;
mod system_modal;
mod system_stats;
mod workspace_ui;

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use cosmic::app::{Core, Settings, Task};
use cosmic::iced::event::{PlatformSpecific, wayland};
use cosmic::iced::platform_specific::runtime::wayland::layer_surface::{
    IcedMargin, IcedOutput, SctkLayerSurfaceSettings,
};
use cosmic::iced::platform_specific::shell::commands::layer_surface::{
    Anchor, KeyboardInteractivity, Layer, destroy_layer_surface, set_anchor, set_exclusive_zone,
    set_input_zone, set_margin, set_size,
};
use cosmic::iced::{
    Background, Border, Color, ContentFit, Event, Length, Limits, Subscription, alignment, event,
    window,
};
use cosmic::widget::{button, container, icon, image, row};
use cosmic::{Element, theme};
use ferese_protocols::effects::v1::client::ferese_effects_manager_v1::FereseEffectsManagerV1;
use ferese_protocols::effects::v1::client::ferese_surface_effects_v1;
use ferese_protocols::effects::v1::client::ferese_surface_effects_v1::FereseSurfaceEffectsV1;
use ferese_theme::icons::{accented as accented_icon, tinted as bar_icon};
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
fn configured_font(family: Option<&str>) -> cosmic::font::Font {
    ferese_theme::font(family)
}

#[derive(Clone, Copy)]
struct BarMetrics {
    height: f32,
    icon_size: u16,
    icon_slot_width: u16,
    control_height: f32,
    group_item_height: f32,
}

impl From<panel::PanelGeometry> for BarMetrics {
    fn from(geometry: panel::PanelGeometry) -> Self {
        let height = geometry.height;
        let control_height = (height - 4.0).max(21.0).min(height);
        // Leave two logical pixels inside the group border on each side,
        // including compact bars.
        let group_height = control_height.max(24.0).min(height);

        Self {
            height,
            // Logical sizes: Wayland/iced applies each surface's output scale.
            // Compacting the bar must not also shrink its readable content.
            icon_size: ferese_theme::panel::ICON_SIZE,
            icon_slot_width: ferese_theme::panel::ICON_SLOT_WIDTH,
            control_height,
            group_item_height: (group_height - 4.0).max(0.0),
        }
    }
}

fn main() -> cosmic::iced::Result {
    renderer::configure_shell();
    let theme_snapshot = ferese_theme_client::service::current();
    let (mut config, startup_error) = match config::load(&theme_snapshot) {
        Ok(config) => (config, None),
        Err(error) => {
            eprintln!("ferese-shell: {error}; starting with defaults");
            let notice = format!(
                "The shell started with default settings. Your configuration file has not been changed.\n\n{error}"
            );
            (ShellConfig::default(), Some(notice))
        }
    };
    let compositor_wallpaper = std::env::var_os("FERESE_COMPOSITOR_WALLPAPER").is_some();
    if compositor_wallpaper {
        // The compositor owns one GPU image, shared across output renderers;
        // do not decode another copy or allocate full-screen software buffers.
        config.wallpaper.path = None;
    }
    let shell_font = configured_font(config.font_family.as_deref());
    // Decode alongside toolkit/GPU initialization, never during a UI draw.
    let wallpaper = config.wallpaper.path.clone().map(|path| {
        let (sender, receiver) = cosmic::iced::futures::channel::oneshot::channel();
        std::thread::spawn(move || {
            let result =
                cosmic::iced::advanced::graphics::image::load(&image::Handle::from_path(path))
                    .map(|pixels| {
                        image::Handle::from_rgba(pixels.width(), pixels.height(), pixels.into_raw())
                    })
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

    cosmic::app::run::<FereseShell>(settings, (config, wallpaper, startup_error, theme_snapshot))
}

type WallpaperLoad =
    cosmic::iced::futures::channel::oneshot::Receiver<Result<image::Handle, String>>;

struct FereseShell {
    core: Core,
    config: ShellConfig,
    theme_snapshot: ferese_ipc::theme::Snapshot,
    render: rendering::Rendering,
    wallpaper: Option<image::Handle>,
    control: Option<ShellControl>,
    snapshot: ShellSnapshot,
    workspace_ui: workspace_ui::WorkspaceUi,
    overview_active: bool,
    clock: String,
    outputs: Vec<OutputSurfaces>,
    effects_context: Option<std::rc::Rc<EffectsContext>>,
    notifications: notifications::Center,
    notification_surface: Option<notification_ui::NotificationSurface>,
    status_service: status::Service,
    status: status::Snapshot,
    system_stats: system_stats::Snapshot,
    connections: status_ui::connections::State,
    recorder: recording::Recorder,
    capture_status: capture_status::Model,
    media: media::Model,
    activities: activities::Model,
    calendar_offset: i32,
    status_error: Option<status::ActionError>,
    night_light_temperature: Option<u16>,
    theme_error: Option<String>,
    pending_config_error: Option<String>,
    menu: Option<status_ui::OpenMenu>,
    closing_effects: std::collections::HashMap<window::Id, EffectsBinding>,
    system_modal: Option<system_modal::SystemModal>,
    launcher: Option<launcher::Launcher>,
    launcher_files: launcher::files::Files,
    display_mode: display_mode::Model,
    guide_shown: bool,
    guide_load: keybinding_guide::LoadState,
    guide_attempts: u8,
    pending_power: Option<(window::Id, system_modal::PowerAction)>,
}

struct OutputSurfaces {
    runtime: panel_runtime::Runtime,
    scale: f64,
    output: wl_output::WlOutput,
    name: Option<String>,
    bar: window::Id,
    wallpaper: Option<window::Id>,
    size: Option<(i32, i32)>,
    effects: Option<EffectsBinding>,
    bar_regions: Vec<[f32; 5]>,
    panel_resolution: Option<panel_layout::Resolution>,
    bar_margin_horizontal: i32,
    hidden: bool,
}

#[derive(Clone, Debug)]
enum Message {
    Launcher(launcher::Message),
    LauncherEnter(window::Id, bool),
    NightLightTemperature(u16),
    CommitNightLightTemperature,
    HubOpen(window::Id, status_ui::Menu),
    HubClose(window::Id),
    HubWorkspaceScroll(window::Id, cosmic::iced::mouse::ScrollDelta),
    HubOverview(window::Id),
    HubWorkspaceHover(window::Id, Option<u64>),
    HubWorkspaceSelect(window::Id, u64),
    HubWorkspaceMove(window::Id, u64, u64),
    HubWorkspaceFocus(
        window::Id,
        Option<u64>,
        Option<cosmic::iced::keyboard::key::Named>,
    ),
    HubToastExpired(window::Id, Instant),
    HubWorkspaceFeedbackExpired(window::Id, Instant),
    HubIgnore,
    PanelScale(window::Id, f32),
    AdaptiveDeadline(window::Id, u64),
    PanelSurfaceChanged(window::Id, Box<ferese_panel::scene::Scene>),
    PanelResolved(window::Id, panel_layout::Resolution),
    ThemeChanged(Box<ferese_ipc::theme::Snapshot>),
    ThemeMode(ferese_config::theme::Mode),
    ThemeModeSet(Result<(), String>),
    WallpaperLoaded(Result<image::Handle, String>),
    Event(Event, window::Id),
    NativeSurface(
        window::Id,
        Result<(Connection, wl_surface::WlSurface), String>,
    ),
    ClockChanged(String),
    RecorderEvent(recording::Event),
    CaptureStatus(capture_status::Event),
    RecorderElapsed,
    MediaUpdated(std::sync::Arc<ferese_ipc::media::Snapshot>),
    MediaAction(serde_json::Value),
    MediaCompleted(Result<(), String>),
    MediaTick,
    ActivitiesUpdated(std::sync::Arc<ferese_ipc::activities::Snapshot>),
    ActivityTick,
    Pomodoro(
        ferese_ipc::activities::PomodoroAction,
        Option<ferese_activities::Id>,
    ),
    PomodoroResult(Result<(), String>),
    ActivityHover(window::Id, bool),
    ActivityActivate(window::Id, ferese_activities::Id),
    ActivityNavigation(
        window::Id,
        Option<ferese_activities::Id>,
        Option<cosmic::iced::keyboard::key::Named>,
    ),
    MediaChoose,
    MediaSeek(u64),
    MediaSeekCommit,
    MediaArt(media::Artwork),
    ControlReady,
    ActivateWorkspace(u64),
    HoverWorkspace(window::Id, u64, cosmic::iced::advanced::widget::Id, bool),
    WorkspaceTooltipDelay(u64),
    WorkspaceTooltipBounds(u64, Option<cosmic::iced::Rectangle>),
    WorkspaceIconsLoaded(Result<workspace_ui::IconIndex, String>),
    ToggleOverview,
    StatusUpdated(status::Update),
    SystemStats(system_stats::Snapshot),
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
    PanelAction(Box<Message>),
    OpenMenu(status_ui::Menu, cosmic::iced::Rectangle<i32>),
    OpenPopover(status_ui::Menu, status_ui::PopoverAnchor),
    Control(status::Action),
    Connections(status_ui::connections::Event),
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
    type Flags = (
        ShellConfig,
        Option<WallpaperLoad>,
        Option<String>,
        ferese_ipc::theme::Snapshot,
    );
    type Message = Message;

    const APP_ID: &'static str = APP_ID;

    fn core(&self) -> &Core {
        &self.core
    }

    fn core_mut(&mut self) -> &mut Core {
        &mut self.core
    }

    fn init(
        core: Core,
        (config, wallpaper, startup_error, theme_snapshot): Self::Flags,
    ) -> (Self, Task<Self::Message>) {
        let control = ShellControl::connect()
            .map_err(|error| eprintln!("ferese-shell: shell control unavailable: {error}"))
            .ok();
        // The compositor reports the complete configuration error through its
        // accepted-theme snapshot. Use the local warning only when standalone.
        let pending_config_error = if control.is_none() {
            startup_error
        } else {
            None
        };
        let mut notification_center =
            notifications::Center::new(config.notifications.clone(), config.animations);
        notification_center.activity_bridge = config.activities.sources.notification_bridge;
        let app = Self {
            core,
            theme_snapshot,
            notifications: notification_center,
            notification_surface: None,
            status_service: status::Service::start(config.status.settings_command.clone()),
            status: status::Snapshot::default(),
            system_stats: Default::default(),
            connections: Default::default(),
            recorder: recording::Recorder::default(),
            capture_status: capture_status::Model::new(),
            media: media::Model::default(),
            activities: activities::Model::default(),
            calendar_offset: 0,
            status_error: None,
            night_light_temperature: None,
            theme_error: None,
            pending_config_error,
            menu: None,
            closing_effects: Default::default(),
            system_modal: None,
            launcher: None,
            launcher_files: Default::default(),
            display_mode: display_mode::Model::default(),
            guide_shown: false,
            guide_load: Default::default(),
            guide_attempts: 0,
            pending_power: None,
            render: rendering::Rendering::new(&config),
            clock: clock::current_time_for(
                config.panels[0].presentation == ferese_panel::Presentation::Text,
            ),
            config,
            wallpaper: None,
            control,
            snapshot: ShellSnapshot::default(),
            workspace_ui: Default::default(),
            overview_active: false,
            outputs: Vec::new(),
            effects_context: None,
        };
        let wallpaper_task = match wallpaper {
            Some(receiver) => cosmic::task::future(async move {
                cosmic::Action::App(Message::WallpaperLoaded(
                    receiver
                        .await
                        .unwrap_or_else(|error| Err(error.to_string())),
                ))
            }),
            None => Task::none(),
        };

        (app, wallpaper_task)
    }

    fn subscription(&self) -> Subscription<Self::Message> {
        Subscription::batch([
            self.launcher_subscription(),
            self.control
                .as_ref()
                .map_or_else(Subscription::none, |control| {
                    control.subscription().map(|_| Message::ControlReady)
                }),
            ferese_theme_client::service::subscription()
                .map(|snapshot| Message::ThemeChanged(Box::new(snapshot))),
            self.notifications
                .subscription()
                .map(Message::NotificationEvent),
            self.notifications
                .tick_subscription()
                .map(|_| Message::NotificationTick),
            event::listen_with(|event, status, id| match &event {
                Event::Keyboard(cosmic::iced::keyboard::Event::KeyPressed {
                    key:
                        cosmic::iced::keyboard::Key::Named(cosmic::iced::keyboard::key::Named::Enter),
                    modifiers,
                    ..
                }) if status == cosmic::iced::event::Status::Captured => {
                    Some(Message::LauncherEnter(id, modifiers.alt()))
                }
                Event::PlatformSpecific(PlatformSpecific::Wayland(wayland::Event::Frame(..))) => {
                    EFFECT_FRAME_PENDING
                        .load(Ordering::Relaxed)
                        .then_some(Message::Event(event, id))
                }
                Event::Keyboard(_)
                | Event::Mouse(
                    cosmic::iced::mouse::Event::ButtonPressed(_)
                    | cosmic::iced::mouse::Event::CursorLeft
                    | cosmic::iced::mouse::Event::CursorEntered,
                )
                | Event::Window(
                    window::Event::Opened { .. }
                    | window::Event::Closed
                    | window::Event::Rescaled(_),
                )
                | Event::PlatformSpecific(PlatformSpecific::Wayland(
                    wayland::Event::Popup(..)
                    | wayland::Event::Layer(..)
                    | wayland::Event::Output(..),
                )) => Some(Message::Event(event, id)),
                _ => None,
            }),
            clock::subscription(
                self.outputs.iter().any(|output| !output.hidden),
                self.config.panels[0].presentation == ferese_panel::Presentation::Text,
            )
            .map(Message::ClockChanged),
            activities::subscription::subscription().map(Message::ActivitiesUpdated),
            self.activity_tick_subscription()
                .map(|_| Message::ActivityTick),
            self.media.subscription().map(Message::MediaUpdated),
            self.media_tick_subscription().map(|_| Message::MediaTick),
            self.media.art.subscription().map(Message::MediaArt),
            self.recorder.subscription().map(Message::RecorderEvent),
            if self.capture_item_enabled() {
                self.capture_status
                    .subscription()
                    .map(Message::CaptureStatus)
            } else {
                Subscription::none()
            },
            self.capture_status
                .tick_subscription(self.capture_timer_visible())
                .map(|_| Message::RecorderElapsed),
            self.recorder
                .elapsed_subscription()
                .map(|_| Message::RecorderElapsed),
            system_stats::subscription(self.outputs.iter().any(|output| {
                !output.hidden
                    && self.panel_for_bar(output.bar).presentation
                        == ferese_panel::Presentation::Text
            }))
            .map(Message::SystemStats),
            self.status_service
                .subscription()
                .map(Message::StatusUpdated),
        ])
    }

    fn update(&mut self, message: Self::Message) -> Task<Self::Message> {
        let task = match message {
            Message::Connections(event) => self.update_connections(event),
            Message::Launcher(message) => self.update_launcher(message),
            Message::LauncherEnter(id, alt) => {
                if self.launcher.as_ref().is_some_and(|l| l.id == id) {
                    self.update_launcher(launcher::Message::Accept(alt))
                } else {
                    Task::none()
                }
            }
            Message::HubOpen(id, kind) => self.open_hub(id, kind),
            Message::HubClose(id) => self.close_hub(id),
            Message::HubWorkspaceScroll(id, delta) => self.scroll_hub_workspaces(id, delta),
            Message::HubOverview(id) => self.open_hub_overview(id),
            Message::HubWorkspaceHover(id, workspace) => self.hub_workspace_hover(id, workspace),
            Message::HubWorkspaceSelect(id, workspace) => self.hub_workspace_select(id, workspace),
            Message::HubWorkspaceMove(id, window, workspace) => {
                self.hub_workspace_move(id, window, workspace)
            }
            Message::HubWorkspaceFocus(id, focused, key) => {
                self.hub_workspace_focus(id, focused, key)
            }
            Message::HubToastExpired(id, revision) => self.hub_toast_expired(id, revision),
            Message::HubWorkspaceFeedbackExpired(id, revision) => {
                self.hub_workspace_feedback_expired(id, revision)
            }
            Message::HubIgnore => Task::none(),
            Message::PanelResolved(id, resolution) => {
                let allocation_changed = self
                    .outputs
                    .iter()
                    .find(|output| output.bar == id)
                    .is_some_and(|output| {
                        output
                            .panel_resolution
                            .as_ref()
                            .is_some_and(|previous| previous != &resolution)
                    });
                let close_menu = self.menu.as_ref().is_some_and(|menu| {
                    menu.anchor.parent == id
                        && menu.anchor.item.is_some()
                        && ((menu.kind == status_ui::Menu::Overflow
                            && resolution.overflow.is_empty())
                            || allocation_changed)
                });
                if let Some(output) = self.outputs.iter_mut().find(|output| output.bar == id) {
                    output.panel_resolution = Some(resolution);
                }
                let margin = self.update_panel_margin(id, false);
                let tooltip = if allocation_changed {
                    self.dismiss_workspace_tooltip_for_bar(id)
                } else {
                    Task::none()
                };
                if close_menu {
                    Task::batch([margin, tooltip, self.destroy_menu()])
                } else {
                    Task::batch([margin, tooltip])
                }
            }
            Message::PanelSurfaceChanged(id, scene) => {
                let regions = scene
                    .input_rectangles(1.)
                    .into_iter()
                    .map(|r| [r[0] as f32, r[1] as f32, r[2] as f32, r[3] as f32, 0.])
                    .collect();
                let Some(output) = self.outputs.iter_mut().find(|output| output.bar == id) else {
                    return Task::none();
                };

                output.bar_regions = regions;
                set_input_zone(
                    id,
                    bar::input_region(
                        ferese_panel::PanelSurface::None,
                        output.hidden,
                        &output.bar_regions,
                    ),
                )
            }

            Message::ActivitiesUpdated(snapshot) => {
                self.activities.snapshot = snapshot;
                self.sync_hub_activity()
            }
            Message::ActivityTick => self.sync_hub_activity(),
            Message::Pomodoro(action, session) => cosmic::task::future(async move {
                let result = tokio::task::spawn_blocking(move || {
                    let args = serde_json::to_value(ferese_ipc::activities::PomodoroRequest {
                        version: ferese_ipc::activities::VERSION,
                        action,
                        session,
                    })
                    .map_err(|error| error.to_string())?;
                    crate::compositor_ipc::request("pomodoro", args).map(|_| ())
                })
                .await
                .unwrap_or_else(|error| Err(error.to_string()));
                cosmic::Action::App(Message::PomodoroResult(result))
            }),
            Message::PomodoroResult(result) => {
                if let Err(error) = result {
                    self.notifications
                        .service_error("Pomodoro could not be changed", &error);
                }
                Task::none()
            }
            Message::ActivityHover(id, entered) => {
                self.activity_hover(id, entered);
                self.sync_hub_activity()
            }
            Message::ActivityActivate(id, activity) => self.activate_activity(id, activity),
            Message::ActivityNavigation(id, focused, key) => self
                .activity_navigation(id, focused, key)
                .unwrap_or_else(Task::none),
            Message::MediaUpdated(snapshot) => {
                self.media.receive(snapshot);
                self.refresh_media_art(self.menu.as_ref().is_some_and(|menu| {
                    menu.kind == status_ui::Menu::Media && !menu.motion.closing()
                }));
                Task::none()
            }
            Message::MediaAction(args) => self.media.command(args),
            Message::MediaCompleted(result) => self.media.completed(result),
            Message::MediaTick => {
                self.media.now_us = ferese_ipc::media::now_us();
                Task::none()
            }
            Message::MediaChoose => {
                self.media.choosing = !self.media.choosing;
                Task::none()
            }
            Message::MediaSeek(position) => {
                self.media.seeking = Some(position);
                Task::none()
            }
            Message::MediaSeekCommit => {
                let Some(position) = self.media.seeking.take() else {
                    return Task::none();
                };
                let mut args = self.media.action("seek");
                args["position_us"] = serde_json::json!(position);
                args["track_id"] = serde_json::json!(
                    self.media
                        .snapshot
                        .selected
                        .as_ref()
                        .and_then(|p| p.track_id.clone())
                );
                self.media.command(args)
            }
            Message::MediaArt(art) => {
                if self.media.art.accepts(&art) {
                    self.media.artwork = Some(art);
                }
                Task::none()
            }
            Message::ThemeChanged(snapshot) => {
                let mut config = self.config.clone();
                config.apply_theme(&snapshot.presented);
                config.theme_mode = snapshot.mode;
                if snapshot.error != self.theme_error {
                    self.pending_config_error = snapshot.error.clone();
                    self.theme_error = snapshot.error.clone();
                }
                self.theme_snapshot = *snapshot;
                self.apply_config(config)
            }
            Message::ThemeMode(mode) => cosmic::task::future(async move {
                let result = tokio::task::spawn_blocking(move || {
                    let mut connection =
                        ferese_ipc::Connection::connect().map_err(|e| e.to_string())?;
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
            Message::WallpaperLoaded(result) => {
                match result {
                    Ok(handle) => self.wallpaper = Some(handle),
                    Err(error) => eprintln!("ferese-shell: wallpaper unavailable: {error}"),
                }
                Task::none()
            }
            Message::NativeSurface(id, result) => {
                match result {
                    Ok((_connection, surface))
                        if self.outputs.iter().any(|output| output.bar == id) =>
                    {
                        self.attach_effects(id, &surface)
                    }
                    Ok((_connection, surface)) => {
                        self.attach_power_material(id, &surface);
                        self.attach_launcher_effects(id, &surface);
                        if let Some(entry) = &mut self.notification_surface
                            && entry.id == id
                            && entry.effects.is_none()
                        {
                            match EffectsBinding::attach_role(
                                &mut self.effects_context,
                                &surface,
                                None,
                                1.0,
                            ) {
                                Ok(binding) => entry.effects = Some(binding),
                                Err(error) => eprintln!(
                                    "ferese-shell: notification effects unavailable: {error}"
                                ),
                            }
                        }

                        if let Some(menu) = &mut self.menu
                            && menu.id == id
                        {
                            menu.prepare_surface(&mut self.effects_context, &surface);
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
                self.sync_hub_activity()
            }
            Message::SystemStats(snapshot) => {
                self.system_stats = snapshot;
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
                Task::batch([self.sync_hub_activity(), self.sync_notification_surface()])
            }
            Message::NotificationEvent(event) => {
                let notice = if let notifications::Event::Notice(notice) = &event {
                    Some(notice.id)
                } else {
                    None
                };
                self.notifications.handle_event(event);
                if self.notifications.ready {
                    self.status.notifications = Some(status::Notifications {
                        count: self.notifications.unread(),
                        dnd: self.notifications.dnd,
                    });
                }
                let hub = notice
                    .filter(|_| !self.config.activities.sources.notification_bridge)
                    .map_or_else(Task::none, |notice| self.hub_notice(notice));
                Task::batch([
                    hub,
                    self.sync_hub_activity(),
                    self.sync_hub_notices(),
                    self.sync_notification_surface(),
                ])
            }
            Message::NotificationTick => {
                self.notifications.tick();
                Task::batch([
                    self.sync_hub_activity(),
                    self.sync_hub_notices(),
                    self.sync_notification_surface(),
                ])
            }
            Message::DismissNotification(id) => {
                self.notifications.close(id, 2);
                Task::batch([self.sync_hub_activity(), self.sync_notification_surface()])
            }
            Message::RemoveNotification(id) => {
                self.notifications.dismiss(id);
                Task::batch([self.sync_hub_activity(), self.sync_notification_surface()])
            }
            Message::InvokeNotification(id, action) => {
                self.notifications.invoke(id, action);
                Task::batch([self.sync_hub_activity(), self.sync_notification_surface()])
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
                Task::batch([self.sync_hub_activity(), self.sync_notification_surface()])
            }
            Message::ClearNotifications => {
                self.notifications.clear();
                Task::batch([self.sync_hub_activity(), self.sync_notification_surface()])
            }
            Message::AnimateMenu => self.animate_menu(),
            Message::PanelAction(message) => {
                let close = self.destroy_menu();
                let action = self.update(*message);
                close.chain(action)
            }
            Message::OpenMenu(..) => Task::none(), // Bar views attach an explicit item/surface anchor.
            Message::OpenPopover(kind, anchor) => {
                let tooltip = self.dismiss_workspace_tooltip();
                Task::batch([tooltip, self.open_menu(kind, anchor)])
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
            Message::DisplaysLoaded(generation, result) => {
                self.finish_display_inventory(generation, result)
            }
            Message::SelectDisplayMode(mode) => self.select_display_mode(mode, true),
            Message::ApplyDisplayMode => self.apply_display_mode(),
            Message::DisplayModeCompleted(serial, close, result) => {
                self.finish_display_mode(serial, close, result)
            }
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
            Message::NightLightTemperature(value) => {
                self.night_light_temperature = Some(value);
                Task::none()
            }
            Message::CommitNightLightTemperature => {
                if let Some(value) = self.night_light_temperature.take() {
                    return self.update(Message::Control(status::Action::NightLightTemperature(
                        value,
                    )));
                }
                Task::none()
            }
            Message::Control(action) => {
                if let Some(action) = system_modal::PowerAction::from_status(action.clone()) {
                    return self.open_system_modal(action, None);
                }

                if let status::Action::PowerProfile(profile) = action {
                    self.status_error = self
                        .status_service
                        .send(status::Action::PowerProfile(profile))
                        .err();

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
                            return self.sync_hub_activity();
                        }
                        status::Action::Notifications => {
                            return Task::batch([
                                self.close_menu(),
                                self.toggle_notification_history(),
                            ]);
                        }
                        _ => {}
                    }
                }

                self.status_error = self.status_service.send(action.clone()).err();

                if self.status_error.is_none() {
                    self.optimistic_status(&action);
                }

                if matches!(
                    action,
                    status::Action::Notifications | status::Action::Settings
                ) {
                    return self.close_menu();
                }

                Task::none()
            }
            Message::Event(event, id) => self.handle_event(event, id),
            Message::ClockChanged(labels) => {
                self.clock = labels;
                Task::none()
            }
            Message::RecorderEvent(event) => {
                self.recorder.handle(event);
                self.sync_hub_activity()
            }
            Message::CaptureStatus(event) => {
                self.capture_status.apply(event);
                self.sync_hub_activity()
            }
            Message::RecorderElapsed => Task::none(),
            Message::PanelScale(id, scale) => {
                if let Some(entry) = self.outputs.iter_mut().find(|entry| entry.bar == id)
                    && scale.is_finite()
                    && scale > 0.
                    && scale <= 16.
                {
                    entry.scale = f64::from(scale);
                }
                Task::batch([self.sync_hub_workspaces(), self.resize_hub(id)])
            }
            Message::AdaptiveDeadline(id, ticket) => self.adaptive_deadline(id, ticket),
            Message::ControlReady => {
                let overview_was_active = self.overview_active;
                let mut reload_task = Task::none();
                if let Some(control) = &self.control {
                    let poll = control.poll();

                    if poll.disconnected {
                        return cosmic::iced::exit();
                    }

                    if let Some(active) = poll.overview_active {
                        self.overview_active = active;
                        if active {
                            reload_task = self.dismiss_workspace_tooltip();
                        }
                    }

                    if let Some(source) = poll.config {
                        reload_task = Task::batch([reload_task, self.reload_config(source)]);
                    }

                    if let Some(snapshot) = poll.snapshot {
                        let mode_was_active = self.snapshot.binding_mode.is_some();
                        self.snapshot = snapshot;
                        reload_task = Task::batch([reload_task, self.refresh_launcher()]);
                        let mode = self.binding_mode_transition(mode_was_active);
                        let feedback = self.sync_hub_workspace_feedback(
                            overview_was_active || self.overview_active,
                        );
                        let mut tasks = vec![
                            reload_task,
                            mode,
                            self.update_workspace_ui(),
                            feedback,
                            self.refresh_adaptive_panels(),
                        ];
                        let mut visibility_changed = false;
                        let mut suspended_hubs = Vec::new();
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

                            tasks.push(set_exclusive_zone(
                                entry.bar,
                                entry
                                    .runtime
                                    .panel(&self.config.panels[0])
                                    .exclusive_zone(hidden),
                            ));
                            entry.hidden = hidden;
                            if hidden && entry.runtime.hub.is_some() {
                                suspended_hubs.push(entry.bar);
                            }
                            visibility_changed = true;
                            if let Some(effects) = &entry.effects
                                && let Err(error) = effects.set_visible(
                                    !hidden
                                        && entry.runtime.panel(&self.config.panels[0]).presentation
                                            != ferese_panel::Presentation::Text,
                                )
                            {
                                eprintln!("ferese-shell: could not update panel material: {error}");
                            }
                            tasks.push(set_input_zone(
                                entry.bar,
                                bar::input_region(
                                    ferese_panel::PanelSurface::None,
                                    hidden,
                                    &entry.bar_regions,
                                ),
                            ));
                        }
                        for id in suspended_hubs {
                            tasks.push(self.suspend_hub(id));
                        }
                        if self
                            .menu
                            .as_ref()
                            .is_some_and(|menu| self.bar_hidden(menu.anchor.parent))
                        {
                            tasks.push(self.destroy_menu());
                        }
                        if visibility_changed {
                            tasks.push(self.dismiss_workspace_tooltip());
                            self.refresh_media_art(false);
                        }

                        reload_task = Task::batch(tasks);
                    }
                    for command in poll.commands {
                        let task = match command {
                            control::ControlCommand::DismissLauncher => self.destroy_launcher(),
                            control::ControlCommand::Thumbnail(token, window, image) => {
                                self.launcher_thumbnail(token, window, image);
                                Task::none()
                            }
                            control::ControlCommand::ToggleLauncher(output) => {
                                self.toggle_launcher(output)
                            }
                            control::ControlCommand::ToggleDisplays(output) => {
                                self.open_display_modal(Some(&output))
                            }
                            control::ControlCommand::MonitorsChanged => {
                                self.load_display_inventory()
                            }
                            control::ControlCommand::ToggleGuide(output) => {
                                self.toggle_guide(output)
                            }
                            control::ControlCommand::CancelLogout(serial) => {
                                self.cancel_logout_modal(serial)
                            }
                            control::ControlCommand::Logout(serial, output) => self
                                .open_system_modal(
                                    system_modal::PowerAction::Logout(serial),
                                    Some(&output),
                                ),
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
                self.dismiss_workspace_tooltip()
            }
            Message::HoverWorkspace(bar, workspace, target, entered) => {
                self.hover_workspace(bar, workspace, target, entered)
            }
            Message::WorkspaceTooltipDelay(serial) => self.locate_workspace_tooltip(serial),
            Message::WorkspaceTooltipBounds(serial, bounds) => {
                self.show_workspace_tooltip(serial, bounds)
            }
            Message::WorkspaceIconsLoaded(result) => self.workspace_icons_loaded(result),
            Message::ToggleOverview => {
                self.overview_active = !self.overview_active;
                if let Some(control) = &self.control {
                    control.set_overview_active(self.overview_active);
                }
                self.dismiss_workspace_tooltip()
            }
        };
        self.flush_config_error();
        self.sync_connection_discovery();
        if let Some(control) = &self.control {
            control.set_background_title_updates(
                self.launcher.is_some()
                    || self.outputs.iter().any(|output| {
                        output.runtime.hub.as_ref().is_some_and(|hub| {
                            hub.borrow().hub.mode()
                                == ferese_panel::morph::Mode::Expanded(
                                    ferese_panel::morph::Content::Workspaces,
                                )
                        })
                    }),
            );
        }
        task
    }

    fn style(&self) -> Option<cosmic::iced::theme::Style> {
        // Shell surfaces own their backgrounds. Never inherit an opaque
        // application clear color during startup or a system-theme change.
        Some(shell_surface_style(self.config.theme))
    }

    fn view(&self) -> Element<'_, Self::Message> {
        self.render.text("").into()
    }

    fn view_window(&self, _id: window::Id) -> Element<'_, Self::Message> {
        self.render.text("").into()
    }
}

impl FereseShell {
    fn flush_config_error(&mut self) {
        if self.notifications.ready
            && self.outputs.iter().any(|output| !output.hidden)
            && let Some(error) = self.pending_config_error.take()
        {
            eprintln!("ferese-shell: configuration warning: {error}");
            self.notifications
                .service_error("Configuration could not be loaded", &error);
        }
    }

    fn reload_config(&mut self, source: String) -> Task<Message> {
        let config = match config::parse_source(&source, &self.theme_snapshot) {
            Ok(config) => config,
            Err(error) => {
                eprintln!("ferese-shell: reload rejected; keeping current config: {error}");
                self.notifications
                    .service_error("Configuration could not be loaded", &error.to_string());
                return Task::none();
            }
        };

        self.apply_config(config)
    }

    fn apply_config(&mut self, mut config: config::ShellConfig) -> Task<Message> {
        if std::env::var_os("FERESE_COMPOSITOR_WALLPAPER").is_some() {
            config.wallpaper.path = None;
        }

        if self.config.panels[0].presentation != config.panels[0].presentation {
            self.clock = clock::current_time_for(
                config.panels[0].presentation == ferese_panel::Presentation::Text,
            );
        }
        self.render = rendering::Rendering::new(&config);

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

        let composition_changed = !self.config.panels[0].same_composition(&config.panels[0]);
        let edge_changed = self.config.panels[0].edge != config.panels[0].edge;
        let old = self.config.theme;
        let old_geometry = self.config.panels[0].resolved_geometry();
        let theme = config.theme;
        let geometry = config.panels[0].resolved_geometry();
        let geometry_changed = edge_changed || old_geometry != geometry;
        self.notifications.configure(config.notifications.clone());
        self.notifications
            .configure_activity_bridge(config.activities.sources.notification_bridge);
        self.config = config;
        if composition_changed || geometry_changed {
            for entry in &mut self.outputs {
                entry.runtime.reset(
                    &self.config.panels[0],
                    &self.snapshot,
                    entry.name.as_deref(),
                );
            }
        } else {
            for entry in &mut self.outputs {
                entry.runtime.update_appearance(&self.config.panels[0]);
            }
        }
        let workspace_task = self.update_workspace_ui();
        let mut tasks = vec![workspace_task, self.sync_hub_activity()];

        if composition_changed || geometry_changed {
            tasks.push(self.destroy_menu());
            for output in &mut self.outputs {
                // Keep the measured allocation and margins until the next layout
                // publishes its replacement through PanelResolved.
                tasks.push(set_input_zone(
                    output.bar,
                    bar::input_region(
                        ferese_panel::PanelSurface::None,
                        output.hidden,
                        &output.bar_regions,
                    ),
                ));
            }
        }

        if !self.guide_load.manual
            && !self.config.status.keybinding_guide
            && self
                .system_modal
                .as_ref()
                .is_some_and(|modal| modal.is_guide())
        {
            tasks.push(self.destroy_system_modal(false));
        }

        if old.palette() != theme.palette() {
            tasks.push(cosmic::command::set_theme(theme.palette().native_theme()));
        }

        let entry_iter = self
            .outputs
            .iter()
            .filter(|_| geometry_changed || composition_changed);
        for entry in entry_iter {
            let panel = entry.runtime.panel(&self.config.panels[0]);
            let geometry = panel.resolved_geometry();
            let (anchor, _) =
                surfaces::panel_placement(self.config.panels[0].edge, geometry.edge_margin, 0);
            let hub_size = entry
                .runtime
                .hub
                .as_ref()
                .map(|hub| hub.borrow().allocation);
            tasks.push(set_anchor(
                entry.bar,
                if hub_size.is_some() {
                    anchor
                } else {
                    anchor | Anchor::LEFT | Anchor::RIGHT
                },
            ));
            let (width, height) = hub_size
                .map_or((None, Some(geometry.height.round() as u32)), |size| {
                    (Some(size[0]), Some(size[1]))
                });
            tasks.push(set_size(entry.bar, width, height));
            tasks.push(
                cosmic::iced::platform_specific::shell::commands::layer_surface::set_keyboard_interactivity(
                    entry.bar,
                    KeyboardInteractivity::None,
                ),
            );
            tasks.push(set_exclusive_zone(
                entry.bar,
                panel.exclusive_zone(entry.hidden),
            ));
        }

        let bars: Vec<_> = self.outputs.iter().map(|entry| entry.bar).collect();
        for bar in bars {
            tasks.push(self.update_panel_margin(
                bar,
                composition_changed
                    || edge_changed
                    || old_geometry.edge_margin != geometry.edge_margin,
            ));
        }
        if geometry_changed || composition_changed {
            tasks.push(self.dismiss_workspace_tooltip());
            if let Some(surface) = self.notification_surface.take() {
                tasks.push(destroy_layer_surface(surface.id));
            }
            tasks.push(self.sync_notification_surface());
        }
        Task::batch(tasks)
    }

    fn update_panel_margin(&mut self, bar: window::Id, force: bool) -> Task<Message> {
        let panel = self.panel_for_bar(bar);
        let is_hub = panel.style == ferese_panel::PanelStyle::Morph;
        let geometry = panel.resolved_geometry();
        let Some(entry) = self.outputs.iter_mut().find(|entry| entry.bar == bar) else {
            return Task::none();
        };
        let margin = if is_hub {
            0
        } else {
            entry.size.zip(entry.panel_resolution.as_ref()).map_or(
                entry.bar_margin_horizontal,
                |((width, _), resolution)| {
                    resolution.side_margin(
                        geometry.side_margins,
                        width,
                        f32::from(geometry.inner_padding.round() as u16),
                    )
                },
            )
        };
        if entry.bar_margin_horizontal == margin && !force {
            return Task::none();
        }
        entry.bar_margin_horizontal = margin;
        let (_, margin) =
            surfaces::panel_placement(self.config.panels[0].edge, geometry.edge_margin, margin);
        set_margin(bar, margin.top, margin.right, margin.bottom, margin.left)
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
    use crate::bar::bar_hidden;
    use crate::bar::focused_bar_title;
    use crate::bar::workspace_active_on_bar;
    use crate::control::{OutputSnapshot, WindowSnapshot};
    pub(super) fn shell_with_measured_panel() -> FereseShell {
        let config = ShellConfig::default();
        // This output is never dispatched; tests use only its stored geometry.
        let (socket, _peer) = std::os::unix::net::UnixStream::pair().unwrap();
        let connection = Connection::from_socket(socket).unwrap();
        let output = wl_output::WlOutput::inert(connection.backend().downgrade());
        FereseShell {
            core: Core::default(),
            theme_snapshot: ferese_theme_client::service::fallback(),
            notifications: notifications::Center::new(
                config.notifications.clone(),
                config.animations,
            ),
            notification_surface: None,
            // Layout and interaction tests supply snapshots directly. Native
            // service workers belong to the separate private-bus tests.
            status_service: status::Service::disconnected_for_test(),
            status: Default::default(),
            system_stats: Default::default(),
            connections: Default::default(),
            recorder: Default::default(),
            capture_status: Default::default(),
            media: Default::default(),
            activities: Default::default(),
            calendar_offset: 0,
            status_error: None,
            night_light_temperature: None,
            theme_error: None,
            pending_config_error: None,
            menu: None,
            closing_effects: Default::default(),
            system_modal: None,
            launcher: None,
            launcher_files: Default::default(),
            display_mode: Default::default(),
            guide_shown: false,
            guide_load: Default::default(),
            guide_attempts: 0,
            pending_power: None,
            render: rendering::Rendering::new(&config),
            config,
            wallpaper: None,
            control: None,
            snapshot: Default::default(),
            workspace_ui: Default::default(),
            overview_active: false,
            clock: String::new(),
            outputs: vec![OutputSurfaces {
                runtime: panel_runtime::Runtime::new(
                    &panel::Panel::default(),
                    &ShellSnapshot::default(),
                    None,
                ),
                scale: 1.,
                output,
                name: Some("test".into()),
                bar: window::Id::unique(),
                wallpaper: None,
                size: Some((1200, 800)),
                effects: None,
                bar_regions: vec![[0., 0., 600., 36., 8.]],
                panel_resolution: Some(panel_layout::Resolution {
                    minimum_width: 600.,
                    ..Default::default()
                }),
                bar_margin_horizontal: 0,
                hidden: false,
            }],
            effects_context: None,
        }
    }

    #[test]
    fn panel_placement_uses_only_the_selected_edge_for_anchor_and_margin() {
        use ferese_panel::Edge;
        for gap in [0, 12] {
            let (anchor, margin) = surfaces::panel_placement(Edge::Top, gap, 32);
            assert_eq!(anchor, Anchor::TOP);
            assert_eq!(
                (margin.top, margin.right, margin.bottom, margin.left),
                (gap, 32, 0, 32)
            );
            let (anchor, margin) = surfaces::panel_placement(Edge::Bottom, gap, 32);
            assert_eq!(anchor, Anchor::BOTTOM);
            assert_eq!(
                (margin.top, margin.right, margin.bottom, margin.left),
                (0, 32, gap, 32)
            );
        }
    }

    #[test]
    fn dnd_refreshes_activity_selection_without_waiting_for_another_event() {
        use cosmic::Application;
        use ferese_activities::{Data, Owner, Source, Store, Template, Value};
        let mut shell = shell_with_measured_panel();
        shell.config.panels[0].style = ferese_panel::PanelStyle::Morph;
        shell.config.panels[0].center.groups[0]
            .items
            .push(ferese_panel::Item::new(
                "test-activities",
                ferese_panel::ItemKind::Activities,
            ));
        shell.outputs[0]
            .runtime
            .reset(&shell.config.panels[0], &shell.snapshot, Some("test"));
        shell.snapshot.outputs.push(OutputSnapshot {
            id: 1,
            name: "test".into(),
            active_workspace: 1,
            focused: true,
        });
        shell.notifications.ready = true;
        let mut store = Store::new(1);
        for n in 1..=2 {
            store
                .start(
                    Owner { pid: n, birth: 1 },
                    n.to_string(),
                    Data {
                        source: Source::Publisher,
                        output: Some("test".into()),
                        priority: 100,
                        icon: "hammer".into(),
                        title: format!("Task {n}"),
                        value: Value::Indeterminate,
                        template: Template::Progress,
                    },
                    0,
                    0,
                    None,
                    true,
                    true,
                    None,
                )
                .unwrap();
        }
        shell.activities.snapshot = std::sync::Arc::new(ferese_ipc::activities::Snapshot {
            activities: store.entries().cloned().collect(),
            ..Default::default()
        });
        shell.sync_activity_presentations();
        let selected = |shell: &FereseShell| {
            shell.outputs[0]
                .runtime
                .hub
                .as_ref()
                .unwrap()
                .borrow()
                .activities
                .selection
                .visible
                .len()
        };
        assert_eq!(selected(&shell), 2);
        drop(shell.update(Message::Control(status::Action::Dnd(true))));
        assert_eq!(selected(&shell), 1);
        drop(shell.update(Message::Control(status::Action::Dnd(false))));
        assert_eq!(selected(&shell), 2);
    }

    #[test]
    fn reload_preserves_the_last_presented_theme_without_a_control_connection() {
        use cosmic::Application;

        let mut shell = shell_with_measured_panel();
        let mut snapshot = ferese_theme_client::service::fallback();
        snapshot.mode = ferese_config::theme::Mode::Light;
        snapshot.presented.appearance = ferese_config::theme::Appearance::Light;
        snapshot.presented.tokens.geometry.shell_radius = 23.;
        snapshot.presented.tokens.typography.font_family = "Reload test font".into();
        drop(shell.update(Message::ThemeChanged(Box::new(snapshot.clone()))));

        drop(shell.reload_config("status { low-battery-threshold 35; }".into()));

        assert!(shell.control.is_none());
        assert_eq!(shell.config.status.low_battery_threshold, 35);
        assert_eq!(shell.config.theme_mode, snapshot.mode);
        assert_eq!(shell.config.theme.appearance, snapshot.presented.appearance);
        assert_eq!(shell.config.theme.material_radius, 23.);
        assert_eq!(
            shell.config.font_family.as_deref(),
            Some("Reload test font")
        );
        assert_eq!(shell.theme_snapshot, snapshot);
    }

    #[test]
    fn cosmetic_reloads_preserve_measured_margins_and_the_open_popover() {
        let mut shell = shell_with_measured_panel();
        shell.config.panels[0].style = ferese_panel::PanelStyle::Floating;
        shell.config.panels[0].geometry.side_margins = Some(240);
        drop(shell.update_panel_margin(shell.outputs[0].bar, false));
        assert_eq!(shell.outputs[0].bar_margin_horizontal, 240);
        let popup = window::Id::unique();
        shell.menu = Some(status_ui::OpenMenu {
            id: popup,
            anchor: status_ui::PopoverAnchor {
                parent: shell.outputs[0].bar,
                panel: shell.config.panels[0].id.clone(),
                item: Some(panel::ItemId("clock".into())),
                rectangle: Default::default(),
            },
            kind: status_ui::Menu::Calendar,
            motion: motion::PopupMotion::new(Default::default()),
            effects: None,
            regions: Default::default(),
        });
        let resolution = shell.outputs[0].panel_resolution.clone();
        for (field, value) in [
            ("background_opacity", serde_json::json!(0.5)),
            ("corner_radius", serde_json::json!("4px 8px")),
            ("border", serde_json::json!(true)),
        ] {
            let edit = field;
            let mut document = ferese_config::Document::parse("").unwrap();
            document
                .set(
                    "panels",
                    serde_json::to_value(&shell.config.panels).unwrap(),
                )
                .unwrap();
            document.set(&format!("panels.0.{field}"), value).unwrap();
            let source = document.to_string();
            let mut config = config::parse_source(&source, &shell.theme_snapshot).unwrap();
            config.theme = shell.config.theme;
            drop(shell.apply_config(config));
            assert_eq!(shell.outputs[0].panel_resolution, resolution, "{edit}");
            assert_eq!(shell.outputs[0].bar_margin_horizontal, 240, "{edit}");
            assert_eq!(shell.menu.as_ref().unwrap().id, popup, "{edit}");
        }
        // A real membership edit closes the popup, but retains the last allocation
        // until the adaptive widget measures and publishes its replacement.
        let mut config = shell.config.clone();
        config.panels[0].end.groups.clear();
        drop(shell.apply_config(config));
        assert!(shell.menu.is_none());
        assert_eq!(shell.outputs[0].panel_resolution, resolution);
        assert_eq!(shell.outputs[0].bar_margin_horizontal, 240);
        use cosmic::Application;
        let replacement = panel_layout::Resolution {
            minimum_width: 900.,
            ..Default::default()
        };
        drop(shell.update(Message::PanelResolved(
            shell.outputs[0].bar,
            replacement.clone(),
        )));
        assert_eq!(
            shell.outputs[0].panel_resolution.as_ref(),
            Some(&replacement)
        );
        assert!(shell.outputs[0].bar_margin_horizontal < 240);
        let before = shell.outputs[0].bar_margin_horizontal;
        drop(shell.reload_config("panel broken {".into()));
        assert_eq!(
            shell.outputs[0].panel_resolution.as_ref(),
            Some(&replacement)
        );
        assert_eq!(shell.outputs[0].bar_margin_horizontal, before);
    }

    #[test]
    fn reading_notifications_does_not_reallocate_and_dismiss_the_popover() {
        use cosmic::Application;
        use cosmic::iced::advanced::layout;
        use cosmic::iced::advanced::renderer::Headless;
        use cosmic::iced::advanced::widget::Tree;
        use cosmic::iced::{Font, Pixels, Size};
        use panel_layout::{Placement, Resolution};

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let renderer = runtime
            .block_on(<cosmic::Renderer as Headless>::new(
                Font::default(),
                Pixels(14.0),
                Some("tiny-skia"),
            ))
            .unwrap();
        let mut shell = shell_with_measured_panel();
        shell.notifications.ready = true;
        shell.status.notifications = Some(status::Notifications {
            count: 3,
            dnd: false,
        });
        let item = panel::ItemId("notifications".into());
        let bar = shell.outputs[0].bar;
        let resolution = |shell: &FereseShell| {
            let mut control = shell.view_status_item(status_ui::Menu::Notifications, false, false);
            let mut tree = Tree::new(control.as_widget());
            let node = control.as_widget_mut().layout(
                &mut tree,
                &renderer,
                &layout::Limits::new(Size::ZERO, Size::new(200.0, 60.0)),
            );
            Resolution {
                items: [(
                    item.clone(),
                    Placement::Visible {
                        representation: panel::Representation::Icon,
                        width: node.size().width,
                    },
                )]
                .into(),
                zone_widths: [0.0, 0.0, node.size().width],
                ..Default::default()
            }
        };
        shell.outputs[0].panel_resolution = Some(resolution(&shell));
        drop(shell.open_menu(
            status_ui::Menu::Notifications,
            status_ui::PopoverAnchor {
                parent: bar,
                panel: shell.config.panels[0].id.clone(),
                item: Some(item.clone()),
                rectangle: cosmic::iced::Rectangle {
                    x: 1000,
                    y: 0,
                    width: 40,
                    height: 36,
                },
            },
        ));
        let popup = shell.menu.as_ref().expect("first click opens history").id;
        assert!(shell.notifications.history_open);
        assert_eq!(shell.status.notifications.as_ref().unwrap().count, 0);
        let after_reading = resolution(&shell);
        drop(shell.update(Message::PanelResolved(bar, after_reading)));
        assert_eq!(
            shell
                .menu
                .as_ref()
                .expect("clearing the badge must not close history")
                .id,
            popup
        );
        assert!(shell.notifications.history_open);
    }

    #[test]
    fn panel_reallocation_dismisses_popovers_in_both_overflow_directions() {
        use cosmic::Application;
        use panel_layout::{Placement, Resolution};
        let item = panel::ItemId("network".into());
        let visible = Resolution {
            items: [(
                item.clone(),
                Placement::Visible {
                    representation: panel::Representation::Icon,
                    width: 28.,
                },
            )]
            .into(),
            zone_widths: [0., 0., 40.],
            ..Default::default()
        };
        let overflow = Resolution {
            items: [(item.clone(), Placement::Overflow)].into(),
            overflow: vec![item.clone()],
            zone_widths: [0., 0., 32.],
            overflow_trigger_width: 24.,
            ..Default::default()
        };
        let mut moved = visible.clone();
        moved.zone_widths[2] = 160.;
        for (before, after, kind) in [
            (&overflow, &visible, status_ui::Menu::Network),
            (&visible, &overflow, status_ui::Menu::Network),
            (&visible, &moved, status_ui::Menu::Network),
            (&overflow, &visible, status_ui::Menu::Overflow),
        ] {
            let mut shell = shell_with_measured_panel();
            let bar = shell.outputs[0].bar;
            shell.outputs[0].panel_resolution = Some(before.clone());
            shell.menu = Some(status_ui::OpenMenu {
                id: window::Id::unique(),
                anchor: status_ui::PopoverAnchor {
                    parent: bar,
                    panel: shell.config.panels[0].id.clone(),
                    item: Some(if kind == status_ui::Menu::Overflow {
                        panel::ItemId("_overflow".into())
                    } else {
                        item.clone()
                    }),
                    rectangle: cosmic::iced::Rectangle {
                        x: 1100,
                        y: 0,
                        width: 28,
                        height: 36,
                    },
                },
                kind,
                motion: motion::PopupMotion::new(Default::default()),
                effects: None,
                regions: Default::default(),
            });
            let popup = shell.menu.as_ref().unwrap().id;
            drop(shell.update(Message::PanelResolved(window::Id::unique(), after.clone())));
            assert_eq!(
                shell.menu.as_ref().unwrap().id,
                popup,
                "another output must not dismiss this popup"
            );
            drop(shell.update(Message::PanelResolved(bar, before.clone())));
            assert_eq!(
                shell.menu.as_ref().unwrap().id,
                popup,
                "unchanged allocation must preserve the popup"
            );
            drop(shell.update(Message::PanelResolved(bar, after.clone())));
            assert!(
                shell.menu.is_none(),
                "stale {kind:?} popup must be destroyed"
            );
            assert_eq!(shell.outputs[0].panel_resolution.as_ref(), Some(after));
        }
    }

    #[test]
    fn late_open_checks_preserve_owned_wallpaper_and_tooltips() {
        let mut shell = shell_with_measured_panel();
        let wallpaper = window::Id::unique();
        let tooltip = window::Id::unique();
        shell.outputs[0].wallpaper = Some(wallpaper);
        shell.workspace_ui.tooltip = Some(tooltip);
        for id in [wallpaper, tooltip] {
            assert!(shell.owns_surface(id));
            assert_eq!(
                shell
                    .handle_event(
                        Event::Window(window::Event::Opened {
                            position: None,
                            size: cosmic::iced::Size::new(100., 100.),
                        }),
                        id,
                    )
                    .units(),
                0,
                "an owned surface must not be destroyed on opening"
            );
        }
        assert!(!shell.owns_surface(window::Id::unique()));
    }

    #[test]
    fn an_empty_measured_panel_is_not_an_unmeasured_panel() {
        let mut shell = shell_with_measured_panel();
        let bar = shell.outputs[0].bar;
        shell.config.panels[0].style = ferese_panel::PanelStyle::Floating;
        shell.config.panels[0].geometry.side_margins = Some(240);
        shell.outputs[0].panel_resolution = None;
        drop(shell.update_panel_margin(bar, false));
        assert_eq!(shell.outputs[0].bar_margin_horizontal, 0);
        shell.outputs[0].panel_resolution = Some(Default::default());
        drop(shell.update_panel_margin(bar, false));
        assert_eq!(shell.outputs[0].bar_margin_horizontal, 240);
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
        let before = ferese_theme::panel::group_border(
            color(theme.border),
            theme.material_radius.min(16.),
            true,
        );
        let after = ferese_theme::panel::group_border(
            color(contrasted.border),
            contrasted.material_radius.min(16.),
            true,
        );
        assert_eq!(before.border, after.border);
        assert_eq!(before.background, after.background);
        assert_eq!(before.border.color, color(theme.border));
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
        assert_eq!(
            shell_surface_style(theme).background_color,
            Color::TRANSPARENT
        );
        let background = color(theme.for_bar().bar_background);
        assert!(background.r < 0.15 && background.g < 0.15 && background.b < 0.20);
        assert!(background.a > 0.9);
        assert!(
            theme.bar_text_primary[..3]
                .iter()
                .all(|channel| *channel > 220)
        );
    }

    #[test]
    fn compact_bar_preserves_logical_text_and_icon_sizes() {
        for height in [24.0, 26.0, 32.0, 38.0, 44.0] {
            let metrics = BarMetrics::from(panel::PanelGeometry {
                height,
                ..Default::default()
            });
            assert_eq!(metrics.icon_size, 16);
            assert!(metrics.icon_slot_width >= metrics.icon_size);
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
        assert_eq!(
            focused_bar_title(&snapshot, snapshot.outputs.first()),
            "Test"
        );
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

    pub(super) fn snapshot_with_fullscreen_window(workspace: u64) -> ShellSnapshot {
        ShellSnapshot {
            layouts: Default::default(),
            binding_mode: None,
            outputs: vec![OutputSnapshot {
                id: 1,
                name: "eDP-1".to_owned(),
                active_workspace: 7,
                focused: true,
            }],
            workspaces: Vec::new(),
            windows: vec![WindowSnapshot {
                id: 42,
                workspace,
                app_id: "dev.ferese.Test".to_owned(),
                title: "Test".to_owned(),
                focused: true,
                urgent: false,
                fullscreen: true,
                maximized: false,
                floating: false,
                geometry: None,
            }],
        }
    }
}
