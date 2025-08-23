//! [`Gate`] is a synchronization primitive that blocks multiple threads until the gate is opened.

#[cfg(not(feature = "loom"))]
use std::sync::atomic::AtomicUsize;

#[cfg(feature = "loom")]
use loom::sync::atomic::AtomicUsize;

use crate::sync_primitive::SyncPrimitive;

/// [`Gate`] is a synchronization primitive that blocks multiple threads until the gate is opened.
#[derive(Debug, Default)]
pub struct Gate {
    /// [`Gate`] state.
    state: AtomicUsize,
}

impl SyncPrimitive for Gate {
    #[inline]
    fn state(&self) -> &AtomicUsize {
        &self.state
    }

    #[inline]
    fn max_shared_owners() -> usize {
        usize::MAX
    }
}
