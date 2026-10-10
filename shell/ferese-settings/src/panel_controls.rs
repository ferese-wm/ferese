use ferese_config::panel::{Edge, ItemKind, Panel};

use crate::panel_edit::{Action, Destination, Zone};
use crate::{App, Element, Message, schema::Page, store, widget};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum PanelPage {
    #[default]
    Panels,
    Arrange,
}

pub(super) fn initialize(_snapshot: &store::Snapshot) -> Result<store::Edit, String> {
    let panel = Panel::default();
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
    CatalogItem(ItemKind::Workspaces {
        style: ferese_config::panel::WorkspaceStyle::Dots,
    }),
    CatalogItem(ItemKind::FocusedWindow),
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
    pub(super) fn panel_navigation(&self) -> Element<'static, Message> {
        self.settings_button(
            "Back to Panels",
            "M15 18l-6-6 6-6",
            Some(Message::PanelPage(PanelPage::Panels)),
            false,
        )
    }

    pub(super) fn panel_controls(&self) -> Element<'_, Message> {
        let palette = crate::visuals::Palette::from_resolved(&self.resolved.presented);
        let edge = self.preview_panel().map_or(Edge::Top, |panel| panel.edge);
        let mut rows = widget::column([]).spacing(12);
        match self.panel_page {
            PanelPage::Panels => {
                rows = rows.push(
                    widget::container(
                        widget::row([])
                            .spacing(12)
                            .align_y(cosmic::iced::Alignment::Center)
                            .push(crate::visuals::action_icon(
                                if edge == Edge::Top {
                                    "M3 5h18v14H3z M3 9h18"
                                } else {
                                    "M3 5h18v14H3z M3 15h18"
                                },
                                palette.accent,
                            ))
                            .push(
                                widget::column([])
                                    .spacing(3)
                                    .push(self.panel_heading(
                                        if edge == Edge::Top { "Top panel" } else { "Bottom panel" },
                                        14.,
                                    ))
                                    .push(self.note("Shown on all displays"))
                                    .width(cosmic::iced::Length::Fill),
                            )
                            .push(self.settings_button(
                                "Edit panel",
                                "M4 20l4-1 12-12-3-3L5 16z",
                                Some(Message::PanelPage(PanelPage::Arrange)),
                                false,
                            ))
                            .push(
                                self.settings_icon_button(
                                    "Remove custom panel",
                                    "M4 7h16 M9 7V4h6v3 M6 7l1 13h10l1-13 M10 11v5 M14 11v5",
                                    self.draft
                                        .item("panels")
                                        .is_some()
                                        .then(|| Message::Change(store::Edit::Unset("panels".into()))),
                                ),
                            ),
                    )
                    .padding(14)
                    .width(cosmic::iced::Length::Fill)
                    .class(crate::visuals::surface(palette.card, 14.)),
                );
                let preset = self.preview_panel().ok().and_then(|panel| panel.preset());
                let preset_key = match preset {
                    Some(ferese_config::PanelPreset::Continuous) => "continuous",
                    Some(ferese_config::PanelPreset::Islands) => "islands",
                    None => "custom",
                };
                let position = widget::row([Edge::Top, Edge::Bottom].into_iter().map(|position| {
                    let label = if position == Edge::Top { "Top" } else { "Bottom" };
                    widget::button::custom(self.label(label, 12.))
                        .name(format!("Panel position: {label}"))
                        .padding([8, 10])
                        .width(cosmic::iced::Length::Fill)
                        .class(crate::visuals::panel_button(palette, edge == position))
                        .on_press_maybe((edge != position).then_some(Message::PanelEdit(Action::SetEdge(position))))
                        .into()
                }))
                .spacing(2);
                rows = rows.push(
                    widget::container(
                        self.panel_appearance_row(
                            "Position",
                            "Choose the screen edge for the panel.",
                            widget::container(position)
                                .padding(3)
                                .class(crate::visuals::surface(palette.sidebar, 9.))
                                .into(),
                        ),
                    )
                    .padding(16)
                    .class(crate::visuals::surface(palette.card, 14.)),
                );
                let appearance = widget::column([])
                    .spacing(16)
                    .push(self.panel_heading("Appearance", 15.))
                    .push(self.panel_appearance_row(
                        "Background",
                        "Apply a preset, then customize each group.",
                        self.panel_surface_choices(
                            preset_key,
                            &[("continuous", "Continuous"), ("islands", "Islands")],
                            move |key| {
                                Message::PanelEdit(Action::ApplyPreset(if key == "islands" {
                                    ferese_config::PanelPreset::Islands
                                } else {
                                    ferese_config::PanelPreset::Continuous
                                }))
                            },
                        ),
                    ))
                    .push(
                        self.panel_appearance_row(
                            "Default group surface",
                            "Groups can override this in their inspector.",
                            self.panel_surface_choices(
                                self.preview_panel()
                                    .map_or(ferese_config::panel::GroupSurface::None, |panel| panel.group_surface)
                                    .key(),
                                &[("none", "None"), ("inset", "Inset"), ("island", "Island")],
                                |key| {
                                    Message::PanelEdit(Action::SetDefaultSurface(match key {
                                        "inset" => ferese_config::panel::GroupSurface::Inset,
                                        "island" => ferese_config::panel::GroupSurface::Island,
                                        _ => ferese_config::panel::GroupSurface::None,
                                    }))
                                },
                            ),
                        ),
                    )
                    .push(self.panel_divider())
                    .push(self.panel_opacity_controls())
                    .push(self.panel_divider())
                    .push(
                        self.panel_appearance_row(
                            "Group borders",
                            "Outline each group of items.",
                            widget::container(
                                ferese_theme::controls::switch(self.draft.boolean("panels.0.border", false), palette)
                                    .name("Group borders")
                                    .on_press(Message::PanelEdit(Action::SetBorder(
                                        !self.draft.boolean("panels.0.border", false),
                                    ))),
                            )
                            .align_right(cosmic::iced::Length::Fill)
                            .into(),
                        ),
                    )
                    .push(self.panel_divider())
                    .push(self.panel_radius_controls());
                rows = rows.push(
                    widget::container(appearance)
                        .id("panel-appearance-settings")
                        .padding(16)
                        .width(cosmic::iced::Length::Fill)
                        .class(crate::visuals::surface(palette.card, 14.)),
                );
                let mut settings = widget::column([])
                    .spacing(4)
                    .push(widget::container(self.panel_heading("Size & spacing", 15.)).padding([4, 10]));
                for field in crate::schema::fields(Page::Bar)
                    .into_iter()
                    .filter(|field| field.path.starts_with("panels.0.geometry."))
                {
                    let mut field = field;
                    if let crate::schema::Kind::Range { default, .. } = &mut field.kind {
                        let geometry =
                            serde_json::to_value(self.preview_panel().map(|panel| panel.geometry).unwrap_or_default())
                                .unwrap();
                        *default = geometry[field.path.rsplit('.').next().unwrap()].as_f64().unwrap();
                    }
                    settings = settings.push(self.field(field));
                }
                rows = rows.push(
                    widget::container(settings)
                        .padding(12)
                        .class(crate::visuals::surface(palette.card, 14.)),
                );
            }
            PanelPage::Arrange => return self.panel_workspace(),
        }
        rows.into()
    }

    fn panel_appearance_row(
        &self,
        label: &str,
        description: &str,
        control: Element<'static, Message>,
    ) -> Element<'static, Message> {
        widget::row([])
            .spacing(24)
            .align_y(cosmic::iced::Alignment::Center)
            .push(
                widget::column([])
                    .spacing(4)
                    .push(self.label(label.to_owned(), 13.))
                    .push(self.note(description))
                    .width(cosmic::iced::Length::Fill),
            )
            .push(widget::container(control).width(280))
            .width(cosmic::iced::Length::Fill)
            .into()
    }

    fn panel_opacity_controls(&self) -> Element<'static, Message> {
        use cosmic::iced::{Alignment, Length};
        let palette = crate::visuals::Palette::from_resolved(&self.resolved.presented);
        let tokens = &self.resolved.presented.tokens;
        let inherited = if tokens.material.style == "translucent" {
            tokens.material.opacity
        } else {
            1.
        };
        let value = self
            .ranges
            .get("panels.0.background_opacity")
            .copied()
            .unwrap_or_else(|| self.draft.number("panels.0.background_opacity", inherited) * 100.);
        let custom = self.draft.item("panels.0.background_opacity").is_some();
        let field = crate::Field::new(
            "panels.0.background_opacity",
            "Background opacity",
            "",
            crate::Kind::Range {
                default: inherited * 100.,
                min: 0.,
                max: 100.,
                step: 1.,
                suffix: "%",
                integer: false,
            },
        );
        let release = field.clone();
        let control = widget::row([])
            .spacing(10)
            .align_y(Alignment::Center)
            .push(
                crate::slider(0. ..=100., value, move |value| Message::Range(field.clone(), value))
                    .step(1.)
                    .class(ferese_theme::menus::slider(palette.accent, 1.))
                    .on_release(Message::Release(release))
                    .width(Length::Fill),
            )
            .push(
                widget::container(self.label(format!("{value:.0}%"), 12.).align_x(Alignment::Center))
                    .padding([6, 0])
                    .width(48),
            )
            .push(self.settings_icon_button(
                "Use shell opacity",
                "M3 12a9 9 0 1 0 3-6 M3 3v6h6",
                custom.then_some(Message::PanelEdit(Action::ClearOpacity)),
            ));
        self.panel_appearance_row(
            "Background opacity",
            if custom {
                "Panel only. Text and icons stay opaque."
            } else {
                "Using shell opacity. Text and icons stay opaque."
            },
            control.into(),
        )
    }

    fn panel_radius_controls(&self) -> Element<'static, Message> {
        use cosmic::iced::{Alignment, Length};
        let palette = crate::visuals::Palette::from_resolved(&self.resolved.presented);
        let shell_radius = self.resolved.presented.tokens.geometry.shell_radius as f32;
        let custom = self
            .draft
            .item("panels.0.corner_radius")
            .and_then(|value| serde_json::from_value::<ferese_config::panel::CornerRadii>(value.clone()).ok());
        let radii = custom.unwrap_or_else(|| ferese_config::panel::CornerRadii::uniform(shell_radius));
        let modes = widget::row([("Shell default", false), ("Custom", true)].map(|(label, selected)| {
            widget::button::custom(self.label(label, 12.).align_x(Alignment::Center))
                .name(format!("Corner radius: {label}"))
                .padding([8, 12])
                .width(Length::Fill)
                .class(crate::visuals::panel_button(palette, custom.is_some() == selected))
                .on_press_maybe((custom.is_some() != selected).then(|| {
                    Message::PanelEdit(if selected {
                        Action::SetRadius(format!("{shell_radius}px"))
                    } else {
                        Action::ClearRadius
                    })
                }))
                .into()
        }))
        .spacing(2);
        let modes = widget::container(modes)
            .padding(3)
            .class(crate::visuals::surface(palette.sidebar, 9.));
        let mut corners = Vec::new();
        // Match the physical corners: top row first, then bottom-left / bottom-right.
        for (index, label, icon) in [
            (0, "Top left", "M5 19V9a4 4 0 0 1 4-4h10"),
            (1, "Top right", "M5 5h10a4 4 0 0 1 4 4v10"),
            (3, "Bottom left", "M5 5v10a4 4 0 0 0 4 4h10"),
            (2, "Bottom right", "M5 19h10a4 4 0 0 0 4-4V5"),
        ] {
            let value = radii.0[index];
            let tint = if custom.is_some() { palette.text } else { palette.muted };
            corners.push(
                widget::container(
                    widget::row([])
                        .spacing(8)
                        .align_y(Alignment::Center)
                        .push(crate::visuals::action_icon(icon, palette.muted))
                        .push(self.label(label, 12.).width(Length::Fill))
                        .push(
                            self.settings_icon_button(
                                &format!("Decrease {label} radius"),
                                "M5 12h14",
                                (custom.is_some() && value > 0.)
                                    .then_some(Message::PanelEdit(Action::SetCorner(index, (value - 1.).max(0.)))),
                            ),
                        )
                        .push(
                            self.label(format!("{value} px"), 12.)
                                .width(52)
                                .align_x(Alignment::Center)
                                .class(cosmic::theme::Text::Color(tint)),
                        )
                        .push(
                            self.settings_icon_button(
                                &format!("Increase {label} radius"),
                                "M12 5v14 M5 12h14",
                                custom
                                    .is_some()
                                    .then_some(Message::PanelEdit(Action::SetCorner(index, value + 1.))),
                            ),
                        ),
                )
                .padding([8, 10])
                .width(Length::Fill)
                .class(crate::visuals::surface(
                    ferese_theme::mix(palette.card, palette.sidebar, 0.5),
                    8.,
                ))
                .into(),
            );
        }
        let bottom_corners = corners.split_off(2);
        let mut content = widget::column([])
            .spacing(12)
            .push(self.panel_appearance_row("Corner radius", "Set the rounding of each corner.", modes.into()))
            .push(
                widget::column([])
                    .spacing(8)
                    .push(
                        widget::flex_row(corners)
                            .min_item_width(220.)
                            .spacing(8)
                            .width(Length::Fill),
                    )
                    .push(
                        widget::flex_row(bottom_corners)
                            .min_item_width(220.)
                            .spacing(8)
                            .width(Length::Fill),
                    ),
            );
        if self.preview_panel().is_ok_and(|panel| panel.geometry.edge_margin == 0) {
            let bottom = self.preview_panel().is_ok_and(|panel| panel.edge == Edge::Bottom);
            content = content.push(self.note(if bottom {
                "Bottom corners stay square while the panel touches the bottom edge."
            } else {
                "Top corners stay square while the panel touches the top edge."
            }));
        }
        content.into()
    }

    pub(super) fn panel_workspace(&self) -> Element<'_, Message> {
        let panel = match self.preview_panel() {
            Ok(panel) => panel,
            Err(error) => return self.note(&error),
        };
        let palette = crate::visuals::Palette::from_resolved(&self.resolved.presented);
        cosmic::iced::widget::responsive(move |size| {
            let editor = widget::column([])
                .spacing(12)
                .push(self.panel_heading("Arrangement", 15.))
                .push(self.note("Drag items to reorder. Select an item or group to edit."))
                .push(
                    widget::scrollable(self.panel_editor(&panel))
                        .id(widget::Id::new("settings-content"))
                        .direction(cosmic::iced::widget::scrollable::Direction::Vertical(
                            cosmic::iced::widget::scrollable::Scrollbar::new()
                                .width(8)
                                .scroller_width(3),
                        ))
                        .height(cosmic::iced::Length::Fill),
                );
            let inspector = widget::scrollable(self.panel_inspector(&panel))
                .id(widget::Id::new("panel-inspector-scroll"))
                .direction(cosmic::iced::widget::scrollable::Direction::Vertical(
                    cosmic::iced::widget::scrollable::Scrollbar::new()
                        .width(8)
                        .scroller_width(3),
                ))
                .height(cosmic::iced::Length::Fill);
            widget::row([])
                .spacing(16)
                .push(
                    widget::container(editor)
                        .width(cosmic::iced::Length::Fill)
                        .height(cosmic::iced::Length::Fill),
                )
                .push(
                    widget::container(inspector)
                        .width((size.width * 0.48).clamp(220., 320.))
                        .height(cosmic::iced::Length::Fill)
                        .class(crate::visuals::surface(palette.card, 12.)),
                )
                .height(cosmic::iced::Length::Fill)
                .into()
        })
        .height(cosmic::iced::Length::Fill)
        .into()
    }

    fn panel_inspector(&self, panel: &Panel) -> Element<'static, Message> {
        let palette = crate::visuals::Palette::from_resolved(&self.resolved.presented);
        let mut inspector = None;
        for placement in Zone::ALL {
            for (group_index, group) in placement.definition(panel).groups.iter().enumerate() {
                if self.panel_group_selection.as_ref() == Some(&group.id) {
                    inspector = Some(self.group_inspector(panel, group, placement));
                }
                if let Some((item_index, item)) = group
                    .items
                    .iter()
                    .enumerate()
                    .find(|(_, item)| self.panel_selection.as_ref() == Some(&item.id))
                {
                    inspector = Some(self.item_inspector(
                        panel,
                        item,
                        Destination {
                            zone: placement.key(),
                            group: group.id.clone(),
                        },
                        format!("panels.0.{}.groups.{group_index}.items.{item_index}", placement.key()),
                    ));
                }
            }
        }
        widget::container(inspector.unwrap_or_else(|| {
            widget::container(
                widget::column([])
                    .spacing(12)
                    .align_x(cosmic::iced::Alignment::Center)
                    .push(crate::visuals::action_icon(
                        "M4 4h16v16H4z M14 4v16 M7 8h4 M7 12h4",
                        palette.muted,
                    ))
                    .push(self.panel_heading("Make it your own", 15.))
                    .push(self.note(
                        "Select an item to adjust how it appears, or a group to edit its placement and spacing.",
                    )),
            )
            .padding([40, 8])
            .into()
        }))
        .id("panel-inspector")
        .padding(16)
        .width(cosmic::iced::Length::Fill)
        .class(crate::visuals::surface(palette.card, 14.))
        .into()
    }

    fn panel_surface_choices(
        &self,
        selected: &str,
        options: &'static [(&'static str, &'static str)],
        message: impl Fn(&'static str) -> Message,
    ) -> Element<'static, Message> {
        use cosmic::iced::{Alignment, Length};
        let palette = crate::visuals::Palette::from_resolved(&self.resolved.presented);
        let options = widget::row(options.iter().map(|&(key, label)| {
            widget::button::custom(
                widget::row([])
                    .spacing(8)
                    .align_y(Alignment::Center)
                    .push(crate::visuals::action_icon(
                        match key {
                            "continuous" => "M3 7h18v10H3z",
                            "none" => "M4 4l16 16 M4 20L20 4",
                            "inset" => "M3 5h18v14H3z M7 9h10v6H7z",
                            _ => "M2 7h5v10H2z M10 7h4v10h-4z M17 7h5v10h-5z",
                        },
                        palette.muted,
                    ))
                    .push(self.label(label, 12.)),
            )
            .name(format!("Background: {label}"))
            .padding([8, 10])
            .width(Length::Fill)
            .class(crate::visuals::panel_button(palette, selected == key))
            .on_press(message(key))
            .into()
        }))
        .spacing(2);
        widget::container(options)
            .padding(3)
            .width(Length::Fill)
            .class(crate::visuals::surface(palette.sidebar, 9.))
            .into()
    }

    fn panel_editor(&self, panel: &Panel) -> Element<'static, Message> {
        use cosmic::iced::{Alignment, Length};
        let palette = crate::visuals::Palette::from_resolved(&self.resolved.presented);
        let mut zones = widget::column([]).spacing(18);
        if panel.group_count() == ferese_config::panel::MAX_GROUPS {
            zones = zones.push(self.label(format!("{}-group limit reached", ferese_config::panel::MAX_GROUPS), 12.));
        }
        for placement in Zone::ALL {
            let zone = placement.definition(panel);
            let mut groups = widget::column([]).spacing(8).push(
                widget::row([])
                    .align_y(Alignment::Center)
                    .push(self.panel_heading(placement.to_string(), 13.).width(Length::Fill))
                    .push(
                        self.settings_button(
                            "Add group",
                            "M12 5v14 M5 12h14",
                            (panel.group_count() < ferese_config::panel::MAX_GROUPS)
                                .then_some(Message::PanelEdit(Action::AddGroup(placement))),
                            false,
                        ),
                    ),
            );
            let mut small_groups = Vec::new();
            for group in &zone.groups {
                let destination = Destination {
                    zone: placement.key(),
                    group: group.id.clone(),
                };
                let heading = widget::row([]).spacing(6).align_y(Alignment::Center).push(
                    widget::button::custom(
                        widget::row([])
                            .spacing(6)
                            .align_y(Alignment::Center)
                            .push(crate::visuals::action_icon("M3 7h18v13H3z M3 7V4h7l3 3", palette.muted))
                            .push(self.label(group_name(&group.id.0), 13.)),
                    )
                    .name(format!("Edit group {}", group.id.0))
                    .padding([7, 6])
                    .width(Length::Fill)
                    .class(crate::visuals::panel_button(
                        palette,
                        self.panel_group_selection.as_ref() == Some(&group.id),
                    ))
                    .on_press(Message::PanelGroupSelect(group.id.clone())),
                );
                let add_item = crate::visuals::panel_select(
                    cosmic::iced::widget::pick_list(CATALOG.to_vec(), None::<CatalogItem>, move |kind| {
                        Message::PanelEdit(Action::Add(kind.0, destination.clone()))
                    })
                    .placeholder("Add item")
                    .font(self.font)
                    .text_size(12)
                    .padding([6, 8])
                    .width(Length::Shrink),
                    palette,
                );
                let mut items = Vec::new();
                for item in &group.items {
                    let grip = widget::container(crate::visuals::action_icon(
                        "M9 5h.01 M15 5h.01 M9 12h.01 M15 12h.01 M9 19h.01 M15 19h.01",
                        palette.muted,
                    ))
                    .padding([8, 0])
                    .id(crate::panel_drag::grip_id(&item.id));
                    let select = widget::button::custom(
                        widget::row([])
                            .spacing(6)
                            .align_y(Alignment::Center)
                            .push(item_icon(
                                item.kind,
                                if item.visible {
                                    palette.text
                                } else {
                                    palette.muted.scale_alpha(0.6)
                                },
                            ))
                            .push(self.label(item.kind.label(), 13.).width(Length::Fill).class(
                                cosmic::theme::Text::Color(if item.visible {
                                    palette.text
                                } else {
                                    palette.muted.scale_alpha(0.6)
                                }),
                            )),
                    )
                    .name(format!("Edit {} · {}", item.kind.label(), item.id.0))
                    .padding([8, 6])
                    .width(Length::Fill)
                    .class(crate::visuals::panel_button(
                        palette,
                        self.panel_selection.as_ref() == Some(&item.id),
                    ))
                    .on_press(Message::PanelSelect(item.id.clone()));
                    items.push(
                        widget::container(
                            widget::row([])
                                .align_y(Alignment::Center)
                                .push(widget::mouse_area(grip).interaction(cosmic::iced::mouse::Interaction::Grab))
                                .push(select),
                        )
                        .id(crate::panel_drag::item_id(&item.id))
                        .width(Length::Fill)
                        .into(),
                    );
                }
                let mut contents = widget::column([]).spacing(4).push(heading);
                if items.is_empty() {
                    contents = contents.push(widget::container(self.note("Drop items here")).padding([8, 6]));
                } else {
                    contents = contents.push(
                        widget::flex_row(items)
                            .min_item_width(148.)
                            .spacing(4)
                            .width(Length::Fill),
                    );
                }
                contents = contents.push(widget::container(add_item).padding([4, 0]));
                let card: Element<'static, Message> = widget::container(contents)
                    .id(crate::panel_drag::group_id(&group.id))
                    .padding(6)
                    .width(Length::Fill)
                    .class(crate::visuals::surface(palette.card, 10.))
                    .into();
                if group.items.len() > 2 {
                    if !small_groups.is_empty() {
                        groups = groups.push(
                            widget::flex_row(std::mem::take(&mut small_groups))
                                .min_item_width(210.)
                                .spacing(8)
                                .width(Length::Fill),
                        );
                    }
                    groups = groups.push(card);
                } else {
                    small_groups.push(card);
                }
            }
            if !small_groups.is_empty() {
                groups = groups.push(
                    widget::flex_row(small_groups)
                        .min_item_width(210.)
                        .spacing(8)
                        .width(Length::Fill),
                );
            }
            if zone.groups.is_empty() {
                groups = groups.push(
                    widget::container(self.note("Drop an item here, or add a group."))
                        .id(crate::panel_drag::empty_zone_id(placement))
                        .width(Length::Fill)
                        .height(64)
                        .padding(12)
                        .class(crate::visuals::surface(palette.card, 10.)),
                );
            }
            zones = zones.push(widget::container(groups).padding([0, 4]).width(Length::Fill));
        }
        crate::panel_drag::frame(zones.into(), panel, palette.accent)
    }
}

pub(super) fn group_name(id: &str) -> String {
    let mut name = id.replace(['-', '_'], " ");
    if let Some(first) = name.get_mut(..1) {
        first.make_ascii_uppercase();
    }
    name
}

pub(super) fn item_icon(kind: ItemKind, tint: cosmic::iced::Color) -> widget::icon::Icon {
    use ferese_theme::icons;
    let source = match kind {
        ItemKind::Overview => icons::FERESE,
        ItemKind::Workspaces { .. } => icons::OVERVIEW,
        ItemKind::FocusedWindow => icons::DISPLAY,
        ItemKind::Media => icons::MEDIA,
        ItemKind::QuickSettings => icons::CONTROL_CENTER,
        ItemKind::Network => icons::WIFI_FULL,
        ItemKind::Audio => icons::VOLUME_HIGH,
        ItemKind::Recording => icons::RECORD,
        ItemKind::Notifications => icons::NOTIFICATIONS,
        ItemKind::Battery { .. } => icons::BATTERY_75,
        ItemKind::Clock => icons::CALENDAR,
        ItemKind::DisplayMode => icons::DISPLAY_EXTEND,
        ItemKind::Overflow => icons::CHEVRON_DOWN,
    };
    icons::tinted(source, 16, tint)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn customization_uses_canonical_defaults_and_retains_other_fields() {
        let mut snapshot = store::Snapshot::parse("// keep\nanimations { speed 0.8; }\n".into()).unwrap();
        snapshot.edit(&initialize(&snapshot).unwrap()).unwrap();
        let snapshot = store::Snapshot::parse(snapshot.source).unwrap();
        let panels: Vec<Panel> = serde_json::from_value(snapshot.item("panels").unwrap().clone()).unwrap();
        assert_eq!(panels[0].preset(), Some(ferese_config::PanelPreset::Continuous));
        assert_eq!(panels[0].geometry, ferese_config::panel::PanelGeometry::default());
        assert!(panels[0].center.groups[0].items[0].visible);
        assert_eq!(snapshot.number("animations.speed", 1.), 0.8);
        assert!(snapshot.source.contains("// keep"));
        let id = snapshot.string("panels.0.end.groups.0.items.2.id", "");
        let mut changed = snapshot;
        changed
            .edit(&store::set("panels.0.end.groups.0.items.2.overflow", "always"))
            .unwrap();
        let changed = store::Snapshot::parse(changed.source).unwrap();
        assert_eq!(changed.string("panels.0.end.groups.0.items.2.id", ""), id);
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
        let panel = Panel::default();
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
