//! Explicit attachments survive until their sampled renderer submissions finish.
use smithay::backend::renderer::ErasedContextId;
use smithay::backend::renderer::Renderer;
use smithay::backend::renderer::gles::{GlesCompletion, GlesRenderer};
use smithay::backend::renderer::sync::SyncPoint;
use smithay::backend::renderer::utils::Buffer;

pub(super) struct CompletionRecorder {
    pub context: ErasedContextId,
    completion: GlesCompletion,
}

impl CompletionRecorder {
    pub fn snapshot(&self) -> SyncPoint {
        self.completion.snapshot()
    }
}

pub(super) fn recorder(renderer: &GlesRenderer) -> CompletionRecorder {
    CompletionRecorder {
        context: renderer.context_id().erased(),
        completion: renderer.completion(),
    }
}

pub(super) struct RetiredBuffer {
    pub _buffer: Buffer,
    pub completions: Vec<SyncPoint>,
}

impl RetiredBuffer {
    pub fn finished(&self) -> bool {
        self.completions.iter().all(SyncPoint::is_reached)
    }
}
