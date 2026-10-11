use std::cell::RefCell;
use std::time::Instant;

use crate::KeyboardInteractivity;
use cosmic::app::Task;
use cosmic::iced::platform_specific::shell::commands::layer_surface::{set_keyboard_interactivity, set_size};
use cosmic::iced::{Alignment, Length, window};
use cosmic::widget::{button, column, container, row, scrollable};

mod equalizer;
mod workspaces;
use cosmic::{Element, theme};
use ferese_panel::morph::{Content, Hub, Mode, Sample};
use ferese_panel::{Edge, ItemKind, Panel, PanelStyle};
use ferese_theme::icons;

use crate::{FereseShell, Message, color_with_opacity, motion, status_ui, text};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Peek {
    Media,
    Date,
    Workspaces,
}

struct ControlAction {
    icon: &'static [u8],
    label: &'static str,
    message: Option<Message>,
    menu: Option<status_ui::Menu>,
}

pub(super) struct Runtime {
    pub(super) hub: Hub,
    pub(super) menu: Option<status_ui::Menu>,
    previous_menu: Option<status_ui::Menu>,
    toast: Option<u32>,
    pub(super) allocation: [u32; 2],
    minimum_allocation: [u32; 2],
    pub(super) revision: Instant,
    sampled: Instant,
    last_menu: status_ui::Menu,
    peek: Peek,
    previous_peek: Peek,
    previous_peek_width: f64,
    workspace_feedback: Option<Instant>,
    observed_workspace: Option<u64>,
    hovered: bool,
    idle_content_width: Option<f64>,
    available: [f64; 2],
    peek_width: f64,
    idle_width: f64,
    workspace_width: f64,
    workspace_scroll: workspaces::Scroll,
}

impl Runtime {
    pub(super) fn new(panel: &Panel) -> Self {
        let mut hub = Hub::new(panel.morph, panel.resolved_geometry().height.into()).expect("validated panel hub");
        hub.pin_radii(panel.corner_radius.map(|r| r.0.map(f64::from)))
            .expect("validated panel radii");
        hub.advance(std::time::Duration::ZERO, true);
        let now = Instant::now();
        // Keep the native surface stable through hover, including spring overshoot.
        let minimum_allocation = [
            (panel.morph.peek_width.max(panel.morph.idle_width).max(200.) * 1.1).ceil() as u32,
            (panel.morph.peek_height.max(f64::from(panel.resolved_geometry().height)) * 1.1).ceil() as u32,
        ];
        Self {
            allocation: minimum_allocation,
            minimum_allocation,
            hub,
            menu: None,
            previous_menu: None,
            toast: None,
            revision: now,
            sampled: now,
            last_menu: status_ui::Menu::Media,
            peek: Peek::Date,
            previous_peek: Peek::Date,
            previous_peek_width: 200.,
            workspace_feedback: None,
            observed_workspace: None,
            hovered: false,
            idle_content_width: None,
            available: [1280., 720.],
            peek_width: 200.,
            idle_width: panel.morph.idle_width,
            workspace_width: 0.,
            workspace_scroll: Default::default(),
        }
    }

    fn sample(&mut self, now: Instant) -> Sample {
        let policy = motion::settings();
        self.hub.advance(
            now.saturating_duration_since(self.sampled).mul_f64(policy.speed),
            !policy.motion_enabled(),
        );
        self.sampled = self.sampled.max(now);
        self.hub.sample()
    }

    fn retarget(&mut self, mode: Mode, available: [f64; 2], menu: Option<status_ui::Menu>) {
        let now = Instant::now();
        self.available = available;
        if mode != Mode::Peek && self.workspace_feedback.take().is_some() {
            let regular = self.previous_peek;
            self.previous_peek = self.peek;
            self.peek = regular;
            self.peek_width = self.previous_peek_width;
        }
        let before = self.sample(now);
        if before.opacity >= before.previous_opacity {
            self.previous_menu = self.menu;
        }
        let replace_content = self.hub.mode() == mode && self.menu != menu;
        self.menu = menu;
        if mode == Mode::Idle {
            self.hub.retarget_idle(
                self.idle_content_width
                    .unwrap_or(self.idle_width + self.workspace_width)
                    .clamp(24., 1024.),
                available,
            )
        } else if mode == Mode::Peek {
            self.hub.retarget_peek(self.peek_width.min(1024.), available)
        } else {
            self.hub.retarget(mode, available)
        }
        .expect("bounded hub area");
        if replace_content {
            self.hub.replace_content();
        }
        let target = self.hub.allocation_size().map(|n| n.ceil() as u32);
        // Retain the backing surface between opens; resizing after settlement moves its origin.
        self.minimum_allocation = std::array::from_fn(|i| self.minimum_allocation[i].min(available[i].ceil() as u32));
        self.allocation = std::array::from_fn(|i| self.allocation[i].max(target[i]).max(self.minimum_allocation[i]));
        self.revision = now;
    }
}

pub(super) fn new(panel: &Panel) -> Option<RefCell<Runtime>> {
    (panel.style == PanelStyle::Morph).then(|| RefCell::new(Runtime::new(panel)))
}

fn content(kind: status_ui::Menu) -> Content {
    match kind {
        status_ui::Menu::Media => Content::Media,
        status_ui::Menu::Calendar => Content::Calendar,
        status_ui::Menu::Notifications => Content::Notifications,
        _ => Content::Controls,
    }
}

impl FereseShell {
    pub(super) fn hub_mode(&self, id: window::Id) -> Option<Mode> {
        self.outputs
            .iter()
            .find(|entry| entry.bar == id)?
            .runtime
            .hub
            .as_ref()
            .map(|hub| hub.borrow().hub.mode())
    }

    pub(super) fn hub_hover(&mut self, id: window::Id, entered: bool) -> Task<Message> {
        if self.bar_hidden(id) {
            return Task::none();
        }
        if let Some(runtime) = self
            .outputs
            .iter()
            .find(|entry| entry.bar == id)
            .and_then(|entry| entry.runtime.hub.as_ref())
        {
            runtime.borrow_mut().hovered = entered;
        }
        match self.hub_mode(id) {
            Some(Mode::Idle) if entered && self.panel_for_bar(id).morph.peek_on_hover => {
                let player = self.media.snapshot.selected.as_ref().filter(|player| {
                    matches!(
                        player.status,
                        ferese_ipc::media::Playback::Playing | ferese_ipc::media::Playback::Paused
                    )
                });
                let peek = if player.is_some() { Peek::Media } else { Peek::Date };
                let date_width = (self.clock.chars().count() as f64 * 7. + 54.)
                    .clamp(200., self.panel_for_bar(id).morph.peek_width.clamp(200., 280.));
                let width = player.map_or(date_width, |player| {
                    (player.label().chars().count() as f64 * 7. + 77.)
                        .clamp(200., self.panel_for_bar(id).morph.peek_width.clamp(200., 280.))
                });
                if let Some(runtime) = self
                    .outputs
                    .iter()
                    .find(|entry| entry.bar == id)
                    .and_then(|entry| entry.runtime.hub.as_ref())
                {
                    let mut runtime = runtime.borrow_mut();
                    runtime.previous_peek = runtime.peek;
                    runtime.peek = peek;
                    runtime.peek_width = width;
                }
                self.set_hub_mode(id, Mode::Peek, None)
            }
            Some(Mode::Peek) if !entered => self.set_hub_mode(id, Mode::Idle, None),
            _ => Task::none(),
        }
    }

    pub(super) fn open_hub(&mut self, id: window::Id, kind: status_ui::Menu) -> Task<Message> {
        if self.bar_hidden(id) || self.hub_mode(id).is_none() {
            return Task::none();
        }
        if let Some(runtime) = self
            .outputs
            .iter()
            .find(|entry| entry.bar == id)
            .and_then(|entry| entry.runtime.hub.as_ref())
        {
            let mut runtime = runtime.borrow_mut();
            runtime.last_menu = kind;
        }
        if kind == status_ui::Menu::Notifications {
            for notice in &mut self.notifications.entries {
                notice.unread = false;
            }
            if let Some(status) = &mut self.status.notifications {
                status.count = 0;
            }
        }
        if kind == status_ui::Menu::Calendar {
            self.calendar_offset = 0;
        }
        if kind == status_ui::Menu::Media {
            self.media.now_us = ferese_ipc::media::now_us();
            self.media.refresh_art(true);
            self.media.error = None;
        }
        let close = self.destroy_menu();
        let open = self.set_hub_mode(id, Mode::Expanded(content(kind)), Some(kind));
        Task::batch([close, open])
    }

    pub(super) fn suspend_hub(&mut self, id: window::Id) -> Task<Message> {
        let close = self.close_hub(id);
        let Some(hub) = self
            .outputs
            .iter()
            .find(|entry| entry.bar == id)
            .and_then(|entry| entry.runtime.hub.as_ref())
        else {
            return close;
        };
        let mut hub = hub.borrow_mut();
        hub.hub.advance(std::time::Duration::ZERO, true);
        hub.allocation = hub.minimum_allocation;
        Task::batch([close, set_size(id, Some(hub.allocation[0]), Some(hub.allocation[1]))])
    }

    pub(super) fn hub_notice(&mut self, notice: u32) -> Task<Message> {
        let Some(id) = self.active_bar() else {
            return Task::none();
        };
        if self.bar_hidden(id)
            || !matches!(self.hub_mode(id), Some(Mode::Idle | Mode::Peek | Mode::Toast))
            || !self
                .notifications
                .popup_groups()
                .iter()
                .any(|(entry, _, _)| entry.id == notice)
        {
            return Task::none();
        }
        let open = self.set_hub_mode(id, Mode::Toast, None);
        let revision = self
            .outputs
            .iter()
            .find(|entry| entry.bar == id)
            .and_then(|entry| entry.runtime.hub.as_ref())
            .unwrap()
            .borrow()
            .revision;
        self.outputs
            .iter()
            .find(|entry| entry.bar == id)
            .and_then(|entry| entry.runtime.hub.as_ref())
            .unwrap()
            .borrow_mut()
            .toast = Some(notice);
        let timeout = std::time::Duration::from_millis(u64::from(self.config.notifications.timeout_ms));
        Task::batch([
            open,
            Task::perform(
                async move {
                    tokio::time::sleep(timeout).await;
                },
                move |_| cosmic::Action::App(Message::HubToastExpired(id, revision)),
            ),
        ])
    }

    pub(super) fn sync_hub_notices(&mut self) -> Task<Message> {
        let closing: Vec<_> = self
            .outputs
            .iter()
            .filter_map(|entry| {
                let hub = entry.runtime.hub.as_ref()?.borrow();
                (hub.hub.mode() == Mode::Toast
                    && hub.toast.is_some_and(|id| {
                        !self
                            .notifications
                            .entries
                            .iter()
                            .any(|notice| notice.id == id && notice.live)
                    }))
                .then_some(entry.bar)
            })
            .collect();
        Task::batch(closing.into_iter().map(|id| self.close_hub(id)))
    }

    pub(super) fn hub_toast_expired(&mut self, id: window::Id, revision: Instant) -> Task<Message> {
        let current = self
            .outputs
            .iter()
            .find(|entry| entry.bar == id)
            .and_then(|entry| entry.runtime.hub.as_ref())
            .is_some_and(|hub| {
                let hub = hub.borrow();
                hub.revision == revision && hub.hub.mode() == Mode::Toast
            });
        if current { self.close_hub(id) } else { Task::none() }
    }

    pub(super) fn close_hub(&mut self, id: window::Id) -> Task<Message> {
        self.set_hub_mode(id, Mode::Idle, None)
    }

    pub(super) fn set_hub_mode(&mut self, id: window::Id, mode: Mode, menu: Option<status_ui::Menu>) -> Task<Message> {
        let workspace_width = self.hub_workspace_width(id);
        let panel = self.panel_for_bar(id);
        let geometry = panel.resolved_geometry();
        let Some(entry) = self.outputs.iter().find(|entry| entry.bar == id) else {
            return Task::none();
        };
        let Some(hub) = &entry.runtime.hub else {
            return Task::none();
        };
        let (width, height) = entry.size.unwrap_or((1280, 720));
        let available = [
            (f64::from(width) - 2. * f64::from(geometry.side_margins)).max(24.),
            (f64::from(height) - f64::from(geometry.edge_margin)).max(24.),
        ];
        let resize = {
            let mut hub = hub.borrow_mut();
            let before = hub.allocation;
            hub.workspace_width = if workspace_width > 0. {
                workspace_width + 10.
            } else {
                0.
            };
            hub.retarget(mode, available, menu);
            (before != hub.allocation).then_some(hub.allocation)
        };
        Task::batch([
            resize.map_or_else(Task::none, |size| set_size(id, Some(size[0]), Some(size[1]))),
            set_keyboard_interactivity(
                id,
                if mode.expanded() {
                    KeyboardInteractivity::OnDemand
                } else {
                    KeyboardInteractivity::None
                },
            ),
            self.dismiss_workspace_tooltip_for_bar(id),
        ])
    }

    pub(super) fn view_hub(&self, id: window::Id) -> Element<'_, cosmic::Action<Message>> {
        let entry = self.outputs.iter().find(|entry| entry.bar == id).expect("panel output");
        let runtime = entry.runtime.hub.as_ref().expect("Morph runtime");
        let revision = runtime.borrow().revision;
        let measurement = if runtime.borrow().hub.mode() == Mode::Idle {
            let cap = {
                let runtime = runtime.borrow();
                (runtime.idle_width + runtime.workspace_width)
                    .min(runtime.available[0])
                    .min(f64::from(runtime.allocation[0]) / 1.1) as f32
            };
            Some(motion::ContentMeasurement {
                content: self.hub_content(id, Mode::Idle, 1., None, false, true),
                width: cap,
                measured: Box::new(move |size| {
                    let mut runtime = runtime.borrow_mut();
                    let width = f64::from(size.width).ceil().clamp(24., 1024.);
                    runtime.idle_content_width = Some(width);
                    let available = runtime.available;
                    runtime
                        .hub
                        .retarget_idle(width, available)
                        .expect("measured idle content");
                }),
            })
        } else {
            (runtime.borrow().hub.mode() == Mode::Expanded(Content::Controls)).then(|| {
                let menu = runtime.borrow().menu.unwrap_or(status_ui::Menu::Overflow);
                let content = if menu == status_ui::Menu::Overflow {
                    self.hub_controls(id, 1.)
                } else {
                    self.view_hub_menu(menu, 1.)
                };
                let width = runtime.borrow().hub.target_size()[0] as f32;
                motion::ContentMeasurement {
                    content,
                    width: (width - 28.).max(1.),
                    measured: Box::new(move |size| {
                        runtime
                            .borrow_mut()
                            .hub
                            .fit_content_height(f64::from(size.height).ceil() + 56.)
                            .expect("measured hub content");
                    }),
                }
            })
        };
        motion::geometry_driven(
            Some(revision),
            move |now| {
                let (sample, menu, previous_menu) = {
                    let mut runtime = runtime.borrow_mut();
                    (runtime.sample(now), runtime.menu, runtime.previous_menu)
                };
                self.view_hub_at(id, sample, menu, previous_menu)
            },
            move |_| runtime.borrow().hub.active(),
            |_| {},
            |_| None,
            measurement,
        )
    }

    fn view_hub_at(
        &self,
        id: window::Id,
        sample: Sample,
        menu: Option<status_ui::Menu>,
        previous_menu: Option<status_ui::Menu>,
    ) -> Element<'_, cosmic::Action<Message>> {
        let panel = self.panel_for_bar(id);
        let entry = self.outputs.iter().find(|entry| entry.bar == id).expect("panel output");
        let effects = entry.effects.as_ref();
        let bounds = sample.geometry.bounds;
        let mut layers = cosmic::iced::widget::Stack::new();
        if sample.previous_opacity > 0. {
            layers = layers.push(self.hub_content(
                id,
                sample.previous,
                sample.previous_opacity,
                previous_menu,
                false,
                false,
            ));
        }
        if sample.opacity > 0. {
            layers = layers.push(self.hub_content(id, sample.current, sample.opacity, menu, true, false));
        }
        let body = container(layers)
            .id("ferese-panel-hub")
            .width(bounds.width as f32)
            .height(bounds.height as f32)
            .clip(true);
        let root = container(body)
            .center_x(Length::Fill)
            .height(Length::Fill)
            .align_y(if panel.edge == Edge::Bottom {
                cosmic::iced::alignment::Vertical::Bottom
            } else {
                cosmic::iced::alignment::Vertical::Top
            });
        let palette = self.config.theme.for_bar();
        let mut background = palette.surface_raised.map(|value| f32::from(value) / 255.);
        background[3] = palette.panel_opacity(panel.background_opacity.or(Some(1.)));
        let background = cosmic::iced::Color::from_rgba(background[0], background[1], background[2], background[3]);
        let border = crate::color(palette.border);
        ferese_theme::panel_surface::frame(
            self.with_workspace_hover(id, root.into()),
            ferese_theme::panel_surface::Options {
                parameters: ferese_panel::scene::Parameters {
                    radius: Some(ferese_panel::shape::AnchoredRadii(
                        sample.geometry.radii.map(|r| r as f32),
                    )),
                    ..panel.shape_parameters()
                },
                bounds: Some(cosmic::iced::advanced::widget::Id::new("ferese-panel-hub")),
                groups: Vec::new(),
                scale: entry.scale,
                high_contrast: palette.high_contrast,
                background,
                border: if panel.border {
                    border
                } else {
                    cosmic::iced::Color::TRANSPARENT
                },
                divider: cosmic::iced::Color::TRANSPARENT,
                native: effects.is_some(),
            },
            move |scene| {
                if let Some(effects) = effects
                    && let Err(error) = effects.set_panel_presentation(scene)
                {
                    eprintln!("ferese-shell: could not update hub material: {error}");
                }
            },
            Some(Box::new(move |scene| {
                cosmic::Action::App(Message::PanelSurfaceChanged(id, Box::new(scene.clone())))
            })),
        )
    }

    fn hub_content(
        &self,
        id: window::Id,
        mode: Mode,
        opacity: f32,
        menu: Option<status_ui::Menu>,
        interactive: bool,
        intrinsic: bool,
    ) -> Element<'_, cosmic::Action<Message>> {
        let palette = self.config.theme;
        let primary = color_with_opacity(palette.text_primary, opacity);
        let icon_primary = crate::color(palette.text_primary);
        let icon_muted = crate::color(palette.text_muted);
        let content: Element<'_, cosmic::Action<Message>> =
            match mode {
                Mode::Idle | Mode::Peek | Mode::Toast => {
                    let time = self
                        .clock
                        .split_once(", ")
                        .map_or(self.clock.as_str(), |(_, time)| time);
                    let peek = self
                        .outputs
                        .iter()
                        .find(|entry| entry.bar == id)
                        .and_then(|entry| entry.runtime.hub.as_ref())
                        .map_or(Peek::Date, |runtime| {
                            let runtime = runtime.borrow();
                            if !interactive && !intrinsic {
                                runtime.previous_peek
                            } else {
                                runtime.peek
                            }
                        });
                    let label = if mode == Mode::Toast {
                        self.outputs
                            .iter()
                            .find(|entry| entry.bar == id)
                            .and_then(|entry| entry.runtime.hub.as_ref())
                            .and_then(|hub| hub.borrow().toast)
                            .and_then(|id| self.notifications.entries.iter().find(|notice| notice.id == id))
                            .map_or_else(|| time.to_owned(), |notice| notice.title.clone())
                    } else if mode == Mode::Peek {
                        match peek {
                            Peek::Media => self
                                .media
                                .snapshot
                                .selected
                                .as_ref()
                                .map_or_else(|| "Playback ended".to_owned(), |player| player.label().to_owned()),
                            Peek::Date => self.clock.clone(),
                            Peek::Workspaces => self
                                .output_for_bar(id)
                                .and_then(|output| {
                                    self.snapshot
                                        .workspaces
                                        .iter()
                                        .find(|w| w.id == output.active_workspace)
                                })
                                .map_or_else(
                                    || "Workspaces".to_owned(),
                                    |workspace| {
                                        if workspace.name == workspace.index.to_string() {
                                            format!("Workspace {}", workspace.index)
                                        } else {
                                            format!("{} · {}", workspace.index, workspace.name)
                                        }
                                    },
                                ),
                        }
                    } else {
                        let title = crate::bar::focused_bar_title(&self.snapshot, self.output_for_bar(id));
                        if title.trim().is_empty() {
                            self.clock
                                .split_once(", ")
                                .map_or(self.clock.as_str(), |(date, _)| date)
                        } else {
                            title
                        }
                        .to_owned()
                    };
                    let kind = if mode == Mode::Toast {
                        status_ui::Menu::Notifications
                    } else if mode == Mode::Peek {
                        match peek {
                            Peek::Media => status_ui::Menu::Media,
                            Peek::Date => status_ui::Menu::Calendar,
                            Peek::Workspaces => status_ui::Menu::Overflow,
                        }
                    } else {
                        self.outputs
                            .iter()
                            .find(|entry| entry.bar == id)
                            .and_then(|entry| entry.runtime.hub.as_ref())
                            .map_or(status_ui::Menu::Media, |runtime| runtime.borrow().last_menu)
                    };
                    let label = text(label).size(13).class(theme::Text::Color(primary)).ellipsize(
                        cosmic::iced::widget::text::Ellipsize::End(
                            cosmic::iced::advanced::text::EllipsizeHeightLimit::Lines(1),
                        ),
                    );
                    let body: Element<'_, cosmic::Action<Message>> =
                        match mode {
                            Mode::Peek => {
                                let icon = match peek {
                                    Peek::Media => icons::MEDIA,
                                    Peek::Date => icons::CALENDAR,
                                    Peek::Workspaces => icons::OVERVIEW,
                                };
                                let leading: Element<'_, cosmic::Action<Message>> = if peek == Peek::Workspaces {
                                    self.hub_workspaces(id, opacity)
                                } else {
                                    icons::tinted(icon, 16, icon_muted).opacity(opacity).into()
                                };
                                let mut strip = row![leading, label.width(Length::Fill)]
                                    .spacing(10)
                                    .align_y(Alignment::Center);
                                if peek == Peek::Media {
                                    strip =
                                        strip.push(
                                            if self.media.snapshot.selected.as_ref().is_some_and(|player| {
                                                player.status == ferese_ipc::media::Playback::Playing
                                            }) {
                                                self.hub_equalizer(id, opacity)
                                            } else {
                                                icons::tinted(icons::MEDIA_PAUSE, 16, icon_muted)
                                                    .opacity(opacity)
                                                    .into()
                                            },
                                        );
                                }
                                strip.into()
                            }
                            Mode::Toast => row![
                                icons::tinted(icons::NOTIFICATIONS, 16, icon_muted).opacity(opacity),
                                label.width(Length::Fill)
                            ]
                            .spacing(10)
                            .align_y(Alignment::Center)
                            .into(),
                            _ if self
                                .media
                                .snapshot
                                .selected
                                .as_ref()
                                .is_some_and(|player| player.status == ferese_ipc::media::Playback::Playing) =>
                            {
                                row![
                                    icons::tinted(icons::MEDIA, 16, crate::color(palette.accent)).opacity(opacity),
                                    label
                                ]
                                .spacing(8)
                                .align_y(Alignment::Center)
                                .into()
                            }
                            _ => label.into(),
                        };
                    let appearance = button::Style {
                        text_color: Some(primary),
                        icon_color: Some(primary),
                        ..Default::default()
                    };
                    let content_width = if mode == Mode::Idle {
                        Length::Shrink
                    } else {
                        Length::Fill
                    };
                    let content_height = if intrinsic { Length::Fixed(28.) } else { Length::Fill };
                    let context = button::custom(container(body).width(content_width).center_y(content_height))
                        .name("Expand panel hub")
                        .padding(0)
                        .width(content_width)
                        .height(content_height)
                        .class(theme::Button::Custom {
                            active: Box::new(move |_, _| appearance),
                            hovered: Box::new(move |_, _| appearance),
                            pressed: Box::new(move |_, _| appearance),
                            disabled: Box::new(move |_| appearance),
                        })
                        .on_press(cosmic::Action::App(Message::HubOpen(id, kind)));
                    let mut strip = row::with_capacity(2).spacing(10).align_y(Alignment::Center);
                    if mode == Mode::Idle && self.hub_workspace_width(id) > 0. {
                        strip = strip.push(self.hub_workspaces(id, opacity));
                    }
                    let group = container(strip.push(context).width(content_width).height(content_height))
                        .padding([0, 14])
                        .height(content_height);
                    if intrinsic {
                        group.into()
                    } else {
                        group.center_x(Length::Fill).into()
                    }
                }
                Mode::Expanded(selected) => {
                    let mut tabs = row::with_capacity(6)
                        .width(Length::Fill)
                        .spacing(2)
                        .align_y(Alignment::Center);
                    let shortcuts = self.hub_shortcuts(id);
                    let workspace_width = self.hub_workspace_width(id);
                    if workspace_width == 0. && shortcuts.contains(&ItemKind::Overview) {
                        tabs = tabs.push(self.hub_shortcut_button(id, ItemKind::Overview, menu, opacity));
                    }
                    for (kind, label, icon) in [
                        (status_ui::Menu::Media, "Music", icons::MEDIA),
                        (status_ui::Menu::Calendar, "Calendar", icons::CALENDAR),
                        (status_ui::Menu::Notifications, "Notifications", icons::NOTIFICATIONS),
                        (status_ui::Menu::Overflow, "Controls", icons::CONTROL_CENTER),
                    ] {
                        let active = if selected == Content::Controls {
                            menu.unwrap_or(status_ui::Menu::Overflow) == kind
                        } else {
                            selected == content(kind)
                        };
                        let unread = if kind == status_ui::Menu::Notifications {
                            self.notifications.unread()
                        } else {
                            0
                        };
                        let foreground = if unread > 0 {
                            crate::color(palette.accent)
                        } else {
                            icon_primary
                        };
                        let accessible_name = if unread > 0 {
                            format!("{label}, {unread} unread")
                        } else {
                            label.to_owned()
                        };
                        tabs = tabs.push(motion::button(
                            button::custom(icons::tinted(icon, 18, foreground).opacity(opacity))
                                .name(accessible_name)
                                .padding([5, 3])
                                .width(30)
                                .height(28)
                                .on_press(cosmic::Action::App(Message::HubOpen(id, kind))),
                            primary,
                            active,
                            opacity,
                        ));
                    }
                    for shortcut in shortcuts.into_iter().filter(|kind| *kind != ItemKind::Overview) {
                        tabs = tabs.push(self.hub_shortcut_button(id, shortcut, menu, opacity));
                    }
                    let navigation = scrollable(tabs.width(Length::Shrink))
                        .direction(cosmic::iced::widget::scrollable::Direction::Horizontal(
                            cosmic::iced::widget::scrollable::Scrollbar::default()
                                .width(0)
                                .scroller_width(0),
                        ))
                        .width(Length::Fill);
                    let mut tabs = row::with_capacity(3).spacing(2).align_y(Alignment::Center);
                    if workspace_width > 0. {
                        tabs = tabs.push(self.hub_workspaces(id, opacity));
                    }
                    let mut tabs = tabs.push(navigation);
                    tabs = tabs.push(motion::button(
                        button::custom(icons::tinted(icons::CLOSE, 18, icon_primary).opacity(opacity))
                            .name("Collapse panel hub")
                            .padding([5, 3])
                            .width(30)
                            .height(28)
                            .on_press(cosmic::Action::App(Message::HubClose(id))),
                        primary,
                        false,
                        opacity,
                    ));
                    let kind = match selected {
                        Content::Media => status_ui::Menu::Media,
                        Content::Calendar => status_ui::Menu::Calendar,
                        Content::Notifications => status_ui::Menu::Notifications,
                        Content::Controls => menu.unwrap_or(status_ui::Menu::Overflow),
                    };
                    let body = if kind == status_ui::Menu::Overflow {
                        self.hub_controls(id, opacity)
                    } else {
                        self.view_hub_menu(kind, opacity)
                    };
                    column![
                        container(tabs).height(32),
                        scrollable(body)
                            .direction(cosmic::iced::widget::scrollable::Direction::Vertical(
                                cosmic::iced::widget::scrollable::Scrollbar::default()
                                    .width(0)
                                    .scroller_width(0)
                            ))
                            .height(Length::Fill)
                    ]
                    .spacing(8)
                    .padding([8, 14])
                    .width(Length::Fill)
                    .height(Length::Fill)
                    .into()
                }
            };
        let content = if matches!(mode, Mode::Idle | Mode::Peek) {
            cosmic::widget::mouse_area(content)
                .on_scroll(move |delta| cosmic::Action::App(Message::HubWorkspaceScroll(id, delta)))
                .into()
        } else {
            content
        };
        if interactive && opacity >= 0.5 {
            content
        } else {
            content.map(|_| cosmic::Action::App(Message::HubIgnore))
        }
    }

    fn hub_equalizer(&self, id: window::Id, opacity: f32) -> Element<'_, cosmic::Action<Message>> {
        let playing = self
            .media
            .snapshot
            .selected
            .as_ref()
            .is_some_and(|player| player.status == ferese_ipc::media::Playback::Playing);
        let animate = playing && motion::settings().motion_enabled();
        let started = self
            .outputs
            .iter()
            .find(|entry| entry.bar == id)
            .and_then(|entry| entry.runtime.hub.as_ref())
            .map_or_else(Instant::now, |runtime| runtime.borrow().revision);
        equalizer::view(started, animate, color_with_opacity(self.config.theme.accent, opacity))
    }

    fn hub_shortcut_button(
        &self,
        id: window::Id,
        kind: ItemKind,
        menu: Option<status_ui::Menu>,
        opacity: f32,
    ) -> Element<'_, cosmic::Action<Message>> {
        let action = self.hub_control_action(id, kind).expect("shortcut action");
        let primary = crate::color(self.config.theme.for_bar().text_primary);
        motion::button(
            button::custom(icons::tinted(action.icon, 18, primary).opacity(opacity))
                .name(action.label)
                .padding([5, 3])
                .width(30)
                .height(28)
                .on_press_maybe(action.message.map(cosmic::Action::App)),
            primary.scale_alpha(opacity),
            action.menu.is_some() && action.menu == menu,
            opacity,
        )
    }

    fn hub_control_action(&self, id: window::Id, kind: ItemKind) -> Option<ControlAction> {
        let (icon, menu) = match kind {
            ItemKind::Overview => (icons::OVERVIEW, None),
            ItemKind::QuickSettings => (icons::SETTINGS, Some(status_ui::Menu::System)),
            ItemKind::Network => (icons::WIFI, Some(status_ui::Menu::Network)),
            ItemKind::Recording => (icons::RECORD, None),
            ItemKind::Audio => (icons::VOLUME, Some(status_ui::Menu::Audio)),
            ItemKind::Battery { .. } => (icons::BATTERY, Some(status_ui::Menu::Battery)),
            ItemKind::DisplayMode => (icons::DISPLAY, None),
            _ => return None,
        };
        let message = match kind {
            ItemKind::Overview => Some(Message::ToggleOverview),
            ItemKind::Recording => match self.recorder.state {
                crate::recording::State::Selecting | crate::recording::State::Recording(_) => {
                    Some(Message::StopRecording)
                }
                crate::recording::State::Saving => None,
                _ => Some(Message::StartRecording),
            },
            ItemKind::DisplayMode => Some(Message::OpenDisplays(
                self.outputs
                    .iter()
                    .find(|entry| entry.bar == id)
                    .and_then(|entry| entry.name.clone()),
            )),
            _ => menu.map(|menu| Message::HubOpen(id, menu)),
        };
        Some(ControlAction {
            icon,
            menu,
            message,
            label: if kind == ItemKind::Recording {
                self.recorder.label()
            } else {
                kind.label()
            },
        })
    }

    fn hub_shortcuts(&self, id: window::Id) -> Vec<ItemKind> {
        let panel = self.panel_for_bar(id);
        let available = self
            .outputs
            .iter()
            .find(|entry| entry.bar == id)
            .and_then(|entry| entry.size)
            .map_or(panel.morph.expanded_width, |(width, _)| {
                (f64::from(width) - 2. * f64::from(panel.resolved_geometry().side_margins)).max(0.)
            });
        let width = panel.morph.expanded_width.min(available);
        // Four tabs and Close keep their space even while the spring changes width.
        let workspace_width = self.hub_workspace_width(id);
        let capacity = ((width - 28. - workspace_width - 5. * 32. - 8.).max(0.) / 32.).floor() as usize;
        let mut shortcuts = Vec::new();
        for item in [&panel.start, &panel.center, &panel.end]
            .into_iter()
            .flat_map(|zone| &zone.groups)
            .flat_map(|group| &group.items)
            .filter(|item| item.visible && item.overflow != ferese_panel::OverflowPolicy::Always)
        {
            if shortcuts.len() == capacity {
                break;
            }
            if workspace_width > 0. && item.kind == ItemKind::Overview {
                continue;
            }
            if matches!(
                item.kind,
                ItemKind::Network | ItemKind::Recording | ItemKind::Audio | ItemKind::Battery { .. }
            ) {
                continue;
            }
            if self.hub_control_action(id, item.kind).is_some() && !shortcuts.contains(&item.kind) {
                shortcuts.push(item.kind);
            }
        }
        shortcuts
    }

    fn hub_controls(&self, id: window::Id, opacity: f32) -> Element<'_, cosmic::Action<Message>> {
        let primary = color_with_opacity(self.config.theme.text_primary, opacity);
        let mut rows = column::with_capacity(8).spacing(2).width(Length::Fill);
        let mut tiles = Vec::new();
        let panel = self.panel_for_bar(id);
        let shortcuts = self.hub_shortcuts(id);
        for item in [&panel.start, &panel.center, &panel.end]
            .into_iter()
            .flat_map(|zone| &zone.groups)
            .flat_map(|group| &group.items)
            .filter(|item| item.visible)
        {
            if shortcuts.contains(&item.kind) {
                continue;
            }
            if item.kind == ItemKind::Overview && self.hub_workspace_width(id) > 0. {
                continue;
            }
            let Some(action) = self.hub_control_action(id, item.kind) else {
                continue;
            };
            let ControlAction {
                icon, label, message, ..
            } = action;
            tiles.push(motion::button(
                button::custom(
                    row![
                        container(
                            icons::tinted(icon, 22, crate::color(self.config.theme.text_primary)).opacity(opacity)
                        )
                        .center_x(30)
                        .center_y(28),
                        text(label)
                            .size(12)
                            .class(theme::Text::Color(primary))
                            .height(28)
                            .width(Length::Fill)
                            .align_y(cosmic::iced::alignment::Vertical::Center)
                            .ellipsize(cosmic::iced::widget::text::Ellipsize::End(
                                cosmic::iced::advanced::text::EllipsizeHeightLimit::Lines(1),
                            ))
                    ]
                    .spacing(8)
                    .align_y(Alignment::Center),
                )
                .name(label)
                .padding([4, 8])
                .height(36)
                .width(Length::Fill)
                .on_press_maybe(message.map(cosmic::Action::App)),
                primary,
                false,
                opacity,
            ));
        }
        let mut tiles = tiles.into_iter();
        while let Some(left) = tiles.next() {
            let right = tiles
                .next()
                .unwrap_or_else(|| cosmic::widget::Space::new().width(Length::Fill).into());
            rows = rows.push(row![left, right].spacing(8));
        }
        rows.into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cosmic::Application;
    use cosmic::iced::advanced::{Layout, Shell, clipboard, layout, mouse, widget};
    use cosmic::iced::{Event, Rectangle, Size};
    use std::time::Duration;

    fn shell() -> FereseShell {
        let mut shell = crate::tests::shell_with_measured_panel();
        shell.config.panels[0].style = PanelStyle::Morph;
        shell.config.panels[0].border = true;
        shell.outputs[0]
            .runtime
            .reset(&shell.config.panels[0], &shell.snapshot, Some("test"));
        shell.clock = "11 oct, 10:09 am".into();
        shell
    }

    #[test]
    fn workspace_strip_is_output_local_and_does_not_interrupt_open_content() {
        let mut shell = shell();
        let id = shell.outputs[0].bar;
        shell.snapshot.outputs = vec![crate::control::OutputSnapshot {
            id: 4,
            name: "test".into(),
            active_workspace: 9,
            focused: true,
        }];
        shell.snapshot.workspaces = [(9, 4, 0), (10, 7, 10), (11, 4, 0)]
            .into_iter()
            .map(|(workspace, output, count)| crate::control::WorkspaceSnapshot {
                id: workspace,
                name: workspace.to_string(),
                output: Some(output),
                index: workspace as u32,
                window_count: count,
                visible: workspace == 9,
                focused: workspace == 9,
            })
            .collect();
        // A bar can be created after the initial compositor snapshot arrives.
        drop(shell.update(Message::PanelScale(id, 1.25)));
        let width = shell.hub_workspace_width(id);
        assert!(width > 0., "the active empty workspace remains visible");
        shell.snapshot.workspaces[1].window_count = 20;
        drop(shell.update_workspace_ui());
        assert_eq!(
            shell.hub_workspace_width(id),
            width,
            "another output cannot change this strip"
        );
        {
            let mut runtime = shell.outputs[0].runtime.hub.as_ref().unwrap().borrow_mut();
            runtime.hub.advance(Duration::ZERO, true);
            assert_eq!(
                runtime.hub.target_size()[0],
                shell.config.panels[0].morph.idle_width + width + 10.
            );
            assert_eq!(runtime.hub.sample().opacity, 1.);
        }
        drop(shell.hub_hover(id, true));
        {
            let runtime = shell.outputs[0].runtime.hub.as_ref().unwrap().borrow();
            assert_eq!(runtime.hub.target_size()[0], runtime.peek_width);
        }
        drop(shell.open_hub(id, status_ui::Menu::Calendar));
        let before = shell.outputs[0].runtime.hub.as_ref().unwrap().borrow().hub.sample();
        shell.snapshot.workspaces[2].window_count = 1;
        drop(shell.update_workspace_ui());
        assert!(shell.hub_workspace_width(id) > width);
        assert_eq!(
            shell.outputs[0].runtime.hub.as_ref().unwrap().borrow().hub.sample(),
            before
        );
        assert_eq!(shell.hub_mode(id), Some(Mode::Expanded(Content::Calendar)));
        drop(shell.open_hub_overview(id));
        assert!(shell.overview_active);
        assert_eq!(shell.hub_mode(id), Some(Mode::Idle));
    }

    #[test]
    fn workspace_feedback_updates_in_place_and_restores_hover_content() {
        let mut shell = shell();
        let id = shell.outputs[0].bar;
        shell.snapshot.outputs = vec![crate::control::OutputSnapshot {
            id: 4,
            name: "test".into(),
            active_workspace: 1,
            focused: true,
        }];
        shell.snapshot.workspaces = (1..=3)
            .map(|id| crate::control::WorkspaceSnapshot {
                id,
                name: id.to_string(),
                output: Some(4),
                index: id as u32,
                window_count: 1,
                visible: id == 1,
                focused: id == 1,
            })
            .collect();
        drop(shell.sync_hub_workspace_feedback(false));
        assert_eq!(shell.hub_mode(id), Some(Mode::Idle), "initial snapshot must not flash");
        drop(shell.hub_hover(id, true));
        shell.snapshot.outputs[0].active_workspace = 2;
        drop(shell.sync_hub_workspace_feedback(false));
        let first = {
            let mut runtime = shell.outputs[0].runtime.hub.as_ref().unwrap().borrow_mut();
            assert_eq!(runtime.peek, Peek::Workspaces);
            runtime.hub.advance(Duration::ZERO, true);
            runtime.workspace_feedback.unwrap()
        };
        shell.snapshot.outputs[0].active_workspace = 3;
        drop(shell.sync_hub_workspace_feedback(false));
        let latest = {
            let runtime = shell.outputs[0].runtime.hub.as_ref().unwrap().borrow();
            assert_eq!(
                runtime.hub.sample().opacity,
                1.,
                "repeated switches must not restart the fade"
            );
            runtime.workspace_feedback.unwrap()
        };
        assert_ne!(first, latest);
        drop(shell.hub_workspace_feedback_expired(id, first));
        assert_eq!(
            shell.outputs[0].runtime.hub.as_ref().unwrap().borrow().peek,
            Peek::Workspaces
        );
        drop(shell.hub_workspace_feedback_expired(id, latest));
        assert_eq!(shell.hub_mode(id), Some(Mode::Peek));
        assert_eq!(shell.outputs[0].runtime.hub.as_ref().unwrap().borrow().peek, Peek::Date);
        drop(shell.hub_hover(id, false));
        shell.snapshot.outputs[0].active_workspace = 1;
        drop(shell.sync_hub_workspace_feedback(false));
        let revision = shell.outputs[0]
            .runtime
            .hub
            .as_ref()
            .unwrap()
            .borrow()
            .workspace_feedback
            .unwrap();
        drop(shell.hub_workspace_feedback_expired(id, revision));
        assert_eq!(
            shell.hub_mode(id),
            Some(Mode::Idle),
            "keyboard feedback returns to idle"
        );
        shell.snapshot.outputs[0].active_workspace = 2;
        drop(shell.sync_hub_workspace_feedback(true));
        assert_eq!(
            shell.hub_mode(id),
            Some(Mode::Idle),
            "overview selection must not flash"
        );
        drop(shell.hub_hover(id, true));
        shell.snapshot.outputs[0].active_workspace = 3;
        drop(shell.sync_hub_workspace_feedback(false));
        drop(shell.hub_hover(id, false));
        assert_eq!(
            shell.outputs[0].runtime.hub.as_ref().unwrap().borrow().peek,
            Peek::Date,
            "leaving early must clear temporary workspace content"
        );
    }

    #[test]
    fn native_pointer_reentry_always_peeks_without_resizing_the_surface() {
        let mut shell = shell();
        let id = shell.outputs[0].bar;
        let allocation = shell.outputs[0].runtime.hub.as_ref().unwrap().borrow().allocation;
        for _ in 0..20 {
            drop(shell.handle_event(Event::Mouse(mouse::Event::CursorEntered), id));
            assert_eq!(shell.hub_mode(id), Some(Mode::Peek));
            drop(shell.handle_event(Event::Mouse(mouse::Event::CursorLeft), id));
            assert_eq!(shell.hub_mode(id), Some(Mode::Idle));
            let mut runtime = shell.outputs[0].runtime.hub.as_ref().unwrap().borrow_mut();
            runtime.hub.advance(Duration::from_secs(1), false);
            assert_eq!(runtime.allocation, allocation);
        }
        drop(shell.open_hub(id, status_ui::Menu::Calendar));
        drop(shell.handle_event(Event::Mouse(mouse::Event::CursorLeft), id));
        drop(shell.handle_event(Event::Mouse(mouse::Event::CursorEntered), id));
        assert_eq!(shell.hub_mode(id), Some(Mode::Expanded(Content::Calendar)));
    }

    #[test]
    fn hover_only_peeks_and_respects_preference_and_visibility() {
        let mut shell = shell();
        let id = shell.outputs[0].bar;
        drop(shell.hub_hover(id, true));
        assert_eq!(shell.hub_mode(id), Some(Mode::Peek));
        drop(shell.hub_hover(id, true));
        assert_eq!(shell.hub_mode(id), Some(Mode::Peek));
        drop(shell.hub_hover(id, false));
        assert_eq!(shell.hub_mode(id), Some(Mode::Idle));
        drop(shell.set_hub_mode(id, Mode::Toast, None));
        drop(shell.hub_hover(id, true));
        drop(shell.hub_hover(id, false));
        assert_eq!(shell.hub_mode(id), Some(Mode::Toast));
        drop(shell.close_hub(id));
        let mut config = shell.config.clone();
        config.panels[0].morph.peek_on_hover = false;
        drop(shell.apply_config(config));
        drop(shell.hub_hover(id, true));
        assert_eq!(shell.hub_mode(id), Some(Mode::Idle));
        let mut config = shell.config.clone();
        config.panels[0].morph.peek_on_hover = true;
        drop(shell.apply_config(config));
        shell.snapshot.outputs.push(crate::control::OutputSnapshot {
            id: 1,
            name: "test".into(),
            active_workspace: 7,
            focused: true,
        });
        shell.snapshot.windows.push(crate::control::WindowSnapshot {
            id: 42,
            workspace: 7,
            app_id: "test".into(),
            title: "Test".into(),
            focused: true,
            urgent: false,
            fullscreen: true,
            maximized: false,
            floating: false,
        });
        drop(shell.hub_hover(id, true));
        assert_eq!(shell.hub_mode(id), Some(Mode::Idle));
    }

    #[test]
    fn peek_category_and_width_stay_fixed_until_the_next_hover() {
        let mut shell = shell();
        let id = shell.outputs[0].bar;
        drop(shell.hub_hover(id, true));
        assert_eq!(shell.outputs[0].runtime.hub.as_ref().unwrap().borrow().peek, Peek::Date);
        assert_eq!(
            shell.outputs[0]
                .runtime
                .hub
                .as_ref()
                .unwrap()
                .borrow()
                .hub
                .target_size(),
            [200., 44.]
        );
        std::sync::Arc::make_mut(&mut shell.media.snapshot).selected = Some(ferese_ipc::media::Player {
            title: "A long track title that needs the wider preview".into(),
            status: ferese_ipc::media::Playback::Paused,
            ..Default::default()
        });
        drop(shell.hub_hover(id, true));
        assert_eq!(shell.outputs[0].runtime.hub.as_ref().unwrap().borrow().peek, Peek::Date);
        drop(shell.hub_hover(id, false));
        drop(shell.hub_hover(id, true));
        let size = shell.outputs[0]
            .runtime
            .hub
            .as_ref()
            .unwrap()
            .borrow()
            .hub
            .target_size();
        assert_eq!(
            shell.outputs[0].runtime.hub.as_ref().unwrap().borrow().peek,
            Peek::Media
        );
        assert!((200. ..=280.).contains(&size[0]));
        assert_eq!(size[1], 44.);
        std::sync::Arc::make_mut(&mut shell.media.snapshot).selected = None;
        drop(shell.hub_hover(id, true));
        assert_eq!(
            shell.outputs[0].runtime.hub.as_ref().unwrap().borrow().peek,
            Peek::Media
        );
        assert_eq!(
            shell.outputs[0]
                .runtime
                .hub
                .as_ref()
                .unwrap()
                .borrow()
                .hub
                .target_size(),
            size
        );
        drop(shell.hub_hover(id, false));
        drop(shell.hub_hover(id, true));
        assert_eq!(shell.outputs[0].runtime.hub.as_ref().unwrap().borrow().peek, Peek::Date);
    }

    #[test]
    fn collapse_remembers_the_last_selected_panel() {
        let mut shell = shell();
        let id = shell.outputs[0].bar;
        assert_eq!(
            shell.outputs[0].runtime.hub.as_ref().unwrap().borrow().last_menu,
            status_ui::Menu::Media
        );
        drop(shell.open_hub(id, status_ui::Menu::Calendar));
        drop(shell.close_hub(id));
        drop(shell.hub_hover(id, true));
        assert_eq!(
            shell.outputs[0].runtime.hub.as_ref().unwrap().borrow().last_menu,
            status_ui::Menu::Calendar
        );
    }

    #[test]
    fn header_shortcuts_fit_the_settled_width_and_respect_item_visibility() {
        let mut shell = shell();
        let id = shell.outputs[0].bar;
        for (width, count) in [(240., 1), (340., 3), (400., 3)] {
            shell.config.panels[0].morph.expanded_width = width;
            let shortcuts = shell.hub_shortcuts(id);
            assert_eq!(shortcuts.len(), count);
            assert!(!shortcuts.contains(&ItemKind::Network));
            assert!(!shortcuts.contains(&ItemKind::Recording));
            assert!(!shortcuts.contains(&ItemKind::Audio));
            assert!(!shortcuts.iter().any(|kind| matches!(kind, ItemKind::Battery { .. })));
        }
        let before = shell.hub_shortcuts(id);
        drop(shell.open_hub(id, status_ui::Menu::Calendar));
        assert_eq!(
            shell.hub_shortcuts(id),
            before,
            "shortcuts must not shuffle during the spring"
        );
        let panel = &mut shell.config.panels[0];
        for item in [&mut panel.start, &mut panel.center, &mut panel.end]
            .into_iter()
            .flat_map(|zone| &mut zone.groups)
            .flat_map(|group| &mut group.items)
        {
            if item.kind == ItemKind::Overview {
                item.visible = false;
            }
            if item.kind == ItemKind::DisplayMode {
                item.overflow = ferese_panel::OverflowPolicy::Always;
            }
        }
        let shortcuts = shell.hub_shortcuts(id);
        assert!(!shortcuts.contains(&ItemKind::Overview));
        assert!(!shortcuts.contains(&ItemKind::DisplayMode));
        shell.outputs[0].size = Some((200, 600));
        assert!(
            shell.hub_shortcuts(id).is_empty(),
            "small outputs must keep optional controls in the grid"
        );
    }

    #[test]
    fn opening_and_switching_panels_do_not_resize_again_after_settlement() {
        let panel = Panel::from_preset(PanelStyle::Morph);
        let mut runtime = Runtime::new(&panel);
        for menu in [
            status_ui::Menu::Media,
            status_ui::Menu::Calendar,
            status_ui::Menu::Notifications,
            status_ui::Menu::Overflow,
        ] {
            runtime.retarget(Mode::Expanded(content(menu)), [1000., 700.], Some(menu));
            let allocation = runtime.allocation;
            for _ in 0..200 {
                runtime.hub.advance(Duration::from_millis(8), false);
            }
            assert!(!runtime.hub.active());
            assert_eq!(runtime.allocation, allocation);
        }
    }

    #[test]
    fn opening_closing_and_reopening_retain_the_native_surface() {
        let panel = Panel::from_preset(PanelStyle::Morph);
        let mut runtime = Runtime::new(&panel);
        assert_eq!(runtime.allocation, [286, 49]);
        runtime.retarget(
            Mode::Expanded(Content::Calendar),
            [1000., 700.],
            Some(status_ui::Menu::Calendar),
        );
        let expanded_allocation = runtime.allocation;
        assert!(expanded_allocation[0] > 340 && expanded_allocation[1] > 230);
        runtime.hub.advance(Duration::from_millis(50), false);
        let before = runtime.hub.sample().geometry;
        runtime.retarget(Mode::Idle, [1000., 700.], None);
        // A reversal never commits a smaller buffer underneath an unfinished shape.
        assert_eq!(runtime.allocation, expanded_allocation);
        assert!(runtime.hub.sample().geometry.bounds.width >= before.bounds.width);
        runtime.hub.advance(Duration::from_secs(2), false);
        runtime.hub.advance(Duration::from_millis(120), false);
        assert_eq!(runtime.allocation, expanded_allocation);
        assert!(!runtime.hub.active());
        runtime.retarget(
            Mode::Expanded(Content::Calendar),
            [1000., 700.],
            Some(status_ui::Menu::Calendar),
        );
        assert_eq!(runtime.allocation, expanded_allocation);
    }

    #[test]
    fn hover_leave_cannot_close_an_expanded_hub_and_reload_clears_its_owner() {
        let mut shell = shell();
        let id = shell.outputs[0].bar;
        let reservation = shell.config.panels[0].exclusive_zone(false);
        drop(shell.hub_hover(id, true));
        assert_eq!(shell.hub_mode(id), Some(Mode::Peek));
        drop(shell.hub_hover(id, false));
        assert_eq!(shell.hub_mode(id), Some(Mode::Idle));
        drop(shell.update(Message::HubOpen(id, status_ui::Menu::Calendar)));
        assert!(shell.menu.is_none(), "expanded hub owns no popup surface");
        drop(shell.hub_hover(id, false));
        assert_eq!(shell.hub_mode(id), Some(Mode::Expanded(Content::Calendar)));
        assert_eq!(shell.config.panels[0].exclusive_zone(false), reservation);
        drop(shell.update(Message::HubClose(id)));
        assert_eq!(shell.hub_mode(id), Some(Mode::Idle));
        let mut config = shell.config.clone();
        config.panels[0].style = PanelStyle::Floating;
        drop(shell.apply_config(config));
        assert_eq!(shell.hub_mode(id), None);
        assert_eq!(shell.config.panels[0].style, PanelStyle::Floating);
    }

    #[test]
    fn fullscreen_collapses_immediately_and_old_toast_deadlines_do_not_close_new_content() {
        let mut shell = shell();
        let id = shell.outputs[0].bar;
        drop(shell.set_hub_mode(id, Mode::Toast, None));
        let old = shell.outputs[0].runtime.hub.as_ref().unwrap().borrow().revision;
        drop(shell.open_hub(id, status_ui::Menu::Calendar));
        drop(shell.hub_toast_expired(id, old));
        assert_eq!(shell.hub_mode(id), Some(Mode::Expanded(Content::Calendar)));
        drop(shell.suspend_hub(id));
        let runtime = shell.outputs[0].runtime.hub.as_ref().unwrap().borrow();
        assert_eq!(runtime.hub.mode(), Mode::Idle);
        assert_eq!(runtime.allocation, [286, 49]);
        assert!(!runtime.hub.active());
    }

    #[test]
    fn cursor_leave_closes_peek_and_escape_closes_expanded_content() {
        use cosmic::iced::keyboard::{Event as KeyEvent, Key, Location, Modifiers, key};
        let mut shell = shell();
        let id = shell.outputs[0].bar;
        drop(shell.hub_hover(id, true));
        drop(shell.handle_event(Event::Mouse(cosmic::iced::mouse::Event::CursorLeft), id));
        assert_eq!(shell.hub_mode(id), Some(Mode::Idle));
        drop(shell.open_hub(id, status_ui::Menu::Calendar));
        drop(shell.handle_event(Event::Mouse(cosmic::iced::mouse::Event::CursorLeft), id));
        assert_eq!(shell.hub_mode(id), Some(Mode::Expanded(Content::Calendar)));
        drop(shell.handle_event(
            Event::Keyboard(KeyEvent::KeyPressed {
                key: Key::Named(key::Named::Escape),
                modified_key: Key::Named(key::Named::Escape),
                physical_key: key::Physical::Code(key::Code::Escape),
                location: Location::Standard,
                modifiers: Modifiers::empty(),
                text: None,
                repeat: false,
            }),
            id,
        ));
        assert_eq!(shell.hub_mode(id), Some(Mode::Idle));
    }

    #[test]
    #[ignore = "writes Morph review captures to /tmp/ferese-hub-review"]
    fn render_hub_review_states() {
        use cosmic::iced::advanced::renderer::{Headless, Renderer as _};
        let backend = std::env::var("FERESE_REVIEW_BACKEND").unwrap_or_else(|_| "wgpu".into());
        let mut renderer = cosmic::iced::futures::executor::block_on(cosmic::Renderer::new(
            cosmic::iced::Font::default(),
            cosmic::iced::Pixels(14.),
            Some(&backend),
        ))
        .unwrap();
        std::fs::create_dir_all("/tmp/ferese-hub-review").unwrap();
        let mut shell = shell();
        let id = shell.outputs[0].bar;
        std::sync::Arc::make_mut(&mut shell.media.snapshot).selected = Some(ferese_ipc::media::Player {
            title: "Losing My Mind".into(),
            artist: "London Grammar".into(),
            status: ferese_ipc::media::Playback::Playing,
            length_us: Some(240_000_000),
            position_us: Some(80_000_000),
            sampled_at_us: ferese_ipc::media::now_us(),
            can_control: true,
            can_pause: true,
            can_next: true,
            can_previous: true,
            can_seek: true,
            track_id: Some("/track/1".into()),
            ..Default::default()
        });
        shell.snapshot.outputs = vec![crate::control::OutputSnapshot {
            id: 4,
            name: "test".into(),
            active_workspace: 1,
            focused: true,
        }];
        shell.snapshot.workspaces = (1..=5)
            .map(|id| crate::control::WorkspaceSnapshot {
                id,
                name: format!("Desk {id}"),
                output: Some(4),
                index: id as u32,
                window_count: (id % 3) as u32,
                visible: id == 1,
                focused: id == 1,
            })
            .collect();
        drop(shell.update_workspace_ui());
        let theme = shell.config.theme.palette().native_theme();
        for scale in [1., 1.25, 1.5] {
            shell.outputs[0].scale = scale;
            let mut date_pixels = None;
            for (name, mode, menu) in [
                ("idle", Mode::Idle, None),
                ("peek-date", Mode::Peek, None),
                ("peek-date-hover", Mode::Peek, None),
                ("peek-date-pressed", Mode::Peek, None),
                ("peek-media", Mode::Peek, None),
                ("peek-workspaces", Mode::Peek, None),
                ("music", Mode::Expanded(Content::Media), Some(status_ui::Menu::Media)),
                (
                    "wifi-connected",
                    Mode::Expanded(Content::Controls),
                    Some(status_ui::Menu::Network),
                ),
                (
                    "wifi-off",
                    Mode::Expanded(Content::Controls),
                    Some(status_ui::Menu::Network),
                ),
                (
                    "battery",
                    Mode::Expanded(Content::Controls),
                    Some(status_ui::Menu::Battery),
                ),
                (
                    "calendar",
                    Mode::Expanded(Content::Calendar),
                    Some(status_ui::Menu::Calendar),
                ),
                (
                    "alerts",
                    Mode::Expanded(Content::Notifications),
                    Some(status_ui::Menu::Notifications),
                ),
                (
                    "controls",
                    Mode::Expanded(Content::Controls),
                    Some(status_ui::Menu::Overflow),
                ),
                (
                    "controls-narrow",
                    Mode::Expanded(Content::Controls),
                    Some(status_ui::Menu::Overflow),
                ),
                (
                    "controls-wide",
                    Mode::Expanded(Content::Controls),
                    Some(status_ui::Menu::Overflow),
                ),
                (
                    "system",
                    Mode::Expanded(Content::Controls),
                    Some(status_ui::Menu::System),
                ),
                (
                    "spring-60",
                    Mode::Expanded(Content::Calendar),
                    Some(status_ui::Menu::Calendar),
                ),
                (
                    "spring-120",
                    Mode::Expanded(Content::Calendar),
                    Some(status_ui::Menu::Calendar),
                ),
                (
                    "spring-190",
                    Mode::Expanded(Content::Calendar),
                    Some(status_ui::Menu::Calendar),
                ),
                (
                    "spring-250",
                    Mode::Expanded(Content::Calendar),
                    Some(status_ui::Menu::Calendar),
                ),
                (
                    "spring-400",
                    Mode::Expanded(Content::Calendar),
                    Some(status_ui::Menu::Calendar),
                ),
                (
                    "spring-900",
                    Mode::Expanded(Content::Calendar),
                    Some(status_ui::Menu::Calendar),
                ),
            ] {
                let panel = &mut shell.config.panels[0];
                panel.morph.expanded_width = match name {
                    "controls-narrow" => 240.,
                    "controls-wide" => 400.,
                    _ => 340.,
                };
                if name.starts_with("wifi-") {
                    shell.status.network = Some(crate::status::Network {
                        enabled: name == "wifi-connected",
                        connection: (name == "wifi-connected").then(|| "Home network".into()),
                        signal: 80,
                    });
                }
                let workspace_width = shell.hub_workspace_width(id);
                let hub = shell.outputs[0].runtime.hub.as_ref().unwrap();
                let (mut sample, allocation) = {
                    let mut hub = hub.borrow_mut();
                    let animation_time = name.strip_prefix("spring-").map(|ms| ms.parse::<u64>().unwrap());
                    *hub = Runtime::new(&shell.config.panels[0]);
                    hub.workspace_width = if workspace_width > 0. {
                        workspace_width + 10.
                    } else {
                        0.
                    };
                    hub.peek = match name {
                        "peek-media" => Peek::Media,
                        "peek-workspaces" => Peek::Workspaces,
                        _ => Peek::Date,
                    };
                    hub.peek_width = if name == "peek-media" { 260. } else { 200. };
                    hub.retarget(mode, [1200., 800.], menu);
                    hub.hub.advance(
                        Duration::from_millis(animation_time.unwrap_or(0)),
                        animation_time.is_none(),
                    );
                    (hub.hub.sample(), hub.allocation)
                };
                if mode == Mode::Idle || mode == Mode::Expanded(Content::Controls) {
                    let mut measured = shell.view_hub(id);
                    let mut measured_tree = widget::Tree::new(&measured);
                    measured.as_widget_mut().layout(
                        &mut measured_tree,
                        &renderer,
                        &layout::Limits::new(Size::ZERO, Size::new(allocation[0] as f32, allocation[1] as f32)),
                    );
                    let mut runtime = hub.borrow_mut();
                    runtime.hub.advance(Duration::ZERO, true);
                    sample = runtime.hub.sample();
                    if mode == Mode::Idle {
                        assert!(
                            sample.geometry.bounds.width
                                < shell.config.panels[0].morph.idle_width + workspace_width + 10.,
                            "idle should fit its content instead of retaining a fixed text allowance"
                        );
                    }
                    if name == "controls" || name.starts_with("wifi-") {
                        assert!(
                            sample.geometry.bounds.height < shell.config.panels[0].morph.controls_height,
                            "{name} should shrink to its content"
                        );
                    }
                }
                let mut view = shell.view_hub_at(id, sample, menu, None);
                let mut tree = widget::Tree::new(&view);
                let viewport = Rectangle::with_size(Size::new(allocation[0] as f32, allocation[1] as f32));
                let node = view.as_widget_mut().layout(
                    &mut tree,
                    &renderer,
                    &layout::Limits::new(Size::ZERO, viewport.size()),
                );
                if name == "peek-date" || name == "peek-workspaces" {
                    let mut messages = Vec::new();
                    view.as_widget_mut().update(
                        &mut tree,
                        &Event::Mouse(mouse::Event::WheelScrolled {
                            delta: mouse::ScrollDelta::Lines { x: 0., y: -1. },
                        }),
                        Layout::new(&node),
                        mouse::Cursor::Available(cosmic::iced::Point::new(viewport.width / 2., 22.)),
                        &renderer,
                        &mut clipboard::Null,
                        &mut Shell::new(&mut messages),
                        &viewport,
                    );
                    assert_eq!(
                        messages
                            .iter()
                            .filter(|message| matches!(message, cosmic::Action::App(Message::HubWorkspaceScroll(_, _))))
                            .count(),
                        1,
                        "peek must handle a wheel event exactly once"
                    );
                }
                let cursor = if name == "peek-date-hover" || name == "peek-date-pressed" {
                    let position = cosmic::iced::Point::new(viewport.width / 2., viewport.height / 2.);
                    let cursor = mouse::Cursor::Available(position);
                    let mut messages = Vec::new();
                    let event = if name == "peek-date-pressed" {
                        Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left))
                    } else {
                        Event::Mouse(mouse::Event::CursorMoved { position })
                    };
                    view.as_widget_mut().update(
                        &mut tree,
                        &event,
                        Layout::new(&node),
                        cursor,
                        &renderer,
                        &mut clipboard::Null,
                        &mut Shell::new(&mut messages),
                        &viewport,
                    );
                    cursor
                } else {
                    mouse::Cursor::Unavailable
                };
                renderer.reset(viewport);
                view.as_widget().draw(
                    &tree,
                    &mut renderer,
                    &theme,
                    &cosmic::iced::advanced::renderer::Style {
                        text_color: crate::color(shell.config.theme.text_primary),
                        icon_color: crate::color(shell.config.theme.text_primary),
                        scale_factor: scale,
                    },
                    Layout::new(&node),
                    cursor,
                    &viewport,
                );
                let physical = Size::new(
                    (allocation[0] as f64 * scale).ceil() as u32,
                    (allocation[1] as f64 * scale).ceil() as u32,
                );
                let pixels = renderer.screenshot(physical, scale as f32, cosmic::iced::Color::TRANSPARENT);
                assert!(pixels.chunks_exact(4).any(|pixel| pixel[3] > 0));
                if name == "peek-date" {
                    date_pixels = Some(pixels.clone());
                } else if name == "peek-date-hover" || name == "peek-date-pressed" {
                    assert_eq!(
                        Some(&pixels),
                        date_pixels.as_ref(),
                        "Peek must not paint a hover or pressed overlay"
                    );
                }
                image_codec::save_buffer(
                    format!("/tmp/ferese-hub-review/{backend}-{name}-{scale}.png"),
                    &pixels,
                    physical.width,
                    physical.height,
                    image_codec::ColorType::Rgba8,
                )
                .unwrap();
            }
        }
    }

    #[test]
    fn native_input_follows_the_animated_hub_and_bottom_anchor_mirrors_it() {
        use cosmic::iced::advanced::renderer::Headless;
        let renderer = cosmic::iced::futures::executor::block_on(cosmic::Renderer::new(
            cosmic::iced::Font::default(),
            cosmic::iced::Pixels(14.),
            Some("tiny-skia"),
        ))
        .unwrap();
        let mut shell = shell();
        let id = shell.outputs[0].bar;
        drop(shell.open_hub(id, status_ui::Menu::Calendar));
        // Render the initial sample while the backing surface is already expanded.
        let hub = shell.outputs[0].runtime.hub.as_ref().unwrap();
        let sample = hub.borrow().hub.sample();
        for edge in [Edge::Top, Edge::Bottom] {
            shell.config.panels[0].edge = edge;
            let mut view = shell.view_hub_at(id, sample, Some(status_ui::Menu::Calendar), None);
            let mut tree = widget::Tree::new(&view);
            let viewport = Rectangle::with_size(Size::new(340., 230.));
            let mut node =
                view.as_widget_mut()
                    .layout(&mut tree, &renderer, &layout::Limits::new(Size::ZERO, viewport.size()));
            let mut messages = Vec::new();
            let event = Event::Window(window::Event::RedrawRequested(Instant::now()));
            for _ in 0..2 {
                let mut update = Shell::new(&mut messages);
                view.as_widget_mut().update(
                    &mut tree,
                    &event,
                    Layout::new(&node),
                    mouse::Cursor::Unavailable,
                    &renderer,
                    &mut clipboard::Null,
                    &mut update,
                    &viewport,
                );
                if update.is_layout_invalid() {
                    node = view.as_widget_mut().layout(
                        &mut tree,
                        &renderer,
                        &layout::Limits::new(Size::ZERO, viewport.size()),
                    );
                }
            }
            let scene = messages
                .iter()
                .find_map(|message| match message {
                    cosmic::Action::App(Message::PanelSurfaceChanged(_, scene)) => Some(scene),
                    _ => None,
                })
                .expect("native material and input scene");
            assert!(scene.contains([170., if edge == Edge::Top { 17. } else { 213. }]));
            assert!(!scene.contains([20., 17.]), "unused backing space is click-through");
            assert!(
                !scene.contains([170., 100.]),
                "target geometry must not steal input before it is visible"
            );
            assert_eq!(scene.damage.len(), 1);
            assert!(scene.damage[0].width <= 151.);
        }
    }

    #[test]
    fn each_animation_frame_publishes_its_measured_shape_without_event_replay() {
        use cosmic::iced::advanced::renderer::Headless;
        use cosmic::iced::runtime::UserInterface;
        let mut renderer = cosmic::iced::futures::executor::block_on(cosmic::Renderer::new(
            cosmic::iced::Font::default(),
            cosmic::iced::Pixels(14.),
            Some("tiny-skia"),
        ))
        .unwrap();
        let mut shell = shell();
        let id = shell.outputs[0].bar;
        drop(shell.open_hub(id, status_ui::Menu::Calendar));
        let start = Instant::now() + Duration::from_secs(1);
        shell.outputs[0].runtime.hub.as_ref().unwrap().borrow_mut().sampled = start;
        let mut ui = UserInterface::build(
            shell.view_layer(id),
            Size::new(340., 248.),
            Default::default(),
            &mut renderer,
        );
        let mut last_width = 0.;
        // Sample before the spring tail rounds to the same physical pixel.
        for frame in 1..=8 {
            let mut messages = Vec::new();
            ui.update(
                &[Event::Window(window::Event::RedrawRequested(
                    start + Duration::from_millis(frame * 8),
                ))],
                mouse::Cursor::Unavailable,
                &mut renderer,
                &mut clipboard::Null,
                &mut messages,
            );
            let scene = messages
                .iter()
                .find_map(|message| match message {
                    cosmic::Action::App(Message::PanelSurfaceChanged(_, scene)) => Some(scene),
                    _ => None,
                })
                .unwrap_or_else(|| panic!("frame {frame} must publish its freshly laid-out native shape"));
            let width = scene.paint[0].bounds.width;
            assert!(width > last_width, "frame {frame}: {width} must exceed {last_width}");
            last_width = width;
        }
    }
}
