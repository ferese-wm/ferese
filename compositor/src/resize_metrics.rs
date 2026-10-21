//! Opt-in resize diagnostics. No measurement code exists without resize-metrics.
//! Times are unscaled wall time except handoff_animation, which is the existing
//! handoff clock (affected by animation speed and pauses).
use std::collections::HashMap;
use std::time::Duration;

use ferese_core::WorkspaceId;
use ferese_layout::WindowId;

const BUCKETS_MS: [u64; 16] = [
    16, 32, 64, 75, 80, 85, 100, 150, 250, 290, 299, 300, 301, 310, 350, 500,
];

#[derive(Debug, Default)]
struct Histogram {
    counts: [u64; 17],
}

impl Histogram {
    fn add(&mut self, duration: Duration) {
        let index = BUCKETS_MS.partition_point(|ms| Duration::from_millis(*ms) < duration);
        self.counts[index] += 1;
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum End {
    Commit,
    Deadline,
    Replaced,
    Cancelled,
}

#[derive(Default, Debug)]
struct Client {
    workspace: Option<WorkspaceId>,
    app_id: String,
    started: Option<Duration>,
    target_changed: Option<Duration>,
    last_observation: Duration,
    handoff_started: Option<Duration>,
    barriers: u64,
    deadlines: u64,
    commits: u64,
    replaced: u64,
    cancelled: u64,
    viewport_pauses: u64,
    other_window_pauses: u64,
    viewport_seen: bool,
    others_seen: bool,
    viewport_overlap: Duration,
    other_overlap: Duration,
    barrier_duration: Histogram,
    viewport_blocked: Histogram,
    other_windows_blocked: Histogram,
    target_to_handoff: Histogram,
    handoff_wall: Histogram,
    handoff_animation: Histogram,
}

#[derive(Default)]
pub(crate) struct ResizeMetrics {
    clients: HashMap<WindowId, Client>,
    workspace_started: HashMap<WorkspaceId, Duration>,
    workspace_duration: Histogram,
}

impl ResizeMetrics {
    pub(crate) fn begin(
        &mut self,
        id: WindowId,
        workspace: Option<WorkspaceId>,
        app_id: &str,
        now: Duration,
    ) {
        // Keep an overlapping workspace barrier continuous through retargets.
        let previous_workspace = self.end_client(id, now, End::Replaced);
        if previous_workspace != workspace {
            self.finish_workspace(previous_workspace, now);
        }

        let client = self.clients.entry(id).or_default();
        client.workspace = workspace;
        client.app_id.clear();
        client.app_id.push_str(app_id);
        client.started = Some(now);
        client.target_changed = Some(now);
        client.last_observation = now;
        client.handoff_started = None;
        client.viewport_seen = false;
        client.others_seen = false;
        client.viewport_overlap = Duration::ZERO;
        client.other_overlap = Duration::ZERO;
        client.barriers += 1;
        if let Some(workspace) = workspace {
            self.workspace_started.entry(workspace).or_insert(now);
        }
    }

    fn end_client(&mut self, id: WindowId, now: Duration, reason: End) -> Option<WorkspaceId> {
        let client = self.clients.get_mut(&id)?;
        let started = client.started.take()?;
        client.barrier_duration.add(now.saturating_sub(started));
        client.viewport_blocked.add(client.viewport_overlap);
        client.other_windows_blocked.add(client.other_overlap);
        match reason {
            End::Commit => client.commits += 1,
            End::Deadline => client.deadlines += 1,
            End::Replaced => client.replaced += 1,
            End::Cancelled => client.cancelled += 1,
        }
        client.workspace
    }

    pub(crate) fn end(&mut self, id: WindowId, now: Duration, reason: End) {
        let Some(workspace) = self.end_client(id, now, reason) else {
            return;
        };
        self.finish_workspace(Some(workspace), now);
    }

    fn finish_workspace(&mut self, workspace: Option<WorkspaceId>, now: Duration) {
        let Some(workspace) = workspace else { return };
        if !self
            .clients
            .values()
            .any(|client| client.workspace == Some(workspace) && client.started.is_some())
            && let Some(started) = self.workspace_started.remove(&workspace)
        {
            self.workspace_duration.add(now.saturating_sub(started));
        }
    }

    pub(crate) fn observe(
        &mut self,
        id: WindowId,
        now: Duration,
        viewport_active: bool,
        other_active: bool,
    ) {
        let Some(client) = self.clients.get_mut(&id) else {
            return;
        };
        if client.started.is_none() {
            return;
        }
        let interval = now.saturating_sub(client.last_observation);
        client.last_observation = now;
        if viewport_active {
            client.viewport_overlap += interval;
            if !client.viewport_seen {
                client.viewport_seen = true;
                client.viewport_pauses += 1;
            }
        }
        if other_active {
            client.other_overlap += interval;
            if !client.others_seen {
                client.others_seen = true;
                client.other_window_pauses += 1;
            }
        }
    }

    pub(crate) fn pause_clock(&mut self, now: Duration) {
        // Activity overlap excludes globally paused presentation time. Barrier
        // and target-to-handoff durations remain actual wall-clock durations.
        for client in self
            .clients
            .values_mut()
            .filter(|client| client.started.is_some())
        {
            client.last_observation = now;
        }
    }

    pub(crate) fn handoff_tick(&mut self, id: WindowId, now: Duration, blocked: bool) {
        let Some(client) = self.clients.get_mut(&id) else {
            return;
        };
        if !blocked && client.handoff_started.is_none() {
            client.handoff_started = Some(now);
            if let Some(changed) = client.target_changed.take() {
                client.target_to_handoff.add(now.saturating_sub(changed));
            }
        }
    }

    pub(crate) fn handoff_end(&mut self, id: WindowId, now: Duration, elapsed: Duration) {
        let Some(client) = self.clients.get_mut(&id) else {
            return;
        };
        if let Some(started) = client.handoff_started.take() {
            client.handoff_wall.add(now.saturating_sub(started));
            client.handoff_animation.add(elapsed);
        }
    }

    pub(crate) fn dump(&self) {
        // Enabling the feature explicitly opts into the exit report. This does
        // not depend on a tracing filter and adds no per-frame log messages.
        eprintln!(
            "resize-metrics: histogram upper bounds ms {BUCKETS_MS:?}, final bucket overflow"
        );
        eprintln!(
            "resize-metrics: workspace barrier durations {:?}",
            self.workspace_duration
        );
        for (id, client) in &self.clients {
            eprintln!("resize-metrics: client {id:?} {client:?}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overlapping_clients_deadlines_and_handoff_are_measured_separately() {
        let mut metrics = ResizeMetrics::default();
        let ws = Some(WorkspaceId(1));
        let ms = Duration::from_millis;
        metrics.begin(WindowId(1), ws, "terminal", ms(0));
        metrics.begin(WindowId(2), ws, "editor", ms(50));
        metrics.observe(WindowId(1), ms(100), true, true);
        metrics.observe(WindowId(1), ms(300), true, true);
        metrics.end(WindowId(1), ms(300), End::Deadline);
        assert!(metrics.workspace_started.contains_key(&WorkspaceId(1)));
        metrics.end(WindowId(2), ms(310), End::Commit);
        assert!(metrics.workspace_started.is_empty());
        metrics.handoff_tick(WindowId(1), ms(310), false);
        metrics.handoff_end(WindowId(1), ms(390), ms(80));
        let client = &metrics.clients[&WindowId(1)];
        assert_eq!(client.deadlines, 1);
        assert_eq!(client.viewport_pauses, 1);
        assert_eq!(client.other_window_pauses, 1);
        assert_eq!(client.viewport_overlap, ms(300));
        assert_eq!(client.other_overlap, ms(300));
        assert_eq!(client.barrier_duration.counts[11], 1);
        assert_eq!(client.handoff_animation.counts[4], 1);
        assert_eq!(client.handoff_wall.counts[4], 1);
        assert_eq!(metrics.workspace_duration.counts[13], 1);
    }

    #[test]
    fn globally_paused_time_is_not_counted_as_animation_blocking_overlap() {
        let mut metrics = ResizeMetrics::default();
        let ms = Duration::from_millis;
        metrics.begin(WindowId(1), Some(WorkspaceId(1)), "terminal", ms(0));
        metrics.observe(WindowId(1), ms(40), true, true);
        metrics.pause_clock(ms(340));
        metrics.observe(WindowId(1), ms(356), true, true);
        metrics.end(WindowId(1), ms(356), End::Deadline);
        let client = &metrics.clients[&WindowId(1)];
        assert_eq!(client.viewport_overlap, ms(56));
        assert_eq!(client.other_overlap, ms(56));
        assert_eq!(client.barrier_duration.counts[15], 1);
        assert_eq!(client.deadlines, 1);
    }
}
