use cosmic::iced::{Alignment, Color, Length, alignment};
use cosmic::widget::{button, column, container, row};
use cosmic::{Element, theme};

use super::bar::status_icon;
use super::controls::control_card;
use super::{Menu, MenuRows, MenuStyle, audio, battery, notification_controls};
use crate::status::{self, Action};
use crate::{FereseShell, Message, ShellTheme, accented_icon, color, color_with_opacity, motion, shell_font, text};

pub(super) fn view<'a>(
    shell: &'a FereseShell,
    mut rows: MenuRows<'a>,
    style: MenuStyle,
    show_settings_link: bool,
) -> MenuRows<'a> {
    let MenuStyle {
        theme,
        primary,
        muted,
        opacity: p,
    } = style;
    use ferese_config::theme::{Appearance, Mode};
    let mut modes = row([]).spacing(6);
    for (mode, label) in [(Mode::Light, "☀ Light"), (Mode::Dark, "☾ Dark"), (Mode::Auto, "◐ Auto")] {
        modes = modes.push(motion::button(
            button::custom(text(label).font(shell_font()).size(13))
                .width(Length::Fill)
                .padding(9)
                .on_press(cosmic::Action::App(Message::ThemeMode(mode))),
            primary,
            shell.config.theme_mode == mode,
            p,
        ));
    }
    if show_settings_link
        && shell
            .config
            .status
            .settings_command
            .as_ref()
            .is_some_and(|command| !command.is_empty())
    {
        let settings = motion::button(
            button::custom(
                ferese_theme::icons::tinted(ferese_theme::icons::SETTINGS, 18, color(theme.text_primary)).opacity(p),
            )
            .name("Open Ferese Settings")
            .width(30)
            .height(28)
            .padding([5, 3])
            .on_press(cosmic::Action::App(Message::Control(Action::Settings))),
            primary,
            false,
            p,
        );
        modes = modes.push(cosmic::widget::tooltip(
            settings,
            text("Open Ferese Settings").size(12),
            cosmic::widget::tooltip::Position::Left,
        ));
    }
    rows = rows.push(modes.align_y(Alignment::Center));
    if shell.config.theme_mode == Mode::Auto {
        rows = rows.push(
            text(if theme.appearance == Appearance::Light {
                "Auto · currently light"
            } else {
                "Auto · currently dark"
            })
            .font(shell_font())
            .size(12)
            .class(cosmic::theme::Text::Color(muted)),
        );
    }
    let mut connections = row::with_capacity(3).spacing(8).width(Length::Fill);

    if let Some(n) = &shell.status.network {
        connections = connections.push(connection_control(
            "Wi-Fi",
            if n.enabled {
                n.connection.as_deref().unwrap_or("Not connected")
            } else {
                "Off"
            },
            status_icon(Menu::Network, &shell.status).0,
            n.enabled,
            Some(Action::Wifi(!n.enabled)),
            theme,
            p,
        ));
    }

    if let Some(b) = &shell.status.bluetooth {
        connections = connections.push(connection_control(
            "Bluetooth",
            if b.enabled {
                b.devices.first().map(String::as_str).unwrap_or("On")
            } else {
                "Off"
            },
            status_icon(Menu::Bluetooth, &shell.status).0,
            b.enabled,
            Some(Action::Bluetooth(!b.enabled)),
            theme,
            p,
        ));
    }

    if let Some(b) = &shell.status.battery {
        connections = connections.push(connection_control(
            "Battery",
            &format!("{}%", b.percent),
            status_icon(Menu::Battery, &shell.status).0,
            b.status == "Charging",
            None,
            theme,
            p,
        ));
    }

    if shell.status.network.is_some() || shell.status.bluetooth.is_some() || shell.status.battery.is_some() {
        rows = rows.push(control_card(connections.into(), primary, p));
    }
    rows = audio::view(shell, rows, style, true);
    rows = battery::brightness(shell, rows, style);
    rows = notification_controls(shell, rows, style, true);
    rows
}

fn connection_caption(detail: &str) -> String {
    const MAX_CHARS: usize = 12;
    let normalized = detail.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut chars = normalized.chars();
    let mut caption: String = chars.by_ref().take(MAX_CHARS).collect();
    if chars.next().is_some() {
        caption.pop();
        caption.push('…');
    }
    caption
}

fn connection_control<'a>(
    label: &'static str,
    detail: &str,
    source: &'static [u8],
    enabled: bool,
    action: Option<Action>,
    palette: ShellTheme,
    opacity: f32,
) -> Element<'a, cosmic::Action<Message>> {
    let selected = enabled && action.is_some();
    let (fill, on_accent) = ferese_theme::accent_pair(
        ferese_theme::composite(color(palette.accent), color(palette.surface_base)),
        color(palette.text_primary),
    );
    let foreground = if selected {
        Color {
            a: opacity,
            ..on_accent
        }
    } else {
        color_with_opacity(palette.text_primary, opacity)
    };
    let icon_color = foreground;
    let muted = if selected {
        foreground
    } else {
        color_with_opacity(palette.text_muted, opacity)
    };
    let icon = container(accented_icon(source, 28, icon_color, icon_color))
        .width(Length::Fill)
        .height(28)
        .align_x(alignment::Horizontal::Center)
        .align_y(alignment::Vertical::Center);
    let content = column![icon].spacing(2).width(Length::Fill).align_x(Alignment::Center);
    let content = content.push(
        text(connection_caption(detail))
            .size(11)
            .width(Length::Fill)
            .height(16)
            .align_x(alignment::Horizontal::Center)
            .wrapping(cosmic::iced::widget::text::Wrapping::None)
            .ellipsize(cosmic::iced::widget::text::Ellipsize::End(
                cosmic::iced::advanced::text::EllipsizeHeightLimit::Lines(1),
            ))
            .class(theme::Text::Color(muted)),
    );
    let Some(action) = action else {
        return container(content).width(Length::FillPortion(1)).padding(4).into();
    };
    let button = button::custom(content)
        .width(Length::Fill)
        .padding(4)
        .name(format!("{label}: {}, {detail}", if enabled { "on" } else { "off" }))
        .on_press(cosmic::Action::App(Message::Control(action)));
    let tile: Element<'_, cosmic::Action<Message>> = if selected {
        button
            .class(ferese_theme::controls::filled_button(
                fill,
                foreground,
                motion::radius(14.),
                opacity,
            ))
            .into()
    } else {
        motion::button(button, foreground, false, opacity)
    };
    container(tile).width(Length::FillPortion(1)).into()
}

pub(super) fn heading<'a>(
    shell: &'a FereseShell,
    mut heading: super::MenuHeading<'a>,
    style: MenuStyle,
) -> super::MenuHeading<'a> {
    let MenuStyle {
        theme,
        primary,
        opacity: p,
        ..
    } = style;
    if shell
        .config
        .status
        .settings_command
        .as_ref()
        .and_then(|command| command.first())
        .is_some_and(|program| status::available(program))
    {
        heading = heading.push(motion::button(
            button::custom(accented_icon(
                ferese_theme::icons::SETTINGS,
                20,
                primary,
                color_with_opacity(theme.accent, p),
            ))
            .padding(6)
            .name("Open Settings")
            .on_press(cosmic::Action::App(Message::Control(Action::Settings))),
            primary,
            false,
            p,
        ));
    }
    heading
}

#[cfg(test)]
mod tests {
    #[test]
    fn connection_captions_fit_one_short_line() {
        assert_eq!(super::connection_caption("88%"), "88%");
        assert_eq!(super::connection_caption("Flow84@Lofree"), "Flow84@Lofr…");
        assert_eq!(super::connection_caption("123456789012"), "123456789012");
        assert_eq!(super::connection_caption("My very long headphones"), "My very lon…");
        assert_eq!(super::connection_caption("  Device\n name  "), "Device name");
        let caption = super::connection_caption("耳機耳機耳機耳機耳機耳機耳機");
        assert_eq!(caption.chars().count(), 12);
        assert!(caption.ends_with('…'));
    }
}
