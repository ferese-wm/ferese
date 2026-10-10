use crate::panel_edit::{Action, Destination, Zone};
use crate::{App, Element, Field, Kind, Message, widget};
use cosmic::iced::{Alignment, Length};
use ferese_config::panel::{Group, Item, ItemKind, Panel, Representation};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Choice(&'static str, &'static str);
impl std::fmt::Display for Choice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.1)
    }
}

impl App {
    fn inspector_heading(&self, title: &str, subtitle: &str) -> Element<'static, Message> {
        widget::column([])
            .spacing(5)
            .push(
                widget::row([])
                    .align_y(Alignment::Center)
                    .push(self.panel_heading(title.to_owned(), 16.).width(Length::Fill))
                    .push(self.settings_icon_button(
                        "Deselect",
                        "M6 6l12 12 M6 18L18 6",
                        Some(Message::PanelClearSelection),
                    )),
            )
            .push(self.note(subtitle))
            .push(self.panel_divider())
            .into()
    }

    pub(super) fn panel_divider(&self) -> Element<'static, Message> {
        let p = crate::visuals::Palette::from_resolved(&self.resolved.presented);
        widget::container(widget::Space::new().height(1).width(Length::Fill))
            .class(crate::visuals::surface(ferese_theme::mix(p.card, p.text, 0.09), 0.))
            .into()
    }

    pub(super) fn item_inspector(
        &self,
        panel: &Panel,
        item: &Item,
        destination: Destination,
        path: String,
    ) -> Element<'static, Message> {
        let palette = crate::visuals::Palette::from_resolved(&self.resolved.presented);
        let mut rows = widget::column([])
            .spacing(12)
            .push(self.inspector_heading(item.kind.label(), "Item settings"));
        let item_id = item.id.clone();
        rows = rows.push(
            widget::column([])
                .spacing(8)
                .push(self.label("Placement", 13.))
                .push(crate::visuals::panel_select(
                    cosmic::iced::widget::pick_list(
                        crate::panel_edit::destinations(panel),
                        Some(destination.clone()),
                        move |destination| Message::PanelEdit(Action::Move(item_id.clone(), destination)),
                    )
                    .font(self.font)
                    .text_size(13)
                    .padding([8, 10])
                    .width(Length::Fill),
                    palette,
                )),
        );
        rows = rows.push(self.panel_field(
            item,
            Field::new(format!("{path}.visible"), "Show item", "", Kind::Toggle(true)),
        ));
        rows = rows.push(self.panel_divider());
        if matches!(item.kind, ItemKind::Workspaces { .. }) {
            rows = rows.push(self.workspace_style_picker(item));
            rows = rows.push(self.panel_divider());
        }
        rows = rows.push(self.panel_field(
            item,
            Field::new(
                format!("{path}.overflow"),
                "Overflow",
                "Choose when to use the overflow menu.",
                Kind::Choice {
                    default: "auto",
                    choices: &[("never", "Keep visible"), ("auto", "When needed"), ("always", "Always")],
                },
            ),
        ));
        let representations = item.kind.representations();
        if representations.len() > 1 {
            let choices: &'static [(&'static str, &'static str)] = match representations {
                [Representation::Wide, Representation::Icon] => &[("wide", "Full"), ("icon", "Icon")],
                _ => &[("wide", "Full"), ("compact", "Compact"), ("icon", "Icon")],
            };
            rows = rows.push(self.panel_field(
                item,
                Field::new(
                    format!("{path}.representation"),
                    "Preferred size",
                    "Adapts to available space.",
                    Kind::Choice {
                        default: "wide",
                        choices,
                    },
                ),
            ));
        }
        if matches!(item.kind, ItemKind::Battery { .. }) {
            rows = rows.push(self.panel_field(
                item,
                Field::new(
                    format!("{path}.percentage"),
                    "Battery percentage",
                    "",
                    Kind::Toggle(true),
                ),
            ));
        }
        let group = Zone::ALL
            .into_iter()
            .flat_map(|zone| &zone.definition(panel).groups)
            .find(|group| group.id == destination.group)
            .unwrap();
        let index = group
            .items
            .iter()
            .position(|candidate| candidate.id == item.id)
            .unwrap();
        rows.push(self.panel_divider())
            .push(self.inspector_actions(
                (index > 0).then(|| Message::PanelEdit(Action::Earlier(item.id.clone()))),
                (index + 1 < group.items.len()).then(|| Message::PanelEdit(Action::Later(item.id.clone()))),
                "Remove item",
                Some(Message::PanelEdit(Action::Remove(item.id.clone()))),
            ))
            .into()
    }

    fn workspace_style_picker(&self, item: &Item) -> Element<'static, Message> {
        let palette = crate::visuals::Palette::from_resolved(&self.resolved.presented);
        let ItemKind::Workspaces { style: selected } = item.kind else {
            unreachable!()
        };
        let mut choices = widget::column([])
            .spacing(6)
            .push(self.label("Workspace style", 13.))
            .push(self.note("A sample of active, occupied and empty workspaces."));
        for style in ferese_config::panel::WorkspaceStyle::ALL {
            let sample = ferese_theme::workspaces::sample(style, palette, self.font, 22.);
            choices = choices.push(
                widget::button::custom(
                    widget::row![self.label(style.label(), 12.).width(Length::Fill), sample,]
                        .spacing(10)
                        .align_y(Alignment::Center),
                )
                .id(format!("workspace-style:{}", style.key()).into())
                .name(format!("Workspace style: {}", style.label()))
                .padding([8, 10])
                .width(Length::Fill)
                .class(crate::visuals::panel_button(palette, style == selected))
                .on_press_maybe(
                    (style != selected)
                        .then(|| Message::PanelEdit(Action::Set(item.id.clone(), "style".into(), style.key().into()))),
                ),
            );
        }
        choices.into()
    }

    pub(super) fn group_inspector(&self, panel: &Panel, group: &Group, placement: Zone) -> Element<'static, Message> {
        let palette = crate::visuals::Palette::from_resolved(&self.resolved.presented);
        let id = group.id.clone();
        let mut rows = widget::column([])
            .spacing(12)
            .push(self.inspector_heading(
                &crate::panel_controls::group_name(&group.id.0),
                "Group arrangement & spacing",
            ))
            .push(
                widget::column([])
                    .spacing(8)
                    .push(self.label("Placement", 13.))
                    .push(crate::visuals::panel_select(
                        cosmic::iced::widget::pick_list(Zone::ALL.to_vec(), Some(placement), move |zone| {
                            Message::PanelEdit(Action::MoveGroup(id.clone(), zone))
                        })
                        .font(self.font)
                        .text_size(13)
                        .padding([8, 10])
                        .width(Length::Fill),
                        palette,
                    )),
            );
        rows = rows.push(self.panel_divider());
        let mut spacing = widget::column([]).spacing(12);
        for (name, label, value, maximum) in [
            ("spacing", "Item spacing", group.spacing, 64.),
            ("padding_vertical", "Vertical padding", f32::from(group.padding[0]), 32.),
            (
                "padding_horizontal",
                "Horizontal padding",
                f32::from(group.padding[1]),
                32.,
            ),
            ("island_padding", "Island padding", group.island_padding, 32.),
        ] {
            if panel.background != ferese_config::BarLayout::Islands && name == "island_padding" {
                continue;
            }
            let change = |value: f32| {
                let value = if name.starts_with("padding_") {
                    serde_json::json!(value as u16)
                } else {
                    serde_json::json!(value)
                };
                Message::PanelEdit(Action::SetGroup(group.id.clone(), name.into(), value))
            };
            spacing = spacing.push(
                widget::row([])
                    .spacing(8)
                    .align_y(Alignment::Center)
                    .push(self.label(label, 13.).width(Length::Fill))
                    .push(
                        widget::row([])
                            .spacing(4)
                            .align_y(Alignment::Center)
                            .push(self.settings_icon_button(
                                &format!("Decrease {label}"),
                                "M5 12h14",
                                (value > 0.).then(|| change((value - 1.).max(0.))),
                            ))
                            .push(
                                self.label(format!("{value} px"), 12.)
                                    .width(48)
                                    .align_x(Alignment::Center),
                            )
                            .push(self.settings_icon_button(
                                &format!("Increase {label}"),
                                "M12 5v14 M5 12h14",
                                (value < maximum).then(|| change((value + 1.).min(maximum))),
                            )),
                    ),
            );
        }
        rows = rows.push(spacing).push(self.panel_divider());
        let index = placement
            .definition(panel)
            .groups
            .iter()
            .position(|candidate| candidate.id == group.id)
            .unwrap();
        rows = rows.push(
            self.inspector_actions(
                (index > 0).then(|| Message::PanelEdit(Action::EarlierGroup(group.id.clone()))),
                (index + 1 < placement.definition(panel).groups.len())
                    .then(|| Message::PanelEdit(Action::LaterGroup(group.id.clone()))),
                "Remove group",
                group
                    .items
                    .is_empty()
                    .then(|| Message::PanelEdit(Action::RemoveGroup(group.id.clone()))),
            ),
        );
        if !group.items.is_empty() {
            rows = rows.push(self.note("Move or remove this group’s items before removing the group."));
        }
        rows.into()
    }

    fn inspector_actions(
        &self,
        earlier: Option<Message>,
        later: Option<Message>,
        remove_label: &str,
        remove: Option<Message>,
    ) -> Element<'static, Message> {
        widget::row([])
            .spacing(8)
            .push(self.settings_icon_button("Up", "M6 15l6-6 6 6", earlier))
            .push(self.settings_icon_button("Down", "M6 9l6 6 6-6", later))
            .push(self.settings_icon_button(
                remove_label,
                "M4 6h16 M9 6V3h6v3 M6 6l1 15h10l1-15 M10 10v7 M14 10v7",
                remove,
            ))
            .into()
    }

    fn panel_field(&self, item: &Item, field: Field) -> Element<'static, Message> {
        let p = crate::visuals::Palette::from_resolved(&self.resolved.presented);
        let id = item.id.clone();
        let name = field.path.rsplit('.').next().unwrap().to_owned();
        let values = serde_json::to_value(item).expect("serializable panel item");
        match field.kind {
            Kind::Choice { default, choices } => {
                let value = values.get(&name).and_then(serde_json::Value::as_str).unwrap_or(default);
                if name == "overflow" {
                    let selected = choices
                        .iter()
                        .find(|(key, _)| *key == value)
                        .map(|&(key, label)| Choice(key, label));
                    return widget::column([])
                        .spacing(8)
                        .push(self.label(field.label, 13.))
                        .push(crate::visuals::panel_select(
                            cosmic::iced::widget::pick_list(
                                choices
                                    .iter()
                                    .map(|&(key, label)| Choice(key, label))
                                    .collect::<Vec<_>>(),
                                selected,
                                move |choice| {
                                    Message::PanelEdit(Action::Set(id.clone(), name.clone(), choice.0.into()))
                                },
                            )
                            .font(self.font)
                            .text_size(13)
                            .padding([8, 10])
                            .width(Length::Fill),
                            p,
                        ))
                        .push(self.note(&field.description))
                        .into();
                }
                let options = widget::row(choices.iter().map(|&(key, label)| {
                    widget::button::custom(self.label(label, 12.))
                        .name(format!("{}: {label}", field.label))
                        .padding([8, 4])
                        .width(Length::Fill)
                        .class(crate::visuals::panel_button(p, value == key))
                        .on_press(Message::PanelEdit(Action::Set(id.clone(), name.clone(), key.into())))
                        .into()
                }))
                .spacing(2)
                .width(Length::Fill);
                let mut rows = widget::column([]).spacing(8).push(self.label(field.label, 13.)).push(
                    widget::container(options)
                        .padding(3)
                        .class(crate::visuals::surface(p.sidebar, 9.)),
                );
                if !field.description.is_empty() {
                    rows = rows.push(self.note(&field.description));
                }
                rows.into()
            }
            Kind::Toggle(default) => {
                let enabled = values
                    .get(&name)
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(default);
                widget::row([])
                    .spacing(12)
                    .align_y(Alignment::Center)
                    .push(self.label(field.label.clone(), 13.).width(Length::Fill))
                    .push(
                        ferese_theme::controls::switch(enabled, p)
                            .name(format!("{}: {}", field.label, if enabled { "on" } else { "off" }))
                            .on_press(Message::PanelEdit(Action::Set(id, name, (!enabled).into()))),
                    )
                    .into()
            }
            _ => unreachable!("panel inspector fields are choices or toggles"),
        }
    }
}
