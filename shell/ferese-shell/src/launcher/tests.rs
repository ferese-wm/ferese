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
