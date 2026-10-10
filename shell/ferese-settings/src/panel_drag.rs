//! Local item dragging. Pointer motion changes only the drop hint; release saves once.
use cosmic::iced::advanced::{Clipboard, Layout, Shell, Widget, layout, mouse, renderer, widget};
use cosmic::iced::{Border, Color, Event, Length, Point, Rectangle, Size, Vector, keyboard};
use cosmic::{Element, Renderer, Theme};
use ferese_config::panel::{GroupId, ItemId, Panel};

use crate::Message;
use crate::panel_edit::{Action, Destination, Zone};

pub(super) fn item_id(id: &ItemId) -> widget::Id {
    widget::Id::new(format!("panel-editor:item:{}", id.0))
}
pub(super) fn grip_id(id: &ItemId) -> widget::Id {
    widget::Id::new(format!("panel-editor:grip:{}", id.0))
}
pub(super) fn group_id(id: &GroupId) -> widget::Id {
    widget::Id::new(format!("panel-editor:group:{}", id.0))
}
pub(super) fn empty_zone_id(zone: Zone) -> widget::Id {
    widget::Id::new(format!("panel-editor:empty:{}", zone.key()))
}

#[derive(Clone)]
enum Region {
    Grip(ItemId),
    Item(ItemId, Destination),
    Group(Destination),
    Empty(Zone),
}

pub(super) fn frame<'a>(content: Element<'a, Message>, panel: &Panel, accent: Color) -> Element<'a, Message> {
    let mut regions = Vec::new();
    for zone in Zone::ALL {
        if zone.definition(panel).groups.is_empty() {
            regions.push((empty_zone_id(zone), Region::Empty(zone)));
        }
        for group in &zone.definition(panel).groups {
            let destination = Destination {
                zone: zone.key(),
                group: group.id.clone(),
            };
            regions.push((group_id(&group.id), Region::Group(destination.clone())));
            for item in &group.items {
                regions.push((item_id(&item.id), Region::Item(item.id.clone(), destination.clone())));
                regions.push((grip_id(&item.id), Region::Grip(item.id.clone())));
            }
        }
    }
    Element::new(Editor {
        content,
        regions,
        accent,
    })
}

struct Editor<'a> {
    content: Element<'a, Message>,
    regions: Vec<(widget::Id, Region)>,
    accent: Color,
}

#[derive(Default)]
struct State {
    drag: Option<Drag>,
}
struct Drag {
    item: ItemId,
    origin: Point,
    cursor: Point,
    source: Rectangle,
    moved: bool,
    target: Option<Target>,
}

#[derive(Clone)]
enum Target {
    Group {
        destination: Destination,
        before: Option<ItemId>,
        marker: Rectangle,
    },
    Empty(Zone, Rectangle),
}
impl Target {
    fn action(self, item: ItemId) -> Action {
        match self {
            Self::Group {
                destination, before, ..
            } => Action::Place(item, destination, before),
            Self::Empty(zone, _) => Action::PlaceInZone(item, zone),
        }
    }
    fn marker(&self) -> Rectangle {
        match self {
            Self::Group { marker, .. } | Self::Empty(_, marker) => *marker,
        }
    }
}

struct Bounds<'a> {
    regions: &'a [(widget::Id, Region)],
    found: Vec<(Region, Rectangle)>,
}
impl widget::Operation for Bounds<'_> {
    fn traverse(&mut self, children: &mut dyn FnMut(&mut dyn widget::Operation)) {
        children(self);
    }
    fn container(&mut self, id: Option<&widget::Id>, bounds: Rectangle) {
        if let Some((_, region)) = self.regions.iter().find(|(target, _)| Some(target) == id) {
            self.found.push((region.clone(), bounds));
        }
    }
}

fn target_at(regions: &[(Region, Rectangle)], cursor: Point, viewport: &Rectangle) -> Option<Target> {
    if !viewport.contains(cursor) {
        return None;
    }
    for (region, bounds) in regions {
        if !bounds.contains(cursor) {
            continue;
        }
        match region {
            Region::Empty(zone) => return Some(Target::Empty(*zone, *bounds)),
            Region::Group(destination) => {
                let items: Vec<_> = regions
                    .iter()
                    .filter_map(|(region, bounds)| match region {
                        Region::Item(id, group) if group.group == destination.group => Some((id, *bounds)),
                        _ => None,
                    })
                    .collect();
                let next = items.iter().find(|(_, item)| {
                    cursor.y < item.y || cursor.y <= item.y + item.height && cursor.x < item.center_x()
                });
                let (before, marker) = next.map_or((None, *bounds), |(id, item)| {
                    (
                        Some((*id).clone()),
                        Rectangle {
                            x: item.x - 2.,
                            y: item.y,
                            width: 3.,
                            height: item.height,
                        },
                    )
                });
                return Some(Target::Group {
                    destination: destination.clone(),
                    before,
                    marker,
                });
            }
            _ => {}
        }
    }
    None
}

impl Widget<Message, Theme, Renderer> for Editor<'_> {
    fn tag(&self) -> widget::tree::Tag {
        widget::tree::Tag::of::<State>()
    }
    fn state(&self) -> widget::tree::State {
        widget::tree::State::new(State::default())
    }
    fn children(&self) -> Vec<widget::Tree> {
        vec![widget::Tree::new(&self.content)]
    }
    fn diff(&mut self, tree: &mut widget::Tree) {
        tree.diff_children(std::slice::from_mut(&mut self.content));
    }
    fn size(&self) -> Size<Length> {
        self.content.as_widget().size()
    }
    fn layout(&mut self, tree: &mut widget::Tree, renderer: &Renderer, limits: &layout::Limits) -> layout::Node {
        if tree.state.downcast_ref::<State>().drag.as_ref().is_some_and(|drag| {
            !self
                .regions
                .iter()
                .any(|(_, region)| matches!(region, Region::Grip(id) if id == &drag.item))
        }) {
            tree.state.downcast_mut::<State>().drag = None;
        }
        self.content
            .as_widget_mut()
            .layout(&mut tree.children[0], renderer, limits)
    }
    fn update(
        &mut self,
        tree: &mut widget::Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        viewport: &Rectangle,
    ) {
        let mut bounds = Bounds {
            regions: &self.regions,
            found: Vec::new(),
        };
        self.content
            .as_widget_mut()
            .operate(&mut tree.children[0], layout, renderer, &mut bounds);
        let state = tree.state.downcast_mut::<State>();
        if (matches!(event, Event::Window(cosmic::iced::window::Event::Unfocused))
            || matches!(
                event,
                Event::Keyboard(keyboard::Event::KeyPressed {
                    key: keyboard::Key::Named(keyboard::key::Named::Escape),
                    ..
                })
            ))
            && state.drag.take().is_some()
        {
            shell.capture_event();
            shell.request_redraw();
            return;
        }
        match event {
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                if let Some(cursor) = cursor.position().filter(|point| viewport.contains(*point))
                    && let Some((Region::Grip(item), _)) = bounds
                        .found
                        .iter()
                        .find(|(region, bounds)| matches!(region, Region::Grip(_)) && bounds.contains(cursor))
                {
                    let source = bounds
                        .found
                        .iter()
                        .find_map(|(region, bounds)| {
                            matches!(region, Region::Item(id, _) if id == item).then_some(*bounds)
                        })
                        .unwrap();
                    state.drag = Some(Drag {
                        item: item.clone(),
                        origin: cursor,
                        cursor,
                        source,
                        moved: false,
                        target: None,
                    });
                    shell.capture_event();
                    return;
                }
            }
            Event::Mouse(mouse::Event::CursorMoved { .. }) if state.drag.is_some() => {
                let drag = state.drag.as_mut().unwrap();
                if let Some(cursor) = cursor.position() {
                    drag.cursor = cursor;
                    drag.moved |= (cursor.x - drag.origin.x).hypot(cursor.y - drag.origin.y) >= 6.;
                    drag.target = if drag.moved {
                        target_at(&bounds.found, cursor, viewport)
                    } else {
                        None
                    };
                } else {
                    drag.target = None;
                }
                shell.capture_event();
                shell.request_redraw();
                return;
            }
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) if state.drag.is_some() => {
                let drag = state.drag.take().unwrap();
                if drag.moved {
                    if let Some(target) = cursor
                        .position()
                        .and_then(|cursor| target_at(&bounds.found, cursor, viewport))
                    {
                        shell.publish(Message::PanelEdit(target.action(drag.item)));
                    }
                } else if cursor
                    .position()
                    .is_some_and(|point| drag.source.contains(point) && viewport.contains(point))
                {
                    shell.publish(Message::PanelSelect(drag.item));
                }
                shell.capture_event();
                shell.request_redraw();
                return;
            }
            _ => {}
        }
        self.content.as_widget_mut().update(
            &mut tree.children[0],
            event,
            layout,
            cursor,
            renderer,
            clipboard,
            shell,
            viewport,
        );
    }
    fn draw(
        &self,
        tree: &widget::Tree,
        renderer: &mut Renderer,
        theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        use cosmic::iced::advanced::Renderer as _;
        self.content
            .as_widget()
            .draw(&tree.children[0], renderer, theme, style, layout, cursor, viewport);
        if let Some(drag) = tree
            .state
            .downcast_ref::<State>()
            .drag
            .as_ref()
            .filter(|drag| drag.moved)
        {
            renderer.with_layer(*viewport, |renderer| {
                if let Some(target) = &drag.target {
                    renderer.fill_quad(
                        renderer::Quad {
                            bounds: target.marker(),
                            border: Border {
                                color: self.accent,
                                width: 2.,
                                radius: 6.into(),
                                ..Border::default()
                            },
                            ..renderer::Quad::default()
                        },
                        Color { a: 0.12, ..self.accent },
                    );
                }
                let ghost = Rectangle {
                    x: drag.source.x + drag.cursor.x - drag.origin.x,
                    y: drag.source.y + drag.cursor.y - drag.origin.y,
                    ..drag.source
                };
                renderer.fill_quad(
                    renderer::Quad {
                        bounds: ghost,
                        border: Border {
                            color: self.accent,
                            width: 1.,
                            radius: 8.into(),
                            ..Border::default()
                        },
                        ..renderer::Quad::default()
                    },
                    Color { a: 0.12, ..self.accent },
                );
            });
        }
    }
    fn mouse_interaction(
        &self,
        tree: &widget::Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        if tree.state.downcast_ref::<State>().drag.is_some() {
            mouse::Interaction::Grabbing
        } else {
            self.content
                .as_widget()
                .mouse_interaction(&tree.children[0], layout, cursor, viewport, renderer)
        }
    }
    fn a11y_nodes(
        &self,
        layout: Layout<'_>,
        tree: &widget::Tree,
        cursor: mouse::Cursor,
    ) -> iced_accessibility::A11yTree {
        self.content.as_widget().a11y_nodes(layout, &tree.children[0], cursor)
    }
    fn operate(
        &mut self,
        tree: &mut widget::Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn widget::Operation,
    ) {
        self.content
            .as_widget_mut()
            .operate(&mut tree.children[0], layout, renderer, operation);
    }
    fn overlay<'a>(
        &'a mut self,
        tree: &'a mut widget::Tree,
        layout: Layout<'a>,
        renderer: &Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<cosmic::iced::advanced::overlay::Element<'a, Message, Theme, Renderer>> {
        self.content
            .as_widget_mut()
            .overlay(&mut tree.children[0], layout, renderer, viewport, translation)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{Snapshot, set};
    use cosmic::iced::advanced::{clipboard, renderer::Headless};
    use cosmic::iced::{Font, Pixels};
    use cosmic::widget::{button, column, container, row, text};

    fn panel() -> Panel {
        serde_json::from_value(serde_json::json!({"id":"main", "start":{"groups":[{"id":"one", "items":[{"id":"a", "kind":"clock"},{"id":"b", "kind":"clock"}]}]}, "end":{"groups":[{"id":"two", "items":[{"id":"c", "kind":"clock"}]}]}})).unwrap()
    }
    fn view(panel: &Panel) -> Element<'static, Message> {
        let zones = Zone::ALL.into_iter().map(|zone| {
            let mut groups = column([]).spacing(8);
            for group in &zone.definition(panel).groups {
                let items = group.items.iter().map(|item| {
                    container(
                        row([])
                            .push(container(text("::")).width(20).height(32).id(grip_id(&item.id)))
                            .push(
                                button::custom(text(item.id.0.clone()))
                                    .width(60)
                                    .height(32)
                                    .on_press(Message::PanelSelect(item.id.clone())),
                            ),
                    )
                    .id(item_id(&item.id))
                    .into()
                });
                groups = groups.push(
                    container(column(items).spacing(8))
                        .width(140)
                        .padding(8)
                        .id(group_id(&group.id)),
                );
            }
            if zone.definition(panel).groups.is_empty() {
                groups = groups.push(
                    container(text("Drop here"))
                        .width(140)
                        .height(100)
                        .id(empty_zone_id(zone)),
                );
            }
            groups.into()
        });
        frame(row(zones).spacing(20).into(), panel, Color::WHITE)
    }
    fn renderer() -> Renderer {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime
            .block_on(<Renderer as Headless>::new(
                Font::default(),
                Pixels(14.),
                Some("tiny-skia"),
            ))
            .unwrap()
    }
    fn send(
        view: &mut Element<'_, Message>,
        tree: &mut widget::Tree,
        node: &layout::Node,
        renderer: &Renderer,
        event: Event,
        point: Point,
        messages: &mut Vec<Message>,
    ) {
        let viewport = Rectangle::with_size(Size::new(500., 200.));
        view.as_widget_mut().update(
            tree,
            &event,
            Layout::new(node),
            mouse::Cursor::Available(point),
            renderer,
            &mut clipboard::Null,
            &mut Shell::new(messages),
            &viewport,
        );
    }
    #[test]
    fn dragging_moves_across_groups_only_on_release_and_empty_zones_are_targets() {
        let panel = panel();
        let renderer = renderer();
        let mut view = view(&panel);
        let mut tree = widget::Tree::new(&view);
        let viewport = Rectangle::with_size(Size::new(500., 200.));
        let node = view
            .as_widget_mut()
            .layout(&mut tree, &renderer, &layout::Limits::new(Size::ZERO, viewport.size()));
        let mut snapshot = Snapshot::parse(String::new()).unwrap();
        snapshot.edit(&set("panels", serde_json::json!([panel]))).unwrap();
        for (destination, empty) in [(Point::new(332., 20.), false), (Point::new(180., 20.), true)] {
            let mut messages = Vec::new();
            send(
                &mut view,
                &mut tree,
                &node,
                &renderer,
                Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
                Point::new(12., 20.),
                &mut messages,
            );
            send(
                &mut view,
                &mut tree,
                &node,
                &renderer,
                Event::Mouse(mouse::Event::CursorMoved { position: destination }),
                destination,
                &mut messages,
            );
            assert!(messages.is_empty(), "motion must not save");
            send(
                &mut view,
                &mut tree,
                &node,
                &renderer,
                Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)),
                destination,
                &mut messages,
            );
            assert_eq!(messages.len(), 1);
            let Message::PanelEdit(action) = messages.remove(0) else {
                panic!("expected drop action")
            };
            if empty {
                assert!(matches!(&action, Action::PlaceInZone(id, Zone::Center) if id.0 == "a"));
            } else {
                assert!(
                    matches!(&action, Action::Place(id, destination, Some(before)) if id.0 == "a" && destination.group.0 == "two" && before.0 == "c")
                );
            }
            let mut draft = snapshot.clone();
            for edit in crate::panel_edit::plan(&draft, action).unwrap() {
                draft.edit(&edit).unwrap();
            }
            Snapshot::parse(draft.source).unwrap();
        }
    }

    #[test]
    fn cancellation_outside_viewport_and_focus_loss_never_save_and_clicks_select() {
        let panel = panel();
        let renderer = renderer();
        let mut view = view(&panel);
        let mut tree = widget::Tree::new(&view);
        let viewport = Rectangle::with_size(Size::new(500., 200.));
        let node = view
            .as_widget_mut()
            .layout(&mut tree, &renderer, &layout::Limits::new(Size::ZERO, viewport.size()));
        for cancel in [true, false] {
            let mut messages = Vec::new();
            let outside = Point::new(600., 20.);
            send(
                &mut view,
                &mut tree,
                &node,
                &renderer,
                Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
                Point::new(12., 20.),
                &mut messages,
            );
            send(
                &mut view,
                &mut tree,
                &node,
                &renderer,
                Event::Mouse(mouse::Event::CursorMoved { position: outside }),
                outside,
                &mut messages,
            );
            if cancel {
                send(
                    &mut view,
                    &mut tree,
                    &node,
                    &renderer,
                    Event::Window(cosmic::iced::window::Event::Unfocused),
                    outside,
                    &mut messages,
                );
            }
            send(
                &mut view,
                &mut tree,
                &node,
                &renderer,
                Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)),
                outside,
                &mut messages,
            );
            assert!(messages.is_empty());
        }
        for cancel in [
            Event::Window(cosmic::iced::window::Event::Unfocused),
            Event::Keyboard(keyboard::Event::KeyPressed {
                key: keyboard::Key::Named(keyboard::key::Named::Escape),
                modified_key: keyboard::Key::Named(keyboard::key::Named::Escape),
                physical_key: keyboard::key::Physical::Code(keyboard::key::Code::Escape),
                location: keyboard::Location::Standard,
                modifiers: keyboard::Modifiers::empty(),
                text: None,
                repeat: false,
            }),
        ] {
            let mut messages = Vec::new();
            let destination = Point::new(332., 20.);
            send(
                &mut view,
                &mut tree,
                &node,
                &renderer,
                Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
                Point::new(12., 20.),
                &mut messages,
            );
            send(
                &mut view,
                &mut tree,
                &node,
                &renderer,
                Event::Mouse(mouse::Event::CursorMoved { position: destination }),
                destination,
                &mut messages,
            );
            send(
                &mut view,
                &mut tree,
                &node,
                &renderer,
                cancel,
                destination,
                &mut messages,
            );
            send(
                &mut view,
                &mut tree,
                &node,
                &renderer,
                Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)),
                destination,
                &mut messages,
            );
            assert!(messages.is_empty(), "cancellation must not save a valid drop");
        }
        for point in [Point::new(12., 20.), Point::new(60., 20.)] {
            let mut messages = Vec::new();
            for event in [
                mouse::Event::ButtonPressed(mouse::Button::Left),
                mouse::Event::ButtonReleased(mouse::Button::Left),
            ] {
                send(
                    &mut view,
                    &mut tree,
                    &node,
                    &renderer,
                    Event::Mouse(event),
                    point,
                    &mut messages,
                );
            }
            assert_eq!(messages.len(), 1);
            assert!(matches!(&messages[0], Message::PanelSelect(id) if id.0 == "a"));
        }
    }

    #[test]
    fn drop_hint_follows_wrapped_reading_order_and_ignores_clipped_targets() {
        let destination = Destination {
            zone: "start",
            group: GroupId("one".into()),
        };
        let regions = vec![
            (
                Region::Group(destination.clone()),
                Rectangle {
                    x: 0.,
                    y: 0.,
                    width: 140.,
                    height: 100.,
                },
            ),
            (
                Region::Item(ItemId("a".into()), destination.clone()),
                Rectangle {
                    x: 8.,
                    y: 8.,
                    width: 60.,
                    height: 32.,
                },
            ),
            (
                Region::Item(ItemId("b".into()), destination.clone()),
                Rectangle {
                    x: 76.,
                    y: 8.,
                    width: 60.,
                    height: 32.,
                },
            ),
            (
                Region::Item(ItemId("c".into()), destination),
                Rectangle {
                    x: 8.,
                    y: 48.,
                    width: 60.,
                    height: 32.,
                },
            ),
        ];
        let viewport = Rectangle::with_size(Size::new(140., 100.));
        for (point, expected) in [
            (Point::new(20., 20.), Some("a")),
            (Point::new(70., 20.), Some("b")),
            (Point::new(130., 20.), Some("c")),
            (Point::new(80., 70.), None),
        ] {
            let Some(Target::Group { before, .. }) = target_at(&regions, point, &viewport) else {
                panic!("expected group")
            };
            assert_eq!(before.as_ref().map(|id| id.0.as_str()), expected);
        }
        assert!(
            target_at(
                &regions,
                Point::new(20., 70.),
                &Rectangle::with_size(Size::new(140., 40.))
            )
            .is_none()
        );
    }
}
