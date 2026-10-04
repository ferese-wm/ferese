use std::collections::{HashSet, VecDeque};
use std::time::{Duration, Instant};

use smithay::backend::allocator::dmabuf::Dmabuf;
use smithay::backend::drm::DrmNode;
use smithay::backend::renderer::ImportDma;
use smithay::reexports::calloop::RegistrationToken;
use smithay::reexports::calloop::timer::{TimeoutAction, Timer};
use smithay::reexports::wayland_server::backend::ClientId;
use smithay::wayland::dmabuf::{DmabufGlobal, ImportNotifier};

use crate::Ferese;

const MAX_PENDING: usize = 64;
const MAX_PER_CLIENT: usize = 16;
const MAX_WAIT: Duration = Duration::from_secs(5);

struct Pending<K, C, T> {
    owner: K,
    client: C,
    deadline: Instant,
    value: T,
}

struct Queue<K, C, T> {
    pending: VecDeque<Pending<K, C, T>>,
}

impl<K, C, T> Default for Queue<K, C, T> {
    fn default() -> Self {
        Self {
            pending: VecDeque::new(),
        }
    }
}

impl<K: Eq, C: Eq, T> Queue<K, C, T> {
    fn enqueue(&mut self, owner: K, client: C, value: T, now: Instant) -> Result<(), T> {
        if self.pending.len() >= MAX_PENDING
            || self.pending.iter().filter(|entry| entry.client == client).count() >= MAX_PER_CLIENT
        {
            return Err(value);
        }

        self.pending.push_back(Pending {
            owner,
            client,
            value,
            deadline: now + MAX_WAIT,
        });
        Ok(())
    }

    fn process(&mut self, owner: &K, mut complete: impl FnMut(T)) {
        self.remove_if(|entry| &entry.owner == owner, |entry| complete(entry.value));
    }

    fn remove_if(
        &mut self,
        mut remove: impl FnMut(&Pending<K, C, T>) -> bool,
        mut complete: impl FnMut(Pending<K, C, T>),
    ) {
        for _ in 0..self.pending.len() {
            let entry = self.pending.pop_front().unwrap();
            if remove(&entry) {
                complete(entry);
            } else {
                self.pending.push_back(entry);
            }
        }
    }

    fn deadline(&self) -> Option<Instant> {
        self.pending.front().map(|entry| entry.deadline)
    }
}

struct Request {
    buffer: Dmabuf,
    notifier: ImportNotifier,
}

#[derive(Default)]
pub(crate) struct ImportManager {
    globals: HashSet<DmabufGlobal>,
    queue: Queue<DmabufGlobal, ClientId, Request>,
    timer: Option<RegistrationToken>,
}

impl Drop for ImportManager {
    fn drop(&mut self) {
        self.queue.remove_if(|_| true, |entry| entry.value.notifier.failed());
    }
}

impl ImportManager {
    pub(crate) fn register(&mut self, global: DmabufGlobal) {
        self.globals.insert(global);
    }

    pub(crate) fn remove(&mut self, global: DmabufGlobal) {
        self.globals.remove(&global);
        self.queue.process(&global, |request| request.notifier.failed());
    }

    pub(crate) fn enqueue(&mut self, global: DmabufGlobal, buffer: Dmabuf, notifier: ImportNotifier, now: Instant) {
        let Some(client) = notifier.client() else {
            notifier.failed();
            return;
        };

        if !self.globals.contains(&global) {
            notifier.failed();
            return;
        }

        if let Err(request) = self
            .queue
            .enqueue(global, client.id(), Request { buffer, notifier }, now)
        {
            request.notifier.failed();
        }
    }

    pub(crate) fn process<R: ImportDma>(&mut self, global: DmabufGlobal, renderer: &mut R, node: Option<DrmNode>) {
        self.process_with(global, node, |buffer| renderer.import_dmabuf(buffer, None).is_ok());
    }

    pub(crate) fn process_with(
        &mut self,
        global: DmabufGlobal,
        node: Option<DrmNode>,
        mut import: impl FnMut(&Dmabuf) -> bool,
    ) {
        self.queue.process(&global, |request| {
            if request.notifier.client().is_none() {
                request.notifier.failed();
                return;
            }

            if !import(&request.buffer) {
                request.notifier.failed();
                return;
            }

            if let Some(node) = node {
                request.buffer.set_node(node);
            }

            if let Err(error) = request.notifier.successful::<Ferese>() {
                tracing::debug!(?error, "dma-buf client disappeared before import completed");
            }
        });
    }

    pub(crate) fn expire(&mut self, now: Instant) {
        self.queue.remove_if(
            |entry| entry.deadline <= now || entry.value.notifier.client().is_none(),
            |entry| entry.value.notifier.failed(),
        );
    }
}

impl Ferese {
    pub(crate) fn process_pending_dmabuf_imports(&mut self) {
        self.dmabuf_imports.expire(Instant::now());
        if self.dmabuf_imports.queue.deadline().is_some() {
            if let Some(backend) = self.direct_backend.as_mut() {
                backend.process_dmabuf_imports(&mut self.dmabuf_imports);
            } else if let Some(backend) = self.nested_backend.as_ref() {
                let mut backend = backend.borrow_mut();
                // Nested Ferese publishes exactly one DMA-BUF global.
                if let Some(global) = self.dmabuf_imports.globals.iter().next().copied() {
                    self.dmabuf_imports.process(global, backend.renderer(), None);
                }
            }
        }

        if let Some(deadline) = self.dmabuf_imports.queue.deadline() {
            if self.dmabuf_imports.timer.is_none() {
                match self
                    .loop_handle
                    .insert_source(Timer::from_deadline(deadline), |_, _, state| {
                        state.dmabuf_imports.expire(Instant::now());
                        if let Some(deadline) = state.dmabuf_imports.queue.deadline() {
                            TimeoutAction::ToInstant(deadline)
                        } else {
                            state.dmabuf_imports.timer = None;
                            TimeoutAction::Drop
                        }
                    }) {
                    Ok(token) => self.dmabuf_imports.timer = Some(token),
                    Err(error) => {
                        tracing::warn!(%error, "could not schedule DMA-BUF import deadline");
                        self.dmabuf_imports
                            .queue
                            .remove_if(|_| true, |entry| entry.value.notifier.failed());
                    }
                }
            }
        } else if let Some(token) = self.dmabuf_imports.timer.take() {
            self.loop_handle.remove(token);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::fd::AsRawFd;

    #[test]
    fn renderer_order_cannot_consume_another_globals_requests() {
        for order in [[1, 2], [2, 1]] {
            let now = Instant::now();
            let mut queue = Queue::default();
            queue.enqueue(2, 1, "B-only", now).unwrap();
            let mut results = Vec::new();
            for owner in order {
                queue.process(&owner, |buffer| {
                    assert_eq!(owner, 2);
                    assert_eq!(buffer, "B-only");
                    results.push(true);
                });
            }

            assert_eq!(results, [true]);
            assert!(queue.pending.is_empty());
        }
    }

    #[test]
    fn unavailable_imports_are_bounded_and_recover_before_the_deadline() {
        let now = Instant::now();
        let mut queue = Queue::default();
        for index in 0..MAX_PENDING {
            queue.enqueue(1, index / MAX_PER_CLIENT, index, now).unwrap();
        }

        assert_eq!(queue.enqueue(1, 99, 100, now), Err(100));
        let mut completed = Vec::new();
        queue.remove_if(
            |entry| entry.deadline <= now + MAX_WAIT / 2,
            |_| panic!("expired early"),
        );
        queue.process(&1, |value| completed.push(value));
        assert_eq!(completed, (0..MAX_PENDING).collect::<Vec<_>>());
        assert!(queue.deadline().is_none());
    }

    #[test]
    fn per_client_admission_leaves_room_for_other_clients() {
        let now = Instant::now();
        let mut queue = Queue::default();
        for index in 0..MAX_PER_CLIENT {
            queue.enqueue(1, 1, index, now).unwrap();
        }

        assert_eq!(queue.enqueue(2, 1, 100, now), Err(100));
        queue.enqueue(2, 2, 101, now).unwrap();
    }

    #[test]
    fn expiry_and_device_removal_release_resources_once_without_touching_other_owners() {
        if !crate::startup_tests::private_runtime(
            "dmabuf_imports::tests::expiry_and_device_removal_release_resources_once_without_touching_other_owners",
        ) {
            return;
        }

        let now = Instant::now();
        let mut queue = Queue::default();
        let removed = tempfile::tempfile().unwrap();
        let expired = tempfile::tempfile().unwrap();
        let fd_removed = removed.as_raw_fd();
        let fd_expired = expired.as_raw_fd();
        queue.enqueue(1, 1, removed, now).unwrap();
        queue.enqueue(2, 1, expired, now + Duration::from_secs(1)).unwrap();
        let mut failures = 0;
        queue.process(&1, |_| failures += 1);
        // SAFETY: F_GETFD observes descriptor validity without modifying it.
        assert_eq!(unsafe { libc::fcntl(fd_removed, libc::F_GETFD) }, -1);
        assert_ne!(unsafe { libc::fcntl(fd_expired, libc::F_GETFD) }, -1);
        queue.remove_if(|entry| entry.deadline <= now + MAX_WAIT, |_| panic!("expired early"));
        queue.remove_if(
            |entry| entry.deadline <= now + MAX_WAIT + Duration::from_secs(1),
            |_| failures += 1,
        );
        assert_eq!(unsafe { libc::fcntl(fd_expired, libc::F_GETFD) }, -1);
        assert_eq!(failures, 2);
        assert!(queue.deadline().is_none());
    }
}
