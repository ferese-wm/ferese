//! Session broker: verified process identities, one-shot policy deadlines and
//! bounded long-poll subscribers. Process inspection runs on IPC workers.
mod publication;
use ferese_activities::{Data, Id, Owner, Source, State, Store, Template, Value};
use ferese_ipc::{
    Response,
    activities::{Publish, Snapshot},
};
use serde_json::json;
use std::collections::BTreeMap;
use std::sync::mpsc::SyncSender;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

#[derive(Clone, Debug)]
pub(crate) struct Rules {
    pub enabled: bool,
    pub matcher: ferese_config::activities::Matcher,
}
pub(crate) type SharedRules = Arc<RwLock<Arc<Rules>>>;

pub(crate) struct Engine {
    pub store: Store,
    pub rules: SharedRules,
    pub monitor: Option<crate::ipc::activity_owner::Monitor>,
    process_enabled: bool,
    process_sender: Option<smithay::reexports::calloop::channel::SyncSender<crate::ipc::IpcEvent>>,
    process_watcher: Option<crate::activities_process::Watcher>,
    pub timer: Option<crate::ipc::DeadlineTimer<crate::Ferese>>,
    revision: u64,
    cached_snapshot: Option<Arc<Snapshot>>,
    publisher: Option<publication::Publisher>,
    watchers: BTreeMap<u64, (u64, SyncSender<Response>)>,
    locked: bool,
    rates: BTreeMap<Owner, (u64, u16)>,
    windows: BTreeMap<Id, u64>,
    results: BTreeMap<Id, ferese_config::activities::ResultText>,
}
impl Default for Engine {
    fn default() -> Self {
        let settings = ferese_config::activities::Settings::default();
        Self {
            store: Store::new(ferese_ipc::activities::now_ms() ^ u64::from(std::process::id()) << 32),
            rules: Arc::new(RwLock::new(Arc::new(Rules {
                enabled: false,
                matcher: ferese_config::activities::Matcher::new(&settings).expect("built-in rules"),
            }))),
            monitor: None,
            process_enabled: false,
            process_sender: None,
            process_watcher: None,
            timer: None,
            revision: 1,
            cached_snapshot: None,
            publisher: publication::Publisher::new()
                .map_err(|error| tracing::warn!(%error, "activity snapshot worker unavailable"))
                .ok(),
            watchers: BTreeMap::new(),
            locked: false,
            rates: BTreeMap::new(),
            results: BTreeMap::new(),
            windows: BTreeMap::new(),
        }
    }
}
impl Engine {
    pub fn detect_process(
        &mut self,
        owner: Owner,
        matched: ferese_config::activities::Matched,
        output: Option<String>,
        fd: &std::os::fd::OwnedFd,
    ) {
        if crate::ipc::activity_owner::exited(fd) {
            return;
        }
        // A dispatched detection is not an admission acknowledgement. Retry
        // starts after rejection, and refresh existing records without restarting
        // their eligibility delay or recency boost.
        if self.resolve(owner, "process", None).is_ok() {
            let _ = self.publish(
                owner,
                Publish::Update {
                    local: "process".into(),
                    handle: None,
                    value: None,
                    lease_ms: Some(5_000),
                },
                None,
                None,
            );
        } else {
            let _ = self.publish(
                owner,
                Publish::Hook {
                    local: "process".into(),
                    command: String::new(),
                    cwd: String::new(),
                },
                Some(matched),
                output,
            );
        }
    }

    pub fn configure(
        &mut self,
        settings: &ferese_config::activities::Settings,
        matcher: ferese_config::activities::Matcher,
    ) {
        self.store.limits = settings.limits();
        self.process_enabled = settings.sources.process_watch;
        *self.rules.write().expect("activity rules") = Arc::new(Rules {
            enabled: settings.sources.shell_hook,
            matcher,
        });
        self.refresh_process_source();
        self.changed();
    }
    pub fn attach_process_source(
        &mut self,
        sender: smithay::reexports::calloop::channel::SyncSender<crate::ipc::IpcEvent>,
    ) {
        self.process_sender = Some(sender);
        self.refresh_process_source();
    }
    fn refresh_process_source(&mut self) {
        if !self.process_enabled {
            self.process_watcher = None;
        } else if self.process_watcher.is_none()
            && let Some(sender) = &self.process_sender
        {
            match crate::activities_process::Watcher::start(self.rules.clone(), sender.clone()) {
                Ok(watcher) => self.process_watcher = Some(watcher),
                Err(error) => tracing::warn!(%error, "could not start optional activity process source"),
            }
        }
    }
    pub fn snapshot(&mut self) -> Arc<Snapshot> {
        if let Some(snapshot) = &self.cached_snapshot {
            return snapshot.clone();
        }
        let snapshot = Arc::new(Snapshot {
            version: ferese_ipc::activities::VERSION,
            revision: self.revision,
            now_ms: ferese_ipc::activities::now_ms(),
            limits: self.store.limits,
            activities: self.store.entries().cloned().collect(),
            windows: self
                .windows
                .iter()
                .map(|(activity, window)| ferese_ipc::activities::ActivityWindow {
                    activity: activity.clone(),
                    window: *window,
                })
                .collect(),
        });
        self.cached_snapshot = Some(snapshot.clone());
        snapshot
    }
    pub fn remove_connection(&mut self, owner: u64) {
        self.watchers.remove(&owner);
    }
    pub fn set_locked(&mut self, locked: bool) {
        self.locked = locked;
        if locked {
            if let Some(publisher) = &self.publisher {
                publisher.revoke();
            }
            publication::locked(std::mem::take(&mut self.watchers).into_values().collect());
        }
    }
    fn deliver(&mut self, recipients: Vec<(u64, SyncSender<Response>)>) {
        if self.locked {
            publication::locked(recipients);
            return;
        }
        if recipients.is_empty() {
            return;
        }
        let snapshot = self.snapshot();
        if let Some(publisher) = &self.publisher {
            publisher.send(snapshot, recipients);
        } else {
            publication::unavailable(recipients);
        }
    }
    pub fn get(&mut self, request: u64, sender: SyncSender<Response>) {
        self.deliver(vec![(request, sender)]);
    }
    pub fn watch(&mut self, connection: u64, request: u64, since: u64, sender: SyncSender<Response>) {
        if self.locked || since != self.revision {
            self.get(request, sender);
        } else if self.watchers.len() < 32 {
            self.watchers.insert(connection, (request, sender));
        } else {
            let _ = sender.try_send(Response::error(
                request,
                "subscriber_limit",
                "Activity subscriber limit",
            ));
        }
    }
    pub fn owner_exit(&mut self, owner: Owner) {
        self.store.owner_exit(owner);
        self.rates.remove(&owner);
        self.changed();
    }
    pub fn changed(&mut self) {
        self.revision = self.revision.saturating_add(1);
        self.cached_snapshot = None;
        self.results
            .retain(|id, _| self.store.entries().any(|entry| &entry.id == id));
        self.windows
            .retain(|id, _| self.store.entries().any(|entry| &entry.id == id));
        let recipients = std::mem::take(&mut self.watchers).into_values().collect();
        self.deliver(recipients);
        self.schedule();
    }
    pub fn tick(&mut self) {
        self.store.advance(ferese_ipc::activities::now_ms());
        self.changed();
    }
    pub fn schedule(&self) {
        if let Some(monitor) = &self.monitor {
            monitor.retain(
                &self
                    .store
                    .entries()
                    .filter(|entry| entry.state == State::Running)
                    .map(|entry| entry.id.owner)
                    .collect(),
            );
        }
        if let Some(timer) = &self.timer {
            let now = ferese_ipc::activities::now_ms();
            timer.update(
                self.store
                    .next_deadline(now)
                    .map(|deadline| Instant::now() + Duration::from_millis(deadline.saturating_sub(now))),
            );
        }
    }
    pub fn publish_verified(
        &mut self,
        verified: crate::ipc::activity_owner::VerifiedOwner,
        publish: Publish,
        matched: Option<ferese_config::activities::Matched>,
        output: Option<String>,
        window: Option<u64>,
    ) -> Result<serde_json::Value, String> {
        // Admission and publication run in one compositor dispatch. A queued
        // request owns its pidfd, but is not yet subject to monitor pruning.
        let owner = self
            .monitor
            .as_ref()
            .ok_or("Activity owner monitor unavailable")?
            .register(verified)
            .map_err(|error| error.to_string())?;
        let result = self
            .publish_from(owner, publish, matched, output, window)
            .map_err(str::to_owned);
        self.schedule();
        result
    }

    pub fn publish(
        &mut self,
        owner: Owner,
        publish: Publish,
        matched: Option<ferese_config::activities::Matched>,
        output: Option<String>,
    ) -> Result<serde_json::Value, &'static str> {
        self.publish_from(owner, publish, matched, output, None)
    }

    fn publish_from(
        &mut self,
        owner: Owner,
        publish: Publish,
        matched: Option<ferese_config::activities::Matched>,
        output: Option<String>,
        window: Option<u64>,
    ) -> Result<serde_json::Value, &'static str> {
        let started = match &publish {
            Publish::Start { local, .. } | Publish::Run { local, .. } | Publish::Hook { local, .. } => {
                Some(local.clone())
            }
            _ => None,
        };
        let now = ferese_ipc::activities::now_ms();
        if matches!(publish, Publish::Hook { .. }) && matched.is_none() {
            return Ok(json!({"matched":false}));
        }
        self.rates.retain(|_, (at, _)| now.saturating_sub(*at) < 60_000);
        if self.rates.len() >= 128 && !self.rates.contains_key(&owner) {
            return Err("owner publication quota exceeded");
        }
        let rate = self.rates.entry(owner).or_insert((now, 0));
        if now.saturating_sub(rate.0) >= 1_000 {
            *rate = (now, 0);
        }
        // Each connection waits for acknowledgement; excess replaceable updates
        // are explicitly rejected so terminal events retain capacity.
        if (matches!(publish, Publish::Update { .. }) && rate.1 >= 30)
            || (matches!(
                publish,
                Publish::Start { .. } | Publish::Hook { .. } | Publish::Run { .. }
            ) && rate.1 >= 10)
        {
            return Err("publication rate exceeded");
        }
        rate.1 = rate.1.saturating_add(1);
        // Expiration is a change even when the following publication is rejected.
        // Notify subscribers before a store mutation can silently sweep leases.
        if self.store.advance(now) {
            self.changed();
        }
        let result = match publish {
            Publish::Start {
                local,
                mut data,
                lease_ms,
            } => {
                if data.output.is_none() {
                    data.output = output;
                }
                let id = self
                    .store
                    .start(owner, local, data, now, 0, lease_ms, true, true, None)?;
                json!({"handle":id})
            }
            Publish::Run { local, title, .. } => {
                if let Some(matched) = matched {
                    let data = Data {
                        source: if local == "process" {
                            Source::Process
                        } else {
                            Source::Command
                        },
                        output,
                        priority: matched.rule.priority,
                        icon: matched.rule.icon.clone(),
                        title: title.unwrap_or(matched.title),
                        value: Value::Indeterminate,
                        template: Template::Progress,
                    };
                    let id = self.store.start(
                        owner,
                        local,
                        data,
                        now,
                        matched.rule.min_duration_ms,
                        None,
                        true,
                        matched.rule.flash_on_fail,
                        matched.group,
                    )?;
                    self.results.insert(id.clone(), matched.rule.result);
                    json!({"handle":id,"progress":matched.rule.progress})
                } else {
                    let data = Data {
                        source: Source::Command,
                        output,
                        priority: 100,
                        icon: "ferese".into(),
                        title: title.unwrap_or_else(|| "Command".into()),
                        value: Value::Indeterminate,
                        template: Template::Progress,
                    };
                    let id = self.store.start(owner, local, data, now, 0, None, true, true, None)?;
                    json!({"handle":id,"progress":"indeterminate"})
                }
            }
            Publish::Hook { local, .. } => {
                let Some(matched) = matched else {
                    return Ok(json!({"matched":false}));
                };
                let data = Data {
                    source: if local == "process" {
                        Source::Process
                    } else {
                        Source::Command
                    },
                    output,
                    priority: matched.rule.priority,
                    icon: matched.rule.icon.clone(),
                    title: matched.title,
                    value: Value::Indeterminate,
                    template: Template::Progress,
                };
                let lease = (local == "process").then_some(5_000);
                let id = self.store.start(
                    owner,
                    local,
                    data,
                    now,
                    matched.rule.min_duration_ms,
                    lease,
                    true,
                    matched.rule.flash_on_fail,
                    matched.group,
                )?;
                self.results.insert(id.clone(), matched.rule.result);
                json!({"matched":true,"handle":id,"progress":matched.rule.progress})
            }
            Publish::Update {
                local,
                handle,
                value,
                lease_ms,
            } => {
                let id = self.resolve(owner, &local, handle)?;
                self.store.update(owner, &id, value, lease_ms, now)?;
                json!({})
            }
            Publish::End {
                local,
                handle,
                result,
                message,
            } => {
                let id = self.resolve(owner, &local, handle)?;
                let message = message.or_else(|| {
                    self.results.get(&id).map(|text| {
                        if result == State::Done {
                            text.ok.clone()
                        } else {
                            text.fail.clone()
                        }
                    })
                });
                self.store.finish(owner, &id, result, message, now)?;
                json!({})
            }
            Publish::Cancel { local, handle } => {
                let id = self.resolve(owner, &local, handle)?;
                self.store.finish(owner, &id, State::Cancelled, None, now)?;
                json!({})
            }
        };
        if let Some(local) = started
            && let Some(id) = self.store.resolve(owner, &local)
            && let Some(window) = window
        {
            self.windows.insert(id.clone(), window);
        }
        self.changed();
        Ok(result)
    }
    fn resolve(&self, owner: Owner, local: &str, handle: Option<Id>) -> Result<Id, &'static str> {
        match handle {
            Some(id) if id.local == local => Ok(id),
            Some(_) => Err("handle does not match local ID"),
            None => self
                .store
                .resolve(owner, local)
                .cloned()
                .ok_or("unknown owner-scoped activity"),
        }
    }
}

impl crate::Ferese {
    pub(crate) fn activity_window(&self, owner: &crate::ipc::activity_owner::VerifiedOwner) -> Option<u64> {
        use smithay::reexports::wayland_server::Resource;
        let mut ancestors = owner.ancestor_pids().peekable();
        ancestors.peek()?;
        let windows: Vec<_> = self
            .windows
            .records()
            .filter_map(|(id, record)| {
                let client = record.surface.as_ref()?.client()?;
                let pid = u32::try_from(client.get_credentials(&self.display_handle).ok()?.pid).ok()?;
                Some((pid, id.0))
            })
            .collect();
        origin_window(ancestors, &windows)
    }
}

fn origin_window(ancestors: impl Iterator<Item = u32>, windows: &[(u32, u64)]) -> Option<u64> {
    for pid in ancestors {
        let mut matches = windows.iter().filter(|(client, _)| *client == pid);
        if let Some((_, window)) = matches.next() {
            // A terminal server may own several windows. Never guess from focus.
            return matches.next().is_none().then_some(*window);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    fn owner() -> Owner {
        Owner { pid: 12, birth: 3 }
    }
    fn data() -> Data {
        Data {
            source: Source::Publisher,
            output: None,
            priority: 255,
            icon: "hammer".into(),
            title: "Build".into(),
            value: Value::Indeterminate,
            template: Template::Progress,
        }
    }
    #[test]
    fn session_lock_cancels_pending_and_rejects_new_activity_reads() {
        let mut engine = Engine::default();
        let revision = engine.snapshot().revision;
        let (send, receive) = std::sync::mpsc::sync_channel(1);
        engine.watch(1, 42, revision, send);
        engine.set_locked(true);
        let reply = receive.recv().unwrap();
        assert_eq!(reply.id, 42);
        assert_eq!(reply.error.unwrap().code, "session_locked");
        assert!(reply.result.is_none());
        engine.changed();
        for since in [revision, engine.revision] {
            let (send, receive) = std::sync::mpsc::sync_channel(1);
            engine.watch(2, 43, since, send);
            assert_eq!(receive.recv().unwrap().error.unwrap().code, "session_locked");
        }
        let (send, receive) = std::sync::mpsc::sync_channel(1);
        engine.get(44, send);
        assert_eq!(receive.recv().unwrap().error.unwrap().code, "session_locked");
        engine.set_locked(false);
        let (send, receive) = std::sync::mpsc::sync_channel(1);
        engine.watch(3, 45, revision, send);
        assert!(receive.recv_timeout(Duration::from_secs(2)).unwrap().result.is_some());
    }

    #[test]
    fn rejected_updates_do_not_leave_a_snapshot_from_before_lease_expiry() {
        let mut engine = Engine::default();
        let id = engine
            .store
            .start(
                owner(),
                "expired".into(),
                data(),
                ferese_ipc::activities::now_ms().saturating_sub(5_000),
                0,
                Some(1_000),
                true,
                true,
                None,
            )
            .unwrap();
        let before = engine.snapshot();
        assert_eq!(before.activities.len(), 1);
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        engine.watch(1, 11, before.revision, sender);
        assert!(
            engine
                .publish(
                    owner(),
                    Publish::Update {
                        local: "expired".into(),
                        handle: Some(id),
                        value: None,
                        lease_ms: None,
                    },
                    None,
                    None
                )
                .is_err()
        );
        assert!(engine.snapshot().activities.is_empty());
        let response = receiver.recv_timeout(Duration::from_secs(2)).unwrap();
        let snapshot: Snapshot = serde_json::from_value(response.result.unwrap()).unwrap();
        assert!(snapshot.activities.is_empty());
        assert!(snapshot.revision > before.revision);
        assert_eq!(before.activities.len(), 1);
    }

    #[test]
    fn snapshots_are_lazy_shared_and_immutable_across_publications() {
        let mut engine = Engine::default();
        engine.changed();
        assert!(engine.cached_snapshot.is_none(), "no subscriber needs a snapshot");
        let before = engine.snapshot();
        assert!(Arc::ptr_eq(&before, &engine.snapshot()));
        let (first, first_reply) = std::sync::mpsc::sync_channel(1);
        let (second, second_reply) = std::sync::mpsc::sync_channel(1);
        engine.watch(1, 11, before.revision, first);
        engine.watch(2, 22, before.revision, second);
        engine
            .publish(
                owner(),
                Publish::Start {
                    local: "build".into(),
                    data: data(),
                    lease_ms: None,
                },
                None,
                None,
            )
            .unwrap();
        let after = engine.snapshot();
        assert!(!Arc::ptr_eq(&before, &after));
        assert!(before.activities.is_empty());
        assert_eq!(after.activities.len(), 1);
        for (id, reply) in [(11, first_reply), (22, second_reply)] {
            let reply = reply.recv_timeout(Duration::from_secs(2)).unwrap();
            assert_eq!(reply.id, id);
            let snapshot: Snapshot = serde_json::from_value(reply.result.unwrap()).unwrap();
            assert_eq!(snapshot.revision, after.revision);
            assert_eq!(snapshot.activities[0].id, after.activities[0].id);
        }
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        engine.watch(3, 33, before.revision, sender);
        assert_eq!(receiver.recv_timeout(Duration::from_secs(2)).unwrap().id, 33);
        assert!(Arc::ptr_eq(&after, &engine.snapshot()));
    }

    #[test]
    fn process_that_exits_before_detection_dispatch_is_not_published() {
        let mut child = std::process::Command::new("sleep").arg("30").spawn().unwrap();
        let (owner, fd, _) = crate::ipc::activity_owner::inspect(child.id()).unwrap();
        let matched = ferese_config::activities::Matcher::new(&Default::default())
            .unwrap()
            .matches("cargo test", "/tmp", &[])
            .unwrap();
        child.kill().unwrap();
        child.wait().unwrap();
        let mut engine = Engine::default();
        engine.detect_process(owner, matched, None, &fd);
        assert_eq!(engine.store.entries().count(), 0);
    }

    #[test]
    fn rejected_process_detection_is_retried_without_restarting_admitted_records() {
        let mut engine = Engine::default();
        let (owner, fd, _) = crate::ipc::activity_owner::inspect(std::process::id()).unwrap();
        let settings = ferese_config::activities::Settings::default();
        let matched = ferese_config::activities::Matcher::new(&settings)
            .unwrap()
            .matches("cargo test", "/tmp/test", &[])
            .unwrap();
        engine.store.limits.max_total = 0;
        engine.detect_process(owner, matched.clone(), None, &fd);
        assert_eq!(engine.store.entries().count(), 0);
        engine.store.limits.max_total = 128;
        engine.detect_process(owner, matched.clone(), None, &fd);
        let admitted = engine.store.entries().next().unwrap().clone();
        engine.detect_process(owner, matched, None, &fd);
        let refreshed = engine.store.entries().next().unwrap();
        assert_eq!(refreshed.id, admitted.id);
        assert_eq!(refreshed.eligible_at, admitted.eligible_at);
        assert_eq!(engine.store.entries().count(), 1);
    }

    #[test]
    fn terminal_matching_uses_nearest_unique_ancestor_and_never_guesses() {
        let ancestry = [400, 300, 200, 100];
        assert_eq!(origin_window(ancestry.into_iter(), &[(200, 7), (100, 8)]), Some(7));
        assert_eq!(
            origin_window(ancestry.into_iter(), &[(200, 7), (200, 9), (100, 8)]),
            None
        );
        assert_eq!(origin_window(ancestry.into_iter(), &[(900, 7)]), None);
        assert_eq!(origin_window([].into_iter(), &[(200, 7)]), None);
    }

    #[test]
    fn activity_window_survives_updates_and_results_then_expires() {
        let mut engine = Engine::default();
        engine.store.limits.start_delay_ms = 0;
        engine
            .publish_from(
                owner(),
                Publish::Run {
                    local: "build".into(),
                    title: Some("Build".into()),
                    command: String::new(),
                    cwd: String::new(),
                },
                None,
                None,
                Some(42),
            )
            .unwrap();
        let id = engine.store.entries().next().unwrap().id.clone();
        assert_eq!(engine.snapshot().windows[0].window, 42);
        engine
            .publish(
                owner(),
                Publish::Update {
                    local: "build".into(),
                    handle: Some(id.clone()),
                    value: Some(Value::Progress(0.5)),
                    lease_ms: None,
                },
                None,
                None,
            )
            .unwrap();
        assert_eq!(engine.snapshot().windows[0].window, 42);
        engine
            .publish(
                owner(),
                Publish::End {
                    local: "build".into(),
                    handle: Some(id.clone()),
                    result: State::Done,
                    message: None,
                },
                None,
                None,
            )
            .unwrap();
        engine.owner_exit(owner());
        assert_eq!(engine.snapshot().windows[0].activity, id);
        engine.store.advance(ferese_ipc::activities::now_ms() + 2_000);
        engine.changed();
        assert!(engine.snapshot().windows.is_empty());
    }

    #[test]
    fn one_shot_disconnect_does_not_own_lifetime_and_rate_limit_preserves_completion() {
        let mut engine = Engine::default();
        let result = engine
            .publish(
                owner(),
                Publish::Start {
                    local: "build".into(),
                    data: data(),
                    lease_ms: None,
                },
                None,
                Some("first".into()),
            )
            .unwrap();
        let id: Id = serde_json::from_value(result["handle"].clone()).unwrap();
        engine.remove_connection(1);
        assert_eq!(engine.store.entries().count(), 1);
        for _ in 0..40 {
            let _ = engine.publish(
                owner(),
                Publish::Update {
                    local: "build".into(),
                    handle: Some(id.clone()),
                    value: Some(Value::Progress(0.5)),
                    lease_ms: None,
                },
                None,
                Some("other".into()),
            );
        }
        assert_eq!(
            engine.store.entries().next().unwrap().data.output.as_deref(),
            Some("first")
        );
        engine
            .publish(
                owner(),
                Publish::End {
                    local: "build".into(),
                    handle: Some(id),
                    result: State::Done,
                    message: None,
                },
                None,
                None,
            )
            .unwrap();
        assert_eq!(
            engine.store.entries().count(),
            0,
            "completion before delay never flashes"
        );
    }
    #[test]
    fn rule_is_pinned_across_atomic_reload_and_no_raw_metadata_is_published() {
        let settings = ferese_config::activities::Settings::default();
        let matched = ferese_config::activities::Matcher::new(&settings)
            .unwrap()
            .matches("cargo test --secret", "/secret/cwd", &[])
            .unwrap();
        let mut engine = Engine::default();
        engine
            .publish(
                owner(),
                Publish::Run {
                    local: "build".into(),
                    title: None,
                    command: String::new(),
                    cwd: String::new(),
                },
                Some(matched),
                Some("first".into()),
            )
            .unwrap();
        let before = engine.store.entries().next().unwrap().clone();
        let mut replacement = settings.clone();
        replacement.start_delay_ms = 5_000;
        replacement.rules[0].title = "Replacement".into();
        engine.configure(
            &replacement,
            ferese_config::activities::Matcher::new(&replacement).unwrap(),
        );
        let after = engine.store.entries().next().unwrap();
        assert_eq!(after.data.title, "cargo test");
        assert_eq!(after.eligible_at, before.eligible_at);
        let serialized = serde_json::to_string(engine.snapshot().as_ref()).unwrap();
        assert!(!serialized.contains("secret"));
    }
}
