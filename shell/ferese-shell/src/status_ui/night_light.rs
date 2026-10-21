use cosmic::widget::{column, slider};

use super::*;

pub(super) fn view<'a>(
    shell: &'a FereseShell,
    mut rows: MenuRows<'a>,
    style: MenuStyle,
) -> MenuRows<'a> {
    let Some(status) = &shell.status.night_light else {
        return rows.push(shell.render.text("Night light is unavailable").size(13));
    };
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
        shell
            .render
            .text(format!("Color temperature · {temperature} K"))
            .size(13),
        slider(1000..=6500, temperature, |value| cosmic::Action::App(
            Message::NightLightTemperature(value)
        ))
        .step(100u16)
        .on_release(cosmic::Action::App(Message::CommitNightLightTemperature)),
        shell.render.text("Lower temperatures are warmer").size(12),
    ]
    .spacing(8);
    rows = rows.push(control_card(
        shell.render,
        warmth.into(),
        style.primary,
        style.opacity,
    ));
    if status.supported_outputs == 0 {
        rows = rows.push(
            shell
                .render
                .text("No display supports night light")
                .size(13),
        );
    } else if status.enabled && status.failed_outputs > 0 {
        rows = rows.push(
            shell
                .render
                .text("Night light could not be applied to some displays")
                .size(12),
        );
    }
    rows
}
