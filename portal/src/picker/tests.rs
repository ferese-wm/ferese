use super::{Message, Prompt, Selection, SourceGroup, SourcePicker};
use crate::capture::Source;
use cosmic::Application;
use cosmic::app::Core;

fn picker(prompt: Prompt) -> SourcePicker {
    let snapshot = ferese_theme_client::service::fallback();
    let palette = ferese_theme::Palette::from_resolved(&snapshot.presented).flat();
    SourcePicker::init(
        Core::default(),
        (
            prompt,
            palette.sidebar,
            palette,
            cosmic::iced::Font::default(),
        ),
    )
    .0
}

fn prompt() -> Prompt {
    serde_json::from_value(serde_json::json!({
        "app": "test", "multiple": true, "workspace_shell": true, "workspace_quality": true,
        "sources": [
            {"name":"workspace:1", "label":"Workspace 1", "width":800, "height":600, "x":0, "y":0, "scale":1},
            {"name":"DP-1", "label":"Display", "width":800, "height":600, "x":0, "y":0, "scale":1},
            {"name":"window:2", "label":"Window", "width":80, "height":60, "x":0, "y":0, "scale":1}
        ]
    })).expect("picker request fixture deserializes")
}

#[test]
fn source_options_replace_the_picker_page_without_resizing_or_losing_selection() {
    use cosmic::iced::advanced::renderer::Headless;
    use cosmic::iced::advanced::{Layout, Shell, clipboard, layout, mouse, widget};
    use cosmic::iced::{Event, Font, Pixels, Point, Rectangle, Size};

    let runtime = tokio::runtime::Runtime::new().unwrap();
    let renderer = runtime
        .block_on(<cosmic::Renderer as Headless>::new(
            Font::default(),
            Pixels(14.),
            Some("tiny-skia"),
        ))
        .unwrap();
    let mut prompt = prompt();
    prompt
        .sources
        .retain(|source| source.workspace_id().is_some());
    let mut picker = picker(prompt);
    drop(picker.update(Message::Select(0)));
    let size = {
        let mut view = picker.view();
        let mut tree = widget::Tree::new(&view);
        view.as_widget_mut()
            .layout(
                &mut tree,
                &renderer,
                &layout::Limits::new(Size::ZERO, Size::new(560., 600.)),
            )
            .size()
    };
    let size = Size::new(size.width.ceil(), size.height.ceil());
    assert!(
        size.height < 400.,
        "a single source should have a compact picker"
    );
    drop(picker.update(Message::ContentSize(Some(size))));
    let limited = picker.content_limited;
    drop(picker.update(Message::OpenOptions(0)));
    drop(picker.update(Message::IncludeShell));
    let bounds = Rectangle::with_size(size);
    let mut messages = Vec::new();
    {
        let mut view = picker.view();
        let mut tree = widget::Tree::new(&view);
        let node = view.as_widget_mut().layout(
            &mut tree,
            &renderer,
            &layout::Limits::new(Size::ZERO, size),
        );
        assert!(
            view.as_widget_mut()
                .overlay(
                    &mut tree,
                    Layout::new(&node),
                    &renderer,
                    &bounds,
                    cosmic::iced::Vector::ZERO,
                )
                .is_none(),
            "source settings must be a page, not a modal inside the picker"
        );
        assert_eq!(
            node.size(),
            size,
            "opening options must use the configured window size"
        );
        let content = Layout::new(&node).children().next().unwrap();
        let body = content.children().nth(1).unwrap();
        assert!(
            body.children().next().unwrap().bounds().height <= body.bounds().height + 0.5,
            "both workspace settings and their descriptions must fit without scrolling"
        );
        let footer = content.children().last().unwrap();
        let done = footer.children().last().unwrap().bounds();
        assert!(done.y >= 0. && done.y + done.height <= size.height);
        let a11y =
            view.as_widget()
                .a11y_nodes(Layout::new(&node), &tree, mouse::Cursor::Unavailable);
        let switch = a11y
            .root()
            .iter()
            .chain(a11y.children())
            .find(|node| node.node().label() == Some("Include shell"))
            .expect("workspace options expose a named shell switch")
            .node()
            .bounds()
            .unwrap();
        assert!(
            switch.y1 <= f64::from(footer.bounds().y),
            "both workspace settings must fit in a short picker"
        );
        let point = Point::new(done.center_x(), done.center_y());
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
    }
    drop(picker.update(Message::WorkspaceQuality(crate::workspace::Quality::High)));
    assert!(
        messages
            .iter()
            .any(|message| matches!(message, Message::CloseOptions))
    );
    drop(picker.update(Message::ContentSize(Some(Size::new(560., 500.)))));
    assert_eq!(
        picker.last_size,
        Some(size),
        "stale source measurements cannot resize options"
    );
    for message in messages {
        drop(picker.update(message));
    }
    assert!(picker.options_source.is_none());
    assert_eq!(picker.selected, [0]);
    assert_eq!(picker.workspace_quality, crate::workspace::Quality::High);
    assert!(picker.include_shell);
    assert_eq!(
        picker.content_limited, limited,
        "returning must preserve the source list's layout"
    );
}

#[test]
fn shell_requires_a_selected_workspace_and_compositor_support() {
    let mut prompt = prompt();
    assert!(!prompt.allows_shell([]));
    assert!(!prompt.allows_shell(["DP-1", "window:2"]));
    assert!(!prompt.allows_shell(["workspace:99"]));
    assert!(prompt.allows_shell(["workspace:1"]));
    assert!(prompt.allows_shell(["workspace:1", "DP-1"]));
    prompt.workspace_shell = false;
    assert!(!prompt.allows_shell(["workspace:1"]));
    prompt.workspace_shell = true;
    prompt.recording = true;
    assert!(!prompt.allows_shell(["workspace:1"]));
    prompt.recording = false;
    prompt.window_capture = true;
    assert!(!prompt.allows_shell(["workspace:1"]));
}

#[test]
fn older_picker_messages_do_not_enable_shell_capture() {
    let selection: Selection =
        serde_json::from_str(r#"{"names":["workspace:1"],"persist_mode":0}"#)
            .expect("legacy selection fixture deserializes");
    assert!(!selection.include_shell);
    assert_eq!(
        selection.workspace_quality,
        crate::workspace::Quality::Balanced
    );
    let mut value = serde_json::to_value(prompt()).expect("prompt serializes to JSON");
    value
        .as_object_mut()
        .expect("serialized prompt is a JSON object")
        .remove("workspace_shell");
    let prompt: Prompt =
        serde_json::from_value(value).expect("legacy request fixture deserializes");
    assert!(!prompt.allows_shell(["workspace:1"]));
}

#[test]
fn quality_requires_workspace_selection_and_protocol_support() {
    let mut prompt = prompt();
    assert!(!prompt.allows_quality([]));
    assert!(!prompt.allows_quality(["DP-1", "window:2", "workspace:99"]));
    assert!(prompt.allows_quality(["workspace:1", "DP-1"]));
    prompt.workspace_quality = false;
    assert!(!prompt.allows_quality(["workspace:1"]));
    prompt.workspace_quality = true;
    prompt.recording = true;
    assert!(!prompt.allows_quality(["workspace:1"]));
    prompt.recording = false;
    prompt.window_capture = true;
    assert!(!prompt.allows_quality(["workspace:1"]));
    let mut value = serde_json::to_value(prompt).expect("prompt serializes to JSON");
    value
        .as_object_mut()
        .expect("serialized prompt is a JSON object")
        .remove("workspace_quality");
    let older: Prompt = serde_json::from_value(value).expect("legacy request fixture deserializes");
    assert!(!older.workspace_quality);
    assert!(
        serde_json::from_str::<Selection>(
            r#"{"names":["workspace:1"],"persist_mode":0,"workspace_quality":"unknown"}"#
        )
        .is_err()
    );
}

#[test]
fn changing_single_source_tabs_clears_consent_and_ignores_stale_rows() {
    let mut prompt = prompt();
    prompt.multiple = false;
    let mut picker = picker(prompt);
    assert!(picker.selected.is_empty());
    assert!(picker.options_source.is_none());
    assert_eq!(picker.workspace_quality, crate::workspace::Quality::Native);
    drop(picker.update(Message::Group(SourceGroup::Workspaces)));
    drop(picker.update(Message::Select(0)));
    drop(picker.update(Message::OpenOptions(0)));
    drop(picker.update(Message::IncludeShell));
    assert!(picker.include_shell);
    assert!(picker.options_source.is_some());
    drop(picker.update(Message::Group(SourceGroup::Windows)));
    assert_eq!(
        picker.group,
        SourceGroup::Workspaces,
        "the options page ignores source-tab clicks"
    );
    drop(picker.update(Message::Escape));
    assert_eq!(picker.selected, [0]);
    drop(picker.update(Message::Group(SourceGroup::Windows)));
    assert!(picker.selected.is_empty());
    assert!(!picker.include_shell);
    assert!(picker.options_source.is_none());
    drop(picker.update(Message::Select(0)));
    assert!(
        picker.selected.is_empty(),
        "an old tab's click must not select a hidden source"
    );
    drop(picker.update(Message::Select(2)));
    assert_eq!(picker.selected, [2]);
    assert!(!picker.source_has_options(2));
}

#[test]
fn mixed_selections_are_visible_in_tabs_and_options_follow_actual_consent() {
    let mut prompt = prompt();
    prompt.persist_mode = 2;
    prompt.rememberable = vec!["DP-1".into()];
    let mut picker = picker(prompt);
    drop(picker.update(Message::Select(1)));
    drop(picker.update(Message::OpenOptions(1)));
    drop(picker.update(Message::Remember));
    assert!(picker.remember);
    drop(picker.update(Message::CloseOptions));
    drop(picker.update(Message::Group(SourceGroup::Workspaces)));
    drop(picker.update(Message::Select(0)));
    drop(picker.update(Message::OpenOptions(0)));
    drop(picker.update(Message::IncludeShell));
    drop(picker.update(Message::CloseOptions));
    assert_eq!(picker.selected, [1, 0]);
    assert!(!picker.remember, "workspace consent cannot be remembered");
    assert!(picker.include_shell);
    drop(picker.update(Message::Group(SourceGroup::Windows)));
    drop(picker.update(Message::Search("not a window".into())));
    assert_eq!(picker.visible_sources().count(), 0);
    assert_eq!(picker.selected, [1, 0]);
    drop(picker.update(Message::Group(SourceGroup::Workspaces)));
    assert!(picker.query.is_empty());
    drop(picker.update(Message::Select(0)));
    assert!(!picker.can_include_shell());
    assert!(!picker.include_shell);
    assert_eq!(picker.selected, [1]);
}

#[test]
fn search_keeps_source_indices_and_cannot_bypass_the_selection_limit() {
    let mut prompt = prompt();
    prompt.sources.extend((3..16).map(|id| Source {
        name: format!("window:{id}"),
        label: format!("Editor {id}"),
        width: 800,
        height: 600,
        x: 0,
        y: 0,
        scale: 1,
    }));
    let mut picker = picker(prompt);
    drop(picker.update(Message::Group(SourceGroup::Windows)));
    drop(picker.update(Message::Search(" EDITOR 12 ".into())));
    let index = picker
        .visible_sources()
        .next()
        .expect("search finds the requested window")
        .0;
    assert_eq!(picker.prompt.sources[index].name, "window:12");
    drop(picker.update(Message::Select(index)));
    drop(picker.update(Message::Search(String::new())));
    let indices: Vec<_> = picker.visible_sources().map(|(index, _)| index).collect();
    for index in indices {
        if !picker.selected.contains(&index) {
            drop(picker.update(Message::Select(index)));
        }
    }
    assert_eq!(picker.selected.len(), 8);
    picker.prompt.persist_mode = 0;
    assert!(!picker.can_remember());
    assert!(picker.prompt.dialog_height() <= 600.);
}

#[test]
fn actions_stay_visible_with_many_sources_and_expanded_options() {
    use cosmic::iced::advanced::layout;
    use cosmic::iced::advanced::renderer::Headless;
    use cosmic::iced::advanced::widget::Tree;
    use cosmic::iced::{Font, Pixels, Size};
    let runtime = tokio::runtime::Runtime::new().expect("test runtime starts");
    let renderer = runtime
        .block_on(<cosmic::Renderer as Headless>::new(
            Font::default(),
            Pixels(14.),
            Some("tiny-skia"),
        ))
        .expect("headless renderer is available");
    let mut prompt = prompt();
    prompt.sources[0].label = "A long workspace title that should fit on one row without moving the dialog actions out of view".into();
    prompt.sources.extend((3..200).map(|id| Source {
        name: format!("workspace:{id}"),
        label: format!("Workspace {id}"),
        width: 1920,
        height: 1080,
        x: 0,
        y: 0,
        scale: 1,
    }));
    let mut picker = picker(prompt);
    drop(picker.update(Message::Group(SourceGroup::Workspaces)));
    drop(picker.update(Message::Select(0)));
    for expanded in [false, true] {
        picker.options_source = expanded.then_some(0);
        for (width, height) in [(560., 600.), (400., 460.), (360., 400.)] {
            picker.max_height = height;
            picker.content_limited = true;
            let mut view = picker.view();
            let mut tree = Tree::new(view.as_widget());
            let node = view.as_widget_mut().layout(
                &mut tree,
                &renderer,
                &layout::Limits::new(Size::ZERO, Size::new(width, height)),
            );
            assert!(
                node.size().height <= height + 0.5,
                "picker exceeds the available height"
            );
            let content = &node.children()[0];
            let footer = content.children().last().expect("picker has a footer");
            assert!(
                footer.bounds().y + footer.size().height <= content.size().height + 0.5,
                "actions overflow at {width}×{height}, expanded={expanded}"
            );
            assert_eq!(footer.children().len(), if expanded { 2 } else { 3 });
            for action in &footer.children()[1..] {
                assert!(action.size().width >= 48.);
                assert!(
                    action.size().width <= 160.,
                    "dialog actions should remain compact"
                );
                assert!(action.size().height >= 24.);
            }
            if expanded {
                let bounds = cosmic::iced::Rectangle::with_size(Size::new(width, height));
                assert!(
                    view.as_widget_mut()
                        .overlay(
                            &mut tree,
                            cosmic::iced::advanced::Layout::new(&node),
                            &renderer,
                            &bounds,
                            cosmic::iced::Vector::ZERO,
                        )
                        .is_none(),
                    "options must stay in the picker page"
                );
                let body = &content.children()[1];
                assert!(body.bounds().y >= 0.);
                assert!(body.bounds().y + body.size().height <= footer.bounds().y);
            }
        }
    }
}

#[test]
fn short_lists_and_search_results_fit_content_and_keep_actions_adjacent() {
    use cosmic::iced::advanced::layout;
    use cosmic::iced::advanced::renderer::Headless;
    use cosmic::iced::advanced::widget::Tree;
    use cosmic::iced::{Font, Pixels, Size};

    fn settle(picker: &mut SourcePicker, renderer: &cosmic::Renderer) -> layout::Node {
        for _ in 0..3 {
            let node = {
                let mut view = picker.view();
                let mut tree = Tree::new(view.as_widget());
                view.as_widget_mut().layout(
                    &mut tree,
                    renderer,
                    &layout::Limits::new(Size::ZERO, Size::new(560., picker.max_height)),
                )
            };
            let was_limited = picker.content_limited;
            drop(picker.update(Message::ContentSize(Some(node.size()))));
            if picker.content_limited == was_limited {
                let fields = &node.children()[0];
                let footer = fields.children().last().expect("picker has actions");
                let sources = &fields.children()[fields.children().len() - 2];
                let gap = footer.bounds().y - sources.bounds().y - sources.size().height;
                assert!(
                    (gap - 18.).abs() < 0.5,
                    "actions have an unnecessary {gap}px gap"
                );
                assert!(node.size().height <= picker.max_height + 0.5);
                return node;
            }
        }
        panic!("picker layout must settle without a resize loop")
    }

    let runtime = tokio::runtime::Runtime::new().expect("test runtime starts");
    let renderer = runtime
        .block_on(<cosmic::Renderer as Headless>::new(
            Font::default(),
            Pixels(14.),
            Some("tiny-skia"),
        ))
        .expect("headless renderer is available");
    let mut prompt = prompt();
    prompt.window_capture = true;
    prompt
        .sources
        .retain(|source| SourceGroup::Windows.contains(source));
    prompt.sources[0].label = "Editor unique match".into();
    let mut picker = picker(prompt);
    picker.max_height = 400.;
    let one = settle(&mut picker, &renderer).size().height;
    assert!(
        one < 300.,
        "one window should not need a tall dialog: {one}"
    );
    assert!(!picker.content_limited);

    picker.prompt.sources.extend((3..18).map(|id| Source {
        name: format!("window:{id}"),
        label: format!("Other window {id}"),
        width: 1280,
        height: 800,
        x: 0,
        y: 0,
        scale: 1,
    }));
    drop(picker.update(Message::Measure));
    assert_eq!(settle(&mut picker, &renderer).size().height, 400.);
    assert!(
        picker.content_limited,
        "long lists must scroll within the available area"
    );

    drop(picker.update(Message::Search("unique match".into())));
    let filtered = settle(&mut picker, &renderer).size().height;
    assert!(
        filtered < 400.,
        "search results must shrink the picker again"
    );
    assert!(!picker.content_limited);
    drop(picker.update(Message::Search("no matching window".into())));
    assert!((settle(&mut picker, &renderer).size().height - filtered).abs() < 1.);
}

#[test]
fn options_page_leaves_native_material_visible_without_a_second_background() {
    use cosmic::iced::advanced::renderer::{self, Headless, Renderer as _};
    use cosmic::iced::advanced::widget::Tree;
    use cosmic::iced::advanced::{Layout, layout, mouse};
    use cosmic::iced::{Color, Font, Pixels, Rectangle, Size};
    let runtime = tokio::runtime::Runtime::new().expect("test runtime starts");
    let mut renderer = runtime
        .block_on(<cosmic::Renderer as Headless>::new(
            Font::default(),
            Pixels(14.),
            Some("tiny-skia"),
        ))
        .expect("headless renderer is available");
    let mut picker = picker(prompt());
    picker.background.a = 0.71;
    drop(picker.update(Message::Group(SourceGroup::Workspaces)));
    drop(picker.update(Message::Select(0)));
    drop(picker.update(Message::OpenOptions(0)));
    let size = Size::new(560, 600);
    let bounds = Rectangle::with_size(Size::new(560., 600.));
    let theme = picker.palette.native_theme();
    let style = renderer::Style {
        text_color: picker.palette.text,
        icon_color: picker.palette.text,
        scale_factor: 1.,
    };
    let mut view = picker.view();
    let mut tree = Tree::new(view.as_widget());
    let node = view.as_widget_mut().layout(
        &mut tree,
        &renderer,
        &layout::Limits::new(Size::ZERO, bounds.size()),
    );
    renderer.reset(bounds);
    view.as_widget().draw(
        &tree,
        &mut renderer,
        &theme,
        &style,
        Layout::new(&node),
        mouse::Cursor::Unavailable,
        &bounds,
    );
    assert!(
        view.as_widget_mut()
            .overlay(
                &mut tree,
                Layout::new(&node),
                &renderer,
                &bounds,
                cosmic::iced::Vector::ZERO,
            )
            .is_none()
    );
    let pixels = Headless::screenshot(&mut renderer, size, 1., Color::TRANSPARENT);
    let alpha = |x: f32, y: f32| pixels[((y as usize * size.width as usize + x as usize) * 4) + 3];
    // The page must leave Ferese's surface material visible in its padding.
    // There is no source list, scrim, or nested material behind the controls.
    for y in 0..size.height {
        assert_eq!(alpha(8., y as f32), 0, "material covered at row {y}");
    }
    assert!(
        pixels.chunks_exact(4).any(|pixel| pixel[3] > 0),
        "options must still draw their text and controls"
    );
}

#[test]
#[ignore = "exports native picker previews for visual review"]
fn render_picker_previews() {
    use cosmic::iced::advanced::renderer::{self, Headless, Renderer as _};
    use cosmic::iced::advanced::widget::Tree;
    use cosmic::iced::advanced::{Layout, layout, mouse};
    use cosmic::iced::{Font, Pixels, Rectangle, Size};
    let runtime = tokio::runtime::Runtime::new().expect("test runtime starts");
    let mut renderer = runtime
        .block_on(<cosmic::Renderer as Headless>::new(
            Font::default(),
            Pixels(14.),
            Some("tiny-skia"),
        ))
        .expect("headless renderer is available");
    let output = std::path::PathBuf::from(
        std::env::var("FERESE_PICKER_PREVIEW_DIR")
            .unwrap_or_else(|_| "/tmp/ferese-picker-previews".into()),
    );
    std::fs::create_dir_all(&output).expect("preview directory is writable");
    for appearance in [
        ferese_config::theme::Appearance::Light,
        ferese_config::theme::Appearance::Dark,
    ] {
        for case in ["displays", "workspace", "options", "recording", "compact"] {
            let mut prompt = prompt();
            prompt.app = "OBS Studio".into();
            prompt.sources.push(Source {
                name: "HDMI-A-1".into(),
                label: "Samsung Odyssey G80SD".into(),
                width: 3840,
                height: 2160,
                x: 0,
                y: 0,
                scale: 1,
            });
            prompt.sources[1].label = "Built-in display".into();
            for id in 3..9 {
                prompt.sources.push(Source {
                    name: format!("workspace:{id}"),
                    label: format!("Workspace {id} · HDMI-A-1"),
                    width: 2400,
                    height: 1350,
                    x: 0,
                    y: 0,
                    scale: 1,
                });
            }
            prompt.recording = case == "recording";
            let mut picker = picker(prompt);
            let resolved = ferese_config::theme::resolve(
                &ferese_config::Document::parse(
                    if appearance == ferese_config::theme::Appearance::Light {
                        "theme { mode \"light\"; }"
                    } else {
                        "theme { mode \"dark\"; }"
                    },
                )
                .expect("preview configuration parses"),
                std::path::Path::new("/tmp"),
                jiff::Timestamp::now(),
                |_| Err("preview has no theme imports".into()),
            )
            .expect("preview theme resolves")
            .theme;
            picker.palette = ferese_theme::Palette::from_resolved(&resolved).flat();
            picker.background = picker.palette.sidebar;
            if case == "workspace" || case == "options" || case == "compact" {
                drop(picker.update(Message::Group(SourceGroup::Workspaces)));
                drop(picker.update(Message::Select(0)));
            } else {
                drop(picker.update(Message::Select(1)));
            }
            picker.options_source = matches!(case, "options" | "recording" | "compact")
                .then_some(if case == "recording" { 1 } else { 0 });
            let size = if case == "compact" {
                Size::new(360, 400)
            } else {
                Size::new(560, 580)
            };
            let bounds = Rectangle::with_size(Size::new(size.width as f32, size.height as f32));
            let theme = picker.palette.native_theme();
            let mut view = picker.view();
            let mut tree = Tree::new(view.as_widget());
            let node = view.as_widget_mut().layout(
                &mut tree,
                &renderer,
                &layout::Limits::new(Size::ZERO, bounds.size()),
            );
            renderer.reset(bounds);
            view.as_widget().draw(
                &tree,
                &mut renderer,
                &theme,
                &renderer::Style {
                    text_color: picker.palette.text,
                    icon_color: picker.palette.text,
                    scale_factor: 1.,
                },
                Layout::new(&node),
                mouse::Cursor::Unavailable,
                &bounds,
            );
            if let Some(mut overlay) = view.as_widget_mut().overlay(
                &mut tree,
                Layout::new(&node),
                &renderer,
                &bounds,
                cosmic::iced::Vector::ZERO,
            ) {
                let popup = overlay.as_overlay_mut().layout(&renderer, bounds.size());
                renderer.with_layer(popup.bounds(), |renderer| {
                    overlay.as_overlay().draw(
                        renderer,
                        &theme,
                        &renderer::Style {
                            text_color: picker.palette.text,
                            icon_color: picker.palette.text,
                            scale_factor: 1.,
                        },
                        Layout::new(&popup),
                        mouse::Cursor::Unavailable,
                    );
                });
            }
            let pixels = Headless::screenshot(&mut renderer, size, 1., picker.palette.sidebar);
            let path = output.join(format!("picker-{appearance:?}-{case}.png"));
            image::save_buffer(
                path,
                &pixels,
                size.width,
                size.height,
                image::ColorType::Rgba8,
            )
            .expect("preview image writes successfully");
        }
    }
}

#[test]
fn selection_and_configure_events_keep_the_fitted_layout() {
    use cosmic::iced::advanced::layout;
    use cosmic::iced::advanced::renderer::Headless;
    use cosmic::iced::advanced::widget::Tree;
    use cosmic::iced::{Font, Pixels, Size};

    let runtime = tokio::runtime::Runtime::new().expect("test runtime starts");
    let renderer = runtime
        .block_on(<cosmic::Renderer as Headless>::new(
            Font::default(),
            Pixels(14.),
            Some("tiny-skia"),
        ))
        .expect("headless renderer is available");
    let mut prompt = prompt();
    prompt.sources.extend((3..30).map(|id| Source {
        name: format!("workspace:{id}"),
        label: format!("Workspace {id}"),
        width: 1920,
        height: 1080,
        x: 0,
        y: 0,
        scale: 1,
    }));
    let mut picker = picker(prompt);
    drop(picker.update(Message::Group(SourceGroup::Workspaces)));
    picker.max_height = 400.;
    let mut tree = Tree::new(picker.view().as_widget());
    let layout = |picker: &SourcePicker, tree: &mut Tree| {
        let mut view = picker.view();
        view.as_widget_mut().diff(tree);
        view.as_widget_mut().layout(
            tree,
            &renderer,
            &layout::Limits::new(Size::ZERO, Size::new(560., 400.)),
        )
    };
    let mut node = layout(&picker, &mut tree);
    for _ in 0..3 {
        drop(picker.update(Message::ContentSize(Some(node.size()))));
        node = layout(&picker, &mut tree);
    }
    assert!(node.size().height <= 400.5);
    let footer = |node: &layout::Node| node.children()[0].children().last().unwrap().bounds();
    let original = footer(&node);
    for message in [
        Message::Select(0),
        Message::Measure,
        Message::MonitorSize(Some(Size::new(800., 424.))),
        Message::Select(0),
        Message::Group(SourceGroup::Workspaces),
        Message::Search(String::new()),
    ] {
        drop(picker.update(message));
        node = layout(&picker, &mut tree);
        assert!(
            node.size().height <= 400.5,
            "interaction must not draw an unrestricted-height frame"
        );
        assert_eq!(
            footer(&node),
            original,
            "source clicks must not move the Share button"
        );
    }
}

#[test]
fn native_popups_do_not_replace_the_dialog_attachment_or_layout() {
    let mut picker = picker(prompt());
    picker.content_limited = true;
    let popup = cosmic::iced::window::Id::unique();
    assert_ne!(Some(popup), picker.core.main_window_id());
    let task = picker.update(Message::WindowOpened(popup));
    assert_eq!(
        task.units(),
        0,
        "a popup must not get parent and material attachments meant for the main window"
    );
    assert!(picker.content_limited);
    assert!(picker.material.is_none());
}
