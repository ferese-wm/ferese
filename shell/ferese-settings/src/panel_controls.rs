use ferese_config::panel::{Defaults, Group, GroupSurface, ItemKind, Panel, Representation};

use crate::panel_edit::{Action, Destination, Zone};
use crate::{App, Element, Field, Kind, Message, schema::Page, store, widget};

pub(super) fn initialize(snapshot: &store::Snapshot) -> Result<store::Edit, String> {
    let panel = Panel::from_defaults(&Defaults {
        bar_layout: serde_json::from_value(
            snapshot
                .item("status.bar_layout")
                .cloned()
                .unwrap_or_else(|| "continuous".into()),
        )
        .map_err(|error| error.to_string())?,
        bar_island_padding: snapshot.number(
            "status.bar_island_padding",
            f64::from(ferese_config::default_bar_island_padding()),
        ) as f32,
        window_title: snapshot.boolean("status.window_title", true),
        battery_percentage: snapshot.boolean("status.battery_percentage", true),
    });
    ferese_config::panel::validate(std::slice::from_ref(&panel))?;
    Ok(store::set(
        "panels",
        serde_json::to_value(vec![panel]).map_err(|error| error.to_string())?,
    ))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct CatalogItem(ItemKind);
impl std::fmt::Display for CatalogItem {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.0.label())
    }
}
const CATALOG: [CatalogItem; 12] = [
    CatalogItem(ItemKind::Overview),
    CatalogItem(ItemKind::Workspaces),
    CatalogItem(ItemKind::FocusedWindow { enabled: true }),
    CatalogItem(ItemKind::Media),
    CatalogItem(ItemKind::QuickSettings),
    CatalogItem(ItemKind::Network),
    CatalogItem(ItemKind::Audio),
    CatalogItem(ItemKind::Recording),
    CatalogItem(ItemKind::Notifications),
    CatalogItem(ItemKind::Battery { percentage: true }),
    CatalogItem(ItemKind::Clock),
    CatalogItem(ItemKind::DisplayMode),
];

#[derive(Clone, Copy, Debug, PartialEq)]
struct Pixels(f32);
impl std::fmt::Display for Pixels {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{} px", self.0)
    }
}

impl App {
    pub(super) fn panel_controls(&self) -> Element<'_, Message> {
        let palette = crate::visuals::Palette::from_resolved(&self.resolved.presented);
        let mut rows = widget::column([]).spacing(8);
        let Some(value) = self.draft.item("panels") else {
            return rows
                .push(self.note("Customize the current items. Changes save automatically and can be undone."))
                .push(widget::button::standard("Customize items").on_press(Message::CustomizePanel))
                .into();
        };
        let panels: Vec<Panel> = serde_json::from_value(value.clone()).expect("validated panel composition");
        rows = rows.push(self.field(Field::new(
            "panels.0.background",
            "Background",
            "",
            Kind::Choice {
                default: "continuous",
                choices: &[("continuous", "Continuous"), ("islands", "Islands")],
            },
        )));
        for placement in Zone::ALL {
            let zone_name = placement.key();
            let zone = placement.definition(&panels[0]);
            rows = rows.push(
                widget::row([])
                    .align_y(cosmic::iced::Alignment::Center)
                    .push(self.label(placement.to_string(), 15.).width(cosmic::iced::Length::Fill))
                    .push(self.settings_button(
                        "Add group",
                        "M12 5v14 M5 12h14",
                        Some(Message::PanelEdit(Action::AddGroup(placement))),
                        false,
                    )),
            );
            for (group_index, group) in zone.groups.iter().enumerate() {
                let destination = Destination {
                    zone: zone_name,
                    group: group.id.clone(),
                };
                let target = destination.clone();
                rows = rows.push(
                    widget::row([])
                        .spacing(8)
                        .align_y(cosmic::iced::Alignment::Center)
                        .push(self.settings_button(
                            &group.id.0,
                            "M6 9l6 6 6-6",
                            Some(Message::PanelGroupSelect(group.id.clone())),
                            self.panel_group_selection.as_ref() == Some(&group.id),
                        ))
                        .push(widget::Space::new().width(cosmic::iced::Length::Fill))
                        .push(self.settings_icon_button(
                            "Move group earlier",
                            "M6 15l6-6 6 6",
                            (group_index > 0).then(|| Message::PanelEdit(Action::EarlierGroup(group.id.clone()))),
                        ))
                        .push(
                            self.settings_icon_button(
                                "Move group later",
                                "M6 9l6 6 6-6",
                                (group_index + 1 < zone.groups.len())
                                    .then(|| Message::PanelEdit(Action::LaterGroup(group.id.clone()))),
                            ),
                        )
                        .push(
                            self.settings_icon_button(
                                if group.items.is_empty() {
                                    "Remove group"
                                } else {
                                    "Move or remove items to remove this group"
                                },
                                "M6 6l12 12 M6 18L18 6",
                                group
                                    .items
                                    .is_empty()
                                    .then(|| Message::PanelEdit(Action::RemoveGroup(group.id.clone()))),
                            ),
                        )
                        .push(ferese_theme::controls::select(
                            cosmic::iced::widget::pick_list(CATALOG.to_vec(), None::<CatalogItem>, move |kind| {
                                Message::PanelEdit(Action::Add(kind.0, target.clone()))
                            })
                            .placeholder("Add item")
                            .font(self.font)
                            .text_size(12),
                            palette,
                        )),
                );
                if self.panel_group_selection.as_ref() == Some(&group.id) {
                    rows = rows.push(self.group_inspector(&panels[0], group, placement));
                }
                for (item_index, item) in group.items.iter().enumerate() {
                    let path = format!("panels.0.{zone_name}.groups.{group_index}.items.{item_index}");
                    let selected = self.panel_selection.as_ref() == Some(&item.id);
                    rows = rows.push(
                        widget::row([])
                            .spacing(5)
                            .align_y(cosmic::iced::Alignment::Center)
                            .push(self.settings_button(
                                &format!("{} · {}", item.kind.label(), item.id.0),
                                "M6 9l6 6 6-6",
                                Some(Message::PanelSelect(item.id.clone())),
                                selected,
                            ))
                            .push(widget::Space::new().width(cosmic::iced::Length::Fill))
                            .push(self.settings_icon_button(
                                "Move earlier",
                                "M6 15l6-6 6 6",
                                (item_index > 0).then(|| Message::PanelEdit(Action::Earlier(item.id.clone()))),
                            ))
                            .push(
                                self.settings_icon_button(
                                    "Move later",
                                    "M6 9l6 6 6-6",
                                    (item_index + 1 < group.items.len())
                                        .then(|| Message::PanelEdit(Action::Later(item.id.clone()))),
                                ),
                            )
                            .push(self.settings_icon_button(
                                "Remove item",
                                "M6 6l12 12 M6 18L18 6",
                                Some(Message::PanelEdit(Action::Remove(item.id.clone()))),
                            )),
                    );
                    if !selected {
                        continue;
                    }
                    let item_id = item.id.clone();
                    rows = rows.push(
                        widget::row([])
                            .spacing(8)
                            .align_y(cosmic::iced::Alignment::Center)
                            .push(self.label("Placement", 13.).width(cosmic::iced::Length::Fill))
                            .push(ferese_theme::controls::select(
                                cosmic::iced::widget::pick_list(
                                    crate::panel_edit::destinations(&panels[0]),
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
                }
            }
        }
        rows.into()
    }

    fn group_inspector(&self, panel: &Panel, group: &Group, placement: Zone) -> Element<'_, Message> {
        let palette = crate::visuals::Palette::from_resolved(&self.resolved.presented);
        let id = group.id.clone();
        let path = crate::panel_edit::group_record_path(panel, &id).expect("existing group");
        let mut rows = widget::column([]).spacing(4).push(
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

    pub(super) fn bar_fields(&self) -> Vec<Field> {
        let mut fields = crate::schema::fields(Page::Bar);
        if self.draft.item("panels").is_some() {
            fields.retain(|field| {
                !matches!(
                    field.path.as_str(),
                    "status.bar_layout"
                        | "status.bar_island_padding"
                        | "status.window_title"
                        | "status.battery_percentage"
                )
            });
        }
        fields
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn customization_starts_from_current_settings_and_retains_other_fields() {
        let mut snapshot = store::Snapshot::parse("// keep\nstatus { bar-layout \"islands\"; bar-island-padding 9.5; battery-percentage #false; window-title #false; }\nanimations { speed 0.8; }\n".into()).unwrap();
        snapshot.edit(&initialize(&snapshot).unwrap()).unwrap();
        let snapshot = store::Snapshot::parse(snapshot.source).unwrap();
        let panels: Vec<Panel> = serde_json::from_value(snapshot.item("panels").unwrap().clone()).unwrap();
        assert_eq!(panels[0].background, ferese_config::BarLayout::Islands);
        assert_eq!(panels[0].end.groups[0].island_padding, 9.5);
        assert_eq!(
            panels[0].center.groups[0].items[0].kind,
            ItemKind::FocusedWindow { enabled: false }
        );
        assert_eq!(
            panels[0].end.groups[0].items[6].kind,
            ItemKind::Battery { percentage: false }
        );
        assert_eq!(snapshot.number("animations.speed", 1.0), 0.8);
        assert!(snapshot.source.contains("// keep"));
        let id = snapshot.string("panels.0.end.groups.0.items.2.id", "");
        let mut changed = snapshot;
        changed
            .edit(&store::set("panels.0.end.groups.0.items.2.overflow", "always"))
            .unwrap();
        let changed = store::Snapshot::parse(changed.source).unwrap();
        assert_eq!(changed.string("panels.0.end.groups.0.items.2.id", ""), id);
        assert_eq!(changed.string("panels.0.end.groups.0.items.2.overflow", ""), "always");
    }
}
