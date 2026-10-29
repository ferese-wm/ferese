use cosmic::iced::advanced::renderer::Headless;
use cosmic::iced::advanced::{Layout, Shell, clipboard, layout, mouse, widget};
use cosmic::iced::{Event, Font, Pixels, Point, Rectangle, Size};
use ferese_config::theme::{Appearance, Mode};

use super::{Menu, PopoverAnchor};
use crate::status::{self, Action};
use crate::{Message, rendering};

#[test]
fn compact_tiles_keep_distinct_targets_and_unavailable_controls_are_disabled() {
    let mut renderer = cosmic::iced::futures::executor::block_on(cosmic::Renderer::new(
        Font::default(),
        Pixels(14.),
        Some("tiny-skia"),
    ))
    .unwrap();
    for radius in [0., 26.] {
        for supported in [false, true] {
            let mut app = crate::tests::shell_with_measured_panel();
            let mut resolved = ferese_config::theme::default_theme();
            if radius == 0. {
                resolved.appearance = Appearance::Light;
                resolved.tokens =
                    ferese_config::theme::preset("ferese-blue-light", Appearance::Light).unwrap();
            }
            app.config.apply_theme(&resolved);
            app.config.theme_mode = Mode::Auto;
            app.config.theme.material_radius = radius;
            app.render = rendering::Rendering::new(&app.config);
            app.status = status::Snapshot {
                network: Some(status::Network {
                    enabled: true,
                    connection: Some("A very long network name".into()),
                    signal: 82,
                }),
                bluetooth: Some(status::Bluetooth {
                    enabled: true,
                    devices: vec!["Headphones with a very long device name".into()],
                }),
                brightness: Some(82),
                audio: Some(status::Audio {
                    volume: 64,
                    muted: false,
                    output: "Built-in speakers".into(),
                }),
                battery: Some(status::Battery {
                    percent: 92,
                    status: if supported { "Charging" } else { "Discharging" }.into(),
                }),
                night_light: Some(ferese_ipc::night_light::Status {
                    supported_outputs: usize::from(supported),
                    temperature: 4500,
                    ..Default::default()
                }),
                notifications: Some(status::Notifications {
                    count: 0,
                    dnd: false,
                }),
                ..Default::default()
            };
            let bar = app.outputs[0].bar;
            drop(app.open_menu(
                Menu::System,
                PopoverAnchor {
                    parent: bar,
                    panel: app.config.panels[0].id.clone(),
                    item: None,
                    rectangle: Rectangle {
                        x: 0,
                        y: 0,
                        width: 24,
                        height: 36,
                    },
                },
            ));
            let now = std::time::Instant::now();
            app.menu
                .as_mut()
                .unwrap()
                .motion
                .begin(now - std::time::Duration::from_secs(1));
            let mut view = app.view_status_menu_at(now);
            let mut tree = widget::Tree::new(&view);
            let node = view.as_widget_mut().layout(
                &mut tree,
                &renderer,
                &layout::Limits::new(Size::ZERO, Size::new(Menu::System.width(), 720.)),
            );
            let bounds = node.bounds();
            assert!(bounds.height <= 450., "Control Center grew to {bounds:?}");
            let a11y =
                view.as_widget()
                    .a11y_nodes(Layout::new(&node), &tree, mouse::Cursor::Unavailable);
            let nodes: Vec<_> = a11y.root().iter().chain(a11y.children()).collect();
            let mut tile_bounds = Vec::new();
            for (label, action, mode) in [
                ("Wi-Fi:", Some(Action::Wifi(false)), None),
                ("Bluetooth:", Some(Action::Bluetooth(false)), None),
                (
                    "Night Light:",
                    supported.then_some(Action::NightLight(true)),
                    None,
                ),
                ("Do Not Disturb:", Some(Action::Dnd(true)), None),
                (
                    "Dark Mode:",
                    None,
                    Some(if resolved.appearance == Appearance::Dark {
                        Mode::Light
                    } else {
                        Mode::Dark
                    }),
                ),
                ("Battery:", None, None),
            ] {
                let tile = nodes
                    .iter()
                    .find(|node| {
                        node.node()
                            .label()
                            .is_some_and(|name| name.starts_with(label))
                    })
                    .unwrap_or_else(|| panic!("missing {label}"));
                let rect = tile.node().bounds().unwrap();
                assert!(rect.x0 >= 0. && rect.x1 <= f64::from(bounds.width));
                assert!(rect.y0 >= 0. && rect.y1 <= f64::from(bounds.height));
                assert!(rect.x1 - rect.x0 >= 120. && rect.y1 - rect.y0 >= 44.);
                let point = Point::new(
                    ((rect.x0 + rect.x1) / 2.) as f32,
                    ((rect.y0 + rect.y1) / 2.) as f32,
                );
                let mut messages = Vec::new();
                for event in [
                    Event::Mouse(mouse::Event::CursorMoved { position: point }),
                    Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
                    Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)),
                ] {
                    view.as_widget_mut().update(
                        &mut tree,
                        &event,
                        Layout::new(&node),
                        mouse::Cursor::Available(point),
                        &renderer,
                        &mut clipboard::Null,
                        &mut Shell::new(&mut messages),
                        &bounds,
                    );
                }
                let mut actions = Vec::new();
                let mut modes = Vec::new();
                for message in messages {
                    match message {
                        cosmic::Action::App(Message::Control(action)) => actions.push(action),
                        cosmic::Action::App(Message::ThemeMode(mode)) => modes.push(mode),
                        _ => {}
                    }
                }
                assert_eq!(actions, action.into_iter().collect::<Vec<_>>(), "{label}");
                assert_eq!(modes, mode.into_iter().collect::<Vec<_>>(), "{label}");
                tile_bounds.push(rect);
            }
            assert_eq!(tile_bounds[0].y0, tile_bounds[1].y0);
            assert_eq!(tile_bounds[2].y0, tile_bounds[3].y0);
            assert!(tile_bounds[0].x1 < tile_bounds[1].x0);
            assert!(tile_bounds[1].y1 < tile_bounds[2].y0);
            assert_eq!(tile_bounds[4].y0, tile_bounds[5].y0);
            let horizontal_gap = tile_bounds[1].x0 - tile_bounds[0].x1;
            let vertical_gap = tile_bounds[2].y0 - tile_bounds[0].y1;
            assert!(
                vertical_gap < horizontal_gap,
                "grid rows should be closer than columns"
            );
            assert!(tile_bounds[4].y0 - tile_bounds[2].y1 < horizontal_gap);

            if let Ok(directory) = std::env::var("FERESE_POPOVER_CAPTURE_DIR") {
                use cosmic::iced::advanced::Renderer;
                let theme = cosmic::theme::active();
                renderer.reset(bounds);
                view.as_widget().draw(
                    &tree,
                    &mut renderer,
                    &theme,
                    &cosmic::iced::advanced::renderer::Style {
                        text_color: cosmic::iced::Color::WHITE,
                        icon_color: cosmic::iced::Color::WHITE,
                        scale_factor: 1.,
                    },
                    Layout::new(&node),
                    mouse::Cursor::Unavailable,
                    &bounds,
                );
                let size = Size::new(bounds.width.ceil() as u32, bounds.height.ceil() as u32);
                let pixels =
                    Headless::screenshot(&mut renderer, size, 1., cosmic::iced::Color::TRANSPARENT);
                std::fs::create_dir_all(&directory).unwrap();
                image_codec::save_buffer(
                    std::path::Path::new(&directory)
                        .join(format!("controls-{radius}-{supported}.png")),
                    &pixels,
                    size.width,
                    size.height,
                    image_codec::ColorType::Rgba8,
                )
                .unwrap();
            }
        }
    }
}
