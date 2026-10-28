use cosmic::iced::advanced::widget::operation::Operation;
use cosmic::iced::advanced::widget::{Tree, tree};
use cosmic::iced::advanced::{Clipboard, Layout, Shell, Widget, layout, mouse, overlay, renderer};
use cosmic::iced::{Event, Rectangle, Size, Vector};
use cosmic::{Element, Renderer, Theme};

/// Measure natural content independently of the previous window height. A
/// bounded second layout keeps long lists scrollable without clipping actions.
pub(super) fn fit<Message: 'static>(
    content: Element<'_, Message>,
    height_limit: Option<f32>,
    on_resize: fn(Size) -> Message,
) -> Element<'_, Message> {
    Element::new(ContentSize {
        content,
        height_limit,
        on_resize,
    })
}

#[derive(Default)]
struct ReportedSize(Option<Size>);

struct ContentSize<'a, Message> {
    content: Element<'a, Message>,
    height_limit: Option<f32>,
    on_resize: fn(Size) -> Message,
}

impl<Message> Widget<Message, Theme, Renderer> for ContentSize<'_, Message> {
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<ReportedSize>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(ReportedSize::default())
    }

    fn children(&self) -> Vec<Tree> {
        vec![Tree::new(&self.content)]
    }

    fn diff(&mut self, tree: &mut Tree) {
        tree.diff_children(std::slice::from_mut(&mut self.content));
    }

    fn size(&self) -> Size<cosmic::iced::Length> {
        self.content.as_widget().size()
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        let limits = layout::Limits::new(
            Size::new(limits.min().width, 0.),
            Size::new(
                limits.max().width,
                self.height_limit.unwrap_or(f32::INFINITY),
            ),
        );
        self.content
            .as_widget_mut()
            .layout(&mut tree.children[0], renderer, &limits)
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn Operation,
    ) {
        operation.traverse(&mut |operation| {
            self.content.as_widget_mut().operate(
                &mut tree.children[0],
                layout,
                renderer,
                operation,
            );
        });
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        viewport: &Rectangle,
    ) {
        let size = layout.bounds().size();
        let reported = tree.state.downcast_mut::<ReportedSize>();
        if reported.0 != Some(size) {
            reported.0 = Some(size);
            shell.publish((self.on_resize)(size));
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
        tree: &Tree,
        renderer: &mut Renderer,
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
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        self.content.as_widget().mouse_interaction(
            &tree.children[0],
            layout,
            cursor,
            viewport,
            renderer,
        )
    }

    fn overlay<'a>(
        &'a mut self,
        tree: &'a mut Tree,
        layout: Layout<'a>,
        renderer: &Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'a, Message, Theme, Renderer>> {
        self.content.as_widget_mut().overlay(
            &mut tree.children[0],
            layout,
            renderer,
            viewport,
            translation,
        )
    }

    fn a11y_nodes(
        &self,
        layout: Layout<'_>,
        tree: &Tree,
        cursor: mouse::Cursor,
    ) -> iced_accessibility::A11yTree {
        self.content
            .as_widget()
            .a11y_nodes(layout, &tree.children[0], cursor)
    }
}
