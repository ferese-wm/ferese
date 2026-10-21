use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Clone, Default, Debug)]
pub(super) struct Cancellation(Arc<AtomicBool>);
impl Cancellation {
    pub(super) fn cancel(&self) {
        self.0.store(true, Ordering::Relaxed);
    }
    pub(super) fn cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
}
