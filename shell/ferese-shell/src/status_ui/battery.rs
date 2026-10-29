use cosmic::iced::{Alignment, Length};
use cosmic::widget::{button, column, container, row};
use cosmic::{Element, theme};

use super::controls::level_control;
use super::{Menu, MenuRows, MenuStyle};
use crate::status::Action;
use crate::{
    FereseShell, Message, ShellTheme, accented_icon, bar_icon, color_with_opacity, motion,
};

pub(super) fn view<'a>(
    shell: &'a FereseShell,
    mut rows: MenuRows<'a>,
    style: MenuStyle,
) -> MenuRows<'a> {
    let MenuStyle {
        theme,
        primary,
        muted,
        opacity: p,
    } = style;
    if let Some(b) = &shell.status.battery {
        rows = rows.push(
            row![
                accented_icon(
                    super::bar::status_icon(Menu::Battery, &shell.status).0,
                    28,
                    primary,
                    color_with_opacity(theme.accent, p)
                ),
                shell.render.text(format!("{}%", b.percent)).size(14),
            ]
            .spacing(8)
            .align_y(Alignment::Center),
        );
        rows = rows.push(
            shell
                .render
                .text(match b.status.as_str() {
                    "Charging" => "Connected to power",
                    "Discharging" => "Running on battery power",
                    "Full" => "Battery fully charged",
                    _ => "Battery status reported by the system",
                })
                .size(12)
                .class(theme::Text::Color(muted)),
        );
    }
    rows = rows.push(ferese_theme::menus::section_label(
        "Power mode",
        shell.render.font,
        muted,
    ));
    if let Some(profiles) = &shell.status.power_profiles {
        let mut modes = row::with_capacity(3).spacing(6).width(Length::Fill);
        let label_iter = [
            (
                "Power saver",
                ferese_theme::icons::POWER_SAVER,
                "power-saver",
            ),
            ("Balanced", ferese_theme::icons::POWER_BALANCED, "balanced"),
            (
                "Performance",
                ferese_theme::icons::POWER_PERFORMANCE,
                "performance",
            ),
        ]
        .into_iter()
        .enumerate();
        for (index, (label, icon, profile)) in label_iter {
            if profiles.available[index] {
                modes = modes.push(power_tile(
                    shell.render,
                    label,
                    icon,
                    profile,
                    profiles.active == profile,
                    theme,
                    p,
                ));
            }
        }
        rows = rows.push(modes);
    } else {
        rows = rows.push(
            shell
                .render
                .text("Power Profiles service unavailable")
                .size(12)
                .class(theme::Text::Color(muted)),
        );
    }

    let mut power = row::with_capacity(3).spacing(6).width(Length::Fill);
    for (enabled, label, icon, action) in [
        (
            shell.status.reboot,
            "Restart",
            ferese_theme::icons::RESTART,
            Action::Reboot,
        ),
        (
            shell.status.poweroff,
            "Power off",
            ferese_theme::icons::POWER_OFF,
            Action::Poweroff,
        ),
        (
            shell.status.suspend,
            "Suspend",
            ferese_theme::icons::SUSPEND,
            Action::Suspend,
        ),
    ] {
        if enabled {
            power = power.push(power_tile_action(
                shell.render,
                label,
                icon,
                action,
                theme,
                p,
            ));
        }
    }

    if shell.status.reboot || shell.status.poweroff || shell.status.suspend {
        rows = rows.push(ferese_theme::menus::section_label(
            "Power",
            shell.render.font,
            muted,
        ));
        rows = rows.push(power);
    }
    rows
}

pub(super) fn brightness<'a>(
    shell: &'a FereseShell,
    mut rows: MenuRows<'a>,
    style: MenuStyle,
) -> MenuRows<'a> {
    if let Some(value) = shell.status.brightness {
        rows = rows.push(level_control(
            shell.render,
            "Display brightness",
            container(bar_icon(ferese_theme::icons::BRIGHTNESS, 16, style.primary))
                .padding(2)
                .into(),
            value,
            1..=100,
            Action::Brightness,
            style,
        ));
    }
    rows
}

fn power_tile<'a>(
    render: crate::rendering::Rendering,
    label: &'static str,
    icon: &'static [u8],
    profile: &'static str,
    selected: bool,
    theme: ShellTheme,
    opacity: f32,
) -> Element<'a, cosmic::Action<Message>> {
    let foreground = color_with_opacity(
        if selected {
            theme.accent
        } else {
            theme.text_primary
        },
        opacity,
    );
    let tile = container(
        column![bar_icon(icon, 19, foreground), render.text(label).size(11)]
            .spacing(3)
            .align_x(Alignment::Center),
    )
    .width(Length::Fill)
    .center_x(Length::Fill);
    let btn = button::custom(tile)
        .width(Length::FillPortion(1))
        .padding([8, 2])
        .on_press(cosmic::Action::App(Message::Control(Action::PowerProfile(
            profile,
        ))));

    motion::button(render, btn, foreground, selected, opacity)
}

fn power_tile_action<'a>(
    render: crate::rendering::Rendering,
    label: &'static str,
    icon: &'static [u8],
    action: Action,
    theme: ShellTheme,
    opacity: f32,
) -> Element<'a, cosmic::Action<Message>> {
    let foreground = color_with_opacity(theme.text_primary, opacity);
    let tile = container(
        column![bar_icon(icon, 19, foreground), render.text(label).size(11)]
            .spacing(3)
            .align_x(Alignment::Center),
    )
    .width(Length::Fill)
    .center_x(Length::Fill);
    let btn = button::custom(tile)
        .width(Length::FillPortion(1))
        .padding([8, 2])
        .on_press(cosmic::Action::App(Message::ConfirmPower(action)));

    motion::button(render, btn, foreground, false, opacity)
}
