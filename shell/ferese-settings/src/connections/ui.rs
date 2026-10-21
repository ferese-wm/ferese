use std::fmt;

use cosmic::Element;
use cosmic::iced::{Alignment, Length};
use cosmic::widget::{self, button, column, container, row};
use zeroize::Zeroizing;

use super::{Command, Network, Security, Tab};
use crate::{App, Message, visuals};

#[derive(Clone)]
pub(crate) enum Input {
    Refresh,
    PollPrompt,
    Tab(Tab),
    Adapter(String),
    Run(Command),
    Select(Network),
    Password(Zeroizing<String>),
    SubmitWifi,
    CancelWifi,
    Hidden(bool),
    HiddenName(String),
    HiddenSecurity(Security),
    PairingValue(u64, Zeroizing<String>),
    Answer(u64, bool),
    Forget(Command),
    ConfirmForget,
    CancelForget,
}

impl fmt::Debug for Input {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ConnectionsInput")
    }
}

pub(super) fn message(input: Input) -> Message {
    Message::Connections(input)
}

impl App {
    pub(super) fn connection_button(
        &self,
        label: &str,
        input: Input,
        enabled: bool,
        selected: bool,
    ) -> Element<'static, Message> {
        let p = visuals::Palette::from_resolved(&self.resolved.presented);
        let (icon, icon_only) = match &input {
            Input::Refresh => (Some("M20 7V3l-3 3a8 8 0 1 0 3 6 M20 3h-5"), true),
            Input::Forget(_) | Input::ConfirmForget => (
                Some("M4 6h16 M9 6V3h6v3 M6 6l1 15h10l1-15 M10 10v7 M14 10v7"),
                matches!(input, Input::Forget(_)),
            ),
            Input::Select(network) if network.saved.is_some() => {
                (Some("M8 15a5 5 0 1 1 4-4l9-9 M16 7l3 3 M13 10l3 3"), true)
            }
            Input::Run(Command::BluetoothTrust(_, on)) => (
                Some(if *on {
                    "M12 3l8 3v6c0 5-8 9-8 9s-8-4-8-9V6z"
                } else {
                    "M12 3l8 3v6c0 5-8 9-8 9s-8-4-8-9V6z M8 12l3 3 5-6"
                }),
                true,
            ),
            Input::Tab(Tab::Wifi) => (Some(crate::schema::Page::Connections.icon()), false),
            Input::Tab(Tab::Bluetooth) => (Some("M12 3l6 5-12 8 6 5V3 M6 8l12 8-6 5"), false),
            Input::Run(Command::WifiScan | Command::BluetoothScan(_)) => (
                Some("M10 3a7 7 0 1 0 0 14a7 7 0 0 0 0-14 M15 15l6 6"),
                false,
            ),
            Input::Run(Command::BluetoothStop) => (Some("M6 6h12v12H6z"), false),
            Input::Run(
                Command::WifiConnect(..) | Command::BluetoothConnect(_) | Command::BluetoothPair(_),
            )
            | Input::Select(_)
            | Input::SubmitWifi => (Some("M4 12h15 M14 7l5 5-5 5"), false),
            Input::Run(Command::WifiDisconnect(_) | Command::BluetoothDisconnect(_)) => {
                (Some("M7 7l10 10 M17 7L7 17"), false)
            }
            Input::Hidden(_) => (Some("M12 5v14 M5 12h14"), false),
            Input::Answer(..) => (Some("M5 12l4 4L19 6"), false),
            _ => (None, false),
        };
        let mut content = row([]).spacing(5).align_y(Alignment::Center);
        if let Some(icon) = icon {
            content = content.push(visuals::action_icon(
                icon,
                if selected { p.accent } else { p.text },
            ));
        }
        if !icon_only {
            content = content.push(self.label(label.to_owned(), 12.));
        }
        let control = button::custom(content)
            .name(label.to_owned())
            .padding(if icon_only { [4, 4] } else { [4, 8] })
            .class(visuals::button_style(p, selected))
            .on_press_maybe(enabled.then(|| message(input)));
        if icon_only {
            widget::tooltip(
                control,
                self.label(label.to_owned(), 11.),
                widget::tooltip::Position::Top,
            )
            .into()
        } else {
            control.into()
        }
    }

    pub(super) fn connection_note(&self, text: &str) -> Element<'static, Message> {
        let p = visuals::Palette::from_resolved(&self.resolved.presented);
        self.label(text.to_owned(), 11.)
            .class(cosmic::theme::Text::Color(p.muted))
            .into()
    }

    pub(super) fn connection_card<'a>(
        &self,
        body: impl Into<Element<'a, Message>>,
    ) -> Element<'a, Message> {
        let p = visuals::Palette::from_resolved(&self.resolved.presented);
        container(body)
            .padding([7, 10])
            .width(Length::Fill)
            .class(visuals::surface(p.card, p.radius))
            .into()
    }

    pub(crate) fn connections_view(&self) -> Element<'_, Message> {
        let s = &self.connections;
        let p = visuals::Palette::from_resolved(&self.resolved.presented);
        let mut body = column([]).spacing(6);
        body = body.push(
            row([])
                .spacing(6)
                .push(self.connection_button(
                    "Wi-Fi",
                    Input::Tab(Tab::Wifi),
                    true,
                    s.tab == Tab::Wifi,
                ))
                .push(self.connection_button(
                    "Bluetooth",
                    Input::Tab(Tab::Bluetooth),
                    true,
                    s.tab == Tab::Bluetooth,
                ))
                .push(widget::Space::new().width(Length::Fill))
                .push(self.connection_button("Refresh", Input::Refresh, !s.loading, false)),
        );
        if let Some(error) = &s.error {
            body = body.push(
                container(self.label(error.clone(), 12.))
                    .padding(8)
                    .width(Length::Fill)
                    .class(visuals::surface(p.error, 10.)),
            );
        }
        if let Some(busy) = s.busy {
            let mut status = row([])
                .align_y(Alignment::Center)
                .spacing(6)
                .push(self.label(busy, 12.));
            if busy == "Pairing…" {
                status = status.push(self.connection_button(
                    "Cancel",
                    Input::Run(Command::CancelPairing),
                    true,
                    false,
                ));
            }
            body = body.push(status);
        }
        // Pairing stays visible even if the user switches between the two tabs.
        if let Some(prompt) = &s.prompt {
            body = body.push(self.pairing_view(prompt));
        }
        if s.forget.is_some() {
            body = body.push(
                self.connection_card(
                    column([])
                        .spacing(6)
                        .push(self.label("Forget this connection?", 14.))
                        .push(self.connection_note(
                            "Saved credentials or pairing keys will be removed. You will need to connect again.",
                        ))
                        .push(
                            row([])
                                .spacing(8)
                                .push(self.connection_button("Cancel", Input::CancelForget, true, false))
                                .push(self.connection_button("Forget", Input::ConfirmForget, s.busy.is_none(), false)),
                        ),
                ),
            );
        }
        if let Some(snapshot) = &s.snapshot {
            body = body.push(match s.tab {
                Tab::Wifi => self.wifi_view(&snapshot.wifi),
                Tab::Bluetooth => self.bluetooth_view(&snapshot.bluetooth),
            });
        } else {
            body = body.push(self.connection_note(if s.loading {
                "Loading connection services…"
            } else {
                "Connection services are unavailable. Use Refresh to try again."
            }));
        }
        body.into()
    }
}
