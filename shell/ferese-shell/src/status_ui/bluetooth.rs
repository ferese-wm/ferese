use cosmic::iced::Length;
use cosmic::widget::column;
use cosmic::{Element, theme};

use super::bar::status_icon;
use super::controls::status_summary;
use super::{Menu, MenuRows, MenuStyle};
use crate::status::Action;
use crate::{FereseShell, Message};

const DEVICE_LIST_HEIGHT: f32 = 180.0;

fn device_list_height(count: usize) -> Option<f32> {
    (count > 4).then_some(DEVICE_LIST_HEIGHT)
}

pub(super) fn view<'a>(shell: &'a FereseShell, mut rows: MenuRows<'a>, style: MenuStyle, in_hub: bool) -> MenuRows<'a> {
    let MenuStyle { muted, .. } = style;
    if let Some(b) = &shell.status.bluetooth {
        rows = rows.push(status_summary(
            shell.render,
            status_icon(Menu::Bluetooth, &shell.status).0,
            if b.enabled {
                "Bluetooth is on"
            } else {
                "Bluetooth is off"
            },
            if !b.enabled {
                "Device connections are paused"
            } else if b.devices.is_empty() {
                "No devices connected"
            } else {
                "Your connected devices"
            },
            style,
            b.enabled,
            Some((b.enabled, Action::Bluetooth(!b.enabled))),
        ));

        if b.enabled && !b.devices.is_empty() {
            let mut devices = column::with_capacity(b.devices.len()).spacing(8);
            for device in &b.devices {
                devices = devices.push(
                    column![
                        shell.render.text(device).size(14),
                        shell.render.text("Connected").size(12).class(theme::Text::Color(muted)),
                    ]
                    .spacing(3),
                );
            }

            // Morph scrolls the whole view, so its device list needs no nested viewport.
            let list: Element<'_, cosmic::Action<Message>> =
                if let Some(height) = device_list_height(b.devices.len()).filter(|_| !in_hub) {
                    cosmic::iced::widget::scrollable(devices)
                        .height(height)
                        .width(Length::Fill)
                        .into()
                } else {
                    devices.into()
                };
            rows = rows.push(list);
        }
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn short_device_lists_shrink_and_long_lists_are_bounded() {
        for count in 0..=4 {
            assert_eq!(device_list_height(count), None);
        }
        for count in [5, 20, 1000] {
            assert_eq!(device_list_height(count), Some(180.0));
        }
    }
}
