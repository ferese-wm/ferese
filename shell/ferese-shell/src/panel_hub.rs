use std::cell::RefCell;
use std::time::Instant;

use crate::KeyboardInteractivity;
use cosmic::app::Task;
use cosmic::iced::platform_specific::shell::commands::layer_surface::{set_keyboard_interactivity, set_size};
use cosmic::iced::{Alignment, Length, window};
use cosmic::widget::{button, column, container, mouse_area, row, scrollable};
use cosmic::{Element, theme};
use ferese_panel::morph::{Content, Hub, Mode, Sample};
use ferese_panel::{Edge, ItemKind, Panel, PanelStyle};

use crate::{FereseShell, Message, color_with_opacity, motion, status_ui, text};

pub(super) struct Runtime {
    pub(super) hub: Hub,
    pub(super) menu: Option<status_ui::Menu>,
    previous_menu: Option<status_ui::Menu>,
    toast: Option<u32>,
    pub(super) allocation: [u32; 2],
    pub(super) revision: Instant,
    sampled: Instant,
}

impl Runtime {
    pub(super) fn new(panel: &Panel) -> Self {
        let mut hub = Hub::new(panel.morph, panel.resolved_geometry().height.into()).expect("validated panel hub");
        hub.pin_radii(panel.corner_radius.map(|r| r.0.map(f64::from)))
            .expect("validated panel radii");
        hub.advance(std::time::Duration::ZERO, true);
        let now = Instant::now();
        Self {
            allocation: hub.target_size().map(|n| n.ceil() as u32),
            hub,
            menu: None,
            previous_menu: None,
            toast: None,
            revision: now,
            sampled: now,
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
        let before = self.sample(now);
        if before.opacity >= before.previous_opacity {
            self.previous_menu = self.menu;
        }
        let replace_content = self.hub.mode() == mode && self.menu != menu;
        self.menu = menu;
        self.hub.retarget(mode, available).expect("bounded hub area");
        if replace_content {
            self.hub.replace_content();
        }
        let target = self.hub.target_size().map(|n| n.ceil() as u32);
        // Allocate before growing; shrink only after the visual has settled.
        self.allocation = std::array::from_fn(|i| self.allocation[i].max(target[i]));
        self.revision = now;
    }

    fn settle(&mut self, revision: Instant) -> bool {
        if self.revision != revision || self.hub.active() {
            return false;
        }
        let target = self.hub.target_size().map(|n| n.ceil() as u32);
        if self.allocation == target {
            return false;
        }
        self.allocation = target;
        true
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
        match self.hub_mode(id) {
            Some(Mode::Idle) if entered => self.set_hub_mode(id, Mode::Peek, None),
            Some(Mode::Peek) if !entered => self.set_hub_mode(id, Mode::Idle, None),
            _ => Task::none(),
        }
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
        hub.allocation = hub.hub.target_size().map(|value| value.ceil() as u32);
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
        let size = {
            let mut hub = hub.borrow_mut();
            hub.retarget(mode, available, menu);
            hub.allocation
        };
        Task::batch([
            set_size(id, Some(size[0]), Some(size[1])),
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

    pub(super) fn hub_settled(&mut self, id: window::Id, revision: Instant) -> Task<Message> {
        let Some(hub) = self
            .outputs
            .iter()
            .find(|entry| entry.bar == id)
            .and_then(|entry| entry.runtime.hub.as_ref())
        else {
            return Task::none();
        };
        let mut hub = hub.borrow_mut();
        if hub.settle(revision) {
            set_size(id, Some(hub.allocation[0]), Some(hub.allocation[1]))
        } else {
            Task::none()
        }
    }

    pub(super) fn view_hub(&self, id: window::Id) -> Element<'_, cosmic::Action<Message>> {
        let entry = self.outputs.iter().find(|entry| entry.bar == id).expect("panel output");
        let runtime = entry.runtime.hub.as_ref().expect("Morph runtime");
        let revision = runtime.borrow().revision;
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
            move |_| (!runtime.borrow().hub.active()).then_some(cosmic::Action::App(Message::HubSettled(id, revision))),
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
            layers = layers.push(self.hub_content(id, sample.previous, sample.previous_opacity, previous_menu, false));
        }
        if sample.opacity > 0. {
            layers = layers.push(self.hub_content(id, sample.current, sample.opacity, menu, true));
        }
        let body = container(layers)
            .id("ferese-panel-hub")
            .width(bounds.width as f32)
            .height(bounds.height as f32)
            .clip(true);
        let body = mouse_area(body)
            .on_enter(cosmic::Action::App(Message::HubHover(id, true)))
            .on_exit(cosmic::Action::App(Message::HubHover(id, false)));
        let root = container(body)
            .center_x(Length::Fill)
            .height(Length::Fill)
            .align_y(if panel.edge == Edge::Bottom {
                cosmic::iced::alignment::Vertical::Bottom
            } else {
                cosmic::iced::alignment::Vertical::Top
            });
        let mut palette = self.config.theme.for_bar();
        palette.bar_background[3] = (palette.panel_opacity(panel.background_opacity) * 255.).round() as u8;
        ferese_theme::panel_surface::frame(
            root.into(),
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
                background: crate::color(palette.bar_background),
                border: if panel.border {
                    crate::color(palette.border)
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
    ) -> Element<'_, cosmic::Action<Message>> {
        let palette = self.config.theme;
        let primary = color_with_opacity(palette.text_primary, opacity);
        let muted = color_with_opacity(palette.text_muted, opacity);
        let content: Element<'_, cosmic::Action<Message>> = match mode {
            Mode::Idle | Mode::Peek | Mode::Toast => {
                let time = self
                    .clock
                    .split_once(", ")
                    .map_or(self.clock.as_str(), |(_, time)| time);
                let active = self.recorder.busy()
                    || self
                        .media
                        .snapshot
                        .selected
                        .as_ref()
                        .is_some_and(|player| player.status == ferese_ipc::media::Playback::Playing);
                let activity = text("●").size(8).class(theme::Text::Color(if active {
                    color_with_opacity(palette.accent, opacity)
                } else {
                    muted.scale_alpha(0.4)
                }));
                let label = if mode == Mode::Toast {
                    self.outputs
                        .iter()
                        .find(|entry| entry.bar == id)
                        .and_then(|entry| entry.runtime.hub.as_ref())
                        .and_then(|hub| hub.borrow().toast)
                        .and_then(|id| self.notifications.entries.iter().find(|notice| notice.id == id))
                        .map_or_else(|| time.to_owned(), |notice| notice.title.clone())
                } else if mode == Mode::Peek {
                    if self.recorder.busy() {
                        self.recorder.label().to_owned()
                    } else if let Some(player) = &self.media.snapshot.selected {
                        player.label().to_owned()
                    } else {
                        time.to_owned()
                    }
                } else {
                    time.to_owned()
                };
                let kind = if mode == Mode::Toast {
                    status_ui::Menu::Notifications
                } else if self.recorder.busy() {
                    status_ui::Menu::Recording
                } else if active {
                    status_ui::Menu::Media
                } else {
                    status_ui::Menu::Calendar
                };
                motion::button(
                    button::custom(
                        container(
                            row![
                                activity,
                                text(label).size(13).class(theme::Text::Color(primary)).ellipsize(
                                    cosmic::iced::widget::text::Ellipsize::End(
                                        cosmic::iced::advanced::text::EllipsizeHeightLimit::Lines(1)
                                    )
                                )
                            ]
                            .spacing(8)
                            .align_y(Alignment::Center),
                        )
                        .center_x(Length::Fill)
                        .center_y(Length::Fill),
                    )
                    .name("Expand panel hub")
                    .padding([0, 14])
                    .width(Length::Fill)
                    .height(Length::Fill)
                    .on_press(cosmic::Action::App(Message::HubOpen(id, kind))),
                    primary,
                    false,
                    opacity,
                )
            }
            Mode::Expanded(selected) => {
                let mut tabs = row::with_capacity(5)
                    .width(Length::Fill)
                    .spacing(2)
                    .align_y(Alignment::Center);
                for (kind, label) in [
                    (status_ui::Menu::Media, "Music"),
                    (status_ui::Menu::Calendar, "Calendar"),
                    (status_ui::Menu::Notifications, "Alerts"),
                    (status_ui::Menu::Overflow, "Controls"),
                ] {
                    tabs = tabs.push(motion::button(
                        button::custom(text(label).size(11).class(theme::Text::Color(primary)))
                            .name(label)
                            .padding([6, 6])
                            .width(Length::Fill)
                            .height(28)
                            .on_press(cosmic::Action::App(Message::HubOpen(id, kind))),
                        primary,
                        selected == content(kind),
                        opacity,
                    ));
                }
                tabs = tabs.push(motion::button(
                    button::custom(text("×").size(18).class(theme::Text::Color(muted)))
                        .name("Collapse panel hub")
                        .padding(0)
                        .width(24)
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
                                .width(3)
                                .scroller_width(3)
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
        if interactive && opacity >= 0.5 {
            content
        } else {
            content.map(|_| cosmic::Action::App(Message::HubIgnore))
        }
    }

    fn hub_controls(&self, id: window::Id, opacity: f32) -> Element<'_, cosmic::Action<Message>> {
        let primary = color_with_opacity(self.config.theme.text_primary, opacity);
        let muted = color_with_opacity(self.config.theme.text_muted, opacity);
        let mut rows = column::with_capacity(12).spacing(4).width(Length::Fill);
        let panel = self.panel_for_bar(id);
        for item in [&panel.start, &panel.center, &panel.end]
            .into_iter()
            .flat_map(|zone| &zone.groups)
            .flat_map(|group| &group.items)
            .filter(|item| item.visible)
        {
            let message = match item.kind {
                ItemKind::Overview => Message::ToggleOverview,
                ItemKind::Workspaces { .. } => {
                    let mut workspaces = column::with_capacity(self.snapshot.workspaces.len()).spacing(2);
                    for workspace in self
                        .snapshot
                        .workspaces_for_output(self.output_for_bar(id).map(|output| output.id))
                    {
                        workspaces = workspaces.push(self.hub_button(
                            &workspace.name,
                            Message::ActivateWorkspace(workspace.id),
                            primary,
                            opacity,
                        ));
                    }
                    rows = rows
                        .push(text(item.kind.label()).size(11).class(theme::Text::Color(muted)))
                        .push(workspaces);
                    continue;
                }
                ItemKind::FocusedWindow => {
                    rows = rows.push(
                        text(crate::bar::focused_bar_title(&self.snapshot, self.output_for_bar(id)))
                            .size(12)
                            .class(theme::Text::Color(muted)),
                    );
                    continue;
                }
                ItemKind::Media => Message::HubOpen(id, status_ui::Menu::Media),
                ItemKind::QuickSettings => Message::HubOpen(id, status_ui::Menu::System),
                ItemKind::Network => Message::HubOpen(id, status_ui::Menu::Network),
                ItemKind::Audio => Message::HubOpen(id, status_ui::Menu::Audio),
                ItemKind::Recording => Message::HubOpen(id, status_ui::Menu::Recording),
                ItemKind::Notifications => Message::HubOpen(id, status_ui::Menu::Notifications),
                ItemKind::Battery { .. } => Message::HubOpen(id, status_ui::Menu::Battery),
                ItemKind::Clock => Message::HubOpen(id, status_ui::Menu::Calendar),
                ItemKind::DisplayMode => Message::OpenDisplays(
                    self.outputs
                        .iter()
                        .find(|entry| entry.bar == id)
                        .and_then(|entry| entry.name.clone()),
                ),
                ItemKind::Overflow => continue,
            };
            rows = rows.push(self.hub_button(item.kind.label(), message, primary, opacity));
        }
        rows.into()
    }

    fn hub_button<'a>(
        &self,
        label: &str,
        message: Message,
        primary: cosmic::iced::Color,
        opacity: f32,
    ) -> Element<'a, cosmic::Action<Message>> {
        motion::button(
            button::custom(text(label.to_owned()).size(12).class(theme::Text::Color(primary)))
                .name(label.to_owned())
                .padding([7, 8])
                .width(Length::Fill)
                .on_press(cosmic::Action::App(message)),
            primary,
            false,
            opacity,
        )
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
        shell.outputs[0]
            .runtime
            .reset(&shell.config.panels[0], &shell.snapshot, Some("test"));
        shell.clock = "Sun, 10:09".into();
        shell
    }

    #[test]
    fn growth_allocates_first_and_collapse_releases_only_after_settlement() {
        let panel = Panel::from_preset(PanelStyle::Morph);
        let mut runtime = Runtime::new(&panel);
        assert_eq!(runtime.allocation, [150, 34]);
        runtime.retarget(
            Mode::Expanded(Content::Calendar),
            [1000., 700.],
            Some(status_ui::Menu::Calendar),
        );
        let old_revision = runtime.revision;
        assert_eq!(runtime.allocation, [340, 230]);
        runtime.hub.advance(Duration::from_millis(50), false);
        let before = runtime.hub.sample().geometry;
        runtime.retarget(Mode::Idle, [1000., 700.], None);
        // A reversal never commits a smaller buffer underneath an unfinished shape.
        assert_eq!(runtime.allocation, [340, 230]);
        assert!(runtime.hub.sample().geometry.bounds.width >= before.bounds.width);
        assert!(!runtime.settle(old_revision));
        assert!(!runtime.settle(runtime.revision));
        runtime.hub.advance(Duration::from_secs(2), false);
        runtime.hub.advance(Duration::from_millis(120), false);
        assert!(runtime.settle(runtime.revision));
        assert_eq!(runtime.allocation, [150, 34]);
        assert!(!runtime.hub.active());
    }

    #[test]
    fn hover_leave_cannot_close_an_expanded_hub_and_reload_clears_its_owner() {
        let mut shell = shell();
        let id = shell.outputs[0].bar;
        let reservation = shell.config.panels[0].exclusive_zone(false);
        drop(shell.update(Message::HubHover(id, true)));
        assert_eq!(shell.hub_mode(id), Some(Mode::Peek));
        drop(shell.update(Message::HubHover(id, false)));
        assert_eq!(shell.hub_mode(id), Some(Mode::Idle));
        drop(shell.update(Message::HubOpen(id, status_ui::Menu::Calendar)));
        assert!(shell.menu.is_none(), "expanded hub owns no popup surface");
        drop(shell.update(Message::HubHover(id, false)));
        assert_eq!(shell.hub_mode(id), Some(Mode::Expanded(Content::Calendar)));
        assert_eq!(shell.config.panels[0].exclusive_zone(false), reservation);
        drop(shell.update(Message::HubClose(id)));
        assert_eq!(shell.hub_mode(id), Some(Mode::Idle));
        let mut config = shell.config.clone();
        config.panels[0].style = PanelStyle::Floating;
        drop(shell.apply_config(config));
        assert_eq!(shell.hub_mode(id), None);
        drop(shell.update(Message::HubSettled(id, Instant::now())));
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
        assert_eq!(runtime.allocation, [150, 34]);
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
        let theme = shell.config.theme.palette().native_theme();
        for scale in [1., 1.25, 1.5] {
            shell.outputs[0].scale = scale;
            for (name, mode, menu) in [
                ("idle", Mode::Idle, None),
                ("peek", Mode::Peek, None),
                ("music", Mode::Expanded(Content::Media), Some(status_ui::Menu::Media)),
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
            ] {
                let hub = shell.outputs[0].runtime.hub.as_ref().unwrap();
                let (sample, allocation) = {
                    let mut hub = hub.borrow_mut();
                    hub.retarget(mode, [1200., 800.], menu);
                    hub.hub.advance(Duration::ZERO, true);
                    let revision = hub.revision;
                    hub.settle(revision);
                    (hub.hub.sample(), hub.allocation)
                };
                let mut view = shell.view_hub_at(id, sample, menu, None);
                let mut tree = widget::Tree::new(&view);
                let viewport = Rectangle::with_size(Size::new(allocation[0] as f32, allocation[1] as f32));
                let node = view.as_widget_mut().layout(
                    &mut tree,
                    &renderer,
                    &layout::Limits::new(Size::ZERO, viewport.size()),
                );
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
                    mouse::Cursor::Unavailable,
                    &viewport,
                );
                let physical = Size::new(
                    (allocation[0] as f64 * scale).ceil() as u32,
                    (allocation[1] as f64 * scale).ceil() as u32,
                );
                let pixels = renderer.screenshot(physical, scale as f32, cosmic::iced::Color::TRANSPARENT);
                assert!(pixels.chunks_exact(4).any(|pixel| pixel[3] > 0));
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
        // Keep the first animation sample at idle while the backing surface is already expanded.
        let hub = shell.outputs[0].runtime.hub.as_ref().unwrap();
        hub.borrow_mut().sampled = Instant::now() + Duration::from_secs(1);
        for edge in [Edge::Top, Edge::Bottom] {
            shell.config.panels[0].edge = edge;
            let mut view = shell.view_layer(id);
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
}
