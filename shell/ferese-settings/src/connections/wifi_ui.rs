use cosmic::Element;
use cosmic::iced::{Alignment, Length};
use cosmic::widget::{column, row, text_input};
use zeroize::{Zeroize, Zeroizing};

use super::ui::{Input, message};
use super::{Command, Security};
use crate::{App, Message, visuals};

impl App {
    pub(super) fn wifi_view(
        &self,
        result: &Result<super::wifi::Wifi, String>,
    ) -> Element<'_, Message> {
        let s = &self.connections;
        let p = visuals::Palette::from_resolved(&self.resolved.presented);
        let ready = s.busy.is_none();
        let Ok(wifi) = result else {
            return self.connection_card(
                column([])
                    .spacing(8)
                    .push(self.label("Wi-Fi unavailable", 14.))
                    .push(self.connection_note(result.as_ref().unwrap_err())),
            );
        };
        let mut body = column([]).spacing(6);
        let status = if wifi.radios.is_empty() {
            "No Wi-Fi adapter"
        } else if !wifi.hardware_enabled {
            "Blocked by a hardware switch or airplane mode"
        } else if wifi.enabled {
            "On"
        } else {
            "Off"
        };
        body = body.push(
            self.connection_card(
                row([])
                    .spacing(8)
                    .align_y(Alignment::Center)
                    .push(
                        column([])
                            .spacing(2)
                            .width(Length::Fill)
                            .push(self.label("Wi-Fi", 15.))
                            .push(self.connection_note(status)),
                    )
                    .push(
                        ferese_theme::controls::switch(wifi.enabled, p)
                            .name("Wi-Fi power")
                            .on_press_maybe(
                                (ready
                                    && (wifi.hardware_enabled || wifi.enabled)
                                    && !wifi.radios.is_empty())
                                .then(|| message(Input::Run(Command::WifiPower(!wifi.enabled)))),
                            ),
                    ),
            ),
        );
        if !wifi.enabled || !wifi.hardware_enabled || wifi.radios.is_empty() {
            return body.into();
        }
        body = body.push(
            row([])
                .align_y(Alignment::Center)
                .push(self.label("Networks", 14.).width(Length::Fill))
                .push(self.connection_button("Scan", Input::Run(Command::WifiScan), ready, false)),
        );
        if wifi.networks.is_empty() {
            body = body
                .push(self.connection_note("No networks found. Scan to look for nearby networks."));
        }
        for network in &wifi.networks {
            let radio = wifi
                .radios
                .iter()
                .find(|r| r.path == network.device)
                .map(|r| r.name.as_str())
                .unwrap_or("");
            let state = if network.connected {
                "Connected · "
            } else if network.connecting {
                "Connecting · "
            } else if network.saved.is_some() {
                "Saved · "
            } else {
                ""
            };
            let detail = format!(
                "{state}{} · {}% signal{}",
                network.security.label(),
                network.strength,
                if wifi.radios.len() > 1 {
                    format!(" · {radio}")
                } else {
                    String::new()
                }
            );
            let mut controls = row([]).spacing(6);
            if network.connected || network.connecting {
                controls = controls.push(self.connection_button(
                    "Disconnect",
                    Input::Run(Command::WifiDisconnect(network.device.clone())),
                    ready,
                    false,
                ));
            } else if network.saved.is_some()
                || !network.security.password() && network.security.can_create()
            {
                controls = controls.push(self.connection_button(
                    "Connect",
                    Input::Run(Command::WifiConnect(
                        network.clone(),
                        Zeroizing::new(String::new()),
                    )),
                    ready,
                    false,
                ));
            } else {
                controls = controls.push(self.connection_button(
                    "Connect…",
                    Input::Select(network.clone()),
                    ready,
                    false,
                ));
            }
            if network.saved.is_some() && network.security.password() {
                controls = controls.push(self.connection_button(
                    "Password…",
                    Input::Select(network.clone()),
                    ready,
                    false,
                ));
            }
            body = body.push(
                self.connection_card(
                    row([])
                        .spacing(8)
                        .align_y(Alignment::Center)
                        .push(visuals::action_icon(
                            super::super::schema::Page::Connections.icon(),
                            p.accent,
                        ))
                        .push(
                            column([])
                                .spacing(2)
                                .width(Length::Fill)
                                .push(self.label(network.name.clone(), 13.))
                                .push(self.connection_note(&detail)),
                        )
                        .push(controls),
                ),
            );
            if s.selected.as_ref().is_some_and(|selected| {
                selected.ssid == network.ssid
                    && selected.device == network.device
                    && selected.security == network.security
            }) {
                body = body.push(self.wifi_credentials());
            }
        }
        body = body.push(self.connection_button(
            "Join a hidden network…",
            Input::Hidden(true),
            ready,
            false,
        ));
        if s.hidden {
            body = body.push(self.wifi_credentials());
        }
        if !wifi.saved.is_empty() {
            body = body.push(self.label("Saved networks", 14.));
            for saved in &wifi.saved {
                body = body.push(
                    self.connection_card(
                        row([])
                            .spacing(8)
                            .align_y(Alignment::Center)
                            .push(visuals::action_icon(
                                crate::schema::Page::Connections.icon(),
                                p.muted,
                            ))
                            .push(
                                column([])
                                    .spacing(2)
                                    .width(Length::Fill)
                                    .push(self.label(saved.name.clone(), 13.))
                                    .push(self.connection_note(saved.security.label())),
                            )
                            .push(self.connection_button(
                                "Forget",
                                Input::Forget(Command::WifiForget(saved.path.clone())),
                                ready,
                                false,
                            )),
                    ),
                );
            }
        }
        body.into()
    }

    pub(super) fn wifi_credentials(&self) -> Element<'_, Message> {
        let s = &self.connections;
        let p = visuals::Palette::from_resolved(&self.resolved.presented);
        let target = self.wifi_target();
        let mut body = column([]).spacing(6).push(self.label(
            if s.hidden {
                "Hidden network".into()
            } else {
                format!(
                    "Join {}",
                    s.selected
                        .as_ref()
                        .map(|n| n.name.as_str())
                        .unwrap_or("network")
                )
            },
            16.,
        ));
        if s.hidden {
            body = body.push(
                text_input("Network name (SSID)", s.hidden_name.clone())
                    .font(self.font)
                    .size(13)
                    .padding([5, 8])
                    .style(visuals::input_style(p))
                    .on_input(|name| message(Input::HiddenName(name))),
            );
            let mut choices = row([]).spacing(6);
            for security in [
                Security::Personal,
                Security::Sae,
                Security::Open,
                Security::Owe,
            ] {
                choices = choices.push(self.connection_button(
                    security.label(),
                    Input::HiddenSecurity(security),
                    true,
                    s.hidden_security == security,
                ));
            }
            body = body.push(choices);
        }
        let security = target
            .as_ref()
            .map(|n| n.security)
            .unwrap_or(s.hidden_security);
        if security.password() {
            body = body.push(
                text_input("Network password", s.password.as_str())
                    .id(cosmic::widget::Id::new("wifi-password"))
                    .password()
                    .font(self.font)
                    .size(13)
                    .padding([5, 8])
                    .style(visuals::input_style(p))
                    .on_input(|value| message(Input::Password(Zeroizing::new(value))))
                    .on_submit(|mut value| {
                        value.zeroize();
                        message(Input::SubmitWifi)
                    }),
            );
        }
        if !security.can_create() {
            body = body.push(self.connection_note(
                "Enterprise and legacy networks need a saved NetworkManager profile with their security settings.",
            ));
        }
        body = body.push(
            row([])
                .spacing(8)
                .push(self.connection_button("Cancel", Input::CancelWifi, true, false))
                .push(self.connection_button(
                    "Connect",
                    Input::SubmitWifi,
                    target.is_some()
                        && security.can_create()
                        && super::wifi::valid_password(security, &s.password)
                        && s.busy.is_none(),
                    true,
                )),
        );
        self.connection_card(body)
    }
}
