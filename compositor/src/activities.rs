//! Session broker: verified process identities, one-shot policy deadlines and
//! bounded long-poll subscribers. Process inspection runs on IPC workers.
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
    watchers: BTreeMap<u64, (u64, SyncSender<Response>)>,
    rates: BTreeMap<Owner, (u64, u16)>,
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
            watchers: BTreeMap::new(),
            rates: BTreeMap::new(),
            results: BTreeMap::new(),
        }
    }
}
impl Engine {
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
    pub fn snapshot(&self) -> Snapshot {
        Snapshot {
            version: ferese_ipc::activities::VERSION,
            revision: self.revision,
            now_ms: ferese_ipc::activities::now_ms(),
            limits: self.store.limits,
            activities: self.store.entries().cloned().collect(),
        }
    }
    pub fn remove_connection(&mut self, owner: u64) {
        self.watchers.remove(&owner);
    }
    pub fn watch(&mut self, connection: u64, request: u64, since: u64, sender: SyncSender<Response>) {
        if since != self.revision {
            let _ = sender.try_send(Response::success(request, json!(self.snapshot())));
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
        self.results
            .retain(|id, _| self.store.entries().any(|entry| &entry.id == id));
        let snapshot = json!(self.snapshot());
        for (_, (id, sender)) in std::mem::take(&mut self.watchers) {
            let _ = sender.try_send(Response::success(id, snapshot.clone()));
        }
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
    pub fn publish(
        &mut self,
        owner: Owner,
        publish: Publish,
        matched: Option<ferese_config::activities::Matched>,
        output: Option<String>,
    ) -> Result<serde_json::Value, &'static str> {
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
        let serialized = serde_json::to_string(&engine.snapshot()).unwrap();
        assert!(!serialized.contains("secret"));
    }
}
