use std::time::Instant;

use cosmic::iced::advanced::{
    Clipboard, Layout, Renderer as _, Shell, Widget, layout, mouse, renderer, widget,
};
use cosmic::iced::{Color, Event, Length, Rectangle, Size, window};
use cosmic::{Element, Theme};

use crate::Message;

pub(super) fn view<'a>(
    started: Instant,
    animate: bool,
    color: Color,
) -> Element<'a, cosmic::Action<Message>> {
    Element::new(Equalizer {
        started,
        animate,
        color,
    })
}

struct Equalizer {
    started: Instant,
    animate: bool,
    color: Color,
}

impl Widget<cosmic::Action<Message>, Theme, cosmic::Renderer> for Equalizer {
    fn tag(&self) -> widget::tree::Tag {
        widget::tree::Tag::of::<Instant>()
    }

    fn state(&self) -> widget::tree::State {
        widget::tree::State::new(self.started)
    }

    fn size(&self) -> Size<Length> {
        Size::new(Length::Fixed(13.), Length::Fixed(14.))
    }

    fn layout(
        &mut self,
        _: &mut widget::Tree,
        _: &cosmic::Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        layout::Node::new(limits.resolve(13., 14., Size::ZERO))
    }

    fn update(
        &mut self,
        tree: &mut widget::Tree,
        event: &Event,
        _: Layout<'_>,
        _: mouse::Cursor,
        _: &cosmic::Renderer,
        _: &mut dyn Clipboard,
        shell: &mut Shell<'_, cosmic::Action<Message>>,
        _: &Rectangle,
    ) {
        if let Event::Window(window::Event::RedrawRequested(at)) = event {
            *tree.state.downcast_mut::<Instant>() = *at;
            if self.animate {
                shell.request_redraw();
            }
        }
    }

    fn draw(
        &self,
        tree: &widget::Tree,
        renderer: &mut cosmic::Renderer,
        _: &Theme,
        _: &renderer::Style,
        layout: Layout<'_>,
        _: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        let bounds = layout.bounds();
        let Some(clip) = bounds.intersection(viewport) else {
            return;
        };
        let elapsed = tree
            .state
            .downcast_ref::<Instant>()
            .saturating_duration_since(self.started)
            .as_secs_f32();
        renderer.with_layer(clip, |renderer| {
            for index in 0..3 {
                let phase = (elapsed - index as f32 * 0.2) * std::f32::consts::TAU / 0.9;
                let fraction = if self.animate {
                    0.65 + 0.35 * phase.sin()
                } else {
                    0.45
                };
                let height = bounds.height * fraction;
                renderer.fill_quad(
                    renderer::Quad {
                        bounds: Rectangle::new(
                            (
                                bounds.x + index as f32 * 5.,
                                bounds.y + bounds.height - height,
                            )
                                .into(),
                            (3., height).into(),
                        ),
                        border: cosmic::iced::Border {
                            radius: 2.0.into(),
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

#[cfg(test)]
mod tests {
    use super::*;
    use cosmic::iced::advanced::clipboard;
    use cosmic::iced::advanced::renderer::Headless;

    #[test]
    fn bars_redraw_without_rebuilding_layout_or_publishing_messages() {
        let renderer = cosmic::iced::futures::executor::block_on(cosmic::Renderer::new(
            cosmic::iced::Font::default(),
            cosmic::iced::Pixels(14.),
            Some("tiny-skia"),
        ))
        .unwrap();
        for animate in [true, false] {
            let started = Instant::now();
            let mut element = view(started, animate, Color::WHITE);
            let mut tree = widget::Tree::new(&element);
            let node = element.as_widget_mut().layout(
                &mut tree,
                &renderer,
                &layout::Limits::new(Size::ZERO, Size::new(100., 100.)),
            );
            let mut messages = Vec::new();
            for frame in 0..120 {
                let mut shell = Shell::new(&mut messages);
                element.as_widget_mut().update(
                    &mut tree,
                    &Event::Window(window::Event::RedrawRequested(
                        started + std::time::Duration::from_millis(frame * 8),
                    )),
                    Layout::new(&node),
                    mouse::Cursor::Unavailable,
                    &renderer,
                    &mut clipboard::Null,
                    &mut shell,
                    &Rectangle::with_size(Size::new(100., 100.)),
                );
                assert!(!shell.is_layout_invalid());
                assert_eq!(
                    shell.redraw_request(),
                    if animate {
                        window::RedrawRequest::NextFrame
                    } else {
                        window::RedrawRequest::Wait
                    }
                );
            }
            assert!(messages.is_empty());
        }
    }
}
