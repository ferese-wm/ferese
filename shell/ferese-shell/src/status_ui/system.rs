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
    let tile_icon = |source, enabled| {
        bar_icon(
            source,
            18,
            if enabled { style.primary } else { style.muted },
        )
        .into()
    };
    if let Some(network) = &shell.status.network {
        tiles.push(connection_tile(
            shell,
            "Wi-Fi",
            if !network.enabled {
                "Off"
            } else if network.connection.is_some() {
                "Connected"
            } else {
                "Not connected"
            },
            tile_icon(status_icon(Menu::Network, &shell.status).0, network.enabled),
            Action::Wifi(!network.enabled),
            Menu::Network,
            style,
        ));
    }
    if let Some(bluetooth) = &shell.status.bluetooth {
        tiles.push(connection_tile(
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
            tile_icon(
                status_icon(Menu::Bluetooth, &shell.status).0,
                bluetooth.enabled,
            ),
            Action::Bluetooth(!bluetooth.enabled),
            Menu::Bluetooth,
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
        tile_icon(ferese_theme::icons::NIGHT_LIGHT, supported && enabled),
        supported && enabled,
        supported.then_some(Message::Control(Action::NightLight(!enabled))),
        style,
    ));
    if let Some(notifications) = &shell.status.notifications {
        tiles.push(quick_tile(
            shell,
            "Do Not Disturb",
            if notifications.dnd { "On" } else { "Off" },
            tile_icon(ferese_theme::icons::DND, notifications.dnd),
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
        tile_icon(ferese_theme::icons::APPEARANCE, dark),
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
            shell.battery_indicator(style.theme, style.opacity, false, 12.),
            true,
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

fn icon_well<'a>(
    shell: &FereseShell,
    icon: Element<'a, cosmic::Action<Message>>,
    style: MenuStyle,
) -> Element<'a, cosmic::Action<Message>> {
    container(icon)
        .center_x(28)
        .center_y(28)
        .class(ferese_theme::controls::surface(
            color(style.theme.surface_base).scale_alpha(0.25 * style.opacity),
            shell.render.radius(8.),
        ))
        .into()
}

fn tile_text<'a>(
    shell: &FereseShell,
    label: &'static str,
    detail: std::borrow::Cow<'a, str>,
    style: MenuStyle,
) -> Element<'a, cosmic::Action<Message>> {
    let text = |label: std::borrow::Cow<'a, str>, size: f32, foreground| {
        shell
            .render
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
    column![
        text(label.into(), 12., style.primary),
        text(detail, 11., style.muted)
    ]
    .spacing(0)
    .width(Length::Fill)
    .into()
}

fn connection_tile<'a>(
    shell: &'a FereseShell,
    label: &'static str,
    detail: impl Into<std::borrow::Cow<'a, str>>,
    icon: Element<'a, cosmic::Action<Message>>,
    action: Action,
    kind: Menu,
    style: MenuStyle,
) -> Element<'a, cosmic::Action<Message>> {
    let detail = detail.into();
    let name = format!("{label}: {detail}");
    let appearance = || {
        ferese_theme::controls::notification_button(
            style.primary,
            style.primary.scale_alpha(0.04 * style.opacity),
            shell.render.radius(10.),
            false,
        )
    };
    let toggle = button::custom(icon_well(shell, icon, style))
        .id(cosmic::widget::Id::new(format!("radio:{label}:{action:?}")))
        .padding([10, 6, 10, 10])
        .name(format!(
            "Turn {} {label}",
            if matches!(action, Action::Wifi(true) | Action::Bluetooth(true)) {
                "on"
            } else {
                "off"
            }
        ))
        .class(appearance())
        .on_press(cosmic::Action::App(Message::Control(action)));
    let details = button::custom(
        row![
            tile_text(shell, label, detail, style),
            bar_icon(ferese_theme::icons::CHEVRON_DOWN, 8, style.muted)
        ]
        .spacing(4)
        .align_y(Alignment::Center),
    )
    .id(cosmic::widget::Id::new(format!("details:{label}")))
    .padding([10, 10, 10, 2])
    .height(48)
    .width(Length::Fill)
    .name(name)
    .class(appearance())
    .on_press(cosmic::Action::App(Message::Connections(
        super::connections::Event::ShowPanel(kind),
    )));
    control_card(
        shell.render,
        row![toggle, details].align_y(Alignment::Center).into(),
        style.primary,
        style.opacity,
    )
    .padding(0)
    .width(Length::FillPortion(1))
    .into()
}

fn quick_tile<'a>(
    shell: &'a FereseShell,
    label: &'static str,
    detail: impl Into<std::borrow::Cow<'a, str>>,
    icon: Element<'a, cosmic::Action<Message>>,
    enabled: bool,
    action: Option<Message>,
    style: MenuStyle,
) -> Element<'a, cosmic::Action<Message>> {
    let render = shell.render;
    let foreground = if enabled { style.primary } else { style.muted };
    let icon = icon_well(shell, icon, style);
    let detail = detail.into();
    let name = format!("{label}: {detail}");
    let content = row![icon, tile_text(shell, label, detail, style),]
        .spacing(8)
        .align_y(Alignment::Center);
    let mut button = button::custom(control_card(
        render,
        content.into(),
        style.primary,
        style.opacity,
    ))
    .id(cosmic::widget::Id::new(format!("quick:{label}:{enabled}")))
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
