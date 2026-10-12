//! A small ring using Iced's native canvas and Ferese's shared spring. The
//! stock progress widget owns an unconditional easing loop, so this version
//! exposes reduced-motion and visibility policy explicitly.
use crate::Message;
use cosmic::iced::widget::{Action, Canvas, canvas};
use cosmic::iced::{Color, Point, Radians, Rectangle, mouse, window};
use cosmic::{Element, Theme};
use ferese_animation::{AnimatedValue, SpringConfig};
use std::time::{Duration, Instant};

pub(super) fn view<'a>(
    progress: Option<f32>,
    animate: bool,
    reduced: bool,
    color: Color,
) -> Element<'a, cosmic::Action<Message>> {
    Canvas::new(Ring {
        progress,
        animate,
        reduced,
        color,
    })
    .width(26)
    .height(26)
    .into()
}
struct Ring {
    progress: Option<f32>,
    animate: bool,
    reduced: bool,
    color: Color,
}
pub(crate) struct RingState {
    value: AnimatedValue,
    sampled: Option<Instant>,
    elapsed: Duration,
}
impl Default for RingState {
    fn default() -> Self {
        Self {
            value: AnimatedValue::new(0.),
            sampled: None,
            elapsed: Duration::ZERO,
        }
    }
}
impl RingState {
    fn advance(&mut self, progress: Option<f32>, animate: bool, reduced: bool, now: Instant) -> bool {
        let elapsed = self
            .sampled
            .map_or(Duration::ZERO, |before| now.saturating_duration_since(before));
        let initial = self.sampled.is_none();
        self.sampled = Some(now);
        self.elapsed = self.elapsed.saturating_add(elapsed);
        self.value.retarget_preserving_motion(f64::from(progress.unwrap_or(0.)));
        if initial || reduced || !animate {
            self.value.snap();
        } else {
            self.value.advance(
                elapsed,
                SpringConfig {
                    stiffness: 700.,
                    damping: 45.,
                    position_tolerance: 0.002,
                    velocity_tolerance: 0.002,
                    ..Default::default()
                },
            );
        }
        animate && !reduced && (progress.is_none() || self.value.is_animating())
    }
}
impl canvas::Program<cosmic::Action<Message>, Theme, cosmic::Renderer> for Ring {
    type State = RingState;
    fn update(
        &self,
        state: &mut RingState,
        event: &canvas::Event,
        _bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> Option<Action<cosmic::Action<Message>>> {
        if let canvas::Event::Window(window::Event::RedrawRequested(now)) = event
            && state.advance(self.progress, self.animate, self.reduced, *now)
        {
            return Some(Action::request_redraw());
        }
        None
    }
    fn draw(
        &self,
        state: &RingState,
        renderer: &cosmic::Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> Vec<canvas::Geometry<cosmic::Renderer>> {
        let mut frame = canvas::Frame::new(renderer, bounds.size());
        let center = Point::new(bounds.width / 2., bounds.height / 2.);
        let radius = bounds.width.min(bounds.height) / 2. - 1.5;
        frame.stroke(
            &canvas::Path::circle(center, radius),
            canvas::Stroke::default()
                .with_color(self.color.scale_alpha(0.15))
                .with_width(1.5),
        );
        let (start, fraction) = if self.progress.is_some() {
            (
                -std::f32::consts::FRAC_PI_2,
                if state.sampled.is_none() {
                    self.progress.unwrap_or(0.)
                } else {
                    state.value.current as f32
                }
                .clamp(0., 1.),
            )
        } else if self.animate && !self.reduced {
            (state.elapsed.as_secs_f32() / 3. * std::f32::consts::TAU, 0.23)
        } else {
            (0., 0.)
        };
        if fraction > 0. {
            let arc = canvas::Path::new(|path| {
                path.arc(canvas::path::Arc {
                    center,
                    radius,
                    start_angle: Radians(start),
                    end_angle: Radians(start + fraction * std::f32::consts::TAU),
                })
            });
            frame.stroke(
                &arc,
                canvas::Stroke::default()
                    .with_color(self.color)
                    .with_width(1.5)
                    .with_line_cap(canvas::LineCap::Round),
            );
        }
        vec![frame.into_geometry()]
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ring_stops_when_settled_hidden_or_reduced_and_keeps_interrupted_velocity() {
        let now = Instant::now();
        let mut state = RingState::default();
        assert!(!state.advance(Some(0.1), true, false, now));
        assert!(state.advance(Some(0.6), true, false, now + Duration::from_millis(50)));
        let velocity = state.value.velocity;
        state.value.retarget_preserving_motion(0.9);
        assert_eq!(state.value.velocity, velocity);
        for frame in 1..=200 {
            state.advance(Some(0.9), true, false, now + Duration::from_millis(50 + frame * 16));
        }
        assert!(!state.value.is_animating());
        assert!(!state.advance(None, false, false, now + Duration::from_secs(5)));
        assert!(!state.advance(Some(0.2), true, true, now + Duration::from_secs(6)));
        assert_eq!(state.value.current, 0.2_f32 as f64);
        assert!(state.advance(None, true, false, now + Duration::from_secs(7)));
    }
}
