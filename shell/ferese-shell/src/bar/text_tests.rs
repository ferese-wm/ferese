use super::*;
use crate::control;
use cosmic::iced::advanced::renderer::Headless;
use cosmic::iced::advanced::{
    Layout, Renderer as _, Shell, clipboard, layout, mouse, renderer, widget,
};
use cosmic::iced::{Event, Font, Pixels, Point, Rectangle, Size};

fn fixture() -> FereseShell {
    let mut shell = crate::tests::shell_with_measured_panel();
    shell.status_service = crate::status::Service::disconnected_for_test();
    shell.config.panels[0].presentation = ferese_panel::Presentation::Text;
    shell.config.panels[0].style = ferese_panel::PanelStyle::Flat;
    shell.snapshot.outputs = vec![
        control::OutputSnapshot {
            id: 5,
            name: "test".into(),
            active_workspace: 42,
            focused: false,
        },
        control::OutputSnapshot {
            id: 6,
            name: "other".into(),
            active_workspace: 99,
            focused: true,
        },
    ];
    shell.snapshot.workspaces = (1..=4)
        .map(|index| control::WorkspaceSnapshot {
            id: 40 + u64::from(index),
            name: index.to_string(),
            output: Some(5),
            index,
            window_count: u32::from(index == 1 || index == 3),
            visible: index == 2,
            focused: false,
        })
        .collect();
    shell.snapshot.layouts.insert(42, "scrolling".into());
    shell.snapshot.windows = vec![control::WindowSnapshot {
        id: 123,
        workspace: 44,
        geometry: None,
        app_id: "test".into(),
        title: "Attention".into(),
        focused: false,
        urgent: true,
        fullscreen: false,
        maximized: false,
        floating: false,
    }];
    shell.status.audio = Some(crate::status::Audio {
        volume: 40,
        muted: false,
        output: "test".into(),
    });
    shell.status.battery = Some(crate::status::Battery {
        percent: 82,
        status: "discharging".into(),
    });
    shell.system_stats = crate::system_stats::Snapshot {
        cpu: Some(12),
        disk: Some(64),
        network: Some(crate::system_stats::NetworkRate {
            interface: "wlan0".into(),
            received: 1024,
            transmitted: 0,
        }),
    };
    shell.status.network = Some(crate::status::Network {
        enabled: true,
        connection: Some("Home".into()),
        signal: 85,
        ..Default::default()
    });
    shell.clock = "Fri 30 Oct 14:07".into();
    shell
}

#[derive(Default)]
struct Labels(Vec<(String, Rectangle)>);
impl widget::Operation for Labels {
    fn traverse(&mut self, visit: &mut dyn FnMut(&mut dyn widget::Operation)) {
        visit(self);
    }
    fn text(&mut self, _: Option<&widget::Id>, bounds: Rectangle, text: &str) {
        self.0.push((text.into(), bounds));
    }
}

fn renderer() -> cosmic::Renderer {
    cosmic::iced::futures::executor::block_on(cosmic::Renderer::new(
        Font::default(),
        Pixels(13.),
        Some("tiny-skia"),
    ))
    .unwrap()
}

#[test]
fn text_bar_shows_only_existing_workspaces_on_its_output() {
    let mut shell = fixture();
    let bar = shell.outputs[0].bar;
    let model = shell.text_bar_model(bar);
    assert_eq!(model.layout, "scroll");
    assert_eq!(model.cpu.as_deref(), Some("cpu 12%"));
    assert_eq!(model.disk.as_deref(), Some("disk 64%"));
    assert_eq!(model.network.as_deref(), Some("wifi 85%"));
    assert_eq!(model.throughput.as_deref(), Some("rx 1KiB/s tx 0B/s"));
    assert_eq!(model.workspaces.len(), 4);
    assert_eq!(
        model
            .workspaces
            .iter()
            .filter(|w| w.active)
            .map(|w| w.id)
            .collect::<Vec<_>>(),
        vec![Some(42)]
    );
    assert!(model.workspaces[3].urgent);
    assert_eq!(
        model.workspaces.iter().map(|w| w.id).collect::<Vec<_>>(),
        vec![Some(41), Some(42), Some(43), Some(44)]
    );
    shell.status.audio.as_mut().unwrap().muted = true;
    shell.status.battery = None;
    shell.snapshot.layouts.clear();
    shell.snapshot.binding_mode = Some("resize".into());
    let model = shell.text_bar_model(bar);
    assert_eq!(model.volume.as_deref(), Some("vol muted"));
    assert!(model.battery.is_none());
    assert_eq!(model.layout, "?");
    assert!(!model.interactive);
    assert!(model.title.contains("Esc to exit"));
}

#[test]
fn text_bar_layout_pixels_and_clicks_survive_redraws() {
    let mut renderer = renderer();
    for appearance in [
        ferese_config::theme::Appearance::Dark,
        ferese_config::theme::Appearance::Light,
    ] {
        for width in [320., 480., 800., 1440.] {
            let mut shell = fixture();
            shell.theme_snapshot.presented.appearance = appearance;
            shell.theme_snapshot.presented.tokens = ferese_config::theme::preset(
                if appearance == ferese_config::theme::Appearance::Dark {
                    "ferese-blue"
                } else {
                    "ferese-blue-light"
                },
                appearance,
            )
            .expect("built-in fixture theme");
            shell.config.apply_theme(&shell.theme_snapshot.presented);
            shell.config.font_family = Some("DejaVu Sans".into());
            shell.render = crate::rendering::Rendering::new(&shell.config);
            let mut focused = shell.snapshot.windows[0].clone();
            focused.id = 124;
            focused.workspace = 42;
            focused.urgent = false;
            focused.focused = true;
            focused.title = "ferese ~ cargo watch -x check".into();
            shell.snapshot.windows.push(focused);
            let bar = shell.outputs[0].bar;
            let viewport = Rectangle::with_size(Size::new(width, 24.));
            let mut view = shell.view_layer(bar);
            let mut tree = widget::Tree::new(&view);
            let node = view.as_widget_mut().layout(
                &mut tree,
                &renderer,
                &layout::Limits::new(Size::ZERO, viewport.size()),
            );
            let mut messages = Vec::new();
            view.as_widget_mut().update(
                &mut tree,
                &Event::Window(window::Event::RedrawRequested(std::time::Instant::now())),
                Layout::new(&node),
                mouse::Cursor::Unavailable,
                &renderer,
                &mut clipboard::Null,
                &mut Shell::new(&mut messages),
                &viewport,
            );
            let mut labels = Labels::default();
            view.as_widget_mut()
                .operate(&mut tree, Layout::new(&node), &renderer, &mut labels);
            assert!(
                labels.0.iter().any(|(label, _)| label == "settings"),
                "missing settings at {width}"
            );
            for (label, bounds) in &labels.0 {
                assert!(
                    bounds.x >= 0.
                        && bounds.y >= 0.
                        && bounds.x + bounds.width <= width + 0.1
                        && bounds.y + bounds.height <= 24.1,
                    "{label}: {bounds:?} outside {viewport:?}"
                );
            }
            let a11y =
                view.as_widget()
                    .a11y_nodes(Layout::new(&node), &tree, mouse::Cursor::Unavailable);
            let nodes: Vec<_> = a11y.root().iter().chain(a11y.children()).collect();
            if width == 1440. {
                for expected in ["cpu 12%", "disk 64%", "wifi 85%", "rx 1KiB/s tx 0B/s"] {
                    assert!(
                        labels.0.iter().any(|(label, _)| label == expected),
                        "missing {expected}"
                    );
                }
                for expected in ["cpu 12%", "disk 64%", "rx 1KiB/s tx 0B/s"] {
                    let (_, bounds) = labels
                        .0
                        .iter()
                        .find(|(label, _)| label == expected)
                        .unwrap();
                    for event in [
                        Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
                        Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)),
                    ] {
                        view.as_widget_mut().update(
                            &mut tree,
                            &event,
                            Layout::new(&node),
                            mouse::Cursor::Available(bounds.center()),
                            &renderer,
                            &mut clipboard::Null,
                            &mut Shell::new(&mut messages),
                            &viewport,
                        );
                    }
                    assert!(
                        !messages.iter().any(|message| matches!(
                            message,
                            cosmic::Action::App(Message::OpenPopover(..))
                        )),
                        "{expected} opened a popup"
                    );
                }
                for index in 1..=4 {
                    let name = format!("Workspace {index}");
                    let cell = nodes
                        .iter()
                        .find(|node| {
                            node.node()
                                .label()
                                .is_some_and(|label| label.starts_with(&name))
                        })
                        .unwrap();
                    let bounds = cell.node().bounds().unwrap();
                    assert_eq!(bounds.width(), 28.);
                    assert_eq!(bounds.height(), 24.);
                }
            }
            let target = nodes
                .iter()
                .find(|node| node.node().label() == Some("settings"))
                .unwrap()
                .node()
                .bounds()
                .unwrap();
            let point = Point::new(((target.x0 + target.x1) * 0.5) as f32, 12.);
            for event in [
                Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
                Event::Window(window::Event::RedrawRequested(std::time::Instant::now())),
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
                    &viewport,
                );
            }
            assert!(messages.iter().any(|message| matches!(message, cosmic::Action::App(Message::OpenPopover(status_ui::Menu::System, anchor)) if anchor.parent == bar)));
            renderer.reset(viewport);
            view.as_widget().draw(
                &tree,
                &mut renderer,
                &shell.config.theme.palette().native_theme(),
                &renderer::Style {
                    text_color: Color::WHITE,
                    icon_color: Color::WHITE,
                    scale_factor: 1.,
                },
                Layout::new(&node),
                mouse::Cursor::Unavailable,
                &viewport,
            );
            for scale in [1., 1.25, 1.5, 2.] {
                let physical = Size::new((width * scale) as u32, (24. * scale) as u32);
                let scaled = renderer.screenshot(physical, scale, Color::TRANSPARENT);
                assert!(
                    scaled.chunks_exact(4).all(|pixel| pixel[3] == 255),
                    "transparent pixels at scale {scale}"
                );
            }
            let pixels = renderer.screenshot(Size::new(width as u32, 24), 1., Color::TRANSPARENT);
            if width == 1440. {
                let pixel = |x: usize, y: usize| &pixels[(y * width as usize + x) * 4..][..4];
                let palette = Palette::from_resolved(&shell.theme_snapshot.presented);
                assert_eq!(pixel(30, 1), palette.accent.into_rgba8());
                assert_eq!(pixel(86, 1), palette.urgent.into_rgba8());
                assert_eq!(pixel(4, 4), palette.text.into_rgba8());
                assert_eq!(pixel(1, 23), palette.border.into_rgba8());
            }
            assert!(
                pixels.chunks_exact(4).all(|pixel| pixel[3] == 255),
                "bar must be opaque"
            );
            if let Some(directory) = std::env::var_os("FERESE_TEST_TEXT_BAR_PREVIEWS") {
                let directory = std::path::PathBuf::from(directory);
                std::fs::create_dir_all(&directory).unwrap();
                image_codec::save_buffer(
                    directory.join(format!("text-bar-{appearance:?}-{width}.png")),
                    &pixels,
                    width as u32,
                    24,
                    image_codec::ColorType::Rgba8,
                )
                .unwrap();
            }
        }
    }
}

#[test]
fn workspace_renumbering_cancels_a_held_press() {
    let renderer = renderer();
    let mut app = fixture();
    let bar = app.outputs[0].bar;
    let viewport = Rectangle::with_size(Size::new(1440., 24.));
    let mut tree = widget::Tree::empty();
    let mut send = |app: &FereseShell, event: Event, point: Point| {
        let mut view = app.view_layer(bar);
        tree.diff(&mut view);
        let node = view.as_widget_mut().layout(
            &mut tree,
            &renderer,
            &layout::Limits::new(Size::ZERO, viewport.size()),
        );
        let mut messages = Vec::new();
        view.as_widget_mut().update(
            &mut tree,
            &event,
            Layout::new(&node),
            mouse::Cursor::Available(point),
            &renderer,
            &mut clipboard::Null,
            &mut Shell::new(&mut messages),
            &viewport,
        );
        messages
            .into_iter()
            .filter_map(|message| {
                if let cosmic::Action::App(Message::ActivateWorkspace(id)) = message {
                    Some(id)
                } else {
                    None
                }
            })
            .collect::<Vec<_>>()
    };
    let press = || Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left));
    let release = || Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left));
    let first = Point::new(14., 12.);
    assert!(send(&app, press(), first).is_empty());
    app.snapshot.workspaces[0].id = 901;
    assert!(
        send(&app, release(), first).is_empty(),
        "a held press must not activate the replacement workspace"
    );
    assert!(send(&app, press(), first).is_empty());
    assert_eq!(send(&app, release(), first), vec![901]);
}

#[test]
fn dynamic_cells_follow_removal_sparse_indices_and_output_ownership() {
    let mut shell = fixture();
    let bar = shell.outputs[0].bar;
    shell
        .snapshot
        .workspaces
        .retain(|workspace| workspace.id != 42);
    shell.snapshot.workspaces[0].index = 12;
    shell.snapshot.workspaces[1].output = Some(6);
    let model = shell.text_bar_model(bar);
    assert_eq!(
        model
            .workspaces
            .iter()
            .map(|w| (w.index, w.id))
            .collect::<Vec<_>>(),
        vec![(4, Some(44)), (12, Some(41))]
    );
    assert!(model.workspaces.iter().all(|w| !w.active));
    shell.snapshot.workspaces.clear();
    assert!(shell.text_bar_model(bar).workspaces.is_empty());
}

#[test]
fn text_wifi_distinguishes_disabled_disconnected_and_unavailable() {
    let mut shell = fixture();
    let bar = shell.outputs[0].bar;
    shell.status.network.as_mut().unwrap().enabled = false;
    assert_eq!(
        shell.text_bar_model(bar).network.as_deref(),
        Some("wifi off")
    );
    let network = shell.status.network.as_mut().unwrap();
    network.enabled = true;
    network.connection = None;
    assert_eq!(
        shell.text_bar_model(bar).network.as_deref(),
        Some("wifi offline")
    );
    shell.status.network = None;
    assert!(shell.text_bar_model(bar).network.is_none());
    shell.system_stats = Default::default();
    let model = shell.text_bar_model(bar);
    assert!(model.cpu.is_none() && model.disk.is_none() && model.throughput.is_none());
}

#[test]
fn text_bar_repaints_same_mode_theme_changes_without_recreating_the_surface() {
    use cosmic::Application;
    use ferese_config::theme::Appearance;

    let mut shell = fixture();
    let mut renderer = renderer();
    let bar = shell.outputs[0].bar;
    let mut tree = widget::Tree::empty();
    let viewport = Rectangle::with_size(Size::new(1440., 24.));
    for (appearance, background, accent, text, border) in [
        (Appearance::Dark, "#182818", "#80E0A0", "#E8F8E8", "#406040"),
        (Appearance::Dark, "#302010", "#FFC080", "#FFF0E0", "#786050"),
        (
            Appearance::Light,
            "#F0E0D0",
            "#804020",
            "#201008",
            "#907060",
        ),
    ] {
        let mut snapshot = shell.theme_snapshot.clone();
        snapshot.presented.appearance = appearance;
        let tokens = &mut snapshot.presented.tokens;
        tokens.surface.bar.background = background.into();
        tokens.surface.bar.text_primary = text.into();
        tokens.surface.bar.text_muted = text.into();
        tokens.colors.accent = accent.into();
        tokens.colors.border = border.into();
        // Distinct general surface tokens catch accidentally using popup colors.
        tokens.colors.surface_base = "#080810".into();
        tokens.colors.text_primary = "#B0B0B0".into();
        drop(shell.update(Message::ThemeChanged(Box::new(snapshot))));
        assert_eq!(
            shell.outputs[0].bar, bar,
            "theme changes must reuse the panel surface"
        );

        let mut view = shell.view_layer(bar);
        tree.diff(view.as_widget_mut());
        let node = view.as_widget_mut().layout(
            &mut tree,
            &renderer,
            &layout::Limits::new(Size::ZERO, viewport.size()),
        );
        renderer.reset(viewport);
        view.as_widget().draw(
            &tree,
            &mut renderer,
            &shell.config.theme.palette().native_theme(),
            &renderer::Style {
                text_color: Color::WHITE,
                icon_color: Color::WHITE,
                scale_factor: 1.,
            },
            Layout::new(&node),
            mouse::Cursor::Unavailable,
            &viewport,
        );
        let pixels = renderer.screenshot(Size::new(1440, 24), 1., Color::TRANSPARENT);
        for (x, y, expected, role) in [
            (1400, 1, background, "bar background"),
            (30, 1, accent, "active workspace"),
            (4, 4, text, "occupied workspace marker"),
            (1400, 23, border, "bottom border"),
        ] {
            let offset = (y * 1440 + x) * 4;
            assert_eq!(
                &pixels[offset..offset + 4],
                ferese_theme::parse_color(expected)
                    .expect("valid fixture color")
                    .into_rgba8(),
                "{role} did not follow the {appearance:?} theme with background {background}"
            );
        }
    }
}
