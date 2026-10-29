use cosmic::iced::{Alignment, Length};
use cosmic::widget::{Space, button, column, container, row};
use cosmic::{Element, theme};
use ferese_config::theme::{Appearance, Mode};

use super::bar::status_icon;
use super::controls::control_card;
use super::{Menu, MenuRows, MenuStyle, audio, battery};
use crate::status::Action;
use crate::{FereseShell, Message, bar_icon, color, motion};

pub(super) fn view<'a>(
    shell: &'a FereseShell,
    mut rows: MenuRows<'a>,
    style: MenuStyle,
    show_settings_link: bool,
) -> MenuRows<'a> {
    if show_settings_link {
        rows = rows.push(heading(
            shell,
            row![
                shell
                    .render
                    .text("Control Center")
                    .size(14)
                    .width(Length::Fill)
            ]
            .height(28)
            .align_y(Alignment::Center),
            style,
        ));
    }
    let mut tiles = Vec::with_capacity(6);
    if let Some(network) = &shell.status.network {
        tiles.push(quick_tile(
            shell,
            "Wi-Fi",
            if !network.enabled {
                "Off"
            } else if network.connection.is_some() {
                "Connected"
            } else {
                "Not connected"
            },
            status_icon(Menu::Network, &shell.status).0,
            network.enabled,
            Some(Message::Control(Action::Wifi(!network.enabled))),
            style,
        ));
    }
    if let Some(bluetooth) = &shell.status.bluetooth {
        tiles.push(quick_tile(
            shell,
            "Bluetooth",
            if !bluetooth.enabled {
                "Off"
            } else {
                bluetooth
                    .devices
                    .first()
                    .map(String::as_str)
                    .unwrap_or("On")
            },
            status_icon(Menu::Bluetooth, &shell.status).0,
            bluetooth.enabled,
            Some(Message::Control(Action::Bluetooth(!bluetooth.enabled))),
            style,
        ));
    }
    let night_light = shell.status.night_light.as_ref();
    let supported = night_light.is_some_and(|status| status.supported_outputs > 0);
    let enabled = night_light.is_some_and(|status| status.enabled);
    tiles.push(quick_tile(
        shell,
        "Night Light",
        if !supported {
            "Unavailable"
        } else if enabled {
            "On"
        } else {
            "Off"
        },
        ferese_theme::icons::NIGHT_LIGHT,
        supported && enabled,
        supported.then_some(Message::Control(Action::NightLight(!enabled))),
        style,
    ));
    if let Some(notifications) = &shell.status.notifications {
        tiles.push(quick_tile(
            shell,
            "Do Not Disturb",
            if notifications.dnd { "On" } else { "Off" },
            ferese_theme::icons::DND,
            notifications.dnd,
            Some(Message::Control(Action::Dnd(!notifications.dnd))),
            style,
        ));
    }
    let dark = style.theme.appearance == Appearance::Dark;
    tiles.push(quick_tile(
        shell,
        "Dark Mode",
        if dark { "On" } else { "Off" },
        ferese_theme::icons::APPEARANCE,
        dark,
        Some(Message::ThemeMode(if dark {
            Mode::Light
        } else {
            Mode::Dark
        })),
        style,
    ));
    if let Some(battery) = &shell.status.battery {
        tiles.push(quick_tile(
            shell,
            "Battery",
            format!(
                "{}%{}",
                battery.percent,
                if matches!(battery.status.as_str(), "Charging" | "Pending charge") {
                    " · Charging"
                } else {
                    ""
                }
            ),
            status_icon(Menu::Battery, &shell.status).0,
            matches!(battery.status.as_str(), "Charging" | "Pending charge"),
            None,
            style,
        ));
    }
    let mut grid = column::with_capacity(3).spacing(4).width(Length::Fill);
    let mut tiles = tiles.into_iter();
    while let Some(first) = tiles.next() {
        let second = tiles
            .next()
            .unwrap_or_else(|| Space::new().width(Length::FillPortion(1)).into());
        grid = grid.push(row![first, second].spacing(8).width(Length::Fill));
    }
    rows = rows.push(grid);
    rows = battery::brightness(shell, rows, style);
    audio::view(shell, rows, style, true)
}

fn quick_tile<'a>(
    shell: &'a FereseShell,
    label: &'static str,
    detail: impl Into<std::borrow::Cow<'a, str>>,
    source: &'static [u8],
    enabled: bool,
    action: Option<Message>,
    style: MenuStyle,
) -> Element<'a, cosmic::Action<Message>> {
    let render = shell.render;
    let foreground = if enabled { style.primary } else { style.muted };
    let icon_fill = ferese_theme::mix(
        color(style.theme.surface_base),
        color(style.theme.accent),
        if enabled { 0.12 } else { 0.0 },
    )
    .scale_alpha(style.opacity);
    let icon_color = if enabled {
        ferese_theme::foreground(icon_fill, color(style.theme.accent)).scale_alpha(style.opacity)
    } else {
        foreground
    };
    let icon = container(bar_icon(source, 18, icon_color))
        .center_x(28)
        .center_y(28)
        .class(ferese_theme::controls::surface(
            icon_fill,
            render.radius(8.),
        ));
    let detail = detail.into();
    let name = format!("{label}: {detail}");
    let text = |label: std::borrow::Cow<'a, str>, size: f32, foreground| {
        render
            .text(label)
            .size(size)
            .line_height(cosmic::iced::widget::text::LineHeight::Absolute(
                (size + 1.).into(),
            ))
            .width(Length::Fill)
            .wrapping(cosmic::iced::widget::text::Wrapping::None)
            .ellipsize(cosmic::iced::widget::text::Ellipsize::End(
                cosmic::iced::advanced::text::EllipsizeHeightLimit::Lines(1),
            ))
            .class(theme::Text::Color(foreground))
    };
    let content = row![
        icon,
        column![
            text(label.into(), 12., style.primary),
            text(detail, 11., style.muted)
        ]
        .spacing(0)
        .width(Length::Fill),
    ]
    .spacing(8)
    .align_y(Alignment::Center);
    let mut button = button::custom(control_card(
        render,
        content.into(),
        style.primary,
        style.opacity,
    ))
    .width(Length::FillPortion(1))
    .padding(0)
    .name(name);
    if let Some(action) = action {
        button = button.on_press(cosmic::Action::App(action));
    }
    motion::button(render, button, foreground, false, style.opacity)
}

fn settings_button<'a>(
    shell: &'a FereseShell,
    style: MenuStyle,
) -> Option<Element<'a, cosmic::Action<Message>>> {
    shell
        .config
        .status
        .settings_command
        .as_ref()
        .filter(|command| !command.is_empty())?;
    Some(motion::button(
        shell.render,
        button::custom(bar_icon(ferese_theme::icons::SETTINGS, 18, style.primary))
            .width(28)
            .height(28)
            .padding(5)
            .name("Open Settings")
            .on_press(cosmic::Action::App(Message::Control(Action::Settings))),
        style.primary,
        false,
        style.opacity,
    ))
}

pub(super) fn heading<'a>(
    shell: &'a FereseShell,
    heading: super::MenuHeading<'a>,
    style: MenuStyle,
) -> super::MenuHeading<'a> {
    if let Some(settings) = settings_button(shell, style) {
        heading.push(settings)
    } else {
        heading
    }
}
