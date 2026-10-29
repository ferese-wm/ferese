use cosmic::Element;
use cosmic::iced::{Alignment, Length};
use cosmic::widget::{column, row};

use super::Command;
use super::ui::{Input, message};
use crate::{App, Message, visuals};

impl App {
    pub(super) fn bluetooth_view(
        &self,
        result: &Result<super::bluetooth::Bluetooth, String>,
    ) -> Element<'static, Message> {
        let s = &self.connections;
        let p = visuals::Palette::from_resolved(&self.resolved.presented);
        let ready = s.busy.is_none();
        let Ok(bluetooth) = result else {
            return self.connection_card(
                column([])
                    .spacing(8)
                    .push(self.label("Bluetooth unavailable", 14.))
                    .push(self.connection_note(result.as_ref().expect_err(
                        "the unavailable-services branch contains a connection error",
                    ))),
            );
        };
        let mut body = column([]).spacing(6);
        if bluetooth.adapters.len() > 1 {
            let mut choices = row([]).spacing(6);
            for adapter in &bluetooth.adapters {
                choices = choices.push(self.connection_button(
                    &adapter.name,
                    Input::Adapter(adapter.path.clone()),
                    ready,
                    s.adapter.as_ref() == Some(&adapter.path),
                ));
            }
            body = body.push(choices);
        }
        let Some(adapter) = bluetooth
            .adapters
            .iter()
            .find(|a| Some(&a.path) == s.adapter.as_ref())
        else {
            return body
                .push(self.connection_note("No Bluetooth adapter is available."))
                .into();
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
                            .push(self.label("Bluetooth", 15.))
                            .push(self.connection_note(&format!(
                                "{} · {}",
                                adapter.name,
                                if adapter.powered { "On" } else { "Off" }
                            ))),
                    )
                    .push(
                        ferese_theme::controls::switch(adapter.powered, p)
                            .name("Bluetooth power")
                            .on_press_maybe(ready.then(|| {
                                message(Input::Run(Command::BluetoothPower(
                                    adapter.path.clone(),
                                    !adapter.powered,
                                )))
                            })),
                    ),
            ),
        );
        if !adapter.powered {
            return body.into();
        }
        let scanning = s
            .client
            .as_ref()
            .is_some_and(|c| c.discovering(&adapter.path));
        body = body.push(
            row([])
                .align_y(Alignment::Center)
                .push(
                    self.label(
                        if scanning {
                            "Searching for devices…"
                        } else {
                            "Devices"
                        },
                        14.,
                    )
                    .width(Length::Fill),
                )
                .push(self.connection_button(
                    if scanning {
                        "Stop scan"
                    } else {
                        "Find devices"
                    },
                    Input::Run(if scanning {
                        Command::BluetoothStop
                    } else {
                        Command::BluetoothScan(adapter.path.clone())
                    }),
                    ready,
                    false,
                )),
        );
        body = body.push(self.connection_note(if adapter.discovering {
            "Put your device in pairing mode. Discovery stops automatically after 30 seconds."
        } else {
            "Choose Find devices to discover nearby devices in pairing mode."
        }));
        let devices: Vec<_> = bluetooth
            .devices
            .iter()
            .filter(|d| d.adapter == adapter.path)
            .collect();
        for paired in [true, false] {
            body = body.push(self.label(
                if paired {
                    "Paired devices"
                } else {
                    "Nearby devices"
                },
                14.,
            ));
            if !devices.iter().any(|d| d.paired == paired) {
                body = body.push(self.connection_note(if paired {
                    "No paired devices yet."
                } else {
                    "No nearby devices found."
                }));
            }
            let device_iter = devices.iter().filter(|d| d.paired == paired);
            for device in device_iter {
                let state = if device.connected {
                    "Connected"
                } else if device.paired {
                    "Disconnected"
                } else {
                    "Not paired"
                };
                let detail = format!(
                    "{state} · {}{}{}",
                    device.address,
                    if device.trusted { " · Trusted" } else { "" },
                    device
                        .battery
                        .map(|b| format!(" · {b}% battery"))
                        .unwrap_or_default()
                );
                let mut controls = row([]).spacing(6);
                let action = if device.connected {
                    Command::BluetoothDisconnect(device.path.clone())
                } else if device.paired {
                    Command::BluetoothConnect(device.path.clone())
                } else {
                    Command::BluetoothPair(device.path.clone())
                };
                controls = controls.push(self.connection_button(
                    if device.connected {
                        "Disconnect"
                    } else if device.paired {
                        "Connect"
                    } else {
                        "Pair"
                    },
                    Input::Run(action),
                    ready,
                    false,
                ));
                if device.paired {
                    controls = controls.push(self.connection_button(
                        if device.trusted { "Untrust" } else { "Trust" },
                        Input::Run(Command::BluetoothTrust(
                            device.path.clone(),
                            !device.trusted,
                        )),
                        ready,
                        false,
                    ));
                    controls = controls.push(self.connection_button(
                        "Forget",
                        Input::Forget(Command::BluetoothForget(
                            device.adapter.clone(),
                            device.path.clone(),
                        )),
                        ready,
                        false,
                    ));
                }
                let icon = if device.icon.contains("audio") {
                    "M3 9v6h4l5 4V5L7 9z M16 8a6 6 0 0 1 0 8"
                } else {
                    "M12 3l6 5-12 8 6 5V3 M6 8l12 8-6 5"
                };
                body = body.push(
                    self.connection_card(
                        row([])
                            .spacing(8)
                            .align_y(Alignment::Center)
                            .push(visuals::action_icon(
                                icon,
                                if device.connected { p.accent } else { p.muted },
                            ))
                            .push(
                                column([])
                                    .spacing(2)
                                    .width(Length::Fill)
                                    .push(self.label(device.name.clone(), 13.))
                                    .push(self.connection_note(&detail)),
                            )
                            .push(controls),
                    ),
                );
            }
        }
        body.into()
    }
}
