use crate::Message;
use cosmic::iced::advanced::{Clipboard, Layout, Shell, Widget, layout, mouse, renderer, widget};
use cosmic::iced::{Event, Length, Rectangle, Size, Vector, window};
use cosmic::{Element, Theme};

type Action = cosmic::Action<Message>;

pub(super) struct Target {
    pub workspace: u64,
    pub bounds: Rectangle,
    pub windows: Vec<(u64, Rectangle)>,
}

pub(super) fn area(
    content: Element<'_, Action>,
    id: window::Id,
    targets: Vec<Target>,
) -> Element<'_, Action> {
    Element::new(DragArea {
        content,
        id,
        targets,
    })
}

struct DragArea<'a> {
    content: Element<'a, Action>,
    id: window::Id,
    targets: Vec<Target>,
}
#[derive(Default)]
struct State {
    pressed: Option<(u64, u64, cosmic::iced::Point)>,
    moved: bool,
}

impl Widget<Action, Theme, cosmic::Renderer> for DragArea<'_> {
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
    fn layout(
        &mut self,
        tree: &mut widget::Tree,
        renderer: &cosmic::Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
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
        renderer: &cosmic::Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Action>,
        viewport: &Rectangle,
    ) {
        let state = tree.state.downcast_mut::<State>();
        let point = cursor
            .position()
            .map(|p| p - Vector::new(layout.bounds().x, layout.bounds().y));
        let target = point
            .filter(|_| cursor.is_over(*viewport))
            .and_then(|p| self.targets.iter().find(|t| t.bounds.contains(p)));
        match event {
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                if let (Some(target), Some(point)) = (target, point)
                    && let Some((window, _)) = target
                        .windows
                        .iter()
                        .rev()
                        .find(|(_, rect)| rect.contains(point))
                {
                    state.pressed = Some((*window, target.workspace, point));
                    state.moved = false;
                    shell.capture_event();
                    return;
                }
            }
            Event::Mouse(mouse::Event::CursorMoved { .. }) => {
                if let (Some((_, _, start)), Some(point)) = (state.pressed, point) {
                    state.moved |= start.distance(point) > 4.;
                    shell.request_redraw();
                }
            }
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => {
                if let Some((window, source, _)) = state.pressed.take() {
                    if let Some(target) = target {
                        if state.moved && target.workspace != source {
                            shell.publish(Action::App(Message::HubWorkspaceMove(
                                self.id,
                                window,
                                target.workspace,
                            )));
                        } else if !state.moved && target.workspace == source {
                            shell
                                .publish(Action::App(Message::HubWorkspaceSelect(self.id, source)));
                        }
                    }
                    state.moved = false;
                    shell.capture_event();
                    return;
                }
            }
            Event::Window(window::Event::Unfocused) => {
                state.pressed = None;
                state.moved = false;
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
        renderer: &mut cosmic::Renderer,
        theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        self.content.as_widget().draw(
            &tree.children[0],
            renderer,
            theme,
            style,
            layout,
            cursor,
            viewport,
        );
    }
    fn a11y_nodes(
        &self,
        layout: Layout<'_>,
        tree: &widget::Tree,
        cursor: mouse::Cursor,
    ) -> iced_accessibility::A11yTree {
        self.content
            .as_widget()
            .a11y_nodes(layout, &tree.children[0], cursor)
    }
    fn mouse_interaction(
        &self,
        tree: &widget::Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &cosmic::Renderer,
    ) -> mouse::Interaction {
        if tree.state.downcast_ref::<State>().pressed.is_some() {
            return mouse::Interaction::Grabbing;
        }
        self.content.as_widget().mouse_interaction(
            &tree.children[0],
            layout,
            cursor,
            viewport,
            renderer,
        )
    }
    fn operate(
        &mut self,
        tree: &mut widget::Tree,
        layout: Layout<'_>,
        renderer: &cosmic::Renderer,
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
        renderer: &cosmic::Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<cosmic::iced::advanced::overlay::Element<'a, Action, Theme, cosmic::Renderer>> {
        self.content.as_widget_mut().overlay(
            &mut tree.children[0],
            layout,
            renderer,
            viewport,
            translation,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cosmic::iced::advanced::clipboard;
    use cosmic::iced::advanced::renderer::Headless;

    #[test]
    fn drag_moves_the_pressed_window_once_and_click_still_selects_workspace() {
        let renderer = cosmic::iced::futures::executor::block_on(cosmic::Renderer::new(
            cosmic::iced::Font::default(),
            cosmic::iced::Pixels(14.),
            None,
        ))
        .unwrap();
        let bar = window::Id::unique();
        let build = || {
            area(
                cosmic::widget::Space::new().width(120).height(40).into(),
                bar,
                vec![
                    Target {
                        workspace: 100,
                        bounds: Rectangle::new((0., 0.).into(), (56., 40.).into()),
                        windows: vec![(999, Rectangle::new((6., 19.).into(), (44., 17.).into()))],
                    },
                    Target {
                        workspace: 101,
                        bounds: Rectangle::new((64., 0.).into(), (56., 40.).into()),
                        windows: vec![],
                    },
                ],
            )
        };
        for (end, expected) in [((80., 25.), Some((999, 101))), ((15., 25.), None)] {
            let mut view = build();
            let mut tree = widget::Tree::new(&view);
            let node = view.as_widget_mut().layout(
                &mut tree,
                &renderer,
                &layout::Limits::new(Size::ZERO, Size::new(120., 40.)),
            );
            let mut messages = Vec::new();
            for (event, cursor) in [
                (
                    Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
                    (15., 25.),
                ),
                (
                    Event::Mouse(mouse::Event::CursorMoved {
                        position: end.into(),
                    }),
                    end,
                ),
                (
                    Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)),
                    end,
                ),
                (
                    Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)),
                    end,
                ),
            ] {
                view.as_widget_mut().update(
                    &mut tree,
                    &event,
                    Layout::new(&node),
                    mouse::Cursor::Available(cursor.into()),
                    &renderer,
                    &mut clipboard::Null,
                    &mut Shell::new(&mut messages),
                    &Rectangle::new((0., 0.).into(), (120., 40.).into()),
                );
            }
            assert_eq!(
                messages.len(),
                1,
                "one action per gesture, even with a repeated release"
            );
            match (&messages[0], expected) {
                (Action::App(Message::HubWorkspaceMove(id, window, destination)), Some((w, d))) => {
                    assert_eq!((*id, *window, *destination), (bar, w, d))
                }
                (Action::App(Message::HubWorkspaceSelect(id, workspace)), None) => {
                    assert_eq!((*id, *workspace), (bar, 100))
                }
                _ => panic!("unexpected gesture action"),
            }
        }
    }
}
