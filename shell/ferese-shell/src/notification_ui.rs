use cosmic::iced::border::Shape as BorderShape;
use cosmic::iced::widget::scrollable::{Direction, Scrollbar};
use cosmic::widget::{column, scrollable};

use super::*;

const POPUP_WIDTH: u32 = 368;
const HISTORY_MAX_HEIGHT: f32 = 620.0;

fn popup_height(notice: &notifications::Notice, count: usize) -> u32 {
    let body = if notice.body.is_empty() && count == 1 { 0 } else { 22 };
    let actions = if notice.live && notice.actions.iter().any(|(key, _)| key != "default") {
        39
    } else {
        0
    };
    76 + body + actions + if count > 1 { 4 } else { 0 }
}

fn blend(base: Color, foreground: Color, amount: f32) -> Color {
    Color {
        r: base.r + (foreground.r - base.r) * amount,
        g: base.g + (foreground.g - base.g) * amount,
        b: base.b + (foreground.b - base.b) * amount,
        a: base.a,
    }
}

fn age_label(age: Duration) -> String {
    match age.as_secs() {
        0..60 => "now".into(),
        60..3600 => format!("{}m ago", age.as_secs() / 60),
        3600..86400 => format!("{}h ago", age.as_secs() / 3600),
        seconds => format!("{}d ago", seconds / 86400),
    }
}

fn card_button<'a>(
    content: impl Into<Element<'a, cosmic::Action<Message>>>,
    message: Message,
    foreground: Color,
    hover: Color,
    round: bool,
    accessible_name: String,
) -> Element<'a, cosmic::Action<Message>> {
    button::custom(content)
        .name(accessible_name)
        .width(if round { Length::Shrink } else { Length::Fill })
        .padding(0)
        .on_press(cosmic::Action::App(message))
        .class(ferese_theme::controls::notification_button(
            foreground,
            hover,
            if round { motion::radius(11.) } else { 0. },
            round,
        ))
        .into()
}

fn card_surface<'a>(
    content: impl Into<Element<'a, cosmic::Action<Message>>>,
    history: bool,
    style: container::Style,
) -> Element<'a, cosmic::Action<Message>> {
    container(content)
        .id(if history {
            "ferese-history-card"
        } else {
            "ferese-blur-card"
        })
        .width(Length::Fill)
        .clip_to_border(true)
        .class(theme::Container::custom(move |_| style))
        .into()
}

fn icon_control<'a>(
    source: &'static [u8],
    label: &'static str,
    message: Message,
    foreground: Color,
    background: Color,
    extent: u16,
) -> Element<'a, cosmic::Action<Message>> {
    let control = card_button(
        container(bar_icon(source, 14, foreground))
            .center_x(extent)
            .center_y(extent),
        message,
        foreground,
        background,
        true,
        label.into(),
    );
    cosmic::widget::tooltip(
        control,
        text(label).size(11).class(theme::Text::Color(foreground)),
        cosmic::widget::tooltip::Position::Left,
    )
    .class(theme::Container::custom(move |_| container::Style {
        background: Some(Background::Color(background)),
        border: Border {
            shape: BorderShape::Continuous,
            radius: motion::radius(12.0).into(),
            ..Default::default()
        },
        text_color: Some(foreground),
        ..Default::default()
    }))
    .into()
}

pub(super) struct NotificationSurface {
    pub id: window::Id,
    output: wl_output::WlOutput,
    height: u32,
    pub effects: Option<EffectsBinding>,
    pub regions: motion::Regions,
}

impl FereseShell {
    pub(super) fn notification_history_height_limit(&self) -> f32 {
        self.outputs
            .iter()
            .find(|output| output.bar == self.bar_surface_id)
            .and_then(|output| output.size)
            .map_or(HISTORY_MAX_HEIGHT, |(_, height)| {
                let theme = self.config.theme;
                (height as f32 - theme.bar_margin_top as f32 - theme.bar_height - 8.0 - 12.0).max(1.0)
            })
            .min(HISTORY_MAX_HEIGHT)
    }

    pub(super) fn toggle_notification_history(&mut self) -> Task<Message> {
        if self.notifications.history_open {
            return self.close_menu();
        }
        // Cards can open history too. Anchor those requests to the active bar;
        // bell clicks pass their exact anchor through the normal menu path.
        if let Some(output) = self.outputs.iter().find(|entry| {
            self.snapshot
                .outputs
                .iter()
                .any(|output| output.focused && Some(output.name.as_str()) == entry.name.as_deref())
        }) {
            self.bar_surface_id = output.bar;
        }
        let width = self
            .outputs
            .iter()
            .find(|entry| entry.bar == self.bar_surface_id)
            .and_then(|entry| entry.size)
            .map_or(POPUP_WIDTH as i32, |size| size.0);
        self.open_menu(
            status_ui::Menu::Notifications,
            cosmic::iced::Rectangle {
                x: (width - 48).max(0),
                y: 0,
                width: 24,
                height: self.config.theme.bar_height.round() as i32,
            },
        )
    }

    pub(super) fn close_notification_history(&mut self) -> Task<Message> {
        self.close_menu()
    }

    pub(super) fn sync_notification_surface(&mut self) -> Task<Message> {
        // Hidden toast surfaces receive no frame callbacks. Their closes have
        // no visible transition to finish, so release them immediately.
        self.notifications.tick();
        if self.notifications.ready {
            self.status.notifications = Some(status::Notifications {
                count: self.notifications.unread(),
                dnd: self.notifications.dnd,
            });
        }
        let count = self.notifications.popup_groups().len();
        let wanted = self.notifications.ready && !self.notifications.history_open && count > 0;
        let selected = self
            .outputs
            .iter()
            .find(|entry| {
                self.snapshot
                    .outputs
                    .iter()
                    .any(|output| output.focused && Some(output.name.as_str()) == entry.name.as_deref())
            })
            .or_else(|| self.outputs.first())
            .map(|entry| entry.output.clone());
        let mut tasks = Vec::new();
        if self
            .notification_surface
            .as_ref()
            .is_some_and(|surface| !wanted || selected.as_ref() != Some(&surface.output))
        {
            let surface = self.notification_surface.take().unwrap();
            tasks.push(destroy_layer_surface(surface.id));
        }
        if !wanted {
            return Task::batch(tasks);
        }
        let Some(output) = selected else {
            return Task::batch(tasks);
        };
        let desired_height = self
            .notifications
            .popup_groups()
            .iter()
            .map(|(notice, _, count)| popup_height(notice, *count) + if *count > 1 { 12 } else { 0 })
            .sum::<u32>()
            + count.saturating_sub(1) as u32 * 8
            + 8;
        let output_height = self
            .outputs
            .iter()
            .find(|entry| entry.output == output)
            .and_then(|entry| entry.size)
            .map_or(1080, |size| size.1);
        let top = self.config.theme.bar_height + self.config.theme.bar_margin_top as f32 + 12.0;
        let height = desired_height.min((output_height as f32 - top - 12.0).max(100.0) as u32);
        if let Some(surface) = &mut self.notification_surface {
            if surface.height != height {
                surface.height = height;
                tasks.push(set_size(surface.id, Some(POPUP_WIDTH), Some(height)));
            }
        } else {
            let id = window::Id::unique();
            self.notification_surface = Some(NotificationSurface {
                id,
                output: output.clone(),
                height,
                effects: None,
                regions: Default::default(),
            });
            let top = self.config.theme.bar_height + self.config.theme.bar_margin_top as f32 + 12.0;
            let action = cosmic::surface::action::app_layer_shell::<Self>(
                |_| Default::default(),
                move |_| SctkLayerSurfaceSettings {
                    id,
                    layer: Layer::Overlay,
                    keyboard_interactivity: KeyboardInteractivity::None,
                    anchor: Anchor::TOP | Anchor::RIGHT,
                    output: IcedOutput::Output(output.clone()),
                    margin: IcedMargin {
                        top: top as i32,
                        right: 12,
                        ..Default::default()
                    },
                    exclusive_zone: -1,
                    size: Some((Some(POPUP_WIDTH), Some(height))),
                    size_limits: Limits::NONE,
                    namespace: "ferese-shell-notifications".into(),
                    ..Default::default()
                },
                Some(Box::new(Self::view_notifications)),
            );
            tasks.push(cosmic::task::message(cosmic::Action::Surface(action)));
        }
        Task::batch(tasks)
    }

    fn notification_card(
        &self,
        notice: &notifications::Notice,
        opacity: f32,
        history: bool,
        count: usize,
    ) -> Element<'_, cosmic::Action<Message>> {
        let palette = self.config.theme;
        let tint = move |rgba| {
            let mut value = color(rgba);
            value.a *= opacity;
            value
        };
        let foreground = tint(palette.text_primary);
        let muted = tint(palette.text_muted);
        let accent = tint(palette.accent);
        let base = tint(if history {
            palette.surface_base
        } else {
            palette.surface_popover
        });
        let compositor_material = !history
            && self
                .notification_surface
                .as_ref()
                .is_some_and(|surface| surface.effects.is_some());
        let divider = tint(palette.border);
        let well = blend(base, foreground, 0.07);
        let id = notice.id;
        let mut bold = *SHELL_FONT.get().unwrap().read().unwrap();
        bold.weight = cosmic::iced::font::Weight::Bold;
        let app_icon = if notice.app.to_lowercase().starts_with("ferese") || notice.icon.is_empty() {
            accented_icon(ferese_theme::icons::FERESE, 18, accent, accent)
        } else if notice.icon.starts_with('/') {
            icon::icon(icon::from_path(notice.icon.clone().into())).size(18)
        } else {
            icon::from_name(notice.icon.as_str()).size(18).icon()
        }
        .opacity(opacity);
        let icon_well = container(app_icon)
            .center_x(30)
            .center_y(30)
            .class(theme::Container::custom(move |_| container::Style {
                background: Some(Background::Color(well)),
                border: Border {
                    shape: BorderShape::Continuous,
                    radius: motion::radius(15.0).into(),
                    ..Default::default()
                },
                ..Default::default()
            }));
        let icon: Element<'_, cosmic::Action<Message>> = if count > 1 {
            let badge_fill = ferese_theme::composite(
                color(palette.accent),
                color(if history {
                    palette.surface_base
                } else {
                    palette.surface_popover
                }),
            );
            let (badge_fill, badge_text) = ferese_theme::accent_pair(badge_fill, color(palette.text_primary));
            let badge = container(
                text(count.to_string())
                    .size(10)
                    .font(bold)
                    .class(theme::Text::Color(badge_text.scale_alpha(opacity))),
            )
            .center_x(16)
            .center_y(16)
            .class(theme::Container::custom(move |_| container::Style {
                background: Some(Background::Color(badge_fill.scale_alpha(opacity))),
                border: Border {
                    shape: BorderShape::Continuous,
                    radius: motion::radius(8.0).into(),
                    ..Default::default()
                },
                ..Default::default()
            }));
            cosmic::iced::widget::stack([
                container(icon_well)
                    .padding(cosmic::iced::Padding {
                        top: 4.0,
                        ..Default::default()
                    })
                    .into(),
                container(badge).align_right(34).into(),
            ])
            .into()
        } else {
            icon_well.into()
        };
        let app = if notice.app.is_empty() {
            "NOTIFICATION".to_owned()
        } else {
            notice.app.to_uppercase()
        };
        let app = if count > 1 {
            format!("{app} · {count} NOTIFICATIONS")
        } else {
            app
        };
        let header = row([])
            .spacing(10)
            .align_y(alignment::Vertical::Center)
            .push(icon)
            .push(
                container(text(app).size(11).font(bold).class(theme::Text::Color(muted)))
                    .width(Length::Fill)
                    .height(16)
                    .clip(true),
            )
            .push(
                text(age_label(notice.received_at.elapsed()))
                    .size(11)
                    .class(theme::Text::Color(muted)),
            )
            .push(icon_control(
                ferese_theme::icons::CLOSE,
                "Dismiss notification",
                if history && count > 1 {
                    Message::RemoveNotificationGroup(notice.app.clone())
                } else if history {
                    Message::RemoveNotification(id)
                } else {
                    Message::DismissNotification(id)
                },
                muted,
                well,
                22,
            ));
        let mut content = column([]).spacing(4).push(header).push(
            container(
                text(notice.title.clone())
                    .size(14)
                    .font(bold)
                    .class(theme::Text::Color(foreground)),
            )
            .height(18)
            .clip(true),
        );
        let body = if count > 1 {
            if notice.body.is_empty() {
                format!("{} more", count - 1)
            } else {
                format!("{} · {} more", notice.body, count - 1)
            }
        } else {
            notice.body.clone()
        };
        if !body.is_empty() {
            content = content.push(
                container(text(body).size(13).class(theme::Text::Color(muted)))
                    .height(if history && count == 1 {
                        Length::Shrink
                    } else {
                        Length::Fixed(18.0)
                    })
                    .clip(true),
            );
        }
        let mut face = column([]).push(container(content).padding([12, 14]).width(Length::Fill));
        if notice.live {
            let available: Vec<_> = notice
                .actions
                .iter()
                .filter(|(key, _)| key != "default")
                .take(2)
                .collect();
            if !available.is_empty() {
                let mut actions = row([]);
                for (index, (key, label)) in available.into_iter().enumerate() {
                    if index > 0 {
                        actions =
                            actions.push(container(cosmic::iced::widget::Space::new().width(1).height(38)).class(
                                theme::Container::custom(move |_| container::Style {
                                    background: Some(Background::Color(divider)),
                                    ..Default::default()
                                }),
                            ));
                    }
                    actions = actions.push(card_button(
                        container(
                            text(label.clone())
                                .font(bold)
                                .size(13)
                                .class(theme::Text::Color(accent)),
                        )
                        .center_x(Length::Fill)
                        .center_y(38),
                        Message::InvokeNotification(id, key.clone()),
                        accent,
                        well,
                        false,
                        label.clone(),
                    ));
                }
                face = face
                    .push(
                        container(cosmic::iced::widget::Space::new().width(Length::Fill).height(1)).class(
                            theme::Container::custom(move |_| container::Style {
                                background: Some(Background::Color(divider)),
                                ..Default::default()
                            }),
                        ),
                    )
                    .push(actions);
            }
        }
        let card = card_surface(
            face,
            history,
            container::Style {
                background: if history {
                    Some(Background::Color(Color {
                        a: 0.045 * opacity,
                        ..foreground
                    }))
                } else {
                    (!compositor_material).then_some(Background::Color(base))
                },
                border: Border {
                    shape: BorderShape::Continuous,
                    color: color_with_opacity(palette.text_muted, 0.18 * opacity),
                    width: if history { 0.0 } else { 1.0 },
                    radius: palette.material_radius.into(),
                    ..Default::default()
                },
                snap: true,
                ..Default::default()
            },
        );
        let card: Element<'_, cosmic::Action<Message>> = if count > 1 {
            // With translucent materials, complete backing rectangles show
            // through the face and multiply its tint. Paint only the exposed
            // bottom edges, after the card's measured height.
            let back = move |inset: f32| {
                container(
                    container(cosmic::iced::widget::Space::new().width(Length::Fill).height(6))
                        .width(Length::Fill)
                        .class(theme::Container::custom(move |_| container::Style {
                            background: Some(Background::Color(Color {
                                a: 0.045 * opacity,
                                ..foreground
                            })),
                            border: Border {
                                shape: BorderShape::Continuous,
                                radius: [0.0, 0.0, motion::radius(6.0), motion::radius(6.0)].into(),
                                ..Default::default()
                            },
                            ..Default::default()
                        })),
                )
                .padding([0.0, inset])
            };
            column([card, back(6.0).into(), back(12.0).into()]).into()
        } else {
            card
        };
        let mut area = cosmic::widget::mouse_area(card)
            .on_enter(cosmic::Action::App(Message::HoverNotification(id, true)))
            .on_exit(cosmic::Action::App(Message::HoverNotification(id, false)));
        if count > 1 {
            area = area.on_press(cosmic::Action::App(if history {
                Message::ToggleNotificationGroup(notice.app.clone())
            } else {
                Message::ToggleNotificationHistory
            }));
        } else if notice.live && notice.actions.iter().any(|(key, _)| key == "default") {
            area = area.on_press(cosmic::Action::App(Message::InvokeNotification(id, "default".into())));
        }
        area.into()
    }

    pub(super) fn view_notifications(&self) -> Element<'_, cosmic::Action<Message>> {
        motion::frame_driven(
            self.notifications.motion_revision(),
            |now| self.view_notifications_at(now),
            |now| self.notifications.frame_active(now),
            |now| {
                if let Some(surface) = &self.notification_surface
                    && let Some(effects) = &surface.effects
                {
                    let _ = effects.set_presentation(
                        &surface.regions.lock().unwrap(),
                        1.0,
                        self.notifications
                            .popup_groups_at(now)
                            .iter()
                            .map(|(_, opacity, _)| *opacity),
                        ferese_surface_effects_v1::Role::Popover,
                    );
                }
            },
            |now| {
                self.notifications
                    .has_finished_closes(now)
                    .then_some(cosmic::Action::App(Message::NotificationTick))
            },
        )
    }

    pub(super) fn view_notifications_at(&self, now: std::time::Instant) -> Element<'_, cosmic::Action<Message>> {
        if self.notifications.history_open {
            let palette = self.config.theme;
            let surface = self.menu.as_ref();
            let progress = surface.map_or(1.0, |menu| menu.motion.progress_at(now));
            let opacity = 1.0;
            let color = move |rgba| color_with_opacity(rgba, opacity);
            let primary = color(palette.text_primary);
            let muted = color(palette.text_muted);
            let accent = color(palette.accent);
            let base = color(palette.surface_base);
            let compositor_material = surface.is_some_and(|menu| menu.effects.is_some());
            let well = blend(base, primary, 0.07);
            let mut bold = *SHELL_FONT.get().unwrap().read().unwrap();
            bold.weight = cosmic::iced::font::Weight::Bold;
            let groups = self.notifications.history_groups();
            let count = self.notifications.entries.len();
            let subtitle = if count == 0 {
                "A quiet moment".to_owned()
            } else {
                format!(
                    "{count} {} · {} {}",
                    if count == 1 { "notification" } else { "notifications" },
                    groups.len(),
                    if groups.len() == 1 { "app" } else { "apps" }
                )
            };
            let heading = column([])
                .spacing(3)
                .push(
                    text("Notifications")
                        .size(16)
                        .font(bold)
                        .class(theme::Text::Color(primary)),
                )
                .push(text(subtitle).size(11).class(theme::Text::Color(muted)));
            let header = row([])
                .spacing(10)
                .align_y(alignment::Vertical::Center)
                .push(container(heading).width(Length::Fill))
                .push(icon_control(
                    ferese_theme::icons::CLOSE,
                    "Close notification center",
                    Message::ToggleNotificationHistory,
                    muted,
                    well,
                    28,
                ));
            let dnd_color = if self.notifications.dnd { accent } else { muted };
            let dnd_icon = if self.notifications.dnd {
                ferese_theme::icons::NOTIFICATIONS_OFF
            } else {
                ferese_theme::icons::NOTIFICATIONS
            };
            let dnd = row([])
                .spacing(10)
                .align_y(alignment::Vertical::Center)
                .push(accented_icon(dnd_icon, 20, dnd_color, dnd_color))
                .push(
                    container(
                        column([])
                            .spacing(2)
                            .push(
                                text("Do Not Disturb")
                                    .size(12)
                                    .font(bold)
                                    .class(theme::Text::Color(primary)),
                            )
                            .push(
                                text(if self.notifications.dnd {
                                    "Popups paused"
                                } else {
                                    "Popups enabled"
                                })
                                .size(11)
                                .class(theme::Text::Color(muted)),
                            ),
                    )
                    .width(Length::Fill),
                )
                .push(motion::button(
                    button::custom(
                        container(
                            text(if self.notifications.dnd { "On" } else { "Off" })
                                .size(12)
                                .font(bold)
                                .class(theme::Text::Color(dnd_color)),
                        )
                        .center_x(40)
                        .center_y(26),
                    )
                    .on_press(cosmic::Action::App(Message::Control(status::Action::Dnd(
                        !self.notifications.dnd,
                    ))))
                    .padding(0),
                    dnd_color,
                    self.notifications.dnd,
                    1.0,
                ));
            let controls = container(dnd)
                .padding(10)
                .width(Length::Fill)
                .class(theme::Container::custom(move |_| container::Style {
                    background: Some(Background::Color(base)),
                    border: Border {
                        shape: BorderShape::Continuous,
                        radius: palette.material_radius.into(),
                        ..Default::default()
                    },
                    ..Default::default()
                }));
            let mut content = column([]).spacing(10).push(header).push(controls);
            if count == 0 {
                let empty = column([])
                    .spacing(10)
                    .align_x(alignment::Horizontal::Center)
                    .push(
                        container(accented_icon(ferese_theme::icons::NOTIFICATIONS, 26, muted, accent))
                            .center_x(56)
                            .center_y(56)
                            .class(theme::Container::custom(move |_| container::Style {
                                background: Some(Background::Color(well)),
                                border: Border {
                                    shape: BorderShape::Continuous,
                                    radius: motion::radius(28.0).into(),
                                    ..Default::default()
                                },
                                ..Default::default()
                            })),
                    )
                    .push(
                        text("You're all caught up")
                            .size(15)
                            .font(bold)
                            .class(theme::Text::Color(primary)),
                    )
                    .push(
                        text("New notifications will appear here.")
                            .size(11)
                            .class(theme::Text::Color(muted)),
                    );
                content = content.push(container(empty).center_x(Length::Fill).center_y(Length::Fill));
            } else {
                content = content.push(
                    row([])
                        .align_y(alignment::Vertical::Center)
                        .push(
                            text("Recent")
                                .size(11)
                                .font(bold)
                                .class(theme::Text::Color(muted))
                                .width(Length::Fill),
                        )
                        .push(icon_control(
                            ferese_theme::icons::TRASH,
                            "Clear all notifications",
                            Message::ClearNotifications,
                            muted,
                            well,
                            26,
                        )),
                );
                let mut entries = column([]).spacing(8);
                for group in groups {
                    let latest = group[0];
                    if group.len() > 1 && self.notifications.expanded_apps.contains(&latest.app) {
                        let app = if latest.app.is_empty() {
                            "Notifications"
                        } else {
                            &latest.app
                        };
                        let group_header = row([])
                            .spacing(6)
                            .align_y(alignment::Vertical::Center)
                            .push(
                                text(format!("{} · {}", app, group.len()))
                                    .size(11)
                                    .font(bold)
                                    .class(theme::Text::Color(muted))
                                    .width(Length::Fill),
                            )
                            .push(icon_control(
                                ferese_theme::icons::CHEVRON_UP,
                                "Collapse group",
                                Message::ToggleNotificationGroup(latest.app.clone()),
                                muted,
                                well,
                                24,
                            ))
                            .push(icon_control(
                                ferese_theme::icons::TRASH,
                                "Clear group",
                                Message::RemoveNotificationGroup(latest.app.clone()),
                                muted,
                                well,
                                24,
                            ));
                        let mut cards = column([]).spacing(8).push(group_header);
                        for notice in group {
                            cards = cards.push(self.notification_card(notice, opacity, true, 1));
                        }
                        entries = entries.push(cards);
                    } else {
                        entries = entries.push(self.notification_card(latest, opacity, true, group.len()));
                    }
                }
                content = content.push(
                    scrollable(entries)
                        .direction(Direction::Vertical(Scrollbar::hidden()))
                        .height(Length::Shrink),
                );
            }
            let panel = container(content)
                .id("ferese-blur-card")
                .padding(16)
                .height(if count == 0 {
                    Length::Fixed(280.0_f32.min(self.notification_history_height_limit()))
                } else {
                    Length::Shrink
                })
                .max_height(self.notification_history_height_limit())
                .width(Length::Fill)
                .class(theme::Container::custom(move |_| container::Style {
                    background: (!compositor_material).then_some(Background::Color(color(palette.surface_popover))),
                    border: Border {
                        shape: BorderShape::Continuous,
                        color: color_with_opacity(palette.text_muted, 0.18 * opacity),
                        width: 1.0,
                        radius: palette.material_radius.into(),
                        ..Default::default()
                    },
                    snap: true,
                    ..Default::default()
                }))
                .into();
            motion::animated(
                panel,
                progress,
                surface.map(|surface| surface.regions.clone()).unwrap_or_default(),
                palette.material_radius,
            )
        } else {
            let mut cards = column([]).spacing(8);
            for (notice, opacity, count) in self.notifications.popup_groups_at(now) {
                cards = cards.push(self.notification_card(notice, opacity, false, count));
            }
            motion::animated(
                container(
                    scrollable(cards)
                        .direction(Direction::Vertical(Scrollbar::hidden()))
                        .height(Length::Fill),
                )
                .padding(4)
                .into(),
                1.0,
                self.notification_surface
                    .as_ref()
                    .map(|surface| surface.regions.clone())
                    .unwrap_or_default(),
                self.config.theme.material_radius,
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cosmic::iced::advanced::renderer::{Headless, Renderer as _};
    use cosmic::iced::advanced::{Layout, layout, mouse, widget::Tree};
    use cosmic::iced::{Font, Pixels, Rectangle, Size};

    #[test]
    fn notification_action_hover_stays_inside_card_corners() {
        check_action_hover("tiny-skia");
    }

    #[test]
    #[ignore = "requires a GPU or software Vulkan adapter"]
    fn gpu_notification_action_hover_stays_inside_card_corners() {
        check_action_hover("wgpu");
    }

    fn check_action_hover(backend: &str) {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let mut renderer = runtime
            .block_on(<cosmic::Renderer as Headless>::new(
                Font::default(),
                Pixels(14.0),
                Some(backend),
            ))
            .unwrap();
        let theme = cosmic::Theme::dark();
        let bounds = Rectangle::with_size(Size::new(368.0, 114.0));
        for history in [false, true] {
            for scale in [1.0, 1.25, 1.5] {
                let action = card_button(
                    cosmic::widget::Space::new().height(38),
                    Message::InvokeNotification(1, "action".into()),
                    Color::WHITE,
                    Color::WHITE,
                    false,
                    "Action".into(),
                );
                let face = column([cosmic::widget::Space::new().height(76).into(), action]);
                let mut card = card_surface(
                    face,
                    history,
                    container::Style {
                        border: Border {
                            shape: BorderShape::Continuous,
                            radius: 14.0.into(),
                            width: if history { 0.0 } else { 1.0 },
                            ..Default::default()
                        },
                        snap: true,
                        ..Default::default()
                    },
                );
                let mut tree = Tree::new(card.as_widget());
                let node =
                    card.as_widget_mut()
                        .layout(&mut tree, &renderer, &layout::Limits::new(Size::ZERO, bounds.size()));
                renderer.reset(bounds);
                card.as_widget().draw(
                    &tree,
                    &mut renderer,
                    &theme,
                    &Default::default(),
                    Layout::new(&node),
                    mouse::Cursor::Available((80.0, 100.0).into()),
                    &bounds,
                );
                let size = Size::new((bounds.width * scale) as u32, (bounds.height * scale) as u32);
                let pixels = Headless::screenshot(&mut renderer, size, scale, Color::TRANSPARENT);
                let alpha = |x: u32, y: u32| pixels[((y * size.width + x) * 4 + 3) as usize];
                assert_eq!(
                    alpha(0, size.height - 1),
                    0,
                    "{backend}, scale={scale}, history={history}"
                );
                assert!(
                    alpha(size.width / 2, size.height - 4) > 200,
                    "the action hover must still render"
                );
            }
        }
    }
}
