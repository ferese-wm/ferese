use cosmic::iced::border::Shape as BorderShape;
use cosmic::iced::{Alignment, Background, Border, Color, Length};
use cosmic::widget::{button, column, container, row, slider};
use cosmic::{Element, theme};

use crate::status::Action;
use crate::{Message, ShellTheme, accented_icon, color, color_with_opacity, motion, shell_font, text};

pub(super) fn status_summary<'a>(
    source: &'static [u8],
    title: &'a str,
    subtitle: &'a str,
    style: super::MenuStyle,
    enabled: bool,
    toggle: Option<(bool, Action)>,
) -> Element<'a, cosmic::Action<Message>> {
    let super::MenuStyle {
        theme,
        primary,
        muted,
        opacity,
    } = style;
    let accent = color_with_opacity(theme.accent, opacity);
    let badge_color = if enabled { accent } else { primary };
    let badge = container(accented_icon(source, 22, primary, accent))
        .width(32)
        .height(32)
        .center_x(32)
        .center_y(32)
        .class(theme::Container::custom(move |_| container::Style {
            background: Some(Background::Color(Color {
                a: if enabled { 0.22 * opacity } else { 0.06 * opacity },
                ..badge_color
            })),
            border: Border {
                shape: BorderShape::Continuous,
                radius: motion::radius(16.0).into(),
                ..Default::default()
            },
            ..Default::default()
        }));
    let badge: Element<'_, cosmic::Action<Message>> = if let Some((_, action)) = toggle {
        motion::button(
            button::custom(badge)
                .padding(0)
                .name(if enabled { "Turn off" } else { "Turn on" })
                .on_press(cosmic::Action::App(Message::Control(action))),
            badge_color,
            false,
            opacity,
        )
    } else {
        badge.into()
    };
    let summary = row![
        badge,
        column![
            text(title).size(14),
            text(subtitle).size(12).class(theme::Text::Color(muted)),
        ]
        .spacing(4)
        .width(Length::Fill)
    ]
    .spacing(8)
    .align_y(Alignment::Center);
    summary.into()
}

pub(super) fn level_meter(value: u8, foreground: Color, opacity: f32) -> Element<'static, cosmic::Action<Message>> {
    let segment = move |amount: u16, filled: bool| {
        container(text(""))
            .width(Length::FillPortion(amount))
            .height(5)
            .class(theme::Container::custom(move |_| container::Style {
                background: Some(Background::Color(if filled {
                    foreground
                } else {
                    Color {
                        a: 0.1 * opacity,
                        ..foreground
                    }
                })),
                border: Border {
                    shape: BorderShape::Continuous,
                    radius: motion::radius(2.5).into(),
                    ..Default::default()
                },
                ..Default::default()
            }))
    };
    let value = u16::from(value.min(100));
    let mut meter = row::with_capacity(2).width(Length::Fill);
    if value > 0 {
        meter = meter.push(segment(value, true));
    }
    if value < 100 {
        meter = meter.push(segment(100 - value, false));
    }
    meter.into()
}

pub(super) fn control_card<'a>(
    content: Element<'a, cosmic::Action<Message>>,
    foreground: Color,
    opacity: f32,
) -> Element<'a, cosmic::Action<Message>> {
    ferese_theme::menus::section(content, foreground, opacity, motion::radius(22.))
}

pub(super) fn menu_button<'a>(
    label: &'a str,
    message: Message,
    foreground: Color,
    opacity: f32,
) -> Element<'a, cosmic::Action<Message>> {
    motion::button(
        ferese_theme::menus::button(label, cosmic::Action::App(message), shell_font()),
        foreground,
        false,
        opacity,
    )
}

pub(super) fn shell_switch<'a>(
    label: &str,
    enabled: bool,
    action: Action,
    palette: ShellTheme,
    opacity: f32,
) -> Element<'a, cosmic::Action<Message>> {
    let toggle = ferese_theme::menus::switch(enabled, palette.palette(), opacity);
    motion::button(
        button::custom(toggle)
            .padding(4)
            .name(format!("{label}: {}", if enabled { "on" } else { "off" }))
            .on_press(cosmic::Action::App(Message::Control(action))),
        color(palette.text_primary),
        false,
        opacity,
    )
}

pub(super) fn toggle_row<'a>(
    label: &'a str,
    on: bool,
    action: Action,
    palette: ShellTheme,
    opacity: f32,
) -> Element<'a, cosmic::Action<Message>> {
    ferese_theme::menus::row(label, shell_switch(label, on, action, palette, opacity), shell_font())
}

pub(super) fn slider_row(
    source: &'static [u8],
    value: u8,
    brightness: bool,
    foreground: Color,
    icon_accent: Color,
    opacity: f32,
) -> Element<'static, cosmic::Action<Message>> {
    let control = slider(if brightness { 1..=100 } else { 0..=100 }, value, move |value| {
        cosmic::Action::App(Message::Control(if brightness {
            Action::Brightness(value)
        } else {
            Action::Volume(value)
        }))
    })
    .width(Length::Fill)
    .height(24)
    .class(ferese_theme::menus::slider(foreground, opacity));
    row![
        accented_icon(source, 18, foreground, icon_accent),
        control,
        text(format!("{value}%")).size(12).width(36)
    ]
    .spacing(8)
    .align_y(Alignment::Center)
    .into()
}
