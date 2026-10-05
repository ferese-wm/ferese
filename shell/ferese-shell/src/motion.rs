mod frames;
pub(crate) use frames::frame_driven;

use cosmic::iced::advanced::{Clipboard, Layout, Renderer as _, Shell, Widget, layout, mouse, renderer, widget};
use cosmic::iced::{Event, Length, Rectangle, Size, Transformation, Vector};
use cosmic::{Element, Theme};

pub type Regions = std::sync::Arc<std::sync::Mutex<Vec<[f32; 5]>>>;

pub(crate) use ferese_animation::MotionSettings as Settings;
use ferese_animation::{AnimatedValue, SpringConfig};

static SETTINGS: std::sync::OnceLock<std::sync::RwLock<Settings>> = std::sync::OnceLock::new();
static SHELL_RADIUS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(14.0_f32.to_bits());

/// All shell shapes share one radius, capped to fit smaller controls.
pub(crate) fn radius(limit: f32) -> f32 {
    f32::from_bits(SHELL_RADIUS.load(std::sync::atomic::Ordering::Relaxed)).min(limit)
}

pub(crate) fn configure(settings: Settings, radius: f32) {
    SHELL_RADIUS.store(radius.to_bits(), std::sync::atomic::Ordering::Relaxed);
    *SETTINGS
        .get_or_init(|| std::sync::RwLock::new(Settings::default()))
        .write()
        .unwrap() = settings;
}

pub(crate) fn settings() -> Settings {
    *SETTINGS
        .get_or_init(|| std::sync::RwLock::new(Settings::default()))
        .read()
        .unwrap()
}

pub(crate) struct PopupMotion {
    value: AnimatedValue,
    started: Option<std::time::Instant>,
    settings: Settings,
}

impl PopupMotion {
    pub(crate) fn update_settings(&mut self, settings: Settings) {
        self.update_settings_at(settings, std::time::Instant::now());
    }

    fn update_settings_at(&mut self, settings: Settings, now: std::time::Instant) {
        self.value = self.sample(now);
        // Stored velocities use simulation seconds. Preserve visible velocity
        // when changing the clock rate during an in-flight transition.
        self.value.velocity *= self.settings.speed / settings.speed;
        self.settings = settings;
        if self.started.is_some() {
            self.started = Some(now);
        }
    }

    pub(crate) fn new(settings: Settings) -> Self {
        Self {
            value: AnimatedValue {
                current: 0.0,
                target: 1.0,
                velocity: 0.0,
            },
            started: None,
            settings,
        }
    }

    pub(crate) fn begin(&mut self, now: std::time::Instant) {
        if self.started.is_none() {
            self.started = Some(now);
        }
    }

    fn sample(&self, now: std::time::Instant) -> AnimatedValue {
        let mut value = self.value;
        let Some(started) = self.started else {
            return value;
        };
        if !self.settings.motion_enabled() {
            value.snap();
            return value;
        }
        let spring = self.settings.spring_config().expect("validated motion settings");
        value.advance(
            now.saturating_duration_since(started).mul_f64(self.settings.speed),
            SpringConfig {
                position_tolerance: 0.00001,
                velocity_tolerance: 0.00001,
                ..spring
            },
        );
        value
    }

    pub(crate) fn progress_at(&self, now: std::time::Instant) -> f32 {
        self.sample(now).current as f32
    }

    pub(crate) fn progress(&self) -> f32 {
        self.progress_at(std::time::Instant::now())
    }
    pub(crate) fn closing(&self) -> bool {
        self.value.target == 0.0
    }
    pub(crate) fn revision(&self) -> Option<std::time::Instant> {
        self.started
    }
    pub(crate) fn frame_active(&self, now: std::time::Instant) -> bool {
        self.started.is_some() && self.animating_at(now)
    }
    pub(crate) fn animating(&self) -> bool {
        self.animating_at(std::time::Instant::now())
    }
    pub(crate) fn animating_at(&self, now: std::time::Instant) -> bool {
        let value = self.sample(now);
        value.current != value.target || value.velocity != 0.0
    }

    pub(crate) fn retarget(&mut self, target: f32, now: std::time::Instant) {
        self.value = self.sample(now);
        self.value.set_target(f64::from(target));
        self.started = Some(now);
    }
}

/// Animate paint only; the original button retains its layout, hit target,
/// keyboard handling, pressed state, and accessibility identity.
pub(crate) fn button<'a, M: Clone + 'a>(
    button: cosmic::widget::button::Button<'a, M>,
    foreground: cosmic::iced::Color,
    selected: bool,
    opacity: f32,
) -> Element<'a, M> {
    let paint = move |progress: f32, pressed: bool| {
        ferese_theme::controls::shell_button(foreground, selected, opacity, radius(14.), progress, pressed)
    };
    painted_button(button, paint)
}

pub(crate) fn circular_button<'a, M: Clone + 'a>(
    button: cosmic::widget::button::Button<'a, M>,
    foreground: cosmic::iced::Color,
    fill: Option<cosmic::iced::Color>,
    opacity: f32,
    diameter: f32,
) -> Element<'a, M> {
    let paint = move |progress: f32, pressed: bool| {
        let mut style =
            ferese_theme::controls::shell_button(foreground, false, opacity, diameter / 2.0, progress, pressed);
        style.shape = Some(cosmic::iced::border::Shape::Circular);
        if let Some(fill) = fill {
            style.background = Some(
                cosmic::iced::Color {
                    a: opacity * if pressed { 0.75 } else { 1.0 - 0.1 * progress },
                    ..fill
                }
                .into(),
            );
        }

        style
    };
    painted_button(button, paint)
}

fn painted_button<'a, M: Clone + 'a>(
    button: cosmic::widget::button::Button<'a, M>,
    paint: impl Fn(f32, bool) -> cosmic::widget::button::Style + Copy + 'static,
) -> Element<'a, M> {
    let progress = std::rc::Rc::new(std::cell::Cell::new(0.0));
    let active = progress.clone();
    let hovered = progress.clone();
    let content = button
        .class(cosmic::theme::Button::Custom {
            active: Box::new(move |_, _| paint(active.get(), false)),
            hovered: Box::new(move |_, _| paint(hovered.get(), false)),
            pressed: Box::new(move |_, _| paint(1.0, true)),
            disabled: Box::new(move |_| paint(0.0, false)),
        })
        .into();
    Element::new(Hover {
        content,
        progress,
        duration: SETTINGS
            .get()
            .map_or_else(Settings::default, |settings| *settings.read().unwrap())
            .duration(120.0),
    })
}

#[derive(Default)]
struct HoverState {
    current: f32,
    start: f32,
    target: f32,
    started: Option<std::time::Instant>,
}

impl HoverState {
    fn advance(&mut self, target: f32, now: std::time::Instant, duration: std::time::Duration) -> bool {
        if self.target != target {
            self.start = self.current;
            self.target = target;
            self.started = Some(now);
        }
        let t = if duration.is_zero() {
            1.0
        } else {
            self.started.map_or(1.0, |start| {
                (now.saturating_duration_since(start).as_secs_f32() / duration.as_secs_f32()).min(1.0)
            })
        };
        self.current = self.start + (self.target - self.start) * t * t * (3.0 - 2.0 * t);
        if t == 1.0 {
            self.current = target;
            self.started = None;
        }
        self.current != target
    }
}

struct Hover<'a, M> {
    content: Element<'a, M>,
    progress: std::rc::Rc<std::cell::Cell<f32>>,
    duration: std::time::Duration,
}

impl<M> Widget<M, Theme, cosmic::Renderer> for Hover<'_, M> {
    fn a11y_nodes(
        &self,
        layout: Layout<'_>,
        tree: &widget::Tree,
        cursor: mouse::Cursor,
    ) -> iced_accessibility::A11yTree {
        self.content.as_widget().a11y_nodes(layout, &tree.children[0], cursor)
    }

    fn tag(&self) -> widget::tree::Tag {
        widget::tree::Tag::of::<HoverState>()
    }

    fn state(&self) -> widget::tree::State {
        widget::tree::State::new(HoverState::default())
    }

    fn children(&self) -> Vec<widget::Tree> {
        vec![widget::Tree::new(&self.content)]
    }

    fn diff(&mut self, tree: &mut widget::Tree) {
        tree.diff_children(std::slice::from_mut(&mut self.content));
    }

    fn size(&self) -> Size<Length> {
        self.content.as_widget().size()
    }

    fn layout(
        &mut self,
        tree: &mut widget::Tree,
        renderer: &cosmic::Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        self.content
            .as_widget_mut()
            .layout(&mut tree.children[0], renderer, limits)
    }

    fn update(
        &mut self,
        tree: &mut widget::Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &cosmic::Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, M>,
        viewport: &Rectangle,
    ) {
        let now = match event {
            Event::Window(cosmic::iced::window::Event::RedrawRequested(at)) => *at,
            _ => std::time::Instant::now(),
        };
        let hovered = cursor.is_over(layout.bounds()) && cursor.is_over(*viewport);
        let state = tree.state.downcast_mut::<HoverState>();
        if state.advance(if hovered { 1.0 } else { 0.0 }, now, self.duration) {
            shell.request_redraw();
        }
        self.progress.set(state.current);
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
        tree: &widget::Tree,
        renderer: &mut cosmic::Renderer,
        theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        self.progress.set(tree.state.downcast_ref::<HoverState>().current);
        self.content
            .as_widget()
            .draw(&tree.children[0], renderer, theme, style, layout, cursor, viewport);
    }

    fn mouse_interaction(
        &self,
        tree: &widget::Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &cosmic::Renderer,
    ) -> mouse::Interaction {
        self.content
            .as_widget()
            .mouse_interaction(&tree.children[0], layout, cursor, viewport, renderer)
    }

    fn operate(
        &mut self,
        tree: &mut widget::Tree,
        layout: Layout<'_>,
        renderer: &cosmic::Renderer,
        operation: &mut dyn widget::Operation,
    ) {
        self.content
            .as_widget_mut()
            .operate(&mut tree.children[0], layout, renderer, operation);
    }

    fn overlay<'a>(
        &'a mut self,
        tree: &'a mut widget::Tree,
        layout: Layout<'a>,
        renderer: &cosmic::Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<cosmic::iced::advanced::overlay::Element<'a, M, Theme, cosmic::Renderer>> {
        self.content
            .as_widget_mut()
            .overlay(&mut tree.children[0], layout, renderer, viewport, translation)
    }
}

pub fn animated<'a, M: 'a>(content: Element<'a, M>, progress: f32, regions: Regions, radius: f32) -> Element<'a, M> {
    Element::new(Motion {
        content,
        progress,
        regions,
        radius,
    })
}

struct Motion<'a, M> {
    content: Element<'a, M>,
    progress: f32,
    regions: Regions,
    radius: f32,
}

impl<M> Motion<'_, M> {
    fn translation(&self) -> Vector {
        Vector::new(0.0, -4.0 * (1.0 - self.progress))
    }

    fn cursor(&self, cursor: mouse::Cursor) -> mouse::Cursor {
        cursor.position().map_or(mouse::Cursor::Unavailable, |p| {
            mouse::Cursor::Available(p - self.translation())
        })
    }
}

impl<M> Widget<M, Theme, cosmic::Renderer> for Motion<'_, M> {
    fn tag(&self) -> widget::tree::Tag {
        self.content.as_widget().tag()
    }

    fn state(&self) -> widget::tree::State {
        self.content.as_widget().state()
    }

    fn children(&self) -> Vec<widget::Tree> {
        self.content.as_widget().children()
    }

    fn diff(&mut self, tree: &mut widget::Tree) {
        self.content.as_widget_mut().diff(tree);
    }

    fn size(&self) -> Size<Length> {
        self.content.as_widget().size()
    }

    fn layout(
        &mut self,
        tree: &mut widget::Tree,
        renderer: &cosmic::Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        self.content.as_widget_mut().layout(tree, renderer, limits)
    }

    fn update(
        &mut self,
        tree: &mut widget::Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &cosmic::Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, M>,
        viewport: &Rectangle,
    ) {
        let mut collector = CollectRegions(Vec::new());
        self.content
            .as_widget_mut()
            .operate(tree, layout, renderer, &mut collector);
        if collector.0.is_empty() {
            // A single-content popover (e.g. battery) has one inner card, not
            // a full-surface rectangle including the outer transparent padding.
            if let Some(content) = layout.children().next() {
                collector.0.push(content.bounds());
            }
        }
        *self.regions.lock().unwrap() = collector.into_regions(self.translation(), self.radius);
        let cursor = self.cursor(cursor);
        self.content
            .as_widget_mut()
            .update(tree, event, layout, cursor, renderer, clipboard, shell, viewport);
    }

    fn draw(
        &self,
        tree: &widget::Tree,
        renderer: &mut cosmic::Renderer,
        theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        let v = self.translation();
        let transform = Transformation::translate(v.x, v.y);
        // Layout/region collection still run before the surface starts opening.
        // No child may paint an opaque control before the presentation begins.
        if self.progress <= 0.0 {
            return;
        }
        renderer.with_transformation(transform, |renderer| {
            self.content
                .as_widget()
                .draw(tree, renderer, theme, style, layout, self.cursor(cursor), viewport)
        });
    }

    fn mouse_interaction(
        &self,
        tree: &widget::Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &cosmic::Renderer,
    ) -> mouse::Interaction {
        self.content
            .as_widget()
            .mouse_interaction(tree, layout, self.cursor(cursor), viewport, renderer)
    }

    fn operate(
        &mut self,
        tree: &mut widget::Tree,
        layout: Layout<'_>,
        renderer: &cosmic::Renderer,
        operation: &mut dyn widget::Operation,
    ) {
        self.content.as_widget_mut().operate(tree, layout, renderer, operation);
    }
}

struct CollectRegions(Vec<Rectangle>);

impl CollectRegions {
    fn into_regions(self, translation: Vector, radius: f32) -> Vec<[f32; 5]> {
        self.0
            .into_iter()
            .take(32)
            .filter(|r| r.width > 0.0 && r.height > 0.0)
            .map(|r| {
                [
                    r.x + translation.x,
                    r.y + translation.y,
                    r.width,
                    r.height,
                    radius.max(0.0),
                ]
            })
            .collect()
    }
}

impl widget::Operation for CollectRegions {
    fn traverse(&mut self, children: &mut dyn FnMut(&mut dyn widget::Operation)) {
        children(self);
    }

    fn container(&mut self, id: Option<&widget::Id>, bounds: Rectangle) {
        if id == Some(&widget::Id::new("ferese-blur-card")) {
            let contains = |outer: &Rectangle, inner: &Rectangle| {
                outer.x <= inner.x
                    && outer.y <= inner.y
                    && outer.x + outer.width >= inner.x + inner.width
                    && outer.y + outer.height >= inner.y + inner.height
            };
            // Nested controls belong to their enclosing card's single blur pass.
            if self.0.iter().any(|outer| contains(outer, &bounds)) {
                return;
            }
            self.0.retain(|inner| !contains(&bounds, inner));
            self.0.push(bounds);
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn duration_bounce_spring_finishes_popup_close() {
        let settings = super::Settings {
            spring: ferese_animation::SpringSettings {
                duration_ms: Some(240.0),
                bounce: 0.3,
                overshoot: true,
                ..Default::default()
            },
            ..Default::default()
        };
        settings.validate().unwrap();
        let now = std::time::Instant::now();
        let mut motion = super::PopupMotion::new(settings);
        motion.begin(now);
        let opened = now + std::time::Duration::from_secs(1);
        assert_eq!(motion.sample(opened).current, 1.0);
        motion.retarget(0.0, opened);
        let closed = motion.sample(opened + std::time::Duration::from_secs(1));
        assert!(motion.closing());
        assert_eq!(closed.current, closed.target);
        assert_eq!(closed.velocity, 0.0);
    }

    use super::*;

    #[test]
    fn material_regions_preserve_fractional_layout_and_translation() {
        let collector = CollectRegions(vec![Rectangle::new((0.25, 2.5).into(), (100.25, 40.5).into())]);
        assert_eq!(
            collector.into_regions(Vector::new(0.125, -0.25), 13.5),
            vec![[0.375, 2.25, 100.25, 40.5, 13.5]]
        );
    }

    #[test]
    fn centered_modal_material_tracks_surface_position_and_animation() {
        let mut collector = CollectRegions(Vec::new());
        let card = Rectangle::new((380.0, 420.0).into(), (440.0, 220.0).into());
        widget::Operation::container(&mut collector, Some(&widget::Id::new("ferese-blur-card")), card);
        assert_eq!(
            collector.into_regions(Vector::new(0.0, -2.0), 14.0),
            vec![[380., 418., 440., 220., 14.]]
        );
    }

    #[test]
    fn popup_waits_for_configuration_and_preserves_reversal_velocity() {
        let now = std::time::Instant::now();
        let mut motion = PopupMotion::new(Settings::default());
        assert_eq!(motion.progress_at(now + std::time::Duration::from_secs(30)), 0.0);
        motion.begin(now);
        let reverse = now + std::time::Duration::from_millis(90);
        let before = motion.sample(reverse);
        motion.begin(reverse);
        assert_eq!(motion.sample(reverse), before);
        motion.retarget(0.0, reverse);
        let after = motion.sample(reverse);
        assert_eq!(after.current, before.current);
        assert_eq!(after.velocity, before.velocity);
        assert!(motion.closing());
        assert_eq!(motion.progress_at(reverse + std::time::Duration::from_secs(2)), 0.0);
    }

    #[test]
    fn live_speed_change_preserves_visible_velocity_and_reduced_motion_snaps() {
        let now = std::time::Instant::now();
        let mut motion = PopupMotion::new(Settings::default());
        motion.begin(now);
        let time = now + std::time::Duration::from_millis(90);
        let before = motion.sample(time);
        motion.update_settings_at(
            Settings {
                speed: 2.0,
                ..Settings::default()
            },
            time,
        );
        let after = motion.sample(time);
        assert_eq!(after.current, before.current);
        assert_eq!(after.velocity * 2.0, before.velocity);
        motion.update_settings_at(
            Settings {
                reduced_motion: true,
                ..Settings::default()
            },
            time,
        );
        assert_eq!(motion.progress_at(time), 1.0);
    }

    #[test]
    fn popup_reversal_and_motion_policy_are_consistent() {
        let now = std::time::Instant::now();
        let mut motion = PopupMotion::new(Settings {
            speed: 0.75,
            ..Settings::default()
        });
        motion.begin(now);
        let reverse = now + std::time::Duration::from_millis(120);
        let before = motion.progress_at(reverse);
        motion.retarget(0.0, reverse);
        assert_eq!(motion.progress_at(reverse), before);
        let reopen = reverse + std::time::Duration::from_millis(60);
        let before = motion.progress_at(reopen);
        motion.retarget(1.0, reopen);
        assert_eq!(motion.progress_at(reopen), before);
        assert_eq!(motion.progress_at(reopen + std::time::Duration::from_secs(2)), 1.0);
        for settings in [
            Settings {
                reduced_motion: true,
                ..Settings::default()
            },
            Settings {
                enabled: false,
                ..Settings::default()
            },
        ] {
            let mut motion = PopupMotion::new(settings);
            assert_eq!(motion.progress_at(now), 0.0);
            motion.begin(now);
            assert_eq!(motion.progress_at(now), 1.0);
            motion.retarget(0.0, now);
            assert_eq!(motion.progress_at(now), 0.0);
        }
    }

    #[test]
    fn updating_motion_policy_preserves_surface_readiness() {
        let mut motion = PopupMotion::new(Settings::default());
        motion.update_settings(Settings {
            reduced_motion: true,
            ..Settings::default()
        });
        assert_eq!(motion.progress(), 0.0);
        motion.begin(std::time::Instant::now());
        assert_eq!(motion.progress(), 1.0);
    }

    #[test]
    fn hover_fades_settle_and_idle_does_not_schedule_frames() {
        let now = std::time::Instant::now();
        let duration = std::time::Duration::from_millis(120);
        let mut hover = HoverState::default();
        assert!(!hover.advance(0.0, now, duration));
        assert!(hover.advance(1.0, now + std::time::Duration::from_secs(10), duration));
        assert_eq!(hover.current, 0.0);
        let start = now + std::time::Duration::from_secs(10);
        assert!(hover.advance(1.0, start + duration / 2, duration));
        assert!((hover.current - 0.5).abs() < 0.00001);
        assert!(!hover.advance(1.0, start + duration, duration));
        assert_eq!(hover.current, 1.0);
        assert!(!hover.advance(1.0, start + duration * 10, duration));
        assert!(hover.advance(0.0, start + duration * 11, duration));
        assert!(!hover.advance(0.0, start + duration * 12, duration));
        assert_eq!(hover.current, 0.0);
    }

    #[test]
    fn hover_reversals_preserve_current_paint() {
        let now = std::time::Instant::now();
        let duration = std::time::Duration::from_millis(120);
        let mut hover = HoverState::default();
        hover.advance(1.0, now, duration);
        hover.advance(1.0, now + duration / 2, duration);
        let before = hover.current;
        assert!(hover.advance(0.0, now + duration / 2, duration));
        assert_eq!(hover.current, before);
        hover.advance(0.0, now + duration, duration);
        assert!(hover.current > 0.0 && hover.current < before);
    }

    #[test]
    fn hover_obeys_global_motion_policy() {
        assert_eq!(
            Settings {
                speed: 0.75,
                ..Settings::default()
            }
            .duration(120.0),
            std::time::Duration::from_millis(160)
        );
        for settings in [
            Settings {
                enabled: false,
                ..Settings::default()
            },
            Settings {
                reduced_motion: true,
                ..Settings::default()
            },
        ] {
            assert!(settings.duration(120.0).is_zero());
            let mut hover = HoverState::default();
            assert!(!hover.advance(1.0, std::time::Instant::now(), settings.duration(120.0)));
            assert_eq!(hover.current, 1.0);
        }
    }

    #[test]
    fn material_regions_include_cards_but_not_the_parent_or_gaps() {
        let mut collector = CollectRegions(Vec::new());
        let id = widget::Id::new("ferese-blur-card");
        widget::Operation::container(
            &mut collector,
            None,
            Rectangle::new((0.0, 0.0).into(), (320.0, 200.0).into()),
        );
        let first = Rectangle::new((12.0, 12.0).into(), (140.0, 80.0).into());
        let second = Rectangle::new((160.0, 12.0).into(), (140.0, 80.0).into());
        widget::Operation::container(&mut collector, Some(&id), first);
        widget::Operation::container(&mut collector, Some(&id), second);
        assert_eq!(collector.0, vec![first, second]);
        widget::Operation::container(
            &mut collector,
            Some(&id),
            Rectangle::new((20.0, 20.0).into(), (40.0, 40.0).into()),
        );
        assert_eq!(collector.0, vec![first, second]);
    }
}
