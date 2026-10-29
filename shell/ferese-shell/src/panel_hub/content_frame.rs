use crate::Message;
use cosmic::iced::advanced::{
    Clipboard, Layout, Renderer as _, Shell, Widget, layout, mouse, renderer, widget,
};
use cosmic::iced::{Event, Length, Rectangle, Size, Vector};
use cosmic::{Element, Theme};

type Action = cosmic::Action<Message>;

// Content keeps its destination layout while the shell springs around it.
// Centering against the animated width cancels the shell's movement; anchoring
// to the screen edge keeps its vertical position stable too.
pub(super) fn view(
    content: Element<'_, Action>,
    size: [f64; 2],
    edge: ferese_panel::Edge,
) -> Element<'_, Action> {
    Element::new(ContentFrame {
        content,
        target: Size::new(size[0] as f32, size[1] as f32),
        edge,
    })
}

struct ContentFrame<'content> {
    content: Element<'content, Action>,
    target: Size,
    edge: ferese_panel::Edge,
}

impl Widget<Action, Theme, cosmic::Renderer> for ContentFrame<'_> {
    fn children(&self) -> Vec<widget::Tree> {
        vec![widget::Tree::new(&self.content)]
    }
    fn diff(&mut self, tree: &mut widget::Tree) {
        tree.diff_children(std::slice::from_mut(&mut self.content));
    }
    fn size(&self) -> Size<Length> {
        Size::new(Length::Fill, Length::Fill)
    }
    fn layout(
        &mut self,
        tree: &mut widget::Tree,
        renderer: &cosmic::Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        let size = limits.resolve(Length::Fill, Length::Fill, Size::ZERO);
        let child = self
            .content
            .as_widget_mut()
            .layout(
                &mut tree.children[0],
                renderer,
                &layout::Limits::new(self.target, self.target),
            )
            .move_to(cosmic::iced::Point::new(
                (size.width - self.target.width) / 2.,
                if self.edge == ferese_panel::Edge::Bottom {
                    size.height - self.target.height
                } else {
                    0.
                },
            ));
        layout::Node::with_children(size, vec![child])
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
        let viewport = viewport.intersection(&layout.bounds()).unwrap_or_default();
        let cursor = if cursor.is_over(viewport) {
            cursor
        } else {
            mouse::Cursor::Unavailable
        };
        self.content.as_widget_mut().update(
            &mut tree.children[0],
            event,
            layout.children().next().expect("content layout"),
            cursor,
            renderer,
            clipboard,
            shell,
            &viewport,
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
        if let Some(clip) = viewport.intersection(&layout.bounds()) {
            renderer.with_layer(clip, |renderer| {
                self.content.as_widget().draw(
                    &tree.children[0],
                    renderer,
                    theme,
                    style,
                    layout.children().next().expect("content layout"),
                    cursor,
                    &clip,
                );
            });
        }
    }
    fn a11y_nodes(
        &self,
        layout: Layout<'_>,
        tree: &widget::Tree,
        cursor: mouse::Cursor,
    ) -> iced_accessibility::A11yTree {
        self.content.as_widget().a11y_nodes(
            layout.children().next().expect("content layout"),
            &tree.children[0],
            cursor,
        )
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
            layout.children().next().expect("content layout"),
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
        self.content.as_widget_mut().operate(
            &mut tree.children[0],
            layout.children().next().expect("content layout"),
            renderer,
            operation,
        );
    }
    fn overlay<'content>(
        &'content mut self,
        tree: &'content mut widget::Tree,
        layout: Layout<'content>,
        renderer: &cosmic::Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<cosmic::iced::advanced::overlay::Element<'content, Action, Theme, cosmic::Renderer>>
    {
        self.content.as_widget_mut().overlay(
            &mut tree.children[0],
            layout.children().next().expect("content layout"),
            renderer,
            viewport,
            translation,
        )
    }
}
