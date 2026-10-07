use ferese_config::panel::{Defaults, ItemKind, Panel};

use crate::panel_edit::{Action, Destination, Zone};
use crate::{App, Element, Field, Kind, Message, schema::Page, store, widget};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum PanelTab {
    #[default]
    Panels,
    Items,
    Appearance,
}

impl PanelTab {
    const ALL: [Self; 3] = [Self::Panels, Self::Items, Self::Appearance];
    pub fn label(self) -> &'static str {
        match self {
            Self::Panels => "Panels",
            Self::Items => "Items",
            Self::Appearance => "Appearance",
        }
    }
}

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
    pub(super) fn panel_tabs(&self) -> Element<'static, Message> {
        let palette = crate::visuals::Palette::from_resolved(&self.resolved.presented);
        let tabs = widget::row(PanelTab::ALL.into_iter().map(|tab| {
            widget::button::custom(self.label(tab.label(), 12.))
                .padding([7, 16])
                .class(crate::visuals::button_style(palette, self.panel_tab == tab))
                .on_press(Message::PanelTab(tab))
                .into()
        }))
        .spacing(3);
        widget::container(tabs)
            .padding(3)
            .class(crate::visuals::surface(palette.card, 12.))
            .into()
    }

    pub(super) fn panel_controls(&self) -> Element<'_, Message> {
        let palette = crate::visuals::Palette::from_resolved(&self.resolved.presented);
        let mut rows = widget::column([]).spacing(12);
        match self.panel_tab {
            PanelTab::Panels => {
                rows = rows.push(
                    widget::container(
                        widget::row([])
                            .spacing(12)
                            .align_y(cosmic::iced::Alignment::Center)
                            .push(crate::visuals::action_icon("M3 5h18v14H3z M3 9h18", palette.accent))
                            .push(
                                widget::column([])
                                    .spacing(3)
                                    .push(self.label("Top panel", 14.))
                                    .push(self.note("Shown on all displays"))
                                    .width(cosmic::iced::Length::Fill),
                            )
                            .push(self.settings_button(
                                "Edit items",
                                "M4 20l4-1 12-12-3-3L5 16z",
                                Some(Message::PanelTab(PanelTab::Items)),
                                false,
                            )),
                    )
                    .padding(14)
                    .width(cosmic::iced::Length::Fill)
                    .class(crate::visuals::surface(palette.card, 14.)),
                );
                let mut settings = widget::column([]).spacing(1).push(self.label("Panel settings", 14.));
                for field in crate::schema::fields(Page::Bar)
                    .into_iter()
                    .filter(|field| field.path.starts_with("theme.geometry."))
                {
                    settings = settings.push(self.field(field));
                }
                rows = rows.push(
                    widget::container(settings)
                        .padding(12)
                        .class(crate::visuals::surface(palette.card, 14.)),
                );
            }
            PanelTab::Items => {
                let Some(value) = self.draft.item("panels") else {
                    return rows
                        .push(
                            self.note(
                                "Start with the current arrangement. Changes save automatically and can be undone.",
                            ),
                        )
                        .push(self.settings_button(
                            "Edit panel",
                            "M4 20l4-1 12-12-3-3L5 16z",
                            Some(Message::CustomizePanel),
                            false,
                        ))
                        .into();
                };
                let panels: Vec<Panel> = serde_json::from_value(value.clone()).expect("validated panel composition");
                let panel = &panels[0];
                rows = rows
                    .push(self.note("Drag the handles to arrange items. Select an item or group to edit it."))
                    .push(self.panel_editor(panel));
                if let Some(id) = &self.panel_selection {
                    for placement in Zone::ALL {
                        for (group_index, group) in placement.definition(panel).groups.iter().enumerate() {
                            if let Some((item_index, item)) =
                                group.items.iter().enumerate().find(|(_, item)| &item.id == id)
                            {
                                let path =
                                    format!("panels.0.{}.groups.{group_index}.items.{item_index}", placement.key());
                                let destination = Destination {
                                    zone: placement.key(),
                                    group: group.id.clone(),
                                };
                                rows = rows.push(
                                    widget::container(self.item_inspector(panel, item, destination, path))
                                        .id("panel-inspector")
                                        .padding(12)
                                        .width(cosmic::iced::Length::Fill)
                                        .class(crate::visuals::surface(palette.card, 14.)),
                                );
                            }
                        }
                    }
                } else if let Some(id) = &self.panel_group_selection {
                    for placement in Zone::ALL {
                        if let Some(group) = placement.definition(panel).groups.iter().find(|group| &group.id == id) {
                            rows = rows.push(
                                widget::container(self.group_inspector(panel, group, placement))
                                    .id("panel-inspector")
                                    .padding(12)
                                    .width(cosmic::iced::Length::Fill)
                                    .class(crate::visuals::surface(palette.card, 14.)),
                            );
                        }
                    }
                }
            }
            PanelTab::Appearance => {
                let authored = self.draft.item("panels").is_some();
                rows = rows.push(self.field(Field::new(
                    if authored {
                        "panels.0.background"
                    } else {
                        "status.bar_layout"
                    },
                    "Panel background",
                    "",
                    Kind::Choice {
                        default: "continuous",
                        choices: &[("continuous", "Continuous"), ("islands", "Islands")],
                    },
                )));
                if authored {
                    let panels: Vec<Panel> =
                        serde_json::from_value(self.draft.item("panels").unwrap().clone()).unwrap();
                    rows = rows.push(self.label("Group surfaces", 14.));
                    for placement in Zone::ALL {
                        for group in &placement.definition(&panels[0]).groups {
                            rows = rows.push(self.settings_button(
                                &format!("{} · {}", placement, group.id.0),
                                "M4 20l4-1 12-12-3-3L5 16z",
                                Some(Message::PanelGroupSelect(group.id.clone())),
                                false,
                            ));
                        }
                    }
                } else {
                    for field in crate::schema::fields(Page::Bar)
                        .into_iter()
                        .filter(|field| field.path == "status.bar_island_padding")
                    {
                        rows = rows.push(self.field(field));
                    }
                }
            }
        }
        rows.into()
    }

    fn panel_editor(&self, panel: &Panel) -> Element<'static, Message> {
        let palette = crate::visuals::Palette::from_resolved(&self.resolved.presented);
        let mut zones = Vec::new();
        for placement in Zone::ALL {
            let zone = placement.definition(panel);
            let mut groups = widget::column([]).spacing(8).push(
                widget::row([])
                    .align_y(cosmic::iced::Alignment::Center)
                    .push(self.label(placement.to_string(), 14.).width(cosmic::iced::Length::Fill))
                    .push(self.settings_icon_button(
                        "Add group",
                        "M12 5v14 M5 12h14",
                        Some(Message::PanelEdit(Action::AddGroup(placement))),
                    )),
            );
            for group in &zone.groups {
                let target = Destination {
                    zone: placement.key(),
                    group: group.id.clone(),
                };
                let mut controls = widget::column([]).spacing(8).push(self.settings_button(
                    &group.id.0,
                    "M6 9l6 6 6-6",
                    Some(Message::PanelGroupSelect(group.id.clone())),
                    self.panel_group_selection.as_ref() == Some(&group.id),
                ));
                let chips = group
                    .items
                    .iter()
                    .map(|item| {
                        let grip = widget::container(crate::visuals::action_icon(
                            "M9 5h.01 M15 5h.01 M9 12h.01 M15 12h.01 M9 19h.01 M15 19h.01",
                            palette.muted,
                        ))
                        .padding([4, 2])
                        .id(crate::panel_drag::grip_id(&item.id));
                        let select = widget::button::custom(self.label(item.kind.label(), 12.))
                            .name(format!("Edit {} · {}", item.kind.label(), item.id.0))
                            .padding([5, 6])
                            .class(crate::visuals::button_style(
                                palette,
                                self.panel_selection.as_ref() == Some(&item.id),
                            ))
                            .on_press(Message::PanelSelect(item.id.clone()));
                        widget::container(
                            widget::row([])
                                .align_y(cosmic::iced::Alignment::Center)
                                .push(widget::mouse_area(grip).interaction(cosmic::iced::mouse::Interaction::Grab))
                                .push(select),
                        )
                        .id(crate::panel_drag::item_id(&item.id))
                        .padding(2)
                        .class(crate::visuals::surface(palette.sidebar, 8.))
                        .into()
                    })
                    .collect();
                controls = controls.push(widget::flex_row(chips).width(cosmic::iced::Length::Fill).spacing(5));
                controls = controls.push(ferese_theme::controls::select(
                    cosmic::iced::widget::pick_list(CATALOG.to_vec(), None::<CatalogItem>, move |kind| {
                        Message::PanelEdit(Action::Add(kind.0, target.clone()))
                    })
                    .placeholder("Add item")
                    .font(self.font)
                    .text_size(12)
                    .width(cosmic::iced::Length::Fill),
                    palette,
                ));
                groups = groups.push(
                    widget::container(controls)
                        .id(crate::panel_drag::group_id(&group.id))
                        .padding(10)
                        .width(cosmic::iced::Length::Fill)
                        .class(crate::visuals::surface(palette.card, 10.)),
                );
            }
            if zone.groups.is_empty() {
                groups = groups.push(
                    widget::container(self.note("Drop an item here or add a group."))
                        .id(crate::panel_drag::empty_zone_id(placement))
                        .width(cosmic::iced::Length::Fill)
                        .height(72)
                        .padding(10),
                );
            }
            zones.push(
                widget::container(groups)
                    .padding(10)
                    .width(cosmic::iced::Length::Fill)
                    .class(crate::visuals::surface(palette.sidebar, 12.))
                    .into(),
            );
        }
        let editor = widget::flex_row(zones)
            .width(cosmic::iced::Length::Fill)
            .min_item_width(220.)
            .spacing(8)
            .into();
        crate::panel_drag::frame(editor, panel, palette.accent)
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

    #[test]
    fn editor_zones_wrap_without_putting_groups_outside_the_available_width() {
        use cosmic::Application;
        use cosmic::iced::advanced::{Layout, layout, renderer::Headless, widget as advanced};
        use cosmic::iced::{Font, Pixels, Rectangle, Size};
        struct Bounds {
            ids: Vec<advanced::Id>,
            groups: Vec<Rectangle>,
        }
        impl advanced::Operation for Bounds {
            fn traverse(&mut self, children: &mut dyn FnMut(&mut dyn advanced::Operation)) {
                children(self);
            }
            fn container(&mut self, id: Option<&advanced::Id>, bounds: Rectangle) {
                if id.is_some_and(|id| self.ids.contains(id)) {
                    self.groups.push(bounds);
                }
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
        let app = App::init(
            crate::Core::default(),
            ("/unused/editor.kdl".into(), store::Snapshot::parse(String::new()), None),
        )
        .0;
        let panel = Panel::from_defaults(&Defaults::default());
        for width in [280., 560., 900.] {
            let mut view = app.panel_editor(&panel);
            let mut tree = advanced::Tree::new(&view);
            let node = view.as_widget_mut().layout(
                &mut tree,
                &renderer,
                &layout::Limits::new(Size::ZERO, Size::new(width, 2000.)),
            );
            let mut bounds = Bounds {
                ids: Zone::ALL
                    .into_iter()
                    .flat_map(|zone| &zone.definition(&panel).groups)
                    .map(|group| crate::panel_drag::group_id(&group.id))
                    .collect(),
                groups: Vec::new(),
            };
            view.as_widget_mut()
                .operate(&mut tree, Layout::new(&node), &renderer, &mut bounds);
            assert_eq!(bounds.groups.len(), bounds.ids.len());
            for group in bounds.groups {
                assert!(
                    group.x >= -0.1 && group.x + group.width <= width + 0.1,
                    "{group:?} outside {width}"
                );
            }
        }
    }
}
