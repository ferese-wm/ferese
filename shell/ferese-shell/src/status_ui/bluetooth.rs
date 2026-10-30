use cosmic::{theme, widget::column};

use super::connections::{self, Event};
use super::controls::shell_switch;
use super::{MenuRows, MenuStyle};
use crate::status::{Action, Discovery};
use crate::{FereseShell, Message};

pub(super) fn view<'a>(
    shell: &'a FereseShell,
    mut rows: MenuRows<'a>,
    style: MenuStyle,
) -> MenuRows<'a> {
    let Some(bluetooth) = &shell.status.bluetooth else {
        return rows;
    };
    if !bluetooth.enabled {
        return rows.push(
            shell
                .render
                .text("Turn on Bluetooth to see nearby devices.")
                .size(12)
                .class(theme::Text::Color(style.muted)),
        );
    }
    let available = bluetooth.available.as_deref().unwrap_or_default();
    let mut devices = column::with_capacity(available.len()).spacing(2);
    let busy = shell.connections.pending.as_ref();
    let mut section = None;
    for device in available {
        let known = device.paired || device.connected;
        if section != Some(known) {
            section = Some(known);
            devices = devices.push(
                shell
                    .render
                    .text(if known {
                        "My devices"
                    } else {
                        "Nearby devices"
                    })
                    .size(11)
                    .class(theme::Text::Color(style.muted)),
            );
        }
        let detail = if busy.is_some_and(|pending| {
            pending.radio == Discovery::Bluetooth && pending.path == device.path
        }) {
            if busy.unwrap().cancelled {
                "Canceling…"
            } else if busy.unwrap().disconnecting {
                "Disconnecting…"
            } else if !device.paired {
                "Pairing…"
            } else {
                "Connecting…"
            }
        } else if device.connected {
            "Connected"
        } else if device.paired {
            "Paired"
        } else {
            "Pair and connect"
        };
        devices = devices.push(connections::connection_row(
            shell,
            ferese_theme::icons::BLUETOOTH_ON,
            &device.name,
            detail,
            Some(Message::Connections(Event::Bluetooth(device.clone()))),
            device.connected,
            style,
        ));
    }
    if available.is_empty() {
        for name in &bluetooth.devices {
            devices = devices.push(connections::connection_row(
                shell,
                ferese_theme::icons::BLUETOOTH_CONNECTED,
                name,
                "Connected",
                None,
                true,
                style,
            ));
        }
        devices = devices.push(
            shell
                .render
                .text(if bluetooth.available.is_some() {
                    "No nearby devices found."
                } else {
                    "Looking for nearby devices…"
                })
                .size(12)
                .class(theme::Text::Color(style.muted)),
        );
    }
    rows = rows.push(connections::connection_list(
        devices.into(),
        "bluetooth-devices",
        230.,
    ));
    if let Some(request) = &shell.connections.pairing
        && let Some(pending) = &shell.connections.pending
    {
        use crate::status::wireless::{Password, pairing::Prompt};
        use cosmic::widget::{button, column, row, text_input};
        let serial = pending.serial;
        let id = request.id;
        let mut prompt = column::with_capacity(4).spacing(6);
        let text = match &request.prompt {
            Prompt::Confirm(Some(_)) => "Does this code match your device?",
            Prompt::Confirm(None) => "Allow this device to pair?",
            Prompt::Pin => "Enter the PIN shown on your device.",
            Prompt::Passkey => "Enter the six-digit code shown on your device.",
            Prompt::Display(_) => "Type this code on your device, then press Enter.",
            Prompt::Authorize => "Allow this device to connect?",
        };
        prompt = prompt.push(shell.render.text(text).size(12));
        match &request.prompt {
            Prompt::Confirm(Some(code)) => {
                prompt = prompt.push(shell.render.text(format!("{code:06}")).size(18));
            }
            Prompt::Display(code) => {
                prompt = prompt.push(shell.render.text(code).size(18));
            }
            Prompt::Pin | Prompt::Passkey => {
                let mut input = text_input("Pairing code", shell.connections.pairing_text.as_str())
                    .id(cosmic::widget::Id::new("bluetooth-pairing"))
                    .size(13)
                    .on_input(move |value| {
                        cosmic::Action::App(Message::Connections(Event::PairingText(
                            serial,
                            id,
                            Password::new(value),
                        )))
                    });
                if request.accepts(shell.connections.pairing_text.as_str()) {
                    input = input.on_submit(move |value| {
                        drop(Password::new(value));
                        cosmic::Action::App(Message::Connections(Event::PairingReply(
                            serial, id, true,
                        )))
                    });
                }
                prompt = prompt.push(input);
            }
            _ => {}
        }
        let mut buttons = row![
            button::custom(shell.render.text("Cancel").size(12))
                .name("Cancel pairing")
                .padding([6, 10])
                .on_press(cosmic::Action::App(Message::Connections(
                    Event::PairingReply(serial, id, false)
                )))
        ]
        .spacing(6);
        if !matches!(request.prompt, Prompt::Display(_)) {
            let mut confirm = button::custom(shell.render.text("Confirm").size(12))
                .name("Confirm pairing")
                .padding([6, 10]);
            if request.accepts(shell.connections.pairing_text.as_str()) {
                confirm = confirm.on_press(cosmic::Action::App(Message::Connections(
                    Event::PairingReply(serial, id, true),
                )));
            }
            buttons = buttons.push(confirm);
        }
        rows = rows.push(prompt.push(buttons));
    }
    connections::footer(shell, rows, style, Discovery::Bluetooth)
}

pub(super) fn heading<'a>(
    shell: &'a FereseShell,
    mut heading: super::MenuHeading<'a>,
    style: MenuStyle,
) -> super::MenuHeading<'a> {
    if let Some(bluetooth) = &shell.status.bluetooth {
        heading = heading.push(shell_switch(
            shell.render,
            "Bluetooth",
            bluetooth.enabled,
            Action::Bluetooth(!bluetooth.enabled),
            style.theme,
            style.opacity,
        ));
    }
    heading
}
