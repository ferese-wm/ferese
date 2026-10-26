//! Client pacing constraints. Only observed display/callback progress releases FIFO waits.
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use calloop::RegistrationToken;
use calloop::timer::{TimeoutAction, Timer};
use smithay::backend::renderer::element::Id;
use smithay::output::Output;
use smithay::reexports::wayland_protocols::wp::commit_timing::v1::server::wp_commit_timer_v1::{self, WpCommitTimerV1};
use smithay::reexports::wayland_protocols::wp::commit_timing::v1::server::wp_commit_timing_manager_v1::WpCommitTimingManagerV1;
use smithay::reexports::wayland_protocols::wp::fifo::v1::server::wp_fifo_manager_v1::WpFifoManagerV1;
use smithay::reexports::wayland_protocols::wp::fifo::v1::server::wp_fifo_v1::WpFifoV1;
use smithay::reexports::wayland_server::backend::{ClientId, DisconnectReason};
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::{Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource, Weak};
use smithay::utils::{Clock, Monotonic, Time};
use smithay::wayland::commit_timing::{CommitTimerData, CommitTimerStateUserData, CommitTimingManagerState};
use smithay::wayland::compositor::{Barrier, Blocker, BlockerState, add_blocker, add_pre_commit_hook, is_sync_subsurface, with_states};
use smithay::wayland::fifo::{FifoBarrierCachedState, FifoCachedState, FifoManagerState};
use smithay::wayland::{Dispatch2, GlobalDispatch2};

use crate::Ferese;
use crate::state::ClientState;

const MAX_PENDING: usize = 128;
const MAX_CLIENT_PENDING: usize = 32;
const MAX_SURFACE_PENDING: usize = 8;
const HIDDEN_CADENCE: Duration = Duration::from_millis(17);

#[derive(Default)]
struct Budget {
    total: Arc<AtomicUsize>,
    clients: HashMap<ClientId, Arc<AtomicUsize>>,
}

struct Ticket {
    counts: [Arc<AtomicUsize>; 3],
}

impl Blocker for Ticket {
    fn state(&self) -> BlockerState {
        BlockerState::Released
    }
}

impl Drop for Ticket {
    fn drop(&mut self) {
        for count in &self.counts {
            count.fetch_sub(1, Ordering::Relaxed);
        }
    }
}

impl Budget {
    fn acquire(&mut self, client: ClientId, surface: Arc<AtomicUsize>) -> Option<Ticket> {
        self.clients
            .retain(|_, count| count.load(Ordering::Relaxed) != 0);
        let client = self.clients.entry(client).or_default().clone();
        let counts = [self.total.clone(), client, surface];
        let limits = [MAX_PENDING, MAX_CLIENT_PENDING, MAX_SURFACE_PENDING];
        let mut accepted = true;
        for (count, limit) in counts.iter().zip(limits) {
            accepted &= count.fetch_add(1, Ordering::Relaxed) < limit;
        }
        let ticket = Ticket { counts };
        accepted.then_some(ticket)
    }
}

#[derive(Clone, Default)]
struct ConstraintBlocker(Arc<AtomicU8>);

impl ConstraintBlocker {
    fn release(&self) {
        self.0.store(1, Ordering::Release);
    }

    fn cancel(&self) {
        self.0.store(2, Ordering::Release);
    }
}

impl Blocker for ConstraintBlocker {
    fn state(&self) -> BlockerState {
        match self.0.load(Ordering::Acquire) {
            0 => BlockerState::Pending,
            1 => BlockerState::Released,
            _ => BlockerState::Cancelled,
        }
    }
}

struct TimedCommit {
    surface: Weak<WlSurface>,
    client: Client,
    deadline: Duration,
    blocker: ConstraintBlocker,
}

struct FifoCommit {
    surface: Weak<WlSurface>,
    client: Client,
    fallback: Duration,
    barrier: Barrier,
}

pub(crate) struct PresentationConstraints {
    _fifo: FifoManagerState,
    timing: CommitTimingManagerState,
    budget: Budget,
    timed: Vec<TimedCommit>,
    fifo: Vec<FifoCommit>,
    notify: Vec<Client>,
    timer: Option<(RegistrationToken, Duration)>,
}

impl PresentationConstraints {
    pub fn new(display: &DisplayHandle) -> Self {
        Self {
            _fifo: FifoManagerState::new::<Ferese>(display),
            timing: CommitTimingManagerState::unmanaged::<Ferese>(display),
            budget: Budget::default(),
            timed: Vec::new(),
            fifo: Vec::new(),
            notify: Vec::new(),
            timer: None,
        }
    }

    /// A client needs wp_presentation.clock_id to interpret commit timestamps.
    pub fn disable_timing(&self, display: &DisplayHandle) {
        display.disable_global::<Ferese>(self.timing.global());
    }
}

fn queue_client(notify: &mut Vec<Client>, client: Client) {
    if !notify.iter().any(|queued| queued.id() == client.id()) {
        notify.push(client);
    }
}

fn now() -> Duration {
    Clock::<Monotonic>::new().now().into()
}

fn hidden_deadline(timestamp: Duration) -> Duration {
    // Share a cadence across hidden clients instead of waking once per surface's
    // staggered commit time. Hidden FIFO conditions may be cleared early.
    let remainder = Duration::from_nanos((timestamp.as_nanos() % HIDDEN_CADENCE.as_nanos()) as u64);
    timestamp.saturating_sub(remainder) + HIDDEN_CADENCE
}

pub(crate) fn install(surface: &WlSurface) {
    add_pre_commit_hook::<Ferese, _>(surface, |state, display, surface| {
        let (timestamp, fifo_wait, count) = with_states(surface, |states| {
            let timestamp = states
                .data_map
                .get::<CommitTimerStateUserData>()
                .and_then(|timer| timer.borrow_mut().timestamp.take());
            let wait = states
                .cached_state
                .get::<FifoCachedState>()
                .pending()
                .wait_barrier
                && states
                    .cached_state
                    .get::<FifoBarrierCachedState>()
                    .pending()
                    .barrier
                    .as_ref()
                    .is_some_and(|barrier| !barrier.is_signaled());
            let count = states
                .data_map
                .get_or_insert(Arc::<AtomicUsize>::default)
                .clone();
            (timestamp, wait, count)
        });
        let fifo_wait = fifo_wait && !is_sync_subsurface(surface);
        let timestamp = timestamp.map(|time| Duration::from(Time::<Monotonic>::from(time)));
        let timestamp = timestamp.filter(|deadline| *deadline > now());
        if !fifo_wait && timestamp.is_none() && count.load(Ordering::Relaxed) == 0 {
            return;
        }
        let Some(client) = surface.client() else {
            return;
        };
        let Some(ticket) = state
            .presentation_constraints
            .budget
            .acquire(client.id(), count)
        else {
            // Releasing a timestamp early would violate the client's not-before constraint.
            // Retire the abusive client instead of retaining an unbounded transaction queue.
            display
                .backend_handle()
                .kill_client(client.id(), DisconnectReason::ConnectionClosed);
            return;
        };
        add_blocker(surface, ticket);
        if let Some(deadline) = timestamp {
            let blocker = ConstraintBlocker::default();
            add_blocker(surface, blocker.clone());
            state.presentation_constraints.timed.push(TimedCommit {
                surface: surface.downgrade(),
                client,
                deadline,
                blocker,
            });
        }
    });
}

impl Ferese {
    pub(crate) fn presentation_constraint_committed(&mut self, surface: &WlSurface) {
        // A parent commit can apply synchronized descendants without a separate handler call.
        let mut surfaces = Vec::new();
        smithay::desktop::utils::with_surfaces_surface_tree(surface, |surface, _| {
            surfaces.push(surface.clone())
        });
        for surface in surfaces {
            let barrier = with_states(&surface, |states| {
                states
                    .cached_state
                    .get::<FifoBarrierCachedState>()
                    .current()
                    .barrier
                    .take()
            });
            let Some(barrier) = barrier.filter(|barrier| !barrier.is_signaled()) else {
                continue;
            };
            let Some(client) = surface.client() else {
                continue;
            };
            // The pending FIFO cache retains a barrier until wait_barrier consumes
            // it, so an ordinary commit can apply the same handle again. Preserve
            // its original pacing condition instead of treating it as a replacement.
            if self
                .presentation_constraints
                .fifo
                .iter()
                .any(|entry| entry.surface == surface.downgrade() && entry.barrier == barrier)
            {
                continue;
            }
            // Superseding FIFO state may release an earlier condition, as permitted by the protocol.
            self.presentation_constraints.fifo.retain(|entry| {
                if entry.surface == surface.downgrade() {
                    entry.barrier.signal();
                    queue_client(
                        &mut self.presentation_constraints.notify,
                        entry.client.clone(),
                    );
                    false
                } else {
                    true
                }
            });
            if self.presentation_constraints.fifo.len() == MAX_PENDING {
                barrier.signal();
                continue;
            }
            self.presentation_constraints.fifo.push(FifoCommit {
                surface: surface.downgrade(),
                client,
                fallback: hidden_deadline(now()),
                barrier,
            });
        }
    }

    pub(crate) fn presentation_constraint_destroyed(&mut self, surface: &WlSurface) {
        let weak = surface.downgrade();
        self.presentation_constraints.timed.retain(|entry| {
            if entry.surface == weak {
                entry.blocker.cancel();
                self.presentation_constraints
                    .notify
                    .push(entry.client.clone());
                false
            } else {
                true
            }
        });
        self.presentation_constraints.fifo.retain(|entry| {
            if entry.surface == weak {
                entry.barrier.signal();
                self.presentation_constraints
                    .notify
                    .push(entry.client.clone());
                false
            } else {
                true
            }
        });
    }

    pub(crate) fn presentation_constraint_frame(&mut self, output: &Output) {
        let eligible = self.callback_outputs();
        self.presentation_constraints.fifo.retain(|entry| {
            let visible = entry
                .surface
                .upgrade()
                .is_ok_and(|surface| eligible.get(&Id::from(&surface)) == Some(output));
            if visible {
                entry.barrier.signal();
                self.presentation_constraints
                    .notify
                    .push(entry.client.clone());
                false
            } else {
                true
            }
        });
    }

    pub(crate) fn presentation_constraint_captured(
        &mut self,
        mut surface_visible: impl FnMut(&WlSurface) -> bool,
    ) {
        self.presentation_constraints.fifo.retain(|entry| {
            let captured = entry
                .surface
                .upgrade()
                .is_ok_and(|surface| surface_visible(&surface));
            if captured {
                // Capture is sufficient for client forward progress when a nested
                // host withholds redraws. It is not physical presentation evidence.
                entry.barrier.signal();
                self.presentation_constraints
                    .notify
                    .push(entry.client.clone());
                false
            } else {
                true
            }
        });
    }

    pub(crate) fn presentation_constraint_capture_pending(
        &self,
        mut surface_visible: impl FnMut(&WlSurface) -> bool,
    ) -> bool {
        self.presentation_constraints.fifo.iter().any(|entry| {
            entry
                .surface
                .upgrade()
                .is_ok_and(|surface| surface_visible(&surface))
        })
    }

    pub(crate) fn poll_presentation_constraints(&mut self) {
        let timestamp = now();
        let eligible = self.callback_outputs();
        self.presentation_constraints.timed.retain(|entry| {
            if entry.surface.upgrade().is_err() {
                entry.blocker.cancel();
            } else if entry.deadline <= timestamp {
                entry.blocker.release();
            } else {
                return true;
            }
            self.presentation_constraints
                .notify
                .push(entry.client.clone());
            false
        });
        self.presentation_constraints.fifo.retain(|entry| {
            let hidden = entry
                .surface
                .upgrade()
                .map_or(true, |surface| !eligible.contains_key(&Id::from(&surface)));
            if entry.barrier.is_signaled() || hidden && entry.fallback <= timestamp {
                entry.barrier.signal();
                self.presentation_constraints
                    .notify
                    .push(entry.client.clone());
                false
            } else {
                true
            }
        });
        let deadline = self
            .presentation_constraints
            .timed
            .iter()
            .map(|entry| entry.deadline)
            .chain(
                self.presentation_constraints
                    .fifo
                    .iter()
                    .filter(|entry| {
                        entry
                            .surface
                            .upgrade()
                            .map_or(true, |surface| !eligible.contains_key(&Id::from(&surface)))
                    })
                    .map(|entry| entry.fallback),
            )
            .min();
        if self
            .presentation_constraints
            .timer
            .as_ref()
            .map(|(_, deadline)| *deadline)
            != deadline
        {
            if let Some((token, _)) = self.presentation_constraints.timer.take() {
                self.loop_handle.remove(token);
            }
            if let Some(deadline) = deadline {
                let timer = Timer::from_deadline(
                    Instant::now()
                        + deadline
                            .saturating_sub(timestamp)
                            .min(Duration::from_secs(86_400)),
                );
                match self.loop_handle.insert_source(timer, |_, _, state| {
                    state.presentation_constraints.timer = None;
                    state.poll_presentation_constraints();
                    TimeoutAction::Drop
                }) {
                    Ok(token) => self.presentation_constraints.timer = Some((token, deadline)),
                    Err(error) => {
                        tracing::error!(%error, "could not schedule client commit timing");
                        for entry in self.presentation_constraints.timed.drain(..) {
                            entry.blocker.cancel();
                            queue_client(&mut self.presentation_constraints.notify, entry.client);
                        }
                        for entry in self.presentation_constraints.fifo.drain(..) {
                            entry.barrier.signal();
                            queue_client(&mut self.presentation_constraints.notify, entry.client);
                        }
                    }
                }
            }
        }
        let clients = std::mem::take(&mut self.presentation_constraints.notify);
        let mut seen = Vec::new();
        for client in clients {
            if seen.contains(&client.id()) {
                continue;
            }
            seen.push(client.id());
            if let Some(data) = client.get_data::<ClientState>() {
                data.compositor_state
                    .blocker_cleared(self, &self.display_handle.clone());
            }
        }
    }
}

macro_rules! delegate {
    ($($interface:ty),+ $(,)?) => {$ (
        impl<U: Dispatch2<$interface, Ferese>> Dispatch<$interface, U> for Ferese {
            fn request(state: &mut Self, client: &Client, resource: &$interface, request: <$interface as Resource>::Request, data: &U, display: &DisplayHandle, init: &mut DataInit<'_, Self>) {
                data.request(state, client, resource, request, display, init);
            }
            fn destroyed(state: &mut Self, client: ClientId, resource: &$interface, data: &U) { data.destroyed(state, client, resource); }
        }
        impl<U: GlobalDispatch2<$interface, Ferese>> GlobalDispatch<$interface, U> for Ferese {
            fn bind(state: &mut Self, display: &DisplayHandle, client: &Client, resource: New<$interface>, data: &U, init: &mut DataInit<'_, Self>) { data.bind(state, display, client, resource, init); }
            fn can_view(client: Client, data: &U) -> bool { data.can_view(&client) }
        }
    )+};
}

delegate!(WpFifoManagerV1, WpFifoV1, WpCommitTimingManagerV1);

impl Dispatch<WpCommitTimerV1, CommitTimerData> for Ferese {
    fn request(
        state: &mut Self,
        client: &Client,
        resource: &WpCommitTimerV1,
        request: wp_commit_timer_v1::Request,
        data: &CommitTimerData,
        display: &DisplayHandle,
        init: &mut DataInit<'_, Self>,
    ) {
        if let wp_commit_timer_v1::Request::SetTimestamp {
            tv_sec_hi, tv_nsec, ..
        } = request
            && (tv_nsec >= 1_000_000_000 || tv_sec_hi > i32::MAX as u32)
        {
            resource.post_error(
                wp_commit_timer_v1::Error::InvalidTimestamp,
                "commit timestamp is outside the monotonic clock domain",
            );
            return;
        }
        data.request(state, client, resource, request, display, init);
    }
}

#[cfg(test)]
#[path = "presentation_constraints_tests.rs"]
pub(crate) mod protocol_tests;
