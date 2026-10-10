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
fn panel_preview_stays_visible_while_the_items_editor_scrolls() {
    #[derive(Default)]
    struct Bounds {
        preview: Option<Rectangle>,
        viewport: Option<Rectangle>,
    }
    impl Operation for Bounds {
        fn traverse(&mut self, children: &mut dyn FnMut(&mut dyn Operation)) {
            children(self);
        }
        fn container(&mut self, id: Option<&widget::Id>, bounds: Rectangle) {
            if id == Some(&widget::Id::new("panel-composition-preview")) {
                self.preview = Some(bounds);
            }
        }
        fn scrollable(
            &mut self,
            id: Option<&widget::Id>,
            bounds: Rectangle,
            _: Rectangle,
            _: Vector,
            _: &mut dyn cosmic::iced::advanced::widget::operation::Scrollable,
        ) {
            if id == Some(&widget::Id::new("settings-content")) {
                self.viewport = Some(bounds);
            }
        }
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let renderer = runtime
        .block_on(<cosmic::Renderer as Headless>::new(
            Font::default(),
            Pixels(14.),
            Some("tiny-skia"),
        ))
        .unwrap();
    let mut app = App::init(
        Core::default(),
        ("/unused/editor.kdl".into(), Snapshot::parse(String::new()), None),
    )
    .0;
    app.page = Page::Bar;
    app.panel_page = panel_controls::PanelPage::Arrange;
    app.draft
        .edit(&panel_controls::initialize(&app.draft).unwrap())
        .unwrap();
    let mut view = app.page_view();
    let mut tree = Tree::new(&view);
    let node = view.as_widget_mut().layout(
        &mut tree,
        &renderer,
        &layout::Limits::new(Size::ZERO, Size::new(740., 650.)),
    );
    let mut before = Bounds::default();
    view.as_widget_mut()
        .operate(&mut tree, Layout::new(&node), &renderer, &mut before);
    let mut scroll = cosmic::iced::advanced::widget::operation::scrollable::scroll_to::<()>(
        widget::Id::new("settings-content"),
        cosmic::iced::widget::scrollable::AbsoluteOffset { x: None, y: Some(200.) },
    );
    view.as_widget_mut()
        .operate(&mut tree, Layout::new(&node), &renderer, &mut scroll);
    let mut position = ScrollPosition::default();
    view.as_widget_mut()
        .operate(&mut tree, Layout::new(&node), &renderer, &mut position);
    assert!(position.offset > 0. && position.content_height > position.viewport_height);
    let mut after = Bounds::default();
    view.as_widget_mut()
        .operate(&mut tree, Layout::new(&node), &renderer, &mut after);
    assert_eq!(before.preview, after.preview);
    let preview = after.preview.unwrap();
    let viewport = after.viewport.unwrap();
    assert!(preview.y + preview.height <= viewport.y);
    assert!(preview.height > 0. && viewport.height > 0.);
}

#[test]
#[ignore = "release scroll/render sample; requires headless renderer backends"]
fn appearance_scroll_preserves_progress_and_settles_visibility() {
    #[derive(Default)]
    struct Labels {
        labels: Vec<Rectangle>,
        viewport: Option<Rectangle>,
        translation: Vector,
    }
    impl Operation for Labels {
        fn traverse(&mut self, children: &mut dyn FnMut(&mut dyn Operation)) {
            children(self);
        }
        fn text(&mut self, _: Option<&widget::Id>, bounds: Rectangle, text: &str) {
            if matches!(
                text,
                "Shell corner radius" | "Shell opacity" | "Background blur" | "Interface font" | "Focus border"
            ) {
                self.labels.push(bounds);
            }
        }
        fn scrollable(
            &mut self,
            id: Option<&widget::Id>,
            bounds: Rectangle,
            _: Rectangle,
            translation: Vector,
            _: &mut dyn cosmic::iced::advanced::widget::operation::Scrollable,
        ) {
            if id == Some(&widget::Id::new("settings-content")) {
                self.viewport = Some(bounds);
                self.translation = translation;
            }
        }
    }
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
            let mut checked_labels = 0;

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
                            y: if frame < 28 { -48. } else { 48. },
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
                let mut labels = Labels::default();
                view.as_widget_mut()
                    .operate(tree, Layout::new(&node), &renderer, &mut labels);
                let started = std::time::Instant::now();
                let pixels = Headless::screenshot(&mut renderer, physical, scale, theme.cosmic().bg_color().into());
                if (4..52).contains(&frame) {
                    render_times.push(started.elapsed().as_micros());
                }
                if let Some(viewport) = labels.viewport {
                    for bounds in labels.labels {
                        let bounds = bounds - labels.translation;
                        if bounds.y < viewport.y + 2. || bounds.y + bounds.height > viewport.y + viewport.height - 2. {
                            continue;
                        }
                        let mut ink = 0;
                        for y in (bounds.y * scale).ceil() as u32..((bounds.y + bounds.height) * scale).floor() as u32 {
                            for x in
                                (bounds.x * scale).ceil() as u32..((bounds.x + bounds.width) * scale).floor() as u32
                            {
                                let index = ((y * physical.width + x) * 4) as usize;
                                if pixels[index..index + 3].iter().any(|channel| *channel > 140) {
                                    ink += 1;
                                }
                            }
                        }
                        assert!(
                            ink > 12,
                            "missing field text: backend={backend} split={split} frame={frame} bounds={bounds:?}"
                        );
                        checked_labels += 1;
                    }
                }
                drop(view);

                for message in messages {
                    if matches!(message, Message::RowVisibility(..)) {
                        visibility_messages += 1;
                        assert!(frame < 56, "visibility kept changing after scrolling stopped");
                    }

                    let _ = app.update(message);
                }
            }

            render_times.sort_unstable();
            assert!(checked_labels > 20, "scroll check must include visible field labels");
            eprintln!(
                "backend={backend} split={split} render_median_us={} p95_us={} visibility_messages={visibility_messages}",
                render_times[render_times.len() / 2],
                render_times[render_times.len() * 95 / 100]
            );
        }
    }
}

#[test]
fn gpu_panel_text_survives_preview_clipping_and_scrolling() {
    // Native startup preloads text fonts; headless renderers skip that step.
    // Supply a bundled proportional font even on minimal CI images.
    cosmic::iced::advanced::graphics::text::font_system()
        .write()
        .unwrap()
        .load_font(std::borrow::Cow::Borrowed(include_bytes!(
            "../../../assets/fonts/Comfortaa-Regular.otf"
        )));

    #[derive(Default)]
    struct Labels {
        labels: Vec<(String, Rectangle, Option<Rectangle>)>,
        traversal_start: usize,
    }
    impl Operation for Labels {
        fn traverse(&mut self, children: &mut dyn FnMut(&mut dyn Operation)) {
            let start = self.labels.len();
            children(self);
            self.traversal_start = start;
        }
        fn text(&mut self, _: Option<&widget::Id>, bounds: Rectangle, text: &str) {
            if matches!(
                text,
                "Top panel"
                    | "Shown on all displays"
                    | "Position"
                    | "Background"
                    | "Default group surface"
                    | "Background opacity"
                    | "Group borders"
                    | "Corner radius"
                    | "Size & spacing"
                    | "Side margins"
                    | "Window clearance"
                    | "Arrangement"
                    | "Start"
                    | "Center"
                    | "End"
                    | "Add group"
                    | "Add item"
                    | "Overview"
                    | "Workspaces"
                    | "Battery"
                    | "Placement"
                    | "Show item"
                    | "Overflow"
                    | "Preferred size"
                    | "Battery percentage"
                    | "Make it your own"
                    | "Surface"
                    | "Item spacing"
                    | "Vertical padding"
                    | "Horizontal padding"
                    | "Undo"
                    | "Reload configuration"
            ) {
                self.labels.push((text.into(), bounds, None));
            }
        }
        fn scrollable(
            &mut self,
            _: Option<&widget::Id>,
            viewport: Rectangle,
            _: Rectangle,
            translation: Vector,
            _: &mut dyn cosmic::iced::advanced::widget::operation::Scrollable,
        ) {
            // Scrollable reports its offset after traversing its content.
            for (_, bounds, clip) in &mut self.labels[self.traversal_start..] {
                *bounds = *bounds - translation;
                *clip = Some(clip.map_or(viewport, |clip| {
                    (clip - translation).intersection(&viewport).unwrap_or_default()
                }));
            }
        }
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let mut renderer = runtime
        .block_on(<cosmic::Renderer as Headless>::new(
            Font::default(),
            Pixels(14.),
            Some("wgpu"),
        ))
        .expect("GPU panel rendering requires a hardware or software WGPU adapter");
    for width in [740., 1100.] {
        for scale in [1., 1.75] {
            for selection in ["panels", "items", "battery", "status"] {
                let mut app = App::init(
                    Core::default(),
                    (
                        "/unused/panel-text.kdl".into(),
                        Snapshot::parse("theme { mode dark; family \"ferese-blue\"; }".into()),
                        Some(InitialPage::Bar),
                    ),
                )
                .0;
                app.font = Font::with_name("Comfortaa");
                app.panel_page = if selection == "panels" {
                    panel_controls::PanelPage::Panels
                } else {
                    panel_controls::PanelPage::Arrange
                };
                app.panel_selection = (selection == "battery").then(|| ferese_config::panel::ItemId("battery".into()));
                app.panel_group_selection =
                    (selection == "status").then(|| ferese_config::panel::GroupId("status".into()));
                let bounds = Rectangle::with_size(Size::new(width, 650.));
                let physical = Size::new((width * scale) as u32, (650. * scale) as u32);
                let theme = app.native_palette.native_theme();
                let mut view = app.page_view();
                let mut tree = Tree::new(&view);
                let node =
                    view.as_widget_mut()
                        .layout(&mut tree, &renderer, &layout::Limits::new(Size::ZERO, bounds.size()));
                let mut checked = 0;
                for offset in [0., 180., 400.] {
                    for id in ["settings-content", "panel-inspector-scroll"] {
                        let mut scroll = cosmic::iced::advanced::widget::operation::scrollable::scroll_to::<()>(
                            widget::Id::new(id),
                            cosmic::iced::widget::scrollable::AbsoluteOffset {
                                x: None,
                                y: Some(offset),
                            },
                        );
                        view.as_widget_mut()
                            .operate(&mut tree, Layout::new(&node), &renderer, &mut scroll);
                    }
                    renderer.reset(bounds);
                    view.as_widget().draw(
                        &tree,
                        &mut renderer,
                        &theme,
                        &renderer::Style {
                            text_color: app.native_palette.text,
                            icon_color: app.native_palette.text,
                            scale_factor: scale as f64,
                        },
                        Layout::new(&node),
                        mouse::Cursor::Unavailable,
                        &bounds,
                    );
                    if let Some(mut overlay) =
                        view.as_widget_mut()
                            .overlay(&mut tree, Layout::new(&node), &renderer, &bounds, Vector::ZERO)
                    {
                        let overlay = overlay.as_overlay_mut();
                        let node = overlay.layout(&renderer, bounds.size());
                        renderer.with_layer(bounds, |renderer| {
                            overlay.draw(
                                renderer,
                                &theme,
                                &renderer::Style {
                                    text_color: app.native_palette.text,
                                    icon_color: app.native_palette.text,
                                    scale_factor: scale as f64,
                                },
                                Layout::new(&node),
                                mouse::Cursor::Unavailable,
                            );
                        });
                    }
                    let mut labels = Labels::default();
                    view.as_widget_mut()
                        .operate(&mut tree, Layout::new(&node), &renderer, &mut labels);
                    let pixels = Headless::screenshot(&mut renderer, physical, scale, theme.cosmic().bg_color().into());
                    for (label, rect, clip) in labels.labels {
                        let clip = clip.unwrap_or(bounds);
                        if rect.width <= 0.
                            || rect.height <= 0.
                            || rect.x < clip.x
                            || rect.y < clip.y + 2.
                            || rect.x + rect.width > clip.x + clip.width
                            || rect.y + rect.height > clip.y + clip.height - 2.
                        {
                            continue;
                        }
                        let mut ink = 0;
                        for y in (rect.y * scale).ceil() as u32..((rect.y + rect.height) * scale).floor() as u32 {
                            for x in (rect.x * scale).ceil() as u32..((rect.x + rect.width) * scale).floor() as u32 {
                                let index = ((y * physical.width + x) * 4) as usize;
                                if pixels[index..index + 3].iter().any(|channel| *channel > 140) {
                                    ink += 1;
                                }
                            }
                        }
                        assert!(
                            ink > 12,
                            "missing panel text {label:?}: state={selection} width={width} scale={scale} offset={offset} bounds={rect:?}"
                        );
                        checked += 1;
                    }
                }
                assert!(checked >= 6, "state={selection} must exercise visible panel labels");
            }
        }
    }
}

/// Reproducible sketches rendered from the actual widgets, with no live services.
#[test]
#[ignore = "writes Panel & Shell review images to /tmp/ferese-panel-review-{backend}"]
fn render_panel_review_states() {
    let backend = std::env::var("FERESE_REVIEW_BACKEND").unwrap_or_else(|_| "tiny-skia".into());
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let mut renderer = runtime
        .block_on(<cosmic::Renderer as Headless>::new(
            Font::default(),
            Pixels(14.),
            Some(&backend),
        ))
        .unwrap();
    let directory = format!("/tmp/ferese-panel-review-{backend}");
    std::fs::create_dir_all(&directory).unwrap();
    let mut app = App::init(
        Core::default(),
        (
            "/unused/review.kdl".into(),
            Snapshot::parse(String::new()),
            Some(InitialPage::Bar),
        ),
    )
    .0;
    app.status = "Changes save automatically".into();
    for appearance in ["light", "dark"] {
        let theme_config = Snapshot::parse(format!("theme {{ mode \"{appearance}\"; }}")).unwrap();
        let theme = ferese_config::theme::resolve(
            &theme_config.doc,
            std::path::Path::new("/tmp"),
            "2026-10-07T12:00:00Z".parse().unwrap(),
            |_| unreachable!(),
        )
        .unwrap()
        .theme;
        app.resolved.presented = theme.clone();
        app.resolved.theme = theme;
        app.font = fonts::interface_font(&app.resolved.presented.tokens.typography.font_family);
        app.native_palette = visuals::Palette::from_resolved(&app.resolved.presented);
        for (name, tab, item, group) in [
            ("panels", panel_controls::PanelPage::Panels, None, None),
            ("mixed-surfaces", panel_controls::PanelPage::Panels, None, None),
            (
                "surface-inspector",
                panel_controls::PanelPage::Arrange,
                None,
                Some("title"),
            ),
            ("corners", panel_controls::PanelPage::Panels, None, None),
            ("preview-hover", panel_controls::PanelPage::Panels, None, None),
            ("navigation", panel_controls::PanelPage::Arrange, None, None),
            ("shortcuts", panel_controls::PanelPage::Panels, None, None),
            ("motion", panel_controls::PanelPage::Panels, None, None),
            ("items", panel_controls::PanelPage::Arrange, None, None),
            (
                "workspace-styles",
                panel_controls::PanelPage::Arrange,
                Some("workspaces"),
                None,
            ),
            (
                "selected-item",
                panel_controls::PanelPage::Arrange,
                Some("battery"),
                None,
            ),
            (
                "selected-group",
                panel_controls::PanelPage::Arrange,
                None,
                Some("status"),
            ),
        ] {
            app.page = match name {
                "shortcuts" => Page::Shortcuts,
                "motion" => Page::Motion,
                _ => Page::Bar,
            };
            app.draft = Snapshot::parse(String::new()).unwrap();
            if matches!(name, "mixed-surfaces" | "surface-inspector") {
                let mut panel = ferese_config::panel::Panel::from_preset(ferese_config::PanelPreset::Islands);
                panel.center.groups[0].surface = Some(ferese_config::panel::GroupSurface::None);
                panel.end.groups[0].surface = Some(ferese_config::panel::GroupSurface::Inset);
                panel.geometry.edge_margin = 6;
                app.draft
                    .edit(&crate::store::set("panels", serde_json::to_value(vec![panel]).unwrap()))
                    .unwrap();
            }
            if name == "corners" {
                for edit in crate::panel_edit::plan(
                    &app.draft,
                    crate::panel_edit::Action::SetRadius("4px 8px 12px 16px".into()),
                )
                .unwrap()
                {
                    app.draft.edit(&edit).unwrap();
                }
                app.draft = Snapshot::parse(app.draft.source.clone()).unwrap();
            }
            app.panel_page = tab;
            app.sidebar_open = name == "navigation";
            app.panel_selection = item.map(|id| ferese_config::panel::ItemId(id.into()));
            app.panel_group_selection = group.map(|id| ferese_config::panel::GroupId(id.into()));
            for (width, height) in [(1100, 860), (740, 650)] {
                let bounds = Rectangle::with_size(Size::new(width as f32, height as f32));
                let theme = app.native_palette.native_theme();
                let mut view = app.page_view();
                let mut tree = Tree::new(&view);
                let node =
                    view.as_widget_mut()
                        .layout(&mut tree, &renderer, &layout::Limits::new(Size::ZERO, bounds.size()));
                #[derive(Default)]
                struct HoverTarget(Option<Rectangle>);
                impl Operation for HoverTarget {
                    fn traverse(&mut self, children: &mut dyn FnMut(&mut dyn Operation)) {
                        children(self);
                    }
                    fn container(&mut self, id: Option<&widget::Id>, bounds: Rectangle) {
                        if id == Some(&widget::Id::new("preview-item:battery")) {
                            self.0 = Some(bounds);
                        }
                    }
                }
                let cursor = if name == "preview-hover" {
                    let mut target = HoverTarget::default();
                    view.as_widget_mut()
                        .operate(&mut tree, Layout::new(&node), &renderer, &mut target);
                    mouse::Cursor::Available(target.0.expect("battery is visible in the review preview").center())
                } else {
                    mouse::Cursor::Unavailable
                };
                // Responsive content is laid out before drawing, as it is in the app.
                renderer.reset(bounds);
                view.as_widget().draw(
                    &tree,
                    &mut renderer,
                    &theme,
                    &renderer::Style {
                        text_color: Color::WHITE,
                        icon_color: Color::WHITE,
                        scale_factor: 1.,
                    },
                    Layout::new(&node),
                    cursor,
                    &bounds,
                );
                if let Some(mut overlay) =
                    view.as_widget_mut()
                        .overlay(&mut tree, Layout::new(&node), &renderer, &bounds, Vector::ZERO)
                {
                    let overlay = overlay.as_overlay_mut();
                    let layout = overlay.layout(&renderer, bounds.size());
                    renderer.with_layer(bounds, |renderer| {
                        overlay.draw(
                            renderer,
                            &theme,
                            &renderer::Style {
                                text_color: app.native_palette.text,
                                icon_color: app.native_palette.text,
                                scale_factor: 1.,
                            },
                            Layout::new(&layout),
                            mouse::Cursor::Unavailable,
                        );
                    });
                }
                let pixels = Headless::screenshot(
                    &mut renderer,
                    Size::new(width, height),
                    1.,
                    theme.cosmic().bg_color().into(),
                );
                let path = format!("{directory}/{appearance}-{name}-{width}.png");
                image::save_buffer(&path, &pixels, width, height, image::ColorType::Rgba8).unwrap();
                eprintln!("{path}");
            }
        }
    }
}

#[test]
fn selecting_panel_items_preserves_editor_scroll_and_pane_bounds() {
    #[derive(Default, Debug, PartialEq)]
    struct Panes {
        preview: Option<Rectangle>,
        editor: Option<Rectangle>,
        inspector: Option<Rectangle>,
    }
    impl Operation for Panes {
        fn traverse(&mut self, children: &mut dyn FnMut(&mut dyn Operation)) {
            children(self);
        }
        fn container(&mut self, id: Option<&widget::Id>, bounds: Rectangle) {
            if id == Some(&widget::Id::new("panel-composition-preview")) {
                self.preview = Some(bounds);
            }
        }
        fn scrollable(
            &mut self,
            id: Option<&widget::Id>,
            bounds: Rectangle,
            _: Rectangle,
            _: Vector,
            _: &mut dyn cosmic::iced::advanced::widget::operation::Scrollable,
        ) {
            if id == Some(&widget::Id::new("settings-content")) {
                self.editor = Some(bounds);
            }
            if id == Some(&widget::Id::new("panel-inspector-scroll")) {
                self.inspector = Some(bounds);
            }
        }
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let renderer = runtime
        .block_on(<cosmic::Renderer as Headless>::new(
            Font::default(),
            Pixels(14.),
            Some("tiny-skia"),
        ))
        .unwrap();
    for width in [740., 1100.] {
        let mut app = App::init(
            Core::default(),
            (
                "/unused/layout.kdl".into(),
                Snapshot::parse(String::new()),
                Some(InitialPage::Bar),
            ),
        )
        .0;
        app.panel_page = panel_controls::PanelPage::Arrange;
        let mut tree = None;
        let mut initial = None;
        for selection in [
            None,
            Some(Message::PanelSelect(ferese_config::panel::ItemId("battery".into()))),
            Some(Message::PanelGroupSelect(ferese_config::panel::GroupId(
                "status".into(),
            ))),
            Some(Message::PanelClearSelection),
        ] {
            if let Some(selection) = selection {
                let _ = app.update(selection);
            }
            let mut view = app.page_view();
            let tree = tree.get_or_insert_with(|| Tree::new(&view));
            tree.diff(view.as_widget_mut());
            let node = view.as_widget_mut().layout(
                tree,
                &renderer,
                &layout::Limits::new(Size::ZERO, Size::new(width, 650.)),
            );
            if initial.is_none() {
                let mut scroll = cosmic::iced::advanced::widget::operation::scrollable::scroll_to::<()>(
                    widget::Id::new("settings-content"),
                    cosmic::iced::widget::scrollable::AbsoluteOffset { x: None, y: Some(180.) },
                );
                view.as_widget_mut()
                    .operate(tree, Layout::new(&node), &renderer, &mut scroll);
            }
            let mut panes = Panes::default();
            view.as_widget_mut()
                .operate(tree, Layout::new(&node), &renderer, &mut panes);
            let mut position = ScrollPosition::default();
            view.as_widget_mut()
                .operate(tree, Layout::new(&node), &renderer, &mut position);
            assert_eq!(position.offset, 180., "selection moved the arrangement");
            let editor = panes.editor.unwrap();
            let inspector = panes.inspector.unwrap();
            assert!(editor.x + editor.width < inspector.x, "panes overlap");
            assert!(inspector.x + inspector.width <= width, "inspector leaves window");
            assert!(editor.height > 100. && inspector.height > 100.);
            if let Some(initial) = &initial {
                assert_eq!(&panes, initial);
            } else {
                initial = Some(panes);
            }
            assert!(app.pending.is_empty() && !app.saving && app.draft.item("panels").is_none());
        }
    }
}

#[test]
fn sidebar_collapses_at_the_breakpoint_and_compact_navigation_dismisses() {
    #[derive(Default)]
    struct Bounds {
        sidebar: Option<Rectangle>,
        page: Option<Rectangle>,
        toggle: Option<Rectangle>,
    }
    impl Operation for Bounds {
        fn traverse(&mut self, children: &mut dyn FnMut(&mut dyn Operation)) {
            children(self);
        }
        fn container(&mut self, id: Option<&widget::Id>, bounds: Rectangle) {
            if id == Some(&widget::Id::new("settings-sidebar")) {
                self.sidebar = Some(bounds);
            }
            if id == Some(&widget::Id::new("settings-page")) {
                self.page = Some(bounds);
            }
            if id == Some(&widget::Id::new("settings-navigation-toggle")) {
                self.toggle = Some(bounds);
            }
        }
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let renderer = runtime
        .block_on(<cosmic::Renderer as Headless>::new(
            Font::default(),
            Pixels(14.),
            Some("tiny-skia"),
        ))
        .unwrap();
    let mut app = App::init(
        Core::default(),
        (
            "/unused/sidebar.kdl".into(),
            Snapshot::parse(String::new()),
            Some(InitialPage::Bar),
        ),
    )
    .0;
    app.panel_page = panel_controls::PanelPage::Arrange;
    let mut tree = None;
    for width in [1100., 959., 740., 960., 1100.] {
        let mut view = app.page_view();
        let tree = tree.get_or_insert_with(|| Tree::new(&view));
        tree.diff(view.as_widget_mut());
        let node = view.as_widget_mut().layout(
            tree,
            &renderer,
            &layout::Limits::new(Size::ZERO, Size::new(width, 650.)),
        );
        let mut bounds = Bounds::default();
        view.as_widget_mut()
            .operate(tree, Layout::new(&node), &renderer, &mut bounds);
        let compact = width < 960.;
        assert_eq!(bounds.sidebar.is_none(), compact);
        assert_eq!(bounds.toggle.is_some(), compact);
        assert_eq!(bounds.page.unwrap().x, if compact { 0. } else { 204. });
        assert_eq!(bounds.page.unwrap().width, if compact { width } else { width - 204. });
    }
    let _ = app.update(Message::Sidebar(true));
    assert!(app.sidebar_open);
    let mut view = app.page_view();
    let mut tree = Tree::new(&view);
    let size = Size::new(740., 650.);
    let node = view
        .as_widget_mut()
        .layout(&mut tree, &renderer, &layout::Limits::new(Size::ZERO, size));
    let mut messages = Vec::new();
    {
        let viewport = Rectangle::with_size(size);
        let mut overlay = view
            .as_widget_mut()
            .overlay(&mut tree, Layout::new(&node), &renderer, &viewport, Vector::ZERO)
            .expect("compact navigation overlay");
        let overlay = overlay.as_overlay_mut();
        let layout = overlay.layout(&renderer, size);
        let mut bounds = Bounds::default();
        overlay.operate(Layout::new(&layout), &renderer, &mut bounds);
        assert_eq!(bounds.sidebar.unwrap().width, 244.);
        assert!(
            bounds.page.is_none(),
            "background must be excluded from modal focus navigation"
        );
        overlay.update(
            &Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
            Layout::new(&layout),
            mouse::Cursor::Available(Point::new(500., 300.)),
            &renderer,
            &mut clipboard::Null,
            &mut Shell::new(&mut messages),
        );
        assert!(
            messages
                .iter()
                .any(|message| matches!(message, Message::Sidebar(false)))
        );
    }
    drop(view);
    for message in messages {
        let _ = app.update(message);
    }
    assert!(!app.sidebar_open);
    let _ = app.update(Message::Sidebar(true));
    let _ = app.update(Message::Page(Page::Bar));
    assert!(!app.sidebar_open, "reselecting the current page closes navigation too");
    assert!(app.pending.is_empty() && !app.saving);
}
