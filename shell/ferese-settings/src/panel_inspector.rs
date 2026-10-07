use crate::panel_edit::{Action, Destination, Zone};
use crate::{App, Element, Field, Kind, Message, store, widget};
use ferese_config::panel::{Group, GroupSurface, Item, ItemKind, Panel, Representation};

#[derive(Clone, Copy, Debug, PartialEq)]
struct Pixels(f32);
impl std::fmt::Display for Pixels {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{} px", self.0)
    }
}

impl App {
    pub(super) fn item_inspector(
        &self,
        panel: &Panel,
        item: &Item,
        destination: Destination,
        path: String,
    ) -> Element<'static, Message> {
        let palette = crate::visuals::Palette::from_resolved(&self.resolved.presented);
        let mut rows = widget::column([])
            .spacing(4)
            .push(self.label(format!("{} · {}", item.kind.label(), item.id.0), 14.));
        let item_id = item.id.clone();
        rows = rows.push(
            widget::row([])
                .spacing(8)
                .align_y(cosmic::iced::Alignment::Center)
                .push(self.label("Placement", 13.).width(cosmic::iced::Length::Fill))
                .push(ferese_theme::controls::select(
                    cosmic::iced::widget::pick_list(
                        crate::panel_edit::destinations(panel),
                        Some(destination.clone()),
                        move |destination| Message::PanelEdit(Action::Move(item_id.clone(), destination)),
                    )
                    .font(self.font)
                    .text_size(12),
                    palette,
                )),
        );
        rows = rows.push(self.panel_field(
            &item.id,
            Field::new(
                format!("{path}.visible"),
                "Show item",
                "Unavailable services stay hidden.",
                Kind::Toggle(true),
            ),
        ));
        rows = rows.push(self.panel_field(
            &item.id,
            Field::new(
                format!("{path}.overflow"),
                "When space is limited",
                "Keep visible moves to overflow only if the panel cannot fit its minimum controls.",
                Kind::Choice {
                    default: "auto",
                    choices: &[
                        ("never", "Keep visible"),
                        ("auto", "Overflow when needed"),
                        ("always", "Always in overflow"),
                    ],
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
                &item.id,
                Field::new(
                    format!("{path}.representation"),
                    "Preferred size",
                    "The panel may use a smaller size to fit.",
                    Kind::Choice {
                        default: "wide",
                        choices,
                    },
                ),
            ));
        }
        if matches!(item.kind, ItemKind::Battery { .. }) {
            rows = rows.push(self.panel_field(
                &item.id,
                Field::new(
                    format!("{path}.percentage"),
                    "Battery percentage",
                    "",
                    Kind::Toggle(true),
                ),
            ));
        }
        if matches!(item.kind, ItemKind::FocusedWindow { .. }) {
            rows = rows.push(self.panel_field(
                &item.id,
                Field::new(format!("{path}.enabled"), "Window title", "", Kind::Toggle(true)),
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
        rows = rows.push(
            widget::row([])
                .spacing(6)
                .push(self.settings_button(
                    "Move earlier",
                    "M6 15l6-6 6 6",
                    (index > 0).then(|| Message::PanelEdit(Action::Earlier(item.id.clone()))),
                    false,
                ))
                .push(self.settings_button(
                    "Move later",
                    "M6 9l6 6 6-6",
                    (index + 1 < group.items.len()).then(|| Message::PanelEdit(Action::Later(item.id.clone()))),
                    false,
                ))
                .push(self.settings_button(
                    "Remove item",
                    "M6 6l12 12 M6 18L18 6",
                    Some(Message::PanelEdit(Action::Remove(item.id.clone()))),
                    false,
                )),
        );
        rows.into()
    }

    pub(super) fn group_inspector(&self, panel: &Panel, group: &Group, placement: Zone) -> Element<'static, Message> {
        let palette = crate::visuals::Palette::from_resolved(&self.resolved.presented);
        let id = group.id.clone();
        let path = crate::panel_edit::group_record_path(panel, &id).expect("existing group");
        let mut rows = widget::column([])
            .spacing(4)
            .push(self.label(format!("Group · {}", group.id.0), 14.))
            .push(
                widget::row([])
                    .align_y(cosmic::iced::Alignment::Center)
                    .push(self.label("Placement", 13.).width(cosmic::iced::Length::Fill))
                    .push(ferese_theme::controls::select(
                        cosmic::iced::widget::pick_list(Zone::ALL.to_vec(), Some(placement), move |zone| {
                            Message::PanelEdit(Action::MoveGroup(id.clone(), zone))
                        })
                        .font(self.font)
                        .text_size(12),
                        palette,
                    )),
            );
        let id = group.id.clone();
        rows = rows.push(
            self.field(Field::new(
                format!("{path}.surface"),
                "Group surface",
                "",
                Kind::Choice {
                    default: "inset",
                    choices: &[("none", "None"), ("inset", "Inset"), ("island", "Island")],
                },
            ))
            .map(move |message| match message {
                Message::Change(store::Edit::Set(_, value)) => {
                    Message::PanelEdit(Action::SetGroup(id.clone(), "surface".into(), value))
                }
                other => other,
            }),
        );
        // Pick lists save once per choice and resolve the group ID at that time.
        // They do not leave index-based slider drafts behind when groups move.
        for (name, label, value, maximum) in [
            ("spacing", "Item spacing", group.spacing, 64),
            ("padding_vertical", "Vertical padding", f32::from(group.padding[0]), 32),
            (
                "padding_horizontal",
                "Horizontal padding",
                f32::from(group.padding[1]),
                32,
            ),
            ("island_padding", "Island padding", group.island_padding, 32),
        ] {
            if group.surface == GroupSurface::None && name != "spacing"
                || group.surface != GroupSurface::Island && name == "island_padding"
            {
                continue;
            }
            let mut options: Vec<_> = (0..=maximum).map(|number| Pixels(number as f32)).collect();
            if !options.contains(&Pixels(value)) {
                options.push(Pixels(value));
            }
            let id = group.id.clone();
            rows = rows.push(
                widget::row([])
                    .align_y(cosmic::iced::Alignment::Center)
                    .push(self.label(label, 13.).width(cosmic::iced::Length::Fill))
                    .push(ferese_theme::controls::select(
                        cosmic::iced::widget::pick_list(options, Some(Pixels(value)), move |value| {
                            let value = if name.starts_with("padding_") {
                                serde_json::json!(value.0 as u16)
                            } else {
                                serde_json::json!(value.0)
                            };
                            Message::PanelEdit(Action::SetGroup(id.clone(), name.into(), value))
                        })
                        .font(self.font)
                        .text_size(12),
                        palette,
                    )),
            );
        }
        let index = placement
            .definition(panel)
            .groups
            .iter()
            .position(|candidate| candidate.id == group.id)
            .unwrap();
        rows = rows.push(
            widget::row([])
                .spacing(6)
                .push(self.settings_button(
                    "Move earlier",
                    "M6 15l6-6 6 6",
                    (index > 0).then(|| Message::PanelEdit(Action::EarlierGroup(group.id.clone()))),
                    false,
                ))
                .push(
                    self.settings_button(
                        "Move later",
                        "M6 9l6 6 6-6",
                        (index + 1 < placement.definition(panel).groups.len())
                            .then(|| Message::PanelEdit(Action::LaterGroup(group.id.clone()))),
                        false,
                    ),
                )
                .push(
                    self.settings_button(
                        "Remove group",
                        "M6 6l12 12 M6 18L18 6",
                        group
                            .items
                            .is_empty()
                            .then(|| Message::PanelEdit(Action::RemoveGroup(group.id.clone()))),
                        false,
                    ),
                ),
        );
        if !group.items.is_empty() {
            rows = rows.push(self.note("Move or remove the items before removing this group."));
        }
        rows.into()
    }

    fn panel_field(&self, id: &ferese_config::panel::ItemId, field: Field) -> Element<'static, Message> {
        let id = id.clone();
        let name = field.path.rsplit('.').next().unwrap().to_owned();
        self.field(field).map(move |message| match message {
            Message::Change(store::Edit::Set(_, value)) => {
                Message::PanelEdit(Action::Set(id.clone(), name.clone(), value))
            }
            other => other,
        })
    }
}
