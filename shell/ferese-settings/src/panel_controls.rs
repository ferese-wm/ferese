use ferese_config::panel::{Defaults, ItemKind, Panel, Representation};

use crate::panel_edit::{Action, Destination};
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
        for (zone_name, zone) in [
            ("start", &panels[0].start),
            ("center", &panels[0].center),
            ("end", &panels[0].end),
        ] {
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
                        .push(
                            self.label(destination.to_string(), 14.)
                                .width(cosmic::iced::Length::Fill),
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
