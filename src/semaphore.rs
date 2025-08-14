//! [`Semaphore`] is a synchronization primitive that allows a fixed number of threads to access a
//! resource concurrently.

use crate::sync_primitive::SyncPrimitive;
use crate::wait_queue::WaitQueue;
#[cfg(feature = "loom")]
use loom::sync::atomic::AtomicUsize;
use std::fmt;
#[cfg(not(feature = "loom"))]
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering::Relaxed;

/// [`Semaphore`] is a synchronization primitive that allows a fixed number of threads to access a
/// resource concurrently.
pub struct Semaphore {
    state: AtomicUsize,
}

impl fmt::Debug for Semaphore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let state = self.state.load(Relaxed);
        let semaphore_count = state & WaitQueue::DATA_MASK;
        let wait_queue_being_processed = state & WaitQueue::LOCKED_FLAG == WaitQueue::LOCKED_FLAG;
        let wait_queue_tail_addr = state & WaitQueue::ADDR_MASK;
        f.debug_struct("WaitQueue")
            .field("state", &state)
            .field("semaphore_count", &semaphore_count)
            .field("wait_queue_being_processed", &wait_queue_being_processed)
            .field("wait_queue_tail_addr", &wait_queue_tail_addr)
            .finish()
    }
}

impl SyncPrimitive for Semaphore {
    #[inline]
    fn state(&self) -> &AtomicUsize {
        &self.state
    }

    #[inline]
    fn max_shared_owners() -> usize {
        WaitQueue::DATA_MASK
    }
}
