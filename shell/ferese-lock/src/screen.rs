use std::borrow::Cow;

use cosmic::font::Font;
use cosmic::iced::{Alignment, Background, Border, Color, ContentFit, Length, Padding, Size};
use cosmic::widget::container::Style;
use cosmic::widget::icon::Icon;
use cosmic::widget::{Text, column, container, image, row};
use cosmic::{Element, Renderer, Theme, iced, theme, widget};

use crate::{AuthState, Locker, Message, password_field, wallpaper};

impl Locker {
    fn label<'a>(
        &self,
        label: impl Into<Cow<'a, str>> + 'a,
        size: u16,
    ) -> Text<'a, Theme, Renderer> {
        ferese_theme::text(label, self.appearance.font).size(size)
    }

    fn status_label<'a>(
        &self,
        text: impl Into<Cow<'a, str>> + 'a,
        id: &'static str,
    ) -> Text<'a, Theme, Renderer> {
        use iced::advanced::Widget;
        let mut label = self.label(text, 12);
        <_ as Widget<Message, Theme, Renderer>>::set_id(&mut label, widget::Id::new(id));
        label
    }

    pub(super) fn screen(&self) -> Element<'_, Message> {
        iced::widget::responsive(move |size| self.screen_content(size)).into()
    }

    pub(super) fn screen_content(&self, size: Size) -> Element<'_, Message> {
        let compact = size.height < 560.;
        let avatar_size = (size.height * 0.08).clamp(56., 88.);
        let clock_size = (size.height * 0.06)
            .min(
                size.width
                    * if self.appearance.twelve_hour {
                        0.055
                    } else {
                        0.085
                    },
            )
            .clamp(32., 72.) as u16;
        let margin = if size.width < 640. { 24. } else { 36. };
        let top = (size.height * 0.09).max(24.);
        let a = &self.appearance;
        let mask = self.password.mask();
        let input_content = container(
            self.label(
                if mask.is_empty() {
                    "Password".into()
                } else {
                    mask
                },
                14,
            )
            .wrapping(iced::widget::text::Wrapping::None),
        )
        .center_y(24)
        .width(Length::Fill)
        .class(theme::Container::custom(|_| Style {
            text_color: Some(Color::WHITE),
            ..Default::default()
        }));
        let input = Element::new(password_field::PasswordField::new(
            input_content.into(),
            self.state.may_edit(),
        ));
        let submit = widget::button::custom(widget::icon::from_name("go-next-symbolic").size(16))
            .class(theme::Button::Icon)
            .name("Unlock")
            .padding(4)
            .on_press_maybe(self.can_submit().then_some(Message::Submit));
        let input = container(row![input, submit].spacing(4).align_y(Alignment::Center))
            .padding([4, 10])
            .class(theme::Container::custom(move |_| Style {
                background: Some(
                    if a.high_contrast || a.reduce_transparency {
                        Color::from_rgb(0.12, 0.13, 0.15)
                    } else {
                        Color::from_rgba(
                            (a.panel.r + 1.) * 0.5,
                            (a.panel.g + 1.) * 0.5,
                            (a.panel.b + 1.) * 0.5,
                            0.24,
                        )
                    }
                    .into(),
                ),
                text_color: Some(Color::WHITE),
                icon_color: Some(Color::WHITE),
                border: Border {
                    radius: (a.radius * 2.).min(26.).into(),
                    ..Default::default()
                },
                ..Default::default()
            }));
        let avatar: Element<'_, Message> = if let Some(photo) = &a.avatar {
            image(photo.clone())
                .width(avatar_size)
                .height(avatar_size)
                .content_fit(ContentFit::Cover)
                .border_radius(avatar_size / 2.)
                .into()
        } else {
            container(line_icon(
                "M16 8a4 4 0 1 1-8 0 4 4 0 0 1 8 0 M4 21v-2a8 8 0 0 1 16 0v2",
                46,
            ))
            .center_x(avatar_size)
            .center_y(avatar_size)
            .class(theme::Container::custom(move |_| {
                let fill = ferese_theme::composite(a.accent.scale_alpha(0.18), a.panel);
                let on = ferese_theme::foreground(fill, a.accent);
                Style {
                    background: Some(Background::Color(fill)),
                    text_color: Some(on),
                    icon_color: Some(on),
                    border: Border {
                        shape: cosmic::iced::border::Shape::Continuous,
                        radius: (avatar_size / 2.).into(),
                        ..Default::default()
                    },
                    ..Default::default()
                }
            }))
            .into()
        };
        let mut identity = column([])
            .spacing(if compact { 8 } else { 12 })
            .align_x(Alignment::Center);
        if !compact {
            identity = identity.push(avatar);
        }
        let status = if matches!(self.state, AuthState::Rejected(_)) {
            format!("{} You can edit now and submit again shortly.", self.status)
        } else {
            self.status.to_owned()
        };
        let identity = identity
            .push(self.label(&self.user, 16).class(Color::WHITE))
            .push(widget::Space::new().height(6))
            .push(input)
            .push(
                container(
                    self.status_label(status, "authentication-status")
                        .width(Length::Fill)
                        .align_x(Alignment::Center)
                        .class(Color::WHITE),
                )
                .height(60)
                .width(Length::Fill),
            )
            .push(
                container(
                    self.status_label(
                        self.keyboard_layout
                            .as_deref()
                            .unwrap_or("Keyboard layout unavailable"),
                        "keyboard-layout",
                    )
                    .class(Color::WHITE),
                )
                .height(18),
            )
            .push(
                container(
                    self.status_label(
                        if self.caps_lock {
                            "Caps Lock is on"
                        } else {
                            ""
                        },
                        "keyboard-warning",
                    )
                    .class(Color::WHITE),
                )
                .height(18),
            )
            .width(
                (size.height * 0.23)
                    .clamp(200., 240.)
                    .min((size.width - margin * 2.).max(160.)),
            );
        let mut clock = column([]).spacing(0).align_x(Alignment::Center);

        if a.show_clock && !compact {
            clock = clock.push(
                self.label(&self.clock, clock_size)
                    .font(Font {
                        weight: cosmic::iced::font::Weight::Light,
                        ..a.font
                    })
                    .class(Color::WHITE),
            );
        }

        if a.show_date && !compact {
            clock = clock.push(
                self.label(&self.date, 16)
                    .class(Color::WHITE.scale_alpha(0.72)),
            );
        }

        let clock_height = if a.show_clock && !compact {
            clock_size as f32 * 1.3
        } else {
            0.
        } + if a.show_date && !compact { 24. } else { 0. };
        let gap = (size.height * 0.44 - top - clock_height).max(24.);
        let content: Element<'_, Message> = if compact {
            widget::scrollable(container(identity).center_x(Length::Fill).padding([16, 0])).into()
        } else {
            column![
                container(clock).center_x(Length::Fill),
                widget::Space::new().height(gap),
                container(identity).center_x(Length::Fill),
                widget::Space::new().height(Length::Fill),
                row![
                    row![ferese_symbol(), self.label("Ferese", 13)]
                        .spacing(5)
                        .align_y(Alignment::Center),
                    widget::Space::new().width(Length::Fill),
                    self.label(
                        if self.preview {
                            "Preview · your session is not locked"
                        } else {
                            ""
                        },
                        11
                    )
                ]
                .align_y(Alignment::Center),
            ]
            .height(Length::Fill)
            .into()
        };
        let foreground = container(content)
            .padding(Padding {
                top: if compact { 0. } else { top },
                right: margin,
                bottom: margin,
                left: margin,
            })
            .width(Length::Fill)
            .height(Length::Fill)
            .class(theme::Container::custom(move |_| Style {
                text_color: Some(Color::WHITE.scale_alpha(0.65)),
                background: Some(Background::Color(Color::from_rgba(
                    0.025,
                    0.035,
                    0.06,
                    if a.high_contrast { 1. } else { a.dim },
                ))),
                ..Default::default()
            }));
        widget::mouse_area(Element::new(wallpaper::Wallpaper {
            image: a.wallpaper.clone(),
            content: foreground.into(),
        }))
        .on_press(Message::FocusPassword)
        .into()
    }
}

fn line_icon(path: &str, size: u16) -> Icon {
    let svg = format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24"><path d="{path}" fill="none" stroke="white" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round"/></svg>"#
    );
    widget::icon(widget::icon::from_svg_bytes(svg.into_bytes()).symbolic(true)).size(size)
}

fn ferese_symbol() -> Icon {
    widget::icon(widget::icon::from_svg_bytes(ferese_theme::icons::FERESE).symbolic(true)).size(30)
}
