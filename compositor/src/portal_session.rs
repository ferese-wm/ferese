use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::mpsc::SyncSender;
use std::time::{Duration, Instant};

use ferese_ipc::Response;
use serde::Deserialize;
use serde_json::{Value, json};
use smithay::reexports::calloop::timer::{TimeoutAction, Timer};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Inhibition {
    flags: u32,
    app: String,
    reason: String,
}

struct Query {
    token: u32,
    owner: Option<u64>,
    deadline: Instant,
    pending: HashSet<u64>,
}

pub(crate) struct PortalSession {
    inhibitors: HashMap<u64, Inhibition>,
    monitors: HashSet<u64>,
    waiters: HashMap<u64, (u64, SyncSender<Response>)>,
    phase: u32,
    locked: bool,
    revision: u64,
    inhibitor_revision: u32,
    next_query: u32,
    query: Option<Query>,
    transitions: VecDeque<Value>,
}

impl Default for PortalSession {
    fn default() -> Self {
        Self {
            inhibitors: HashMap::new(),
            monitors: HashSet::new(),
            waiters: HashMap::new(),
            phase: 1,
            locked: false,
            revision: 0,
            inhibitor_revision: 0,
            next_query: 0,
            query: None,
            transitions: VecDeque::new(),
        }
    }
}

impl PortalSession {
    pub(crate) fn register(&mut self, owner: u64, inhibition: Inhibition) -> Result<(), String> {
        if inhibition.flags == 0
            || inhibition.flags & !15 != 0
            || inhibition.app.len() > 512
            || inhibition.reason.len() > 1024
            || inhibition
                .app
                .chars()
                .chain(inhibition.reason.chars())
                .any(char::is_control)
        {
            return Err("Invalid session inhibition".into());
        }
        if self.inhibitors.len() >= 16 && !self.inhibitors.contains_key(&owner) {
            return Err("Session inhibition limit reached".into());
        }
        self.inhibitors.insert(owner, inhibition);
        self.inhibitor_revision = self.inhibitor_revision.wrapping_add(1);
        Ok(())
    }

    pub(crate) fn register_monitor(&mut self, owner: u64) -> Result<(), String> {
        if self.monitors.len() >= 8 && !self.monitors.contains(&owner) {
            return Err("Session monitor limit reached".into());
        }
        self.monitors.insert(owner);
        if let Some(query) = &mut self.query
            && self.phase == 2
        {
            query.pending.insert(owner);
        }
        Ok(())
    }

    pub(crate) fn acknowledge(&mut self, owner: u64, token: u32) -> Result<(), String> {
        if !self.monitors.contains(&owner) {
            return Err("Unknown session monitor".into());
        }
        if let Some(query) = &mut self.query
            && query.token == token
            && self.phase == 2
            && query.pending.remove(&owner)
        {
            self.changed();
        }
        Ok(())
    }

    pub(crate) fn remove(&mut self, owner: u64) {
        self.waiters.remove(&owner);
        if self.phase == 2
            && self
                .query
                .as_ref()
                .is_some_and(|query| query.owner == Some(owner))
        {
            self.set_phase(1);
        }
        if self.inhibitors.remove(&owner).is_some() {
            self.inhibitor_revision = self.inhibitor_revision.wrapping_add(1);
        }
        let monitor_removed = self.monitors.remove(&owner);
        let acknowledged = self
            .query
            .as_mut()
            .is_some_and(|query| query.pending.remove(&owner));
        if monitor_removed || acknowledged {
            self.changed();
        }
    }

    pub(crate) fn idle_inhibited(&self) -> bool {
        self.inhibitors
            .values()
            .any(|inhibitor| inhibitor.flags & 8 != 0)
    }

    pub(crate) fn begin_query(&mut self) -> u32 {
        self.next_query = self.next_query.wrapping_add(1).max(1);
        let token = self.next_query;
        self.query = Some(Query {
            token,
            owner: None,
            deadline: Instant::now() + Duration::from_secs(1),
            pending: self.monitors.clone(),
        });
        self.phase = 2;
        self.changed();
        token
    }

    pub(crate) fn begin_owned_query(&mut self, owner: u64) -> u32 {
        let token = self.begin_query();
        self.query
            .as_mut()
            .expect("begin_query installs the query before setting its owner")
            .owner = Some(owner);
        token
    }

    pub(crate) fn cancel_query(&mut self, token: u32) {
        if self
            .query
            .as_ref()
            .is_some_and(|query| query.token == token)
        {
            self.set_phase(1);
        }
    }

    pub(crate) fn query_ready(&self) -> bool {
        self.phase == 2
            && self
                .query
                .as_ref()
                .is_some_and(|query| query.pending.is_empty() || Instant::now() >= query.deadline)
    }

    pub(crate) fn validate_end(
        &self,
        token: u32,
        inhibitor_revision: u32,
        force: bool,
    ) -> Result<(), String> {
        if !self.query_ready() || self.query.as_ref().is_none_or(|query| query.token != token) {
            return Err(
                "Session-ending confirmation expired or applications are still responding".into(),
            );
        }
        if self.inhibitor_revision != inhibitor_revision
            || (!force
                && self
                    .inhibitors
                    .values()
                    .any(|inhibitor| inhibitor.flags & 1 != 0))
        {
            return Err(
                "Applications changed their inhibitors; review the confirmation again".into(),
            );
        }
        Ok(())
    }

    pub(crate) fn commit_end(
        &mut self,
        token: u32,
        inhibitor_revision: u32,
        force: bool,
    ) -> Result<(), String> {
        self.validate_end(token, inhibitor_revision, force)?;
        self.set_phase(3);
        Ok(())
    }

    pub(crate) fn set_phase(&mut self, phase: u32) {
        if phase == 2 {
            self.begin_query();
            return;
        }
        if phase == 1 {
            self.query = None;
        }
        if self.phase != phase {
            self.phase = phase;
            self.changed();
        }
    }

    pub(crate) fn set_locked(&mut self, locked: bool) {
        if self.locked != locked {
            self.locked = locked;
            self.changed();
        }
    }

    fn transition(&self) -> Value {
        json!({"screensaver-active": self.locked, "session-state": self.phase, "revision": self.revision, "query-token": self.query.as_ref().map_or(0, |query| query.token)})
    }

    fn changed(&mut self) {
        self.revision = self.revision.wrapping_add(1);
        if self.transitions.len() == 64 {
            self.transitions.pop_front();
        }
        self.transitions.push_back(self.transition());
        let snapshot = self.snapshot();
        for (_, (id, response)) in self.waiters.drain() {
            let _ = response.try_send(Response::success(id, snapshot.clone()));
        }
    }

    pub(crate) fn watch(
        &mut self,
        owner: u64,
        id: u64,
        since: u64,
        response: SyncSender<Response>,
    ) {
        if since != self.revision {
            let _ = response.try_send(Response::success(id, self.snapshot()));
        } else {
            self.waiters.insert(owner, (id, response));
        }
    }

    pub(crate) fn snapshot(&self) -> Value {
        let inhibitors = self
            .inhibitors
            .values()
            .map(|inhibitor| json!({"flags": inhibitor.flags, "app": inhibitor.app, "reason": inhibitor.reason}))
            .collect::<Vec<_>>();
        let remaining = self.query.as_ref().map_or(0, |query| {
            query
                .deadline
                .saturating_duration_since(Instant::now())
                .as_millis() as u64
        });
        json!({"screensaver-active": self.locked, "session-state": self.phase, "revision": self.revision,
            "inhibitors": inhibitors, "inhibitor-revision": self.inhibitor_revision,
            "query-token": self.query.as_ref().map_or(0, |query| query.token), "query-ready": self.query_ready(),
            "query-remaining-ms": remaining, "transitions": self.transitions, "monitor-owners": self.monitors})
    }
}

impl crate::Ferese {
    pub(crate) fn end_portal_session(&mut self) {
        let token = self.portal_session.query.as_ref().map(|query| query.token);
        let result = self.loop_handle.insert_source(
            Timer::from_duration(Duration::from_millis(200)),
            move |_, _, state| {
                if state.portal_session.phase == 3
                    && state.portal_session.query.as_ref().map(|query| query.token) == token
                {
                    state.loop_signal.stop();
                }
                TimeoutAction::Drop
            },
        );
        if let Err(error) = result {
            tracing::error!(%error, "failed to schedule session ending");
            self.loop_signal.stop();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inhibition(flags: u32) -> Inhibition {
        Inhibition {
            flags,
            app: "test.app".into(),
            reason: "Playing media".into(),
        }
    }

    #[test]
    fn inhibition_is_connection_scoped_and_invalid_flags_do_not_replace_it() {
        let mut session = PortalSession::default();
        session.register(1, inhibition(8)).unwrap();
        session.register(2, inhibition(8)).unwrap();
        assert!(session.register(1, inhibition(16)).is_err());
        session.remove(1);
        assert!(session.idle_inhibited());
        session.remove(2);
        assert!(!session.idle_inhibited());
    }

    #[test]
    fn acknowledgements_are_generation_scoped_and_disconnect_releases_pending_monitor() {
        let mut session = PortalSession::default();
        session.register_monitor(1).unwrap();
        session.register_monitor(2).unwrap();
        let old = session.begin_query();
        assert!(!session.query_ready());
        let token = session.begin_query();
        session.acknowledge(1, old).unwrap();
        assert!(!session.query_ready());
        session.acknowledge(1, token).unwrap();
        assert!(!session.query_ready());
        session.remove(2);
        assert!(session.query_ready());
        session.cancel_query(old);
        assert!(session.query_ready());
        session.cancel_query(token);
        assert!(!session.query_ready());
    }

    #[test]
    fn late_inhibitor_requires_a_new_confirmation_and_force_is_explicit() {
        let mut session = PortalSession::default();
        let token = session.begin_query();
        let approved = session.inhibitor_revision;
        session.register(1, inhibition(1)).unwrap();
        assert!(session.commit_end(token, approved, true).is_err());
        assert!(
            session
                .commit_end(token, session.inhibitor_revision, false)
                .is_err()
        );
        session
            .commit_end(token, session.inhibitor_revision, true)
            .unwrap();
        assert_eq!(session.snapshot()["session-state"], 3);
        assert!(
            session
                .commit_end(token, session.inhibitor_revision, true)
                .is_err()
        );
    }

    #[test]
    fn validation_keeps_query_open_and_owner_disconnect_cancels_it() {
        let mut session = PortalSession::default();
        let token = session.begin_owned_query(9);
        session.validate_end(token, 0, false).unwrap();
        assert_eq!(session.snapshot()["session-state"], 2);
        session.remove(9);
        assert_eq!(session.snapshot()["session-state"], 1);
        assert!(session.validate_end(token, 0, false).is_err());
    }

    #[test]
    fn old_logout_token_does_not_cancel_a_new_shutdown_query() {
        let mut session = PortalSession::default();
        let logout = session.begin_query();
        let shutdown = session.begin_owned_query(9);
        session.cancel_query(logout);
        session.validate_end(shutdown, 0, false).unwrap();
    }

    #[test]
    fn unresponsive_monitors_have_a_one_second_deadline() {
        let mut session = PortalSession::default();
        session.register_monitor(1).unwrap();
        session.begin_query();
        assert!(!session.query_ready());
        session.query.as_mut().unwrap().deadline = Instant::now() - Duration::from_millis(1);
        assert!(session.query_ready());
    }

    #[test]
    fn state_watch_preserves_fast_transitions_and_stops_on_disconnect() {
        let mut session = PortalSession::default();
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        session.watch(1, 10, 0, sender);
        assert!(receiver.try_recv().is_err());
        session.set_locked(true);
        let response = receiver.try_recv().unwrap();
        assert_eq!(response.result.unwrap()["screensaver-active"], true);
        session.set_locked(false);
        assert_eq!(
            session.snapshot()["transitions"].as_array().unwrap().len(),
            2
        );
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        session.watch(1, 11, session.revision, sender);
        session.remove(1);
        assert!(receiver.recv().is_err());
    }
}
