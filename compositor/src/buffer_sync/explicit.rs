//! Capability-gated explicit acquire/release ownership.
use std::collections::HashMap;
use std::fs::OpenOptions;
use std::os::fd::OwnedFd;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak as ArcWeak};
use std::time::Duration;

use calloop::RegistrationToken;
use smithay::backend::drm::{DrmDeviceFd, DrmNode, NodeType};
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::backend::renderer::sync::Fence;
use smithay::backend::renderer::utils::{
    Buffer, on_commit_buffer_handler, with_renderer_surface_state,
};
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::{Client, Resource, Weak};
use smithay::utils::DeviceFd;
use smithay::wayland::compositor::{
    Blocker, BlockerState, SurfaceAttributes, add_blocker, is_sync_subsurface,
};
use smithay::wayland::drm_syncobj::{
    DrmSyncPoint, DrmSyncobjCachedState, DrmSyncobjHandler, DrmSyncobjState,
    supports_syncobj_eventfd,
};

use super::gpu::{CompletionRecorder, RetiredBuffer};
use crate::Ferese;

const MAX_SURFACES: usize = 512;
const MAX_CLIENT_SURFACES: usize = 64;
const MAX_RETIRED: usize = 2048;
const RETIRE_INTERVAL: Duration = Duration::from_millis(4);

struct ReleaseInner {
    surface: Weak<WlSurface>,
    client: Client,
    point: DrmSyncPoint,
    transferred: AtomicBool,
    retired: AtomicBool,
}

impl ReleaseInner {
    fn retire_unused(&self) {
        if !self.transferred.load(Ordering::Acquire)
            && !self.retired.swap(true, Ordering::AcqRel)
            && let Err(error) = self.point.signal()
        {
            tracing::debug!(%error, "could not release unused explicit attachment");
        }
    }
}

impl Drop for ReleaseInner {
    fn drop(&mut self) {
        self.retire_unused();
    }
}

#[derive(Clone)]
pub(super) struct ReleaseLease(Arc<ReleaseInner>);

impl Blocker for ReleaseLease {
    fn state(&self) -> BlockerState {
        BlockerState::Released
    }
}

#[derive(Default)]
pub(super) struct ExplicitSync {
    pub state: Option<DrmSyncobjState>,
    pub import_gpu: Option<DrmNode>,
    devices: HashMap<DrmNode, DrmDeviceFd>,
    slots: HashMap<Weak<WlSurface>, Client>,
    buffers: HashMap<Weak<WlSurface>, Buffer>,
    recorders: HashMap<Option<DrmNode>, CompletionRecorder>,
    pub(super) retired: Vec<RetiredBuffer>,
    pub(super) timer: Option<RegistrationToken>,
    leases: Vec<ArcWeak<ReleaseInner>>,
}

impl DrmSyncobjHandler for Ferese {
    fn drm_syncobj_state(&mut self) -> Option<&mut DrmSyncobjState> {
        self.buffer_sync.explicit.state.as_mut()
    }
}

impl Ferese {
    pub(super) fn prepare_explicit_buffer(
        &mut self,
        surface: &WlSurface,
        gpu: Option<DrmNode>,
        acquire: DrmSyncPoint,
        release: DrmSyncPoint,
        timeout: Duration,
    ) {
        if !self.admit_explicit_surface(surface) {
            let gate = super::AcquireGate::default();
            gate.0.store(super::CANCELLED, Ordering::Release);
            add_blocker(surface, gate);
            return;
        }
        // Avoid an eventfd, watcher, timer and an extra event-loop round trip
        // for the common case where the producer has already finished.
        if acquire.is_signaled() {
            if self.budget_buffer_transaction(surface) {
                drop(self.new_release_lease(surface, release));
            }
            return;
        }
        match acquire.generate_blocker() {
            Ok((_, source)) => {
                let release = self.new_release_lease(surface, release);
                self.wait_for_buffer(surface, source, gpu, Some(release), timeout);
            }
            Err(error) => {
                let _ = release.signal();
                let gate = super::AcquireGate::default();
                gate.0.store(super::CANCELLED, Ordering::Release);
                add_blocker(surface, gate);
                if let Some(client) = surface.client() {
                    tracing::warn!(%error, "could not watch explicit acquire point");
                    self.reject_buffer_client(&client, "explicit acquire registration failed");
                }
            }
        }
    }

    pub(crate) fn register_sync_renderer(
        &mut self,
        renderer: &GlesRenderer,
        node: Option<DrmNode>,
    ) {
        self.buffer_sync
            .explicit
            .recorders
            .insert(node, super::gpu::recorder(renderer));
        let Some(node) = node else {
            return;
        };
        if node.ty() != NodeType::Render {
            return;
        }
        // Open an independent render-node descriptor; retaining libseat's card
        // descriptor here would delay session release on device removal.
        let Some(path) = node.dev_path() else {
            return;
        };
        let Ok(file) = OpenOptions::new().read(true).write(true).open(path) else {
            return;
        };
        let fd = DrmDeviceFd::new(DeviceFd::from(OwnedFd::from(file)));
        if !supports_syncobj_eventfd(&fd) {
            return;
        }
        self.buffer_sync.explicit.devices.insert(node, fd.clone());
        if self.buffer_sync.explicit.state.is_none() {
            self.buffer_sync.explicit.state =
                Some(DrmSyncobjState::new::<Ferese>(&self.display_handle, fd));
            self.buffer_sync.explicit.import_gpu = Some(node);
        }
    }

    pub(super) fn admit_explicit_surface(&mut self, surface: &WlSurface) -> bool {
        let Some(client) = surface.client() else {
            return false;
        };
        let manager = &mut self.buffer_sync.explicit;
        let weak = surface.downgrade();
        manager.leases.retain(|lease| {
            lease
                .upgrade()
                .is_some_and(|lease| !lease.retired.load(Ordering::Acquire))
        });
        let leases: Vec<_> = manager.leases.iter().filter_map(ArcWeak::upgrade).collect();
        if manager.retired.len() >= MAX_RETIRED
            || leases.len() >= super::MAX_PENDING
            || leases
                .iter()
                .filter(|lease| lease.client.id() == client.id())
                .count()
                >= super::MAX_PER_CLIENT
            || leases.iter().filter(|lease| lease.surface == weak).count() >= super::MAX_PER_SURFACE
            || (!manager.slots.contains_key(&weak)
                && (manager.slots.len() >= MAX_SURFACES
                    || manager
                        .slots
                        .values()
                        .filter(|owner| owner.id() == client.id())
                        .count()
                        >= MAX_CLIENT_SURFACES))
        {
            self.reject_buffer_client(&client, "explicit buffer lifetime budget exhausted");
            return false;
        }
        manager.slots.entry(weak).or_insert(client);
        true
    }

    pub(super) fn new_release_lease(
        &mut self,
        surface: &WlSurface,
        point: DrmSyncPoint,
    ) -> ReleaseLease {
        let lease = ReleaseLease(Arc::new(ReleaseInner {
            surface: surface.downgrade(),
            client: surface
                .client()
                .expect("validated explicit surface has an owner"),
            point,
            transferred: AtomicBool::new(false),
            retired: AtomicBool::new(false),
        }));
        self.buffer_sync
            .explicit
            .leases
            .push(Arc::downgrade(&lease.0));
        // Transactions retain released blockers through state promotion. This
        // ticket releases a cancelled/superseded attachment even if its acquire
        // completed while FIFO, a parent, or commit timing still delayed it.
        add_blocker(surface, lease.clone());
        lease
    }

    pub(crate) fn commit_synced_buffers(&mut self, surface: &WlSurface) {
        if is_sync_subsurface(surface) {
            return;
        }
        if self.buffer_sync.explicit.slots.is_empty()
            && self.buffer_sync.explicit.buffers.is_empty()
        {
            on_commit_buffer_handler::<Self>(surface);
            return;
        }
        let mut updated = Vec::new();
        smithay::desktop::utils::with_surfaces_surface_tree(surface, |surface, states| {
            if states
                .cached_state
                .get::<SurfaceAttributes>()
                .current()
                .buffer
                .is_some()
            {
                let release = states
                    .cached_state
                    .get::<DrmSyncobjCachedState>()
                    .current()
                    .release_point
                    .clone();
                updated.push((surface.clone(), release));
            }
        });
        // Captures of previous contents have already finished submission. Their
        // completion recorders are included before the old attachment retires.
        for (surface, _) in &updated {
            if let Some(buffer) = self
                .buffer_sync
                .explicit
                .buffers
                .remove(&surface.downgrade())
            {
                self.retire_explicit_buffer(buffer);
            }
        }
        on_commit_buffer_handler::<Self>(surface);
        for (surface, release) in updated {
            let buffer =
                with_renderer_surface_state(&surface, |data| data.buffer().cloned()).flatten();
            if let Some(buffer) = buffer.filter(Buffer::has_explicit_sync) {
                if let Some(release) = release {
                    for lease in self
                        .buffer_sync
                        .explicit
                        .leases
                        .iter()
                        .filter_map(ArcWeak::upgrade)
                    {
                        if lease.surface == surface.downgrade()
                            && lease.point.timeline() == release.timeline()
                            && lease.point.point() == release.point()
                        {
                            lease.transferred.store(true, Ordering::Release);
                        }
                    }
                }
                self.buffer_sync
                    .explicit
                    .buffers
                    .insert(surface.downgrade(), buffer);
            }
        }
    }

    fn retire_explicit_buffer(&mut self, buffer: Buffer) {
        let completions = self
            .buffer_sync
            .explicit
            .recorders
            .values()
            .filter(|recorder| buffer.was_sampled_by(&recorder.context))
            .map(CompletionRecorder::snapshot)
            .filter(|sync| !sync.is_reached())
            .collect();
        let retired = RetiredBuffer {
            _buffer: buffer,
            completions,
        };
        if retired.finished() {
            return;
        }
        if let Some(existing) = self
            .buffer_sync
            .explicit
            .retired
            .iter_mut()
            .find(|existing| existing._buffer.same_attachment(&retired._buffer))
        {
            existing.completions.extend(retired.completions);
        } else {
            self.buffer_sync.explicit.retired.push(retired);
        }
    }

    pub(super) fn destroy_explicit_surface(&mut self, surface: &WlSurface) {
        let weak = surface.downgrade();
        // A synchronized child's pending transaction can outlive wl_surface
        // destruction while its wl_subsurface resource remains alive. It has
        // never been sampled; release that attachment without promoting it.
        for lease in self
            .buffer_sync
            .explicit
            .leases
            .iter()
            .filter_map(ArcWeak::upgrade)
        {
            if lease.surface == weak {
                lease.retire_unused();
            }
        }
        if let Some(client) = self.buffer_sync.explicit.slots.remove(&weak) {
            // Acquire may have completed while another constraint still holds
            // this transaction. Prune now-dead transactions and release their
            // unused attachments even though no acquire watcher remains.
            self.queue_buffer_notification(client);
        }
        if let Some(buffer) = self.buffer_sync.explicit.buffers.remove(&weak) {
            self.retire_explicit_buffer(buffer);
        }
    }

    pub(super) fn remove_sync_device(&mut self, node: DrmNode) {
        // Preserve this renderer's last reads even if another GPU takes over
        // timeline imports and the original renderer/context is retired.
        let removed_context = self
            .buffer_sync
            .explicit
            .recorders
            .get(&Some(node))
            .map(|recorder| recorder.context.clone());
        let buffers: Vec<_> = self
            .buffer_sync
            .explicit
            .buffers
            .values()
            .filter(|buffer| {
                removed_context
                    .as_ref()
                    .is_some_and(|context| buffer.was_sampled_by(context))
            })
            .cloned()
            .collect();
        let mut owners = std::collections::HashMap::new();
        for buffer in buffers {
            if let Some(owner) = buffer.client() {
                owners.insert(owner.id(), owner);
            }
            self.retire_explicit_buffer(buffer);
        }
        for owner in owners.values() {
            self.reject_buffer_client(
                owner,
                "GPU removed while its renderer owned an explicit buffer",
            );
        }
        self.buffer_sync.explicit.recorders.remove(&Some(node));
        self.buffer_sync.explicit.devices.remove(&node);
        if self.buffer_sync.explicit.import_gpu != Some(node) {
            return;
        }
        let next = self
            .buffer_sync
            .explicit
            .devices
            .iter()
            .next()
            .map(|(node, fd)| (*node, fd.clone()));
        // Import migration has no per-timeline success result in this Smithay
        // API. A failed reimport would strand a release fence on a dead fd.
        // Retire old owners and their global; fresh clients can use a surviving
        // GPU without inheriting partially migrated timeline objects.
        let mut seen = std::collections::HashSet::new();
        let owners: Vec<_> = self
            .buffer_sync
            .explicit
            .slots
            .values()
            .filter(|owner| seen.insert(owner.id()))
            .cloned()
            .collect();
        for owner in owners {
            self.reject_buffer_client(&owner, "explicit synchronization device removed");
        }
        if let Some(state) = self.buffer_sync.explicit.state.take() {
            self.display_handle
                .disable_global::<Self>(state.into_global());
        }
        self.buffer_sync.explicit.import_gpu = None;
        if let Some((node, fd)) = next {
            self.buffer_sync.explicit.state =
                Some(DrmSyncobjState::new::<Self>(&self.display_handle, fd));
            self.buffer_sync.explicit.import_gpu = Some(node);
        }
    }

    pub(super) fn poll_explicit_buffers(&mut self) {
        self.buffer_sync
            .explicit
            .retired
            .retain(|buffer| !buffer.finished());
        // A client disconnect can destroy a surface without a dispatched
        // wl_surface.destroy request. Retire its owned attachment as well.
        let dead: Vec<_> = self
            .buffer_sync
            .explicit
            .buffers
            .keys()
            .filter(|surface| surface.upgrade().is_err())
            .cloned()
            .collect();
        for surface in dead {
            if let Some(buffer) = self.buffer_sync.explicit.buffers.remove(&surface) {
                self.retire_explicit_buffer(buffer);
            }
            self.buffer_sync.explicit.slots.remove(&surface);
        }
        self.buffer_sync
            .explicit
            .slots
            .retain(|surface, _| surface.upgrade().is_ok());
        if self.buffer_sync.explicit.retired.is_empty() {
            if let Some(token) = self.buffer_sync.explicit.timer.take() {
                self.loop_handle.remove(token);
            }
        } else if self.buffer_sync.explicit.timer.is_none() {
            use calloop::timer::{TimeoutAction, Timer};
            match self.loop_handle.insert_source(
                Timer::from_duration(RETIRE_INTERVAL),
                |_, _, state| {
                    state.buffer_sync.explicit.timer = None;
                    state.poll_explicit_buffers();
                    TimeoutAction::Drop
                },
            ) {
                Ok(token) => self.buffer_sync.explicit.timer = Some(token),
                Err(error) => {
                    tracing::error!(%error, "could not schedule explicit buffer retirement");
                }
            }
        }
    }
}
