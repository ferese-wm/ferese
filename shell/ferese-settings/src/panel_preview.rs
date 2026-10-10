use cosmic::iced::{Alignment, Length};
use cosmic::widget::{button, column, container, row};
use ferese_config::panel::layout::{Placement, Resolution};
use ferese_config::panel::{Availability, Group, Item, ItemId, ItemKind, Panel, Representation, Zone};
use ferese_theme::panel::{Sample, Spacing, frame};

use crate::{App, Element, Message, panel_controls, visuals};

const SAMPLE_AVAILABILITY: Availability = Availability {
    media: true,
    network: true,
    audio: true,
    notifications: true,
    battery: true,
    external_display: true,
};

fn available(item: &Item) -> bool {
    item.available(SAMPLE_AVAILABILITY) && !matches!(item.kind, ItemKind::FocusedWindow { enabled: false })
}

impl App {
    pub(super) fn panel_preview(&self) -> Element<'_, Message> {
        let panel = match self.preview_panel() {
            Ok(panel) => panel,
            Err(error) => return self.note(&format!("Panel preview unavailable: {error}")),
        };
        let mut rows = column([])
            .spacing(12)
            .push(
                row([])
                    .align_y(Alignment::Center)
                    .push(self.label("Composition preview", 14.).width(Length::Fill))
                    .push(self.note("Sample content")),
            )
            .push(
                container(self.panel_preview_frame(panel.clone()))
                    .padding(cosmic::iced::Padding {
                        top: if panel.edge == ferese_config::panel::Edge::Top {
                            10.
                        } else {
                            26.
                        },
                        bottom: if panel.edge == ferese_config::panel::Edge::Top {
                            26.
                        } else {
                            10.
                        },
                        left: 12.,
                        right: 12.,
                    })
                    .width(Length::Fill)
                    .class(visuals::surface(
                        visuals::Palette::from_resolved(&self.resolved.presented).sidebar,
                        10.,
                    )),
            );
        if self.panel_preview_overflow {
            let mut items = column([]).spacing(2);
            for id in &self.panel_preview_resolution.overflow {
                if let Some(item) = panel.item(id).filter(|item| available(item)) {
                    items = items.push(self.settings_button(
                        &format!("{} · {}", item.kind.label(), item.id.0),
                        "M6 9l6 6 6-6",
                        Some(Message::PanelSelect(item.id.clone())),
                        self.panel_selection.as_ref() == Some(id),
                    ));
                }
            }
            if !self.panel_preview_resolution.overflow.is_empty() {
                rows = rows.push(container(crate::scrollable(items).height(Length::Shrink)).max_height(160.));
            }
        }
        let palette = visuals::Palette::from_resolved(&self.resolved.presented);
        container(rows)
            .id("panel-composition-preview")
            .padding(16)
            .width(Length::Fill)
            .class(visuals::surface(palette.card, 14.))
            .into()
    }

    pub(super) fn preview_panel(&self) -> Result<Panel, String> {
        let panels = match self.draft.item("panels") {
            Some(panels) => panels.clone(),
            None => {
                let crate::store::Edit::Set(_, panels) = panel_controls::initialize(&self.draft)? else {
                    unreachable!()
                };
                panels
            }
        };
        serde_json::from_value::<Vec<Panel>>(panels)
            .map_err(|error| error.to_string())?
            .into_iter()
            .next()
            .ok_or_else(|| "No panel configured.".into())
    }

    fn panel_preview_frame(&self, panel: Panel) -> Element<'_, Message> {
        let mut samples = Vec::new();
        for zone in [&panel.center, &panel.start, &panel.end] {
            for item in zone
                .groups
                .iter()
                .flat_map(|group| &group.items)
                .filter(|item| available(item))
            {
                for representation in item.representations() {
                    samples.push(Sample {
                        id: item.id.clone(),
                        representation,
                        minimum: match item.kind {
                            ItemKind::Workspaces => Some(24.),
                            _ => None,
                        },
                        view: self.preview_control(item, representation, None),
                    });
                }
            }
        }
        let overflow = panel.overflow_group();
        samples.push(Sample {
            id: ItemId("_overflow".into()),
            representation: Representation::Icon,
            minimum: None,
            view: self.preview_control(&overflow.items[0], Representation::Icon, None),
        });
        let overflow_width = 2. * f32::from(overflow.padding[1])
            + if panel.background == ferese_config::BarLayout::Islands {
                2. * overflow.island_padding
            } else {
                0.
            };
        let continuous = panel.background == ferese_config::BarLayout::Continuous;
        let corners = panel
            .resolved_radius(self.resolved.presented.tokens.geometry.shell_radius as f32)
            .at_edge(
                panel.edge,
                self.resolved.presented.tokens.geometry.top_bar_margin_top == 0,
            )
            .0;
        let tokens = &self.resolved.presented.tokens;
        let inherited = if tokens.material.style == "translucent" {
            tokens.material.opacity as f32
        } else {
            1.
        };
        let opacity = panel.background_opacity.unwrap_or(inherited);
        let measured_panel = panel.clone();
        let content = frame(
            std::borrow::Cow::Owned(measured_panel),
            samples,
            Spacing {
                overflow_decoration: overflow_width,
                zone_gap: 32.,
            },
            self.panel_preview_resolution.clone(),
            move |resolution| {
                let start = self.preview_zone(&panel, &panel.start, resolution);
                let center = self.preview_zone(&panel, &panel.center, resolution);
                let mut end = self.preview_zone(&panel, &panel.end, resolution);
                if !resolution.overflow.is_empty() {
                    let mut trigger = Resolution::default();
                    trigger.items.insert(
                        ItemId("_overflow".into()),
                        Placement::Visible {
                            representation: Representation::Icon,
                            width: resolution.overflow_trigger_width,
                        },
                    );
                    let trigger = self.preview_zone(
                        &panel,
                        &Zone {
                            groups: vec![overflow.clone()],
                            spacing: 0.,
                        },
                        &trigger,
                    );
                    let has_end = panel.end.groups.iter().flat_map(|group| &group.items)
                        .any(|item| matches!(resolution.items.get(&item.id), Some(Placement::Visible { width, .. }) if *width > 0.));
                    end = row([])
                        .spacing(if has_end { panel.end.spacing } else { 0. })
                        .align_y(Alignment::Center)
                        .push(trigger)
                        .push(end)
                        .into();
                }
                cosmic::iced::widget::stack![
                    row([])
                        .width(Length::Fill)
                        .height(Length::Fill)
                        .align_y(Alignment::Center)
                        .push(start)
                        .push(cosmic::widget::Space::new().width(Length::Fill))
                        .push(end),
                    container(center).center_x(Length::Fill).center_y(Length::Fill),
                ]
                .into()
            },
            Message::PanelPreviewResolved,
        );
        let palette = visuals::Palette::from_resolved(&self.resolved.presented);
        container(content)
            .width(Length::Fill)
            .height(48)
            .clip(true)
            .class(cosmic::theme::Container::custom(move |_| {
                let mut style = ferese_theme::controls::surface_appearance(
                    cosmic::iced::Color {
                        a: opacity,
                        ..palette.card
                    },
                    0.,
                );
                style.border.radius = corners.into();
                if !continuous {
                    style.background = None;
                }
                style
            }))
            .into()
    }

    fn preview_zone(&self, panel: &Panel, zone: &Zone, resolution: &Resolution) -> Element<'static, Message> {
        let mut groups = row([]).spacing(zone.spacing).align_y(Alignment::Center);
        for group in &zone.groups {
            let mut controls = row([]).align_y(Alignment::Center);
            let mut count = 0;
            for item in &group.items {
                let Some(Placement::Visible { representation, width }) = resolution.items.get(&item.id) else {
                    continue;
                };
                if *width <= 0. {
                    continue;
                }
                if count > 0 {
                    controls =
                        controls.push(cosmic::widget::Space::new().width(item.gap_before.unwrap_or(group.spacing)));
                }
                controls = controls.push(self.preview_control(item, *representation, Some(*width)));
                count += 1;
            }
            if count > 0 {
                groups = groups.push(self.preview_group(panel, group, controls.into()));
            }
        }
        groups.into()
    }

    fn preview_group(
        &self,
        panel: &Panel,
        group: &Group,
        controls: Element<'static, Message>,
    ) -> Element<'static, Message> {
        let palette = visuals::Palette::from_resolved(&self.resolved.presented);
        let radius = panel
            .resolved_radius(self.resolved.presented.tokens.geometry.shell_radius as f32)
            .at_edge(
                panel.edge,
                self.resolved.presented.tokens.geometry.top_bar_margin_top == 0,
            )
            .0;
        let tokens = &self.resolved.presented.tokens;
        let inherited = if tokens.material.style == "translucent" {
            tokens.material.opacity as f32
        } else {
            1.
        };
        let opacity = panel.background_opacity.unwrap_or(inherited);
        let border = panel.border;
        let content: Element<'static, Message> = container(controls)
            .padding(group.padding)
            .class(cosmic::theme::Container::custom(move |_| {
                ferese_theme::panel::group_border(palette.muted.scale_alpha(0.4), radius, border)
            }))
            .into();
        if panel.background == ferese_config::BarLayout::Continuous {
            return content;
        }
        container(content)
            .padding([2., group.island_padding])
            .class(cosmic::theme::Container::custom(move |_| {
                let mut style = ferese_theme::controls::surface_appearance(
                    cosmic::iced::Color {
                        a: opacity,
                        ..palette.card
                    },
                    0.,
                );
                style.border.radius = radius.into();
                style
            }))
            .into()
    }

    fn preview_control(
        &self,
        item: &Item,
        representation: Representation,
        width: Option<f32>,
    ) -> Element<'static, Message> {
        use ferese_theme::icons;
        let palette = visuals::Palette::from_resolved(&self.resolved.presented);
        let source = match item.kind {
            ItemKind::Overview => icons::FERESE,
            ItemKind::Workspaces => icons::OVERVIEW,
            ItemKind::FocusedWindow { .. } => icons::DISPLAY,
            ItemKind::Media => icons::MEDIA,
            ItemKind::QuickSettings => icons::CONTROL_CENTER,
            ItemKind::Network => icons::WIFI_FULL,
            ItemKind::Audio => icons::VOLUME_HIGH,
            ItemKind::Recording => icons::RECORD,
            ItemKind::Notifications => icons::NOTIFICATIONS,
            ItemKind::Battery { .. } => icons::BATTERY_75,
            ItemKind::Clock => icons::CALENDAR,
            ItemKind::DisplayMode => icons::DISPLAY,
            ItemKind::Overflow => icons::CHEVRON_DOWN,
        };
        let label = match (item.kind, representation) {
            (ItemKind::Workspaces, _) => "1  2  3",
            (ItemKind::FocusedWindow { .. }, _) => "Settings",
            (ItemKind::Media, Representation::Wide) => "Song title · Artist  ▷",
            (ItemKind::Media, Representation::Compact) => "Song title",
            (ItemKind::Battery { percentage: true }, Representation::Wide) => "75%",
            (ItemKind::Clock, Representation::Wide) => "Wed, 7 Oct · 14:35",
            (ItemKind::Clock, Representation::Compact) => "14:35",
            _ => "",
        };
        let mut content = row([])
            .height(Length::Fill)
            .spacing(5)
            .align_y(Alignment::Center)
            .push(container(icons::tinted(source, 16, palette.text)).id(format!("preview-icon:{}", item.id.0)));

        if !label.is_empty() {
            content = content.push(
                self.label(label, 12.)
                    .wrapping(cosmic::iced::widget::text::Wrapping::None),
            );
        }
        let action = if item.kind == ItemKind::Overflow {
            Message::PanelPreviewOverflow
        } else {
            Message::PanelSelect(item.id.clone())
        };
        let control: Element<'static, Message> = button::custom(content)
            .name(format!("Edit {}", item.id.0))
            .padding([4, 7])
            .height(28)
            .on_press(action)
            .class(preview_button(
                palette,
                if item.kind == ItemKind::Overflow {
                    self.panel_preview_overflow
                } else {
                    self.panel_selection.as_ref() == Some(&item.id)
                },
            ))
            .into();
        container(control)
            .id(format!("preview-item:{}", item.id.0))
            .width(width.map_or(Length::Shrink, Length::Fixed))
            .clip(true)
            .into()
    }
}

/// Preview controls sit on a shared surface. Only interaction paints a background;
/// keep their hit area and border geometry identical in every state.
fn preview_button(palette: visuals::Palette, selected: bool) -> cosmic::theme::Button {
    let paint = move |hovered: bool, pressed: bool, focused: bool| {
        let background = if selected {
            Some(
                palette
                    .accent
                    .scale_alpha(if hovered || pressed { 0.26 } else { 0.16 })
                    .into(),
            )
        } else if pressed {
            Some(palette.text.scale_alpha(0.14).into())
        } else if hovered || focused {
            Some(palette.text.scale_alpha(0.08).into())
        } else {
            None
        };
        cosmic::widget::button::Style {
            shape: Some(cosmic::iced::border::Shape::Continuous),
            background,
            text_color: Some(palette.text),
            icon_color: Some(palette.text),
            border_radius: 6.into(),
            border_width: 1.,
            border_color: if focused {
                palette.accent
            } else if selected {
                palette.accent.scale_alpha(0.6)
            } else {
                cosmic::iced::Color::TRANSPARENT
            },
            outline: None,
            outline_width: 0.,
            ..Default::default()
        }
    };
    cosmic::theme::Button::Custom {
        active: Box::new(move |focused, _| paint(false, false, focused)),
        hovered: Box::new(move |focused, _| paint(true, false, focused)),
        pressed: Box::new(move |focused, _| paint(true, true, focused)),
        disabled: Box::new(move |_| paint(false, false, false)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cosmic::Application;
    use cosmic::iced::advanced::{Layout, Shell, clipboard, layout, mouse, renderer::Headless, widget};
    use cosmic::iced::{Event, Font, Pixels, Point, Rectangle, Size};

    #[derive(Default)]
    struct Bounds {
        targets: Vec<(widget::Id, ItemId)>,
        items: Vec<(ItemId, Rectangle)>,
    }
    impl widget::Operation for Bounds {
        fn traverse(&mut self, children: &mut dyn FnMut(&mut dyn widget::Operation)) {
            children(self);
        }
        fn container(&mut self, id: Option<&widget::Id>, bounds: Rectangle) {
            if let Some((_, item)) = self.targets.iter().find(|(target, _)| Some(target) == id) {
                self.items.push((item.clone(), bounds));
            }
        }
    }

    #[test]
    fn preview_measures_adapts_and_targets_exact_instances_without_writing_config() {
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
            (
                "/unused/preview-test.kdl".into(),
                crate::store::Snapshot::parse(String::new()),
                None,
            ),
        )
        .0;
        let panel = app.preview_panel().unwrap();
        let source = app.draft.source.clone();
        let mut tree = None;
        for width in [2400., 400., 180., 2400.] {
            let mut view = app.panel_preview_frame(panel.clone());
            let tree = tree.get_or_insert_with(|| widget::Tree::new(&view));
            tree.diff(view.as_widget_mut());
            let viewport = Rectangle::with_size(Size::new(width, 80.));
            let node = view
                .as_widget_mut()
                .layout(tree, &renderer, &layout::Limits::new(Size::ZERO, viewport.size()));
            let mut messages = Vec::new();
            for _ in 0..3 {
                view.as_widget_mut().update(
                    tree,
                    &Event::Window(cosmic::iced::window::Event::RedrawRequested(std::time::Instant::now())),
                    Layout::new(&node),
                    mouse::Cursor::Unavailable,
                    &renderer,
                    &mut clipboard::Null,
                    &mut Shell::new(&mut messages),
                    &viewport,
                );
            }
            assert_eq!(messages.len(), 1, "unchanged allocation is published once");
            let Message::PanelPreviewResolved(resolution) = &messages[0] else {
                panic!("expected resolution")
            };
            assert!(resolution.fits(width));
            let clock = ItemId("clock".into());
            if width == 2400. {
                assert!(matches!(
                    resolution.items.get(&clock),
                    Some(Placement::Visible {
                        representation: Representation::Wide,
                        ..
                    })
                ));
                assert!(resolution.overflow.is_empty());
            } else {
                assert!(!resolution.overflow.is_empty());
            }
            let mut bounds = Bounds {
                targets: resolution
                    .items
                    .keys()
                    .cloned()
                    .chain(std::iter::once(ItemId("_overflow".into())))
                    .map(|id| (widget::Id::new(format!("preview-item:{}", id.0)), id))
                    .collect(),
                ..Default::default()
            };
            view.as_widget_mut()
                .operate(tree, Layout::new(&node), &renderer, &mut bounds);
            for (id, bounds) in &bounds.items {
                let expected = if id.0 == "_overflow" {
                    resolution.overflow_trigger_width
                } else {
                    let Placement::Visible { width, .. } = resolution.items[id] else {
                        panic!("overflowed item was rendered")
                    };
                    width
                };
                assert!(
                    (bounds.width - expected).abs() < 0.1,
                    "{id:?}: {bounds:?}, expected {expected}"
                );
                assert!(
                    bounds.x >= -0.1 && bounds.x + bounds.width <= width + 0.1,
                    "{bounds:?} outside {width}"
                );
            }
            if width == 2400. {
                let target = bounds.items.iter().find(|(id, _)| id == &clock).unwrap().1.center();
                for event in [
                    mouse::Event::ButtonPressed(mouse::Button::Left),
                    mouse::Event::ButtonReleased(mouse::Button::Left),
                ] {
                    view.as_widget_mut().update(
                        tree,
                        &Event::Mouse(event),
                        Layout::new(&node),
                        mouse::Cursor::Available(Point::new(target.x, target.y)),
                        &renderer,
                        &mut clipboard::Null,
                        &mut Shell::new(&mut messages),
                        &viewport,
                    );
                }
                assert_eq!(
                    messages
                        .iter()
                        .filter(|message| matches!(message, Message::PanelSelect(id) if id == &clock))
                        .count(),
                    1
                );
            }
        }
        assert_eq!(app.draft.source, source);
        assert!(app.pending.is_empty());
    }

    #[test]
    fn preview_title_stays_centered_and_separate_from_asymmetric_side_groups() {
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
            (
                "/unused/preview-centering.kdl".into(),
                crate::store::Snapshot::parse(String::new()),
                None,
            ),
        )
        .0;
        for background in [ferese_config::BarLayout::Continuous, ferese_config::BarLayout::Islands] {
            let mut panel = Panel::from_defaults(&ferese_config::panel::Defaults {
                bar_layout: ferese_config::BarLayout::Islands,
                ..Default::default()
            });
            panel.background = background;
            // Match a three-group layout with media in navigation and a larger status group.
            let media = panel.end.groups[0].items.remove(0);
            panel.start.groups[0].items.push(media);
            let title = &panel.center.groups[0].items[0];
            let mut sample = app.preview_control(title, Representation::Wide, None);
            let mut sample_tree = widget::Tree::new(&sample);
            let natural = sample
                .as_widget_mut()
                .layout(
                    &mut sample_tree,
                    &renderer,
                    &layout::Limits::new(Size::ZERO, Size::new(2400., 80.)),
                )
                .size()
                .width;
            for width in [560., 668., 800., 1200.] {
                let mut view = app.panel_preview_frame(panel.clone());
                let mut tree = widget::Tree::new(&view);
                let node = view.as_widget_mut().layout(
                    &mut tree,
                    &renderer,
                    &layout::Limits::new(Size::ZERO, Size::new(width, 80.)),
                );
                let mut bounds = Bounds {
                    targets: [&panel.start, &panel.center, &panel.end]
                        .into_iter()
                        .flat_map(|zone| &zone.groups)
                        .flat_map(|group| &group.items)
                        .map(|item| (widget::Id::new(format!("preview-item:{}", item.id.0)), item.id.clone()))
                        .chain(std::iter::once((
                            widget::Id::new("preview-item:_overflow"),
                            ItemId("_overflow".into()),
                        )))
                        .collect(),
                    ..Default::default()
                };
                view.as_widget_mut()
                    .operate(&mut tree, Layout::new(&node), &renderer, &mut bounds);
                let center = bounds
                    .items
                    .iter()
                    .find(|(id, _)| id == &title.id)
                    .expect("sample title remains visible")
                    .1;
                assert!((center.center_x() - width / 2.).abs() < 0.1, "{center:?} at {width}");
                assert!((center.width - natural).abs() < 0.1, "sample title must not be clipped");
                for (id, side) in &bounds.items {
                    if id == &title.id {
                        continue;
                    }
                    let gap = if side.center_x() < center.center_x() {
                        center.x - side.x - side.width
                    } else {
                        side.x - center.x - center.width
                    };
                    assert!(
                        gap >= 31.9,
                        "{background:?} at {width}: {} crowds title ({gap}px)",
                        id.0
                    );
                }
            }
        }
    }

    #[test]
    fn preview_icons_are_centered_in_their_hit_areas_and_across_groups() {
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
        let mut app = App::init(
            crate::Core::default(),
            (
                "/unused/alignment.kdl".into(),
                crate::store::Snapshot::parse(String::new()),
                None,
            ),
        )
        .0;
        for background in [ferese_config::BarLayout::Continuous, ferese_config::BarLayout::Islands] {
            let panel = Panel::from_defaults(&ferese_config::panel::Defaults {
                bar_layout: background,
                ..Default::default()
            });
            let ids: Vec<_> = [&panel.start, &panel.center, &panel.end]
                .into_iter()
                .flat_map(|zone| &zone.groups)
                .flat_map(|group| &group.items)
                .map(|item| item.id.clone())
                .chain(std::iter::once(ItemId("_overflow".into())))
                .collect();
            for width in [180., 400., 2400.] {
                for selected in [None, Some(ItemId("battery".into()))] {
                    app.panel_selection = selected;
                    let mut view = app.panel_preview_frame(panel.clone());
                    let mut tree = widget::Tree::new(&view);
                    let node = view.as_widget_mut().layout(
                        &mut tree,
                        &renderer,
                        &layout::Limits::new(Size::ZERO, Size::new(width, 80.)),
                    );
                    let mut controls = Bounds {
                        targets: ids
                            .iter()
                            .map(|id| (widget::Id::new(format!("preview-item:{}", id.0)), id.clone()))
                            .collect(),
                        ..Default::default()
                    };
                    let mut icons = Bounds {
                        targets: ids
                            .iter()
                            .map(|id| (widget::Id::new(format!("preview-icon:{}", id.0)), id.clone()))
                            .collect(),
                        ..Default::default()
                    };
                    view.as_widget_mut()
                        .operate(&mut tree, Layout::new(&node), &renderer, &mut controls);
                    view.as_widget_mut()
                        .operate(&mut tree, Layout::new(&node), &renderer, &mut icons);
                    assert!(!controls.items.is_empty());
                    assert_eq!(controls.items.len(), icons.items.len());
                    for (id, icon) in &icons.items {
                        let control = controls.items.iter().find(|(item, _)| item == id).unwrap().1;
                        assert!(
                            (icon.center_y() - control.center_y()).abs() < 0.01,
                            "{background:?} at {width}: {} icon {icon:?} is off-center in {control:?}",
                            id.0
                        );
                        assert!(
                            (icon.center_y() - node.size().height / 2.).abs() < 0.01,
                            "{} is not on the panel centerline",
                            id.0
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn preview_hover_and_selection_stay_inside_the_control_and_idle_is_transparent() {
        use cosmic::iced::Color;
        use cosmic::iced::advanced::renderer::{self, Renderer as _};
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let mut renderer = runtime
            .block_on(<cosmic::Renderer as Headless>::new(
                Font::default(),
                Pixels(14.),
                Some("tiny-skia"),
            ))
            .unwrap();
        let mut app = App::init(
            crate::Core::default(),
            (
                "/unused/hover.kdl".into(),
                crate::store::Snapshot::parse(String::new()),
                None,
            ),
        )
        .0;
        let item = Item::new("battery", ItemKind::Battery { percentage: false });
        let viewport = Rectangle::with_size(Size::new(96., 48.));
        let backdrop = Color::from_rgb8(35, 47, 61);
        let theme = app.native_palette.native_theme();
        let mut baseline: Option<Vec<u8>> = None;
        let mut baseline_bounds = None;
        for state in ["idle", "hovered", "pressed", "selected"] {
            app.panel_selection = (state == "selected").then(|| item.id.clone());
            let mut view: Element<'_, Message> = container(app.preview_control(&item, Representation::Icon, None))
                .center_x(Length::Fill)
                .center_y(Length::Fill)
                .into();
            let mut tree = widget::Tree::new(&view);
            let node =
                view.as_widget_mut()
                    .layout(&mut tree, &renderer, &layout::Limits::new(Size::ZERO, viewport.size()));
            let mut bounds = Bounds {
                targets: vec![(widget::Id::new("preview-item:battery"), item.id.clone())],
                ..Default::default()
            };
            view.as_widget_mut()
                .operate(&mut tree, Layout::new(&node), &renderer, &mut bounds);
            let target = bounds.items[0].1;
            if let Some(baseline) = baseline_bounds {
                assert_eq!(target, baseline);
            } else {
                baseline_bounds = Some(target);
            }
            let cursor = if state == "hovered" || state == "pressed" {
                mouse::Cursor::Available(target.center())
            } else {
                mouse::Cursor::Unavailable
            };
            if state == "pressed" {
                view.as_widget_mut().update(
                    &mut tree,
                    &Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
                    Layout::new(&node),
                    cursor,
                    &renderer,
                    &mut clipboard::Null,
                    &mut Shell::new(&mut Vec::new()),
                    &viewport,
                );
            }
            renderer.reset(viewport);
            view.as_widget().draw(
                &tree,
                &mut renderer,
                &theme,
                &renderer::Style {
                    text_color: app.native_palette.text,
                    icon_color: app.native_palette.text,
                    scale_factor: 1.,
                },
                Layout::new(&node),
                cursor,
                &viewport,
            );
            let pixels = Headless::screenshot(&mut renderer, Size::new(96, 48), 1., backdrop);
            if let Some(baseline) = &baseline {
                let mut changed = 0;
                for (index, (before, after)) in baseline.chunks_exact(4).zip(pixels.chunks_exact(4)).enumerate() {
                    if before != after {
                        changed += 1;
                        assert!(
                            target.contains(Point::new((index % 96) as f32 + 0.5, (index / 96) as f32 + 0.5)),
                            "{state} paints outside the hit area"
                        );
                    }
                }
                assert!(changed > 40, "{state} has no visible feedback");
            } else {
                // Above the glyph but inside the button: untouched backdrop, not
                // an opaque rectangle that hides the panel's chosen opacity.
                let index = ((target.y as usize + 2) * 96 + target.center_x() as usize) * 4;
                assert_eq!(&pixels[index..index + 4], &pixels[..4]);
                baseline = Some(pixels);
            }
        }
    }

    #[test]
    fn invalid_legacy_status_reports_a_preview_error_instead_of_panicking() {
        let app = App::init(
            crate::Core::default(),
            (
                "/unused/preview-test.kdl".into(),
                crate::store::Snapshot::parse("status { bar-layout unknown; }".into()),
                None,
            ),
        )
        .0;
        assert!(app.preview_panel().is_err());
        let _ = app.panel_preview();
        assert!(app.pending.is_empty());
    }
}
