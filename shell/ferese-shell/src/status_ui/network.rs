use cosmic::iced::{Alignment, Length};
use cosmic::theme;
use cosmic::widget::{button, column, row, text_input};

use super::connections::{self, Event};
use super::controls::shell_switch;
use super::{MenuRows, MenuStyle};
use crate::status::wireless::{Password, Security, WifiNetwork};
use crate::status::{Action, Discovery};
use crate::{FereseShell, Message};

pub(super) fn view<'a>(
    shell: &'a FereseShell,
    mut rows: MenuRows<'a>,
    style: MenuStyle,
) -> MenuRows<'a> {
    let Some(network) = &shell.status.network else {
        return rows;
    };
    if !network.enabled {
        return rows.push(
            shell
                .render
                .text("Turn on Wi-Fi to see available networks.")
                .size(12)
                .class(theme::Text::Color(style.muted)),
        );
    }
    let available = network.available.as_deref().unwrap_or_default();
    let mut list = column::with_capacity(available.len());
    let busy = shell.connections.pending.as_ref();
    for network in available {
        let detail = if busy
            .is_some_and(|pending| pending.radio == Discovery::Wifi && pending.path == network.path)
        {
            if busy.unwrap().disconnecting {
                "Disconnecting…"
            } else {
                "Connecting…"
            }
        } else if network.connected {
            "Connected"
        } else if network.saved.is_some() {
            "Saved"
        } else {
            match network.security {
                Security::Open => "Open network",
                Security::Personal | Security::Transition | Security::Sae => "Password required",
                Security::Enterprise => "Enterprise network",
                Security::Owe => "Enhanced open network",
                Security::Unsupported => "Requires setup",
            }
        };
        list = list.push(network_row(shell, network, detail, style));
    }
    if available.is_empty() {
        if let Some(name) = &network.connection {
            list = list.push(
                row![
                    crate::bar_icon(ferese_theme::icons::WIFI_FULL, 14, style.primary),
                    shell.render.text(name).size(13).width(Length::Fill),
                    crate::bar_icon(ferese_theme::icons::CHECK, 14, style.primary),
                ]
                .spacing(8)
                .padding([6, 6])
                .align_y(Alignment::Center),
            );
        } else {
            list = list.push(
                shell
                    .render
                    .text(if network.available.is_some() {
                        "No networks found."
                    } else {
                        "Looking for networks…"
                    })
                    .size(12)
                    .class(theme::Text::Color(style.muted)),
            );
        }
    }
    rows = rows.push(connections::connection_list(
        list.into(),
        "wifi-networks",
        140.,
    ));
    if let Some(error) = &network.scan_error {
        rows = rows.push(
            shell
                .render
                .text(error)
                .size(12)
                .class(theme::Text::Color(style.muted)),
        );
    }
    if let Some(network) = &shell.connections.wifi {
        let mut input = text_input("Password", shell.connections.password.as_str())
            .id(cosmic::widget::Id::new("wifi-password"))
            .password()
            .size(13)
            .on_input(|value| {
                cosmic::Action::App(Message::Connections(Event::Password(Password::new(value))))
            });
        if !shell.connections.password.as_str().is_empty() {
            input = input.on_submit(|value| {
                drop(Password::new(value));
                cosmic::Action::App(Message::Connections(Event::Submit))
            });
        }
        let mut connect = button::custom(shell.render.text("Connect").size(12))
            .padding([6, 10])
            .name(format!("Connect to {}", network.name));
        if !shell.connections.password.as_str().is_empty() {
            connect = connect.on_press(cosmic::Action::App(Message::Connections(Event::Submit)));
        }
        rows = rows.push(
            column![
                shell
                    .render
                    .text(format!("Password for {}", network.name))
                    .size(12),
                input,
                row![
                    button::custom(shell.render.text("Cancel").size(12))
                        .padding([6, 10])
                        .on_press(cosmic::Action::App(Message::Connections(Event::Cancel))),
                    connect,
                ]
                .spacing(6)
                .align_y(Alignment::Center)
            ]
            .spacing(6),
        );
    }
    connections::footer(shell, rows, style, Discovery::Wifi)
}

fn network_row<'a>(
    shell: &FereseShell,
    network: &'a WifiNetwork,
    detail: &str,
    style: MenuStyle,
) -> cosmic::Element<'a, cosmic::Action<Message>> {
    let icon = match network.strength {
        75.. => ferese_theme::icons::WIFI_FULL,
        40..=74 => ferese_theme::icons::WIFI_MEDIUM,
        _ => ferese_theme::icons::WIFI_LOW,
    };
    let tint = if network.connected {
        crate::color_with_opacity(style.theme.accent, style.opacity)
    } else {
        style.primary
    };
    let title = shell
        .render
        .text(&network.name)
        .size(13)
        .line_height(cosmic::iced::widget::text::LineHeight::Absolute(16.into()))
        .width(Length::Fill)
        .wrapping(cosmic::iced::widget::text::Wrapping::None)
        .ellipsize(cosmic::iced::widget::text::Ellipsize::End(
            cosmic::iced::advanced::text::EllipsizeHeightLimit::Lines(1),
        ))
        .class(theme::Text::Color(style.primary));
    let mut content = row![crate::bar_icon(icon, 14, tint), title]
        .spacing(8)
        .align_y(Alignment::Center);
    if shell
        .connections
        .pending
        .as_ref()
        .is_some_and(|pending| pending.radio == Discovery::Wifi && pending.path == network.path)
    {
        content = content.push(
            shell
                .render
                .text(detail.to_owned())
                .size(11)
                .class(theme::Text::Color(style.muted)),
        );
    } else if network.connected {
        content = content.push(crate::bar_icon(ferese_theme::icons::CHECK, 14, tint));
    } else if !matches!(network.security, Security::Open | Security::Owe) {
        content = content.push(crate::bar_icon(ferese_theme::icons::LOCK, 12, style.muted));
    }
    let enabled = shell.connections.pending.is_none();
    let mut button = button::custom(content)
        .id(cosmic::widget::Id::new(format!(
            "wifi:{}:{}:{:02x?}:{:?}:{}:{enabled}",
            network.path, network.device, network.ssid, network.security, network.connected
        )))
        .width(Length::Fill)
        .padding([6, 6])
        .name(format!("{}: {detail}", network.name));
    if enabled {
        button = button.on_press(cosmic::Action::App(Message::Connections(Event::Wifi(
            network.clone(),
        ))));
    }
    // Keep the stable button ID inside the paint wrapper so reordered rows reset presses.
    crate::motion::button(shell.render, button, style.primary, false, style.opacity)
}

pub(super) fn heading<'a>(
    shell: &'a FereseShell,
    mut heading: super::MenuHeading<'a>,
    style: MenuStyle,
) -> super::MenuHeading<'a> {
    if let Some(network) = &shell.status.network {
        heading = heading.push(shell_switch(
            shell.render,
            "Wi-Fi",
            network.enabled,
            Action::Wifi(!network.enabled),
            style.theme,
            style.opacity,
        ));
    }
    heading
}
