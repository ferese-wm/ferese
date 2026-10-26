use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::mpsc::SyncSender;

use ferese_ipc::events::{Change, Event, Snapshot, VERSION};

pub(crate) const QUEUE_CAPACITY: usize = 32;
const MAX_SUBSCRIBERS: usize = 16;

struct Subscriber {
    sender: SyncSender<Arc<Event>>,
    initial: bool,
}

#[derive(Default)]
pub(crate) struct Subscribers {
    subscribers: HashMap<u64, Subscriber>,
    previous: Option<Snapshot>,
    generation: u64,
    pub(crate) config_revision: u64,
    pub(crate) outputs: RefCell<crate::ipc::outputs::Cache>,
}

impl Subscribers {
    pub(crate) fn subscribe(
        &mut self,
        owner: u64,
        sender: SyncSender<Arc<Event>>,
    ) -> Result<(), &'static str> {
        if self.subscribers.len() >= MAX_SUBSCRIBERS {
            return Err("Event subscriber limit reached");
        }
        self.subscribers.insert(
            owner,
            Subscriber {
                sender,
                initial: true,
            },
        );
        Ok(())
    }

    pub(crate) fn remove(&mut self, owner: u64) {
        self.subscribers.remove(&owner);
        if self.subscribers.is_empty() {
            self.previous = None;
            self.outputs.get_mut().clear();
        }
    }

    pub(crate) fn active(&self) -> bool {
        !self.subscribers.is_empty()
    }

    /// Called only after logical mutations for a dispatch have completed.
    pub(crate) fn publish(&mut self, snapshot: Snapshot) {
        let changes = self
            .previous
            .as_ref()
            .map(|previous| snapshot.changes_since(previous))
            .unwrap_or_default();
        if changes.is_empty()
            && !self
                .subscribers
                .values()
                .any(|subscriber| subscriber.initial)
        {
            // Keep the latest shared payload even when a theme publication
            // produced equal logical values, so later comparisons stay cheap.
            self.previous = self.active().then_some(snapshot);
            return;
        }
        self.generation = self.generation.wrapping_add(1);
        let mut events = changes
            .into_iter()
            .map(|change| {
                Arc::new(Event {
                    version: VERSION,
                    generation: self.generation,
                    last: false,
                    change,
                })
            })
            .collect::<Vec<_>>();
        if let Some(last) = events.last_mut() {
            Arc::get_mut(last)
                .expect("new initial-event records have not been shared with subscribers")
                .last = true;
        }
        let initial = self
            .subscribers
            .values()
            .any(|subscriber| subscriber.initial)
            .then(|| {
                Arc::new(Event {
                    version: VERSION,
                    generation: self.generation,
                    last: true,
                    change: Change::Snapshot {
                        desktop: Box::new(snapshot.clone()),
                    },
                })
            });
        self.subscribers.retain(|_, subscriber| {
            let delivered = if subscriber.initial {
                subscriber
                    .sender
                    .try_send(
                        initial
                            .as_ref()
                            .expect("the initial event batch is built before subscription delivery")
                            .clone(),
                    )
                    .is_ok()
            } else {
                events
                    .iter()
                    .all(|event| subscriber.sender.try_send(event.clone()).is_ok())
            };
            subscriber.initial = false;
            // A partial generation is never followed by another one: an
            // overflowing stream ends, and reconnecting starts with a snapshot.
            delivered
        });
        self.previous = self.active().then_some(snapshot);
        if !self.active() {
            self.outputs.get_mut().clear();
        }
    }
}

impl crate::Ferese {
    pub(crate) fn publish_ipc_events(&mut self) {
        if !self.ipc_events.active()
            || self.desktop_transition.is_some()
            || self
                .direct_backend
                .as_ref()
                .is_some_and(|backend| backend.reconciling())
        {
            return;
        }
        let snapshot = self.ipc_event_snapshot();
        self.ipc_events.publish(snapshot);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ferese_ipc::events::{ConfigState, LockState};
    use serde_json::json;
    use std::sync::mpsc::{TryRecvError, sync_channel};

    #[test]
    #[ignore = "manual release benchmark for IPC snapshot construction"]
    fn profile_unchanged_event_snapshots() {
        let name = "ipc_events::tests::profile_unchanged_event_snapshots";
        if !crate::startup_tests::private_runtime(name) {
            return;
        }
        let mut events = smithay::reexports::calloop::EventLoop::try_new().unwrap();
        let state = crate::startup_tests::state(&mut events);
        for _ in 0..100 {
            std::hint::black_box(state.ipc_event_snapshot());
        }
        for run in 1..=3 {
            let started = std::time::Instant::now();
            for _ in 0..2000 {
                std::hint::black_box(state.ipc_event_snapshot());
            }
            eprintln!(
                "event_snapshot_benchmark run={run} iterations=2000 elapsed_us={}",
                started.elapsed().as_micros()
            );
        }
    }

    #[test]
    #[ignore = "manual release benchmark for populated IPC publication"]
    fn profile_populated_event_publication() {
        use crate::state::DesktopOutput;
        use smithay::output::{Mode, Output, PhysicalProperties, Scale, Subpixel};
        use smithay::utils::Transform;

        if !crate::startup_tests::private_runtime(
            "ipc_events::tests::profile_populated_event_publication",
        ) {
            return;
        }
        let mut events = smithay::reexports::calloop::EventLoop::try_new().unwrap();
        let mut state = crate::startup_tests::state(&mut events);
        let output = DesktopOutput {
            output: Output::new(
                "benchmark".into(),
                PhysicalProperties {
                    serial_number: String::new(),
                    size: (0, 0).into(),
                    subpixel: Subpixel::Unknown,
                    make: "test".into(),
                    model: "test".into(),
                },
            ),
            identity: "benchmark".into(),
            mode: Mode {
                size: (1920, 1080).into(),
                refresh: 60_000,
            },
            transform: Transform::Normal,
            scale: Scale::Fractional(1.0),
            position: (0, 0).into(),
        };
        state.begin_desktop_transition();
        let changes = state.publish_desktop(vec![output]).unwrap();
        state.finish_desktop_transition(changes);
        let mut clients = Vec::new();
        for window_count in [0, 1, 20, 100] {
            while clients.len() < window_count {
                let (window, client) =
                    crate::state::test_window(&mut state, &mut events, 0xff204060);
                let id = state.windows.ids()[&window];
                let record = state.windows.record_mut(id).unwrap();
                record.app_id = "org.example.Editor".into();
                record.title = format!("Document {} — publication benchmark", id.0);
                clients.push(client);
            }
            let (sender, receive) = sync_channel(QUEUE_CAPACITY);
            state.ipc_events.subscribe(1, sender).unwrap();
            state.publish_ipc_events();
            assert!(matches!(
                receive.try_recv().unwrap().change,
                Change::Snapshot { .. }
            ));
            for _ in 0..100 {
                state.publish_ipc_events();
            }
            for run in 1..=3 {
                let started = std::time::Instant::now();
                for _ in 0..2000 {
                    state.publish_ipc_events();
                }
                eprintln!(
                    "event_publication_benchmark windows={window_count} run={run} iterations=2000 elapsed_us={}",
                    started.elapsed().as_micros()
                );
            }
            assert!(matches!(receive.try_recv(), Err(TryRecvError::Empty)));
            state.ipc_events.remove(1);
        }
    }

    fn snapshot() -> Snapshot {
        Snapshot {
            outputs: json!([]).into(),
            workspaces: json!([]),
            windows: Vec::new(),
            focus: json!(null),
            config: ConfigState::default(),
            theme: json!({}).into(),
            lock: LockState {
                phase: "unlocked".into(),
                sleeping: false,
            },
        }
    }

    #[test]
    fn initial_snapshot_and_changes_have_complete_generations_without_duplicates() {
        let mut hub = Subscribers::default();
        let (sender, receive) = sync_channel(QUEUE_CAPACITY);
        hub.subscribe(1, sender).unwrap();
        let mut state = snapshot();
        hub.publish(state.clone());
        assert!(matches!(
            receive.try_recv().unwrap().change,
            Change::Snapshot { .. }
        ));
        hub.publish(state.clone());
        assert!(matches!(receive.try_recv(), Err(TryRecvError::Empty)));
        state.outputs = json!([{"id": 2}]).into();
        state.workspaces = json!([{"output": 2}]);
        state.focus = json!({"output": 2});
        hub.publish(state);
        let events = receive.try_iter().collect::<Vec<_>>();
        assert_eq!(events.len(), 3);
        assert!(
            events
                .iter()
                .all(|event| event.generation == events[0].generation)
        );
        assert!(!events[0].last && !events[1].last && events[2].last);
    }

    #[test]
    fn equal_theme_publications_retain_shared_storage_without_emitting_events() {
        let mut hub = Subscribers::default();
        let (sender, receive) = sync_channel(QUEUE_CAPACITY);
        hub.subscribe(1, sender).unwrap();
        let first = snapshot();
        hub.publish(first.clone());
        receive.try_recv().unwrap();
        let generation = hub.generation;
        let mut next = first;
        next.theme = Arc::new(next.theme.as_ref().clone());
        let payload = next.theme.clone();
        hub.publish(next);
        assert!(matches!(receive.try_recv(), Err(TryRecvError::Empty)));
        assert_eq!(hub.generation, generation);
        assert!(Arc::ptr_eq(&hub.previous.as_ref().unwrap().theme, &payload));
    }

    #[test]
    fn full_and_disconnected_subscribers_do_not_block_healthy_readers() {
        let mut hub = Subscribers::default();
        let (slow, slow_receive) = sync_channel(1);
        let (fast, fast_receive) = sync_channel(QUEUE_CAPACITY);
        let (closed, closed_receive) = sync_channel(1);
        drop(closed_receive);
        hub.subscribe(1, slow).unwrap();
        hub.subscribe(2, fast).unwrap();
        hub.subscribe(3, closed).unwrap();
        let mut state = snapshot();
        hub.publish(state.clone());
        fast_receive.try_recv().unwrap();
        state.focus = json!({"window": 7});
        hub.publish(state);
        assert!(matches!(
            fast_receive.try_recv().unwrap().change,
            Change::FocusChanged { .. }
        ));
        assert_eq!(hub.subscribers.len(), 1);
        slow_receive.try_recv().unwrap();
        assert!(matches!(
            slow_receive.try_recv(),
            Err(TryRecvError::Disconnected)
        ));
        hub.remove(2);
        assert!(!hub.active());
        assert!(hub.previous.is_none());
    }

    #[test]
    fn subscribers_are_bounded_independently_of_command_workers() {
        let mut hub = Subscribers::default();
        let mut receivers = Vec::new();
        for owner in 0..MAX_SUBSCRIBERS as u64 {
            let (sender, receiver) = sync_channel(QUEUE_CAPACITY);
            hub.subscribe(owner, sender).unwrap();
            receivers.push(receiver);
        }
        assert!(hub.subscribe(99, sync_channel(1).0).is_err());
        hub.remove(0);
        assert!(hub.subscribe(99, sync_channel(1).0).is_ok());
    }

    #[test]
    fn all_changed_domains_share_one_generation() {
        let mut hub = Subscribers::default();
        let (sender, receive) = sync_channel(QUEUE_CAPACITY);
        hub.subscribe(1, sender).unwrap();
        let mut state = snapshot();
        hub.publish(state.clone());
        receive.try_recv().unwrap();
        state.outputs = json!([{"id": 1}]).into();
        state.workspaces = json!([{"output": 1}]);
        state.windows = vec![ferese_ipc::events::Window {
            id: 7,
            app_id: "org.example.Terminal".into(),
            title: "Terminal".into(),
            focused: false,
            workspace: 1,
            floating: false,
            fullscreen: false,
        }];
        state.focus = json!({"window": 7});
        state.config.revision = 1;
        state.theme = json!({"mode": "light"}).into();
        state.lock.phase = "locked".into();
        hub.publish(state);
        let events = receive.try_iter().collect::<Vec<_>>();
        assert_eq!(events.len(), 7);
        assert!(
            events
                .iter()
                .all(|event| event.generation == events[0].generation)
        );
        assert!(events[..6].iter().all(|event| !event.last));
        assert!(events[6].last);
    }

    #[test]
    fn topology_events_wait_for_publication_and_report_config_commits() {
        use crate::state::DesktopOutput;
        use smithay::output::{Mode, Output, PhysicalProperties, Scale, Subpixel};
        use smithay::reexports::calloop::EventLoop;
        use smithay::reexports::wayland_server::Display;
        use smithay::utils::Transform;

        if !crate::startup_tests::private_runtime(
            "ipc_events::tests::topology_events_wait_for_publication_and_report_config_commits",
        ) {
            return;
        }
        let mut event_loop = EventLoop::try_new().unwrap();
        let mut config = crate::config::Config::default().runtime_config().unwrap();
        config.wallpaper.path = None;
        let mut state =
            crate::Ferese::new(&mut event_loop, Display::new().unwrap(), config).unwrap();
        let output = |name: &str, x| DesktopOutput {
            output: Output::new(
                name.into(),
                PhysicalProperties {
                    serial_number: String::new(),
                    size: (0, 0).into(),
                    subpixel: Subpixel::Unknown,
                    make: "test".into(),
                    model: "test".into(),
                },
            ),
            identity: name.into(),
            mode: Mode {
                size: (800, 600).into(),
                refresh: 60_000,
            },
            transform: Transform::Normal,
            scale: Scale::Fractional(1.0),
            position: (x, 0).into(),
        };
        let (sender, receive) = sync_channel(QUEUE_CAPACITY);
        state.ipc_events.subscribe(900, sender).unwrap();
        state.begin_desktop_transition();
        let changes = state.publish_desktop(vec![output("a", 0)]).unwrap();
        state.publish_ipc_events();
        assert!(matches!(receive.try_recv(), Err(TryRecvError::Empty)));
        state.finish_desktop_transition(changes);
        state.publish_ipc_events();
        let initial = receive.try_recv().unwrap();
        let Change::Snapshot { desktop } = &initial.change else {
            panic!("initial snapshot")
        };
        assert_eq!(desktop.outputs.as_array().unwrap().len(), 1);
        assert_eq!(desktop.workspaces[0]["output"], desktop.outputs[0]["id"]);
        state.begin_desktop_transition();
        let a = state.current_desktop_outputs().remove(0);
        let changes = state.publish_desktop(vec![a, output("b", 800)]).unwrap();
        state.publish_ipc_events();
        assert!(matches!(receive.try_recv(), Err(TryRecvError::Empty)));
        state.finish_desktop_transition(changes);
        state.publish_ipc_events();
        let events = receive.try_iter().collect::<Vec<_>>();
        assert!(events.last().unwrap().last);
        assert!(
            events
                .iter()
                .all(|event| event.generation == events[0].generation)
        );
        let final_state = state.ipc_event_snapshot();
        assert_eq!(final_state.outputs.as_array().unwrap().len(), 2);
        for workspace in final_state.workspaces.as_array().unwrap() {
            assert!(
                final_state
                    .outputs
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|output| output["id"] == workspace["output"])
            );
        }
        let revision = state.ipc_events.config_revision;
        state
            .reload_config_source("appearance { window-effects { opacity 0.8; }; }".into())
            .unwrap();
        assert_eq!(state.ipc_events.config_revision, revision + 1);
        state.publish_ipc_events();
        assert!(
            receive
                .try_iter()
                .any(|event| matches!(event.change, Change::ConfigChanged { .. }))
        );

        // Negotiate with the real listener and connection worker, then verify
        // that the initial snapshot reaches the wire after dispatch publication.
        for version in [VERSION + 1, VERSION] {
            let socket = ferese_ipc::socket::resolve(None).unwrap();
            let (completed, completion) = sync_channel(1);
            let worker = std::thread::spawn(move || {
                let mut stream = std::os::unix::net::UnixStream::connect(socket).unwrap();
                stream
                    .set_read_timeout(Some(std::time::Duration::from_secs(2)))
                    .unwrap();
                ferese_ipc::write_frame(
                    &mut stream,
                    &ferese_ipc::Request {
                        version: ferese_ipc::VERSION,
                        id: 42,
                        kind: "command".into(),
                        command: "event-stream".into(),
                        args: json!({"version": version}),
                    },
                )
                .unwrap();
                let response: ferese_ipc::Response = ferese_ipc::read_frame(&mut stream).unwrap();
                assert_eq!(response.id, 42);
                if version != VERSION {
                    assert_eq!(response.error.unwrap().code, "unsupported_event_version");
                } else {
                    assert!(response.error.is_none());
                    let initial: Event = ferese_ipc::read_frame(&mut stream).unwrap();
                    assert!(initial.last);
                    let Change::Snapshot { desktop } = initial.change else {
                        panic!("initial wire snapshot")
                    };
                    assert_eq!(desktop.outputs.as_array().unwrap().len(), 2);
                }
                completed.send(()).unwrap();
            });
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
            while completion.try_recv().is_err() {
                assert!(
                    std::time::Instant::now() < deadline,
                    "subscription worker timed out"
                );
                event_loop
                    .dispatch(std::time::Duration::from_millis(10), &mut state)
                    .unwrap();
                state.publish_ipc_events();
            }
            worker.join().unwrap();
        }
    }
}
