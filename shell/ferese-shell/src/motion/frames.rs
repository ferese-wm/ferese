//! Surface-local animation sampling. Iced's Wayland backend gates NextFrame
//! redraws on that surface's frame callback; no animation timer is needed.
use super::*;
use std::time::Instant;

pub(crate) fn geometry_driven<'a, M: 'a>(
    revision: Option<Instant>,
    build: impl Fn(Instant) -> Element<'a, M> + 'a,
    active: impl Fn(Instant) -> bool + 'a,
    after_frame: impl Fn(Instant) + 'a,
    completion: impl Fn(Instant) -> Option<M> + 'a,
) -> Element<'a, M> {
    driven(revision, build, active, after_frame, completion, true)
}

pub(crate) fn frame_driven<'a, M: 'a>(
    revision: Option<Instant>,
    build: impl Fn(Instant) -> Element<'a, M> + 'a,
    active: impl Fn(Instant) -> bool + 'a,
    after_frame: impl Fn(Instant) + 'a,
    completion: impl Fn(Instant) -> Option<M> + 'a,
) -> Element<'a, M> {
    driven(revision, build, active, after_frame, completion, false)
}

fn driven<'a, M: 'a>(
    revision: Option<Instant>,
    build: impl Fn(Instant) -> Element<'a, M> + 'a,
    active: impl Fn(Instant) -> bool + 'a,
    after_frame: impl Fn(Instant) + 'a,
    completion: impl Fn(Instant) -> Option<M> + 'a,
    geometry_changes: bool,
) -> Element<'a, M> {
    let now = Instant::now();
    let content = build(now);
    let content_active = active(now);
    Element::new(FrameDriven {
        content,
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

struct FrameDriven<'a, M> {
    content: Element<'a, M>,
    content_active: bool,
    geometry_changes: bool,
    revision: Option<Instant>,
    build: Box<dyn Fn(Instant) -> Element<'a, M> + 'a>,
    active: Box<dyn Fn(Instant) -> bool + 'a>,
    after_frame: Box<dyn Fn(Instant) + 'a>,
    completion: Box<dyn Fn(Instant) -> Option<M> + 'a>,
}

impl<'a, M: 'a> Widget<M, Theme, cosmic::Renderer> for FrameDriven<'a, M> {
    fn tag(&self) -> widget::tree::Tag {
        widget::tree::Tag::of::<FrameState>()
    }
    fn state(&self) -> widget::tree::State {
        widget::tree::State::new(FrameState::default())
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
        tree.state.downcast_mut::<FrameState>().limits = Some(*limits);
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
        self.content.as_widget().a11y_nodes(layout, &tree.children[0], cursor)
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
        let redraw = if let Event::Window(cosmic::iced::window::Event::RedrawRequested(at)) = event {
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
                    fresh_layout = Some(
                        self.content
                            .as_widget_mut()
                            .layout(&mut tree.children[0], renderer, &limits)
                            .move_to(layout.position()),
                    );
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
                if tree.state.downcast_mut::<FrameState>().complete(completion.is_none())
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
    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut widget::Tree,
        layout: Layout<'b>,
        renderer: &cosmic::Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<cosmic::iced::advanced::overlay::Element<'b, M, Theme, cosmic::Renderer>> {
        self.content
            .as_widget_mut()
            .overlay(&mut tree.children[0], layout, renderer, viewport, translation)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cosmic::iced::advanced::renderer::Headless;
    use cosmic::iced::window::{Event as WindowEvent, RedrawRequest};
    use std::{cell::RefCell, time::Duration};

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
            assert_eq!(samples.borrow().len(), settled_builds, "settled redraw rebuilt content");
            let values = samples.borrow();
            assert!(values.windows(2).all(|pair| pair[1] >= pair[0]));
            assert_eq!(values.last(), Some(&1.0));
            assert!(values.len() > (hz / 4) as usize, "high refresh callbacks were skipped");
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
