//! GPU copies remain on their renderer's event-loop thread until their fence is ready.
use std::collections::HashMap;
use std::time::{Duration, Instant};

use smithay::backend::renderer::gles::{GlesMapping, GlesRenderer};
use smithay::backend::renderer::sync::SyncPoint;
use smithay::backend::renderer::{ErasedContextId, ExportMem, Renderer};
use smithay::reexports::calloop::timer::{TimeoutAction, Timer};
use smithay::reexports::calloop::{
    Interest, Mode, PostAction, RegistrationToken, generic::Generic,
};

use crate::Ferese;

const MAX_READBACKS: usize = 32;
const MAX_READBACK_BYTES: usize = 256 * 1024 * 1024;
const READBACK_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_POLL_DELAY: Duration = Duration::from_millis(16);

pub(crate) type Valid = Box<dyn Fn(&Ferese) -> bool>;
pub(crate) type Completion = Box<dyn FnOnce(&mut Ferese, Result<(), String>)>;
pub(crate) type Mapped<T> = Box<dyn FnOnce(&[u8]) -> Result<T, String>>;
type MappedCompletion<T> = Box<dyn FnOnce(&mut Ferese, Result<T, String>)>;
pub(crate) type Pixels = Box<dyn FnOnce(&mut Ferese, Result<Vec<u8>, String>)>;

pub(crate) struct Readback {
    mapping: GlesMapping,
    context: ErasedContextId,
    fence: Option<SyncPoint>,
    bytes: usize,
}

impl Readback {
    /// Call immediately after copying into the PBO: a render-frame fence was
    /// inserted before the copy and cannot establish readback completion.
    pub(crate) fn new(
        renderer: &mut GlesRenderer,
        mapping: GlesMapping,
        bytes: usize,
    ) -> Result<Self, String> {
        let fence = renderer.insert_fence().map_err(|error| error.to_string())?;
        Ok(Self {
            mapping,
            context: renderer.context_id().erased(),
            fence,
            bytes,
        })
    }

    fn map<'a>(&'a self, renderer: &mut GlesRenderer) -> Result<&'a [u8], String> {
        if renderer.context_id().erased() != self.context {
            return Err("Capture renderer was replaced".into());
        }
        if self.fence.as_ref().is_some_and(|fence| !fence.is_reached()) {
            return Err("Readback GPU copy is not complete".into());
        }
        // Drivers without fence support retain the previous functional path.
        // Mapping can still wait on those drivers; never pretend a fence exists.
        let pixels = renderer
            .map_texture(&self.mapping)
            .map_err(|error| error.to_string())?;
        pixels
            .get(..self.bytes)
            .ok_or_else(|| "Incomplete GPU readback".into())
    }
}

struct Pending {
    fence: Option<SyncPoint>,
    bytes: usize,
    epoch: u64,
    valid: Valid,
    complete: Completion,
    readiness: Option<RegistrationToken>,
    timer: RegistrationToken,
    deadline: Instant,
    poll_delay: Duration,
}

#[derive(Default)]
pub(crate) struct Readbacks {
    pending: HashMap<u64, Pending>,
    bytes: usize,
    next: u64,
    quarantined: Vec<(ErasedContextId, SyncPoint)>,
}

impl Readbacks {
    pub(crate) fn prepare(&self, renderer: &mut GlesRenderer, bytes: usize) -> Result<(), String> {
        if !self.can_admit_in_context(bytes, &renderer.context_id().erased()) {
            return Err("GPU readback queue is full or its renderer is quarantined".into());
        }
        renderer
            .cleanup_texture_cache()
            .map_err(|error| error.to_string())
    }

    fn can_admit_in_context(&self, bytes: usize, context: &ErasedContextId) -> bool {
        self.can_admit(bytes) && self.context_id_available(context)
    }

    fn context_id_available(&self, context: &ErasedContextId) -> bool {
        self.quarantined
            .iter()
            .all(|(owner, fence)| owner != context || fence.is_reached())
    }

    fn quarantine(&mut self, context: ErasedContextId, fence: SyncPoint) {
        if !fence.is_reached() {
            self.quarantined.push((context, fence));
        }
    }

    pub(crate) fn can_admit(&self, bytes: usize) -> bool {
        self.pending.len() < MAX_READBACKS
            && bytes > 0
            && bytes <= MAX_READBACK_BYTES.saturating_sub(self.bytes)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Trigger {
    Readiness,
    Timer,
    Cancel,
}

impl Ferese {
    #[cfg(test)]
    pub(crate) fn test_queue_readback_pixels(
        &mut self,
        fence: SyncPoint,
        valid: Valid,
        complete: Pixels,
        pixels: Vec<u8>,
    ) {
        self.wait_readback(
            Some(fence),
            pixels.len(),
            valid,
            Box::new(move |state, status| match status {
                Ok(()) => complete(state, Ok(pixels)),
                Err(error) => {
                    // Match the production PBO queue's deferred failure ordering.
                    state
                        .loop_handle
                        .insert_idle(move |state| complete(state, Err(error)));
                }
            }),
            Instant::now() + READBACK_TIMEOUT,
        );
    }

    pub(crate) fn queue_readback(&mut self, readback: Readback, valid: Valid, complete: Pixels) {
        self.queue_mapped_readback(readback, valid, Box::new(copy_pixels), complete);
    }

    /// Consume ready PBO bytes on their renderer thread without an owned image.
    /// The sink cannot retain the mapping or access compositor/backend state.
    /// Completion runs after the renderer resources and backend borrow are restored.
    pub(crate) fn queue_mapped_readback<T: 'static>(
        &mut self,
        readback: Readback,
        valid: Valid,
        consume: Mapped<T>,
        complete: MappedCompletion<T>,
    ) {
        self.wait_readback(
            readback.fence.clone(),
            readback.bytes,
            valid,
            Box::new(move |state, status| match status {
                Ok(()) => state.map_ready_readback(readback, consume, complete),
                Err(error) => {
                    state.discard_readback(readback);
                    // Registration can fail while a render path temporarily
                    // owns the backend and session list. Publish only after it returns.
                    state
                        .loop_handle
                        .insert_idle(move |state| complete(state, Err(error)));
                }
            }),
            Instant::now() + READBACK_TIMEOUT,
        );
    }

    fn map_ready_readback<T: 'static>(
        &mut self,
        readback: Readback,
        consume: Mapped<T>,
        complete: MappedCompletion<T>,
    ) {
        if let Some(backend) = self.nested_backend.clone()
            && let Ok(mut backend) = backend.try_borrow_mut()
            && backend.renderer().context_id().erased() == readback.context
        {
            let pixels = readback.map(backend.renderer());
            if pixels.is_err()
                && let Some(fence) = &readback.fence
            {
                self.readbacks
                    .quarantine(readback.context.clone(), fence.clone());
            }
            let result = pixels.and_then(consume);
            drop(readback);
            if let Err(error) = backend.renderer().cleanup_texture_cache() {
                tracing::debug!(%error, "could not retire readback resources");
            }
            drop(backend);
            complete(self, result);
            return;
        }
        if let Some(mut backend) = self.direct_backend.take() {
            if let Some(renderer) = backend.readback_renderer(&readback.context) {
                let pixels = readback.map(renderer);
                if pixels.is_err()
                    && let Some(fence) = &readback.fence
                {
                    self.readbacks
                        .quarantine(readback.context.clone(), fence.clone());
                }
                let result = pixels.and_then(consume);
                drop(readback);
                if let Err(error) = renderer.cleanup_texture_cache() {
                    tracing::debug!(%error, "could not retire readback resources");
                }
                self.direct_backend = Some(backend);
                complete(self, result);
                return;
            }
            self.direct_backend = Some(backend);
        }
        complete(self, Err("Capture renderer is unavailable".into()));
    }

    fn discard_readback(&mut self, readback: Readback) {
        let context = readback.context.clone();
        if let Some(fence) = &readback.fence {
            self.readbacks.quarantine(context.clone(), fence.clone());
        }
        drop(readback);
        if let Some(backend) = self.nested_backend.clone()
            && let Ok(mut backend) = backend.try_borrow_mut()
            && backend.renderer().context_id().erased() == context
        {
            let _ = backend.renderer().cleanup_texture_cache();
            return;
        }
        if let Some(backend) = self.direct_backend.as_mut()
            && let Some(renderer) = backend.readback_renderer(&context)
        {
            let _ = renderer.cleanup_texture_cache();
        }
    }

    fn wait_readback(
        &mut self,
        fence: Option<SyncPoint>,
        bytes: usize,
        valid: Valid,
        complete: Completion,
        deadline: Instant,
    ) {
        if !self.readbacks.can_admit(bytes) {
            complete(self, Err("GPU readback queue is full".into()));
            return;
        }
        let id = self.readbacks.next;
        self.readbacks.next = self.readbacks.next.wrapping_add(1);
        let ready = fence.as_ref().is_none_or(SyncPoint::is_reached);
        let fd = (!ready)
            .then(|| fence.as_ref().and_then(SyncPoint::export))
            .flatten();
        let readiness = if let Some(fd) = fd {
            match self.loop_handle.insert_source(
                Generic::new(fd, Interest::READ, Mode::OneShot),
                move |_, _, state| {
                    let result = if state
                        .readbacks
                        .pending
                        .get(&id)
                        .and_then(|entry| entry.fence.as_ref())
                        .is_some_and(SyncPoint::is_reached)
                    {
                        Ok(())
                    } else {
                        Err("GPU readback fence failed".into())
                    };
                    state.finish_readback(id, Trigger::Readiness, result);
                    Ok(PostAction::Remove)
                },
            ) {
                Ok(token) => Some(token),
                Err(error) => {
                    complete(
                        self,
                        Err(format!("Could not watch readback fence: {error}")),
                    );
                    return;
                }
            }
        } else {
            None
        };
        let poll_delay = Duration::from_millis(2);
        let delay = if ready {
            Duration::ZERO
        } else if readiness.is_some() {
            deadline.saturating_duration_since(Instant::now())
        } else {
            poll_delay
        };
        let timer =
            match self
                .loop_handle
                .insert_source(Timer::from_duration(delay), move |_, _, state| {
                    let Some(entry) = state.readbacks.pending.get(&id) else {
                        return TimeoutAction::Drop;
                    };
                    if Instant::now() >= entry.deadline {
                        state.finish_readback(
                            id,
                            Trigger::Timer,
                            Err("GPU readback timed out".into()),
                        );
                        return TimeoutAction::Drop;
                    }
                    if entry.epoch != state.capture_epoch || !(entry.valid)(state) {
                        state.finish_readback(
                            id,
                            Trigger::Timer,
                            Err("GPU readback was cancelled".into()),
                        );
                        return TimeoutAction::Drop;
                    }
                    if entry.fence.as_ref().is_none_or(SyncPoint::is_reached) {
                        state.finish_readback(id, Trigger::Timer, Ok(()));
                        return TimeoutAction::Drop;
                    }
                    let entry = state
                        .readbacks
                        .pending
                        .get_mut(&id)
                        .expect("pending readback");
                    entry.poll_delay = (entry.poll_delay * 2).min(MAX_POLL_DELAY);
                    TimeoutAction::ToDuration(
                        entry
                            .poll_delay
                            .min(entry.deadline.saturating_duration_since(Instant::now())),
                    )
                }) {
                Ok(token) => token,
                Err(error) => {
                    if let Some(token) = readiness {
                        self.loop_handle.remove(token);
                    }
                    complete(
                        self,
                        Err(format!("Could not schedule readback deadline: {error}")),
                    );
                    return;
                }
            };
        self.readbacks.bytes += bytes;
        self.readbacks.pending.insert(
            id,
            Pending {
                fence,
                bytes,
                epoch: self.capture_epoch,
                valid,
                complete,
                readiness,
                timer,
                deadline,
                poll_delay,
            },
        );
    }

    fn finish_readback(&mut self, id: u64, trigger: Trigger, mut result: Result<(), String>) {
        let Some(entry) = self.readbacks.pending.remove(&id) else {
            return;
        };
        self.readbacks.bytes -= entry.bytes;
        if trigger != Trigger::Readiness
            && let Some(token) = entry.readiness
        {
            self.loop_handle.remove(token);
        }
        if trigger != Trigger::Timer {
            self.loop_handle.remove(entry.timer);
        }
        if result.is_ok() && Instant::now() >= entry.deadline {
            result = Err("GPU readback timed out".into());
        }
        if result.is_ok() && (entry.epoch != self.capture_epoch || !(entry.valid)(self)) {
            result = Err("GPU readback was cancelled".into());
        }
        (entry.complete)(self, result);
    }

    pub(crate) fn prune_readbacks(&mut self) {
        let expired = self
            .readbacks
            .pending
            .iter()
            .filter_map(|(id, entry)| {
                (entry.epoch != self.capture_epoch || !(entry.valid)(self)).then_some(*id)
            })
            .collect::<Vec<_>>();
        for id in expired {
            self.finish_readback(
                id,
                Trigger::Cancel,
                Err("GPU readback was cancelled".into()),
            );
        }
        let mut contexts = self
            .direct_backend
            .as_ref()
            .map_or_else(Vec::new, |backend| backend.readback_contexts());
        let mut context_inventory_known = true;
        if let Some(backend) = self.nested_backend.clone() {
            if let Ok(mut backend) = backend.try_borrow_mut() {
                contexts.push(backend.renderer().context_id().erased());
            } else {
                context_inventory_known = false;
            }
        }
        self.readbacks.quarantined.retain(|(context, fence)| {
            !fence.is_reached() && (!context_inventory_known || contexts.contains(context))
        });
    }
}

fn copy_pixels(source: &[u8]) -> Result<Vec<u8>, String> {
    let mut pixels = Vec::new();
    pixels
        .try_reserve_exact(source.len())
        .map_err(|error| error.to_string())?;
    pixels.extend_from_slice(source);
    Ok(pixels)
}

#[cfg(test)]
mod tests;
