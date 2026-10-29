use cosmic::iced::advanced::widget::Tree;
use cosmic::iced::advanced::{Layout, Widget, layout, mouse, renderer};
use cosmic::iced::{Color, Length, Rectangle, Size};
use cosmic::{Element, Renderer, Theme};

/// Leave the compositor's material visible beneath the popup. Painting the
/// source list, a scrim, and another material fill there would stack their alpha
/// and make a translucent dialog look opaque.
pub(super) fn backdrop<'content, Message: 'content>(
    content: Element<'content, Message>,
    popup_measure: Element<'content, Message>,
) -> Element<'content, Message> {
    Element::new(Backdrop {
        content,
        popup_measure,
    })
}

struct Backdrop<'content, Message> {
    content: Element<'content, Message>,
    popup_measure: Element<'content, Message>,
}

impl<Message> Widget<Message, Theme, Renderer> for Backdrop<'_, Message> {
    fn children(&self) -> Vec<Tree> {
        vec![Tree::new(&self.content), Tree::new(&self.popup_measure)]
    }

    fn diff(&mut self, tree: &mut Tree) {
        tree.diff_children(&mut [&mut self.content, &mut self.popup_measure]);
    }

    fn size(&self) -> Size<Length> {
        Size::new(Length::Fill, Length::Fill)
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        let content = self
            .content
            .as_widget_mut()
            .layout(&mut tree.children[0], renderer, limits);
        let popup = self.popup_measure.as_widget_mut().layout(
            &mut tree.children[1],
            renderer,
            &layout::Limits::new(Size::UNIT, limits.max()),
        );
        let size = Size::new(
            content.size().width,
            content
                .size()
                .height
                .max(popup.size().height + 32.)
                .min(limits.max().height),
        );
        layout::Node::with_children(size, vec![content, popup])
    }

    // This is the inactive picker behind a modal. It intentionally receives no
    // events or focus operations; the popover handles the interactive popup.
    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut Renderer,
        theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        _cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        use cosmic::iced::advanced::Renderer as _;
        let mut children = layout.children();
        let content = children
            .next()
            .expect("backdrop layout contains the picker and measured popup");
        let popup = children
            .next()
            .expect("backdrop layout contains the picker and measured popup");
        for region in outside_popup(layout.bounds(), popup.bounds().size()) {
            if let Some(clip) = region.intersection(viewport) {
                renderer.with_layer(clip, |renderer| {
                    self.content.as_widget().draw(
                        &tree.children[0],
                        renderer,
                        theme,
                        style,
                        content,
                        mouse::Cursor::Unavailable,
                        &clip,
                    );
                    renderer.fill_quad(
                        renderer::Quad {
                            bounds: clip,
                            ..Default::default()
                        },
                        Color::from_rgba(0., 0., 0., 0.35),
                    );
                });
            }
        }
    }
}

fn outside_popup(bounds: Rectangle, popup: Size) -> [Rectangle; 4] {
    // Match the centered, rounded placement used by libcosmic's popover.
    let x = ((bounds.center_x().round() - popup.width / 2.)
        .clamp(bounds.x, bounds.x + bounds.width - popup.width))
    .round();
    let y = ((bounds.center_y().round() - popup.height / 2.)
        .clamp(bounds.y, bounds.y + bounds.height - popup.height))
    .round();
    [
        Rectangle::new(bounds.position(), Size::new(bounds.width, y - bounds.y)),
        Rectangle::new(
            cosmic::iced::Point::new(bounds.x, y + popup.height),
            Size::new(bounds.width, bounds.y + bounds.height - y - popup.height),
        ),
        Rectangle::new(
            cosmic::iced::Point::new(bounds.x, y),
            Size::new(x - bounds.x, popup.height),
        ),
        Rectangle::new(
            cosmic::iced::Point::new(x + popup.width, y),
            Size::new(bounds.x + bounds.width - x - popup.width, popup.height),
        ),
    ]
}
