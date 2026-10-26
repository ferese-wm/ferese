use std::time::{Duration, Instant};

use cosmic::iced::advanced::{
    Clipboard, Layout, Renderer as _, Shell, Widget, layout, mouse, renderer, widget,
};
use cosmic::iced::{Color, Event, Length, Rectangle, Size, window};
use cosmic::{Element, Theme};

use crate::Message;

// These three decorative bars do not need to keep the whole panel and its
// backdrop rendering at the display's refresh rate.
const FRAME_INTERVAL: Duration = Duration::from_nanos(1_000_000_000 / 24);

struct FrameState {
    sampled: Instant,
    next_frame: Instant,
}

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
        widget::tree::Tag::of::<FrameState>()
    }

    fn state(&self) -> widget::tree::State {
        widget::tree::State::new(FrameState {
            sampled: self.started,
            next_frame: self.started,
        })
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
        layout: Layout<'_>,
        _: mouse::Cursor,
        _: &cosmic::Renderer,
        _: &mut dyn Clipboard,
        shell: &mut Shell<'_, cosmic::Action<Message>>,
        viewport: &Rectangle,
    ) {
        if let Event::Window(window::Event::RedrawRequested(at)) = event
            && self.animate
            && self.color.a > 0.0
            && layout.bounds().intersection(viewport).is_some()
        {
            let state = tree.state.downcast_mut::<FrameState>();
            if *at >= state.next_frame {
                state.sampled = *at;
                // A delayed frame needs one current sample, not a catch-up
                // burst. Earlier redraws from input preserve this deadline.
                state.next_frame = *at + FRAME_INTERVAL;
            }
            shell.request_redraw_at(window::RedrawRequest::At(state.next_frame));
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
            .downcast_ref::<FrameState>()
            .sampled
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

    fn renderer() -> cosmic::Renderer {
        cosmic::iced::futures::executor::block_on(cosmic::Renderer::new(
            cosmic::iced::Font::default(),
            cosmic::iced::Pixels(14.),
            Some("tiny-skia"),
        ))
        .unwrap()
    }

    fn redraw(
        element: &mut Element<'_, cosmic::Action<Message>>,
        tree: &mut widget::Tree,
        renderer: &cosmic::Renderer,
        at: Instant,
        viewport: Rectangle,
    ) -> window::RedrawRequest {
        let node = element.as_widget_mut().layout(
            tree,
            renderer,
            &layout::Limits::new(Size::ZERO, Size::new(100., 100.)),
        );
        let mut messages = Vec::new();
        let mut shell = Shell::new(&mut messages);
        element.as_widget_mut().update(
            tree,
            &Event::Window(window::Event::RedrawRequested(at)),
            Layout::new(&node),
            mouse::Cursor::Unavailable,
            renderer,
            &mut clipboard::Null,
            &mut shell,
            &viewport,
        );
        assert!(!shell.is_layout_invalid());
        assert!(shell.is_empty());
        shell.redraw_request()
    }

    #[test]
    fn unrelated_redraws_preserve_the_deadline_and_bound_animation_sampling() {
        let renderer = renderer();
        let started = Instant::now();
        let mut element = view(started, true, Color::WHITE);
        let mut tree = widget::Tree::new(&element);
        let mut samples = 0;
        let mut previous = None;
        let mut deadline = started;
        // Frequent input or another widget's animation must neither accelerate
        // these bars nor postpone their next sample indefinitely.
        for millis in 0..1000 {
            let at = started + Duration::from_millis(millis);
            let request = redraw(
                &mut element,
                &mut tree,
                &renderer,
                at,
                Rectangle::with_size(Size::new(100., 100.)),
            );
            let state = tree.state.downcast_ref::<FrameState>();
            if previous != Some(state.sampled) {
                samples += 1;
                if let Some(previous) = previous {
                    assert!(state.sampled.duration_since(previous) >= FRAME_INTERVAL);
                }
                assert_eq!(state.sampled, at);
            } else {
                assert_eq!(state.next_frame, deadline);
            }
            assert!(state.next_frame > at);
            assert_eq!(request, window::RedrawRequest::At(state.next_frame));
            previous = Some(state.sampled);
            deadline = state.next_frame;
        }
        assert!((23..=24).contains(&samples), "sampled {samples} times");
    }

    #[test]
    fn paused_transparent_and_clipped_bars_do_not_schedule_frames() {
        let renderer = renderer();
        let started = Instant::now();
        let viewport = Rectangle::with_size(Size::new(100., 100.));
        for (animate, color, viewport) in [
            (false, Color::WHITE, viewport),
            (true, Color::TRANSPARENT, viewport),
            (
                true,
                Color::WHITE,
                Rectangle::new((20., 20.).into(), Size::new(100., 100.)),
            ),
        ] {
            let mut element = view(started, animate, color);
            let mut tree = widget::Tree::new(&element);
            assert_eq!(
                redraw(&mut element, &mut tree, &renderer, started, viewport),
                window::RedrawRequest::Wait,
            );
        }
        // Partial clipping is still visible and needs its animation.
        let mut element = view(started, true, Color::WHITE);
        let mut tree = widget::Tree::new(&element);
        assert!(matches!(
            redraw(
                &mut element,
                &mut tree,
                &renderer,
                started,
                Rectangle::new((5., 5.).into(), Size::new(100., 100.)),
            ),
            window::RedrawRequest::At(_)
        ));
    }

    #[test]
    fn resumed_and_delayed_frames_sample_current_time_without_catch_up() {
        let renderer = renderer();
        let started = Instant::now();
        let viewport = Rectangle::with_size(Size::new(100., 100.));
        let mut element = view(started, true, Color::WHITE);
        let mut tree = widget::Tree::new(&element);
        redraw(&mut element, &mut tree, &renderer, started, viewport);
        for (animate, seconds) in [(false, 1), (true, 2), (true, 15)] {
            element = view(started, animate, Color::WHITE);
            tree.diff(&mut element);
            let at = started + Duration::from_secs(seconds);
            let request = redraw(&mut element, &mut tree, &renderer, at, viewport);
            if animate {
                assert_eq!(request, window::RedrawRequest::At(at + FRAME_INTERVAL));
                assert_eq!(tree.state.downcast_ref::<FrameState>().sampled, at);
            } else {
                assert_eq!(request, window::RedrawRequest::Wait);
            }
        }
    }
}
