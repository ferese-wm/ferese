use cosmic::iced::border::Shape as BorderShape;
use cosmic::iced::{Alignment, Background, Border, Rectangle};
use cosmic::widget::{button, container, row};
use cosmic::{Element, theme};

use super::Menu;
use crate::status::Snapshot;
use crate::{BarMetrics, FereseShell, Message, accented_icon, bar_content, color, motion, recording, text};

impl FereseShell {
    pub(crate) fn view_media_item(
        &self,
        representation: crate::panel::Representation,
        selected: bool,
    ) -> Element<'_, cosmic::Action<Message>> {
        let theme = self.config.theme.for_bar();
        if representation == crate::panel::Representation::Icon {
            self.view_status_item(Menu::Media, false, selected)
        } else {
            super::media::bar(
                self,
                theme,
                BarMetrics::from(self.config.panels[0].geometry),
                representation,
                selected,
            )
        }
    }

    pub(crate) fn view_status_item(
        &self,
        kind: Menu,
        battery_percentage: bool,
        selected: bool,
    ) -> Element<'_, cosmic::Action<Message>> {
        let theme = self.config.theme.for_bar();
        let metrics = BarMetrics::from(self.config.panels[0].geometry);
        let (source, enabled) = if kind == Menu::Recording {
            let source: &'static [u8] = match self.recorder.state {
                recording::State::Selecting => ferese_theme::icons::RECORD_CANCEL,
                recording::State::Recording(_) => ferese_theme::icons::RECORD_STOP,
                recording::State::Saving => ferese_theme::icons::RECORD_SAVING,
                _ => ferese_theme::icons::RECORD,
            };
            (source, true)
        } else {
            status_icon(kind, &self.status)
        };
        let selected =
            selected || (kind == Menu::Recording && matches!(self.recorder.state, recording::State::Recording(_)));
        let foreground = color(if selected {
            theme.accent
        } else if enabled {
            theme.text_primary
        } else {
            theme.text_muted
        });
        let mut content = row![accented_icon(
            source,
            metrics.icon_size,
            foreground,
            color(theme.accent)
        )]
        .spacing(4)
        .align_y(Alignment::Center);

        if kind == Menu::Recording
            && let Some(elapsed) = self.recorder.elapsed()
        {
            content = content.push(
                text(elapsed)
                    .size(metrics.text_size)
                    .class(theme::Text::Color(foreground)),
            );
        }

        if kind == Menu::Battery
            && battery_percentage
            && let Some(battery) = &self.status.battery
        {
            content = content.push(
                text(format!("{}%", battery.percent))
                    .size(metrics.text_size)
                    .class(theme::Text::Color(foreground)),
            );
        }
        if kind == Menu::Notifications
            && self
                .status
                .notifications
                .as_ref()
                .is_some_and(|n| n.count > 0 && !n.dnd)
        {
            content = content.push(
                container(text(""))
                    .width(4)
                    .height(4)
                    .class(theme::Container::custom(move |_| container::Style {
                        background: Some(Background::Color(foreground)),
                        border: Border {
                            shape: BorderShape::Continuous,
                            radius: motion::radius(2.0).into(),
                            ..Default::default()
                        },
                        ..Default::default()
                    })),
            );
        }
        let content = bar_content(content, metrics.group_item_height);
        let recording_busy = kind == Menu::Recording && self.recorder.busy();
        let control = button::custom(content)
            .name(if recording_busy {
                self.recorder.label().into()
            } else if kind == Menu::Recording {
                "Record a display".into()
            } else {
                status_label(kind, &self.status)
            })
            .padding([0.0, ((metrics.height - f32::from(metrics.icon_size)) * 0.5).max(4.0)])
            .height(metrics.group_item_height)
            .on_press_with_rectangle(move |offset, bounds| {
                if kind == Menu::Recording {
                    return cosmic::Action::App(if recording_busy {
                        Message::StopRecording
                    } else {
                        Message::StartRecording
                    });
                }
                cosmic::Action::App(Message::OpenMenu(
                    kind,
                    Rectangle {
                        x: (bounds.x - offset.x).round() as i32,
                        y: (bounds.y - offset.y).round() as i32,
                        width: bounds.width.round() as i32,
                        height: bounds.height.round() as i32,
                    },
                ))
            });
        motion::button(control, foreground, selected, 1.0)
    }
}

fn status_label(kind: Menu, s: &Snapshot) -> String {
    match kind {
        Menu::Network => match &s.network {
            Some(n) if n.enabled => format!("Wi-Fi: {}", n.connection.as_deref().unwrap_or("not connected")),
            _ => "Wi-Fi: off".into(),
        },
        Menu::Bluetooth => match &s.bluetooth {
            Some(b) if b.enabled => format!("Bluetooth: on, {} devices connected", b.devices.len()),
            _ => "Bluetooth: off".into(),
        },
        Menu::Audio => match &s.audio {
            Some(a) if !a.muted && a.volume > 0 => format!("Audio: {}%", a.volume),
            _ => "Audio: muted".into(),
        },
        Menu::Notifications => match &s.notifications {
            Some(n) if n.dnd => "Notifications: do not disturb".into(),
            Some(n) => format!("Notifications: {} unread", n.count),
            _ => "Notifications: unavailable".into(),
        },
        Menu::Calendar => "Calendar".into(),
        Menu::Overflow => "More panel items".into(),
        Menu::Recording => "Screen recording".into(),
        Menu::Media => "Now playing".into(),
        Menu::Battery => s.battery.as_ref().map_or_else(
            || "Battery: unavailable".into(),
            |b| format!("Battery: {}%, {}", b.percent, b.status),
        ),
        Menu::System => "Control Center".into(),
    }
}

pub(super) fn audio_icon(volume: u8, muted: bool) -> &'static [u8] {
    if muted || volume == 0 {
        ferese_theme::icons::VOLUME_MUTE
    } else if volume <= 33 {
        ferese_theme::icons::VOLUME_LOW
    } else if volume <= 66 {
        ferese_theme::icons::VOLUME_MEDIUM
    } else {
        ferese_theme::icons::VOLUME_HIGH
    }
}

pub(super) fn status_icon(kind: Menu, s: &Snapshot) -> (&'static [u8], bool) {
    match kind {
        Menu::Network => {
            let Some(n) = &s.network else {
                return (ferese_theme::icons::WIFI_OFF, false);
            };
            if !n.enabled || n.connection.is_none() {
                (ferese_theme::icons::WIFI_OFF, false)
            } else if n.signal <= 33 {
                (ferese_theme::icons::WIFI_LOW, true)
            } else if n.signal <= 66 {
                (ferese_theme::icons::WIFI_MEDIUM, true)
            } else {
                (ferese_theme::icons::WIFI_FULL, true)
            }
        }
        Menu::Bluetooth => match &s.bluetooth {
            Some(b) if b.enabled && !b.devices.is_empty() => (ferese_theme::icons::BLUETOOTH_CONNECTED, true),
            Some(b) if b.enabled => (ferese_theme::icons::BLUETOOTH_ON, true),
            _ => (ferese_theme::icons::BLUETOOTH_OFF, false),
        },
        Menu::Audio => s.audio.as_ref().map_or((audio_icon(0, true), false), |a| {
            (audio_icon(a.volume, a.muted), !a.muted && a.volume > 0)
        }),
        Menu::Calendar => (ferese_theme::icons::CALENDAR, true),
        Menu::Overflow => (ferese_theme::icons::CHEVRON_DOWN, true),
        Menu::Recording => (ferese_theme::icons::RECORD, true),
        Menu::Media => (ferese_theme::icons::MEDIA, true),
        Menu::Battery => {
            let Some(b) = &s.battery else {
                return (ferese_theme::icons::BATTERY_EMPTY, false);
            };
            let icon: &'static [u8] = if b.status == "Charging" {
                ferese_theme::icons::BATTERY_CHARGING
            } else if b.percent >= 80 {
                ferese_theme::icons::BATTERY_FULL
            } else if b.percent >= 50 {
                ferese_theme::icons::BATTERY_75
            } else if b.percent >= 20 {
                ferese_theme::icons::BATTERY_50
            } else if b.percent > 0 {
                ferese_theme::icons::BATTERY_25
            } else {
                ferese_theme::icons::BATTERY_EMPTY
            };
            (icon, true)
        }
        Menu::Notifications => {
            if s.notifications.as_ref().is_some_and(|n| n.dnd) {
                (ferese_theme::icons::NOTIFICATIONS_OFF, false)
            } else {
                (ferese_theme::icons::NOTIFICATIONS, true)
            }
        }
        Menu::System => (ferese_theme::icons::CONTROL_CENTER, true),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::status;
    #[test]
    fn icons_follow_real_states() {
        let mut s = Snapshot {
            audio: Some(status::Audio {
                volume: 80,
                muted: true,
                output: String::new(),
            }),
            ..Default::default()
        };
        assert_eq!(status_icon(Menu::Audio, &s), (ferese_theme::icons::VOLUME_MUTE, false));

        s.battery = Some(status::Battery {
            percent: 2,
            status: "Charging".into(),
        });
        assert_eq!(status_icon(Menu::Battery, &s).0, ferese_theme::icons::BATTERY_CHARGING);

        s.notifications = Some(status::Notifications { count: 3, dnd: true });
        assert_eq!(
            status_icon(Menu::Notifications, &s).0,
            ferese_theme::icons::NOTIFICATIONS_OFF
        );
        assert_eq!(status_label(Menu::Notifications, &s), "Notifications: do not disturb");

        s.bluetooth = Some(status::Bluetooth {
            enabled: false,
            devices: vec!["Headphones".into()],
        });
        assert_eq!(status_icon(Menu::Bluetooth, &s).0, ferese_theme::icons::BLUETOOTH_OFF);
    }

    #[test]
    fn battery_bands_and_live_label_follow_charge_state() {
        let mut s = Snapshot::default();
        for (percent, expected) in [
            (0, ferese_theme::icons::BATTERY_EMPTY),
            (19, ferese_theme::icons::BATTERY_25),
            (20, ferese_theme::icons::BATTERY_50),
            (50, ferese_theme::icons::BATTERY_75),
            (80, ferese_theme::icons::BATTERY_FULL),
        ] {
            s.battery = Some(status::Battery {
                percent,
                status: "Discharging".into(),
            });
            assert_eq!(status_icon(Menu::Battery, &s).0, expected);
            assert_eq!(
                status_label(Menu::Battery, &s),
                format!("Battery: {percent}%, Discharging")
            );
        }
    }
}
