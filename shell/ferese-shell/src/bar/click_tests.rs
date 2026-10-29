use crate::{FereseShell, status, status_ui};
use cosmic::Application;
use cosmic::iced::advanced::renderer::Headless;
use cosmic::iced::advanced::{Layout, Shell, clipboard, layout, mouse, widget};
use cosmic::iced::{Event, Font, Pixels, Point, Rectangle, Size, window};
use ferese_panel::PanelStyle;

fn dispatch(
    app: &mut FereseShell,
    tree: &mut widget::Tree,
    renderer: &cosmic::Renderer,
    event: Event,
    cursor: mouse::Cursor,
) {
    let bar = app.outputs[0].bar;
    let viewport = Rectangle::with_size(Size::new(1200., 50.));
    let mut messages = Vec::new();
    {
        let mut view = app.view_layer(bar);
        tree.diff(&mut view);
        let node = view.as_widget_mut().layout(
            tree,
            renderer,
            &layout::Limits::new(Size::ZERO, viewport.size()),
        );
        view.as_widget_mut().update(
            tree,
            &event,
            Layout::new(&node),
            cursor,
            renderer,
            &mut clipboard::Null,
            &mut Shell::new(&mut messages),
            &viewport,
        );
    }
    for message in messages {
        if let cosmic::Action::App(message) = message {
            drop(app.update(message));
        }
    }
}

#[test]
fn panel_controls_open_popovers_after_a_press_redraw_and_release() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let renderer = runtime
        .block_on(cosmic::Renderer::new(
            Font::default(),
            Pixels(14.),
            Some("tiny-skia"),
        ))
        .unwrap();
    for style in [PanelStyle::Flat, PanelStyle::Floating] {
        for (label, kind) in [
            ("Wi-Fi:", status_ui::Menu::Network),
            ("Control Center", status_ui::Menu::System),
        ] {
            let mut app = crate::tests::shell_with_measured_panel();
            app.config.panels[0].style = style;
            app.status.network = Some(status::Network {
                enabled: true,
                connection: Some("Test network".into()),
                signal: 80,
            });
            let mut tree = widget::Tree::empty();
            dispatch(
                &mut app,
                &mut tree,
                &renderer,
                Event::Window(window::Event::RedrawRequested(std::time::Instant::now())),
                mouse::Cursor::Unavailable,
            );
            let point = {
                let mut view = app.view_layer(app.outputs[0].bar);
                tree.diff(&mut view);
                let node = view.as_widget_mut().layout(
                    &mut tree,
                    &renderer,
                    &layout::Limits::new(Size::ZERO, Size::new(1200., 50.)),
                );
                let a11y = view.as_widget().a11y_nodes(
                    Layout::new(&node),
                    &tree,
                    mouse::Cursor::Unavailable,
                );
                let mut nodes = a11y.root().iter().chain(a11y.children());
                let target = nodes
                    .find(|node| {
                        node.node()
                            .label()
                            .is_some_and(|name| name.starts_with(label))
                    })
                    .unwrap_or_else(|| panic!("missing {label} in {style:?}"));
                let bounds = target.node().bounds().expect("button bounds");
                Point::new(
                    ((bounds.x0 + bounds.x1) * 0.5) as f32,
                    ((bounds.y0 + bounds.y1) * 0.5) as f32,
                )
            };
            for event in [
                Event::Mouse(mouse::Event::CursorMoved { position: point }),
                Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
                Event::Window(window::Event::RedrawRequested(std::time::Instant::now())),
                Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)),
            ] {
                dispatch(
                    &mut app,
                    &mut tree,
                    &renderer,
                    event,
                    mouse::Cursor::Available(point),
                );
            }
            assert!(
                app.menu.as_ref().is_some_and(|menu| menu.kind == kind),
                "{label} click did not open its popover in {style:?}"
            );
            let popup = app.menu.as_ref().unwrap().id;
            for _ in 0..2 {
                dispatch(
                    &mut app,
                    &mut tree,
                    &renderer,
                    Event::Window(window::Event::RedrawRequested(std::time::Instant::now())),
                    mouse::Cursor::Available(point),
                );
                assert!(
                    app.menu
                        .as_ref()
                        .is_some_and(|menu| menu.id == popup && !menu.motion.closing()),
                    "{label} popover was dismissed by its first redraw in {style:?}"
                );
            }
        }
    }
}
