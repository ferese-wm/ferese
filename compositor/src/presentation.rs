//! Presentation geometry is rounded once, in output pixels, never first in
//! logical pixels. Content, clips and decorations share those exact edges.
use std::time::{Duration, Instant};

use ferese_animation::AnimatedRect;

use smithay::backend::renderer::element::{Element, Id, Kind, RenderElement, RenderElementStates};
use smithay::backend::renderer::gles::{GlesError, GlesFrame, GlesRenderer, GlesTexProgram, GlesTexture, Uniform};
use smithay::backend::renderer::utils::{CommitCounter, DamageSet};
use smithay::desktop::utils::OutputPresentationFeedback;
use smithay::output::Output;
use smithay::reexports::wayland_protocols::wp::presentation_time::server::wp_presentation_feedback;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{Buffer, Logical, Physical, Point, Rectangle, Scale, Transform};
use smithay::wayland::compositor::SurfaceData;

use crate::render::SharedPixelShaderElement;

pub(crate) const HANDOFF: Duration = Duration::from_millis(80);
pub(crate) const SNAPSHOT_BUDGET: usize = 64 * 1024 * 1024;

/// One window identity and motion sample, regardless of whether its pixels
/// come from a live surface, a resize snapshot, or a retained close image.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct WindowPresentation {
    pub id: ferese_layout::WindowId,
    pub bounds: AnimatedRect,
    pub opacity: ferese_animation::AnimatedValue,
    pub focus_alpha: f32,
    pub dim: f64,
    pub emphasis: ferese_animation::AnimatedValue,
    pub shadow: ferese_animation::AnimatedValue,
    pub scale_content: bool,
    pub native_size: Option<ferese_animation::ClientSize>,
}

impl WindowPresentation {
    pub fn alpha(self) -> f32 {
        self.focus_alpha * self.opacity.current.clamp(0.0, 1.0) as f32
    }

    pub fn focus(self) -> f64 {
        self.emphasis.current.clamp(0.0, 1.0)
    }

    /// A thumbnail is another view of the same content and lifecycle state.
    pub fn thumbnail(mut self, rect: ferese_layout::Rect) -> Self {
        self.bounds = AnimatedRect::new(rect);
        self.scale_content = true;
        self.native_size = None;
        self
    }

    pub fn close(&mut self) {
        self.bounds.set_target(scaled_visual_rect(self.bounds.current, 0.98));
        self.opacity.set_target(0.0);
        self.emphasis.set_target(0.0);
        self.shadow.set_target(0.0);
        self.scale_content = true;
        self.native_size = None;
    }

    /// Shadow intensity and elevation may respond independently; its outline
    /// always follows the window's shared rounded geometry.
    pub fn shadow_factors(self) -> (f64, f64, f64) {
        let strength = self.shadow.current.clamp(0.0, 1.0);
        (0.75 + 0.25 * strength, 0.9 + 0.1 * strength, 0.8 + 0.2 * strength)
    }
}

// Response multipliers preserve damping ratio and the user's crossing policy.
// Emphasis responds sooner; the shadow settles a little later than the bounds.
pub(crate) fn emphasis_spring(base: ferese_animation::SpringConfig) -> ferese_animation::SpringConfig {
    visual_response(base, 0.8)
}

pub(crate) fn shadow_spring(base: ferese_animation::SpringConfig) -> ferese_animation::SpringConfig {
    visual_response(base, 1.2)
}

fn visual_response(base: ferese_animation::SpringConfig, response: f64) -> ferese_animation::SpringConfig {
    ferese_animation::SpringConfig {
        stiffness: base.stiffness / (response * response),
        damping: base.damping / response,
        position_tolerance: 0.001,
        velocity_tolerance: 0.001,
        ..base
    }
}

pub(crate) fn take_output_feedback(
    state: &crate::Ferese,
    output: &Output,
    rendered: &RenderElementStates,
    flags: wp_presentation_feedback::Kind,
) -> OutputPresentationFeedback {
    use smithay::desktop::layer_map_for_output;
    use smithay::desktop::utils::{OutputPresentationFeedback, surface_presentation_feedback_flags_from_states};

    let mut feedback = OutputPresentationFeedback::new(output);
    let visible_output =
        |surface: &WlSurface, _: &SurfaceData| rendered_feedback_output(output, surface.into(), rendered);
    let surface_flags = |surface: &WlSurface, _: &SurfaceData| {
        flags | surface_presentation_feedback_flags_from_states(surface, rendered)
    };

    for window in state.space.elements().filter(|window| {
        state
            .windows
            .ids()
            .get(*window)
            .is_some_and(|id| state.window_belongs_to_output(*id, output))
    }) {
        window.take_presentation_feedback(&mut feedback, visible_output, surface_flags);
    }

    for layer in layer_map_for_output(output).layers() {
        layer.take_presentation_feedback(&mut feedback, visible_output, surface_flags);
    }

    feedback
}

fn rendered_feedback_output(output: &Output, id: Id, rendered: &RenderElementStates) -> Option<Output> {
    rendered
        .element_render_state(id)
        .is_some_and(crate::display_presentation::is_visible)
        .then(|| output.clone())
}

pub(crate) fn frame_delta(last: &mut Instant, now: Instant) -> Duration {
    let delta = now.saturating_duration_since(*last);
    *last = (*last).max(now);
    delta
}

pub(crate) fn physical_rect(
    rect: ferese_layout::Rect,
    origin: Point<i32, Logical>,
    scale: f64,
) -> Rectangle<i32, Physical> {
    let left = ((rect.x - f64::from(origin.x)) * scale).round() as i32;
    let top = ((rect.y - f64::from(origin.y)) * scale).round() as i32;
    let right = ((rect.x + rect.width - f64::from(origin.x)) * scale).round() as i32;
    let bottom = ((rect.y + rect.height - f64::from(origin.y)) * scale).round() as i32;

    Rectangle::new(
        (left, top).into(),
        ((right - left).max(1), (bottom - top).max(1)).into(),
    )
}

pub(crate) use ferese_shape::Shape as CornerShape;

/// Conservative extent of the corner shoulders, in physical pixels.
pub(crate) fn corner_extent(radius: f32, size: smithay::utils::Size<i32, Physical>, shape: CornerShape) -> f32 {
    if shape == CornerShape::Circular {
        return radius;
    }

    (radius * 1.528_665).min(size.w.min(size.h) as f32 * 0.5)
}

/// A single physical outline shared by content and all of its decorations.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct RoundedRect {
    pub rect: Rectangle<i32, Physical>,
    pub radius: f32,
    pub shape: CornerShape,
}

impl RoundedRect {
    pub(crate) fn with_shape(mut self, shape: CornerShape) -> Self {
        self.shape = shape;
        self
    }

    pub(crate) fn new(rect: ferese_layout::Rect, origin: Point<i32, Logical>, scale: f64, radius: f64) -> Self {
        let rect = physical_rect(rect, origin, scale);
        Self {
            radius: clamp_radius(radius * scale, rect.size),
            rect,
            shape: CornerShape::Circular,
        }
    }

    pub(crate) fn from_logical(rect: Rectangle<i32, Logical>, scale: f64, radius: f64) -> Self {
        Self::new(
            ferese_layout::Rect::new(
                rect.loc.x as f64,
                rect.loc.y as f64,
                rect.size.w as f64,
                rect.size.h as f64,
            ),
            (0, 0).into(),
            scale,
            radius,
        )
    }
}

pub(crate) fn clamp_radius(requested: f64, size: smithay::utils::Size<i32, Physical>) -> f32 {
    requested.min(f64::from(size.w.min(size.h)) / 2.0).max(0.0) as f32
}

pub(crate) fn handoff_alpha(elapsed: Duration) -> f32 {
    let progress = (elapsed.as_secs_f64() / HANDOFF.as_secs_f64()).clamp(0.0, 1.0);
    (1.0 - progress * progress * (3.0 - 2.0 * progress)) as f32
}

pub(crate) fn resize_needs_old_frame(
    visual: ferese_layout::Rect,
    target: ferese_layout::Rect,
    buffer_width: i32,
    buffer_height: i32,
) -> bool {
    // Some clients round their configured dimensions to a character grid.
    // Do not retain a snapshot forever for that permanent size difference.
    visual.width > target.width.max(f64::from(buffer_width)) + 0.5
        || visual.height > target.height.max(f64::from(buffer_height)) + 0.5
}

pub(crate) fn snapshot_covers_source(
    snapshot: smithay::utils::Size<i32, Buffer>,
    source: smithay::utils::Size<i32, Physical>,
    snapshot_scale: f64,
    scale: f64,
) -> bool {
    (snapshot_scale - scale).abs() < 0.001 && snapshot.w >= source.w && snapshot.h >= source.h
}

pub(crate) fn advance_handoff(
    elapsed: &mut Duration,
    last: &mut Option<Duration>,
    now: Duration,
    blocked: bool,
    speed: f64,
) -> bool {
    if blocked {
        // A retarget must not consume its client's waiting time as fade time.
        *last = None;
    } else {
        if let Some(previous) = last.replace(now) {
            *elapsed += now.saturating_sub(previous).mul_f64(speed);
        }
    }
    *elapsed < HANDOFF
}

/// A shader canvas whose damage/paint destination uses the shared pixel edges.
#[derive(Clone, Debug)]
pub(crate) struct PhysicalShaderElement {
    pub inner: SharedPixelShaderElement,
    pub geometry: Rectangle<i32, Physical>,
}

impl Element for PhysicalShaderElement {
    fn id(&self) -> &Id {
        self.inner.id()
    }

    fn current_commit(&self) -> CommitCounter {
        self.inner.current_commit()
    }

    fn src(&self) -> Rectangle<f64, Buffer> {
        self.inner.src()
    }

    fn geometry(&self, _: Scale<f64>) -> Rectangle<i32, Physical> {
        self.geometry
    }

    fn damage_since(&self, _: Scale<f64>, commit: Option<CommitCounter>) -> DamageSet<i32, Physical> {
        if commit == Some(self.current_commit()) {
            DamageSet::default()
        } else {
            DamageSet::from_slice(&[Rectangle::from_size(self.geometry.size)])
        }
    }
}

impl RenderElement<GlesRenderer> for PhysicalShaderElement {
    fn draw(
        &self,
        frame: &mut GlesFrame<'_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        opaque: &[Rectangle<i32, Physical>],
    ) -> Result<(), GlesError> {
        self.inner.draw(frame, src, dst, damage, opaque)
    }
}

/// Stable native-pixel texture element. Unlike a resized client buffer, an old
/// frame is only translated/cropped; it never changes the size of its glyphs.
#[derive(Clone, Debug)]
pub(crate) struct NativeTextureElement {
    pub id: Id,
    pub commit: CommitCounter,
    pub texture: GlesTexture,
    pub geometry: Rectangle<i32, Physical>,
    pub source: Rectangle<f64, Buffer>,
    pub alpha: f32,
    pub program: Option<GlesTexProgram>,
    pub uniforms: Vec<Uniform<'static>>,
}

impl Element for NativeTextureElement {
    fn id(&self) -> &Id {
        &self.id
    }

    fn current_commit(&self) -> CommitCounter {
        self.commit
    }

    fn src(&self) -> Rectangle<f64, Buffer> {
        self.source
    }

    fn geometry(&self, _: Scale<f64>) -> Rectangle<i32, Physical> {
        self.geometry
    }

    fn alpha(&self) -> f32 {
        self.alpha
    }

    fn kind(&self) -> Kind {
        Kind::Unspecified
    }

    fn damage_since(&self, _: Scale<f64>, commit: Option<CommitCounter>) -> DamageSet<i32, Physical> {
        if commit == Some(self.commit) {
            DamageSet::default()
        } else {
            DamageSet::from_slice(&[Rectangle::from_size(self.geometry.size)])
        }
    }
}

impl RenderElement<GlesRenderer> for NativeTextureElement {
    fn draw(
        &self,
        frame: &mut GlesFrame<'_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        opaque: &[Rectangle<i32, Physical>],
    ) -> Result<(), GlesError> {
        frame.render_texture_from_to(
            &self.texture,
            src,
            dst,
            damage,
            opaque,
            Transform::Normal,
            self.alpha,
            self.program.as_ref(),
            &self.uniforms,
        )
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn activation_alpha_composes_with_lifecycle_without_retargeting_it() {
        let mut presentation = moving_presentation();
        presentation.focus_alpha = 0.8;
        presentation.opacity.current = 0.5;
        let lifecycle = presentation.opacity;
        assert!((presentation.alpha() - 0.4).abs() < 1e-6);
        presentation.focus_alpha = 0.9;
        assert_eq!(presentation.opacity, lifecycle);
        assert!((presentation.alpha() - 0.45).abs() < 1e-6);
        presentation.close();
        assert_eq!(presentation.focus_alpha, 0.9);
        assert_eq!(presentation.opacity.current, 0.5);
        assert_eq!(presentation.opacity.target, 0.0);
        assert_eq!(
            presentation
                .thumbnail(ferese_layout::Rect::new(0.0, 0.0, 100.0, 100.0))
                .alpha(),
            presentation.alpha()
        );
    }

    fn moving_presentation() -> WindowPresentation {
        use ferese_animation::{AnimatedRect, AnimatedValue, RectVelocity};
        WindowPresentation {
            focus_alpha: 1.0,
            dim: 0.0,
            id: ferese_layout::WindowId(7),
            bounds: AnimatedRect {
                velocity: RectVelocity {
                    x: 150.0,
                    y: -90.0,
                    width: 20.0,
                    height: 5.0,
                },
                ..AnimatedRect::new(ferese_layout::Rect::new(12.25, -4.5, 601.75, 399.25))
            },
            opacity: AnimatedValue {
                current: 0.7,
                target: 1.0,
                velocity: 0.4,
            },
            emphasis: AnimatedValue {
                current: 0.6,
                target: 1.0,
                velocity: 0.3,
            },
            shadow: AnimatedValue {
                current: 0.4,
                target: 1.0,
                velocity: 0.2,
            },
            scale_content: false,
            native_size: None,
        }
    }

    #[test]
    fn thumbnail_and_close_keep_window_identity_and_live_motion() {
        let live = moving_presentation();
        let thumbnail = live.thumbnail(ferese_layout::Rect::new(0.25, 8.5, 120.5, 80.25));
        assert_eq!(thumbnail.id, live.id);
        assert_eq!(thumbnail.opacity, live.opacity);
        assert_eq!(thumbnail.emphasis, live.emphasis);
        assert_eq!(thumbnail.shadow, live.shadow);
        let mut closing = live;
        closing.close();
        assert_eq!(closing.id, live.id);
        assert_eq!(closing.bounds.current, live.bounds.current);
        assert_eq!(closing.bounds.velocity, live.bounds.velocity);
        assert_eq!(closing.opacity.current, live.opacity.current);
        assert_eq!(closing.opacity.velocity, live.opacity.velocity);
        assert_eq!(closing.emphasis.velocity, live.emphasis.velocity);
        assert_eq!(closing.shadow.velocity, live.shadow.velocity);
        for scale in [1.0, 1.25, 1.5, 2.0] {
            assert_eq!(
                RoundedRect::new(live.bounds.current, (0, 0).into(), scale, 14.0),
                RoundedRect::new(closing.bounds.current, (0, 0).into(), scale, 14.0)
            );
        }
    }

    #[test]
    fn layered_responses_preserve_damping_and_settle_independently() {
        use ferese_animation::{AnimatedValue, SpringConfig};
        let base = SpringConfig::default();
        let damping_ratio = |spring: SpringConfig| spring.damping / (2.0 * (spring.mass * spring.stiffness).sqrt());
        for spring in [emphasis_spring(base), shadow_spring(base)] {
            assert!((damping_ratio(spring) - damping_ratio(base)).abs() < 1e-12);
            assert_eq!(spring.crossing, base.crossing);
        }
        let mut emphasis = AnimatedValue {
            current: 0.0,
            target: 1.0,
            velocity: 0.0,
        };
        let mut shadow = emphasis;
        let dt = Duration::from_millis(80);
        emphasis.advance(dt, emphasis_spring(base));
        shadow.advance(dt, shadow_spring(base));
        assert!(emphasis.current > shadow.current);
        let before = (emphasis.velocity, shadow.velocity);
        emphasis.set_target(0.0);
        shadow.set_target(0.0);
        assert_eq!((emphasis.velocity, shadow.velocity), before);
        emphasis.advance(Duration::from_secs(3), emphasis_spring(base));
        shadow.advance(Duration::from_secs(3), shadow_spring(base));
        assert!(!emphasis.is_animating());
        assert!(!shadow.is_animating());
    }

    #[test]
    fn rounded_outline_clamps_after_physical_snapping() {
        // Logical clamping would give 1.125; the snapped 3px rect allows radius 1.5.
        let outline = RoundedRect::new(ferese_layout::Rect::new(0.2, 0.2, 1.5, 4.0), (0, 0).into(), 1.5, 20.0);
        assert_eq!(outline.rect, Rectangle::new((0, 0).into(), (3, 6).into()));
        assert_eq!(outline.radius, 1.5);
        for scale in [1.0, 1.25, 1.5, 2.0] {
            let rect = ferese_layout::Rect::new(-20.4, -10.2, 8.8, 6.4);
            let outline = RoundedRect::new(rect, (-10, -5).into(), scale, 100.0);
            assert_eq!(outline.rect, physical_rect(rect, (-10, -5).into(), scale));
            assert_eq!(
                outline.radius,
                outline.rect.size.w.min(outline.rect.size.h) as f32 / 2.0
            );
        }
    }

    #[test]
    fn physical_radius_preserves_fractions_and_handles_zero() {
        let size = (11, 7).into();
        assert_eq!(clamp_radius(100.0, size), 3.5);
        assert_eq!(clamp_radius(2.25, size), 2.25);
        assert_eq!(clamp_radius(0.0, size), 0.0);
        assert_eq!(clamp_radius(-1.0, size), 0.0);
        assert_eq!(clamp_radius(2.0, (0, 10).into()), 0.0);
        let outline = RoundedRect::new(ferese_layout::Rect::new(0.0, 0.0, 10.0, 10.0), (0, 0).into(), 1.5, 1.5);
        assert_eq!(outline.radius, 2.25);
    }

    #[test]
    fn material_and_window_outline_agree_for_the_same_rect() {
        let logical = Rectangle::new((-3, 7).into(), (11, 9).into());
        for scale in [1.0, 1.25, 1.5, 2.0] {
            assert_eq!(
                RoundedRect::from_logical(logical, scale, 100.0),
                RoundedRect::new(
                    ferese_layout::Rect::new(-3.0, 7.0, 11.0, 9.0),
                    (0, 0).into(),
                    scale,
                    100.0
                )
            );
        }
    }

    use super::*;

    #[test]
    fn presentation_feedback_excludes_missing_and_occluded_surfaces() {
        use smithay::backend::renderer::element::{RenderElementPresentationState, RenderElementState};
        use smithay::output::{PhysicalProperties, Subpixel};

        let output = Output::new(
            "owner".into(),
            PhysicalProperties {
                size: (0, 0).into(),
                subpixel: Subpixel::Unknown,
                make: "test".into(),
                model: "test".into(),
            },
        );
        let id = Id::new();
        let mut states = RenderElementStates::default();
        assert!(rendered_feedback_output(&output, id.clone(), &states).is_none());
        for state in [
            RenderElementState {
                visible_area: 0,
                presentation_state: RenderElementPresentationState::Skipped,
            },
            RenderElementState {
                visible_area: 0,
                presentation_state: RenderElementPresentationState::Rendering { reason: None },
            },
        ] {
            states.states.insert(id.clone(), state);
            assert!(rendered_feedback_output(&output, id.clone(), &states).is_none());
        }
        for presentation_state in [
            RenderElementPresentationState::Rendering { reason: None },
            RenderElementPresentationState::ZeroCopy,
        ] {
            states.states.insert(
                id.clone(),
                RenderElementState {
                    visible_area: 32,
                    presentation_state,
                },
            );
            assert_eq!(
                rendered_feedback_output(&output, id.clone(), &states),
                Some(output.clone())
            );
        }
    }

    #[test]
    fn reversing_a_grow_replaces_the_smaller_snapshot() {
        assert!(!snapshot_covers_source((500, 600).into(), (1000, 600).into(), 1.0, 1.0));
        assert!(snapshot_covers_source((1000, 600).into(), (500, 600).into(), 1.0, 1.0));
        assert!(!snapshot_covers_source((1000, 600).into(), (500, 600).into(), 1.0, 1.8));
    }

    #[test]
    fn shrinking_keeps_old_pixels_until_destination_covers_bounds() {
        let target = ferese_layout::Rect::new(0.0, 0.0, 500.0, 600.0);
        let visual = ferese_layout::Rect::new(0.0, 0.0, 750.0, 600.0);
        assert!(resize_needs_old_frame(visual, target, 500, 600));
        assert!(!resize_needs_old_frame(target, target, 500, 600));
        // Character-grid clients must still release the bounded snapshot.
        assert!(!resize_needs_old_frame(target, target, 492, 592));
        let growing_target = ferese_layout::Rect::new(0.0, 0.0, 1000.0, 600.0);
        assert!(!resize_needs_old_frame(visual, growing_target, 1000, 600));
    }

    #[test]
    fn fresh_snapshot_and_retarget_wait_do_not_skip_the_handoff() {
        let mut elapsed = Duration::ZERO;
        let mut last = None;
        assert!(advance_handoff(
            &mut elapsed,
            &mut last,
            Duration::from_secs(5),
            false,
            1.0
        ));
        assert_eq!(elapsed, Duration::ZERO);
        assert!(advance_handoff(
            &mut elapsed,
            &mut last,
            Duration::from_millis(5010),
            false,
            1.0
        ));
        assert_eq!(elapsed, Duration::from_millis(10));
        advance_handoff(&mut elapsed, &mut last, Duration::from_secs(6), true, 1.0);
        advance_handoff(&mut elapsed, &mut last, Duration::from_secs(7), false, 1.0);
        assert_eq!(elapsed, Duration::from_millis(10));
        assert!(!advance_handoff(
            &mut elapsed,
            &mut last,
            Duration::from_millis(7070),
            false,
            1.0
        ));
    }

    #[test]
    fn no_damage_requests_are_paced_without_buffer_submission() {
        let start = Duration::ZERO;
        let refresh = Duration::from_millis(16);
        let mut clock = crate::frame_scheduler::FrameScheduler::new(refresh);
        assert_eq!(clock.callback_deadline(start, start).unwrap(), start);
        clock.callback_sent(start);
        for millisecond in 1..16 {
            assert_eq!(
                clock
                    .callback_deadline(start + Duration::from_millis(millisecond), start)
                    .unwrap(),
                start + refresh
            );
        }

        let late = start + Duration::from_millis(50);
        assert_eq!(clock.callback_deadline(late, late).unwrap(), late);
        clock.callback_sent(late);
        assert_eq!(clock.callback_deadline(late, late).unwrap(), late + refresh);
    }

    #[test]
    fn independent_output_callback_clocks_do_not_sum_their_rates() {
        let start = Duration::ZERO;
        let mut slow = crate::frame_scheduler::FrameScheduler::new(Duration::from_millis(16));
        let mut fast = crate::frame_scheduler::FrameScheduler::new(Duration::from_millis(8));
        slow.callback_sent(start);
        fast.callback_sent(start);
        fast.callback_sent(start + Duration::from_millis(8));
        assert_eq!(
            slow.callback_deadline(start + Duration::from_millis(8), start).unwrap(),
            start + Duration::from_millis(16)
        );
        assert_eq!(
            fast.callback_deadline(start + Duration::from_millis(8), start).unwrap(),
            start + Duration::from_millis(16)
        );
    }

    #[test]
    fn mixed_refresh_outputs_do_not_double_the_animation_clock() {
        let start = Instant::now();
        let mut last = start;
        let mut elapsed = Duration::ZERO;
        for ms in [8, 16, 16, 24, 32, 32] {
            elapsed += frame_delta(&mut last, start + Duration::from_millis(ms));
        }
        assert_eq!(elapsed, Duration::from_millis(32));
        assert_eq!(frame_delta(&mut last, start), Duration::ZERO);
        assert_eq!(last, start + Duration::from_millis(32));
    }

    #[test]
    fn fractional_motion_does_not_quantize_to_logical_pixels() {
        let rect = ferese_layout::Rect::new(0.3, 0.3, 100.2, 80.2);
        assert_eq!(physical_rect(rect, (0, 0).into(), 1.8).loc, (1, 1).into());
        assert_eq!(physical_rect(rect, (0, 0).into(), 1.8).size, (180, 144).into());
    }

    #[test]
    fn adjacent_frames_share_edges_at_all_output_scales() {
        for scale in [1.0, 1.25, 1.5, 1.8, 2.0] {
            let left = physical_rect(ferese_layout::Rect::new(13.3, 0.0, 500.4, 800.0), (0, 0).into(), scale);
            let right = physical_rect(ferese_layout::Rect::new(513.7, 0.0, 500.4, 800.0), (0, 0).into(), scale);
            assert_eq!(left.loc.x + left.size.w, right.loc.x);
        }
    }

    #[test]
    fn handoff_has_bounded_lifetime_and_no_alpha_jump() {
        assert_eq!(handoff_alpha(Duration::ZERO), 1.0);
        assert_eq!(handoff_alpha(HANDOFF), 0.0);
        assert_eq!(handoff_alpha(Duration::from_secs(1)), 0.0);
        assert!(handoff_alpha(Duration::from_millis(1)) > 0.99);
    }
}

pub(crate) fn scaled_visual_rect(rect: ferese_layout::Rect, scale: f64) -> ferese_layout::Rect {
    let scale = scale.max(0.0);
    let width = rect.width * scale;
    let height = rect.height * scale;

    ferese_layout::Rect::new(
        rect.x + (rect.width - width) / 2.0,
        rect.y + (rect.height - height) / 2.0,
        width,
        height,
    )
}

pub(crate) fn scaled_visual_velocity(
    rect: ferese_layout::Rect,
    velocity: ferese_animation::RectVelocity,
    scale: f64,
    scale_velocity: f64,
) -> ferese_animation::RectVelocity {
    ferese_animation::RectVelocity {
        x: velocity.x + ((1.0 - scale) * velocity.width - rect.width * scale_velocity) * 0.5,
        y: velocity.y + ((1.0 - scale) * velocity.height - rect.height * scale_velocity) * 0.5,
        width: velocity.width * scale + rect.width * scale_velocity,
        height: velocity.height * scale + rect.height * scale_velocity,
    }
}

#[cfg(test)]
mod motion_tests {
    use super::*;
    #[test]
    fn center_scale_velocity_matches_the_visible_rectangle() {
        let rect = ferese_layout::Rect::new(12.25, 18.75, 400.5, 300.25);
        let velocity = ferese_animation::RectVelocity {
            x: 80.0,
            y: -40.0,
            width: 100.0,
            height: 20.0,
        };
        let v = scaled_visual_velocity(rect, velocity, 0.98, 0.07);
        let before = scaled_visual_rect(rect, 0.98);
        let dt = 1e-5;
        let next = ferese_layout::Rect::new(
            rect.x + velocity.x * dt,
            rect.y + velocity.y * dt,
            rect.width + velocity.width * dt,
            rect.height + velocity.height * dt,
        );
        let after = scaled_visual_rect(next, 0.98 + 0.07 * dt);
        for (measured, expected) in [
            ((after.x - before.x) / dt, v.x),
            ((after.y - before.y) / dt, v.y),
            ((after.width - before.width) / dt, v.width),
            ((after.height - before.height) / dt, v.height),
        ] {
            assert!((measured - expected).abs() < 0.0001);
        }
    }
}
