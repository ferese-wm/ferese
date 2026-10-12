use super::*;

fn owner() -> Owner {
    Owner { pid: 42, birth: 100 }
}
fn data(priority: u8) -> Data {
    Data {
        source: Source::Publisher,
        output: Some("one".into()),
        priority,
        icon: "hammer".into(),
        title: "Build".into(),
        value: Value::Indeterminate,
        template: Template::Progress,
    }
}
fn start(store: &mut Store, local: &str, now: Time, priority: u8) -> Id {
    store
        .start(
            owner(),
            local.into(),
            data(priority),
            now,
            0,
            Some(10_000),
            true,
            true,
            None,
        )
        .unwrap()
}

#[test]
fn boundaries_and_updates_do_not_restart_delay_or_boost() {
    let mut store = Store::new(1);
    let id = start(&mut store, "a", 10, 255);
    assert!(!store.entries().next().unwrap().eligible(309, false));
    store
        .update(owner(), &id, Some(Value::Progress(0.5)), None, 200)
        .unwrap();
    let entry = store.entries().next().unwrap();
    assert_eq!(entry.eligible_at, 310);
    assert!(entry.eligible(310, false));
    assert_eq!(entry.score(5_309, 255), 285);
    assert_eq!(entry.score(5_310, 255), 255);
    assert_eq!(store.next_deadline(310), Some(5_310));
}

#[test]
fn fast_failures_cancel_and_suppressed_failure_never_flash() {
    for state in [State::Done, State::Failed, State::Cancelled] {
        let mut store = Store::new(1);
        let id = start(&mut store, "a", 0, 100);
        store.finish(owner(), &id, state, None, 299).unwrap();
        assert_eq!(store.entries().count(), 0);
    }
    let mut store = Store::new(1);
    let id = store
        .start(owner(), "a".into(), data(100), 0, 0, None, true, false, None)
        .unwrap();
    store.finish(owner(), &id, State::Failed, None, 300).unwrap();
    assert_eq!(store.entries().count(), 0);
}

#[test]
fn result_retry_and_owner_exit_preserve_original_deadline() {
    let mut store = Store::new(1);
    let id = start(&mut store, "a", 0, 255);
    store
        .finish(owner(), &id, State::Failed, Some("Failed".into()), 300)
        .unwrap();
    assert_eq!(store.entries().next().unwrap().score(300, 255), 385);
    store.finish(owner(), &id, State::Failed, None, 1_000).unwrap();
    store.owner_exit(owner());
    assert_eq!(store.entries().next().unwrap().result_deadline, Some(1_800));
    assert!(!store.advance(1_799));
    assert!(store.advance(1_800));
    assert_eq!(store.entries().count(), 0);
}

#[test]
fn leases_refresh_but_abandonment_never_claims_a_result() {
    let mut store = Store::new(1);
    let id = store
        .start(owner(), "a".into(), data(100), 0, 0, Some(500), false, true, None)
        .unwrap();
    store.update(owner(), &id, None, None, 499).unwrap();
    assert_eq!(store.entries().next().unwrap().lease_deadline, Some(999));
    store.advance(998);
    assert_eq!(store.entries().count(), 1);
    store.advance(999);
    assert_eq!(store.entries().count(), 0);
    start(&mut store, "a", 1_000, 100);
    store.owner_exit(owner());
    assert_eq!(store.entries().count(), 0);
    assert!(
        store
            .start(owner(), "b".into(), data(0), 0, 0, None, false, true, None)
            .is_err()
    );
}

#[test]
fn owner_session_generation_and_quotas_are_independent_of_connections() {
    let mut store = Store::new(12);
    let id = start(&mut store, "a", 0, 100);
    let other = Owner { pid: 42, birth: 101 };
    assert!(store.update(other, &id, None, None, 0).is_err());
    let mut stale = id.clone();
    stale.session = 11;
    assert!(store.update(owner(), &stale, None, None, 0).is_err());
    assert!(
        store
            .start(owner(), "a".into(), data(100), 0, 0, None, true, true, None)
            .is_err()
    );
    start(&mut store, "b", 0, 0);
    start(&mut store, "c", 0, 0);
    assert!(
        store
            .start(owner(), "d".into(), data(100), 0, 0, None, true, true, None)
            .is_err()
    );
    store.finish(owner(), &id, State::Cancelled, None, 0).unwrap();
    let replacement = start(&mut store, "a", 0, 100);
    assert_ne!(replacement.generation, id.generation);
    assert!(store.update(owner(), &id, None, None, 0).is_err());
}

#[test]
fn stable_ties_boost_expiry_notification_priority_and_hover_pinning() {
    let mut store = Store::new(1);
    let a = start(&mut store, "a", 0, 100);
    let b = start(&mut store, "b", 0, 100);
    let c = start(&mut store, "c", 5_000, 110);
    let mut selection = Selection::default();
    selection.select(
        &ranked(store.entries(), 300, store.limits, "one", false),
        store.limits,
        false,
        0,
    );
    assert_eq!(selection.visible, [a.clone(), b.clone()]);
    selection.enter();
    selection.select(
        &ranked(store.entries(), 5_300, store.limits, "one", false),
        store.limits,
        false,
        0,
    );
    assert_eq!(selection.visible, [a.clone(), b.clone()]);
    assert_eq!(selection.hidden, 1);
    store.owner_exit(owner());
    selection.select(
        &ranked(store.entries(), 5_300, store.limits, "one", false),
        store.limits,
        false,
        0,
    );
    assert!(selection.visible.is_empty());
    selection.leave();
    let _ = c;
}

#[test]
fn dnd_protected_slots_and_output_routing() {
    let mut store = Store::new(1);
    let a = start(&mut store, "a", 0, 100);
    start(&mut store, "b", 0, 100);
    store.finish(owner(), &a, State::Done, None, 300).unwrap();
    let mut selection = Selection::default();
    selection.select(
        &ranked(store.entries(), 300, store.limits, "one", true),
        store.limits,
        true,
        0,
    );
    assert_eq!(selection.visible.len(), 1);
    assert_eq!(selection.hidden, 0);
    selection.select(
        &ranked(store.entries(), 300, store.limits, "one", false),
        store.limits,
        false,
        2,
    );
    assert!(selection.visible.is_empty());
    assert_eq!(selection.hidden, 2);
    store.remove_output("one", Some("two"));
    assert!(ranked(store.entries(), 300, store.limits, "one", false).is_empty());
    assert_eq!(ranked(store.entries(), 300, store.limits, "two", false).len(), 2);
}

#[test]
fn typed_validation_countdown_clock_and_focus_identity() {
    let mut value = data(0);
    for progress in [f32::NAN, f32::INFINITY, -0.1, 1.1] {
        value.value = Value::Progress(progress);
        assert!(value.validate(0).is_err());
    }
    value.value = Value::Indeterminate;
    value.icon = "/tmp/icon.svg".into();
    assert!(value.validate(0).is_err());
    value.icon = "unknown".into();
    value.validate(0).unwrap();
    assert_eq!(value.icon, "ferese");
    value.template = Template::Timer;
    value.value = Value::Countdown {
        remaining_ms: 1_000,
        sampled_at: u64::MAX,
    };
    value.validate(100).unwrap();
    assert_eq!(value.value.remaining_ms(900), Some(200));
    let mut store = Store::new(1);
    let a = start(&mut store, "a", 0, 0);
    let b = start(&mut store, "b", 0, 0);
    let mut focus = Some(b.clone());
    reconcile_focus(&mut focus, &[a.clone(), b.clone()], &[b.clone(), a.clone()]);
    assert_eq!(focus, Some(b.clone()));
    reconcile_focus(&mut focus, &[b, a.clone()], std::slice::from_ref(&a));
    assert_eq!(focus, Some(a));
}

#[test]
fn notification_boost_cannot_outrank_ordinary_and_grouped_lifetimes_remain_independent() {
    let mut store = Store::new(1);
    let a = store
        .start(
            owner(),
            "a".into(),
            data(0),
            0,
            0,
            None,
            true,
            true,
            Some("cargo:directory".into()),
        )
        .unwrap();
    let b = store
        .start(
            owner(),
            "b".into(),
            data(1),
            0,
            0,
            None,
            true,
            true,
            Some("cargo:directory".into()),
        )
        .unwrap();
    let mut notification = data(255);
    notification.source = Source::Notification;
    let notice = store
        .start(owner(), "notice".into(), notification, 0, 0, None, true, true, None)
        .unwrap();
    store.finish(owner(), &notice, State::Failed, None, 300).unwrap();
    let entries = ranked(store.entries(), 300, store.limits, "one", false);
    assert_eq!(entries.last().unwrap().id, notice);
    let mut selection = Selection::default();
    selection.select(&entries, store.limits, false, 0);
    assert_eq!(selection.visible[0], b);
    store.finish(owner(), &b, State::Done, None, 300).unwrap();
    let entries = ranked(store.entries(), 301, store.limits, "one", false);
    selection.select(&entries, store.limits, false, 0);
    assert_eq!(selection.visible[0], a);
    assert_eq!(
        store.entries().find(|entry| entry.id == a).unwrap().state,
        State::Running
    );
    store.remove_output("one", None);
    assert!(store.attach_output("two"));
    assert_eq!(ranked(store.entries(), 301, store.limits, "two", false).len(), 3);
}

#[test]
fn invalid_updates_are_atomic_and_suspend_aware_eligibility_hides_expired_records() {
    let mut store = Store::new(1);
    let id = store
        .start(owner(), "atomic".into(), data(100), 0, 0, Some(1_000), true, true, None)
        .unwrap();
    assert!(
        store
            .update(owner(), &id, Some(Value::Progress(0.8)), Some(0), 300)
            .is_err()
    );
    let entry = store.entries().next().unwrap();
    assert_eq!(entry.data.value, Value::Indeterminate);
    assert_eq!(entry.refreshed_at, 0);
    assert_eq!(entry.lease_deadline, Some(1_000));
    assert!(
        !entry.eligible(5_000, false),
        "a resumed host cannot present an expired lease before its timer callback"
    );
}
