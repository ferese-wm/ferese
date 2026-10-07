use cosmic::iced::{Alignment, Length};
use cosmic::widget::{button, column, container, row};
use ferese_config::panel::layout::{Placement, Resolution};
use ferese_config::panel::{Availability, Group, GroupSurface, Item, ItemId, ItemKind, Panel, Representation, Zone};
use ferese_theme::panel::{Sample, frame};

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
            .spacing(6)
            .push(
                row([])
                    .align_y(Alignment::Center)
                    .push(self.label("Composition preview", 14.).width(Length::Fill))
                    .push(self.note("Top panel · All displays")),
            )
            .push(self.note("Sample content. Select a control to edit it; resize this window to see it adapt."))
            .push(self.panel_preview_frame(panel.clone()));
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
            .padding(12)
            .width(Length::Fill)
            .class(visuals::surface(palette.card, 14.))
            .into()
    }

    fn preview_panel(&self) -> Result<Panel, String> {
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
                            ItemKind::FocusedWindow { .. } => Some(48.),
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
            + if overflow.surface == GroupSurface::Island {
                2. * overflow.island_padding
            } else {
                0.
            };
        let measured_panel = panel.clone();
        let content = frame(
            std::borrow::Cow::Owned(measured_panel),
            samples,
            overflow_width,
            self.panel_preview_resolution.clone(),
            move |resolution| {
                let start = self.preview_zone(&panel.start, resolution);
                let center = self.preview_zone(&panel.center, resolution);
                let mut end = self.preview_zone(&panel.end, resolution);
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
            .height(80)
            .clip(true)
            .class(visuals::surface(palette.sidebar, palette.radius))
            .into()
    }

    fn preview_zone(&self, zone: &Zone, resolution: &Resolution) -> Element<'static, Message> {
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
                groups = groups.push(self.preview_group(group, controls.into()));
            }
        }
        groups.into()
    }

    fn preview_group(&self, group: &Group, controls: Element<'static, Message>) -> Element<'static, Message> {
        let palette = visuals::Palette::from_resolved(&self.resolved.presented);
        if group.surface == GroupSurface::None {
            return controls;
        }
        let controls = container(controls).padding(group.padding);
        if group.surface == GroupSurface::Island {
            container(
                container(controls)
                    .padding(group.island_padding)
                    .class(visuals::surface(palette.card, palette.radius)),
            )
            .id(format!("preview-group:{}", group.id.0))
            .into()
        } else {
            controls.class(visuals::surface(palette.card, palette.radius)).into()
        }
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
            ItemKind::DisplayMode => icons::DISPLAY_EXTEND,
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
            .spacing(5)
            .align_y(Alignment::Center)
            .push(icons::tinted(source, 16, palette.text));
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
            .class(visuals::button_style(
                palette,
                self.panel_selection.as_ref() == Some(&item.id),
            ))
            .into();
        container(control)
            .id(format!("preview-item:{}", item.id.0))
            .width(width.map_or(Length::Shrink, Length::Fixed))
            .clip(true)
            .into()
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
