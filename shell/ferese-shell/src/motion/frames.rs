//! Surface-local animation sampling. Iced's Wayland backend gates NextFrame
//! redraws on that surface's frame callback; no animation timer is needed.
#[cfg(test)]
use crate::motion::Settings;

use super::{
    Clipboard, Element, Event, Layout, Length, Rectangle, Shell, Size, Theme, Vector, Widget,
    layout, mouse, renderer, widget,
};
use std::time::Instant;

pub(crate) struct ContentMeasurement<'content, M> {
    pub content: Element<'content, M>,
    pub width: f32,
    pub measured: Box<dyn Fn(Size) + 'content>,
}

pub(crate) fn geometry_driven<'content, M: 'content>(
    revision: Option<Instant>,
    build: impl Fn(Instant) -> Element<'content, M> + 'content,
    active: impl Fn(Instant) -> bool + 'content,
    after_frame: impl Fn(Instant) + 'content,
    completion: impl Fn(Instant) -> Option<M> + 'content,
    measurement: Option<ContentMeasurement<'content, M>>,
) -> Element<'content, M> {
    driven(
        revision,
        build,
        active,
        after_frame,
        completion,
        true,
        measurement,
    )
}

pub(crate) fn frame_driven<'content, M: 'content>(
    revision: Option<Instant>,
    build: impl Fn(Instant) -> Element<'content, M> + 'content,
    active: impl Fn(Instant) -> bool + 'content,
    after_frame: impl Fn(Instant) + 'content,
    completion: impl Fn(Instant) -> Option<M> + 'content,
) -> Element<'content, M> {
    driven(
        revision,
        build,
        active,
        after_frame,
        completion,
        false,
        None,
    )
}

fn driven<'content, M: 'content>(
    revision: Option<Instant>,
    build: impl Fn(Instant) -> Element<'content, M> + 'content,
    active: impl Fn(Instant) -> bool + 'content,
    after_frame: impl Fn(Instant) + 'content,
    completion: impl Fn(Instant) -> Option<M> + 'content,
    geometry_changes: bool,
    measurement: Option<ContentMeasurement<'content, M>>,
) -> Element<'content, M> {
    let now = Instant::now();
    let content = build(now);
    let content_active = active(now);
    Element::new(FrameDriven {
        content,
        measurement,
        measured: false,
        geometry_changes,
        content_active,
        revision,
        build: Box::new(build),
        active: Box::new(active),
        after_frame: Box::new(after_frame),
        completion: Box::new(completion),
    })
}

#[derive(Default)]
struct FrameState {
    revision: Option<Instant>,
    last_frame: Option<Instant>,
    completed: bool,
    pending: bool,
    limits: Option<layout::Limits>,
    prepared_layout: Option<(layout::Limits, layout::Node)>,
}

impl FrameState {
    fn begin(&mut self, at: Instant, revision: Option<Instant>) -> bool {
        if self.revision != revision {
            self.revision = revision;
            self.last_frame = None;
            self.completed = false;
        }
        if self.last_frame == Some(at) {
            return false;
        }
        self.last_frame = Some(at);
        self.pending = true;
        true
    }

    fn complete(&mut self, active: bool) -> bool {
        if active {
            self.completed = false;
            return false;
        }
        !std::mem::replace(&mut self.completed, true)
    }
}

struct FrameDriven<'content, M> {
    content: Element<'content, M>,
    measurement: Option<ContentMeasurement<'content, M>>,
    measured: bool,
    content_active: bool,
    geometry_changes: bool,
    revision: Option<Instant>,
    build: Box<dyn Fn(Instant) -> Element<'content, M> + 'content>,
    active: Box<dyn Fn(Instant) -> bool + 'content>,
    after_frame: Box<dyn Fn(Instant) + 'content>,
    completion: Box<dyn Fn(Instant) -> Option<M> + 'content>,
}

impl<'content, M: 'content> Widget<M, Theme, cosmic::Renderer> for FrameDriven<'content, M> {
    fn tag(&self) -> widget::tree::Tag {
        widget::tree::Tag::of::<FrameState>()
    }
    fn state(&self) -> widget::tree::State {
        widget::tree::State::new(FrameState::default())
    }
    fn children(&self) -> Vec<widget::Tree> {
        std::iter::once(widget::Tree::new(&self.content))
            .chain(
                self.measurement
                    .iter()
                    .map(|m| widget::Tree::new(&m.content)),
            )
            .collect()
    }
    fn diff(&mut self, tree: &mut widget::Tree) {
        tree.state.downcast_mut::<FrameState>().prepared_layout = None;
        tree.children
            .truncate(1 + usize::from(self.measurement.is_some()));
        tree.children[0].diff(&mut self.content);
        if let Some(measurement) = &mut self.measurement {
            if tree.children.len() == 1 {
                tree.children.push(widget::Tree::new(&measurement.content));
            } else {
                tree.children[1].diff(&mut measurement.content);
            }
        }
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
        let state = tree.state.downcast_mut::<FrameState>();
        state.limits = Some(*limits);
        if let Some((prepared_limits, node)) = state.prepared_layout.take()
            && prepared_limits == *limits
        {
            return node;
        }
        if !self.measured {
            if let Some(measurement) = &mut self.measurement {
                // Measure at the final width once, so spring overshoot never changes wrapping or its target.
                let probe_limits =
                    layout::Limits::new(Size::ZERO, Size::new(measurement.width, f32::INFINITY));
                let node = measurement.content.as_widget_mut().layout(
                    &mut tree.children[1],
                    renderer,
                    &probe_limits,
                );
                (measurement.measured)(node.size());
                let now = Instant::now();
                self.content = (self.build)(now);
                self.content_active = (self.active)(now);
                tree.children[0].diff(&mut self.content);
            }
            self.measured = true;
        }
        self.content
            .as_widget_mut()
            .layout(&mut tree.children[0], renderer, limits)
    }
    fn a11y_nodes(
        &self,
        layout: Layout<'_>,
        tree: &widget::Tree,
        cursor: mouse::Cursor,
    ) -> iced_accessibility::A11yTree {
        self.content
            .as_widget()
            .a11y_nodes(layout, &tree.children[0], cursor)
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
        let redraw = if let Event::Window(cosmic::iced::window::Event::RedrawRequested(at)) = event
        {
            tree.state
                .downcast_mut::<FrameState>()
                .begin(*at, self.revision)
                .then_some(*at)
        } else {
            None
        };
        let mut updated_geometry = false;
        let mut fresh_layout = None;
        if let Some(at) = redraw {
            // Preserve focus and scroll state while sampling the current frame.
            let active = (self.active)(at);
            // Include the final settled sample, then reuse the tree on hover
            // and other redraws. Application updates build fresh content.
            if active || self.content_active {
                self.content = (self.build)(at);
                tree.children[0].diff(&mut self.content);
                if self.geometry_changes {
                    self.content_active = (self.active)(at);
                    let limits = tree
                        .state
                        .downcast_ref::<FrameState>()
                        .limits
                        .expect("laid-out animation");
                    let node = self.content.as_widget_mut().layout(
                        &mut tree.children[0],
                        renderer,
                        &limits,
                    );
                    fresh_layout = Some(node.move_to(layout.position()));
                    updated_geometry = true;
                }
            }
            self.content_active = (self.active)(at);
        }
        // Iced revalidates the base layout without replaying the event. Publish
        // the measured shape and run the child against this frame's layout now.
        self.content.as_widget_mut().update(
            &mut tree.children[0],
            event,
            fresh_layout.as_ref().map_or(layout, Layout::new),
            cursor,
            renderer,
            clipboard,
            shell,
            viewport,
        );
        if updated_geometry {
            // Child updates can invalidate the layout we just measured. Otherwise
            // let runtime revalidation reuse it with the same parent constraints.
            let state = tree.state.downcast_mut::<FrameState>();
            state.prepared_layout = if shell.is_layout_invalid() {
                None
            } else {
                fresh_layout.map(|node| {
                    (
                        state.limits.expect("laid-out animation"),
                        node.move_to(cosmic::iced::Point::ORIGIN),
                    )
                })
            };
            shell.invalidate_layout();
        }
        if let Event::Window(cosmic::iced::window::Event::RedrawRequested(at)) = event {
            let active = (self.active)(*at);
            if active {
                shell.request_redraw();
            }
            // Complete only after a valid layout, including the replay of this frame.
            if std::mem::take(&mut tree.state.downcast_mut::<FrameState>().pending) {
                (self.after_frame)(*at);
                let completion = (self.completion)(*at);
                if tree
                    .state
                    .downcast_mut::<FrameState>()
                    .complete(completion.is_none())
                    && let Some(message) = completion
                {
                    shell.publish(message);
                }
            }
        }
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
        self.content.as_widget().draw(
            &tree.children[0],
            renderer,
            theme,
            style,
            layout,
            cursor,
            viewport,
        );
    }
    fn mouse_interaction(
        &self,
        tree: &widget::Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &cosmic::Renderer,
    ) -> mouse::Interaction {
        self.content.as_widget().mouse_interaction(
            &tree.children[0],
            layout,
            cursor,
            viewport,
            renderer,
        )
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
    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut widget::Tree,
        layout: Layout<'b>,
        renderer: &cosmic::Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<cosmic::iced::advanced::overlay::Element<'b, M, Theme, cosmic::Renderer>> {
        self.content.as_widget_mut().overlay(
            &mut tree.children[0],
            layout,
            renderer,
            viewport,
            translation,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::motion::PopupMotion;
    use cosmic::iced::advanced::Renderer;
    use cosmic::iced::advanced::renderer::Headless;
    use cosmic::iced::runtime::UserInterface;
    use cosmic::iced::window::{Event as WindowEvent, RedrawRequest};
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;
    use std::time::Duration;

    struct CountLayout {
        calls: Rc<Cell<usize>>,
        invalidate: bool,
    }

    impl Widget<(), Theme, cosmic::Renderer> for CountLayout {
        fn size(&self) -> Size<Length> {
            Size::new(Length::Shrink, Length::Shrink)
        }

        fn layout(
            &mut self,
            _: &mut widget::Tree,
            _: &cosmic::Renderer,
            limits: &layout::Limits,
        ) -> layout::Node {
            self.calls.set(self.calls.get() + 1);
            layout::Node::new(limits.resolve(Length::Shrink, Length::Shrink, Size::new(40., 40.)))
        }

        fn update(
            &mut self,
            _: &mut widget::Tree,
            _: &Event,
            _: Layout<'_>,
            _: mouse::Cursor,
            _: &cosmic::Renderer,
            _: &mut dyn Clipboard,
            shell: &mut Shell<'_, ()>,
            _: &Rectangle,
        ) {
            if self.invalidate {
                shell.invalidate_layout();
            }
        }

        fn draw(
            &self,
            _: &widget::Tree,
            _: &mut cosmic::Renderer,
            _: &Theme,
            _: &renderer::Style,
            _: Layout<'_>,
            _: mouse::Cursor,
            _: &Rectangle,
        ) {
        }
    }

    #[test]
    fn geometry_redraw_reuses_its_layout_unless_the_child_invalidates_it() {
        let mut renderer = cosmic::iced::futures::executor::block_on(cosmic::Renderer::new(
            cosmic::iced::Font::default(),
            cosmic::iced::Pixels(14.0),
            Some("tiny-skia"),
        ))
        .unwrap();
        for invalidate in [false, true] {
            let calls = Rc::new(Cell::new(0));
            let probe = calls.clone();
            let element = geometry_driven(
                None,
                move |_| {
                    Element::new(CountLayout {
                        calls: probe.clone(),
                        invalidate,
                    })
                },
                |_| true,
                |_| {},
                |_| None,
                None,
            );
            let mut ui = UserInterface::build(
                element,
                Size::new(200., 200.),
                Default::default(),
                &mut renderer,
            );
            for _ in 0..3 {
                calls.set(0);
                ui.update(
                    &[Event::Window(WindowEvent::RedrawRequested(Instant::now()))],
                    mouse::Cursor::Unavailable,
                    &mut renderer,
                    &mut cosmic::iced::advanced::clipboard::Null,
                    &mut Vec::new(),
                );
                assert_eq!(calls.get(), if invalidate { 2 } else { 1 });
            }
            calls.set(0);
            let _ui = ui.relayout(Size::new(20., 20.), &mut renderer);
            assert_eq!(calls.get(), 1, "new surface bounds need a fresh layout");
        }
    }

    #[test]
    fn callbacks_sample_every_refresh_without_timers_and_stop_when_settled() {
        let mut renderer = cosmic::iced::futures::executor::block_on(cosmic::Renderer::new(
            cosmic::iced::Font::default(),
            cosmic::iced::Pixels(14.0),
            Some("tiny-skia"),
        ))
        .unwrap();
        for hz in [60, 120, 144, 240] {
            let start = Instant::now();
            let mut motion = PopupMotion::new(Settings::default());
            motion.begin(start);
            let samples = RefCell::new(Vec::new());
            let mut element = frame_driven(
                motion.revision(),
                |at| {
                    samples.borrow_mut().push(motion.progress_at(at));
                    cosmic::widget::Space::new().width(20.0).height(20.0).into()
                },
                |at| motion.frame_active(at),
                |_| {},
                |at| (!motion.frame_active(at)).then_some(()),
            );
            let mut tree = widget::Tree::new(&element);
            let node = element.as_widget_mut().layout(
                &mut tree,
                &renderer,
                &layout::Limits::new(Size::ZERO, Size::new(100.0, 100.0)),
            );
            let mut messages = Vec::new();
            let mut finished = false;
            let mut clipboard = cosmic::iced::advanced::clipboard::Null;
            samples.borrow_mut().clear();
            for frame in 0..=hz * 2 {
                let at = start + Duration::from_secs_f64(frame as f64 / hz as f64);
                let event = Event::Window(WindowEvent::RedrawRequested(at));
                let active = motion.frame_active(at);
                for _ in 0..2 {
                    // Iced may replay a redraw after a layout/update pass.
                    let mut shell = Shell::new(&mut messages);
                    element.as_widget_mut().update(
                        &mut tree,
                        &event,
                        Layout::new(&node),
                        mouse::Cursor::Unavailable,
                        &renderer,
                        &mut clipboard,
                        &mut shell,
                        &Rectangle::new((0.0, 0.0).into(), (100.0, 100.0).into()),
                    );
                    assert_eq!(
                        shell.redraw_request(),
                        if active {
                            RedrawRequest::NextFrame
                        } else {
                            RedrawRequest::Wait
                        }
                    );
                }
                assert_eq!(samples.borrow().len(), frame as usize + 1);
                if !active {
                    finished = true;
                    break;
                }
                assert!(
                    messages.is_empty(),
                    "motion should not publish application updates per frame"
                );
            }
            assert!(finished);
            assert_eq!(messages.len(), 1, "only one completion update");
            let settled_builds = samples.borrow().len();
            // A later unrelated redraw must not restart motion or cleanup.
            let mut shell = Shell::new(&mut messages);
            element.as_widget_mut().update(
                &mut tree,
                &Event::Window(WindowEvent::RedrawRequested(start + Duration::from_secs(3))),
                Layout::new(&node),
                mouse::Cursor::Unavailable,
                &renderer,
                &mut clipboard,
                &mut shell,
                &Rectangle::new((0.0, 0.0).into(), (100.0, 100.0).into()),
            );
            assert_eq!(shell.redraw_request(), RedrawRequest::Wait);
            assert_eq!(messages.len(), 1);
            assert_eq!(
                samples.borrow().len(),
                settled_builds,
                "settled redraw rebuilt content"
            );
            let values = samples.borrow();
            assert!(values.windows(2).all(|pair| pair[1] >= pair[0]));
            assert_eq!(values.last(), Some(&1.0));
            assert!(
                values.len() > (hz / 4) as usize,
                "high refresh callbacks were skipped"
            );
        }
        // Keep this mutable: the real render path shares the same renderer.
        renderer.reset(Rectangle::default());
    }

    #[test]
    fn new_transition_can_complete_after_previous_one_settled() {
        let start = Instant::now();
        let mut state = FrameState::default();
        assert!(state.begin(start, Some(start)));
        assert!(state.complete(false));
        assert!(!state.begin(start, Some(start)));
        assert!(!state.complete(false));
        let next = start + Duration::from_millis(10);
        assert!(state.begin(next, Some(next)));
        assert!(!state.complete(true));
        assert!(state.begin(next + Duration::from_millis(400), Some(next)));
        assert!(state.complete(false));
        assert!(!state.complete(false));
    }
}
