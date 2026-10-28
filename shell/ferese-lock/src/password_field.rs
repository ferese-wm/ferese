//! The field renders a mask, but never stores or publishes a secret through the widget tree.
use cosmic::iced::advanced::widget::operation::Focusable;
use cosmic::iced::advanced::{Clipboard, Layout, Shell, Widget, layout, mouse, renderer, widget};
use cosmic::iced::{Event, Length, Rectangle, Size, touch};
use cosmic::{Element, Renderer, Theme};
use iced_accessibility::A11yTree;
use iced_accessibility::accesskit::{Action, Node, Rect, Role};

use crate::Message;

pub(super) struct PasswordField<'a> {
    content: Element<'a, Message>,
    enabled: bool,
    id: widget::Id,
}

impl<'a> PasswordField<'a> {
    pub(super) fn new(content: Element<'a, Message>, enabled: bool) -> Self {
        Self {
            content,
            enabled,
            id: widget::Id::new("password"),
        }
    }

    fn accessible_node(&self, bounds: Rectangle) -> Node {
        let mut node = Node::new(Role::PasswordInput);
        node.set_label("Password");
        node.set_description("Enter your password, then press Enter to unlock. Escape clears it.");
        node.set_bounds(Rect::new(
            bounds.x.into(),
            bounds.y.into(),
            (bounds.x + bounds.width).into(),
            (bounds.y + bounds.height).into(),
        ));
        if self.enabled {
            node.add_action(Action::Focus);
        } else {
            node.set_disabled();
        }
        // No value, child text nodes, selection ranges, or password length are exposed.
        node
    }
}

#[derive(Default)]
struct State {
    focused: bool,
}

impl Focusable for State {
    fn is_focused(&self) -> bool {
        self.focused
    }
    fn focus(&mut self) {
        self.focused = true;
    }
    fn unfocus(&mut self) {
        self.focused = false;
    }
}

impl Widget<Message, Theme, Renderer> for PasswordField<'_> {
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
    fn id(&self) -> Option<widget::Id> {
        Some(self.id.clone())
    }
    fn set_id(&mut self, id: widget::Id) {
        self.id = id;
    }
    fn size(&self) -> Size<Length> {
        self.content.as_widget().size()
    }
    fn layout(
        &mut self,
        tree: &mut widget::Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        self.content
            .as_widget_mut()
            .layout(&mut tree.children[0], renderer, limits)
    }
    fn operate(
        &mut self,
        tree: &mut widget::Tree,
        layout: Layout<'_>,
        _: &Renderer,
        operation: &mut dyn widget::Operation,
    ) {
        let state = tree.state.downcast_mut::<State>();
        if self.enabled {
            operation.focusable(Some(&self.id), layout.bounds(), state);
        } else {
            state.unfocus();
        }
    }
    fn a11y_nodes(&self, layout: Layout<'_>, _: &widget::Tree, _: mouse::Cursor) -> A11yTree {
        A11yTree::leaf(self.accessible_node(layout.bounds()), self.id.clone())
    }
    fn update(
        &mut self,
        _: &mut widget::Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        _: &Renderer,
        _: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        _: &Rectangle,
    ) {
        if !self.enabled {
            return;
        }
        let focus = match event {
            Event::A11y(id, request) => *id == self.id && request.action == Action::Focus,
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                cursor.is_over(layout.bounds())
            }
            Event::Touch(touch::Event::FingerPressed { position, .. }) => {
                layout.bounds().contains(*position)
            }
            _ => false,
        };
        if focus {
            shell.publish(Message::FocusPassword);
            shell.capture_event();
        }
    }
    fn mouse_interaction(
        &self,
        _: &widget::Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        _: &Rectangle,
        _: &Renderer,
    ) -> mouse::Interaction {
        if self.enabled && cursor.is_over(layout.bounds()) {
            mouse::Interaction::Text
        } else {
            mouse::Interaction::default()
        }
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
        if let Some(clip) = layout.bounds().intersection(viewport) {
            renderer.with_layer(clip, |renderer| {
                self.content.as_widget().draw(
                    &tree.children[0],
                    renderer,
                    theme,
                    style,
                    layout,
                    cursor,
                    &clip,
                );
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn semantic_password_entry_never_exposes_rendered_mask() {
        let content = cosmic::widget::text("••••••").into();
        let mut field = PasswordField::new(content, true);
        let node = field.accessible_node(Rectangle::with_size(Size::new(240., 48.)));
        assert_eq!(node.role(), Role::PasswordInput);
        assert_eq!(node.label(), Some("Password"));
        assert!(node.value().is_none());
        assert!(node.children().is_empty());
        assert!(node.supports_action(Action::Focus));
        assert!(!node.is_disabled());
        field.enabled = false;
        let node = field.accessible_node(Rectangle::default());
        assert!(node.is_disabled());
        assert!(!node.supports_action(Action::Focus));
    }
}
