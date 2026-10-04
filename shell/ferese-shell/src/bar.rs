use super::{
    Background, BarMetrics, Border, Color, Element, FereseShell, Length, Message, ShellSnapshot, ShellTheme, alignment,
    bar_icon, button, color, color_with_opacity, container, control, motion, row, status_ui, text, theme, window,
};
use cosmic::iced::border::Shape as BorderShape;

impl FereseShell {
    pub(super) fn output_for_bar(&self, id: window::Id) -> Option<&control::OutputSnapshot> {
        let name = self.outputs.iter().find(|entry| entry.bar == id)?.name.as_deref()?;
        self.snapshot.outputs.iter().find(|output| output.name == name)
    }

    pub(super) fn bar_hidden(&self, id: window::Id) -> bool {
        output_bar_hidden(&self.snapshot, self.output_for_bar(id).map(|output| output.id))
    }

    pub(super) fn view_layer(&self, id: window::Id) -> Element<'_, cosmic::Action<Message>> {
        if self.bar_hidden(id) {
            return container(text("")).width(Length::Fill).height(Length::Fill).into();
        }

        let focused_output = self.output_for_bar(id);
        let shell_theme = self.config.theme.for_bar();
        let bar = BarMetrics::from(shell_theme);
        let mut workspace_row = row::with_capacity(self.snapshot.workspaces.len() + 1)
            .spacing(3)
            .align_y(cosmic::iced::Alignment::Center);

        workspace_row = workspace_row.push(motion::button(
            button::custom(overview_control(bar, color(shell_theme.accent)))
                .height(bar.control_height)
                .padding([0, 7])
                .on_press(cosmic::Action::App(Message::ToggleOverview)),
            color(shell_theme.text_primary),
            self.overview_active,
            1.0,
        ));
        let mut workspace_buttons = row::with_capacity(self.snapshot.workspaces.len())
            .spacing(1)
            .align_y(cosmic::iced::Alignment::Center);

        for workspace in self
            .snapshot
            .workspaces_for_output(focused_output.map(|output| output.id))
        {
            let active = workspace_active_on_bar(workspace.id, focused_output);
            let owner = workspace
                .output
                .and_then(|id| self.snapshot.outputs.iter().find(|output| output.id == id));
            let occupied = workspace.window_count > 0;
            let indicator = workspace_indicator(
                &workspace.name,
                active,
                workspace.visible && !active,
                occupied,
                bar,
                shell_theme,
            );
            let workspace_button = button::custom(indicator)
                .name(format!(
                    "Workspace {}{}{}",
                    workspace.index,
                    if workspace.focused {
                        ", focused"
                    } else if active {
                        ", visible"
                    } else {
                        ""
                    },
                    owner.map_or(String::new(), |output| format!(", on {}", output.name))
                ))
                .height(bar.control_height)
                .padding(0)
                .on_press(cosmic::Action::App(Message::ActivateWorkspace(workspace.id)));

            let selector = motion::button(
                workspace_button,
                color(if active {
                    shell_theme.accent
                } else {
                    shell_theme.text_primary
                }),
                false,
                1.0,
            );
            // Iced tooltips are overlays inside this bar-height layer surface;
            // viewport clamping puts them back over the selector. Keep the
            // accessible button name, without an overlay stealing its target.
            workspace_buttons = workspace_buttons.push(selector);
        }
        workspace_row = workspace_row.push(
            container(workspace_buttons)
                .padding([0, 3])
                .class(theme::Container::custom(move |_| bar_group_style(shell_theme))),
        );

        let foreground = color(shell_theme.text_primary);
        let left = cosmic::iced::widget::scrollable(workspace_row)
            .direction(cosmic::iced::widget::scrollable::Direction::Horizontal(
                cosmic::iced::widget::scrollable::Scrollbar::default()
                    .width(0)
                    .scroller_width(0),
            ))
            .width(Length::Fill)
            .height(bar.control_height);
        let (date, time) = self.clock.split_once(", ").unwrap_or(("", &self.clock));
        let clock = container(motion::button(
            button::custom(
                row![
                    text(date).size(12).class(theme::Text::Color(foreground)),
                    text(time).size(bar.text_size).class(theme::Text::Color(foreground)),
                ]
                .spacing(8)
                .align_y(cosmic::iced::Alignment::Center),
            )
            .padding([4, 8])
            .height(bar.control_height)
            .name("Open calendar")
            .on_press_with_rectangle(move |offset, bounds| {
                cosmic::Action::App(Message::OpenMenuOn(
                    id,
                    status_ui::Menu::Calendar,
                    cosmic::iced::Rectangle {
                        x: (bounds.x - offset.x).round() as i32,
                        y: (bounds.y - offset.y).round() as i32,
                        width: bounds.width.round() as i32,
                        height: bounds.height.round() as i32,
                    },
                ))
            }),
            foreground,
            self.menu
                .as_ref()
                .is_some_and(|menu| menu.kind == status_ui::Menu::Calendar),
            1.0,
        ))
        .height(bar.control_height)
        .align_y(alignment::Vertical::Center)
        .class(theme::Container::custom(move |_| bar_group_style(shell_theme)));
        let mut right = row![
            self.view_status_bar().map(move |action| match action {
                cosmic::Action::App(Message::OpenMenu(kind, anchor)) =>
                    cosmic::Action::App(Message::OpenMenuOn(id, kind, anchor)),
                other => other,
            }),
            clock
        ]
        .spacing(8)
        .align_y(cosmic::iced::Alignment::Center);
        if self.display_mode.external_connected() {
            right = right.push(motion::button(
                button::custom(bar_icon(ferese_theme::icons::DISPLAY, bar.icon_size, foreground))
                    .name("Display mode")
                    .height(bar.control_height)
                    .padding([0, 7])
                    .on_press(cosmic::Action::App(Message::OpenDisplays(
                        self.outputs
                            .iter()
                            .find(|output| output.bar == id)
                            .and_then(|output| output.name.clone()),
                    ))),
                foreground,
                self.display_mode.open,
                1.0,
            ));
        }
        let available = self
            .outputs
            .iter()
            .find(|output| output.bar == id)
            .and_then(|output| output.size)
            .map_or(0., |(width, _)| width as f32)
            - 2. * (shell_theme.panel_padding + shell_theme.bar_margin_horizontal as f32);
        let title_width = (available - 920.).clamp(0., 360.);
        let title = if self.config.status.window_title && title_width > 0. {
            focused_bar_title(&self.snapshot, focused_output)
        } else {
            ""
        };
        let center = text(title)
            .size(bar.text_size)
            .width(title_width)
            .height(bar.control_height)
            .align_x(alignment::Horizontal::Center)
            .align_y(alignment::Vertical::Center)
            .wrapping(cosmic::iced::widget::text::Wrapping::None)
            .ellipsize(cosmic::iced::widget::text::Ellipsize::End(
                cosmic::iced::advanced::text::EllipsizeHeightLimit::Lines(1),
            ))
            .class(theme::Text::Color(foreground));
        let right = cosmic::iced::widget::scrollable(container(right).width(Length::Shrink))
            .direction(cosmic::iced::widget::scrollable::Direction::Horizontal(
                cosmic::iced::widget::scrollable::Scrollbar::default()
                    .width(0)
                    .scroller_width(0),
            ))
            .anchor_right()
            .width(Length::Shrink)
            .height(bar.control_height);
        let content = row![
            container(left).width(Length::Fill),
            center,
            container(right)
                .width(Length::Fill)
                .align_x(alignment::Horizontal::Right),
        ]
        .spacing(8)
        .align_y(cosmic::iced::Alignment::Center)
        .height(Length::Fill);

        let compositor_material = self
            .outputs
            .iter()
            .any(|entry| entry.bar == id && entry.effects.is_some());
        container(content)
            .width(Length::Fill)
            .height(Length::Fill)
            .padding([0, shell_theme.panel_padding.round() as u16])
            .class(theme::Container::custom(move |_| {
                bar_style(shell_theme, compositor_material)
            }))
            .into()
    }
}

pub(super) fn overview_control(bar: BarMetrics, foreground: Color) -> Element<'static, cosmic::Action<Message>> {
    container(bar_icon(
        ferese_theme::icons::FERESE,
        bar.overview_icon_size,
        foreground,
    ))
    .width(bar.overview_icon_size)
    .height(bar.control_height)
    .align_x(alignment::Horizontal::Center)
    .align_y(alignment::Vertical::Center)
    .into()
}

#[cfg(test)]
pub(super) fn bar_hidden(snapshot: &ShellSnapshot) -> bool {
    let output = snapshot
        .outputs
        .iter()
        .find(|output| output.focused)
        .or_else(|| snapshot.outputs.first())
        .map(|output| output.id);
    output_bar_hidden(snapshot, output)
}

pub(super) fn output_bar_hidden(snapshot: &ShellSnapshot, output: Option<u64>) -> bool {
    let active_workspace = snapshot
        .outputs
        .iter()
        .find(|candidate| Some(candidate.id) == output)
        .map(|output| output.active_workspace);

    active_workspace.is_some_and(|workspace| {
        snapshot
            .windows
            .iter()
            .any(|window| window.workspace == workspace && window.fullscreen)
    })
}

pub(super) fn workspace_indicator(
    name: &str,
    active: bool,
    active_elsewhere: bool,
    occupied: bool,
    bar: BarMetrics,
    shell_theme: ShellTheme,
) -> Element<'static, cosmic::Action<Message>> {
    let foreground = if active {
        shell_theme.accent
    } else if occupied {
        shell_theme.text_primary
    } else {
        shell_theme.text_muted
    };
    container(
        text(name.to_owned())
            .size(12)
            .class(theme::Text::Color(color(foreground))),
    )
    .width(24)
    .height(bar.control_height)
    .align_x(alignment::Horizontal::Center)
    .align_y(alignment::Vertical::Center)
    .class(theme::Container::custom(move |_| {
        workspace_selector_style(active, active_elsewhere, occupied, shell_theme)
    }))
    .into()
}

pub(super) fn focused_bar_title<'a>(snapshot: &'a ShellSnapshot, output: Option<&control::OutputSnapshot>) -> &'a str {
    let Some(output) = output else {
        return "";
    };
    snapshot
        .windows
        .iter()
        .find(|window| window.focused && window.workspace == output.active_workspace)
        .map_or("", |window| {
            if window.title.trim().is_empty() {
                &window.app_id
            } else {
                &window.title
            }
        })
}

pub(super) fn workspace_active_on_bar(workspace: u64, output: Option<&control::OutputSnapshot>) -> bool {
    output.is_some_and(|output| output.active_workspace == workspace)
}

pub(super) fn workspace_selector_style(
    active: bool,
    active_elsewhere: bool,
    occupied: bool,
    shell_theme: ShellTheme,
) -> container::Style {
    container::Style {
        background: if active {
            Some(Background::Color(color_with_opacity(shell_theme.accent, 0.16)))
        } else if occupied {
            Some(Background::Color(color_with_opacity(shell_theme.border, 0.3)))
        } else {
            None
        },
        border: Border {
            shape: BorderShape::Continuous,
            color: color_with_opacity(shell_theme.accent, 0.55),
            width: if active_elsewhere { 1.0 } else { 0.0 },
            radius: shell_theme.material_radius.min(14.0).into(),
            ..Default::default()
        },
        ..Default::default()
    }
}

pub(super) fn bar_style(theme: ShellTheme, compositor_material: bool) -> container::Style {
    let mut style = ferese_theme::controls::surface_appearance(color(theme.bar_background), theme.bar_radius);
    if compositor_material {
        style.background = None;
    }
    style.text_color = Some(color(theme.text_primary));
    style.icon_color = style.text_color;
    style.snap = true;
    style
}

pub(super) fn bar_group_style(theme: ShellTheme) -> container::Style {
    container::Style {
        background: Some(Background::Color(color_with_opacity(theme.border, 0.25))),
        border: Border {
            shape: BorderShape::Continuous,
            color: color(theme.border),
            width: 1.0,
            radius: theme.material_radius.min(16.0).into(),
            ..Default::default()
        },
        ..Default::default()
    }
}
