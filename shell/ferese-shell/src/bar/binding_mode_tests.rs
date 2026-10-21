use super::*;
use cosmic::iced::advanced::{Layout, Renderer, Shell, clipboard, layout, mouse, renderer, widget};
use cosmic::iced::{Event, Rectangle, Size};

#[derive(Default)]
struct TextBounds(Vec<(String, Rectangle)>);

impl widget::Operation for TextBounds {
    fn traverse(&mut self, visit: &mut dyn FnMut(&mut dyn widget::Operation)) {
        visit(self);
    }
    fn text(&mut self, _: Option<&widget::Id>, bounds: Rectangle, text: &str) {
        self.0.push((text.to_owned(), bounds));
    }
}

#[test]
fn a_binding_mode_owns_the_panel_instead_of_hidden_morph_hover_or_menus() {
    let mut shell = crate::tests::shell_with_measured_panel();
    shell.config.panels[0].style = ferese_panel::PanelStyle::Morph;
    shell.outputs[0]
        .runtime
        .reset(&shell.config.panels[0], &shell.snapshot, Some("test"));
    let id = shell.outputs[0].bar;
    shell.snapshot.binding_mode = Some("resize".into());
    let allocation = shell.outputs[0]
        .runtime
        .hub
        .as_ref()
        .unwrap()
        .borrow()
        .allocation;
    drop(shell.hub_hover(id, true));
    assert_eq!(shell.hub_mode(id), Some(ferese_panel::morph::Mode::Idle));
    drop(shell.open_hub(id, status_ui::Menu::Calendar));
    assert_eq!(shell.hub_mode(id), Some(ferese_panel::morph::Mode::Idle));
    assert_eq!(
        shell.outputs[0]
            .runtime
            .hub
            .as_ref()
            .unwrap()
            .borrow()
            .allocation,
        allocation
    );
}

#[test]
fn mode_entry_dismisses_existing_popovers_and_collapses_expanded_morph_content() {
    for style in [
        ferese_panel::PanelStyle::Flat,
        ferese_panel::PanelStyle::Morph,
    ] {
        let mut shell = crate::tests::shell_with_measured_panel();
        shell.config.panels[0].style = style;
        shell.outputs[0]
            .runtime
            .reset(&shell.config.panels[0], &shell.snapshot, Some("test"));
        let id = shell.outputs[0].bar;
        let collapsed = shell.outputs[0]
            .runtime
            .hub
            .as_ref()
            .map(|hub| hub.borrow().allocation);
        if collapsed.is_some() {
            drop(shell.open_hub(id, status_ui::Menu::Calendar));
            assert!(shell.hub_mode(id).unwrap().expanded());
        }
        let anchor = status_ui::PopoverAnchor {
            parent: id,
            panel: shell.config.panels[0].id.clone(),
            item: Some(crate::panel::ItemId("clock".into())),
            rectangle: Default::default(),
        };
        shell.menu = Some(status_ui::OpenMenu {
            id: window::Id::unique(),
            anchor: anchor.clone(),
            kind: status_ui::Menu::Calendar,
            motion: crate::motion::PopupMotion::new(Default::default()),
            effects: None,
            regions: Default::default(),
        });
        shell.workspace_ui.tooltip = Some(window::Id::unique());
        shell.snapshot.binding_mode = Some("resize".into());
        drop(shell.binding_mode_transition(false));
        assert!(shell.menu.is_none());
        assert!(shell.workspace_ui.tooltip.is_none());
        drop(shell.open_menu(status_ui::Menu::Calendar, anchor));
        assert!(
            shell.menu.is_none(),
            "a queued click on a displaced control must not reopen its popup"
        );
        if let Some(collapsed) = collapsed {
            assert_eq!(shell.hub_mode(id), Some(ferese_panel::morph::Mode::Idle));
            let hub = shell.outputs[0].runtime.hub.as_ref().unwrap().borrow();
            assert_eq!(hub.allocation, collapsed);
            assert!(!hub.hub.active());
        }
        // Another snapshot while the mode is active does not restart the handoff.
        drop(shell.binding_mode_transition(true));
        shell.snapshot.binding_mode = None;
        drop(shell.binding_mode_transition(true));
        if collapsed.is_some() {
            drop(shell.open_hub(id, status_ui::Menu::Calendar));
            assert!(shell.hub_mode(id).unwrap().expanded());
        }
    }
}

#[test]
fn mode_name_and_escape_hint_fit_the_published_shape_in_every_panel_style() {
    use cosmic::iced::advanced::renderer::Headless;
    let mut renderer = cosmic::iced::futures::executor::block_on(cosmic::Renderer::new(
        cosmic::iced::Font::default(),
        cosmic::iced::Pixels(14.),
        Some("tiny-skia"),
    ))
    .unwrap();
    let previews = std::env::var_os("FERESE_TEST_MODE_PREVIEWS").map(std::path::PathBuf::from);
    if let Some(path) = &previews {
        std::fs::create_dir_all(path).unwrap();
    }
    for style in ferese_panel::PanelStyle::AVAILABLE {
        for name in ["resize", "resize_mode_with_a_long_name_and_a_required_exit"] {
            let mut shell = crate::tests::shell_with_measured_panel();
            shell.config.panels[0].style = style;
            let style = style.key();
            shell.snapshot.binding_mode = Some(name.into());
            shell.outputs[0]
                .runtime
                .reset(&shell.config.panels[0], &shell.snapshot, Some("test"));
            let id = shell.outputs[0].bar;
            let allocation = shell.outputs[0]
                .runtime
                .hub
                .as_ref()
                .map(|hub| hub.borrow().allocation)
                .unwrap_or([
                    1200,
                    shell.config.panels[0].resolved_geometry().height as u32,
                ]);
            let viewport =
                Rectangle::with_size(Size::new(allocation[0] as f32, allocation[1] as f32));
            let mut view = shell.view_layer(id);
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
            let scene = messages
                .iter()
                .find_map(|message| match message {
                    cosmic::Action::App(Message::PanelSurfaceChanged(_, scene)) => Some(scene),
                    _ => None,
                })
                .unwrap_or_else(|| {
                    panic!(
                        "{style}: mode must replace the previous native clipping and input shape"
                    )
                });
            let mut labels = TextBounds::default();
            view.as_widget_mut()
                .operate(&mut tree, Layout::new(&node), &renderer, &mut labels);
            assert!(
                labels.0.iter().any(|(text, _)| text.contains("resize")),
                "{style}: mode name missing"
            );
            assert!(
                labels
                    .0
                    .iter()
                    .any(|(text, _)| text.contains("Esc to exit")),
                "{style}: exit hint missing"
            );
            for (text, bounds) in &labels.0 {
                assert!(
                    bounds.x >= 0.
                        && bounds.y >= 0.
                        && bounds.x + bounds.width <= viewport.width + 0.1
                        && bounds.y + bounds.height <= viewport.height + 0.1,
                    "{style}: {text} exceeds its backing surface: {bounds:?}"
                );
                assert!(
                    scene.paint.iter().any(|paint| {
                        let rect = paint.bounds;
                        f64::from(bounds.x) >= rect.x - 0.1
                            && f64::from(bounds.y) >= rect.y - 0.1
                            && f64::from(bounds.x + bounds.width) <= rect.x + rect.width + 0.1
                            && f64::from(bounds.y + bounds.height) <= rect.y + rect.height + 0.1
                    }),
                    "{style}: {text} is outside the native paint shape"
                );
            }
            renderer.reset(viewport);
            view.as_widget().draw(
                &tree,
                &mut renderer,
                &shell.config.theme.palette().native_theme(),
                &renderer::Style {
                    text_color: color(shell.config.theme.for_bar().text_primary),
                    icon_color: Color::WHITE,
                    scale_factor: 1.,
                },
                Layout::new(&node),
                mouse::Cursor::Unavailable,
                &viewport,
            );
            let pixels = renderer.screenshot(
                Size::new(allocation[0], allocation[1]),
                1.,
                Color::TRANSPARENT,
            );
            assert!(
                pixels.chunks_exact(4).any(|pixel| pixel[3] > 0),
                "{style}: no visible pixels"
            );
            if let Some(path) = &previews {
                image_codec::save_buffer(
                    path.join(format!(
                        "mode-{style}{}.png",
                        if name == "resize" { "" } else { "-long" }
                    )),
                    &pixels,
                    allocation[0],
                    allocation[1],
                    image_codec::ColorType::Rgba8,
                )
                .unwrap();
            }
        }
    }
}
