use cosmic::iced::advanced::{Layout, Renderer as _, Widget, layout, mouse, renderer, widget};
use cosmic::iced::{Color, Length, Rectangle, Size};
use cosmic::{Element, Theme};
use std::sync::Arc;

pub(super) type Geometry = Arc<[(u64, Rectangle)]>;

pub(super) fn view<'a, M: 'a>(rectangles: Geometry, color: Color) -> Element<'a, M> {
    Element::new(Thumbnail { rectangles, color })
}

struct Thumbnail {
    rectangles: Geometry,
    color: Color,
}

impl<M> Widget<M, Theme, cosmic::Renderer> for Thumbnail {
    fn size(&self) -> Size<Length> {
        Size::new(Length::Fixed(44.), Length::Fixed(17.))
    }

    fn layout(&mut self, _: &mut widget::Tree, _: &cosmic::Renderer, limits: &layout::Limits) -> layout::Node {
        layout::Node::new(limits.resolve(44., 17., Size::ZERO))
    }

    fn draw(
        &self,
        _: &widget::Tree,
        renderer: &mut cosmic::Renderer,
        _: &Theme,
        _: &renderer::Style,
        layout: Layout<'_>,
        _: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        let Some(clip) = layout.bounds().intersection(viewport) else {
            return;
        };
        renderer.with_layer(clip, |renderer| {
            for (_, rect) in self.rectangles.iter() {
                renderer.fill_quad(
                    renderer::Quad {
                        bounds: *rect + (layout.bounds().position() - cosmic::iced::Point::ORIGIN),
                        border: cosmic::iced::Border {
                            radius: 0.8.into(),
                            ..Default::default()
                        },
                        ..Default::default()
                    },
                    self.color,
                );
            }
        });
    }
}
