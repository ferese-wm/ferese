use ferese_config::panel::{Defaults, ItemKind, Panel, Representation};

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

impl App {
    pub(super) fn panel_controls(&self) -> Element<'_, Message> {
        let mut rows = widget::column([]).spacing(8);
        let Some(value) = self.draft.item("panels") else {
            return rows
                .push(self.note("Customize the current items without changing their order. Panel composition can also be edited in config.kdl."))
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
                for (item_index, item) in group.items.iter().enumerate() {
                    let path = format!("panels.0.{zone_name}.groups.{group_index}.items.{item_index}");
                    rows = rows.push(self.label(format!("{} · {}", item.kind.label(), item.id.0), 14.));
                    rows = rows.push(self.field(Field::new(
                        format!("{path}.visible"),
                        "Show item",
                        "Unavailable services stay hidden.",
                        Kind::Toggle(true),
                    )));
                    rows = rows.push(self.field(Field::new(
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
                    )));
                    let representations = item.kind.representations();
                    if representations.len() > 1 {
                        let choices: &'static [(&'static str, &'static str)] = match representations {
                            [Representation::Wide, Representation::Icon] => &[("wide", "Full"), ("icon", "Icon")],
                            _ => &[("wide", "Full"), ("compact", "Compact"), ("icon", "Icon")],
                        };
                        rows = rows.push(self.field(Field::new(
                            format!("{path}.representation"),
                            "Preferred size",
                            "The panel may use a smaller size to fit.",
                            Kind::Choice {
                                default: "wide",
                                choices,
                            },
                        )));
                    }
                    if matches!(item.kind, ItemKind::Battery { .. }) {
                        rows = rows.push(self.field(Field::new(
                            format!("{path}.percentage"),
                            "Battery percentage",
                            "",
                            Kind::Toggle(true),
                        )));
                    }
                    if matches!(item.kind, ItemKind::FocusedWindow { .. }) {
                        rows = rows.push(self.field(Field::new(
                            format!("{path}.enabled"),
                            "Window title",
                            "",
                            Kind::Toggle(true),
                        )));
                    }
                }
            }
        }
        rows.into()
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
