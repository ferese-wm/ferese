use cosmic::iced::advanced::{Clipboard, Layout, Shell, Widget, layout, mouse, renderer, widget};
use cosmic::iced::{Event, Length, Rectangle, Size, Vector};
use cosmic::{Element, Theme};

pub(super) fn hover<'content, M: 'content>(
    content: Element<'content, M>,
    hover: Option<HoverTarget<'content, M>>,
) -> Element<'content, M> {
    Element::new(Hover { content, hover })
}

pub(super) struct HoverTarget<'content, M> {
    pub(super) id: widget::Id,
    pub(super) exited: Box<dyn Fn() -> M + 'content>,
}

struct HoverBounds<'content> {
    target: &'content widget::Id,
    bounds: Option<Rectangle>,
}

impl widget::Operation for HoverBounds<'_> {
    fn traverse(&mut self, children: &mut dyn FnMut(&mut dyn widget::Operation)) {
        children(self);
    }

    fn container(&mut self, id: Option<&widget::Id>, bounds: Rectangle) {
        if id == Some(self.target) {
            self.bounds = Some(bounds);
        }
    }
}

struct Hover<'content, M> {
    content: Element<'content, M>,
    hover: Option<HoverTarget<'content, M>>,
}

impl<M> Widget<M, Theme, cosmic::Renderer> for Hover<'_, M> {
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

    fn update(
        &mut self,
        tree: &mut widget::Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &cosmic::Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, M>,
        viewport: &Rectangle,
    ) {
        // The stack may stop dispatching after another control captures input.
        // Reconcile the active tooltip before any child handles the pointer.
        if matches!(
            event,
            Event::Mouse(mouse::Event::CursorMoved { .. } | mouse::Event::CursorLeft)
        ) && let Some(hover) = &self.hover
        {
            let mut target = HoverBounds {
                target: &hover.id,
                bounds: None,
            };
            self.content.as_widget_mut().operate(
                &mut tree.children[0],
                layout,
                renderer,
                &mut target,
            );
            if matches!(event, Event::Mouse(mouse::Event::CursorLeft))
                || !cursor.is_over(*viewport)
                || !target.bounds.is_some_and(|bounds| cursor.is_over(bounds))
            {
                shell.publish((hover.exited)());
            }
        }
        let cursor = if matches!(event, Event::Mouse(mouse::Event::CursorLeft)) {
            mouse::Cursor::Unavailable
        } else {
            cursor
        };
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

    fn mouse_interaction(
        &self,
        tree: &widget::Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &cosmic::Renderer,
    ) -> mouse::Interaction {
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

    fn overlay<'content>(
        &'content mut self,
        tree: &'content mut widget::Tree,
        layout: Layout<'content>,
        renderer: &cosmic::Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<cosmic::iced::advanced::overlay::Element<'content, M, Theme, cosmic::Renderer>>
    {
        self.content.as_widget_mut().overlay(
            &mut tree.children[0],
            layout,
            renderer,
            viewport,
            translation,
        )
    }
}
