//! Surface commits wait for GPU acquire readiness without blocking the compositor.
use std::collections::HashMap;
use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::Duration;

use calloop::timer::{TimeoutAction, Timer};
use calloop::{
    EventSource, Interest, Poll, PostAction, Readiness, RegistrationToken, Token, TokenFactory,
};
use smithay::backend::drm::DrmNode;
use smithay::reexports::wayland_server::backend::{ClientId, DisconnectReason};
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::{Client, Resource, Weak};
use smithay::wayland::compositor::{
    Blocker, BlockerState, BufferAssignment, SurfaceAttributes, add_blocker, add_pre_commit_hook,
    with_states,
};
use smithay::wayland::dmabuf::get_dmabuf;

use crate::Ferese;
use crate::state::ClientState;

const MAX_PENDING: usize = 128;
const MAX_PER_CLIENT: usize = 32;
const MAX_PER_SURFACE: usize = 8;
const MAX_WAIT: Duration = Duration::from_secs(5);
const PENDING: u8 = 0;
const RELEASED: u8 = 1;
const CANCELLED: u8 = 2;

#[derive(Clone)]
struct AcquireGate(Arc<AtomicU8>);

impl Default for AcquireGate {
    fn default() -> Self {
        Self(Arc::new(AtomicU8::new(PENDING)))
    }
}

impl Blocker for AcquireGate {
    fn state(&self) -> BlockerState {
        match self.0.load(Ordering::Acquire) {
            RELEASED => BlockerState::Released,
            CANCELLED => BlockerState::Cancelled,
            _ => BlockerState::Pending,
        }
    }
}

struct Pending {
    surface: Weak<WlSurface>,
    client: Client,
    gpu: Option<DrmNode>,
    gate: AcquireGate,
    readiness: RegistrationToken,
    timeout: RegistrationToken,
}

#[derive(Default)]
pub(crate) struct BufferSync {
    next: u64,
    pending: HashMap<u64, Pending>,
    notify: Vec<Client>,
}

impl Drop for BufferSync {
    fn drop(&mut self) {
        for entry in self.pending.values() {
            entry.gate.0.store(CANCELLED, Ordering::Release);
        }
    }
}

pub(crate) fn install(surface: &WlSurface) {
    add_pre_commit_hook::<Ferese, _>(surface, |state, _, surface| {
        let buffer = with_states(surface, |states| {
            let mut attrs = states.cached_state.get::<SurfaceAttributes>();
            match &attrs.pending().buffer {
                Some(BufferAssignment::NewBuffer(buffer)) => Some(buffer.clone()),
                _ => None,
            }
        });
        let Some(buffer) = buffer else {
            return;
        };
        let Ok(dmabuf) = get_dmabuf(&buffer) else {
            return;
        };
        // Import success only validates format/storage. Implicit write fences
        // can still be pending each time the same buffer is committed.
        if let Ok((_, source)) = dmabuf.generate_blocker(Interest::READ) {
            state.wait_for_buffer(surface, source, dmabuf.node(), MAX_WAIT);
        }
    });
}

/// Convert errors from an individual acquire watcher into cancellation of that
/// client's transaction instead of failing the entire compositor event loop.
struct AcquireSource<S>(S);

impl<S> EventSource for AcquireSource<S>
where
    S: EventSource<Event = (), Ret = Result<(), io::Error>>,
    S::Error: std::fmt::Display,
{
    type Event = Result<(), String>;
    type Metadata = ();
    type Ret = ();
    type Error = io::Error;

    fn process_events<F>(
        &mut self,
        readiness: Readiness,
        token: Token,
        mut callback: F,
    ) -> Result<PostAction, Self::Error>
    where
        F: FnMut(Self::Event, &mut Self::Metadata) -> Self::Ret,
    {
        let mut completed = false;
        match self.0.process_events(readiness, token, |_, _| {
            completed = true;
            callback(Ok(()), &mut ());
            Ok(())
        }) {
            Ok(action) => Ok(action),
            Err(error) => {
                if !completed {
                    callback(Err(error.to_string()), &mut ());
                }
                Ok(PostAction::Remove)
            }
        }
    }

    fn register(&mut self, poll: &mut Poll, factory: &mut TokenFactory) -> calloop::Result<()> {
        self.0.register(poll, factory)
    }
    fn reregister(&mut self, poll: &mut Poll, factory: &mut TokenFactory) -> calloop::Result<()> {
        self.0.reregister(poll, factory)
    }
    fn unregister(&mut self, poll: &mut Poll) -> calloop::Result<()> {
        self.0.unregister(poll)
    }
}

#[derive(Clone, Copy)]
enum Completion {
    Ready,
    Timeout,
    Removed,
    Failed,
}

impl Ferese {
    fn wait_for_buffer<S>(
        &mut self,
        surface: &WlSurface,
        source: S,
        gpu: Option<DrmNode>,
        timeout: Duration,
    ) where
        S: EventSource<Event = (), Ret = Result<(), io::Error>> + 'static,
        S::Error: std::fmt::Display,
    {
        let Some(client) = surface.client() else {
            return;
        };
        let gate = AcquireGate::default();
        add_blocker(surface, gate.clone());
        let owned = self
            .buffer_sync
            .pending
            .values()
            .filter(|entry| entry.client.id() == client.id())
            .count();
        let on_surface = self
            .buffer_sync
            .pending
            .values()
            .filter(|entry| entry.surface == surface.downgrade())
            .count();
        if self.buffer_sync.pending.len() >= MAX_PENDING
            || owned >= MAX_PER_CLIENT
            || on_surface >= MAX_PER_SURFACE
        {
            gate.0.store(CANCELLED, Ordering::Release);
            self.reject_buffer_client(&client, "too many pending GPU acquire waits");
            return;
        }

        let id = self.buffer_sync.next;
        self.buffer_sync.next = self
            .buffer_sync
            .next
            .checked_add(1)
            .expect("GPU acquire ID space exhausted");
        let readiness =
            match self
                .loop_handle
                .insert_source(AcquireSource(source), move |result, _, state| {
                    if let Err(error) = &result {
                        tracing::warn!(%error, "GPU acquire readiness failed");
                    }
                    state.finish_buffer_wait(
                        id,
                        if result.is_ok() {
                            Completion::Ready
                        } else {
                            Completion::Failed
                        },
                    );
                }) {
                Ok(token) => token,
                Err(error) => {
                    gate.0.store(CANCELLED, Ordering::Release);
                    tracing::warn!(%error, "could not register GPU acquire wait");
                    self.reject_buffer_client(&client, "GPU acquire registration failed");
                    return;
                }
            };
        let deadline = match self.loop_handle.insert_source(
            Timer::from_duration(timeout),
            move |_, _, state| {
                state.finish_buffer_wait(id, Completion::Timeout);
                TimeoutAction::Drop
            },
        ) {
            Ok(token) => token,
            Err(error) => {
                self.loop_handle.remove(readiness);
                gate.0.store(CANCELLED, Ordering::Release);
                tracing::warn!(%error, "could not register GPU acquire deadline");
                self.reject_buffer_client(&client, "GPU acquire deadline registration failed");
                return;
            }
        };
        self.buffer_sync.pending.insert(
            id,
            Pending {
                surface: surface.downgrade(),
                client,
                gpu,
                gate,
                readiness,
                timeout: deadline,
            },
        );
    }

    fn reject_buffer_client(&mut self, client: &Client, reason: &str) {
        tracing::warn!(%reason, "disconnecting client with an unusable GPU buffer");
        self.display_handle
            .backend_handle()
            .kill_client(client.id(), DisconnectReason::ConnectionClosed);
        self.queue_buffer_notification(client.clone());
        self.cancel_buffer_owner(client.id());
    }

    fn queue_buffer_notification(&mut self, client: Client) {
        if !self
            .buffer_sync
            .notify
            .iter()
            .any(|other| other.id() == client.id())
        {
            self.buffer_sync.notify.push(client);
        }
    }

    fn finish_buffer_wait(&mut self, id: u64, completion: Completion) {
        let Some(entry) = self.buffer_sync.pending.remove(&id) else {
            return;
        };
        match completion {
            Completion::Ready => self.loop_handle.remove(entry.timeout),
            Completion::Timeout => self.loop_handle.remove(entry.readiness),
            Completion::Removed | Completion::Failed => {
                self.loop_handle.remove(entry.readiness);
                self.loop_handle.remove(entry.timeout);
            }
        }
        let ready = matches!(completion, Completion::Ready) && entry.surface.upgrade().is_ok();
        entry
            .gate
            .0
            .store(if ready { RELEASED } else { CANCELLED }, Ordering::Release);
        if matches!(completion, Completion::Timeout | Completion::Failed) {
            self.reject_buffer_client(&entry.client, "GPU acquire fence did not become usable");
            self.cancel_buffer_owner(entry.client.id());
        }
        self.queue_buffer_notification(entry.client);
    }

    fn cancel_buffer_owner(&mut self, owner: ClientId) {
        let ids: Vec<_> = self
            .buffer_sync
            .pending
            .iter()
            .filter_map(|(id, entry)| (entry.client.id() == owner).then_some(*id))
            .collect();
        for id in ids {
            self.finish_buffer_wait(id, Completion::Removed);
        }
    }

    pub(crate) fn cancel_surface_buffer_waits(&mut self, surface: &WlSurface) {
        let weak = surface.downgrade();
        let ids: Vec<_> = self
            .buffer_sync
            .pending
            .iter()
            .filter_map(|(id, entry)| (entry.surface == weak).then_some(*id))
            .collect();
        for id in ids {
            self.finish_buffer_wait(id, Completion::Removed);
        }
    }

    pub(crate) fn cancel_gpu_buffer_waits(&mut self, gpu: DrmNode) {
        let clients: Vec<_> = self
            .buffer_sync
            .pending
            .values()
            .filter(|entry| entry.gpu == Some(gpu))
            .map(|entry| entry.client.clone())
            .collect();
        for client in clients {
            self.reject_buffer_client(&client, "GPU removed during acquire wait");
            self.cancel_buffer_owner(client.id());
        }
    }

    pub(crate) fn poll_buffer_sync(&mut self) {
        // Never reenter surface/transaction locks from a pre-commit/destruction
        // callback. Promotion runs after the current event-loop dispatch.
        let clients = std::mem::take(&mut self.buffer_sync.notify);
        for client in clients {
            if let Some(data) = client.get_data::<ClientState>() {
                data.compositor_state
                    .blocker_cleared(self, &self.display_handle.clone());
            }
        }
    }
}

#[cfg(test)]
mod tests;
