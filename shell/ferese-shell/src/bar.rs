use super::{
    Background, BarMetrics, Border, Color, Element, FereseShell, Length, Message, ShellSnapshot, ShellTheme, alignment,
    bar_icon, button, color, color_with_opacity, container, control, motion, row, status_ui, text, theme, window,
};
use cosmic::iced::border::Shape as BorderShape;
use ferese_config::BarLayout;

pub(super) mod presentation;

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
        let mode = self.config.status.bar_layout;
        let islands = mode == BarLayout::Islands;
        let output = self.outputs.iter().find(|output| output.bar == id);
        let effects = output.and_then(|output| output.effects.as_ref());
        let compositor_material = effects.is_some();
        let control_height = if islands {
            bar.group_item_height
        } else {
            bar.control_height
        };
        let control_metrics = BarMetrics { control_height, ..bar };
        let mut workspace_row = row::with_capacity(self.snapshot.workspaces.len() + 1)
            .spacing(if islands { 8 } else { 3 })
            .align_y(cosmic::iced::Alignment::Center);

        let overview = motion::button(
            button::custom(overview_control(control_metrics, color(shell_theme.accent)))
                .height(control_height)
                .padding([0, 7])
                .on_press(cosmic::Action::App(Message::ToggleOverview)),
            color(shell_theme.text_primary),
            self.overview_active,
            1.0,
        );
        let mut workspace_buttons = row::with_capacity(self.snapshot.workspaces.len() + usize::from(islands))
            .spacing(1)
            .align_y(cosmic::iced::Alignment::Center);
        if islands {
            workspace_buttons = workspace_buttons.push(overview);
        } else {
            workspace_row = workspace_row.push(overview);
        }

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
                .height(bar.group_item_height)
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
        workspace_row = workspace_row.push(island(
            container(workspace_buttons)
                .padding([2, 3])
                .height(bar.group_height)
                .class(theme::Container::custom(move |_| bar_group_style(shell_theme)))
                .into(),
            shell_theme,
            self.config.status.bar_island_padding,
            islands,
            compositor_material,
        ));

        let foreground = color(shell_theme.text_primary);
        let left = cosmic::iced::widget::scrollable(workspace_row)
            .direction(cosmic::iced::widget::scrollable::Direction::Horizontal(
                cosmic::iced::widget::scrollable::Scrollbar::default()
                    .width(0)
                    .scroller_width(0),
            ))
            .width(Length::Fill)
            .height(if islands { bar.height } else { bar.group_height });
        let (date, time) = self.clock.split_once(", ").unwrap_or(("", &self.clock));
        let clock = motion::button(
            button::custom(
                container(
                    row![
                        text(date).size(12).class(theme::Text::Color(foreground)),
                        text(time).size(bar.text_size).class(theme::Text::Color(foreground)),
                    ]
                    .spacing(8)
                    .align_y(cosmic::iced::Alignment::Center),
                )
                .center_y(bar.group_item_height),
            )
            .padding([0, 8])
            .height(bar.group_item_height)
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
        );
        let clock: Element<'_, cosmic::Action<Message>> = if islands {
            clock
        } else {
            container(clock)
                .padding([2, 3])
                .height(bar.group_height)
                .align_y(alignment::Vertical::Center)
                .class(theme::Container::custom(move |_| bar_group_style(shell_theme)))
                .into()
        };
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
            let display = motion::button(
                button::custom(bar_content(
                    bar_icon(ferese_theme::icons::DISPLAY, bar.icon_size, foreground),
                    control_height,
                ))
                .name("Display mode")
                .height(control_height)
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
            );
            right = right.push(display);
        }
        let right: Element<'_, cosmic::Action<Message>> = if islands {
            island(
                container(right)
                    .padding([2, 3])
                    .height(bar.group_height)
                    .class(theme::Container::custom(move |_| bar_group_style(shell_theme)))
                    .into(),
                shell_theme,
                self.config.status.bar_island_padding,
                true,
                compositor_material,
            )
        } else {
            right.into()
        };
        let available = self
            .outputs
            .iter()
            .find(|output| output.bar == id)
            .and_then(|output| output.size)
            .map_or(0., |(width, _)| width as f32)
            - 2. * (shell_theme.panel_padding + shell_theme.bar_margin_horizontal as f32);
        let media_visible = self.media.snapshot.selected.is_some();
        let title_width = (available - 920. - if media_visible { 400. } else { 0. }).clamp(0., 360.);
        // Without a center title, give media controls more of a narrow bar.
        // Keep equal sides when the title is shown so it stays centered.
        let right_portion = if media_visible && title_width == 0. { 2 } else { 1 };
        let title = if self.config.status.window_title && title_width > 0. {
            focused_bar_title(&self.snapshot, focused_output)
        } else {
            ""
        };
        let center = text(title)
            .size(bar.text_size)
            .width(title_width)
            .height(control_height)
            .align_x(alignment::Horizontal::Center)
            .align_y(alignment::Vertical::Center)
            .wrapping(cosmic::iced::widget::text::Wrapping::None)
            .ellipsize(cosmic::iced::widget::text::Ellipsize::End(
                cosmic::iced::advanced::text::EllipsizeHeightLimit::Lines(1),
            ))
            .class(theme::Text::Color(foreground));
        let center: Element<'_, cosmic::Action<Message>> = if islands && !title.is_empty() {
            island(
                container(center)
                    .height(bar.group_height)
                    .align_y(alignment::Vertical::Center)
                    .class(theme::Container::custom(move |_| bar_group_style(shell_theme)))
                    .into(),
                shell_theme,
                self.config.status.bar_island_padding,
                true,
                compositor_material,
            )
        } else {
            center.into()
        };
        let right = cosmic::iced::widget::scrollable(container(right).width(Length::Shrink))
            .direction(cosmic::iced::widget::scrollable::Direction::Horizontal(
                cosmic::iced::widget::scrollable::Scrollbar::default()
                    .width(0)
                    .scroller_width(0),
            ))
            .anchor_right()
            .width(Length::Shrink)
            .height(if islands { bar.height } else { bar.group_height });
        let content = row![
            container(left).width(Length::Fill),
            center,
            container(right)
                .width(Length::FillPortion(right_portion))
                .align_x(alignment::Horizontal::Right),
        ]
        .spacing(8)
        .align_y(cosmic::iced::Alignment::Center)
        .height(Length::Fill);

        let content = container(content)
            .width(Length::Fill)
            .height(Length::Fill)
            .padding([0, shell_theme.panel_padding.round() as u16])
            .class(theme::Container::custom(move |_| {
                bar_style(shell_theme, compositor_material, islands)
            }))
            .into();
        presentation::frame(
            content,
            mode,
            shell_theme.bar_radius,
            move |regions| {
                if let Some(effects) = effects
                    && let Err(error) =
                        effects.set_material_regions(regions, super::ferese_surface_effects_v1::Role::Panel)
                {
                    eprintln!("ferese-shell: could not update bar material: {error}");
                }
            },
            move |regions| cosmic::Action::App(Message::BarRegionsChanged(id, regions)),
        )
    }
}

pub(super) fn bar_content<'a>(
    content: impl Into<Element<'a, cosmic::Action<Message>>>,
    height: f32,
) -> cosmic::widget::Container<'a, cosmic::Action<Message>, cosmic::Theme, cosmic::Renderer> {
    // A one-pixel optical offset below the geometric center. Compact bars
    // reduce the offset so the 20 px icons keep their full size.
    container(content)
        .padding(cosmic::iced::Padding {
            top: (height - 20.0).clamp(0.0, 2.0),
            ..Default::default()
        })
        .center_y(height)
}

pub(super) fn overview_control(bar: BarMetrics, foreground: Color) -> Element<'static, cosmic::Action<Message>> {
    bar_content(
        bar_icon(ferese_theme::icons::FERESE, bar.overview_icon_size, foreground),
        bar.control_height,
    )
    .width(bar.overview_icon_size)
    .align_x(alignment::Horizontal::Center)
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
    bar_content(
        text(name.to_owned())
            .size(12)
            .class(theme::Text::Color(color(foreground))),
        bar.group_item_height,
    )
    .width(24)
    .align_x(alignment::Horizontal::Center)
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

pub(super) fn bar_style(theme: ShellTheme, compositor_material: bool, islands: bool) -> container::Style {
    let mut style = if islands {
        container::Style::default()
    } else {
        ferese_theme::controls::surface_appearance(color(theme.bar_background), theme.bar_radius)
    };
    if compositor_material {
        style.background = None;
    }
    style.text_color = Some(color(theme.text_primary));
    style.icon_color = style.text_color;
    style.snap = true;
    style
}

pub(super) fn island<'a>(
    content: Element<'a, cosmic::Action<Message>>,
    theme: ShellTheme,
    horizontal_padding: f32,
    islands: bool,
    compositor_material: bool,
) -> Element<'a, cosmic::Action<Message>> {
    if !islands {
        return content;
    }

    let bar = BarMetrics::from(theme);
    container(content)
        .id(presentation::island_id())
        .padding(cosmic::iced::Padding {
            top: (bar.height - bar.group_height) * 0.5,
            bottom: (bar.height - bar.group_height) * 0.5,
            left: horizontal_padding,
            right: horizontal_padding,
        })
        .height(bar.height)
        .align_y(alignment::Vertical::Center)
        .class(cosmic::theme::Container::custom(move |_| {
            bar_style(theme, compositor_material, false)
        }))
        .into()
}

pub(super) fn input_region(
    mode: BarLayout,
    hidden: bool,
    regions: &[[f32; 5]],
) -> Option<Vec<cosmic::iced::Rectangle>> {
    if hidden {
        return Some(Vec::new());
    }

    if mode == BarLayout::Continuous {
        return None;
    }

    Some(
        regions
            .iter()
            .map(|r| cosmic::iced::Rectangle {
                x: r[0].floor(),
                y: r[1].floor(),
                width: (r[0] + r[2]).ceil() - r[0].floor(),
                height: (r[1] + r[3]).ceil() - r[1].floor(),
            })
            .collect(),
    )
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

#[cfg(test)]
mod island_tests {
    use super::*;

    #[test]
    fn island_gaps_are_click_through_and_fullscreen_disables_every_input_region() {
        let regions = [[12.25, 2.5, 80.5, 24., 14.], [400., 2., 160., 24., 14.]];
        let input = input_region(BarLayout::Islands, false, &regions).unwrap();
        assert_eq!(input.len(), 2);
        assert_eq!(
            input[0],
            cosmic::iced::Rectangle::new((12., 2.).into(), (81., 25.).into())
        );
        assert!(!input.iter().any(|region| region.contains((240., 14.).into())));
        assert_eq!(input_region(BarLayout::Islands, true, &regions), Some(Vec::new()));
        assert_eq!(input_region(BarLayout::Continuous, false, &regions), None);
        assert_eq!(input_region(BarLayout::Continuous, true, &regions), Some(Vec::new()));
    }

    #[test]
    fn islands_reuse_the_bar_background_without_replacing_the_inner_section_style() {
        let theme = ShellTheme {
            bar_background: [12, 24, 36, 153],
            ..Default::default()
        };
        let frame = bar_style(theme, false, true);
        assert!(frame.background.is_none());
        assert_eq!(frame.border.width, 0.);
        assert_eq!(frame.shadow.color.a, 0.);
        let outer = bar_style(theme, false, false);
        assert_eq!(outer.background, Some(Background::Color(color(theme.bar_background))));
        assert_eq!(outer.border.radius, theme.bar_radius.into());
        assert!(bar_style(theme, true, false).background.is_none());
        let inner = bar_group_style(theme);
        assert_eq!(
            inner.background,
            Some(Background::Color(color_with_opacity(theme.border, 0.25)))
        );
        assert_eq!(inner.border.width, 1.);
    }
}
