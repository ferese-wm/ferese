//! Bound complete transactions, including ordinary commits behind an acquire.
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, Weak};

use smithay::reexports::wayland_server::backend::ClientId;
use smithay::reexports::wayland_server::{Resource, protocol::wl_surface::WlSurface};
use smithay::wayland::compositor::{Blocker, BlockerState, add_blocker, with_states};

use super::{AcquireGate, CANCELLED, MAX_PENDING, MAX_PER_CLIENT, MAX_PER_SURFACE};
use crate::Ferese;

#[derive(Default)]
struct SurfaceBudget {
    count: Arc<AtomicUsize>,
    tickets: Mutex<Vec<Weak<TicketInner>>>,
}

#[derive(Default)]
pub(super) struct CommitBudget {
    total: Arc<AtomicUsize>,
    clients: HashMap<ClientId, Weak<AtomicUsize>>,
}

struct Ticket {
    _lifetime: Arc<TicketInner>,
}
struct TicketInner {
    counters: [Arc<AtomicUsize>; 3],
    retired: AtomicBool,
}
impl TicketInner {
    fn retire(&self) {
        if !self.retired.swap(true, Ordering::AcqRel) {
            for counter in &self.counters {
                counter.fetch_sub(1, Ordering::AcqRel);
            }
        }
    }
}
impl Blocker for Ticket {
    fn state(&self) -> BlockerState {
        BlockerState::Released
    }
}
impl Drop for TicketInner {
    fn drop(&mut self) {
        self.retire();
    }
}

impl Ferese {
    pub(super) fn budget_buffer_transaction(&mut self, surface: &WlSurface) -> bool {
        let Some(client) = surface.client() else {
            return false;
        };
        let surface_count = with_states(surface, |states| {
            states
                .data_map
                .get_or_insert_threadsafe(SurfaceBudget::default)
                .count
                .clone()
        });
        let budget = &mut self.buffer_sync.budget;
        budget
            .clients
            .retain(|_, counter| counter.strong_count() != 0);
        let client_count = budget
            .clients
            .get(&client.id())
            .and_then(Weak::upgrade)
            .unwrap_or_else(|| {
                let count = Arc::new(AtomicUsize::new(0));
                budget.clients.insert(client.id(), Arc::downgrade(&count));
                count
            });
        let counters = [budget.total.clone(), client_count, surface_count];
        if counters
            .iter()
            .zip([MAX_PENDING, MAX_PER_CLIENT, MAX_PER_SURFACE])
            .any(|(count, limit)| count.load(Ordering::Acquire) >= limit)
        {
            let gate = AcquireGate::default();
            gate.0.store(CANCELLED, Ordering::Release);
            add_blocker(surface, gate);
            self.reject_buffer_client(&client, "too many transactions behind GPU acquire");
            return false;
        }
        for count in &counters {
            count.fetch_add(1, Ordering::AcqRel);
        }
        let ticket = Arc::new(TicketInner {
            counters,
            retired: AtomicBool::new(false),
        });
        with_states(surface, |states| {
            let counter = states
                .data_map
                .get::<SurfaceBudget>()
                .expect("admitted surface budget exists");
            let mut tickets = counter.tickets.lock().expect("surface budget poisoned");
            tickets.retain(|ticket| ticket.strong_count() != 0);
            tickets.push(Arc::downgrade(&ticket));
        });
        add_blocker(surface, Ticket { _lifetime: ticket });
        true
    }

    pub(super) fn retire_buffer_budget(&mut self, surface: &WlSurface) {
        with_states(surface, |states| {
            if let Some(counter) = states.data_map.get::<SurfaceBudget>() {
                for ticket in counter
                    .tickets
                    .lock()
                    .expect("surface budget poisoned")
                    .iter()
                    .filter_map(Weak::upgrade)
                {
                    ticket.retire();
                }
            }
        });
    }

    pub(super) fn budget_buffer_tail(&mut self, surface: &WlSurface) {
        let waiting = surface
            .client()
            .and_then(|client| {
                self.buffer_sync
                    .budget
                    .clients
                    .get(&client.id())
                    .and_then(Weak::upgrade)
            })
            .is_some_and(|counter| counter.load(Ordering::Acquire) != 0);
        // The affected transaction can include another synchronized surface.
        // Track its client's tail, rather than only the surface with the fence.
        if waiting {
            self.budget_buffer_transaction(surface);
        }
    }
}
