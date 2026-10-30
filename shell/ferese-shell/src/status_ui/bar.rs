use cosmic::iced::border::Shape as BorderShape;
use cosmic::iced::{Alignment, Background, Border, Rectangle};
use cosmic::widget::{button, container, row};
use cosmic::{Element, theme};

use super::Menu;
use crate::status::Snapshot;
use crate::{
    BarMetrics, FereseShell, Message, accented_icon, bar_content, color, motion, recording,
};

impl FereseShell {
    pub(crate) fn view_media_item(
        &self,
        representation: crate::panel::Representation,
        selected: bool,
        width: Option<f32>,
    ) -> Element<'_, cosmic::Action<Message>> {
        let theme = self.config.theme.for_bar();
        if representation == crate::panel::Representation::Icon {
            self.view_status_item(Menu::Media, false, selected)
        } else {
            super::media::bar(
                self,
                theme,
                BarMetrics::from(self.config.panels[0].resolved_geometry()),
                representation,
                selected,
                width,
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
        let metrics = BarMetrics::from(self.config.panels[0].resolved_geometry());
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
        let selected = selected
            || (kind == Menu::Recording
                && matches!(self.recorder.state, recording::State::Recording(_)));
        let foreground = color(if selected {
            theme.accent
        } else if enabled {
            theme.text_primary
        } else {
            theme.text_muted
        });
        let mut indicator = if kind == Menu::Battery {
            self.battery_indicator(theme, 1.0, battery_percentage, 14.)
        } else {
            accented_icon(source, metrics.icon_size, foreground, color(theme.accent)).into()
        };
        if kind == Menu::Notifications {
            let unread = self
                .status
                .notifications
                .as_ref()
                .is_some_and(|n| n.count > 0 && !n.dnd);
            let dot = container(cosmic::widget::Space::new())
                .width(4)
                .height(4)
                .class(theme::Container::custom(move |_| container::Style {
                    background: unread.then_some(Background::Color(foreground)),
                    border: Border {
                        shape: BorderShape::Continuous,
                        radius: self.render.radius(2.0).into(),
                        ..Default::default()
                    },
                    ..Default::default()
                }));
            // Keep the popup anchor stable when reading clears the badge.
            indicator = cosmic::iced::widget::stack![
                indicator,
                container(dot)
                    .width(metrics.icon_size)
                    .height(metrics.icon_size)
                    .align_x(Alignment::End),
            ]
            .into();
        }
        let icon_width = if kind == Menu::Battery {
            crate::battery_indicator::WIDTH
        } else {
            f32::from(metrics.icon_slot_width)
        };
        let indicator = if kind == Menu::Battery {
            container(indicator)
        } else {
            container(indicator).center_x(icon_width)
        };
        let mut content = row![indicator].spacing(4).align_y(Alignment::Center);

        if kind == Menu::Recording
            && let Some(elapsed) = self.recorder.elapsed()
        {
            content = content.push(
                self.render
                    .panel_text(elapsed)
                    .class(theme::Text::Color(foreground)),
            );
        }
        let recording_busy = kind == Menu::Recording && self.recorder.busy();
        let content = bar_content(content, metrics.group_item_height);
        let control = button::custom(content)
            .name(if recording_busy {
                self.recorder.label().into()
            } else if kind == Menu::Recording {
                "Record a display".into()
            } else {
                status_label(kind, &self.status)
            })
            .padding([0.0, ((metrics.height - icon_width) * 0.5).max(4.0)])
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
        motion::button(self.render, control, foreground, selected, 1.0)
    }
}

fn status_label(kind: Menu, s: &Snapshot) -> String {
    match kind {
        Menu::Network => match &s.network {
            Some(n) if n.enabled => format!(
                "Wi-Fi: {}",
                n.connection.as_deref().unwrap_or("not connected")
            ),
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
        Menu::Activities => "Activities".into(),
        Menu::Workspaces => "Workspaces".into(),
        Menu::Calendar => "Calendar".into(),
        Menu::Overflow => "More panel items".into(),
        Menu::Recording => "Screen recording".into(),
        Menu::CaptureStatus => "Capture status".into(),
        Menu::Media => "Now playing".into(),
        Menu::Battery => s.battery.as_ref().map_or_else(
            || "Battery: unavailable".into(),
            |b| format!("Battery: {}%, {}", b.percent, b.status),
        ),
        Menu::System => "Control Center".into(),
        Menu::NightLight => {
            if s.night_light.as_ref().is_some_and(|s| s.enabled) {
                "Night light: on".into()
            } else {
                "Night light: off".into()
            }
        }
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
        Menu::NightLight => (
            ferese_theme::icons::NIGHT_LIGHT,
            s.night_light.as_ref().is_some_and(|s| s.enabled),
        ),
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
            Some(b) if b.enabled && !b.devices.is_empty() => {
                (ferese_theme::icons::BLUETOOTH_CONNECTED, true)
            }
            Some(b) if b.enabled => (ferese_theme::icons::BLUETOOTH_ON, true),
            _ => (ferese_theme::icons::BLUETOOTH_OFF, false),
        },
        Menu::Audio => s.audio.as_ref().map_or((audio_icon(0, true), false), |a| {
            (audio_icon(a.volume, a.muted), !a.muted && a.volume > 0)
        }),
        Menu::Calendar => (ferese_theme::icons::CALENDAR, true),
        Menu::Overflow => (ferese_theme::icons::CHEVRON_DOWN, true),
        Menu::Recording => (ferese_theme::icons::RECORD, true),
        Menu::CaptureStatus => (ferese_theme::icons::DISPLAY, true),
        Menu::Media => (ferese_theme::icons::MEDIA, true),
        Menu::Battery => (
            crate::battery_indicator::icon(s.battery.as_ref()),
            s.battery.is_some(),
        ),
        Menu::Notifications => {
            if s.notifications.as_ref().is_some_and(|n| n.dnd) {
                (ferese_theme::icons::NOTIFICATIONS_OFF, false)
            } else {
                (ferese_theme::icons::NOTIFICATIONS, true)
            }
        }
        Menu::Workspaces => (ferese_theme::icons::OVERVIEW, true),
        Menu::Activities => (ferese_theme::icons::FERESE, true),
        Menu::System => (ferese_theme::icons::CONTROL_CENTER, true),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::status;

    #[test]
    fn status_hitboxes_stay_aligned_when_badges_and_battery_values_change() {
        use cosmic::iced::Size;
        use cosmic::iced::advanced::renderer::Headless;
        use cosmic::iced::advanced::{layout, widget};

        let renderer = cosmic::iced::futures::executor::block_on(cosmic::Renderer::new(
            cosmic::iced::Font::default(),
            cosmic::iced::Pixels(14.),
            Some("tiny-skia"),
        ))
        .unwrap();
        let mut shell = crate::tests::shell_with_measured_panel();
        let measure = |shell: &FereseShell, kind, percentage| {
            let mut view = shell.view_status_item(kind, percentage, false);
            let mut tree = widget::Tree::new(&view);
            view.as_widget_mut()
                .layout(
                    &mut tree,
                    &renderer,
                    &layout::Limits::new(Size::ZERO, Size::new(1000., 1000.)),
                )
                .size()
        };
        for height in [28., 36., 48.] {
            shell.config.panels[0].geometry.height = Some(height);
            let ordinary = measure(&shell, Menu::System, false);
            for (count, dnd) in [(0, false), (3, false), (0, false), (3, true)] {
                shell.status.notifications = Some(status::Notifications { count, dnd });
                assert_eq!(
                    measure(&shell, Menu::Notifications, false),
                    ordinary,
                    "notification badges must stay inside the normal icon hitbox"
                );
            }
            for percent in [0, 9, 50, 100] {
                shell.status.battery = Some(status::Battery {
                    percent,
                    status: "Discharging".into(),
                });
                for percentage in [false, true] {
                    let battery = measure(&shell, Menu::Battery, percentage);
                    assert_eq!(battery.height, ordinary.height);
                    assert!(
                        battery.width <= ordinary.width.max(38.5),
                        "battery adds excess horizontal padding: {battery:?} vs {ordinary:?}"
                    );
                    assert!(
                        battery.width >= crate::battery_indicator::WIDTH + 8.,
                        "battery must retain space around the body and terminal"
                    );
                }
            }
        }
    }

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
        assert_eq!(
            status_icon(Menu::Audio, &s),
            (ferese_theme::icons::VOLUME_MUTE, false)
        );

        s.battery = Some(status::Battery {
            percent: 2,
            status: "Charging".into(),
        });
        assert_eq!(
            status_icon(Menu::Battery, &s).0,
            ferese_theme::icons::BATTERY_CHARGING
        );

        s.notifications = Some(status::Notifications {
            count: 3,
            dnd: true,
        });
        assert_eq!(
            status_icon(Menu::Notifications, &s).0,
            ferese_theme::icons::NOTIFICATIONS_OFF
        );
        assert_eq!(
            status_label(Menu::Notifications, &s),
            "Notifications: do not disturb"
        );

        s.bluetooth = Some(status::Bluetooth {
            enabled: false,
            devices: vec!["Headphones".into()],
            ..Default::default()
        });
        assert_eq!(
            status_icon(Menu::Bluetooth, &s).0,
            ferese_theme::icons::BLUETOOTH_OFF
        );
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
