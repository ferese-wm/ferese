use cosmic::theme;
use cosmic::widget::button;

use super::bar::audio_icon;
use super::controls::{level_control, shell_switch};
use super::{MenuRows, MenuStyle};
use crate::status::Action;
use crate::{FereseShell, Message, bar_icon, motion};

pub(super) fn view<'a>(
    shell: &'a FereseShell,
    mut rows: MenuRows<'a>,
    style: MenuStyle,
    combined: bool,
) -> MenuRows<'a> {
    let MenuStyle {
        primary,
        muted,
        opacity: p,
        ..
    } = style;
    if let Some(a) = &shell.status.audio {
        if !combined {
            rows = rows.push(
                shell
                    .render
                    .text(&a.output)
                    .size(12)
                    .class(theme::Text::Color(muted)),
            );
        }
        let icon = motion::button(
            shell.render,
            button::custom(bar_icon(audio_icon(a.volume, a.muted), 16, primary))
                .name(if a.muted {
                    "Unmute sound"
                } else {
                    "Mute sound"
                })
                .padding(2)
                .on_press(cosmic::Action::App(Message::Control(Action::Mute(
                    !a.muted,
                )))),
            primary,
            false,
            p,
        );
        rows = rows.push(level_control(
            shell.render,
            if a.muted { "Sound · Muted" } else { "Sound" },
            icon,
            a.volume,
            0..=100,
            Action::Volume,
            style,
        ));
    }
    rows
}

pub(super) fn heading<'a>(
    shell: &'a FereseShell,
    mut heading: super::MenuHeading<'a>,
    style: MenuStyle,
) -> super::MenuHeading<'a> {
    let MenuStyle {
        theme, opacity: p, ..
    } = style;
    if let Some(audio) = &shell.status.audio {
        heading = heading.push(shell_switch(
            shell.render,
            "Mute",
            audio.muted,
            Action::Mute(!audio.muted),
            theme,
            p,
        ));
    }
    heading
}
