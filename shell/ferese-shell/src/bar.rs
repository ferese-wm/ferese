use super::{
    BarMetrics, Color, Element, FereseShell, Length, Message, ShellSnapshot, alignment, bar_icon,
    button, color, container, control, motion, row, status_ui, theme, window,
};
use crate::panel::{Availability, Item, ItemKind, Panel, Zone};
#[cfg(test)]
use ferese_panel::PanelStyle;
use ferese_panel::{GroupSurface, PanelSurface};

use ferese_theme::panel as adaptive;
pub(super) mod presentation;
mod text;
use crate::panel::Representation;
use crate::panel_layout::Placement;

#[cfg(test)]
mod binding_mode_tests;
#[cfg(test)]
mod click_tests;

impl FereseShell {
    pub(super) fn binding_mode_transition(
        &mut self,
        was_active: bool,
    ) -> cosmic::app::Task<Message> {
        match (was_active, self.snapshot.binding_mode.is_some()) {
            (false, true) => {
                let mut tasks = vec![self.destroy_menu(), self.dismiss_workspace_tooltip()];
                let hubs: Vec<_> = self
                    .outputs
                    .iter()
                    .filter(|entry| entry.runtime.hub.is_some())
                    .map(|entry| entry.bar)
                    .collect();
                for id in hubs {
                    // Release keyboard ownership and return to the collapsed
                    // backing allocation before displaying the mode indicator.
                    tasks.push(self.suspend_hub(id));
                }
                cosmic::app::Task::batch(tasks)
            }
            (true, false) => self.sync_hub_activity(),
            _ => cosmic::app::Task::none(),
        }
    }

    pub(super) fn output_for_bar(&self, id: window::Id) -> Option<&control::OutputSnapshot> {
        let name = self
            .outputs
            .iter()
            .find(|entry| entry.bar == id)?
            .name
            .as_deref()?;
        self.snapshot
            .outputs
            .iter()
            .find(|output| output.name == name)
    }

    pub(super) fn active_bar(&self) -> Option<window::Id> {
        self.outputs
            .iter()
            .find(|entry| {
                self.snapshot.outputs.iter().any(|output| {
                    output.focused && Some(output.name.as_str()) == entry.name.as_deref()
                })
            })
            .or_else(|| self.outputs.first())
            .map(|output| output.bar)
    }

    pub(super) fn bar_hidden(&self, id: window::Id) -> bool {
        output_bar_hidden(
            &self.snapshot,
            self.output_for_bar(id).map(|output| output.id),
        )
    }

    pub(super) fn view_layer(&self, id: window::Id) -> Element<'_, cosmic::Action<Message>> {
        if self.bar_hidden(id) {
            return container(self.render.text(""))
                .width(Length::Fill)
                .height(Length::Fill)
                .into();
        }
        let panel = self.panel_for_bar(id);
        if panel.presentation == ferese_panel::Presentation::Text {
            return self.view_text_bar(id);
        }
        if let Some(name) = &self.snapshot.binding_mode {
            return self.view_binding_mode(id, name);
        }
        if panel.style == ferese_panel::PanelStyle::Morph {
            return self.view_hub(id);
        }
        let mut shell_theme = self.config.theme.for_bar();
        shell_theme.bar_background[3] =
            (shell_theme.panel_opacity(panel.background_opacity) * 255.).round() as u8;
        let output = self.outputs.iter().find(|output| output.bar == id);
        let effects = output.and_then(|output| output.effects.as_ref());
        let compositor_material = effects.is_some();
        let availability = Availability {
            capture: self.capture_status.active(),
            activities: self.activity_available(id),
            media: self.media.snapshot.selected.is_some(),
            network: self.status.network.is_some(),
            audio: self.status.audio.is_some(),
            notifications: self.status.notifications.is_some(),
            battery: self.status.battery.is_some(),
            external_display: self.display_mode.external_connected(),
        };
        let mut samples = Vec::new();
        for zone in [&panel.center, &panel.start, &panel.end] {
            for group in &zone.groups {
                let item_iter = group
                    .items
                    .iter()
                    .filter(|item| item.available(availability));
                for item in item_iter {
                    if matches!(item.kind, ItemKind::FocusedWindow)
                        && focused_bar_title(&self.snapshot, self.output_for_bar(id)).is_empty()
                    {
                        continue;
                    }
                    let alternatives = item.representations();
                    for representation in alternatives {
                        let view = self
                            .view_panel_item(
                                id,
                                item,
                                None,
                                panel.group_surface() == GroupSurface::Island,
                                representation,
                            )
                            .0;
                        samples.push(adaptive::Sample {
                            id: item.id.clone(),
                            representation,
                            minimum: item.kind.minimum_width(),
                            view,
                        });
                    }
                }
            }
        }
        let overflow_group = panel.overflow_group();
        samples.push(adaptive::Sample {
            id: overflow_group.items[0].id.clone(),
            representation: Representation::Icon,
            minimum: None,
            view: self
                .view_panel_item(
                    id,
                    &overflow_group.items[0],
                    None,
                    panel.group_surface() == GroupSurface::Island,
                    Representation::Icon,
                )
                .0,
        });
        // Measure the control itself; reserve group decoration separately.
        let overflow_width = 2.0 * f32::from(overflow_group.padding[1])
            + if panel.group_surface() == GroupSurface::Island {
                2.0 * overflow_group.island_padding
            } else {
                0.0
            };
        let groups = [
            (ferese_panel::shape::ZonePosition::Start, &panel.start),
            (ferese_panel::shape::ZonePosition::Center, &panel.center),
            (ferese_panel::shape::ZonePosition::End, &panel.end),
        ]
        .into_iter()
        .flat_map(|(position, zone)| {
            zone.groups
                .iter()
                .map(move |group| (group_material_id(panel, group), position))
        })
        .chain(std::iter::once((
            group_material_id(panel, &overflow_group),
            ferese_panel::shape::ZonePosition::End,
        )))
        .collect();
        let content = adaptive::frame(
            std::borrow::Cow::Borrowed(panel),
            samples,
            overflow_width,
            output
                .and_then(|output| output.panel_resolution.clone())
                .unwrap_or_default(),
            move |resolution| {
                let zone = |zone: &Zone| {
                    view_zone(
                        panel,
                        zone,
                        availability,
                        |item| matches!(resolution.items.get(&item.id), Some(Placement::Visible { width, .. }) if *width > 0.0),
                        |item, surface| {
                            let Some(Placement::Visible {
                                representation,
                                width,
                            }) = resolution.items.get(&item.id)
                            else {
                                unreachable!()
                            };
                            self.view_panel_item(
                                id,
                                item,
                                Some(*width),
                                surface == GroupSurface::Island,
                                *representation,
                            )
                        },
                    )
                };
                let start = zone(&panel.start);
                let center = zone(&panel.center);
                let end = zone(&panel.end);
                let end: Element<'_, cosmic::Action<Message>> = if resolution.overflow.is_empty() {
                    end
                } else {
                    let trigger = view_zone(
                        panel,
                        &Zone {
                            groups: vec![overflow_group.clone()],
                            spacing: 0.0,
                        },
                        availability,
                        |_| true,
                        |item, surface| {
                            self.view_panel_item(
                                id,
                                item,
                                Some(resolution.overflow_trigger_width),
                                surface == GroupSurface::Island,
                                Representation::Icon,
                            )
                        },
                    );
                    let end_has_items = panel.end.groups.iter().flat_map(|group| &group.items)
                        .any(|item| matches!(resolution.items.get(&item.id), Some(Placement::Visible { width, .. }) if *width > 0.0));
                    if end_has_items {
                        row![trigger, end]
                            .spacing(panel.zone_spacing(&panel.end))
                            .align_y(cosmic::iced::Alignment::Center)
                            .into()
                    } else {
                        trigger
                    }
                };
                cosmic::iced::widget::stack![
                    row![
                        start,
                        cosmic::iced::widget::Space::new().width(Length::Fill),
                        end
                    ]
                    .width(Length::Fill)
                    .height(Length::Fill)
                    .align_y(cosmic::iced::Alignment::Center),
                    container(center)
                        .center_x(Length::Fill)
                        .center_y(Length::Fill),
                ]
                .into()
            },
            move |resolution| cosmic::Action::App(Message::PanelResolved(id, resolution)),
        );
        let content = container(content)
            .width(Length::Fill)
            .height(Length::Fill)
            .padding([0, panel.resolved_geometry().inner_padding.round() as u16])
            .into();
        let content = ferese_theme::panel_surface::frame(
            content,
            ferese_theme::panel_surface::Options {
                parameters: panel.shape_parameters(),
                bounds: None,
                groups,
                scale: output.map_or(1., |entry| entry.scale),
                high_contrast: shell_theme.high_contrast,
                background: color(shell_theme.bar_background),
                border: if panel.border {
                    color(shell_theme.border)
                } else {
                    Color::TRANSPARENT
                },
                divider: color(shell_theme.border),
                native: compositor_material,
            },
            move |scene| {
                if let Some(effects) = effects
                    && let Err(error) = effects.set_panel_presentation(scene)
                {
                    eprintln!("ferese-shell: could not update panel material: {error}");
                }
            },
            Some(Box::new(move |scene| {
                cosmic::Action::App(Message::PanelSurfaceChanged(id, Box::new(scene.clone())))
            })),
        );
        self.with_workspace_hover(id, content)
    }

    fn view_binding_mode<'a>(
        &'a self,
        id: window::Id,
        name: &'a str,
    ) -> Element<'a, cosmic::Action<Message>> {
        let panel = self.panel_for_bar(id);
        let output = self
            .outputs
            .iter()
            .find(|entry| entry.bar == id)
            .expect("panel output");
        let effects = output.effects.as_ref();
        let width = output
            .runtime
            .hub
            .as_ref()
            .map(|hub| hub.borrow().allocation[0])
            .unwrap_or_else(|| output.size.map_or(320, |size| size.0 as u32))
            .min(320);
        let badge_id = cosmic::iced::advanced::widget::Id::new("ferese-binding-mode");
        let title = self
            .render
            .text(format!("Mode: {name}"))
            .size(14)
            .width(Length::Fill)
            .ellipsize(cosmic::iced::widget::text::Ellipsize::End(
                cosmic::iced::advanced::text::EllipsizeHeightLimit::Lines(1),
            ));
        let badge = container(
            row![title, self.render.text("Esc to exit").size(12)]
                .spacing(12)
                .align_y(cosmic::iced::Alignment::Center),
        )
        .id(badge_id.clone())
        .padding([0, 12])
        .width(width as f32)
        .height(panel.resolved_geometry().height)
        .align_y(alignment::Vertical::Center);
        let content = container(badge)
            .center_x(Length::Fill)
            .center_y(Length::Fill);
        let palette = self.config.theme.for_bar();
        let mut background = color(palette.bar_background);
        background.a = palette.panel_opacity(panel.background_opacity);
        // Replace the native paint/input scene as well as the text. Retaining
        // the previous bar or Morph scene can clip a newly displayed mode.
        ferese_theme::panel_surface::frame(
            content.into(),
            ferese_theme::panel_surface::Options {
                parameters: panel.shape_parameters(),
                bounds: Some(badge_id.clone()),
                groups: vec![(badge_id, ferese_panel::shape::ZonePosition::Center)],
                scale: output.scale,
                high_contrast: palette.high_contrast,
                background,
                border: if panel.border {
                    color(palette.border)
                } else {
                    Color::TRANSPARENT
                },
                divider: Color::TRANSPARENT,
                native: effects.is_some(),
            },
            move |scene| {
                if let Some(effects) = effects
                    && let Err(error) = effects.set_panel_presentation(scene)
                {
                    eprintln!("ferese-shell: could not update binding mode material: {error}");
                }
            },
            Some(Box::new(move |scene| {
                cosmic::Action::App(Message::PanelSurfaceChanged(id, Box::new(scene.clone())))
            })),
        )
    }

    pub(super) fn with_workspace_hover<'a>(
        &'a self,
        id: window::Id,
        content: Element<'a, cosmic::Action<Message>>,
    ) -> Element<'a, cosmic::Action<Message>> {
        presentation::hover(
            content,
            self.workspace_ui
                .hover_target(id)
                .map(|(workspace, target)| presentation::HoverTarget {
                    id: target.clone(),
                    exited: Box::new(move || {
                        cosmic::Action::App(Message::HoverWorkspace(
                            id,
                            workspace,
                            target.clone(),
                            false,
                        ))
                    }),
                }),
        )
    }

    fn view_panel_item(
        &self,
        id: window::Id,
        item: &Item,
        width: Option<f32>,
        islands: bool,
        representation: Representation,
    ) -> (Element<'_, cosmic::Action<Message>>, bool) {
        let shell_theme = self.config.theme.for_bar();
        let bar = BarMetrics::from(self.panel_for_bar(id).resolved_geometry());
        let control_height = if islands {
            bar.group_item_height
        } else {
            bar.control_height
        };
        let foreground = color(shell_theme.text_primary);
        let focused_output = self.output_for_bar(id);
        let selected = self.menu.as_ref().is_some_and(|menu| {
            menu.anchor.parent == id
                && menu.anchor.panel == self.config.panels[0].id
                && menu.anchor.item.as_ref() == Some(&item.id)
        });
        let element = match item.kind {
            ItemKind::Overview => motion::button(
                self.render,
                button::custom(overview_control(
                    BarMetrics {
                        control_height,
                        ..bar
                    },
                    color(shell_theme.accent),
                ))
                .height(control_height)
                .padding([0, 7])
                .on_press(cosmic::Action::App(Message::ToggleOverview)),
                foreground,
                self.overview_active,
                1.0,
            ),
            ItemKind::Workspaces { style } => {
                let workspaces = self.view_workspace_item(id, style, &item.id.0, 1.);
                if let Some(width) = width {
                    cosmic::iced::widget::scrollable(workspaces)
                        .direction(cosmic::iced::widget::scrollable::Direction::Horizontal(
                            cosmic::iced::widget::scrollable::Scrollbar::default()
                                .width(0)
                                .scroller_width(0),
                        ))
                        .width(width)
                        .into()
                } else {
                    workspaces
                }
            }
            ItemKind::FocusedWindow => {
                let title = if item.visible && width.is_none_or(|width| width > 0.) {
                    focused_bar_title(&self.snapshot, focused_output)
                } else {
                    ""
                };
                let element: Element<'_, cosmic::Action<Message>> = self
                    .render
                    .panel_text(title)
                    .width(width.map_or(Length::Shrink, Length::Fixed))
                    .height(control_height)
                    .align_x(alignment::Horizontal::Center)
                    .align_y(alignment::Vertical::Center)
                    .wrapping(cosmic::iced::widget::text::Wrapping::None)
                    .ellipsize(cosmic::iced::widget::text::Ellipsize::End(
                        cosmic::iced::advanced::text::EllipsizeHeightLimit::Lines(1),
                    ))
                    .class(theme::Text::Color(foreground))
                    .into();
                return (container(element).max_width(360).into(), !title.is_empty());
            }
            ItemKind::Activities => {
                return self.view_activity_item(
                    id,
                    item,
                    representation,
                    selected,
                    width,
                    control_height,
                );
            }
            ItemKind::Media => self.view_media_item(representation, selected, width),
            ItemKind::QuickSettings => {
                self.view_status_item(status_ui::Menu::System, false, selected)
            }
            ItemKind::NightLight => {
                self.view_status_item(status_ui::Menu::NightLight, false, selected)
            }
            ItemKind::Network => self.view_status_item(status_ui::Menu::Network, false, selected),
            ItemKind::Audio => self.view_status_item(status_ui::Menu::Audio, false, selected),
            ItemKind::CaptureStatus => self.view_capture_status(representation, selected),
            ItemKind::Recording => {
                self.view_status_item(status_ui::Menu::Recording, false, selected)
            }
            ItemKind::Notifications => {
                self.view_status_item(status_ui::Menu::Notifications, false, selected)
            }
            ItemKind::Battery { percentage } => self.view_status_item(
                status_ui::Menu::Battery,
                percentage && representation != Representation::Icon,
                selected,
            ),
            ItemKind::Clock if representation == Representation::Icon => {
                self.view_status_item(status_ui::Menu::Calendar, false, selected)
            }
            ItemKind::Overflow => self.view_status_item(status_ui::Menu::Overflow, false, selected),
            ItemKind::Clock => {
                let (date, time) = self.clock.split_once(", ").unwrap_or(("", &self.clock));
                let mut content = row::with_capacity(2)
                    .spacing(8)
                    .align_y(cosmic::iced::Alignment::Center);
                if representation == Representation::Wide && !date.is_empty() {
                    content = content.push(
                        self.render
                            .panel_text(date)
                            .wrapping(cosmic::iced::widget::text::Wrapping::None)
                            .class(theme::Text::Color(foreground)),
                    );
                }
                let content = content.push(
                    self.render
                        .panel_text(time.to_ascii_uppercase())
                        .wrapping(cosmic::iced::widget::text::Wrapping::None)
                        .class(theme::Text::Color(foreground)),
                );
                motion::button(
                    self.render,
                    button::custom(bar_content(content, bar.group_item_height))
                        .padding([0, 4])
                        .height(bar.group_item_height)
                        .name("Open calendar")
                        .on_press_with_rectangle(move |offset, bounds| {
                            cosmic::Action::App(Message::OpenMenu(
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
                    selected,
                    1.0,
                )
            }
            ItemKind::DisplayMode => motion::button(
                self.render,
                button::custom(
                    bar_content(
                        bar_icon(ferese_theme::icons::DISPLAY, bar.icon_size, foreground),
                        control_height,
                    )
                    .width(bar.icon_slot_width)
                    .align_x(alignment::Horizontal::Center),
                )
                .name("Display mode")
                .height(control_height)
                .padding([
                    0.0,
                    ((bar.height - f32::from(bar.icon_slot_width)) * 0.5).max(4.0),
                ])
                .on_press(cosmic::Action::App(Message::OpenDisplays(
                    self.outputs
                        .iter()
                        .find(|output| output.bar == id)
                        .and_then(|output| output.name.clone()),
                ))),
                foreground,
                self.display_mode.open,
                1.0,
            ),
        };
        let panel = self.config.panels[0].id.clone();
        let item = item.id.clone();
        let element: Element<'_, cosmic::Action<Message>> = container(element)
            .width(width.map_or(Length::Shrink, Length::Fixed))
            .into();
        (
            element.map(move |action| match action {
                cosmic::Action::App(Message::OpenMenu(kind, anchor)) => {
                    cosmic::Action::App(Message::OpenPopover(
                        kind,
                        status_ui::PopoverAnchor {
                            parent: id,
                            panel: panel.clone(),
                            item: Some(item.clone()),
                            rectangle: anchor,
                        },
                    ))
                }
                other => other,
            }),
            true,
        )
    }

    pub(crate) fn view_panel_overflow<'a>(
        &'a self,
        mut rows: cosmic::widget::Column<
            'a,
            cosmic::Action<Message>,
            cosmic::Theme,
            cosmic::Renderer,
        >,
    ) -> cosmic::widget::Column<'a, cosmic::Action<Message>, cosmic::Theme, cosmic::Renderer> {
        let Some(menu) = &self.menu else { return rows };
        let Some(output) = self
            .outputs
            .iter()
            .find(|output| output.bar == menu.anchor.parent)
        else {
            return rows;
        };
        let panel = self.panel_for_bar(output.bar);
        let Some(resolution) = &output.panel_resolution else {
            return rows;
        };
        for id in &resolution.overflow {
            let Some(item) = panel.item(id) else {
                continue;
            };
            let element = self
                .view_panel_item(
                    output.bar,
                    item,
                    Some(menu.kind.width() - 32.0),
                    false,
                    Representation::Wide,
                )
                .0;
            let anchor = menu.anchor.clone();
            let element = element.map(move |action| match action {
                cosmic::Action::App(Message::OpenPopover(kind, mut target)) => {
                    target.rectangle = anchor.rectangle;
                    cosmic::Action::App(Message::OpenPopover(kind, target))
                }
                cosmic::Action::App(
                    message @ (Message::ToggleOverview
                    | Message::ActivateWorkspace(_)
                    | Message::StartRecording
                    | Message::StopRecording),
                ) => cosmic::Action::App(Message::PanelAction(Box::new(message))),
                other => other,
            });
            rows = rows.push(
                cosmic::widget::column![self.render.text(item.kind.label()).size(12), element]
                    .spacing(4),
            );
        }
        rows
    }

    pub(super) fn view_workspace_item(
        &self,
        id: window::Id,
        style: ferese_panel::WorkspaceStyle,
        instance: &str,
        opacity: f32,
    ) -> Element<'_, cosmic::Action<Message>> {
        let focused_output = self.output_for_bar(id);
        let shell_theme = self.config.theme.for_bar();
        let mut palette = shell_theme.palette();
        palette.text.a *= opacity;
        palette.muted.a *= opacity;
        palette.accent.a *= opacity;
        let bar = BarMetrics::from(self.panel_for_bar(id).resolved_geometry());
        let mut workspace_buttons = row::with_capacity(self.snapshot.workspaces.len())
            .spacing(1)
            .align_y(cosmic::iced::Alignment::Center);
        let workspace_iter = self
            .snapshot
            .workspaces_for_output(focused_output.map(|output| output.id));
        for workspace in workspace_iter {
            let active = workspace_active_on_bar(workspace.id, focused_output);
            let owner = workspace
                .output
                .and_then(|id| self.snapshot.outputs.iter().find(|output| output.id == id));
            let transition = self.workspace_ui.transition(workspace.id, active);
            let apps = if style == ferese_panel::WorkspaceStyle::AppIcons {
                self.workspace_ui.apps(workspace.id, &self.snapshot)
            } else {
                Vec::new()
            };
            let details = ferese_theme::workspaces::Workspace {
                name: (&workspace.name).into(),
                index: workspace.index,
                window_count: workspace.window_count,
                active,
                visible_elsewhere: workspace.visible && !active,
            };
            let accessible_name = ferese_theme::workspaces::tooltip_label(&details);
            let indicator = motion::frame_driven(
                transition.revision(),
                move |now| {
                    ferese_theme::workspaces::indicator(
                        style,
                        details.clone(),
                        palette,
                        self.render.font,
                        bar.group_item_height,
                        transition.progress(now),
                        apps.iter()
                            .map(|handle| {
                                cosmic::widget::icon::icon(handle.clone())
                                    .size(16)
                                    .opacity(opacity)
                                    .into()
                            })
                            .collect(),
                    )
                },
                move |now| transition.active(now),
                |_| {},
                |_| None,
            );
            let workspace_button = button::custom(indicator)
                .name(format!(
                    "{accessible_name}{}{}",
                    if workspace.focused { ", focused" } else { "" },
                    owner.map_or(String::new(), |output| format!(", on {}", output.name))
                ))
                .height(bar.group_item_height)
                .padding(0)
                .on_press(cosmic::Action::App(Message::ActivateWorkspace(
                    workspace.id,
                )));

            let selector = motion::button(
                self.render,
                workspace_button,
                color(if active {
                    shell_theme.accent
                } else {
                    shell_theme.text_primary
                }),
                false,
                opacity,
            );
            let target = cosmic::iced::advanced::widget::Id::new(format!(
                "workspace:{id:?}:{instance}:{}",
                workspace.id
            ));
            let selector = cosmic::widget::mouse_area(container(selector).id(target.clone()))
                .on_enter(cosmic::Action::App(Message::HoverWorkspace(
                    id,
                    workspace.id,
                    target.clone(),
                    true,
                )))
                .on_exit(cosmic::Action::App(Message::HoverWorkspace(
                    id,
                    workspace.id,
                    target.clone(),
                    false,
                )));
            // A surface leave may not reset MouseArea's hover state. Recover
            // on reentry without publishing a message for every pointer move.
            let selector = if self.workspace_ui.hovering(id, &target) {
                selector
            } else {
                selector.on_move(move |_| {
                    cosmic::Action::App(Message::HoverWorkspace(
                        id,
                        workspace.id,
                        target.clone(),
                        true,
                    ))
                })
            };
            workspace_buttons = workspace_buttons.push(selector);
        }
        workspace_buttons.into()
    }
}

fn group_material_id(
    panel: &Panel,
    group: &crate::panel::Group,
) -> cosmic::iced::advanced::widget::Id {
    cosmic::iced::advanced::widget::Id::new(format!(
        "panel:{}:group:{}:material",
        panel.id.0, group.id.0
    ))
}

/// Render ordered instances; item rendering does not choose its neighbors or surface.
fn view_zone<'a>(
    panel: &Panel,
    zone: &Zone,
    availability: Availability,
    visible: impl Fn(&Item) -> bool,
    render: impl Fn(&Item, GroupSurface) -> (Element<'a, cosmic::Action<Message>>, bool),
) -> Element<'a, cosmic::Action<Message>> {
    let bar = BarMetrics::from(panel.resolved_geometry());
    let mut groups = row::with_capacity(zone.groups.len())
        .spacing(panel.zone_spacing(zone))
        .align_y(cosmic::iced::Alignment::Center);
    for group in &zone.groups {
        let surface = panel.group_surface();
        let mut controls =
            row::with_capacity(group.items.len()).align_y(cosmic::iced::Alignment::Center);
        let mut count = 0;
        let item_iter = group
            .items
            .iter()
            .filter(|item| item.available(availability) && visible(item));
        for item in item_iter {
            if count > 0 {
                controls = controls.push(
                    cosmic::iced::widget::Space::new()
                        .width(item.gap_before.unwrap_or(group.spacing)),
                );
            }
            let (element, _) = render(item, surface);
            controls = controls
                .push(container(element).id(format!("panel:{}:item:{}", panel.id.0, item.id.0)));
            count += 1;
        }
        if count == 0 {
            continue;
        }
        let horizontal = f32::from(group.padding[1])
            + if panel.group_surface() == GroupSurface::Island {
                group.island_padding
            } else {
                0.
            };
        let content = container(controls)
            .padding(cosmic::iced::Padding {
                top: f32::from(group.padding[0]),
                bottom: f32::from(group.padding[0]),
                left: horizontal,
                right: horizontal,
            })
            .height(bar.height)
            .align_y(alignment::Vertical::Center)
            .id(format!("panel:{}:group:{}", panel.id.0, group.id.0));
        let content: Element<'_, cosmic::Action<Message>> = container(content)
            .id(group_material_id(panel, group))
            .height(bar.height)
            .into();
        groups = groups.push(content);
    }
    groups.into()
}

pub(super) fn bar_content<'a>(
    content: impl Into<Element<'a, cosmic::Action<Message>>>,
    height: f32,
) -> cosmic::widget::Container<'a, cosmic::Action<Message>, cosmic::Theme, cosmic::Renderer> {
    container(content).center_y(height)
}

pub(super) fn overview_control(
    bar: BarMetrics,
    foreground: Color,
) -> Element<'static, cosmic::Action<Message>> {
    bar_content(
        bar_icon(ferese_theme::icons::FERESE, bar.icon_size, foreground),
        bar.control_height,
    )
    .width(bar.icon_slot_width)
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
    if snapshot.binding_mode.is_some() {
        return false;
    }
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

pub(super) fn focused_bar_title<'a>(
    snapshot: &'a ShellSnapshot,
    output: Option<&control::OutputSnapshot>,
) -> &'a str {
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

pub(super) fn workspace_active_on_bar(
    workspace: u64,
    output: Option<&control::OutputSnapshot>,
) -> bool {
    output.is_some_and(|output| output.active_workspace == workspace)
}

pub(super) fn input_region(
    mode: PanelSurface,
    hidden: bool,
    regions: &[[f32; 5]],
) -> Option<Vec<cosmic::iced::Rectangle>> {
    if hidden {
        return Some(Vec::new());
    }

    if mode == PanelSurface::Solid {
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

#[cfg(test)]
mod island_tests {
    use super::*;

    #[test]
    fn mode_indicator_stays_visible_on_fullscreen_outputs() {
        let mut snapshot = super::super::tests::snapshot_with_fullscreen_window(7);
        assert!(output_bar_hidden(&snapshot, Some(1)));
        snapshot.binding_mode = Some("resize".into());
        assert!(!output_bar_hidden(&snapshot, Some(1)));
        snapshot.binding_mode = None;
        assert!(output_bar_hidden(&snapshot, Some(1)));
    }

    use crate::panel::{ItemId, Panel};
    use cosmic::iced::advanced::renderer::Headless;
    use cosmic::iced::advanced::{Layout, layout, widget};
    use cosmic::iced::{Font, Pixels, Rectangle, Size};

    #[derive(Default)]
    struct Bounds {
        targets: Vec<(widget::Id, String)>,
        items: Vec<(String, Rectangle)>,
        islands: Vec<Rectangle>,
        groups: Vec<Rectangle>,
        group_ids: Vec<widget::Id>,
        material_ids: Vec<widget::Id>,
    }

    impl widget::Operation for Bounds {
        fn traverse(&mut self, children: &mut dyn FnMut(&mut dyn widget::Operation)) {
            children(self);
        }

        fn container(&mut self, id: Option<&widget::Id>, bounds: Rectangle) {
            if self.group_ids.iter().any(|target| Some(target) == id) {
                self.groups.push(bounds);
            }
            if self.material_ids.iter().any(|target| Some(target) == id) {
                self.islands.push(bounds);
            }
            if let Some((_, name)) = self.targets.iter().find(|(target, _)| Some(target) == id) {
                self.items.push((name.clone(), bounds));
            }
        }
    }

    #[test]
    fn media_label_grows_with_allocation_and_keeps_transport_inside_the_item() {
        #[derive(Default)]
        struct ContentBounds {
            labels: Vec<Rectangle>,
            buttons: Vec<Rectangle>,
        }
        impl widget::Operation for ContentBounds {
            fn traverse(&mut self, children: &mut dyn FnMut(&mut dyn widget::Operation)) {
                children(self);
            }
            fn text(&mut self, _: Option<&widget::Id>, bounds: Rectangle, _: &str) {
                self.labels.push(bounds);
            }
            fn focusable(
                &mut self,
                _: Option<&widget::Id>,
                bounds: Rectangle,
                _: &mut dyn widget::operation::Focusable,
            ) {
                self.buttons.push(bounds);
            }
        }
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let renderer = runtime
            .block_on(<cosmic::Renderer as Headless>::new(
                Font::default(),
                Pixels(14.),
                Some("tiny-skia"),
            ))
            .unwrap();
        let mut shell = crate::tests::shell_with_measured_panel();
        std::sync::Arc::make_mut(&mut shell.media.snapshot).selected = Some(ferese_ipc::media::Player {
            title: "A long song title that should use the room between navigation and the centered window title".into(),
            status: ferese_ipc::media::Playback::Playing,
            can_control: true,
            can_pause: true,
            ..Default::default()
        });
        let item = Item::new("media", ItemKind::Media);
        let mut previous_label = 0.;
        for width in [120., 200., 320.] {
            let mut view = shell
                .view_panel_item(
                    shell.outputs[0].bar,
                    &item,
                    Some(width),
                    true,
                    Representation::Wide,
                )
                .0;
            let mut tree = widget::Tree::new(&view);
            let node = view.as_widget_mut().layout(
                &mut tree,
                &renderer,
                &layout::Limits::new(Size::ZERO, Size::new(800., 28.)),
            );
            let mut content = ContentBounds::default();
            view.as_widget_mut()
                .operate(&mut tree, Layout::new(&node), &renderer, &mut content);
            assert_eq!(content.labels.len(), 1);
            assert_eq!(content.buttons.len(), 2);
            let label = content.labels[0];
            assert!(label.width > previous_label);
            previous_label = label.width;
            let bounds_iter = content.labels.iter().chain(&content.buttons);
            for bounds in bounds_iter {
                assert!(
                    bounds.x >= 0. && bounds.x + bounds.width <= width + 0.01,
                    "media control exceeds its allocation: {bounds:?}, width={width}"
                );
            }
        }
    }

    #[test]
    fn clock_content_has_balanced_horizontal_padding() {
        #[derive(Default)]
        struct Labels(Vec<Rectangle>);
        impl widget::Operation for Labels {
            fn traverse(&mut self, children: &mut dyn FnMut(&mut dyn widget::Operation)) {
                children(self);
            }
            fn text(&mut self, _: Option<&widget::Id>, bounds: Rectangle, _: &str) {
                self.0.push(bounds);
            }
        }
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let renderer = runtime
            .block_on(<cosmic::Renderer as Headless>::new(
                Font::default(),
                Pixels(14.),
                Some("tiny-skia"),
            ))
            .unwrap();
        let mut shell = crate::tests::shell_with_measured_panel();
        shell.clock = "10 oct, 4:35 pm".into();
        let item = Item::new("clock", ItemKind::Clock);
        for height in [28., 31., 48.] {
            shell.config.panels[0].geometry.height = Some(height);
            for representation in [Representation::Wide, Representation::Compact] {
                let mut view = shell
                    .view_panel_item(shell.outputs[0].bar, &item, None, true, representation)
                    .0;
                let mut tree = widget::Tree::new(&view);
                let node = view.as_widget_mut().layout(
                    &mut tree,
                    &renderer,
                    &layout::Limits::new(Size::ZERO, Size::new(800., height)),
                );
                let mut labels = Labels::default();
                view.as_widget_mut()
                    .operate(&mut tree, Layout::new(&node), &renderer, &mut labels);
                let left = labels.0.first().unwrap().x;
                let last = labels.0.last().unwrap();
                let right = node.size().width - last.x - last.width;
                assert!((left - right).abs() < 0.01);
                assert!(
                    right <= 4.01,
                    "panel height must not add horizontal space to the clock"
                );
            }
        }
    }

    fn measure_zone(
        panel: &Panel,
        zone: &Zone,
        availability: Availability,
        title_content: bool,
    ) -> Bounds {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let renderer = runtime
            .block_on(<cosmic::Renderer as Headless>::new(
                Font::default(),
                Pixels(14.),
                Some("tiny-skia"),
            ))
            .unwrap();
        let view = view_zone(
            panel,
            zone,
            availability,
            |item| title_content || !matches!(item.kind, ItemKind::FocusedWindow),
            |item, _| {
                let width = match item.kind {
                    ItemKind::Overview => 28.,
                    ItemKind::Workspaces { .. } => 72.,
                    ItemKind::FocusedWindow => 80.,
                    ItemKind::Clock => 60.,
                    _ => 20.,
                };
                let height = if matches!(item.kind, ItemKind::FocusedWindow) {
                    24.
                } else {
                    20.
                };
                (
                    container(
                        cosmic::iced::widget::Space::new()
                            .width(width)
                            .height(height),
                    )
                    .into(),
                    !matches!(item.kind, ItemKind::FocusedWindow) || title_content,
                )
            },
        );
        let mut view: Element<'_, cosmic::Action<Message>> = container(view)
            .height(panel.resolved_geometry().height)
            .align_y(alignment::Vertical::Center)
            .into();
        let mut tree = widget::Tree::new(&view);
        let node = view.as_widget_mut().layout(
            &mut tree,
            &renderer,
            &layout::Limits::new(
                Size::ZERO,
                Size::new(1200., panel.resolved_geometry().height),
            ),
        );
        let mut bounds = Bounds {
            group_ids: zone
                .groups
                .iter()
                .map(|group| widget::Id::from(format!("panel:{}:group:{}", panel.id.0, group.id.0)))
                .collect(),
            material_ids: zone
                .groups
                .iter()
                .map(|group| group_material_id(panel, group))
                .collect(),
            targets: zone
                .groups
                .iter()
                .flat_map(|group| &group.items)
                .map(|item| {
                    (
                        widget::Id::from(format!("panel:{}:item:{}", panel.id.0, item.id.0)),
                        item.id.0.clone(),
                    )
                })
                .collect(),
            ..Default::default()
        };
        view.as_widget_mut()
            .operate(&mut tree, Layout::new(&node), &renderer, &mut bounds);
        bounds
    }

    #[test]
    fn islands_cover_the_full_panel_height_independently_of_group_decoration() {
        let mut panel = Panel::from_preset(PanelStyle::Floating);
        for margin in [0, 6] {
            panel.geometry.height = Some(36.);
            panel.geometry.edge_margin = Some(margin);
            for border in [false, true] {
                panel.border = border;
                let bounds = measure_zone(&panel, &panel.start, Availability::default(), true);
                assert_eq!(bounds.islands[0].y, 0.);
                assert_eq!(bounds.islands[0].height, 36.);
                assert_eq!(
                    bounds.groups[0].height, 36.,
                    "group layout follows the same baseline as its shared surface"
                );
            }
        }
    }

    #[test]
    fn composition_preserves_default_group_gaps_and_material_geometry() {
        for padding in [0., 4., 13.5] {
            for background in [PanelStyle::Flat, PanelStyle::Floating] {
                let mut panel = Panel::from_preset(background);
                let group_iter = [&mut panel.start, &mut panel.center, &mut panel.end]
                    .into_iter()
                    .flat_map(|zone| &mut zone.groups);
                for group in group_iter {
                    group.island_padding = padding;
                }
                let start = measure_zone(&panel, &panel.start, Availability::default(), true);
                assert_eq!(
                    start
                        .items
                        .iter()
                        .map(|(id, _)| id.as_str())
                        .collect::<Vec<_>>(),
                    ["overview", "workspaces"]
                );
                let islands = background == PanelStyle::Floating;
                assert_eq!(start.items[0].1.x, if islands { padding + 3. } else { 0. });
                assert_eq!(
                    start.items[1].1.x - start.items[0].1.x,
                    if islands { 29. } else { 34. }
                );
                assert_eq!(start.islands.len(), panel.start.groups.len());
                if islands {
                    assert_eq!(
                        start.islands[0],
                        Rectangle::new(
                            (0., 0.).into(),
                            (107. + 2. * padding, panel.resolved_geometry().height).into()
                        )
                    );
                }
                let end = measure_zone(&panel, &panel.end, Availability::default(), true);
                assert_eq!(
                    end.items
                        .iter()
                        .map(|(id, _)| id.as_str())
                        .collect::<Vec<_>>(),
                    ["quick-settings", "night-light", "recording", "clock"]
                );
                assert_eq!(end.items[1].1.x - end.items[0].1.x, 21.);
                assert_eq!(end.items[2].1.x - end.items[1].1.x, 21.);
                assert_eq!(
                    end.items[3].1.x - end.items[2].1.x,
                    if islands { 21. } else { 34. }
                );
                if islands {
                    assert_eq!(
                        end.islands[0],
                        Rectangle::new(
                            (0., 0.).into(),
                            (129. + 2. * padding, panel.resolved_geometry().height).into()
                        )
                    );
                }
                let center = measure_zone(&panel, &panel.center, Availability::default(), true);
                assert_eq!(center.islands.len(), panel.center.groups.len());
                if islands {
                    assert_eq!(center.islands[0].width, 80. + 2. * padding);
                }
                let empty_title =
                    measure_zone(&panel, &panel.center, Availability::default(), false);
                assert!(empty_title.islands.is_empty());
                assert!(
                    empty_title.items.is_empty(),
                    "an unavailable title leaves no group behind"
                );
            }
        }
    }

    #[test]
    fn group_renderer_obeys_order_and_instance_identity_and_skips_unavailable_groups() {
        let mut panel = Panel::from_preset(PanelStyle::Floating);
        let group = &mut panel.end.groups[0];
        let mut clock = group
            .items
            .iter()
            .find(|item| item.kind == ItemKind::Clock)
            .unwrap()
            .clone();
        let battery = group
            .items
            .iter()
            .find(|item| matches!(item.kind, ItemKind::Battery { .. }))
            .unwrap()
            .clone();
        clock.gap_before = None;
        let mut second = clock.clone();
        second.id = ItemId("other-clock".into());
        group.items = vec![battery, clock, second];
        let mut unavailable = group.clone();
        unavailable.id = crate::panel::GroupId("unavailable".into());
        unavailable.items = vec![unavailable.items.remove(0)];
        panel.end.groups.insert(0, unavailable);
        let result = measure_zone(&panel, &panel.end, Availability::default(), true);
        assert_eq!(
            result
                .items
                .iter()
                .map(|(id, _)| id.as_str())
                .collect::<Vec<_>>(),
            ["clock", "other-clock"]
        );
        assert_eq!(
            result.items[0].1.x, 7.,
            "an unavailable group adds no space"
        );
        assert_eq!(result.items[1].1.x, 68.);
        assert_eq!(result.islands.len(), 1);
        panel.end.groups[1].items.reverse();
        let reordered = measure_zone(&panel, &panel.end, Availability::default(), true);
        assert_eq!(
            reordered
                .items
                .iter()
                .map(|(id, _)| id.as_str())
                .collect::<Vec<_>>(),
            ["other-clock", "clock"]
        );
    }

    #[test]
    fn island_gaps_are_click_through_and_fullscreen_disables_every_input_region() {
        let regions = [[12.25, 2.5, 80.5, 24., 14.], [400., 2., 160., 24., 14.]];
        let input = input_region(PanelSurface::None, false, &regions).unwrap();
        assert_eq!(input.len(), 2);
        assert_eq!(
            input[0],
            cosmic::iced::Rectangle::new((12., 2.).into(), (81., 25.).into())
        );
        assert!(
            !input
                .iter()
                .any(|region| region.contains((240., 14.).into()))
        );
        assert_eq!(
            input_region(PanelSurface::None, true, &regions),
            Some(Vec::new())
        );
        assert_eq!(input_region(PanelSurface::Solid, false, &regions), None);
        assert_eq!(
            input_region(PanelSurface::Solid, true, &regions),
            Some(Vec::new())
        );
    }

    #[test]
    fn panel_surface_policy_applies_to_every_occupied_group() {
        let mut panel = Panel::from_preset(PanelStyle::Floating);
        panel.geometry.height = Some(36.);
        for zone in [&panel.start, &panel.center, &panel.end] {
            let measured = measure_zone(&panel, zone, Availability::default(), true);
            assert_eq!(measured.islands.len(), measured.groups.len());
            let island_iter = measured.islands.iter().zip(&measured.groups);
            for (island, group) in island_iter {
                assert_eq!(island.height, 36.);
                assert_eq!(island.width, group.width);
            }
        }
        panel.apply_preset(PanelStyle::Flat);
        for zone in [&panel.start, &panel.center, &panel.end] {
            let measured = measure_zone(&panel, zone, Availability::default(), true);
            assert_eq!(
                measured.islands.len(),
                measured.groups.len(),
                "changing the shared shape preserves layout bounds"
            );
        }
    }
}
