use cosmic::iced::{Alignment, Length};
use cosmic::theme;
use cosmic::widget::{column, row, slider};

use super::{Action, FereseShell, MenuRows, MenuStyle, Message, control_card, toggle_row};

pub(super) fn view<'a>(
    shell: &'a FereseShell,
    mut rows: MenuRows<'a>,
    style: MenuStyle,
) -> MenuRows<'a> {
    let Some(status) = &shell.status.night_light else {
        return rows.push(shell.render.text("Night light is unavailable").size(13));
    };
    if status.supported_outputs == 0 {
        return rows.push(
            shell
                .render
                .text("No display supports night light")
                .size(12)
                .class(theme::Text::Color(style.muted)),
        );
    }
    rows = rows.push(toggle_row(
        shell.render,
        "Night light",
        status.enabled,
        Action::NightLight(!status.enabled),
        style.theme,
        style.opacity,
    ));
    let temperature = shell.night_light_temperature.unwrap_or(status.temperature);
    let warmth = column![
        row![
            shell
                .render
                .text("Color temperature")
                .size(12)
                .width(Length::Fill),
            shell
                .render
                .text(format!("{temperature} K"))
                .size(12)
                .class(theme::Text::Color(style.muted)),
        ]
        .align_y(Alignment::Center),
        slider(1000..=6500, temperature, |value| cosmic::Action::App(
            Message::NightLightTemperature(value)
        ))
        .step(100u16)
        .on_release(cosmic::Action::App(Message::CommitNightLightTemperature))
        .height(20)
        .handle_width(8)
        .class(super::controls::level_slider(style.primary)),
        shell
            .render
            .text("Lower temperatures are warmer")
            .size(11)
            .class(theme::Text::Color(style.muted)),
    ]
    .spacing(4);
    rows = rows.push(control_card(
        shell.render,
        warmth.into(),
        style.primary,
        style.opacity,
    ));
    if status.enabled && status.failed_outputs > 0 {
        rows = rows.push(
            shell
                .render
                .text("Night light could not be applied to some displays")
                .size(12),
        );
    }
    rows
}
