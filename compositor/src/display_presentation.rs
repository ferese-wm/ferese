//! Displayed content and input transforms, independent of offscreen capture.
use std::collections::{HashMap, HashSet};

use ferese_animation::ClientSize;
use ferese_layout::{Rect, WindowId};
use smithay::backend::renderer::element::{
    Id, RenderElementPresentationState, RenderElementState, RenderElementStates,
};
use smithay::output::Output;

#[derive(Default)]
struct DisplayFrame {
    surfaces: HashSet<Id>,
    windows: HashMap<WindowId, WindowInput>,
}

#[derive(Default)]
struct OutputPresentation {
    displayed: Option<DisplayFrame>,
    queued: Option<DisplayFrame>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct WindowInput {
    pub bounds: Rect,
    pub scale: (f64, f64),
}

impl WindowInput {
    pub fn new(
        presentation: crate::presentation::WindowPresentation,
        committed: Option<ClientSize>,
        output: &Output,
    ) -> Self {
        let scale = output.current_scale().fractional_scale();
        let origin = output.current_location();
        let pixels = crate::presentation::physical_rect(presentation.bounds.current, origin, scale);
        let bounds = Rect::new(
            f64::from(origin.x) + f64::from(pixels.loc.x) / scale,
            f64::from(origin.y) + f64::from(pixels.loc.y) / scale,
            f64::from(pixels.size.w) / scale,
            f64::from(pixels.size.h) / scale,
        );
        let source = if presentation.scale_content {
            committed
        } else {
            presentation.native_size
        };
        Self {
            bounds,
            scale: source.map_or((1.0, 1.0), |size| {
                (
                    f64::from(pixels.size.w) / (f64::from(size.width) * scale).round().max(1.0),
                    f64::from(pixels.size.h) / (f64::from(size.height) * scale).round().max(1.0),
                )
            }),
        }
    }

    pub fn inverse(self, x: f64, y: f64) -> (f64, f64) {
        (
            (x - self.bounds.x) / self.scale.0,
            (y - self.bounds.y) / self.scale.1,
        )
    }
}

#[derive(Default)]
pub(crate) struct DisplayPresentation {
    outputs: HashMap<Output, OutputPresentation>,
}

pub(crate) fn is_visible(state: RenderElementState) -> bool {
    state.visible_area > 0 && state.presentation_state != RenderElementPresentationState::Skipped
}

fn visible(states: &RenderElementStates) -> HashSet<Id> {
    states
        .states
        .iter()
        .filter(|&(_id, state)| is_visible(*state))
        .map(|(id, _state)| id.clone())
        .collect()
}

impl DisplayPresentation {
    // Call only after a successful submission, never from an offscreen capture.
    pub fn queued(
        &mut self,
        output: &Output,
        states: &RenderElementStates,
        windows: impl IntoIterator<Item = (WindowId, WindowInput)>,
    ) {
        self.outputs.entry(output.clone()).or_default().queued = Some(DisplayFrame {
            surfaces: visible(states),
            windows: windows.into_iter().collect(),
        });
    }

    pub fn presented(&mut self, output: &Output) {
        if let Some(record) = self.outputs.get_mut(output)
            && let Some(queued) = record.queued.take()
        {
            record.displayed = Some(queued);
        }
    }

    #[cfg(test)]
    pub fn visible(&self, surface: &Id) -> bool {
        self.outputs.values().any(|record| {
            record
                .displayed
                .as_ref()
                .is_some_and(|frame| frame.surfaces.contains(surface))
        })
    }

    pub fn outputs_for<'a>(&'a self, surface: &'a Id) -> impl Iterator<Item = &'a Output> {
        self.outputs
            .iter()
            .filter(|(_, record)| {
                record
                    .displayed
                    .as_ref()
                    .is_some_and(|frame| frame.surfaces.contains(surface))
            })
            .map(|(output, _)| output)
    }

    pub fn has_presented(&self, output: &Output) -> bool {
        self.outputs
            .get(output)
            .is_some_and(|record| record.displayed.is_some())
    }

    pub fn window(&self, output: &Output, id: WindowId) -> Option<WindowInput> {
        self.outputs
            .get(output)?
            .displayed
            .as_ref()?
            .windows
            .get(&id)
            .copied()
    }

    pub fn remove_output(&mut self, output: &Output) {
        self.outputs.remove(output);
    }

    pub fn clear(&mut self) {
        self.outputs.clear();
    }
}

#[cfg(test)]
mod tests {
    use smithay::backend::renderer::element::RenderElementState;
    use smithay::output::{PhysicalProperties, Subpixel};

    use super::*;

    fn output(name: &str) -> Output {
        Output::new(
            name.into(),
            PhysicalProperties {
                serial_number: String::new(),
                size: (0, 0).into(),
                subpixel: Subpixel::Unknown,
                make: "test".into(),
                model: "test".into(),
            },
        )
    }

    fn scene(id: &Id) -> RenderElementStates {
        let mut states = RenderElementStates::default();
        states.states.insert(
            id.clone(),
            RenderElementState {
                needs_capture: false,
                visible_area: 20,
                presentation_state: RenderElementPresentationState::ZeroCopy,
            },
        );
        states
    }

    #[test]
    fn visibility_changes_only_when_a_submission_is_presented() {
        let mut record = DisplayPresentation::default();
        let output = output("a");
        let id = Id::new();
        record.queued(&output, &scene(&id), []);
        assert!(!record.visible(&id));
        record.presented(&output);
        assert!(record.visible(&id));
        record.presented(&output); // No new submission keeps the last displayed scene.
        assert!(record.visible(&id));

        record.queued(&output, &RenderElementStates::default(), []);
        assert!(record.visible(&id));
        record.presented(&output);
        assert!(!record.visible(&id));
    }

    #[test]
    fn outputs_are_independent_and_queued_history_survives_until_presentation() {
        let mut record = DisplayPresentation::default();
        let a = output("a");
        let b = output("b");
        let id = Id::new();
        for output in [&a, &b] {
            record.queued(output, &scene(&id), []);
            record.presented(output);
        }
        record.remove_output(&a);
        assert!(record.visible(&id));
        record.queued(&b, &scene(&id), []);
        assert!(record.visible(&id));
        record.presented(&b);
        assert!(record.visible(&id));
        record.clear();
        assert!(!record.visible(&id));
    }

    #[test]
    fn occluded_and_skipped_content_do_not_count_as_visible() {
        let id = Id::new();
        let mut states = scene(&id);
        states.states.get_mut(&id).unwrap().visible_area = 0;
        assert!(visible(&states).is_empty());
        states.states.get_mut(&id).unwrap().visible_area = 20;
        states.states.get_mut(&id).unwrap().presentation_state =
            RenderElementPresentationState::Skipped;
        assert!(visible(&states).is_empty());
    }

    #[test]
    fn input_transforms_retire_per_output_and_empty_frames_remove_them() {
        let mut history = DisplayPresentation::default();
        let a = output("a");
        let b = output("b");
        let id = WindowId(1);
        let first = WindowInput {
            bounds: Rect::new(20., 30., 100., 80.),
            scale: (1., 1.),
        };
        let next = WindowInput {
            bounds: Rect::new(200., 300., 200., 160.),
            scale: (2., 2.),
        };
        let states = RenderElementStates::default();
        history.queued(&a, &states, [(id, first)]);
        assert!(!history.has_presented(&a));
        history.presented(&a);
        history.queued(&a, &states, [(id, next)]);
        history.queued(&b, &states, [(id, next)]);
        history.presented(&b);
        assert_eq!(history.window(&a, id), Some(first));
        assert_eq!(history.window(&b, id), Some(next));
        assert_eq!(
            history.window(&b, id).unwrap().inverse(240., 340.),
            (20., 20.)
        );
        history.presented(&a);
        assert_eq!(history.window(&a, id), Some(next));
        history.queued(&a, &states, []);
        history.presented(&a);
        assert!(history.has_presented(&a));
        assert_eq!(history.window(&a, id), None);
        assert_eq!(history.window(&b, id), Some(next));
        history.remove_output(&b);
        assert!(!history.has_presented(&b));
        assert_eq!(history.window(&b, id), None);
    }

    #[test]
    fn input_mapping_uses_the_snapped_frame_and_its_original_content_scale() {
        use ferese_animation::{AnimatedRect, AnimatedValue};
        let output = output("fractional");
        for (scale, source_midpoint) in [
            (1., (32.5, 23.5)),
            (1.25, (32.4, 23.6)),
            (1.5, (98. / 3., 71. / 3.)),
            (2., (32.5, 23.5)),
        ] {
            output.change_current_state(
                None,
                None,
                Some(smithay::output::Scale::Fractional(scale)),
                Some((800, 0).into()),
            );
            let mut presentation = crate::presentation::WindowPresentation {
                id: WindowId(1),
                bounds: AnimatedRect::new(Rect::new(810.3, 20.6, 120.4, 80.3)),
                opacity: AnimatedValue::new(1.),
                window_alpha: 1.,
                dim: 0.,
                emphasis: AnimatedValue::new(0.),
                shadow: AnimatedValue::new(0.),
                scale_content: false,
                native_size: None,
            };
            let native = WindowInput::new(
                presentation,
                Some(ClientSize {
                    width: 65,
                    height: 47,
                }),
                &output,
            );
            assert_eq!(
                native.scale,
                (1., 1.),
                "normal resizing crops rather than stretches content"
            );
            let snapped = crate::presentation::physical_rect(
                presentation.bounds.current,
                (800, 0).into(),
                scale,
            );
            assert!(((native.bounds.x - 800.) * scale - f64::from(snapped.loc.x)).abs() < 1e-10);
            presentation.native_size = Some(ClientSize {
                width: 130,
                height: 90,
            });
            let opening = WindowInput::new(presentation, None, &output);
            assert_eq!(
                opening.scale.0,
                f64::from(snapped.size.w) / (130. * scale).round()
            );
            presentation.scale_content = true;
            let thumbnail = WindowInput::new(
                presentation,
                Some(ClientSize {
                    width: 65,
                    height: 47,
                }),
                &output,
            );
            let point = thumbnail.inverse(
                thumbnail.bounds.x + thumbnail.bounds.width / 2.,
                thumbnail.bounds.y + thumbnail.bounds.height / 2.,
            );
            assert!((point.0 - source_midpoint.0).abs() < 1e-10);
            assert!((point.1 - source_midpoint.1).abs() < 1e-10);
        }
    }
}
