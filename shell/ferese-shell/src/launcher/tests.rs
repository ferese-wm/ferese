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
    shell.render.motion.reduced_motion = true;
    let launcher = shell.launcher.as_mut().unwrap();
    launcher.category = Some(Category::Apps);
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
    shell.render.motion.reduced_motion = true;
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

#[test]
fn motion_does_not_subscribe_to_application_ticks() {
    let mut shell = shell();
    assert_eq!(shell.launcher_subscription().units(), 0);
    let launcher = shell.launcher.as_mut().unwrap();
    launcher.category = Some(Category::Windows);
    assert_eq!(shell.launcher_subscription().units(), 1);
    shell.launcher.as_mut().unwrap().file_status = files::Status::Indexing;
    assert_eq!(shell.launcher_subscription().units(), 2);
    shell.launcher.as_mut().unwrap().category = Some(Category::Files);
    assert_eq!(shell.launcher_subscription().units(), 1);
    shell.launcher.as_mut().unwrap().closing = true;
    assert_eq!(shell.launcher_subscription().units(), 0);
}

#[test]
fn sampled_motion_tracks_every_refresh_and_settles_without_updates() {
    let start = Instant::now();
    for hz in [60, 144, 240] {
        let settings = motion::Settings::default();
        let mut animation = animation::Animation::new(settings, start);
        animation.retarget(220., false, settings, start);
        let mut previous = animation.sample(start);
        let mut samples = 0;
        for frame in 1..hz * 2 {
            let now = start + Duration::from_secs_f64(frame as f64 / hz as f64);
            let sample = animation.sample(now);
            if previous.active() {
                if previous.progress.is_animating() {
                    assert!(sample.progress.current > previous.progress.current);
                }
                if previous.height.is_animating() {
                    assert!(sample.height.current > previous.height.current);
                }
                samples += 1;
            }
            previous = sample;
        }
        assert!(
            samples > hz / 4,
            "motion is sampled at the surface's refresh cadence"
        );
        assert!(!previous.active());
        assert_eq!(previous.progress.current, 1.);
        assert_eq!(previous.height.current, 220.);
        assert!(!animation.sample(start + Duration::from_secs(60)).active());
    }
}

#[test]
fn retargeting_preserves_the_presented_state_and_clock() {
    let start = Instant::now();
    let settings = motion::Settings::default();
    let mut animation = animation::Animation::new(settings, start);
    let now = start + Duration::from_millis(80);
    let before = animation.sample(now);
    animation.retarget(220., false, settings, now);
    let after = animation.sample(now);
    assert_eq!(before.progress, after.progress);
    assert_eq!(before.height.current, after.height.current);
    animation.retarget(220., false, settings, now + Duration::from_millis(20));
    assert_eq!(
        animation.started, now,
        "an unrelated view build must not restart motion"
    );
    animation.retarget(220., true, settings, now + Duration::from_millis(40));
    let closed = animation.sample(now + Duration::from_secs(2));
    assert_eq!(closed.progress.current, 0.);
    assert!(!closed.active());
}

#[test]
fn stale_close_completion_cannot_destroy_a_reopened_launcher() {
    let mut shell = shell();
    let old = shell.launcher.as_ref().unwrap().id;
    let _ = shell.destroy_launcher();
    let _ = shell.toggle_launcher("test".into());
    let _ = shell.update_launcher(Message::Closed(old));
    let id = shell.launcher.as_ref().unwrap().id;
    let _ = shell.update_launcher(Message::Closed(id));
    assert!(
        shell.launcher.is_some(),
        "only a settled closing invocation can complete"
    );
}

#[test]
fn reduced_motion_samples_final_geometry_immediately() {
    let now = Instant::now();
    let settings = motion::Settings {
        reduced_motion: true,
        ..Default::default()
    };
    let mut animation = animation::Animation::new(settings, now);
    animation.retarget(220., false, settings, now);
    let sample = animation.sample(now);
    assert_eq!(sample.progress.current, 1.);
    assert_eq!(sample.height.current, 220.);
    assert!(!sample.active());
    animation.retarget(72., true, settings, now);
    assert_eq!(animation.sample(now).progress.current, 0.);
    assert!(!animation.sample(now).active());
}

#[test]
fn launcher_redraws_locally_and_emits_only_its_close_completion() {
    use cosmic::iced::advanced::renderer::Headless;
    use cosmic::iced::advanced::{Layout, Shell, clipboard, layout, mouse, widget};
    use cosmic::iced::window::RedrawRequest;
    use cosmic::iced::{Rectangle, Size};

    let renderer = cosmic::iced::futures::executor::block_on(cosmic::Renderer::new(
        cosmic::iced::Font::default(),
        cosmic::iced::Pixels(14.),
        Some("tiny-skia"),
    ))
    .unwrap();
    let viewport = Rectangle::with_size(Size::new(720., 404.));
    for hz in [60, 144, 240] {
        for closing in [false, true] {
            let mut shell = shell();
            let settings = shell.render.motion;
            let launcher = shell.launcher.as_mut().unwrap();
            let id = launcher.id;
            let start = Instant::now();
            launcher.category = Some(Category::Windows);
            launcher.closing = closing;
            let animation = launcher.animation.get_mut();
            *animation = animation::Animation::new(
                settings,
                if closing {
                    start - Duration::from_secs(2)
                } else {
                    start
                },
            );
            animation.retarget(220., closing, settings, start);
            let mut view = shell.view_launcher(id);
            let mut tree = widget::Tree::new(&view);
            let node = view.as_widget_mut().layout(
                &mut tree,
                &renderer,
                &layout::Limits::new(Size::ZERO, viewport.size()),
            );
            let mut messages = Vec::new();
            let mut settled = false;
            for frame in 0..hz * 2 {
                let at = start + Duration::from_secs_f64(frame as f64 / hz as f64);
                let active = shell
                    .launcher
                    .as_ref()
                    .unwrap()
                    .animation
                    .borrow()
                    .sample(at)
                    .active();
                for _ in 0..2 {
                    let mut event_shell = Shell::new(&mut messages);
                    view.as_widget_mut().update(
                        &mut tree,
                        &Event::Window(window::Event::RedrawRequested(at)),
                        Layout::new(&node),
                        mouse::Cursor::Unavailable,
                        &renderer,
                        &mut clipboard::Null,
                        &mut event_shell,
                        &viewport,
                    );
                    assert!(
                        !event_shell.are_widgets_invalid(),
                        "surface animation must not invalidate application-wide views"
                    );
                    assert_eq!(
                        event_shell.redraw_request(),
                        if active {
                            RedrawRequest::NextFrame
                        } else {
                            RedrawRequest::Wait
                        }
                    );
                }
                if !active || (closing && !messages.is_empty()) {
                    settled = true;
                    break;
                }
                assert!(
                    messages.is_empty(),
                    "animation frames must not cause application updates"
                );
            }
            assert!(settled);
            if closing {
                assert!(
                    matches!(messages.as_slice(), [cosmic::Action::App(super::super::Message::Launcher(Message::Closed(closed)))] if *closed == id)
                );
            } else {
                assert!(messages.is_empty());
            }
        }
    }
}

#[test]
fn close_completion_finishes_the_settled_invocation() {
    let mut shell = shell();
    let settings = shell.render.motion;
    let launcher = shell.launcher.as_mut().unwrap();
    let id = launcher.id;
    let now = Instant::now();
    let animation = launcher.animation.get_mut();
    *animation = animation::Animation::new(settings, now - Duration::from_secs(4));
    animation.retarget(72., true, settings, now - Duration::from_secs(2));
    launcher.closing = true;
    let _ = shell.update_launcher(Message::Closed(id));
    assert!(shell.launcher.is_none());
}
