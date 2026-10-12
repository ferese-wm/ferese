use std::cell::RefCell;
use std::time::Instant;

use crate::KeyboardInteractivity;
use cosmic::app::Task;
use cosmic::iced::platform_specific::shell::commands::layer_surface::{set_keyboard_interactivity, set_size};
use cosmic::iced::{Alignment, Length, window};
use cosmic::widget::{button, column, container, row, scrollable};

mod activity;
mod battery;
mod content_frame;
mod equalizer;
mod scroll;
mod transition;
use transition::{Event, Transient};
mod workspace_grid;
mod workspaces;
use cosmic::{Element, theme};
use ferese_panel::morph::{Content, Hub, Mode, Sample};
use ferese_panel::{Edge, ItemKind, Panel, PanelStyle};
use ferese_theme::icons;

use crate::{FereseShell, Message, color_with_opacity, motion, status_ui};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Peek {
    Recording,
    Battery,
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

#[derive(Clone, Copy)]
struct PreviousContent {
    menu: Option<status_ui::Menu>,
    peek: Peek,
    toast: Option<u32>,
    size: [f64; 2],
}

pub(super) struct Runtime {
    pub(super) hub: Hub,
    pub(super) menu: Option<status_ui::Menu>,
    previous: PreviousContent,
    transient: Transient,
    pub(super) allocation: [u32; 2],
    minimum_allocation: [u32; 2],
    fillet_space: u32,
    pub(super) revision: Instant,
    sampled: Instant,
    motion: motion::Settings,
    last_menu: status_ui::Menu,
    priority: Option<Peek>,
    peek: Peek,
    observed_workspace: Option<u64>,
    workspace_hover: Option<u64>,
    workspace_selected: Option<u64>,
    workspace_selection_pending: Option<u64>,
    hovered: bool,
    pub(crate) idle_content_width: Option<f64>,
    available: [f64; 2],
    peek_width: f64,
    idle_width: f64,
    workspace_width: f64,
    workspace_scroll: workspaces::Scroll,
    pub(super) activities: crate::activities::Presentation,
}

impl Runtime {
    pub(super) fn new(panel: &Panel) -> Self {
        let mut hub = Hub::new(panel.morph, panel.resolved_geometry().height.into()).expect("validated panel hub");
        hub.pin_radii(panel.corner_radius.map(|r| r.0.map(f64::from)))
            .expect("validated panel radii");
        hub.advance(std::time::Duration::ZERO, true);
        let now = Instant::now();
        // Keep the native surface stable through hover, including spring overshoot.
        let fillet_space = if panel.morph.attached { 24 } else { 0 };
        let minimum_allocation = [
            (panel.morph.peek_width.max(panel.morph.idle_width).max(200.) * 1.1).ceil() as u32 + fillet_space,
            (panel.morph.peek_height.max(f64::from(panel.resolved_geometry().height)) * 1.1).ceil() as u32,
        ];
        Self {
            allocation: minimum_allocation,
            minimum_allocation,
            fillet_space,
            hub,
            menu: None,
            previous: PreviousContent {
                menu: None,
                peek: Peek::Date,
                toast: None,
                size: [panel.morph.idle_width, f64::from(panel.resolved_geometry().height)],
            },
            transient: Transient::None,
            revision: now,
            sampled: now,
            motion: Default::default(),
            last_menu: status_ui::Menu::Workspaces,
            priority: None,
            peek: Peek::Date,
            observed_workspace: None,
            workspace_hover: None,
            workspace_selected: None,
            workspace_selection_pending: None,
            hovered: false,
            idle_content_width: None,
            available: [1280., 720.],
            peek_width: 200.,
            idle_width: panel.morph.idle_width,
            workspace_width: 0.,
            workspace_scroll: Default::default(),
            activities: Default::default(),
        }
    }

    fn sample(&mut self, now: Instant) -> Sample {
        let policy = self.motion;
        self.activities
            .advance(now.saturating_duration_since(self.sampled), !policy.motion_enabled());
        self.hub.advance(
            now.saturating_duration_since(self.sampled).mul_f64(policy.speed),
            !policy.motion_enabled(),
        );
        self.sampled = self.sampled.max(now);
        self.hub.sample()
    }

    pub(crate) fn refresh_activity_size(&mut self) {
        if self.activities.selection.visible.is_empty() {
            return;
        }
        if self.hub.mode() == Mode::Idle {
            let width = self
                .idle_content_width
                .map_or(self.idle_width + self.workspace_width, |width| {
                    width + self.workspace_width
                })
                .clamp(24., 1024.);
            let _ = self.hub.retarget_idle(width, self.available);
        } else if self.hub.mode() == Mode::Peek
            && self.peek != Peek::Workspaces
            && self.priority.is_none()
            && let Some(width) = self.idle_content_width
        {
            self.peek_width = width + self.workspace_width;
            let _ = self
                .hub
                .retarget_peek(self.peek_width.clamp(24., 1024.), self.available);
        }
    }

    fn retarget(&mut self, mode: Mode, available: [f64; 2], menu: Option<status_ui::Menu>) {
        let now = Instant::now();
        let shape_available = [(available[0] - f64::from(self.fillet_space)).max(24.), available[1]];
        self.available = shape_available;
        self.sample(now);
        self.menu = menu;
        if mode == Mode::Idle {
            self.hub.retarget_idle(
                self.idle_content_width
                    .map(|width| {
                        if self.activities.selection.visible.is_empty() {
                            width
                        } else {
                            width + self.workspace_width
                        }
                    })
                    .unwrap_or(self.idle_width + self.workspace_width)
                    .clamp(24., 1024.),
                shape_available,
            )
        } else if mode == Mode::Peek {
            self.hub.retarget_peek(self.peek_width.min(1024.), shape_available)
        } else {
            self.hub.retarget(mode, shape_available)
        }
        .expect("bounded hub area");
        let mut target = self.hub.allocation_size().map(|n| n.ceil() as u32);
        target[0] = target[0]
            .saturating_add(self.fillet_space)
            .min(available[0].ceil() as u32);
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
        status_ui::Menu::Activities => Content::Activities,
        status_ui::Menu::Workspaces => Content::Workspaces,
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
            let mut runtime = runtime.borrow_mut();
            if entered {
                runtime.activities.selection.enter();
            } else {
                runtime.activities.selection.leave();
            }
        }
        self.sync_activity_presentations();
        self.transition_hub(
            id,
            Event::Hover {
                entered,
                enabled: self.panel_for_bar(id).morph.peek_on_hover,
                peek: self.hub_peek_content(id),
                priority: self.hub_priority(),
            },
        )
    }

    pub(super) fn open_hub(&mut self, id: window::Id, kind: status_ui::Menu) -> Task<Message> {
        if self.bar_hidden(id) || self.hub_mode(id).is_none() {
            return Task::none();
        }
        if kind == status_ui::Menu::Notifications {
            for notice in &mut self.notifications.entries {
                notice.unread = false;
            }
            if let Some(status) = &mut self.status.notifications {
                status.count = 0;
            }
            self.sync_activity_presentations();
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
        let open = self.transition_hub(id, Event::Open(kind));
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
            || self.hub_priority().is_some()
            || !matches!(self.hub_mode(id), Some(Mode::Idle | Mode::Peek | Mode::Toast))
            || !self
                .notifications
                .popup_groups()
                .iter()
                .any(|(entry, _, _)| entry.id == notice)
        {
            return Task::none();
        }
        let revision = Instant::now();
        let open = self.transition_hub(
            id,
            Event::Toast {
                notice,
                token: revision,
            },
        );
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
                    && hub.transient.toast().is_some_and(|id| {
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
        self.transition_hub(id, Event::ToastExpired(revision))
    }

    pub(super) fn close_hub(&mut self, id: window::Id) -> Task<Message> {
        self.transition_hub(id, Event::Close)
    }

    #[cfg(test)]
    fn set_hub_mode(&mut self, id: window::Id, mode: Mode, menu: Option<status_ui::Menu>) -> Task<Message> {
        self.transition_hub(id, Event::Present(mode, menu))
    }

    fn transition_hub(&mut self, id: window::Id, event: Event) -> Task<Message> {
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
        let (resize, mode) = {
            let mut hub = hub.borrow_mut();
            hub.motion = self.render.motion;
            let before = hub.allocation;
            hub.workspace_width = if workspace_width > 0. {
                workspace_width + 10.
            } else {
                0.
            };
            if !hub.transition(event, available) {
                return Task::none();
            }
            ((before != hub.allocation).then_some(hub.allocation), hub.hub.mode())
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

    fn hub_idle_width_limit(&self, id: window::Id) -> f32 {
        let runtime = self
            .outputs
            .iter()
            .find(|entry| entry.bar == id)
            .and_then(|entry| entry.runtime.hub.as_ref())
            .expect("Morph runtime");
        let runtime = runtime.borrow();
        if !runtime.activities.selection.visible.is_empty() && self.activities_configured(id) {
            return runtime.hub.target_size()[0].min(runtime.available[0]) as f32;
        }
        let artwork_width = if self.hub_priority().is_some() {
            64.
        } else if self
            .media
            .snapshot
            .selected
            .as_ref()
            .is_some_and(|player| player.status == ferese_ipc::media::Playback::Playing)
        {
            32.
        } else {
            0.
        };
        (runtime.idle_width + artwork_width + runtime.workspace_width)
            .min(runtime.available[0])
            .min(f64::from(runtime.allocation[0].saturating_sub(runtime.fillet_space)) / 1.1) as f32
    }

    pub(super) fn view_hub(&self, id: window::Id) -> Element<'_, cosmic::Action<Message>> {
        let entry = self.outputs.iter().find(|entry| entry.bar == id).expect("panel output");
        let runtime = entry.runtime.hub.as_ref().expect("Morph runtime");
        runtime.borrow_mut().motion = self.render.motion;
        let revision = runtime.borrow().revision;
        let mode = runtime.borrow().hub.mode();
        let workspace_feedback = mode == Mode::Peek && runtime.borrow().peek == Peek::Workspaces;
        let measurement = if (mode == Mode::Idle && !self.has_compact_activities(id)) || workspace_feedback {
            let cap = if workspace_feedback {
                let runtime = runtime.borrow();
                260_f64
                    .min(runtime.available[0])
                    .min(f64::from(runtime.allocation[0].saturating_sub(runtime.fillet_space)) / 1.1)
                    as f32
            } else {
                self.hub_idle_width_limit(id)
            };
            Some(motion::ContentMeasurement {
                content: self.hub_content(id, mode, 1., None, false, true),
                width: cap,
                measured: Box::new(move |size| {
                    let mut runtime = runtime.borrow_mut();
                    let width = f64::from(size.width).ceil().clamp(24., 1024.);
                    let available = runtime.available;
                    if workspace_feedback {
                        runtime.peek_width = width;
                        runtime
                            .hub
                            .retarget_peek(width, available)
                            .expect("measured workspace feedback");
                    } else {
                        runtime.idle_content_width = Some(width);
                        runtime
                            .hub
                            .retarget_idle(width, available)
                            .expect("measured idle content");
                    }
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
                    (runtime.sample(now), runtime.menu, runtime.previous.menu)
                };
                self.view_hub_at(id, sample, menu, previous_menu)
            },
            move |_| {
                let runtime = runtime.borrow();
                runtime.hub.active() || runtime.activities.active()
            },
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
        let (target_size, previous_size) = {
            let runtime = entry.runtime.hub.as_ref().expect("Morph runtime").borrow();
            (runtime.hub.target_size(), runtime.previous.size)
        };
        let mut layers = cosmic::iced::widget::Stack::new();
        if sample.previous_opacity > 0. {
            layers = layers.push(content_frame::view(
                self.hub_content(
                    id,
                    sample.previous,
                    sample.previous_opacity,
                    previous_menu,
                    false,
                    false,
                ),
                previous_size,
                panel.edge,
            ));
        }
        if sample.opacity > 0. {
            layers = layers.push(content_frame::view(
                container(self.hub_content(id, sample.current, sample.opacity, menu, true, false))
                    .id("ferese-hub-content")
                    .width(Length::Fill)
                    .height(Length::Fill)
                    .into(),
                target_size,
                panel.edge,
            ));
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
        background[3] = palette.panel_opacity(panel.background_opacity);
        let background = cosmic::iced::Color::from_rgba(background[0], background[1], background[2], background[3]);
        let mut border = crate::color(palette.border);
        if let Some((tint, alpha)) = self.activity_result_tint(id) {
            border.r += (tint.r - border.r) * alpha;
            border.g += (tint.g - border.g) * alpha;
            border.b += (tint.b - border.b) * alpha;
        }
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
        let peek = self
            .outputs
            .iter()
            .find(|entry| entry.bar == id)
            .and_then(|entry| entry.runtime.hub.as_ref())
            .map_or(Peek::Date, |runtime| {
                let runtime = runtime.borrow();
                if !interactive && !intrinsic {
                    runtime.previous.peek
                } else {
                    runtime.peek
                }
            });
        let content: Element<'_, cosmic::Action<Message>> = match mode {
            Mode::Idle | Mode::Peek
                if self.has_compact_activities(id)
                    && self.hub_priority().is_none()
                    && !(mode == Mode::Peek && peek == Peek::Workspaces) =>
            {
                self.activity_compact(id, opacity, intrinsic)
            }
            Mode::Idle | Mode::Peek | Mode::Toast => {
                let playing = self
                    .media
                    .snapshot
                    .selected
                    .as_ref()
                    .filter(|player| player.status == ferese_ipc::media::Playback::Playing);
                let time = self
                    .clock
                    .split_once(", ")
                    .map_or(self.clock.as_str(), |(_, time)| time);
                let priority = self.hub_priority();
                let activity = if mode == Mode::Idle {
                    priority
                } else if mode == Mode::Peek && matches!(peek, Peek::Recording | Peek::Battery) {
                    Some(peek)
                } else {
                    None
                };
                let label = if mode == Mode::Toast {
                    self.outputs
                        .iter()
                        .find(|entry| entry.bar == id)
                        .and_then(|entry| entry.runtime.hub.as_ref())
                        .and_then(|hub| {
                            let hub = hub.borrow();
                            if !interactive && !intrinsic {
                                hub.previous.toast
                            } else {
                                hub.transient.toast()
                            }
                        })
                        .and_then(|id| self.notifications.entries.iter().find(|notice| notice.id == id))
                        .map_or_else(|| time.to_owned(), |notice| notice.title.clone())
                } else if mode == Mode::Peek {
                    match peek {
                        Peek::Recording | Peek::Battery => String::new(),
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
                } else if let Some(player) = playing {
                    player.label().to_owned()
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
                let kind = if let Some(activity) = activity {
                    if activity == Peek::Recording {
                        status_ui::Menu::Overflow
                    } else {
                        status_ui::Menu::Battery
                    }
                } else if mode == Mode::Toast {
                    status_ui::Menu::Notifications
                } else if mode == Mode::Peek {
                    match peek {
                        Peek::Recording => status_ui::Menu::Overflow,
                        Peek::Battery => status_ui::Menu::Battery,
                        Peek::Media => status_ui::Menu::Media,
                        Peek::Date => status_ui::Menu::Workspaces,
                        Peek::Workspaces => status_ui::Menu::Workspaces,
                    }
                } else if playing.is_some() {
                    status_ui::Menu::Media
                } else if crate::bar::focused_bar_title(&self.snapshot, self.output_for_bar(id))
                    .trim()
                    .is_empty()
                {
                    status_ui::Menu::Workspaces
                } else {
                    self.outputs
                        .iter()
                        .find(|entry| entry.bar == id)
                        .and_then(|entry| entry.runtime.hub.as_ref())
                        .map_or(status_ui::Menu::Workspaces, |runtime| runtime.borrow().last_menu)
                };
                let label = self
                    .render
                    .text(label)
                    .size(13)
                    .class(theme::Text::Color(primary))
                    .ellipsize(cosmic::iced::widget::text::Ellipsize::End(
                        cosmic::iced::advanced::text::EllipsizeHeightLimit::Lines(1),
                    ));
                let body: Element<'_, cosmic::Action<Message>> = match mode {
                    _ if activity.is_some() => {
                        self.hub_activity_strip(id, activity.unwrap(), mode == Mode::Peek, intrinsic, opacity)
                    }
                    Mode::Peek => {
                        let leading = match peek {
                            Peek::Recording | Peek::Battery => unreachable!("activity strip handled above"),
                            Peek::Media => self.hub_music_art(opacity),
                            Peek::Workspaces => self.hub_workspaces(id, opacity),
                            Peek::Date => icons::tinted(icons::morph::CALENDAR, 18, icon_primary)
                                .opacity(opacity)
                                .into(),
                        };
                        let width = if intrinsic { Length::Shrink } else { Length::Fill };
                        let label = label.width(width).align_x(if peek == Peek::Date {
                            Alignment::End
                        } else {
                            Alignment::Start
                        });
                        let mut strip = row![leading, label].spacing(10).width(width).align_y(Alignment::Center);
                        if peek == Peek::Media {
                            strip = strip.push(self.hub_equalizer(id, opacity));
                        }
                        strip.into()
                    }
                    Mode::Toast => row![
                        icons::tinted(icons::morph::NOTIFICATIONS, 16, icon_muted).opacity(opacity),
                        label.width(Length::Fill)
                    ]
                    .spacing(10)
                    .align_y(Alignment::Center)
                    .into(),
                    _ if playing.is_some() => {
                        // Measure the title only after reserving both side widgets,
                        // outer padding, and gaps. During rendering, flex gives it
                        // whatever remains as the shell animates.
                        let workspaces = self.hub_workspace_width(id) as f32;
                        let title_limit = (self.hub_idle_width_limit(id)
                            - if workspaces > 0. { workspaces + 10. } else { 0. }
                            - 28.
                            - 24.
                            - 13.
                            - 16.)
                            .max(0.);
                        let title = if intrinsic {
                            container(label).max_width(title_limit)
                        } else {
                            container(label.width(Length::Fill)).width(Length::Fill)
                        };
                        row![self.hub_music_art(opacity), title, self.hub_equalizer(id, opacity)]
                            .spacing(8)
                            .width(if intrinsic { Length::Shrink } else { Length::Fill })
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
                let content_width = if intrinsic || (mode == Mode::Idle && (playing.is_none() || activity.is_some())) {
                    Length::Shrink
                } else {
                    Length::Fill
                };
                let content_height = if intrinsic { Length::Fixed(28.) } else { Length::Fill };
                let context = button::custom(container(body).width(content_width).center_y(content_height))
                    .name(match activity {
                        Some(Peek::Recording) => format!(
                            "{}. Open recording controls",
                            self.recorder
                                .elapsed()
                                .map_or_else(|| "Saving recording".into(), |elapsed| format!("Recording, {elapsed}"))
                        ),
                        Some(Peek::Battery) => "Low battery. Open battery controls".into(),
                        _ => "Expand panel hub".into(),
                    })
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
                for (kind, label, icon) in [
                    (status_ui::Menu::Workspaces, "Workspaces", icons::morph::WORKSPACES),
                    (status_ui::Menu::Media, "Music", icons::morph::MUSIC),
                    (status_ui::Menu::Calendar, "Calendar", icons::morph::CALENDAR),
                    (
                        status_ui::Menu::Notifications,
                        "Notifications",
                        icons::morph::NOTIFICATIONS,
                    ),
                    (status_ui::Menu::Overflow, "Controls", icons::morph::CONTROLS),
                    (status_ui::Menu::Activities, "Activities", icons::morph::ACTIVITIES),
                ] {
                    if kind == status_ui::Menu::Activities {
                        // Activities follows both the main tabs and optional shortcuts.
                        for shortcut in shortcuts.iter().copied().filter(|kind| *kind != ItemKind::Overview) {
                            tabs = tabs.push(self.hub_shortcut_button(id, shortcut, menu, opacity));
                        }
                        if !self.activities_configured(id) {
                            continue;
                        }
                    }
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
                        self.render,
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
                let navigation = scrollable(tabs.width(Length::Shrink))
                    .direction(cosmic::iced::widget::scrollable::Direction::Horizontal(
                        cosmic::iced::widget::scrollable::Scrollbar::default()
                            .width(0)
                            .scroller_width(0),
                    ))
                    .width(Length::Fill);
                let tabs = row::with_capacity(2).spacing(2).align_y(Alignment::Center);
                let mut tabs = tabs.push(navigation);
                if let Some(battery) = self.hub_battery(id, menu == Some(status_ui::Menu::Battery), opacity) {
                    tabs = tabs.push(battery);
                }
                tabs = tabs.push(motion::button(
                    self.render,
                    button::custom(icons::tinted(icons::morph::CLOSE, 18, icon_primary).opacity(opacity))
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
                    Content::Activities => status_ui::Menu::Activities,
                    Content::Workspaces => status_ui::Menu::Workspaces,
                    Content::Media => status_ui::Menu::Media,
                    Content::Calendar => status_ui::Menu::Calendar,
                    Content::Notifications => status_ui::Menu::Notifications,
                    Content::Controls => menu.unwrap_or(status_ui::Menu::Overflow),
                };
                let body = if kind == status_ui::Menu::Activities {
                    self.activity_list(id, opacity)
                } else if kind == status_ui::Menu::Workspaces {
                    self.hub_workspace_grid(id, opacity)
                } else if kind == status_ui::Menu::Overflow {
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
            scroll::area(content, id)
        } else {
            content
        };
        if interactive && opacity >= 0.5 {
            content
        } else {
            content.map(|_| cosmic::Action::App(Message::HubIgnore))
        }
    }

    fn hub_music_art(&self, opacity: f32) -> Element<'_, cosmic::Action<Message>> {
        match self.media.artwork.as_ref().and_then(|art| art.handle.as_ref()) {
            Some(handle) => cosmic::widget::image(handle.clone())
                .width(24)
                .height(24)
                .opacity(opacity)
                .content_fit(cosmic::iced::ContentFit::Cover)
                .border_radius(5.)
                .into(),
            None => container(
                icons::tinted(icons::morph::MUSIC, 18, crate::color(self.config.theme.text_primary)).opacity(opacity),
            )
            .center_x(24)
            .center_y(24)
            .into(),
        }
    }

    fn hub_equalizer(&self, id: window::Id, opacity: f32) -> Element<'_, cosmic::Action<Message>> {
        let playing = self
            .media
            .snapshot
            .selected
            .as_ref()
            .is_some_and(|player| player.status == ferese_ipc::media::Playback::Playing);
        let animate = playing && self.render.motion.motion_enabled();
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
            self.render,
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
            ItemKind::Overview => (icons::morph::WORKSPACES, None),
            ItemKind::Activities => (icons::morph::ACTIVITIES, Some(status_ui::Menu::Activities)),
            ItemKind::QuickSettings => (icons::morph::SETTINGS, Some(status_ui::Menu::System)),
            ItemKind::Network => (icons::WIFI, Some(status_ui::Menu::Network)),
            ItemKind::Recording => (
                match self.recorder.state {
                    crate::recording::State::Selecting => icons::RECORD_CANCEL,
                    crate::recording::State::Recording(_) => icons::RECORD_STOP,
                    crate::recording::State::Saving => icons::RECORD_SAVING,
                    _ => icons::morph::RECORD,
                },
                None,
            ),
            ItemKind::Audio => (icons::VOLUME, Some(status_ui::Menu::Audio)),
            ItemKind::Battery { .. } => (icons::BATTERY, Some(status_ui::Menu::Battery)),
            ItemKind::DisplayMode if self.display_mode.external_connected() => (icons::morph::DISPLAY, None),
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
        // Built-in tabs, battery and Close keep their space through the morph.
        let tabs = if self.activities_configured(id) { 7. } else { 6. };
        let battery = if self.status.battery.is_some() {
            battery::WIDTH + 2.
        } else {
            0.
        };
        let capacity = ((width - 28. - tabs * 32. - battery - 8.).max(0.) / 32.).floor() as usize;
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
            if matches!(item.kind, ItemKind::Overview | ItemKind::Activities) {
                continue;
            }
            if matches!(
                item.kind,
                ItemKind::Network | ItemKind::Audio | ItemKind::Battery { .. }
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
            if shortcuts.contains(&item.kind) || matches!(item.kind, ItemKind::Battery { .. }) {
                continue;
            }
            if matches!(item.kind, ItemKind::Overview | ItemKind::Activities) {
                continue;
            }
            let Some(action) = self.hub_control_action(id, item.kind) else {
                continue;
            };
            let ControlAction {
                icon, label, message, ..
            } = action;
            tiles.push(motion::button(
                self.render,
                button::custom(
                    row![
                        container(
                            icons::tinted(icon, 22, crate::color(self.config.theme.text_primary)).opacity(opacity)
                        )
                        .center_x(30)
                        .center_y(28),
                        self.render
                            .text(label)
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

    fn seed_activities(shell: &mut FereseShell, count: usize) {
        use ferese_activities::{Data, Owner, Source, Store, Template, Value};
        let now = ferese_ipc::activities::now_ms();
        if !shell.config.panels[0]
            .end
            .groups
            .iter()
            .flat_map(|group| &group.items)
            .any(|item| item.kind == ItemKind::Activities)
        {
            shell.config.panels[0].end.groups[0]
                .items
                .push(ferese_panel::Item::new("test-activities", ItemKind::Activities));
        }
        if shell.snapshot.outputs.is_empty() {
            shell.snapshot.outputs.push(crate::control::OutputSnapshot {
                id: 4,
                name: "test".into(),
                active_workspace: 1,
                focused: true,
            });
        }
        let mut store = Store::new(7);
        for index in 0..count {
            store
                .start(
                    Owner { pid: 40, birth: 1 },
                    format!("task-{index}"),
                    Data {
                        source: Source::Publisher,
                        output: shell
                            .output_for_bar(shell.outputs[0].bar)
                            .map(|output| output.name.clone()),
                        priority: 100 + index as u8,
                        icon: if index == 0 { "hammer" } else { "download" }.into(),
                        title: if index == 2 {
                            "Installing update"
                        } else if index == 0 {
                            "Cargo test: a deliberately long title which must leave all neighboring controls accessible"
                        } else {
                            "Downloading update"
                        }
                        .into(),
                        value: if index == 0 {
                            Value::Indeterminate
                        } else {
                            Value::Progress(0.62)
                        },
                        template: Template::Progress,
                    },
                    now.saturating_sub(500),
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
            now_ms: now,
            revision: 1,
            ..Default::default()
        });
    }
    #[test]
    fn activity_target_must_still_exist_and_activities_has_no_duplicate_shortcut() {
        let mut app = shell();
        let bar = app.outputs[0].bar;
        seed_activities(&mut app, 1);
        let activity = app.activities.snapshot.activities[0].id.clone();
        assert_eq!(app.activity_window(&activity), None);
        std::sync::Arc::make_mut(&mut app.activities.snapshot)
            .windows
            .push(ferese_ipc::activities::ActivityWindow {
                activity: activity.clone(),
                window: 42,
            });
        assert_eq!(app.activity_window(&activity), None);
        app.snapshot.windows.push(crate::control::WindowSnapshot {
            id: 42,
            workspace: 1,
            geometry: None,
            app_id: "foot".into(),
            title: "Terminal".into(),
            focused: false,
            urgent: false,
            fullscreen: false,
            maximized: false,
            floating: false,
        });
        assert_eq!(app.activity_window(&activity), Some(42));
        app.snapshot.windows.clear();
        assert_eq!(app.activity_window(&activity), None);
        app.config.panels[0].morph.expanded_width = 400.;
        assert!(!app.hub_shortcuts(bar).contains(&ItemKind::Activities));
        app.status.battery = None;
        assert!(app.hub_battery(bar, false, 1.).is_none());
        app.status.battery = Some(crate::status::Battery {
            percent: 100,
            status: "Full".into(),
        });
        assert!(app.hub_battery(bar, false, 1.).is_some());
    }

    #[test]
    fn open_activity_list_survives_publisher_churn() {
        use cosmic::iced::advanced::renderer::Headless;
        let renderer = cosmic::iced::futures::executor::block_on(cosmic::Renderer::new(
            cosmic::iced::Font::default(),
            cosmic::iced::Pixels(14.),
            Some("tiny-skia"),
        ))
        .unwrap();
        let mut app = shell();
        let id = app.outputs[0].bar;
        seed_activities(&mut app, 3);
        drop(app.sync_hub_activity());
        drop(app.open_hub(id, status_ui::Menu::Activities));
        let mut tree = widget::Tree::empty();
        for count in [3, 2, 1, 3, 0, 2, 3] {
            let opacity = app.outputs[0]
                .runtime
                .hub
                .as_ref()
                .unwrap()
                .borrow()
                .hub
                .sample()
                .opacity;
            seed_activities(&mut app, count);
            app.sync_activity_presentations();
            assert_eq!(
                app.outputs[0]
                    .runtime
                    .hub
                    .as_ref()
                    .unwrap()
                    .borrow()
                    .hub
                    .sample()
                    .opacity,
                opacity,
                "publisher changes must not restart the open panel's crossfade"
            );
            for _ in 0..50 {
                let runtime = app.outputs[0].runtime.hub.as_ref().unwrap();
                let (sample, previous_menu) = {
                    let mut runtime = runtime.borrow_mut();
                    runtime.hub.advance(Duration::from_millis(16), false);
                    (runtime.hub.sample(), runtime.previous.menu)
                };
                let mut view = app.view_hub_at(id, sample, Some(status_ui::Menu::Activities), previous_menu);
                widget::tree::NAMED.with(|named| *named.borrow_mut() = tree.take_all_named());
                tree.diff(&mut view);
                view.as_widget_mut().layout(
                    &mut tree,
                    &renderer,
                    &layout::Limits::new(Size::ZERO, Size::new(1200., 800.)),
                );
                widget::tree::NAMED.with(|named| named.borrow_mut().clear());
            }
        }
    }

    #[test]
    fn activity_transitions_reconcile_the_existing_widget_tree() {
        use cosmic::iced::advanced::renderer::Headless;
        let renderer = cosmic::iced::futures::executor::block_on(cosmic::Renderer::new(
            cosmic::iced::Font::default(),
            cosmic::iced::Pixels(14.),
            Some("tiny-skia"),
        ))
        .unwrap();
        let mut app = shell();
        let id = app.outputs[0].bar;
        seed_activities(&mut app, 3);
        drop(app.sync_hub_activity());
        let mut tree = widget::Tree::empty();
        for menu in [
            None,
            Some(status_ui::Menu::Activities),
            Some(status_ui::Menu::Calendar),
            Some(status_ui::Menu::Activities),
            None,
        ] {
            if let Some(menu) = menu {
                drop(app.open_hub(id, menu));
            } else {
                drop(app.close_hub(id));
            }
            for _ in 0..50 {
                let runtime = app.outputs[0].runtime.hub.as_ref().unwrap();
                let (sample, previous_menu) = {
                    let mut runtime = runtime.borrow_mut();
                    runtime.hub.advance(Duration::from_millis(16), false);
                    (runtime.hub.sample(), runtime.previous.menu)
                };
                let mut view = app.view_hub_at(id, sample, menu, previous_menu);
                // Application rebuilds temporarily extract named widget state.
                widget::tree::NAMED.with(|named| *named.borrow_mut() = tree.take_all_named());
                tree.diff(&mut view);
                view.as_widget_mut().layout(
                    &mut tree,
                    &renderer,
                    &layout::Limits::new(Size::ZERO, Size::new(1200., 800.)),
                );
                widget::tree::NAMED.with(|named| named.borrow_mut().clear());
            }
        }
    }

    #[test]
    fn live_activity_keeps_workspace_wheel_handler() {
        use cosmic::iced::advanced::{Layout, Shell, clipboard, layout, renderer::Headless, widget};
        use cosmic::iced::{Event, Size, mouse};
        let renderer = cosmic::iced::futures::executor::block_on(cosmic::Renderer::new(
            cosmic::iced::Font::default(),
            cosmic::iced::Pixels(14.),
            Some("tiny-skia"),
        ))
        .unwrap();
        for with_activity in [false, true] {
            let mut app = shell();
            let id = app.outputs[0].bar;
            if with_activity {
                seed_activities(&mut app, 1);
            }
            drop(app.sync_hub_activity());
            let mut view = app.hub_content(id, Mode::Peek, 1., None, true, false);
            let mut tree = widget::Tree::new(&view);
            let node = view.as_widget_mut().layout(
                &mut tree,
                &renderer,
                &layout::Limits::new(Size::ZERO, Size::new(260., 44.)),
            );
            let viewport = cosmic::iced::Rectangle::with_size(Size::new(260., 44.));
            let mut messages = Vec::new();
            view.as_widget_mut().update(
                &mut tree,
                &Event::Mouse(mouse::Event::WheelScrolled {
                    delta: mouse::ScrollDelta::Lines { x: 0., y: 1. },
                }),
                Layout::new(&node),
                mouse::Cursor::Available(cosmic::iced::Point::new(130., 20.)),
                &renderer,
                &mut clipboard::Null,
                &mut Shell::new(&mut messages),
                &viewport,
            );
            assert!(
                messages
                    .iter()
                    .any(|message| matches!(message, cosmic::Action::App(Message::HubWorkspaceScroll(_, _)))),
                "workspace scrolling stopped with_activity={with_activity}"
            );
        }
    }

    #[test]
    fn workspace_feedback_takes_precedence_over_compact_activities() {
        use cosmic::iced::advanced::renderer::Headless;
        #[derive(Debug, Default)]
        struct Labels(Vec<String>);
        impl widget::Operation for Labels {
            fn traverse(&mut self, visit: &mut dyn FnMut(&mut dyn widget::Operation)) {
                visit(self);
            }
            fn text(&mut self, _: Option<&widget::Id>, _: Rectangle, text: &str) {
                self.0.push(text.into());
            }
        }
        let renderer = cosmic::iced::futures::executor::block_on(cosmic::Renderer::new(
            cosmic::iced::Font::default(),
            cosmic::iced::Pixels(14.),
            Some("tiny-skia"),
        ))
        .unwrap();
        let mut app = shell();
        let bar = app.outputs[0].bar;
        seed_activities(&mut app, 1);
        app.snapshot.workspaces = vec![crate::control::WorkspaceSnapshot {
            id: 1,
            name: "Code".into(),
            output: Some(4),
            index: 1,
            window_count: 1,
            visible: true,
            focused: true,
        }];
        drop(app.sync_hub_activity());
        drop(app.transition_hub(bar, transition::Event::Workspaces(Instant::now())));
        assert_eq!(app.hub_mode(bar), Some(Mode::Peek));
        let mut view = app.hub_content(bar, Mode::Peek, 1., None, true, false);
        let mut tree = widget::Tree::new(&view);
        let node = view.as_widget_mut().layout(
            &mut tree,
            &renderer,
            &layout::Limits::new(Size::ZERO, Size::new(260., 44.)),
        );
        let mut labels = Labels::default();
        view.as_widget_mut()
            .operate(&mut tree, Layout::new(&node), &renderer, &mut labels);
        assert!(labels.0.iter().any(|text| text == "1 · Code"), "{labels:?}");
        assert!(!labels.0.iter().any(|text| text.contains("Cargo test")));
    }

    #[test]
    fn dnd_list_keeps_result_rows_reachable_and_preserves_focus_on_update() {
        use cosmic::iced::keyboard::key::Named;
        let mut app = shell();
        let bar = app.outputs[0].bar;
        seed_activities(&mut app, 3);
        let activity = &mut std::sync::Arc::make_mut(&mut app.activities.snapshot).activities[0];
        activity.state = ferese_activities::State::Done;
        activity.result_deadline = Some(ferese_ipc::activities::now_ms() + 10_000);
        let completed = activity.id.clone();
        app.notifications.dnd = true;
        drop(app.sync_hub_activity());
        drop(app.open_hub(bar, status_ui::Menu::Activities));
        let expected: Vec<_> = ferese_activities::ranked(
            app.activities.snapshot.activities.iter(),
            ferese_ipc::activities::now_ms(),
            app.activities.snapshot.limits,
            app.output_for_bar(bar).unwrap().name.as_str(),
            false,
        )
        .iter()
        .map(|entry| entry.id.clone())
        .collect();
        let runtime = app.outputs[0].runtime.hub.as_ref().unwrap();
        assert!(expected.contains(&completed));
        assert!(!runtime.borrow().activities.selection.visible.contains(&completed));
        for (index, id) in expected.iter().enumerate() {
            let key = if index == 0 { Named::Home } else { Named::ArrowDown };
            drop(app.activity_navigation(bar, None, Some(key)));
            assert_eq!(
                app.outputs[0]
                    .runtime
                    .hub
                    .as_ref()
                    .unwrap()
                    .borrow()
                    .activities
                    .focus
                    .as_ref(),
                Some(id)
            );
        }
        drop(app.activity_navigation(bar, Some(completed.clone()), None));
        drop(app.sync_hub_activity());
        assert_eq!(
            app.outputs[0].runtime.hub.as_ref().unwrap().borrow().activities.focus,
            Some(completed)
        );
    }

    #[test]
    fn activity_arrows_follow_tab_focus() {
        use cosmic::iced::advanced::widget::operation;
        use cosmic::iced::advanced::{Layout, layout, renderer::Headless, widget};
        use cosmic::iced::{Size, keyboard::key::Named};
        let renderer = cosmic::iced::futures::executor::block_on(cosmic::Renderer::new(
            cosmic::iced::Font::default(),
            cosmic::iced::Pixels(14.),
            Some("tiny-skia"),
        ))
        .unwrap();
        let mut app = shell();
        let bar = app.outputs[0].bar;
        seed_activities(&mut app, 3);
        drop(app.sync_hub_activity());
        drop(app.open_hub(bar, status_ui::Menu::Activities));
        let ranked = ferese_activities::ranked(
            app.activities.snapshot.activities.iter(),
            ferese_ipc::activities::now_ms(),
            app.activities.snapshot.limits,
            app.output_for_bar(bar).unwrap().name.as_str(),
            false,
        );
        let ids: Vec<_> = ranked.iter().map(|entry| entry.id.clone()).collect();
        let id = &ids[0];
        let first = widget::Id::new(format!(
            "activity-{bar:?}-{}-{}-{}-{}",
            id.session, id.generation, id.owner.pid, id.owner.birth
        ));
        let focused = {
            let mut query = app.activity_focus_query(bar).unwrap();
            let mut view = app.activity_list(bar, 1.);
            let mut tree = widget::Tree::new(&view);
            let node = view.as_widget_mut().layout(
                &mut tree,
                &renderer,
                &layout::Limits::new(Size::ZERO, Size::new(312., 600.)),
            );
            view.as_widget_mut().operate(
                &mut tree,
                Layout::new(&node),
                &renderer,
                &mut operation::focusable::focus::<()>(first),
            );
            let mut tab: Box<dyn widget::Operation> = Box::new(operation::focusable::focus_next());
            loop {
                view.as_widget_mut()
                    .operate(&mut tree, Layout::new(&node), &renderer, &mut tab);
                match tab.finish() {
                    operation::Outcome::Chain(next) => tab = next,
                    _ => break,
                }
            }
            view.as_widget_mut().operate(
                &mut tree,
                Layout::new(&node),
                &renderer,
                &mut operation::black_box(&mut query),
            );
            match widget::Operation::finish(&query) {
                operation::Outcome::Some(focused) => focused,
                _ => panic!("missing focus query result"),
            }
        };
        assert_eq!(focused, Some(ids[1].clone()));
        drop(app.activity_navigation(bar, focused, Some(Named::ArrowDown)));
        assert_eq!(
            app.outputs[0].runtime.hub.as_ref().unwrap().borrow().activities.focus,
            Some(ids[2].clone()),
            "Tab focuses row two, so Down must focus row three"
        );
    }

    #[test]
    fn compact_activities_fit_titles_and_keep_the_badge_adjacent() {
        use cosmic::iced::advanced::renderer::Headless;
        #[derive(Default)]
        struct TextBounds(Vec<(String, Rectangle)>);
        impl widget::Operation for TextBounds {
            fn traverse(&mut self, visit: &mut dyn FnMut(&mut dyn widget::Operation)) {
                visit(self);
            }
            fn text(&mut self, _: Option<&widget::Id>, bounds: Rectangle, text: &str) {
                self.0.push((text.into(), bounds));
            }
        }
        let renderer = cosmic::iced::futures::executor::block_on(cosmic::Renderer::new(
            cosmic::iced::Font::default(),
            cosmic::iced::Pixels(14.),
            Some("tiny-skia"),
        ))
        .unwrap();
        let mut app = shell();
        let id = app.outputs[0].bar;
        let mut widths = Vec::new();
        for count in [1, 2, 3] {
            seed_activities(&mut app, count);
            for activity in &mut std::sync::Arc::make_mut(&mut app.activities.snapshot).activities {
                activity.data.title = "Build".into();
            }
            drop(app.sync_hub_activity());
            let width = app.outputs[0].runtime.hub.as_ref().unwrap().borrow().hub.target_size()[0];
            widths.push(width);
            drop(app.hub_hover(id, true));
            assert_eq!(
                app.outputs[0].runtime.hub.as_ref().unwrap().borrow().hub.target_size()[0],
                width
            );
            let mut view = app.activity_compact(id, 1., false);
            let mut tree = widget::Tree::new(&view);
            let node = view.as_widget_mut().layout(
                &mut tree,
                &renderer,
                &layout::Limits::new(Size::ZERO, Size::new(width as f32, 44.)),
            );
            let mut labels = TextBounds::default();
            view.as_widget_mut()
                .operate(&mut tree, Layout::new(&node), &renderer, &mut labels);
            if count == 3 {
                let title = labels.0.iter().rfind(|(text, _)| text == "Build").unwrap().1;
                let badge = labels.0.iter().find(|(text, _)| text == "+1").unwrap().1;
                assert!((badge.x - title.x - title.width - 8.).abs() < 1., "{:#?}", labels.0);
                assert!(
                    (width as f32 - badge.x - badge.width - 14.).abs() < 1.,
                    "{:#?}",
                    labels.0
                );
            }
            drop(view);
            drop(app.hub_hover(id, false));
        }
        assert!(widths[0] < 180., "short single activity should shrink: {widths:?}");
        assert!(widths[0] < widths[1] && widths[1] < widths[2], "{widths:?}");
        for activity in &mut std::sync::Arc::make_mut(&mut app.activities.snapshot).activities {
            activity.data.value = ferese_activities::Value::Text("A much longer changing phase".into());
        }
        drop(app.sync_hub_activity());
        assert_eq!(
            app.outputs[0].runtime.hub.as_ref().unwrap().borrow().hub.target_size()[0],
            widths[2]
        );
        std::sync::Arc::make_mut(&mut app.activities.snapshot).limits.show_badge = false;
        drop(app.sync_hub_activity());
        assert_eq!(
            app.outputs[0].runtime.hub.as_ref().unwrap().borrow().hub.target_size()[0],
            widths[1],
            "hiding the badge must remove its reserved width"
        );
        std::sync::Arc::make_mut(&mut app.activities.snapshot).limits.show_badge = true;
        drop(app.sync_hub_activity());
        for activity in &mut std::sync::Arc::make_mut(&mut app.activities.snapshot).activities {
            activity.data.title = "A very long title which must be capped in the compact strip".into();
        }
        drop(app.sync_hub_activity());
        let wider = app.outputs[0].runtime.hub.as_ref().unwrap().borrow().hub.target_size()[0];
        assert!(wider > widths[2] && wider < 450., "{wider}");
        app.outputs[0].size = Some((240, 800));
        drop(app.sync_hub_activity());
        let width = app.outputs[0].runtime.hub.as_ref().unwrap().borrow().hub.target_size()[0] as f32;
        assert!(width <= 240.);
        let mut view = app.activity_compact(id, 1., false);
        let mut tree = widget::Tree::new(&view);
        let node = view.as_widget_mut().layout(
            &mut tree,
            &renderer,
            &layout::Limits::new(Size::ZERO, Size::new(width, 44.)),
        );
        let mut labels = TextBounds::default();
        view.as_widget_mut()
            .operate(&mut tree, Layout::new(&node), &renderer, &mut labels);
        let badge = labels.0.iter().find(|(text, _)| text == "+1").unwrap().1;
        assert!(
            (width - badge.x - badge.width - 14.).abs() < 1.,
            "badge must fit a narrow output: {:#?}",
            labels.0
        );
    }

    #[test]
    fn activity_values_do_not_resize_and_hover_pins_survivors_and_dnd_overrides() {
        let mut shell = shell();
        let id = shell.outputs[0].bar;
        seed_activities(&mut shell, 3);
        drop(shell.sync_hub_activity());
        let runtime = shell.outputs[0].runtime.hub.as_ref().unwrap();
        let width = runtime.borrow().hub.target_size()[0];
        let first = runtime.borrow().activities.selection.visible.clone();
        drop(shell.hub_hover(id, true));
        std::sync::Arc::make_mut(&mut shell.activities.snapshot).activities[0]
            .data
            .priority = 255;
        std::sync::Arc::make_mut(&mut shell.activities.snapshot).activities[2]
            .data
            .value = ferese_activities::Value::Text("A changed phase".into());
        drop(shell.sync_hub_activity());
        let runtime = shell.outputs[0].runtime.hub.as_ref().unwrap();
        assert_eq!(runtime.borrow().activities.selection.visible, first);
        assert_eq!(runtime.borrow().hub.target_size()[0], width);
        std::sync::Arc::make_mut(&mut shell.activities.snapshot)
            .activities
            .retain(|entry| entry.id != first[0]);
        drop(shell.sync_hub_activity());
        assert_eq!(
            shell.outputs[0]
                .runtime
                .hub
                .as_ref()
                .unwrap()
                .borrow()
                .activities
                .selection
                .visible,
            vec![first[1].clone()]
        );
        shell.notifications.dnd = true;
        drop(shell.sync_hub_activity());
        let runtime = shell.outputs[0].runtime.hub.as_ref().unwrap();
        assert_eq!(runtime.borrow().activities.selection.visible.len(), 1);
        assert_eq!(runtime.borrow().activities.selection.hidden, 0);
        drop(shell.hub_hover(id, false));
        drop(shell.open_hub(id, status_ui::Menu::Activities));
        assert_eq!(shell.hub_mode(id), Some(Mode::Expanded(Content::Activities)));
    }
    #[test]
    fn shared_activity_item_and_popup_work_in_flat_floating_and_adaptive_panels() {
        for style in [PanelStyle::Flat, PanelStyle::Floating, PanelStyle::Adaptive] {
            let mut shell = shell();
            let id = shell.outputs[0].bar;
            seed_activities(&mut shell, 2);
            shell.config.panels[0].style = style;
            shell.outputs[0]
                .runtime
                .reset(&shell.config.panels[0], &shell.snapshot, Some("test"));
            drop(shell.sync_hub_activity());
            assert!(shell.activity_available(id));
            assert!(shell.outputs[0].runtime.hub.is_none());
            let item = shell.config.panels[0]
                .item(&ferese_panel::ItemId("test-activities".into()))
                .unwrap();
            assert!(
                shell
                    .view_activity_item(id, item, ferese_panel::Representation::Compact, false, Some(160.), 28.)
                    .1
            );
            let anchor = crate::status_ui::PopoverAnchor {
                parent: id,
                panel: shell.config.panels[0].id.clone(),
                item: Some(item.id.clone()),
                rectangle: cosmic::iced::Rectangle {
                    x: 20,
                    y: 0,
                    width: 160,
                    height: 28,
                },
            };
            drop(shell.open_menu(status_ui::Menu::Activities, anchor));
            assert_eq!(shell.menu.as_ref().unwrap().kind, status_ui::Menu::Activities);
            assert!(
                shell
                    .activity_key(
                        shell.menu.as_ref().unwrap().id,
                        cosmic::iced::keyboard::key::Named::ArrowDown
                    )
                    .is_some()
            );
            shell.activities.snapshot = std::sync::Arc::new(Default::default());
            drop(shell.sync_hub_activity());
            assert!(!shell.activity_available(id));
        }
    }
    #[test]
    fn empty_activity_updates_preserve_the_measured_idle_width() {
        let mut app = shell();
        seed_activities(&mut app, 0);
        drop(app.sync_hub_activity());
        {
            let mut runtime = app.outputs[0].runtime.hub.as_ref().unwrap().borrow_mut();
            let available = runtime.available;
            // Layout measures the whole ordinary idle row, including workspace dots.
            runtime.idle_content_width = Some(224.);
            runtime.hub.retarget_idle(224., available).unwrap();
            runtime.hub.advance(Duration::ZERO, true);
        }
        for _ in 0..3 {
            drop(app.sync_hub_activity());
            let runtime = app.outputs[0].runtime.hub.as_ref().unwrap().borrow();
            assert_eq!(runtime.idle_content_width, Some(224.));
            assert_eq!(runtime.hub.target_size()[0], 224.);
            assert!(!runtime.hub.active(), "status refresh restarted the idle morph");
        }
    }

    #[test]
    fn progress_updates_do_not_start_a_second_hub_animation() {
        let mut app = shell();
        let bar = app.outputs[0].bar;
        seed_activities(&mut app, 1);
        drop(app.sync_hub_activity());
        let runtime = app.outputs[0].runtime.hub.as_ref().unwrap();
        runtime.borrow_mut().activities.advance(Duration::ZERO, true);
        assert!(!runtime.borrow().activities.active());
        std::sync::Arc::make_mut(&mut app.activities.snapshot).activities[0]
            .data
            .value = ferese_activities::Value::Progress(0.75);
        drop(app.sync_hub_activity());
        assert!(
            !app.outputs[0]
                .runtime
                .hub
                .as_ref()
                .unwrap()
                .borrow()
                .activities
                .active()
        );
        assert!(app.activity_available(bar));
    }
    #[test]
    fn shell_rendering_configuration_is_instance_owned() {
        let mut animated = shell();
        let mut snap = shell();
        let original = animated.render;
        let mut config = snap.config.clone();
        config.font_family = Some("monospace".into());
        config.theme.material_radius = 3.;
        config.animations.reduced_motion = true;
        drop(snap.apply_config(config));
        assert_eq!(animated.render.font, original.font);
        assert_ne!(snap.render.font, animated.render.font);
        assert_eq!(animated.render.radius(20.), original.radius(20.));
        assert_eq!(snap.render.radius(20.), 3.);
        for instance in [&mut animated, &mut snap] {
            drop(instance.open_hub(instance.outputs[0].bar, status_ui::Menu::Calendar));
        }
        let mut a = animated.outputs[0].runtime.hub.as_ref().unwrap().borrow_mut();
        let mut b = snap.outputs[0].runtime.hub.as_ref().unwrap().borrow_mut();
        let now = Instant::now() + Duration::from_millis(50);
        assert_ne!(a.sample(now).geometry.bounds.height, a.hub.target_size()[1]);
        assert_eq!(b.sample(now).geometry.bounds.height, b.hub.target_size()[1]);
        assert!(a.motion.motion_enabled());
        assert!(!b.motion.motion_enabled());
    }

    #[test]
    fn closing_and_repeated_feedback_preserve_the_outgoing_content() {
        use transition::Event as Transition;
        let mut runtime = Runtime::new(&Panel::default());
        let area = [1000., 700.];
        runtime.transition(
            Transition::Hover {
                entered: true,
                enabled: true,
                peek: (Peek::Media, 240.),
                priority: None,
            },
            area,
        );
        runtime.hub.advance(Duration::from_secs(1), false);
        runtime.transition(Transition::Close, area);
        assert_eq!(
            runtime.previous.peek,
            Peek::Media,
            "the closing fade keeps its media content"
        );
        runtime.hub.advance(Duration::from_secs(1), false);

        let token = Instant::now();
        runtime.transition(Transition::Toast { notice: 7, token }, area);
        runtime.hub.advance(Duration::from_secs(1), false);
        runtime.transition(Transition::ToastExpired(token), area);
        assert_eq!(
            runtime.previous.toast,
            Some(7),
            "expiry keeps the outgoing toast for its fade"
        );
        runtime.hub.advance(Duration::from_secs(1), false);

        runtime.transition(
            Transition::Hover {
                entered: true,
                enabled: true,
                peek: (Peek::Media, 240.),
                priority: None,
            },
            area,
        );
        runtime.hub.advance(Duration::from_secs(1), false);
        runtime.transition(Transition::Workspaces(token), area);
        runtime.hub.advance(Duration::from_millis(60), false);
        runtime.transition(Transition::Workspaces(token + Duration::from_millis(1)), area);
        assert_eq!(
            runtime.previous.peek,
            Peek::Media,
            "repeated feedback must not replace the fading content"
        );
        assert_eq!(runtime.peek, Peek::Workspaces);
    }

    #[test]
    fn stale_transient_expiry_cannot_close_new_content() {
        use transition::Event as Transition;
        let panel = Panel::default();
        let mut runtime = Runtime::new(&panel);
        let area = [1000., 700.];
        let first = Instant::now();
        let second = first + Duration::from_millis(1);
        runtime.transition(
            Transition::Toast {
                notice: 1,
                token: first,
            },
            area,
        );
        runtime.transition(
            Transition::Toast {
                notice: 2,
                token: second,
            },
            area,
        );
        assert!(!runtime.transition(Transition::ToastExpired(first), area));
        assert_eq!(runtime.transient.toast(), Some(2));
        runtime.transition(Transition::Open(status_ui::Menu::Calendar), area);
        assert!(!runtime.transition(Transition::ToastExpired(second), area));
        assert_eq!(runtime.hub.mode(), Mode::Expanded(Content::Calendar));
        runtime.transition(Transition::Close, area);
        runtime.transition(Transition::Workspaces(first), area);
        runtime.transition(
            Transition::Activity {
                priority: Some(Peek::Recording),
                peek: (Peek::Recording, 240.),
            },
            area,
        );
        assert!(!runtime.transition(Transition::WorkspacesExpired(first), area));
        assert_eq!(runtime.peek, Peek::Recording);
        assert!(matches!(runtime.transient, Transient::None));
        assert!(!runtime.transition(
            Transition::Toast {
                notice: 3,
                token: second
            },
            area
        ));
        assert!(!runtime.transition(Transition::Workspaces(second), area));
    }

    #[test]
    fn recording_and_low_battery_take_priority_without_interrupting_open_panels() {
        let mut shell = shell();
        let id = shell.outputs[0].bar;
        std::sync::Arc::make_mut(&mut shell.media.snapshot).selected = Some(ferese_ipc::media::Player {
            title: "Music".into(),
            status: ferese_ipc::media::Playback::Playing,
            ..Default::default()
        });
        drop(shell.hub_hover(id, true));
        assert_eq!(shell.hub_peek_content(id).0, Peek::Media);
        shell.status.battery = Some(crate::status::Battery {
            percent: 12,
            status: "Discharging".into(),
        });
        drop(shell.sync_hub_activity());
        assert_eq!(shell.hub_priority(), Some(Peek::Battery));
        assert_eq!(
            shell.outputs[0].runtime.hub.as_ref().unwrap().borrow().peek,
            Peek::Battery
        );
        shell.recorder.state = crate::recording::State::Recording(std::time::Instant::now());
        drop(shell.sync_hub_activity());
        assert_eq!(shell.hub_priority(), Some(Peek::Recording));
        assert_eq!(
            shell.hub_low_battery(),
            Some(12),
            "battery warning must remain available beside the timer"
        );
        assert_eq!(shell.outputs[0].runtime.hub.as_ref().unwrap().borrow().peek_width, 280.);
        drop(shell.hub_notice(99));
        assert_eq!(
            shell.hub_mode(id),
            Some(Mode::Peek),
            "notifications cannot hide recording"
        );
        shell.status.battery.as_mut().unwrap().status = "Charging".into();
        drop(shell.sync_hub_activity());
        assert_eq!(shell.hub_low_battery(), None);
        assert_eq!(shell.outputs[0].runtime.hub.as_ref().unwrap().borrow().peek_width, 240.);
        shell.recorder.state = crate::recording::State::Saving;
        drop(shell.sync_hub_activity());
        assert_eq!(shell.hub_priority(), Some(Peek::Recording));
        shell.recorder.state = crate::recording::State::Idle;
        drop(shell.sync_hub_activity());
        assert_eq!(
            shell.outputs[0].runtime.hub.as_ref().unwrap().borrow().peek,
            Peek::Media
        );
        drop(shell.open_hub(id, status_ui::Menu::Calendar));
        shell.recorder.state = crate::recording::State::Recording(std::time::Instant::now());
        drop(shell.sync_hub_activity());
        assert_eq!(shell.hub_mode(id), Some(Mode::Expanded(Content::Calendar)));
        drop(shell.close_hub(id));
        assert_eq!(shell.hub_priority(), Some(Peek::Recording));
        assert_eq!(shell.hub_mode(id), Some(Mode::Idle));
    }

    #[test]
    fn low_battery_respects_threshold_charging_and_config_reload() {
        let mut shell = shell();
        let id = shell.outputs[0].bar;
        shell.config.status.low_battery_threshold = 20;
        for (percent, status, expected) in [
            (20, "Discharging", false),
            (19, "Discharging", true),
            (0, "Empty", true),
            (10, "Pending discharge", true),
            (10, "Charging", false),
            (10, "Pending charge", false),
            (10, "Full", false),
            (10, "Unknown", false),
        ] {
            shell.status.battery = Some(crate::status::Battery {
                percent,
                status: status.into(),
            });
            assert_eq!(shell.hub_low_battery().is_some(), expected, "{percent}%, {status}");
        }
        shell.status.battery = Some(crate::status::Battery {
            percent: 15,
            status: "Discharging".into(),
        });
        drop(shell.hub_hover(id, true));
        assert_eq!(
            shell.outputs[0].runtime.hub.as_ref().unwrap().borrow().peek,
            Peek::Battery
        );
        let mut config = shell.config.clone();
        config.status.low_battery_threshold = 10;
        drop(shell.apply_config(config));
        assert_eq!(shell.outputs[0].runtime.hub.as_ref().unwrap().borrow().peek, Peek::Date);
        shell.recorder.state = crate::recording::State::Selecting;
        assert_eq!(shell.hub_priority(), None, "selection is not an active recording");
    }

    #[test]
    fn priority_status_supersedes_workspace_feedback_and_toasts() {
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
        shell.snapshot.outputs[0].active_workspace = 2;
        drop(shell.sync_hub_workspace_feedback(false));
        assert_eq!(shell.hub_mode(id), Some(Mode::Peek));
        shell.recorder.state = crate::recording::State::Recording(std::time::Instant::now());
        drop(shell.sync_hub_activity());
        assert_eq!(shell.hub_mode(id), Some(Mode::Idle));
        assert!(
            shell.outputs[0]
                .runtime
                .hub
                .as_ref()
                .unwrap()
                .borrow()
                .transient
                .workspace_token()
                .is_none()
        );
        shell.snapshot.outputs[0].active_workspace = 3;
        drop(shell.sync_hub_workspace_feedback(false));
        assert_eq!(shell.hub_mode(id), Some(Mode::Idle));
        shell.recorder.state = crate::recording::State::Idle;
        drop(shell.sync_hub_activity());
        drop(shell.set_hub_mode(id, Mode::Toast, None));
        shell.status.battery = Some(crate::status::Battery {
            percent: 1,
            status: "Discharging".into(),
        });
        drop(shell.sync_hub_activity());
        assert_eq!(shell.hub_mode(id), Some(Mode::Idle));
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
        assert!(!shell.overview_active);
        assert_eq!(shell.hub_mode(id), Some(Mode::Expanded(Content::Workspaces)));
    }

    #[test]
    fn workspace_tile_selection_closes_without_confirmation_and_hover_resets() {
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
                output: Some(if id == 3 { 9 } else { 4 }),
                index: id as u32,
                window_count: 0,
                visible: id == 1,
                focused: id == 1,
            })
            .collect();
        drop(shell.sync_hub_workspaces());
        drop(shell.open_hub_overview(id));
        drop(shell.hub_workspace_hover(id, Some(2)));
        drop(shell.hub_workspace_hover(id, None));
        assert_eq!(
            shell.outputs[0].runtime.hub.as_ref().unwrap().borrow().workspace_hover,
            None
        );
        drop(shell.hub_workspace_select(id, 3));
        assert_eq!(
            shell.hub_mode(id),
            Some(Mode::Expanded(Content::Workspaces)),
            "another output must be rejected"
        );
        drop(shell.hub_workspace_select(id, 2));
        shell.snapshot.outputs[0].active_workspace = 2;
        drop(shell.sync_hub_workspace_feedback(false));
        assert_eq!(shell.hub_mode(id), Some(Mode::Idle));
        assert!(!shell.overview_active);
        assert_eq!(
            shell.outputs[0]
                .runtime
                .hub
                .as_ref()
                .unwrap()
                .borrow()
                .workspace_selection_pending,
            None
        );
        shell.snapshot.outputs[0].active_workspace = 1;
        drop(shell.sync_hub_workspace_feedback(false));
        assert_eq!(
            shell.hub_mode(id),
            Some(Mode::Peek),
            "later keyboard switches still get feedback"
        );
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
            runtime.transient.workspace_token().unwrap()
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
            runtime.transient.workspace_token().unwrap()
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
            .transient
            .workspace_token()
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
            geometry: None,
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
    fn date_click_opens_workspaces_in_idle_and_peek_regardless_of_last_tab() {
        use cosmic::iced::advanced::renderer::Headless;
        let renderer = cosmic::iced::futures::executor::block_on(cosmic::Renderer::new(
            cosmic::iced::Font::default(),
            cosmic::iced::Pixels(14.),
            Some("tiny-skia"),
        ))
        .unwrap();
        let mut app = shell();
        let id = app.outputs[0].bar;
        app.snapshot.windows.clear();
        app.snapshot.workspaces.clear();
        app.status.battery = None;
        std::sync::Arc::make_mut(&mut app.media.snapshot).selected = None;
        for last in [status_ui::Menu::Calendar, status_ui::Menu::Activities] {
            app.outputs[0].runtime.hub.as_ref().unwrap().borrow_mut().last_menu = last;
            for mode in [Mode::Idle, Mode::Peek] {
                let mut view = app.hub_content(id, mode, 1., None, true, false);
                let mut tree = widget::Tree::new(&view);
                let viewport = Rectangle::with_size(Size::new(280., 44.));
                let node = view.as_widget_mut().layout(
                    &mut tree,
                    &renderer,
                    &layout::Limits::new(viewport.size(), viewport.size()),
                );
                let mut messages = Vec::new();
                for event in [
                    mouse::Event::ButtonPressed(mouse::Button::Left),
                    mouse::Event::ButtonReleased(mouse::Button::Left),
                ] {
                    view.as_widget_mut().update(
                        &mut tree,
                        &Event::Mouse(event),
                        Layout::new(&node),
                        mouse::Cursor::Available(viewport.center()),
                        &renderer,
                        &mut clipboard::Null,
                        &mut Shell::new(&mut messages),
                        &viewport,
                    );
                }
                let opened: Vec<_> = messages
                    .iter()
                    .filter_map(|message| match message {
                        cosmic::Action::App(Message::HubOpen(_, menu)) => Some(*menu),
                        _ => None,
                    })
                    .collect();
                assert_eq!(opened, vec![status_ui::Menu::Workspaces], "{mode:?}, last tab {last:?}");
            }
        }
    }

    #[test]
    fn collapse_remembers_the_last_selected_panel() {
        let mut shell = shell();
        let id = shell.outputs[0].bar;
        assert_eq!(
            shell.outputs[0].runtime.hub.as_ref().unwrap().borrow().last_menu,
            status_ui::Menu::Workspaces
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
        for (width, count) in [(240., 0), (340., 2), (400., 2)] {
            shell.config.panels[0].morph.expanded_width = width;
            let shortcuts = shell.hub_shortcuts(id);
            assert_eq!(shortcuts.len(), count);
            assert!(!shortcuts.contains(&ItemKind::Network));
            assert_eq!(shortcuts.contains(&ItemKind::Recording), count > 0);
            assert!(!shortcuts.contains(&ItemKind::DisplayMode));
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
            if matches!(item.kind, ItemKind::DisplayMode | ItemKind::Recording) {
                item.overflow = ferese_panel::OverflowPolicy::Always;
            }
        }
        let shortcuts = shell.hub_shortcuts(id);
        assert!(!shortcuts.contains(&ItemKind::Overview));
        assert!(!shortcuts.contains(&ItemKind::DisplayMode));
        assert!(!shortcuts.contains(&ItemKind::Recording));
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
    fn attached_shoulders_fit_the_surface_through_morphs_and_reversals() {
        let mut panel = Panel::from_preset(PanelStyle::Morph);
        panel.morph.attached = true;
        for edge in [Edge::Top, Edge::Bottom] {
            panel.edge = edge;
            let mut runtime = Runtime::new(&panel);
            for mode in [
                Mode::Peek,
                Mode::Expanded(Content::Calendar),
                Mode::Idle,
                Mode::Peek,
                Mode::Idle,
            ] {
                runtime.retarget(mode, [800., 600.], None);
                let allocation = runtime.allocation;
                for _ in 0..18 {
                    runtime.hub.advance(Duration::from_millis(8), false);
                    let sample = runtime.hub.sample();
                    let bounds = sample.geometry.bounds;
                    let scene = ferese_panel::scene::derive(
                        ferese_panel::scene::Parameters {
                            radius: Some(ferese_panel::shape::AnchoredRadii(
                                sample.geometry.radii.map(|r| r as f32),
                            )),
                            ..panel.shape_parameters()
                        },
                        ferese_layout::Rect::new(
                            (f64::from(allocation[0]) - bounds.width) / 2.,
                            0.,
                            bounds.width,
                            bounds.height,
                        ),
                        &[],
                        ferese_panel::shape::PixelGrid::new(1.5).unwrap(),
                    )
                    .unwrap();
                    assert_eq!(scene.paint.len(), 3);
                    for primitive in scene.paint {
                        assert!(primitive.bounds.x >= -0.01);
                        assert!(primitive.bounds.x + primitive.bounds.width <= f64::from(allocation[0]) + 0.01);
                    }
                    assert_eq!(runtime.allocation, allocation, "no resize during settlement");
                }
            }
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
        shell.snapshot.windows = shell
            .snapshot
            .workspaces
            .iter()
            .flat_map(|workspace| {
                (0..workspace.window_count).map(move |n| crate::control::WindowSnapshot {
                    id: workspace.id * 10 + u64::from(n),
                    workspace: workspace.id,
                    app_id: "test".into(),
                    title: format!("Window {}", n + 1),
                    geometry: Some([f64::from(n) * 650., 0., if n == 0 { 640. } else { 320. }, 720.]),
                    focused: false,
                    urgent: false,
                    fullscreen: false,
                    maximized: false,
                    floating: false,
                })
            })
            .collect();
        drop(shell.update_workspace_ui());
        for scale in [1., 1.25, 1.5] {
            shell.outputs[0].scale = scale;
            let mut date_pixels = None;
            for (name, mode, menu) in [
                (
                    "activities-battery-full",
                    Mode::Expanded(Content::Activities),
                    Some(status_ui::Menu::Activities),
                ),
                (
                    "activities-battery-low",
                    Mode::Expanded(Content::Activities),
                    Some(status_ui::Menu::Activities),
                ),
                (
                    "activities-battery-light",
                    Mode::Expanded(Content::Activities),
                    Some(status_ui::Menu::Activities),
                ),
                ("activities-light", Mode::Idle, None),
                ("activities-narrow", Mode::Idle, None),
                ("activities-single", Mode::Idle, None),
                ("activities-split", Mode::Idle, None),
                ("activities-translucent", Mode::Idle, None),
                (
                    "activities-list",
                    Mode::Expanded(Content::Activities),
                    Some(status_ui::Menu::Activities),
                ),
                ("idle", Mode::Idle, None),
                ("idle-attached", Mode::Idle, None),
                ("idle-attached-bottom", Mode::Idle, None),
                ("peek-attached", Mode::Peek, None),
                ("peek-attached-bottom", Mode::Peek, None),
                (
                    "calendar-attached",
                    Mode::Expanded(Content::Calendar),
                    Some(status_ui::Menu::Calendar),
                ),
                (
                    "calendar-attached-bottom",
                    Mode::Expanded(Content::Calendar),
                    Some(status_ui::Menu::Calendar),
                ),
                ("idle-translucent", Mode::Idle, None),
                ("idle-long-title", Mode::Idle, None),
                ("idle-recording", Mode::Idle, None),
                ("idle-low-battery", Mode::Idle, None),
                ("idle-recording-low-battery", Mode::Idle, None),
                ("idle-saving-low-battery", Mode::Idle, None),
                ("peek-recording", Mode::Peek, None),
                ("peek-low-battery", Mode::Peek, None),
                ("peek-recording-low-battery", Mode::Peek, None),
                ("peek-saving-low-battery", Mode::Peek, None),
                ("peek-date", Mode::Peek, None),
                ("peek-date-hover", Mode::Peek, None),
                ("peek-date-pressed", Mode::Peek, None),
                ("peek-media", Mode::Peek, None),
                ("peek-workspaces", Mode::Peek, None),
                (
                    "workspaces",
                    Mode::Expanded(Content::Workspaces),
                    Some(status_ui::Menu::Workspaces),
                ),
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
                std::sync::Arc::make_mut(&mut shell.media.snapshot)
                    .selected
                    .as_mut()
                    .unwrap()
                    .title = if name == "idle-long-title" {
                    "A very long track title that must truncate before the equalizer and never hide its bars".into()
                } else {
                    "Losing My Mind".into()
                };
                shell.recorder.state = if name.contains("recording") {
                    crate::recording::State::Recording(std::time::Instant::now() - Duration::from_secs(65))
                } else if name.contains("saving") {
                    crate::recording::State::Saving
                } else {
                    crate::recording::State::Idle
                };
                shell.status.battery = name.contains("low-battery").then(|| crate::status::Battery {
                    percent: 12,
                    status: "Discharging".into(),
                });
                if name.starts_with("activities-battery-") {
                    shell.status.battery = Some(crate::status::Battery {
                        percent: if name.ends_with("full") {
                            100
                        } else if name.ends_with("low") {
                            9
                        } else {
                            87
                        },
                        status: if name.ends_with("full") {
                            "Full"
                        } else if name.ends_with("low") {
                            "Discharging"
                        } else {
                            "Charging"
                        }
                        .into(),
                    });
                }
                let document = ferese_config::Document::parse(
                    if matches!(name, "activities-light" | "activities-battery-light") {
                        "theme { mode light; }"
                    } else {
                        "theme { mode dark; }"
                    },
                )
                .unwrap();
                let candidate = ferese_config::theme::resolve(
                    &document,
                    std::path::Path::new("/tmp"),
                    jiff::Timestamp::now(),
                    |_| Err("unexpected theme read".into()),
                )
                .unwrap();
                shell.config.apply_theme(&candidate.theme);
                shell.config.theme.bar_background[3] = if name.ends_with("translucent") { 128 } else { 255 };
                let panel = &mut shell.config.panels[0];
                panel.morph.attached = name.contains("attached");
                panel.edge = if name.ends_with("bottom") {
                    Edge::Bottom
                } else {
                    Edge::Top
                };
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
                shell.outputs[0].size = Some((if name == "activities-narrow" { 240 } else { 1200 }, 800));
                let workspace_width = shell.hub_workspace_width(id);
                let hub = shell.outputs[0].runtime.hub.as_ref().unwrap();
                let (mut sample, mut allocation) = {
                    let mut hub = hub.borrow_mut();
                    let animation_time = name.strip_prefix("spring-").map(|ms| ms.parse::<u64>().unwrap());
                    *hub = Runtime::new(&shell.config.panels[0]);
                    hub.workspace_width = if workspace_width > 0. {
                        workspace_width + 10.
                    } else {
                        0.
                    };
                    hub.peek = match name {
                        _ if shell.hub_priority().is_some() => shell.hub_priority().unwrap(),
                        "peek-media" => Peek::Media,
                        "peek-workspaces" => Peek::Workspaces,
                        _ => Peek::Date,
                    };
                    hub.peek_width = if shell.hub_priority().is_some() {
                        shell.hub_peek_content(id).1
                    } else if name == "peek-media" {
                        260.
                    } else {
                        200.
                    };
                    hub.retarget(
                        mode,
                        [if name == "activities-narrow" { 240. } else { 1200. }, 800.],
                        menu,
                    );
                    hub.hub.advance(
                        Duration::from_millis(animation_time.unwrap_or(0)),
                        animation_time.is_none(),
                    );
                    (hub.hub.sample(), hub.allocation)
                };
                if name.starts_with("activities") {
                    seed_activities(&mut shell, if name == "activities-single" { 1 } else { 3 });
                    drop(shell.sync_hub_activity());
                    let mut hub = shell.outputs[0].runtime.hub.as_ref().unwrap().borrow_mut();
                    hub.hub.advance(Duration::from_secs(2), true);
                    hub.activities.advance(Duration::ZERO, true);
                    sample = hub.hub.sample();
                    allocation = hub.allocation;
                } else {
                    shell.activities.snapshot = std::sync::Arc::new(Default::default());
                }
                let hub = shell.outputs[0].runtime.hub.as_ref().unwrap();
                if mode == Mode::Idle || mode == Mode::Expanded(Content::Controls) || name == "peek-workspaces" {
                    let mut measured = shell.view_hub(id);
                    let mut measured_tree = widget::Tree::new(&measured);
                    measured.as_widget_mut().layout(
                        &mut measured_tree,
                        &renderer,
                        &layout::Limits::new(Size::ZERO, Size::new(allocation[0] as f32, allocation[1] as f32)),
                    );
                    let idle_width_limit = f64::from(shell.hub_idle_width_limit(id));
                    let mut runtime = hub.borrow_mut();
                    runtime.hub.advance(Duration::ZERO, true);
                    sample = runtime.hub.sample();
                    if name == "peek-workspaces" {
                        assert!(
                            sample.geometry.bounds.width < 200.,
                            "short workspace feedback should fit its dots and name"
                        );
                    }
                    if mode == Mode::Idle {
                        assert!(
                            sample.geometry.bounds.width <= idle_width_limit,
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
                if shell.hub_priority().is_some() && !mode.expanded() {
                    #[derive(Default)]
                    struct TextBounds(Vec<(String, Rectangle)>);
                    impl widget::Operation for TextBounds {
                        fn traverse(&mut self, visit: &mut dyn FnMut(&mut dyn widget::Operation)) {
                            visit(self);
                        }
                        fn text(&mut self, _: Option<&widget::Id>, bounds: Rectangle, text: &str) {
                            self.0.push((text.to_owned(), bounds));
                        }
                    }
                    let mut bounds = TextBounds::default();
                    view.as_widget_mut()
                        .operate(&mut tree, Layout::new(&node), &renderer, &mut bounds);
                    let left = (viewport.width - sample.geometry.bounds.width as f32) / 2.;
                    let right = left + sample.geometry.bounds.width as f32;
                    for (label, rect) in &bounds.0 {
                        assert!(
                            rect.x >= left && rect.x + rect.width <= right + 0.1,
                            "{name}: {label} must fit inside shell: {rect:?}, {left}..{right}"
                        );
                        assert!(rect.height <= 28., "{name}: {label} must stay on one line: {rect:?}");
                    }
                    assert!(
                        !bounds.0.iter().any(|(text, _)| text == "Losing My Mind"),
                        "media cannot displace priority status"
                    );
                    // Exercise the nested battery button as well as its recording parent.
                    if name == "peek-recording-low-battery" {
                        for (label, expected) in [
                            ("12%", status_ui::Menu::Battery),
                            ("Recording", status_ui::Menu::Overflow),
                        ] {
                            let rect = bounds.0.iter().find(|(text, _)| text == label).unwrap().1;
                            let cursor = mouse::Cursor::Available(rect.center());
                            let mut messages = Vec::new();
                            for event in [
                                mouse::Event::ButtonPressed(mouse::Button::Left),
                                mouse::Event::ButtonReleased(mouse::Button::Left),
                            ] {
                                view.as_widget_mut().update(
                                    &mut tree,
                                    &Event::Mouse(event),
                                    Layout::new(&node),
                                    cursor,
                                    &renderer,
                                    &mut clipboard::Null,
                                    &mut Shell::new(&mut messages),
                                    &viewport,
                                );
                            }
                            let opened: Vec<_> = messages
                                .iter()
                                .filter_map(|message| match message {
                                    cosmic::Action::App(Message::HubOpen(_, menu)) => Some(*menu),
                                    _ => None,
                                })
                                .collect();
                            assert_eq!(opened, vec![expected], "{label} must open exactly its own controls");
                        }
                    }
                }
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
                let theme = shell.config.theme.palette().native_theme();
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
                if name == "idle-translucent" {
                    let x =
                        (((viewport.width + sample.geometry.bounds.width as f32) / 2. - 8.) * scale as f32) as usize;
                    let y = (sample.geometry.bounds.height * scale / 2.) as usize;
                    let alpha = pixels[(y * physical.width as usize + x) * 4 + 3];
                    assert!(alpha.abs_diff(128) <= 2, "Morph must inherit theme alpha, got {alpha}");
                }
                if name == "idle-long-title" {
                    let right = (viewport.width + sample.geometry.bounds.width as f32) / 2.;
                    let left_x = ((right - 28.) * scale as f32).floor() as usize;
                    let right_x = ((right - 14.) * scale as f32).ceil() as usize;
                    let accent = shell.config.theme.accent;
                    assert!(
                        pixels.chunks_exact(4).enumerate().any(|(index, pixel)| {
                            let x = index % physical.width as usize;
                            (left_x..right_x).contains(&x)
                                && pixel[3] > 200
                                && pixel[..3]
                                    .iter()
                                    .zip(accent[..3].iter())
                                    .all(|(a, b)| a.abs_diff(*b) <= 2)
                        }),
                        "long titles must leave the equalizer visible at the right edge"
                    );
                }

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
    fn content_layout_stays_fixed_through_spring_settlement_and_rebuilds() {
        use cosmic::iced::advanced::renderer::Headless;
        #[derive(Default)]
        struct Bounds {
            content: Option<Rectangle>,
            text: Vec<(String, Rectangle)>,
        }
        impl widget::Operation for Bounds {
            fn traverse(&mut self, visit: &mut dyn FnMut(&mut dyn widget::Operation)) {
                visit(self);
            }
            fn container(&mut self, id: Option<&widget::Id>, bounds: Rectangle) {
                if id == Some(&widget::Id::new("ferese-hub-content")) {
                    self.content = Some(bounds);
                }
            }
            fn text(&mut self, _: Option<&widget::Id>, bounds: Rectangle, text: &str) {
                self.text.push((text.to_owned(), bounds));
            }
        }
        fn close(a: Rectangle, b: Rectangle) {
            for (a, b) in [(a.x, b.x), (a.y, b.y), (a.width, b.width), (a.height, b.height)] {
                assert!((a - b).abs() < 0.001, "content moved during settlement: {a} vs {b}");
            }
        }
        let renderer = cosmic::iced::futures::executor::block_on(cosmic::Renderer::new(
            cosmic::iced::Font::default(),
            cosmic::iced::Pixels(14.),
            Some("tiny-skia"),
        ))
        .unwrap();
        let mut shell = shell();
        let id = shell.outputs[0].bar;
        for edge in [Edge::Top, Edge::Bottom] {
            shell.config.panels[0].edge = edge;
            for scale in [1., 1.25, 1.5] {
                shell.outputs[0].scale = scale;
                for mode in [Mode::Peek, Mode::Expanded(Content::Calendar), Mode::Idle] {
                    let runtime = shell.outputs[0].runtime.hub.as_ref().unwrap();
                    runtime.borrow_mut().retarget(mode, [1200., 800.], None);
                    let allocation = runtime.borrow().allocation;
                    let mut baseline: Option<Bounds> = None;
                    let mut elapsed = 0;
                    for at in [180, 250, 400, 650, 1000, 1600, 1600] {
                        let sample = {
                            let mut runtime = runtime.borrow_mut();
                            runtime.hub.advance(Duration::from_millis(at - elapsed), false);
                            elapsed = at;
                            runtime.hub.sample()
                        };
                        let mut view = shell.view_hub_at(id, sample, None, None);
                        let mut tree = widget::Tree::new(&view);
                        let node = view.as_widget_mut().layout(
                            &mut tree,
                            &renderer,
                            &layout::Limits::new(Size::ZERO, Size::new(allocation[0] as f32, allocation[1] as f32)),
                        );
                        let mut bounds = Bounds::default();
                        view.as_widget_mut()
                            .operate(&mut tree, Layout::new(&node), &renderer, &mut bounds);
                        assert!(bounds.content.is_some());
                        assert!(!bounds.text.is_empty());
                        if let Some(baseline) = &baseline {
                            close(bounds.content.unwrap(), baseline.content.unwrap());
                            assert_eq!(bounds.text.len(), baseline.text.len());
                            for ((text, rect), (expected_text, expected_rect)) in bounds.text.iter().zip(&baseline.text)
                            {
                                assert_eq!(text, expected_text);
                                close(*rect, *expected_rect);
                            }
                        } else {
                            baseline = Some(bounds);
                        }
                    }
                }
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
