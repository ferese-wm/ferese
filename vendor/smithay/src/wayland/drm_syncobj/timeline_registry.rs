//! Imported timeline ownership follows all retained points, not protocol objects.
use std::sync::{Arc, Weak};

#[derive(Debug)]
pub(super) struct TimelineRegistry<Client, Timeline> {
    entries: Vec<(Client, Weak<Timeline>)>,
    total_limit: usize,
    client_limit: usize,
}

impl<Client: Eq, Timeline> TimelineRegistry<Client, Timeline> {
    pub fn new(total_limit: usize, client_limit: usize) -> Self {
        Self { entries: Vec::new(), total_limit, client_limit }
    }

    pub fn admit(&mut self, client: &Client) -> bool {
        self.prune();
        self.entries.len() < self.total_limit
            && self.entries.iter().filter(|(owner, _)| owner == client).count() < self.client_limit
    }

    pub fn track(&mut self, client: Client, timeline: &Arc<Timeline>) {
        self.entries.push((client, Arc::downgrade(timeline)));
    }

    pub fn prune(&mut self) {
        self.entries.retain(|(_, timeline)| timeline.strong_count() != 0);
    }

    pub fn live(&mut self) -> impl Iterator<Item = Arc<Timeline>> + '_ {
        self.prune();
        self.entries.iter().filter_map(|(_, timeline)| timeline.upgrade())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_caps_preserve_capacity_for_other_clients_and_reclaim_dead_imports() {
        let mut registry = TimelineRegistry::new(4, 2);
        let first = Arc::new(());
        let second = Arc::new(());
        assert!(registry.admit(&1));
        registry.track(1, &first);
        assert!(registry.admit(&1));
        registry.track(1, &second);
        assert!(!registry.admit(&1));
        assert!(registry.admit(&2));
        drop(first);
        assert!(registry.admit(&1));
        assert_eq!(registry.live().count(), 1);
    }

    #[test]
    fn protocol_object_destruction_does_not_forget_a_committed_point() {
        let mut registry = TimelineRegistry::new(1, 1);
        let protocol_object = Arc::new(());
        let committed_point = protocol_object.clone();
        registry.track(1, &protocol_object);
        drop(protocol_object);
        assert!(!registry.admit(&1));
        assert_eq!(registry.live().count(), 1);
        drop(committed_point);
        assert!(registry.admit(&1));
        assert_eq!(registry.live().count(), 0);
    }

    #[test]
    fn global_limit_is_independent_and_import_destroy_loops_do_not_grow_the_registry() {
        let mut registry = TimelineRegistry::new(2, 2);
        let first = Arc::new(());
        let second = Arc::new(());
        registry.track(1, &first);
        registry.track(2, &second);
        assert!(!registry.admit(&3));
        drop(first);
        drop(second);
        for _ in 0..10_000 {
            let timeline = Arc::new(());
            assert!(registry.admit(&1));
            registry.track(1, &timeline);
            drop(timeline);
        }
        assert!(registry.admit(&1));
        assert!(registry.entries.is_empty());
    }
}
