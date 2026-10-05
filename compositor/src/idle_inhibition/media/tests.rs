use super::*;
use ferese_ipc::media::Playback;
use policy::PlayersState;

fn player(name: &str, status: Playback) -> Player {
    Player {
        name: name.into(),
        owner: format!(":{name}"),
        playing: status == Playback::Playing,
        view: ferese_ipc::media::Player {
            name: name.into(),
            status,
            ..Default::default()
        },
        ..Default::default()
    }
}

#[test]
fn selection_tracks_starts_not_metadata_or_volume_changes() {
    let mut state = PlayersState::default();
    state.update(player("first", Playback::Playing));
    state.update(player("second", Playback::Playing));
    let mut first = state.players["first"].clone();
    first.view.title = "New track".into();
    first.view.volume = Some(0.5);
    state.update(first);
    assert_eq!(state.selected().unwrap().name, "second");

    state.update(player("second", Playback::Paused));
    assert_eq!(state.selected().unwrap().name, "first");
    state.update(player("first", Playback::Paused));
    assert_eq!(state.selected().unwrap().name, "first");
    state.update(player("second", Playback::Playing));
    assert_eq!(state.selected().unwrap().name, "second");
}

#[test]
fn pin_ignore_and_departure_do_not_leave_a_ghost_player() {
    let mut state = PlayersState::default();
    state.update(player("first", Playback::Paused));
    state.update(player("second", Playback::Playing));
    state.pinned = Some("first".into());
    assert_eq!(state.selected().unwrap().name, "first");
    state.ignored.insert("first".into());
    assert_eq!(state.selected().unwrap().name, "second");
    state.remove("first");
    assert!(state.pinned.is_none());
    assert!(state.ignored.is_empty());
    state.remove("second");
    assert!(state.selected().is_none());
}

#[test]
fn initial_ties_are_deterministic_and_stopped_players_are_hidden() {
    let mut state = PlayersState::default();
    for name in ["z", "a"] {
        state.players.insert(name.into(), player(name, Playback::Playing));
    }
    assert_eq!(state.selected().unwrap().name, "a");
    state.update(player("a", Playback::Stopped));
    state.update(player("z", Playback::Stopped));
    assert!(state.selected().is_none());
}

#[test]
fn watch_waits_for_changes_deduplicates_and_drops_disconnected_clients() {
    let mut engine = Engine::default();
    let (send, receive) = mpsc::sync_channel(1);
    engine.watch(1, 42, 0, send);
    engine.update(Snapshot::default());
    assert!(receive.try_recv().is_err());
    let mut state = PlayersState::default();
    state.update(player("first", Playback::Playing));
    engine.update(state.update_snapshot().snapshot);
    let response = receive.try_recv().unwrap();
    assert_eq!(response.id, 42);
    assert_eq!(response.result.unwrap()["revision"], 1);
    let (send, receive) = mpsc::sync_channel(1);
    engine.watch(2, 43, 1, send);
    engine.remove(2);
    state.remove("first");
    engine.update(state.update_snapshot().snapshot);
    assert!(receive.try_recv().is_err());
}

#[test]
fn metadata_is_bounded_and_control_characters_are_removed() {
    assert_eq!(bus::clean("  Track\nname\0  ", 80), "Trackname");
    assert_eq!(bus::clean(&"é".repeat(500), 80).chars().count(), 80);
}

#[test]
fn metadata_and_position_updates_do_not_recompute_idle_policy() {
    let before = player("first", Playback::Playing);
    let mut after = before.clone();
    after.view.title = "New track".into();
    after.view.position_us = Some(10);
    after.view.volume = Some(0.8);
    assert!(!inhibition_changed(
        std::slice::from_ref(&before),
        std::slice::from_ref(&after)
    ));
    after.playing = false;
    assert!(inhibition_changed(
        std::slice::from_ref(&before),
        std::slice::from_ref(&after)
    ));
    after.playing = true;
    after.pid = Some(42);
    assert!(inhibition_changed(
        std::slice::from_ref(&before),
        std::slice::from_ref(&after)
    ));
    assert!(inhibition_changed(std::slice::from_ref(&before), &[]));
}
