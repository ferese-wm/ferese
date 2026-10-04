use super::*;
use cosmic::Application;
use cosmic::iced::advanced::renderer::{Headless, Renderer as _};
use cosmic::iced::advanced::widget::{Operation, Tree};
use cosmic::iced::advanced::{Layout, Shell, clipboard, layout, mouse, renderer};
use cosmic::iced::{Color, Event, Font, Pixels, Point, Rectangle, Size, Vector, window};

#[derive(Debug, Default)]
struct ScrollPosition {
    offset: f32,
    content_height: f32,
    viewport_height: f32,
}

impl Operation for ScrollPosition {
    fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
        operate(self);
    }

    fn scrollable(
        &mut self,
        id: Option<&cosmic::iced::advanced::widget::Id>,
        bounds: Rectangle,
        content: Rectangle,
        translation: Vector,
        _: &mut dyn cosmic::iced::advanced::widget::operation::Scrollable,
    ) {
        if id == Some(&widget::Id::new("settings-content")) {
            self.offset = translation.y;
            self.content_height = content.height;
            self.viewport_height = bounds.height;
        }
    }
}

#[test]
#[ignore = "release scroll/render sample; requires headless renderer backends"]
fn appearance_scroll_preserves_progress_and_settles_visibility() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let bounds = Rectangle::with_size(Size::new(960., 600.));
    let scale = 1.75;
    let physical = Size::new(1680, 1050);

    for backend in ["wgpu", "tiny-skia"] {
        let mut renderer = runtime
            .block_on(<cosmic::Renderer as Headless>::new(
                Font::default(),
                Pixels(14.),
                Some(backend),
            ))
            .expect("requested renderer");

        for split in [false, true] {
            let source = format!("theme {{ split #{split}; family \"ferese-blue\"; }}");
            let mut app = App::init(
                Core::default(),
                (PathBuf::from("/unused/scroll-test.kdl"), Snapshot::parse(source), None),
            )
            .0;
            app.page = Page::Appearance;
            let theme = app.native_palette.native_theme();
            let mut tree: Option<Tree> = None;
            let mut previous = 0.;
            let mut render_times = Vec::new();
            let mut visibility_messages = 0;

            for frame in 0..60 {
                let mut messages = Vec::new();
                let mut clipboard = clipboard::Null;
                let mut shell = Shell::new(&mut messages);
                let cursor = mouse::Cursor::Available(Point::new(680., 300.));
                let mut view = app.page_view();
                let tree = tree.get_or_insert_with(|| Tree::new(view.as_widget()));
                tree.diff(view.as_widget_mut());
                let node =
                    view.as_widget_mut()
                        .layout(tree, &renderer, &layout::Limits::new(Size::ZERO, bounds.size()));
                let event = if !(4..52).contains(&frame) {
                    Event::Window(window::Event::RedrawRequested(std::time::Instant::now()))
                } else {
                    Event::Mouse(mouse::Event::WheelScrolled {
                        delta: mouse::ScrollDelta::Pixels {
                            x: 0.,
                            y: if frame < 28 { -12. } else { 12. },
                        },
                    })
                };
                view.as_widget_mut().update(
                    tree,
                    &event,
                    Layout::new(&node),
                    cursor,
                    &renderer,
                    &mut clipboard,
                    &mut shell,
                    &bounds,
                );

                // Sensors observe real redraw events as well as wheel input.
                if (4..52).contains(&frame) {
                    view.as_widget_mut().update(
                        tree,
                        &Event::Window(window::Event::RedrawRequested(std::time::Instant::now())),
                        Layout::new(&node),
                        cursor,
                        &renderer,
                        &mut clipboard,
                        &mut shell,
                        &bounds,
                    );
                }

                let mut position = ScrollPosition::default();
                view.as_widget_mut()
                    .operate(tree, Layout::new(&node), &renderer, &mut position);
                assert!(position.content_height > position.viewport_height);
                if (4..28).contains(&frame) {
                    assert!(
                        position.offset > previous
                            || position.offset >= position.content_height - position.viewport_height - 0.5,
                        "scroll stopped at {position:?}"
                    );
                }

                if (28..52).contains(&frame) {
                    assert!(
                        position.offset < previous || position.offset == 0.,
                        "reverse scroll stopped at {position:?}"
                    );
                }

                previous = position.offset;
                renderer.reset(bounds);
                view.as_widget().draw(
                    tree,
                    &mut renderer,
                    &theme,
                    &renderer::Style {
                        text_color: Color::WHITE,
                        icon_color: Color::WHITE,
                        scale_factor: scale as f64,
                    },
                    Layout::new(&node),
                    cursor,
                    &bounds,
                );
                drop(view);
                let started = std::time::Instant::now();
                std::hint::black_box(Headless::screenshot(
                    &mut renderer,
                    physical,
                    scale,
                    theme.cosmic().bg_color().into(),
                ));
                if (4..52).contains(&frame) {
                    render_times.push(started.elapsed().as_micros());
                }

                for message in messages {
                    if matches!(message, Message::RowVisibility(..)) {
                        visibility_messages += 1;
                        assert!(frame < 56, "visibility kept changing after scrolling stopped");
                    }

                    let _ = app.update(message);
                }
            }

            render_times.sort_unstable();
            eprintln!(
                "backend={backend} split={split} render_median_us={} p95_us={} visibility_messages={visibility_messages}",
                render_times[render_times.len() / 2],
                render_times[render_times.len() * 95 / 100]
            );
        }
    }
}
