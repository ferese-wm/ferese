use cosmic::iced::border::Shape as BorderShape;
use cosmic::iced::{Alignment, Length, Rectangle, mouse};
use cosmic::widget::{button, column, container, image, mouse_area, row, scrollable, slider};
use cosmic::{Element, theme};
use ferese_ipc::media::Playback;
use ferese_theme::icons;
use serde_json::json;

use super::{Menu, MenuRows, MenuStyle};
use crate::{
    BarMetrics, FereseShell, Message, ShellTheme, accented_icon, bar_content, color,
    color_with_opacity, motion,
};

const SEEK_THUMB_DIAMETER: f32 = 10.0;

fn seek_slider_style(style: MenuStyle) -> cosmic::theme::iced::Slider {
    let paint = std::rc::Rc::new(move |_: &cosmic::Theme| {
        use cosmic::iced::widget::slider::{Breakpoint, Handle, HandleShape, Rail, Style};

        let accent = color_with_opacity(style.theme.accent, style.opacity);
        Style {
            rail: Rail {
                backgrounds: (accent.into(), style.muted.scale_alpha(0.10).into()),
                width: 4.0,
                border: cosmic::iced::Border {
                    shape: BorderShape::Circular,
                    radius: 2.0.into(),
                    ..Default::default()
                },
            },
            handle: Handle {
                corner_shape: BorderShape::Circular,
                shape: HandleShape::Circle {
                    radius: SEEK_THUMB_DIAMETER / 2.0,
                },
                background: accent.into(),
                border_width: 0.0,
                border_color: cosmic::iced::Color::TRANSPARENT,
            },
            breakpoint: Breakpoint {
                color: cosmic::iced::Color::TRANSPARENT,
            },
        }
    });

    cosmic::theme::iced::Slider::Custom {
        active: paint.clone(),
        hovered: paint.clone(),
        dragging: paint,
    }
}

fn seek_pill<'a>(
    content: impl Into<Element<'a, cosmic::Action<Message>>>,
    style: MenuStyle,
) -> Element<'a, cosmic::Action<Message>> {
    container(content)
        .width(Length::Fill)
        .padding([2, 0])
        .class(theme::Container::custom(move |_| container::Style {
            background: Some(color_with_opacity(style.theme.border, style.opacity).into()),
            border: cosmic::iced::Border {
                shape: BorderShape::Circular,
                radius: 12.0.into(),
                ..Default::default()
            },
            ..Default::default()
        }))
        .into()
}

pub(super) fn bar<'a>(
    shell: &'a FereseShell,
    palette: ShellTheme,
    metrics: BarMetrics,
    representation: crate::panel::Representation,
    selected: bool,
    width: Option<f32>,
) -> Element<'a, cosmic::Action<Message>> {
    let Some(player) = &shell.media.snapshot.selected else {
        return shell.render.text("").into();
    };
    let foreground = color(if selected {
        palette.accent
    } else {
        palette.text_primary
    });
    let vertical_padding =
        ((metrics.group_item_height - f32::from(metrics.icon_size)) * 0.5).clamp(0.0, 2.0);
    let height = metrics.group_item_height - 2.0 * vertical_padding;
    let side = height.min(24.0);
    let cover: Element<'_, cosmic::Action<Message>> = match shell
        .media
        .artwork
        .as_ref()
        .and_then(|art| art.handle.as_ref())
    {
        Some(handle) => image(handle.clone())
            .width(side)
            .height(side)
            .content_fit(cosmic::iced::ContentFit::Cover)
            .border_radius(shell.render.radius(6.0))
            .shape(BorderShape::Continuous)
            .into(),
        None => container(accented_icon(
            icons::MEDIA,
            side as u16,
            foreground,
            color(palette.accent),
        ))
        .center_x(side)
        .center_y(side)
        .width(side)
        .into(),
    };
    let label = shell
        .render
        .text(player.label())
        .size(metrics.text_size)
        .wrapping(cosmic::iced::widget::text::Wrapping::None)
        .ellipsize(cosmic::iced::widget::text::Ellipsize::End(
            cosmic::iced::advanced::text::EllipsizeHeightLimit::Lines(1),
        ))
        .width(Length::Shrink)
        .height(height)
        .align_y(cosmic::iced::alignment::Vertical::Center)
        .class(theme::Text::Color(foreground));
    let compact = representation == crate::panel::Representation::Compact;
    let preferred_label_width: f32 = if compact { 112. } else { 320. };
    // Outer padding, the transport button, details padding, and cover spacing
    // share the allocation with the label.
    let chrome = 4.
        + 2.
        + f32::from(metrics.icon_size.max(20))
        + 12.
        + 8.
        + if compact { 0. } else { side + 8. };
    let label_width = width.map_or(preferred_label_width, |width| {
        (width - chrome).max(0.).min(preferred_label_width)
    });
    let label = container(label).max_width(label_width);
    let details = button::custom(bar_content(
        if representation == crate::panel::Representation::Compact {
            row![label].align_y(Alignment::Center)
        } else {
            row![cover, label].spacing(8).align_y(Alignment::Center)
        },
        height,
    ))
    .name(format!("Now playing: {}", player.label()))
    .padding([0, 4])
    .height(height)
    .on_press_with_rectangle(move |offset, bounds| {
        cosmic::Action::App(Message::OpenMenu(
            Menu::Media,
            Rectangle {
                x: (bounds.x - offset.x).round() as i32,
                y: (bounds.y - offset.y).round() as i32,
                width: bounds.width.round() as i32,
                height: bounds.height.round() as i32,
            },
        ))
    });
    let playing = player.status == Playback::Playing;
    let mut toggle = button::custom(
        bar_content(
            accented_icon(
                if playing {
                    icons::MEDIA_PAUSE
                } else {
                    icons::MEDIA_PLAY
                },
                metrics.icon_size,
                foreground,
                color(palette.accent),
            ),
            height,
        )
        .width(20)
        .align_x(cosmic::iced::alignment::Horizontal::Center),
    )
    .name(if playing { "Pause" } else { "Play" })
    .height(height)
    .padding([0, 6]);
    if player.can_toggle() {
        toggle = toggle.on_press(cosmic::Action::App(Message::MediaAction(
            shell.media.action("play-pause"),
        )));
    }

    let bar_style = ferese_theme::controls::surface_appearance(
        color_with_opacity(palette.border, 0.30),
        palette.material_radius.min(16.),
    );

    let controls = container(
        row![
            motion::button(shell.render, details, foreground, false, 1.0),
            motion::button(shell.render, toggle, foreground, false, 1.0)
        ]
        .spacing(2)
        .align_y(Alignment::Center),
    )
    .padding([vertical_padding, 2.0])
    .height(metrics.group_item_height)
    .align_y(cosmic::iced::alignment::Vertical::Center)
    .class(theme::Container::custom(move |_| bar_style));

    let mut area = mouse_area(controls);
    if player.can_raise {
        area = area.on_middle_release(cosmic::Action::App(Message::MediaAction(
            shell.media.action("raise"),
        )));
    }
    if player.can_control && player.volume.is_some() {
        let action = shell.media.action("volume");
        area = area.on_scroll(move |delta| {
            let mut action = action.clone();
            action["delta"] = json!(volume_delta(delta));
            cosmic::Action::App(Message::MediaAction(action))
        });
    }

    area.into()
}

fn volume_delta(delta: mouse::ScrollDelta) -> f64 {
    let delta = match delta {
        mouse::ScrollDelta::Lines { y, .. } => f64::from(y) * 0.05,
        mouse::ScrollDelta::Pixels { y, .. } => f64::from(y) * 0.001,
    };
    if delta.is_finite() {
        delta.clamp(-1.0, 1.0)
    } else {
        0.0
    }
}

fn transport<'a>(
    shell: &'a FereseShell,
    source: &'static [u8],
    label: &'static str,
    action: &str,
    enabled: bool,
    style: MenuStyle,
) -> Element<'a, cosmic::Action<Message>> {
    transport_sized(shell, source, label, action, enabled, style, 44.)
}

fn transport_sized<'a>(
    shell: &'a FereseShell,
    source: &'static [u8],
    label: &'static str,
    action: &str,
    enabled: bool,
    style: MenuStyle,
    size: f32,
) -> Element<'a, cosmic::Action<Message>> {
    let fill =
        (action == "play-pause").then_some(if enabled { style.primary } else { style.muted });
    let foreground = match fill {
        Some(fill) => cosmic::iced::Color {
            a: fill.a,
            ..ferese_theme::foreground(fill, color(style.theme.surface_popover))
        },
        None => {
            if enabled {
                style.primary
            } else {
                style.muted
            }
        }
    };
    let mut control = button::custom(
        container(
            accented_icon(
                source,
                (size - 20.) as u16,
                cosmic::iced::Color {
                    a: 1.,
                    ..foreground
                },
                color(style.theme.accent),
            )
            .opacity(foreground.a),
        )
        .center_x(size - 20.)
        .center_y(size - 20.),
    )
    .padding(10)
    .width(size)
    .height(size)
    .name(label);
    if enabled {
        control = control.on_press(cosmic::Action::App(Message::MediaAction(
            shell.media.action(action),
        )));
    }

    motion::circular_button(shell.render, control, foreground, fill, style.opacity, size)
}

pub(super) fn hub<'a>(
    shell: &'a FereseShell,
    rows: MenuRows<'a>,
    style: MenuStyle,
) -> MenuRows<'a> {
    let Some(player) = &shell.media.snapshot.selected else {
        return rows.push(
            shell
                .render
                .text("No active player")
                .size(14)
                .class(theme::Text::Color(style.muted)),
        );
    };
    let cover: Element<'_, cosmic::Action<Message>> = match shell
        .media
        .artwork
        .as_ref()
        .and_then(|art| art.handle.as_ref())
    {
        Some(handle) => image(handle.clone())
            .width(64)
            .height(64)
            .opacity(style.opacity)
            .content_fit(cosmic::iced::ContentFit::Cover)
            .border_radius(10.)
            .into(),
        None => container(
            accented_icon(
                icons::MEDIA,
                26,
                color(style.theme.text_primary),
                color(style.theme.accent),
            )
            .opacity(style.opacity),
        )
        .center_x(64)
        .center_y(64)
        .class(theme::Container::custom(move |_| {
            ferese_theme::controls::surface_appearance(style.primary.scale_alpha(0.06), 10.)
        }))
        .into(),
    };
    let mut info = column![
        shell
            .render
            .text(player.label())
            .size(14)
            .class(theme::Text::Color(style.primary))
            .width(Length::Fill)
            .ellipsize(cosmic::iced::widget::text::Ellipsize::End(
                cosmic::iced::advanced::text::EllipsizeHeightLimit::Lines(1)
            )),
        shell
            .render
            .text(&player.artist)
            .size(12)
            .class(theme::Text::Color(style.muted))
            .width(Length::Fill)
            .ellipsize(cosmic::iced::widget::text::Ellipsize::End(
                cosmic::iced::advanced::text::EllipsizeHeightLimit::Lines(1)
            ))
    ]
    .spacing(4)
    .width(Length::Fill);
    if let (Some(length), Some(position)) = (
        player.length_us.filter(|length| *length > 0),
        player.position_at(shell.media.now_us),
    ) {
        let value = (shell.media.seeking.unwrap_or(position).min(length) as f64 / length as f64
            * 10_000.)
            .round() as u32;
        if player.can_control && player.can_seek && player.track_id.is_some() {
            info = info.push(
                slider(0..=10_000_u32, value, move |value| {
                    cosmic::Action::App(Message::MediaSeek(seek_position(length, value)))
                })
                .on_release(cosmic::Action::App(Message::MediaSeekCommit))
                .height(16)
                .handle_width(SEEK_THUMB_DIAMETER)
                .class(seek_slider_style(style))
                .width(Length::Fill),
            );
        }
    }
    let rows = rows.push(row![cover, info].spacing(12).align_y(Alignment::Center));
    let playing = player.status == Playback::Playing;
    rows.push(
        container(
            row![
                transport_sized(
                    shell,
                    icons::MEDIA_PREVIOUS,
                    "Previous track",
                    "previous",
                    player.can_control && player.can_previous,
                    style,
                    36.
                ),
                transport_sized(
                    shell,
                    if playing {
                        icons::MEDIA_PAUSE
                    } else {
                        icons::MEDIA_PLAY
                    },
                    if playing { "Pause" } else { "Play" },
                    "play-pause",
                    player.can_toggle(),
                    style,
                    36.
                ),
                transport_sized(
                    shell,
                    icons::MEDIA_NEXT,
                    "Next track",
                    "next",
                    player.can_control && player.can_next,
                    style,
                    36.
                ),
            ]
            .spacing(8),
        )
        .center_x(Length::Fill),
    )
}

pub(super) fn view<'a>(
    shell: &'a FereseShell,
    mut rows: MenuRows<'a>,
    style: MenuStyle,
) -> MenuRows<'a> {
    if let Some(player) = &shell.media.snapshot.selected {
        let cover: Element<'_, cosmic::Action<Message>> = match shell
            .media
            .artwork
            .as_ref()
            .and_then(|art| art.handle.as_ref())
        {
            Some(handle) => image(handle.clone())
                .width(64)
                .height(64)
                .content_fit(cosmic::iced::ContentFit::Contain)
                .border_radius(shell.render.radius(12.0))
                .shape(BorderShape::Continuous)
                .into(),
            None => container(accented_icon(
                icons::MEDIA,
                30,
                style.muted,
                color(style.theme.accent),
            ))
            .center_x(64)
            .center_y(64)
            .into(),
        };
        let text_size = if player.label().len() >= 24 { 13 } else { 15 };
        let mut info = column![
            shell
                .render
                .text(player.label())
                .size(text_size)
                .width(Length::Fill)
                .ellipsize(cosmic::iced::widget::text::Ellipsize::End(
                    cosmic::iced::advanced::text::EllipsizeHeightLimit::Lines(2)
                ))
        ]
        .spacing(4)
        .width(Length::Fill);
        if !player.artist.is_empty() {
            info = info.push(
                shell
                    .render
                    .text(&player.artist)
                    .size(12)
                    .width(Length::Fill)
                    .ellipsize(cosmic::iced::widget::text::Ellipsize::End(
                        cosmic::iced::advanced::text::EllipsizeHeightLimit::Lines(1),
                    ))
                    .class(theme::Text::Color(style.muted)),
            );
        }
        if !player.album.is_empty() {
            info = info.push(
                shell
                    .render
                    .text(&player.album)
                    .size(11)
                    .width(Length::Fill)
                    .ellipsize(cosmic::iced::widget::text::Ellipsize::End(
                        cosmic::iced::advanced::text::EllipsizeHeightLimit::Lines(1),
                    ))
                    .class(theme::Text::Color(style.muted)),
            );
        }

        rows = rows.push(row![cover, info].spacing(12).align_y(Alignment::Center));
        let mut playback = column::with_capacity(2).spacing(6).width(Length::Fill);
        if let (Some(length), Some(position)) = (
            player.length_us.filter(|length| *length > 0),
            player.position_at(shell.media.now_us),
        ) {
            let position = shell.media.seeking.unwrap_or(position).min(length);
            let mut progress = column::with_capacity(2).spacing(2);
            if player.can_control && player.can_seek && player.track_id.is_some() {
                let value = (position as f64 / length as f64 * 10_000.0).round() as u32;
                progress = progress.push(seek_pill(
                    container(
                        slider(0..=10_000_u32, value, move |value| {
                            cosmic::Action::App(Message::MediaSeek(seek_position(length, value)))
                        })
                        .on_release(cosmic::Action::App(Message::MediaSeekCommit))
                        .handle_width(SEEK_THUMB_DIAMETER)
                        .height(20)
                        .class(seek_slider_style(style))
                        .width(Length::Fill),
                    )
                    // Extend the native slider's layout by its reserved thumb
                    // radius so its rail and pointer mapping reach both edges.
                    .padding(cosmic::iced::Padding {
                        left: -SEEK_THUMB_DIAMETER / 2.0,
                        right: -SEEK_THUMB_DIAMETER / 2.0,
                        ..Default::default()
                    }),
                    style,
                ));
            } else {
                progress = progress.push(seek_pill(
                    container(
                        cosmic::iced::widget::ProgressBar::new(
                            0.0..=1.0,
                            (position as f64 / length as f64) as f32,
                        )
                        .girth(4)
                        .length(Length::Fill)
                        .class(cosmic::theme::iced::ProgressBar::custom(move |_| {
                            cosmic::iced::widget::progress_bar::Style {
                                background: style.muted.scale_alpha(0.35).into(),
                                bar: color_with_opacity(style.theme.accent, style.opacity).into(),
                                border: cosmic::iced::Border {
                                    shape: BorderShape::Circular,
                                    radius: 2.0.into(),
                                    ..Default::default()
                                },
                            }
                        })),
                    )
                    .center_y(20),
                    style,
                ));
            }

            progress = progress.push(
                row![
                    shell
                        .render
                        .text(time_label(position))
                        .font(cosmic::iced::Font::MONOSPACE)
                        .size(11)
                        .class(theme::Text::Color(style.muted))
                        .width(Length::Fill),
                    shell
                        .render
                        .text(time_label(length))
                        .font(cosmic::iced::Font::MONOSPACE)
                        .size(11)
                        .class(theme::Text::Color(style.muted))
                ]
                .spacing(12),
            );
            playback = playback.push(progress);
        }

        let playing = player.status == Playback::Playing;
        playback = playback.push(
            container(
                row![
                    transport(
                        shell,
                        icons::MEDIA_PREVIOUS,
                        "Previous track",
                        "previous",
                        player.can_control && player.can_previous,
                        style
                    ),
                    transport(
                        shell,
                        if playing {
                            icons::MEDIA_PAUSE
                        } else {
                            icons::MEDIA_PLAY
                        },
                        if playing { "Pause" } else { "Play" },
                        "play-pause",
                        player.can_toggle(),
                        style
                    ),
                    transport(
                        shell,
                        icons::MEDIA_NEXT,
                        "Next track",
                        "next",
                        player.can_control && player.can_next,
                        style
                    ),
                ]
                .spacing(12),
            )
            .center_x(Length::Fill),
        );
        rows = rows.push(playback);

        let label = if shell.media.snapshot.pinned.is_some() {
            format!("{} · pinned", player.identity)
        } else {
            player.identity.clone()
        };
        rows = rows.push(motion::button(
            shell.render,
            button::custom(shell.render.text(label).size(12))
                .padding([6, 8])
                .on_press(cosmic::Action::App(Message::MediaChoose)),
            style.primary,
            false,
            style.opacity,
        ));
    } else {
        rows = rows.push(
            shell
                .render
                .text("No active player")
                .size(13)
                .class(theme::Text::Color(style.muted)),
        );
    }

    if shell.media.choosing || shell.media.snapshot.selected.is_none() {
        let mut choices = column::with_capacity(shell.media.snapshot.players.len() + 1).spacing(4);
        choices = choices.push(super::controls::menu_button(
            shell.render,
            "Choose automatically",
            Message::MediaAction(json!({"action":"auto"})),
            style.primary,
            style.opacity,
        ));
        for player in &shell.media.snapshot.players {
            let args = json!({"player":player.name,"owner":player.owner});
            let mut pin = args.clone();
            pin["action"] = json!("pin");
            let mut ignore = args;
            ignore["action"] = json!("ignore");
            ignore["ignored"] = json!(!player.ignored);
            let label = if player.identity.is_empty() {
                &player.name
            } else {
                &player.identity
            };
            choices = choices.push(
                row![
                    super::controls::menu_button(
                        shell.render,
                        label,
                        Message::MediaAction(pin),
                        if player.ignored {
                            style.muted
                        } else {
                            style.primary
                        },
                        style.opacity
                    ),
                    super::controls::menu_button(
                        shell.render,
                        if player.ignored { "Show" } else { "Ignore" },
                        Message::MediaAction(ignore),
                        style.muted,
                        style.opacity
                    ),
                ]
                .spacing(6),
            );
        }

        rows = rows.push(scrollable(choices).height(Length::Fixed(140.0)));
    }

    if let Some(error) = &shell.media.error {
        rows = rows.push(
            shell
                .render
                .text(error)
                .size(12)
                .class(theme::Text::Color(style.muted)),
        );
    }

    rows
}

fn seek_position(length: u64, value: u32) -> u64 {
    (u128::from(length) * u128::from(value.min(10_000)) / 10_000) as u64
}

fn time_label(us: u64) -> String {
    let seconds = us / 1_000_000;
    format!("{}:{:02}", seconds / 60, seconds % 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seeking_handles_short_tracks_and_large_durations() {
        assert_eq!(seek_position(500_000, 5000), 250_000);
        assert_eq!(seek_position(500_000, 10_000), 500_000);
        assert_eq!(seek_position(u64::MAX, 10_000), u64::MAX);
        assert_eq!(seek_position(1, 100_000), 1);
    }

    #[test]
    fn wheel_changes_are_bounded_and_pixel_scrolling_is_not_a_full_step() {
        assert_eq!(
            volume_delta(mouse::ScrollDelta::Lines { x: 0.0, y: 1.0 }),
            0.05
        );
        assert_eq!(
            volume_delta(mouse::ScrollDelta::Pixels { x: 0.0, y: 1.0 }),
            0.001
        );
        assert_eq!(
            volume_delta(mouse::ScrollDelta::Lines {
                x: 0.0,
                y: f32::NAN
            }),
            0.0
        );
        assert_eq!(
            volume_delta(mouse::ScrollDelta::Lines { x: 0.0, y: 1000.0 }),
            1.0
        );
    }
}
