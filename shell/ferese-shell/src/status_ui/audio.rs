use cosmic::iced::{Alignment, Length};
use cosmic::widget::{column, row};
use cosmic::{Element, theme};

use super::bar::audio_icon;
use super::controls::{control_card, shell_switch, slider_row};
use super::{MenuRows, MenuStyle};
use crate::status::Action;
use crate::{FereseShell, Message, color_with_opacity};

pub(super) fn view<'a>(
    shell: &'a FereseShell,
    mut rows: MenuRows<'a>,
    style: MenuStyle,
    combined: bool,
) -> MenuRows<'a> {
    let MenuStyle {
        theme,
        primary,
        muted,
        opacity: p,
    } = style;
    if let Some(a) = &shell.status.audio {
        let mut volume_heading =
            row![shell.render.text("Volume").size(13).width(Length::Fill)].align_y(Alignment::Center);
        if combined {
            volume_heading = volume_heading.push(shell_switch(
                shell.render,
                "Mute",
                a.muted,
                Action::Mute(!a.muted),
                theme,
                p,
            ));
        }
        let audio = column![
            volume_heading,
            shell.render.text(&a.output).size(12).class(theme::Text::Color(muted)),
            slider_row(
                shell.render,
                audio_icon(a.volume, a.muted),
                a.volume,
                false,
                primary,
                color_with_opacity(theme.accent, p),
                p,
            )
        ]
        .spacing(4);
        let audio: Element<'_, cosmic::Action<Message>> = if combined {
            control_card(shell.render, audio.into(), primary, p)
        } else {
            audio.into()
        };
        rows = rows.push(audio);
    }
    rows
}

pub(super) fn heading<'a>(
    shell: &'a FereseShell,
    mut heading: super::MenuHeading<'a>,
    style: MenuStyle,
) -> super::MenuHeading<'a> {
    let MenuStyle { theme, opacity: p, .. } = style;
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
