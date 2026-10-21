//! Conservative output feedback for the existing primary-plane policy.
use std::collections::{HashMap, HashSet};

use smithay::backend::allocator::Format;
use smithay::backend::allocator::format::FormatSet;
use smithay::backend::renderer::element::{
    Element, Id, RenderElement, RenderElementPresentationState, RenderElementStates, RenderingReason,
};
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::backend::renderer::utils::with_renderer_surface_state;
use smithay::output::Output;
use smithay::reexports::wayland_protocols::wp::linux_dmabuf::zv1::server::zwp_linux_dmabuf_feedback_v1::TrancheFlags;
use smithay::utils::{Physical, Rectangle, Scale};
use smithay::wayland::dmabuf::{DmabufFeedback, DmabufFeedbackBuilder, DmabufGlobal};

use crate::Ferese;
use crate::render::AnimatedWindowRenderElement;

fn compatible(render: &FormatSet, primary: &FormatSet, observed: Format) -> bool {
    // Do not equate implicit and explicit modifiers, alpha and opaque FourCCs,
    // or formats from cursor/overlay planes. Exact intersection preserves import.
    render.contains(&observed) && primary.contains(&observed)
}

pub(super) struct OutputFeedback {
    render_device: libc::dev_t,
    scanout_device: libc::dev_t,
    render_formats: FormatSet,
    primary_formats: FormatSet,
    observed: Option<Format>,
    preferred: Option<DmabufFeedback>,
    visible: HashSet<Id>,
    candidates: HashSet<Id>,
}

impl OutputFeedback {
    pub fn new(
        render_device: libc::dev_t,
        scanout_device: libc::dev_t,
        render_formats: FormatSet,
        primary_formats: FormatSet,
    ) -> Self {
        Self {
            render_device,
            scanout_device,
            render_formats,
            primary_formats,
            observed: None,
            preferred: None,
            visible: HashSet::new(),
            candidates: HashSet::new(),
        }
    }

    pub fn invalidate(&mut self) {
        self.observed = None;
        self.preferred = None;
        self.visible.clear();
        self.candidates.clear();
    }

    fn observe_format(&mut self, format: Option<Format>) {
        let Some(format) = format else {
            return;
        };
        if self.observed == Some(format) {
            return;
        }
        self.observed = Some(format);
        self.preferred = None;
        if compatible(&self.render_formats, &self.primary_formats, format) {
            // Restrict the preference to the observed primary format/modifier:
            // without ALLOW_PRIMARY_PLANE_SCANOUT_ANY Smithay compares the full
            // format with the swapchain. Renderer-only formats remain in fallback.
            match DmabufFeedbackBuilder::new(
                self.render_device,
                self.render_formats.iter().copied(),
            )
            .add_preference_tranche(self.scanout_device, TrancheFlags::Scanout, [format], 4..=6)
            .build()
            {
                Ok(feedback) => self.preferred = Some(feedback),
                Err(error) => {
                    tracing::warn!(%error, "output DMA-BUF feedback unavailable; retaining render fallback")
                }
            }
        }
    }

    pub fn observed_frame(
        &mut self,
        format: Option<Format>,
        states: &RenderElementStates,
        candidates: HashSet<Id>,
    ) {
        self.observe_format(format);
        self.visible = states
            .states
            .iter()
            .filter(|(_, state)| crate::display_presentation::is_visible(**state))
            .map(|(id, _)| id.clone())
            .collect();
        self.candidates = candidates
            .into_iter()
            .filter(|id| {
                states.states.get(id).is_some_and(|state| {
                    crate::display_presentation::is_visible(*state)
                        && state.presentation_state
                            != RenderElementPresentationState::Rendering {
                                reason: Some(RenderingReason::ScanoutFailed),
                            }
                })
            })
            .collect();
    }
}

pub(super) fn candidates(
    renderer: &mut GlesRenderer,
    elements: &[AnimatedWindowRenderElement],
    output: &Output,
    allowed: bool,
) -> HashSet<Id> {
    if !allowed {
        return HashSet::new();
    }
    let Some(mode) = output.current_mode() else {
        return HashSet::new();
    };
    let bounds = Rectangle::from_size(output.current_transform().transform_size(mode.size));
    let scale = output.current_scale().fractional_scale().into();
    elements
        .iter()
        .filter(|&element| {
            candidate_geometry(element, bounds, scale, || {
                element.underlying_storage(renderer).is_some()
            })
        })
        .map(|element| element.id().clone())
        .collect()
}

fn candidate_geometry(
    element: &impl Element,
    bounds: Rectangle<i32, Physical>,
    scale: Scale<f64>,
    has_storage: impl FnOnce() -> bool,
) -> bool {
    let geometry = element.geometry(scale);
    if element.alpha() != 1.0 || !geometry.contains_rect(bounds) {
        return false;
    }
    // A single opaque rectangle covering the output is deliberately more
    // conservative than reconstructing an arbitrary union of client regions.
    element
        .opaque_regions(scale)
        .iter()
        .any(|region| Rectangle::new(region.loc + geometry.loc, region.size).contains_rect(bounds))
        && has_storage()
}

struct OutputView<'a> {
    global: DmabufGlobal,
    feedback: &'a OutputFeedback,
    eligible: Option<Id>,
}

fn select<'a>(
    global: DmabufGlobal,
    surface: &Id,
    outputs: impl IntoIterator<Item = OutputView<'a>>,
) -> Option<&'a DmabufFeedback> {
    let mut visible = outputs
        .into_iter()
        .filter(|output| output.feedback.visible.contains(surface));
    let output = visible.next()?;
    // A surface visible on multiple outputs gets render fallback rather than
    // whichever output happened to render last. Cross-GPU import is not assumed.
    if visible.next().is_some()
        || output.global != global
        || output.eligible.as_ref() != Some(surface)
        || !output.feedback.candidates.contains(surface)
    {
        return None;
    }
    output.feedback.preferred.as_ref()
}

pub(crate) fn refresh(state: &mut Ferese) {
    let mut preferred = HashMap::new();
    if let Some(backend) = state.direct_backend.as_ref() {
        for (surface, global) in state.dmabuf_feedback.requested() {
            let id = Id::from(surface);
            let outputs = backend
                .devices
                .values()
                .flat_map(|device| device.outputs.values().map(move |output| (device, output)))
                .filter(|(_, output)| state.output_id(&output.output).is_some());
            let views = outputs.map(|(device, output)| {
                let eligible = (backend.can_render()
                    && device.drm.is_active()
                    && output.power.can_render()
                    && output.mirror_source.is_none()
                    && !state.session_lock.active()
                    && !state.output_has_animations(&output.output)
                    && state.output_has_fullscreen(&output.output))
                .then(|| {
                    state
                        .output_id(&output.output)
                        .and_then(|id| state.output_workspaces.active_workspace(id))
                        .and_then(|workspace| state.workspaces.workspace(workspace))
                        .and_then(|workspace| workspace.fullscreen)
                        .filter(|id| {
                            state
                                .current_window_presentation(*id)
                                .is_some_and(|presentation| {
                                    presentation.alpha() == 1.0 && presentation.dim == 0.0
                                })
                        })
                        .and_then(|id| state.windows.window(id))
                        .and_then(|window| window.toplevel())
                        .filter(|toplevel| {
                            with_renderer_surface_state(toplevel.wl_surface(), |state| {
                                state.buffer().is_some()
                            })
                            .unwrap_or(false)
                        })
                        .map(|toplevel| Id::from(toplevel.wl_surface()))
                })
                .flatten();
                OutputView {
                    global: device.dmabuf_global,
                    feedback: &output.dmabuf_feedback,
                    eligible,
                }
            });
            if let Some(feedback) = select(global, &id, views) {
                preferred.insert(surface.clone(), feedback.clone());
            }
        }
    }
    state.dmabuf_feedback.refresh(&preferred);
}

#[cfg(test)]
mod tests {
    use super::*;
    use smithay::backend::allocator::{Fourcc, Modifier};
    use smithay::backend::renderer::element::{RenderElementPresentationState, RenderElementState};
    use smithay::reexports::wayland_server::Display;
    use smithay::wayland::dmabuf::DmabufState;

    fn format(code: Fourcc, modifier: Modifier) -> Format {
        Format { code, modifier }
    }
    fn formats(values: &[Format]) -> FormatSet {
        values.iter().copied().collect()
    }
    fn output(render: &[Format], primary: &[Format]) -> OutputFeedback {
        OutputFeedback::new(10, 20, formats(render), formats(primary))
    }
    fn create_globals() -> (Display<Ferese>, DmabufGlobal, DmabufGlobal) {
        let display = Display::new().unwrap();
        let mut state = DmabufState::new();
        let feedback = DmabufFeedbackBuilder::new(10, [format(Fourcc::Argb8888, Modifier::Linear)])
            .build()
            .unwrap();
        let global =
            state.create_global_with_default_feedback::<Ferese>(&display.handle(), &feedback);
        let other =
            state.create_global_with_default_feedback::<Ferese>(&display.handle(), &feedback);
        (display, global, other)
    }
    fn visible(output: &mut OutputFeedback, id: &Id, f: Format) {
        let mut states = RenderElementStates::default();
        states.states.insert(
            id.clone(),
            RenderElementState {
                needs_capture: false,
                visible_area: 100,
                presentation_state: RenderElementPresentationState::Rendering { reason: None },
            },
        );
        output.observed_frame(Some(f), &states, HashSet::from([id.clone()]));
    }

    #[test]
    fn preferences_require_exact_renderer_and_primary_intersection() {
        let linear = format(Fourcc::Argb8888, Modifier::Linear);
        let implicit = format(Fourcc::Argb8888, Modifier::Invalid);
        let opaque = format(Fourcc::Xrgb8888, Modifier::Linear);
        assert!(compatible(
            &formats(&[linear, implicit]),
            &formats(&[linear]),
            linear
        ));
        assert!(!compatible(
            &formats(&[linear]),
            &formats(&[implicit]),
            linear
        ));
        assert!(!compatible(
            &formats(&[linear]),
            &formats(&[opaque]),
            linear
        ));
        assert!(!compatible(
            &formats(&[linear]),
            &formats(&[implicit]),
            implicit
        ));
        assert!(!compatible(
            &formats(&[opaque]),
            &formats(&[linear]),
            linear
        ));
    }

    #[test]
    fn observed_format_limits_preference_and_retains_renderer_fallback() {
        let linear = format(Fourcc::Argb8888, Modifier::Linear);
        let other = format(Fourcc::Abgr8888, Modifier::Linear);
        let mut o = output(&[linear, other], &[linear, other]);
        assert!(o.preferred.is_none());
        o.observe_format(Some(linear));
        let expected = DmabufFeedbackBuilder::new(10, [linear, other])
            .add_preference_tranche(20, TrancheFlags::Scanout, [linear], 4..=6)
            .build()
            .unwrap();
        assert_eq!(o.preferred.as_ref(), Some(&expected));
        o.observe_format(Some(other));
        assert_ne!(o.preferred.as_ref(), Some(&expected));
        o.observe_format(Some(format(Fourcc::Rgb565, Modifier::Linear)));
        assert!(o.preferred.is_none());
    }

    #[test]
    fn migration_multi_output_cross_gpu_and_policy_changes_fall_back() {
        let (_display, global, other_global) = create_globals();
        let id = Id::new();
        let f = format(Fourcc::Argb8888, Modifier::Linear);
        let mut a = output(&[f], &[f]);
        let mut b = output(&[f], &[f]);
        visible(&mut a, &id, f);
        visible(&mut b, &id, f);
        fn view(
            global: DmabufGlobal,
            feedback: &OutputFeedback,
            eligible: Option<Id>,
        ) -> OutputView<'_> {
            OutputView {
                global,
                feedback,
                eligible,
            }
        }
        assert!(select(global, &id, [view(global, &a, Some(id.clone()))]).is_some());
        assert!(select(global, &id, [view(global, &a, None)]).is_none());
        assert!(
            select(
                global,
                &id,
                [
                    view(global, &a, Some(id.clone())),
                    view(global, &b, Some(id.clone()))
                ]
            )
            .is_none()
        );
        assert!(select(other_global, &id, [view(global, &a, Some(id.clone()))]).is_none());
        a.invalidate();
        assert!(select(global, &id, [view(global, &a, Some(id.clone()))]).is_none());
        assert!(
            select(
                global,
                &id,
                [
                    view(global, &a, Some(id.clone())),
                    view(global, &b, Some(id.clone()))
                ]
            )
            .is_some()
        );
        b.candidates.clear();
        assert!(select(global, &id, [view(global, &b, Some(id.clone()))]).is_none());
    }

    #[test]
    fn reconfiguration_invalidates_observations_until_a_new_frame() {
        let (_display, global, _) = create_globals();
        let id = Id::new();
        let f = format(Fourcc::Argb8888, Modifier::Linear);
        let mut o = output(&[f], &[f]);
        visible(&mut o, &id, f);
        o.invalidate();
        assert!(
            o.observed.is_none()
                && o.preferred.is_none()
                && o.visible.is_empty()
                && o.candidates.is_empty()
        );
        o.observe_format(None);
        assert!(o.preferred.is_none());
        visible(&mut o, &id, f);
        assert!(
            select(
                global,
                &id,
                [OutputView {
                    global,
                    feedback: &o,
                    eligible: Some(id.clone())
                }]
            )
            .is_some()
        );
    }

    #[test]
    fn candidate_geometry_requires_covering_opaque_content_and_storage() {
        use smithay::backend::renderer::element::Kind;
        use smithay::backend::renderer::element::solid::SolidColorRenderElement;
        use smithay::backend::renderer::utils::CommitCounter;
        use smithay::utils::Transform;
        for transform in [Transform::Normal, Transform::_90] {
            let bounds = Rectangle::from_size(transform.transform_size((100, 60).into()));
            for scale in [1.0, 1.25, 2.0] {
                let solid = |geometry, alpha| {
                    SolidColorRenderElement::new(
                        Id::new(),
                        geometry,
                        CommitCounter::default(),
                        [1., 1., 1., alpha],
                        Kind::Unspecified,
                    )
                };
                let opaque = solid(bounds, 1.0);
                assert!(candidate_geometry(&opaque, bounds, scale.into(), || true));
                assert!(
                    !candidate_geometry(&opaque, bounds, scale.into(), || false),
                    "clipped storage must remain hidden"
                );
                let faded = solid(bounds, 0.8);
                assert!(!candidate_geometry(
                    &faded,
                    bounds,
                    scale.into(),
                    || panic!("faded elements must not probe storage")
                ));
                let mut partial = bounds;
                partial.size.w -= 1;
                assert!(!candidate_geometry(
                    &solid(partial, 1.0),
                    bounds,
                    scale.into(),
                    || panic!("partial content must not probe storage")
                ));
            }
        }
    }

    #[test]
    fn failed_scanout_and_occlusion_withdraw_preferences_but_format_hints_can_recover() {
        let (_display, global, _) = create_globals();
        let id = Id::new();
        let f = format(Fourcc::Argb8888, Modifier::Linear);
        let mut o = output(&[f], &[f]);
        for (state, expected) in [
            (
                RenderElementState {
                    needs_capture: false,
                    visible_area: 100,
                    presentation_state: RenderElementPresentationState::Rendering {
                        reason: Some(RenderingReason::FormatUnsupported),
                    },
                },
                true,
            ),
            (
                RenderElementState {
                    needs_capture: false,
                    visible_area: 100,
                    presentation_state: RenderElementPresentationState::Rendering {
                        reason: Some(RenderingReason::ScanoutFailed),
                    },
                },
                false,
            ),
            (
                RenderElementState {
                    needs_capture: false,
                    visible_area: 0,
                    presentation_state: RenderElementPresentationState::Skipped,
                },
                false,
            ),
            (
                RenderElementState {
                    needs_capture: false,
                    visible_area: 100,
                    presentation_state: RenderElementPresentationState::ZeroCopy,
                },
                true,
            ),
        ] {
            let states = RenderElementStates {
                states: HashMap::from([(id.clone(), state)]),
            };
            o.observed_frame(Some(f), &states, HashSet::from([id.clone()]));
            assert_eq!(
                select(
                    global,
                    &id,
                    [OutputView {
                        global,
                        feedback: &o,
                        eligible: Some(id.clone())
                    }]
                )
                .is_some(),
                expected
            );
        }
    }

    #[test]
    fn queue_failure_never_publishes_prepared_feedback_observations() {
        let f = format(Fourcc::Argb8888, Modifier::Linear);
        let id = Id::new();
        let mut o = output(&[f], &[f]);
        let failed = super::super::capture::after_queue(Err::<(), _>("queue failed"), || {
            visible(&mut o, &id, f)
        });
        assert!(failed.is_err());
        assert!(o.visible.is_empty() && o.preferred.is_none());
        super::super::capture::after_queue(Ok::<(), ()>(()), || visible(&mut o, &id, f)).unwrap();
        assert!(o.visible.contains(&id) && o.preferred.is_some());
    }
}
