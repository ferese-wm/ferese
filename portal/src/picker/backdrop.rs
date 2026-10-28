use cosmic::iced::advanced::{Layout, Widget, layout, mouse, renderer, widget::Tree};
use cosmic::iced::{Color, Length, Rectangle, Size};
use cosmic::{Element, Renderer, Theme};

/// Leave the compositor's material visible beneath the popup. Painting the
/// source list, a scrim, and another material fill there would stack their alpha
/// and make a translucent dialog look opaque.
pub(super) fn backdrop<'a, M: 'a>(
    content: Element<'a, M>,
    popup_measure: Element<'a, M>,
) -> Element<'a, M> {
    Element::new(Backdrop {
        content,
        popup_measure,
    })
}

struct Backdrop<'a, M> {
    content: Element<'a, M>,
    popup_measure: Element<'a, M>,
}

impl<M> Widget<M, Theme, Renderer> for Backdrop<'_, M> {
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
            &layout::Limits::new(Size::UNIT, content.size()),
        );
        layout::Node::with_children(content.size(), vec![content, popup])
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
        let content = children.next().unwrap();
        let popup = children.next().unwrap();
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
