use super::*;

fn shell() -> FereseShell {
    let mut shell = crate::tests::shell_with_measured_panel();
    let _ = shell.toggle_launcher("test".into());
    shell
}
fn row(title: &str) -> Row {
    Row {
        id: title.into(),
        title: title.into(),
        icon: None,
        subtitle: "Action".into(),
        target: Target::Action(sources::Action::Lock),
        score: 1.,
    }
}
#[test]
fn escape_pops_one_layer_in_every_category() {
    let mut shell = shell();
    let launcher = shell.launcher.as_mut().unwrap();
    for category in [
        None,
        Some(Category::Apps),
        Some(Category::Files),
        Some(Category::Windows),
    ] {
        launcher.category = category;
        launcher.query = "term".into();
        launcher.alternatives = true;
        assert!(!launcher.escape());
        assert!(launcher.query.is_empty());
        assert!(!launcher.alternatives);
        assert_eq!(launcher.category, category);
        if category.is_some() {
            assert!(!launcher.escape());
            assert_eq!(launcher.category, None);
        }
        assert!(launcher.escape());
    }
}
#[test]
fn typing_never_opens_alternates_and_empty_home_has_pills() {
    let mut shell = shell();
    let _ = shell.update_launcher(Message::Input(
        shell.launcher.as_ref().unwrap().id,
        "fire".into(),
    ));
    let launcher = shell.launcher.as_mut().unwrap();
    assert!(!launcher.alternatives);
    assert_eq!(launcher.target_height(), 72.);
    launcher.rows = vec![row("Firefox"), row("Files")];
    launcher.alternatives = true;
    assert_eq!(launcher.target_height(), 208.);
    assert!(!launcher.escape());
    assert_eq!(launcher.target_height(), 72.);
}
#[test]
fn late_results_cannot_replace_a_new_query_or_invocation() {
    let mut shell = shell();
    let id = shell.launcher.as_ref().unwrap().id;
    let _ = shell.update_launcher(Message::Input(id, "new".into()));
    let _ = shell.update_launcher(Message::Results(
        id,
        0,
        vec![row("old")],
        files::Status::Ready,
    ));
    assert!(shell.launcher.as_ref().unwrap().rows.is_empty());
    let _ = shell.destroy_launcher();
    let _ = shell.update_launcher(Message::Results(
        id,
        1,
        vec![row("old")],
        files::Status::Ready,
    ));
    assert!(shell.launcher.is_none());
    let _ = shell.toggle_launcher("test".into());
    let _ = shell.update_launcher(Message::Results(
        id,
        0,
        vec![row("old")],
        files::Status::Ready,
    ));
    assert!(shell.launcher.as_ref().unwrap().rows.is_empty());
}
#[test]
fn selection_survives_result_reordering() {
    let mut shell = shell();
    let launcher = shell.launcher.as_mut().unwrap();
    launcher.rows = vec![row("one"), row("two")];
    launcher.selected = 1;
    let (id, revision) = (launcher.id, launcher.revision);
    let _ = shell.update_launcher(Message::Results(
        id,
        revision,
        vec![row("two"), row("one")],
        files::Status::Ready,
    ));
    assert_eq!(shell.launcher.as_ref().unwrap().selected, 0);
}
#[test]
fn destructive_actions_require_alt() {
    for action in [
        sources::Action::Poweroff,
        sources::Action::Reboot,
        sources::Action::Logout,
    ] {
        let mut shell = shell();
        let mut result = row("dangerous");
        result.target = Target::Action(action);
        shell.launcher.as_mut().unwrap().rows = vec![result];
        let _ = shell.activate_launcher(false);
        let launcher = shell.launcher.as_ref().unwrap();
        assert!(!launcher.closing);
        assert!(!launcher.activating);
        assert_eq!(launcher.error.as_deref(), Some("Use Alt+Enter to confirm"));
        assert!(shell.system_modal.is_none());
    }
}
#[test]
fn rank_is_deterministic_and_deduplicates_ids() {
    let mut rows = vec![row("B"), row("A"), row("B")];
    sources::rank(&mut rows);
    assert_eq!(
        rows.iter().map(|r| r.title.as_str()).collect::<Vec<_>>(),
        vec!["A", "B"]
    );
    assert!(sources::match_score("é", "Éditeur").is_some());
    assert!(sources::match_score("nope", "Terminal").is_none());
}
#[test]
fn close_releases_catalog_and_restores_keyboard_navigation() {
    let mut shell = shell();
    let apps = Arc::new(apps::Apps::empty());
    let weak = Arc::downgrade(&apps);
    shell.launcher.as_mut().unwrap().apps = Some(apps);
    shell.launcher.as_mut().unwrap().keyboard_nav = true;
    let _ = shell.destroy_launcher();
    assert!(weak.upgrade().is_none());
    assert!(shell.core.keyboard_nav());
    assert!(shell.launcher.is_none());
}

#[test]
fn queries_are_coalesced_and_only_the_latest_result_is_applied() {
    let mut shell = shell();
    let launcher = shell.launcher.as_mut().unwrap();
    launcher.apps = Some(Arc::new(apps::Apps::empty()));
    launcher.running = true;
    let id = launcher.id;
    let _ = shell.update_launcher(Message::Input(id, "first".into()));
    let _ = shell.update_launcher(Message::Input(id, "second".into()));
    let revision = shell.launcher.as_ref().unwrap().revision;
    let _ = shell.update_launcher(Message::Results(
        id,
        0,
        vec![row("stale")],
        files::Status::Ready,
    ));
    assert!(shell.launcher.as_ref().unwrap().running);
    assert!(shell.launcher.as_ref().unwrap().rows.is_empty());
    let _ = shell.update_launcher(Message::Results(
        id,
        revision,
        vec![row("current")],
        files::Status::Ready,
    ));
    assert!(!shell.launcher.as_ref().unwrap().running);
    assert_eq!(shell.launcher.as_ref().unwrap().rows[0].title, "current");
}

#[test]
fn queued_widget_messages_cannot_change_a_reopened_launcher() {
    let mut shell = shell();
    let old = shell.launcher.as_ref().unwrap().id;
    let _ = shell.destroy_launcher();
    let _ = shell.toggle_launcher("test".into());
    let _ = shell.update_launcher(Message::Input(old, "stale paste".into()));
    let _ = shell.update_launcher(Message::Browse(old, Category::Files));
    let launcher = shell.launcher.as_ref().unwrap();
    assert!(launcher.query.is_empty());
    assert_eq!(launcher.category, None);
}

#[test]
fn clicked_rows_keep_their_identity_after_a_result_refresh() {
    let mut shell = shell();
    let launcher = shell.launcher.as_mut().unwrap();
    let id = launcher.id;
    let mut power = row("power");
    power.target = Target::Action(sources::Action::Poweroff);
    let mut window = row("window");
    window.target = Target::Window(99);
    launcher.rows = vec![window, power];
    let _ = shell.update_launcher(Message::ActivateRow(id, "power".into()));
    let launcher = shell.launcher.as_ref().unwrap();
    assert_eq!(launcher.selected, 1);
    assert!(!launcher.closing);
    assert_eq!(launcher.error.as_deref(), Some("Use Alt+Enter to confirm"));
    let _ = shell.update_launcher(Message::ActivateRow(id, "vanished".into()));
    assert_eq!(shell.launcher.as_ref().unwrap().selected, 1);
}

#[test]
fn pending_launch_freezes_search_and_preserves_failure_context() {
    let mut shell = shell();
    let launcher = shell.launcher.as_mut().unwrap();
    launcher.query = "original".into();
    launcher.activating = true;
    let id = launcher.id;
    let _ = shell.update_launcher(Message::Input(id, "other".into()));
    let _ = shell.update_launcher(Message::Browse(id, Category::Files));
    let launcher = shell.launcher.as_ref().unwrap();
    assert_eq!(launcher.query, "original");
    assert_eq!(launcher.category, None);
    assert_eq!(launcher.status_message(), Some("Opening…"));
    let _ = shell.update_launcher(Message::Launched(
        id,
        "Original app".into(),
        Err("not found".into()),
    ));
    let launcher = shell.launcher.as_ref().unwrap();
    assert!(!launcher.activating && !launcher.closing);
    assert_eq!(
        launcher.error.as_deref(),
        Some("Could not open Original app: not found")
    );
    let _ = shell.update_launcher(Message::Input(id, "next".into()));
    assert_eq!(shell.launcher.as_ref().unwrap().query, "next");
}

#[test]
fn old_launch_success_cannot_close_a_reopened_launcher() {
    let mut shell = shell();
    let id = shell.launcher.as_ref().unwrap().id;
    let _ = shell.destroy_launcher();
    let _ = shell.toggle_launcher("test".into());
    let _ = shell.update_launcher(Message::Launched(id, "old app".into(), Ok(())));
    assert!(!shell.launcher.as_ref().unwrap().closing);
}

#[test]
fn pending_search_is_visible_and_never_accepts_a_future_query() {
    let mut shell = shell();
    let launcher = shell.launcher.as_mut().unwrap();
    launcher.query = "first".into();
    launcher.apps = Some(Arc::new(apps::Apps::empty()));
    launcher.running = true;
    let id = launcher.id;
    let _ = shell.update_launcher(Message::Accept(false));
    assert_eq!(
        shell.launcher.as_ref().unwrap().status_message(),
        Some("Searching…")
    );
    let _ = shell.update_launcher(Message::Input(id, "second".into()));
    let revision = shell.launcher.as_ref().unwrap().revision;
    let _ = shell.update_launcher(Message::Results(id, revision, vec![], files::Status::Ready));
    assert_eq!(
        shell.launcher.as_ref().unwrap().status_message(),
        Some("No matches")
    );
    assert!(!shell.launcher.as_ref().unwrap().activating);
    let _ = shell.update_launcher(Message::Results(
        id,
        revision,
        vec![],
        files::Status::Unavailable("File index unavailable".into()),
    ));
    assert_eq!(
        shell.launcher.as_ref().unwrap().status_message(),
        Some("File index unavailable")
    );
}

#[test]
fn vanished_selection_requires_a_new_choice() {
    let mut shell = shell();
    let launcher = shell.launcher.as_mut().unwrap();
    launcher.rows = vec![row("first"), row("second")];
    launcher.selected = 1;
    let (id, revision) = (launcher.id, launcher.revision);
    for _ in 0..2 {
        let _ = shell.update_launcher(Message::Results(
            id,
            revision,
            vec![row("first")],
            files::Status::Ready,
        ));
        let _ = shell.update_launcher(Message::Accept(false));
        let launcher = shell.launcher.as_ref().unwrap();
        assert!(launcher.selected_row().is_none());
        assert!(!launcher.activating);
    }
    // An explicit click restores selection without launching a dangerous action.
    let mut chosen = row("power");
    chosen.target = Target::Action(sources::Action::Poweroff);
    shell.launcher.as_mut().unwrap().rows = vec![chosen];
    let _ = shell.update_launcher(Message::ActivateRow(id, "power".into()));
    assert!(!shell.launcher.as_ref().unwrap().selection_missing);
    assert_eq!(
        shell
            .launcher
            .as_ref()
            .unwrap()
            .selected_row()
            .unwrap()
            .title,
        "power"
    );
}

#[test]
fn grid_capacity_and_navigation_follow_the_allocated_viewport() {
    use cosmic::iced::keyboard::{self, Location, Modifiers, key::Code, key::Physical};
    let mut shell = shell();
    let launcher = shell.launcher.as_mut().unwrap();
    launcher.category = Some(Category::Apps);
    launcher.rows = (0..24).map(|n| row(&format!("app {n}"))).collect();
    let id = launcher.id;
    for (width, height, columns, rows) in [(720, 404, 4, 3), (480, 300, 2, 2), (320, 220, 1, 1)] {
        let launcher = shell.launcher.as_mut().unwrap();
        launcher.viewport = (width, height);
        launcher.selected = 0;
        assert_eq!(launcher.grid_columns(), columns);
        assert_eq!(launcher.grid_rows(), rows);
        assert!(launcher.target_height() <= f64::from(height));
        let _ = shell.launcher_event(
            &Event::Keyboard(keyboard::Event::KeyPressed {
                key: Key::Named(Named::ArrowDown),
                modified_key: Key::Named(Named::ArrowDown),
                physical_key: Physical::Code(Code::ArrowDown),
                location: Location::Standard,
                modifiers: Modifiers::empty(),
                text: None,
                repeat: false,
            }),
            id,
        );
        assert_eq!(
            shell.launcher.as_ref().unwrap().selected,
            columns,
            "Down must select the tile directly below the current one"
        );
    }
}

#[test]
fn accessibility_activation_in_scrolled_app_grid_keeps_target_identity() {
    use cosmic::iced::advanced::renderer::Headless;
    use cosmic::iced::advanced::widget::operation::scrollable;
    use cosmic::iced::advanced::{Layout, Shell, layout, mouse, widget::Tree};
    use cosmic::iced::{Font, Pixels, Rectangle, Size};
    use iced_accessibility::A11yId;
    use iced_accessibility::accesskit::{Action, ActionRequest, TreeId};

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let renderer = runtime
        .block_on(<cosmic::Renderer as Headless>::new(
            Font::default(),
            Pixels(16.),
            None,
        ))
        .unwrap();
    let mut shell = shell();
    let launcher = shell.launcher.as_mut().unwrap();
    launcher.category = Some(Category::Apps);
    launcher.progress = AnimatedValue::new(1.);
    launcher.height = AnimatedValue::new(404.);
    launcher.apps_first_row = 2;
    launcher.selected = 8;
    launcher.rows = (0..24)
        .map(|n| {
            let mut row = row(&format!("App {n}"));
            row.target = Target::App {
                id: format!("test.app{n}"),
                desktop: format!("/nonexistent/test.app{n}.desktop").into(),
            };
            row.subtitle = "Application".into();
            row
        })
        .collect();
    let (id, scroll) = (launcher.id, launcher.apps_scroll_id.clone());
    let mut view = shell.view_launcher(id);
    let bounds = Rectangle::with_size(Size::new(720., 404.));
    let mut tree = Tree::new(&view);
    let node = view.as_widget_mut().layout(
        &mut tree,
        &renderer,
        &layout::Limits::new(Size::ZERO, bounds.size()),
    );
    view.as_widget_mut().operate(
        &mut tree,
        Layout::new(&node),
        &renderer,
        &mut scrollable::scroll_to(
            scroll,
            scrollable::AbsoluteOffset {
                x: None,
                y: Some(216.),
            },
        ),
    );
    let accessible =
        view.as_widget()
            .a11y_nodes(Layout::new(&node), &tree, mouse::Cursor::Unavailable);
    let nodes = accessible.root().iter().chain(accessible.children());
    let target = nodes
        .clone()
        .find(|n| n.node().label() == Some("App 8, Application"))
        .unwrap();
    assert_eq!(target.node().is_selected(), Some(true));
    assert!(
        nodes
            .clone()
            .any(|n| n.node().label() == Some("App 4, Application")),
        "preceding row must remain prefetched"
    );
    let A11yId::Widget(widget_id) = target.id() else {
        panic!("button must have a widget ID")
    };
    let event = Event::A11y(
        widget_id.clone(),
        ActionRequest {
            action: Action::Click,
            target_tree: TreeId::ROOT,
            target_node: target.id().clone().into(),
            data: None,
        },
    );
    let mut messages = Vec::new();
    view.as_widget_mut().update(
        &mut tree,
        &event,
        Layout::new(&node),
        mouse::Cursor::Unavailable,
        &renderer,
        &mut cosmic::iced::advanced::clipboard::Null,
        &mut Shell::new(&mut messages),
        &bounds,
    );
    assert_eq!(messages.len(), 1);
    assert!(
        matches!(&messages[0], cosmic::Action::App(crate::Message::Launcher(Message::ActivateRow(owner, target))) if *owner == id && *target == sources::ResultId::from("App 8"))
    );
}

#[test]
fn small_launcher_views_keep_selected_controls_inside_the_surface() {
    use cosmic::iced::advanced::renderer::Headless;
    use cosmic::iced::advanced::{Layout, layout, mouse, widget::Tree};
    use cosmic::iced::{Font, Pixels, Size};
    use iced_accessibility::accesskit::Role;

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let renderer = runtime
        .block_on(<cosmic::Renderer as Headless>::new(
            Font::default(),
            Pixels(16.),
            None,
        ))
        .unwrap();
    let mut shell = shell();
    for (width, height) in [(320, 220), (480, 300), (720, 404)] {
        for apps in [false, true] {
            for activating in [false, true] {
                let launcher = shell.launcher.as_mut().unwrap();
                launcher.viewport = (width, height);
                launcher.category = Some(if apps {
                    Category::Apps
                } else {
                    Category::Files
                });
                launcher.apps = Some(Arc::new(apps::Apps::empty()));
                launcher.alternatives = !apps;
                launcher.activating = activating;
                launcher.rows = (0..24).map(|n| row(&format!("result {n}"))).collect();
                launcher.progress = AnimatedValue::new(1.);
                launcher.height = AnimatedValue::new(launcher.target_height());
                let id = launcher.id;
                let mut view = shell.view_launcher(id);
                let mut tree = Tree::new(&view);
                let layout = view.as_widget_mut().layout(
                    &mut tree,
                    &renderer,
                    &layout::Limits::new(Size::ZERO, Size::new(width as f32, height as f32)),
                );
                let accessible = view.as_widget().a11y_nodes(
                    Layout::new(&layout),
                    &tree,
                    mouse::Cursor::Unavailable,
                );
                let nodes = accessible.root().iter().chain(accessible.children());
                let input = nodes
                    .clone()
                    .find(|n| n.node().role() == Role::TextInput)
                    .unwrap();
                assert_eq!(input.node().is_disabled(), activating);
                let selected = nodes
                    .clone()
                    .find(|n| n.node().is_selected() == Some(true))
                    .unwrap();
                assert_eq!(selected.node().is_disabled(), activating);
                for control in [input, selected] {
                    let bounds = control.node().bounds().unwrap();
                    assert!(
                        bounds.x0 >= 0.
                            && bounds.y0 >= 0.
                            && bounds.x1 <= f64::from(width)
                            && bounds.y1 <= f64::from(height),
                        "clipped control at {width}x{height}: {bounds:?}"
                    );
                }
            }
        }
    }
}
