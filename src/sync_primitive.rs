//! Define base operations for synchronization primitives.

use std::pin::Pin;
use std::ptr::{addr_of, null};
#[cfg(not(feature = "loom"))]
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering::{AcqRel, Acquire, Relaxed, Release};
use std::thread;

#[cfg(feature = "loom")]
use loom::sync::atomic::AtomicUsize;

use crate::opcode::Opcode;
use crate::wait_queue::WaitQueue;

/// Define base operations for synchronization primitives.
pub(crate) trait SyncPrimitive: Sized {
    /// Returns a reference to the state.
    fn state(&self) -> &AtomicUsize;

    /// Returns the maximum number of shared owners.
    fn max_shared_owners() -> usize;

    /// Converts a reference to `Self` into a memory address.
    fn addr(&self) -> usize {
        let self_ptr: *const Self = addr_of!(*self);
        self_ptr.expose_provenance()
    }

    /// Tries to push a wait queue entry into the wait queue.
    #[must_use]
    fn try_push_wait_queue_entry<F: FnOnce()>(
        &self,
        entry: Pin<&WaitQueue>,
        state: usize,
        begin_wait: F,
    ) -> Option<F> {
        let entry_addr = WaitQueue::ref_to_ptr(&entry).expose_provenance();
        debug_assert_eq!(entry_addr & (!WaitQueue::ADDR_MASK), 0);

        entry.update_next_entry_ptr(WaitQueue::addr_to_ptr(state & WaitQueue::ADDR_MASK));
        let next_state = (state & (!WaitQueue::ADDR_MASK)) | entry_addr;
        if self
            .state()
            .compare_exchange(state, next_state, AcqRel, Acquire)
            .is_ok()
        {
            // The entry cannot be dropped until the result is acknowledged.
            entry.enqueued();
            begin_wait();
            None
        } else {
            Some(begin_wait)
        }
    }

    /// Waits for the desired resources synchronously.
    fn wait_resources_sync<F: FnOnce()>(
        &self,
        state: usize,
        mode: Opcode,
        begin_wait: F,
    ) -> Result<u8, F> {
        debug_assert!(state & WaitQueue::ADDR_MASK != 0 || state & WaitQueue::DATA_MASK != 0);

        let sync_wait = WaitQueue::new_sync(mode, self.addr());
        let pinned_sync_wait = Pin::new(&sync_wait);
        debug_assert_eq!(
            addr_of!(sync_wait),
            WaitQueue::ref_to_ptr(&pinned_sync_wait)
        );

        if let Some(returned) = self.try_push_wait_queue_entry(pinned_sync_wait, state, begin_wait)
        {
            return Err(returned);
        }
        Ok(pinned_sync_wait.poll_result_sync())
    }

    /// Releases resources represented by the supplied operation mode.
    ///
    /// Returns `false` if the resource cannot be released.
    fn release_loop(&self, mut state: usize, mode: Opcode) -> bool {
        while mode.can_release(state) {
            if state & WaitQueue::ADDR_MASK == 0
                || state & WaitQueue::LOCKED_FLAG == WaitQueue::LOCKED_FLAG
            {
                // Release the resource in-place.
                match self.state().compare_exchange(
                    state,
                    state - mode.release_count(),
                    Release,
                    Relaxed,
                ) {
                    Ok(_) => return true,
                    Err(new_state) => state = new_state,
                }
            } else {
                // The wait queue is not empty and not being processed.
                let next_state = (state | WaitQueue::LOCKED_FLAG) - mode.release_count();
                if let Err(new_state) = self
                    .state()
                    .compare_exchange(state, next_state, AcqRel, Relaxed)
                {
                    state = new_state;
                    continue;
                }
                self.process_wait_queue(next_state);
                return true;
            }
        }
        false
    }

    /// Processes the wait queue.
    fn process_wait_queue(&self, mut state: usize) {
        let mut head_entry_ptr: *const WaitQueue = null();
        let mut unlocked = false;
        while !unlocked {
            debug_assert_eq!(state & WaitQueue::LOCKED_FLAG, WaitQueue::LOCKED_FLAG);

            let tail_entry_ptr = WaitQueue::addr_to_ptr(state & WaitQueue::ADDR_MASK);
            if head_entry_ptr.is_null() {
                WaitQueue::iter_forward(tail_entry_ptr, true, |entry, next_entry| {
                    head_entry_ptr = WaitQueue::ref_to_ptr(entry);
                    next_entry.is_none()
                });
            } else {
                WaitQueue::set_prev_ptr(tail_entry_ptr);
            }

            let data = state & WaitQueue::DATA_MASK;
            let mut transferred = 0;
            let mut resolved_entry_ptr: *const WaitQueue = null();
            let mut reset_failed = false;

            WaitQueue::iter_backward(head_entry_ptr, |entry, prev_entry| {
                let desired = entry.opcode().release_count();
                if data + transferred == 0
                    || data + transferred + desired <= Self::max_shared_owners()
                {
                    // The entry can inherit ownerhip.
                    if prev_entry.is_some() {
                        transferred += desired;
                        resolved_entry_ptr = WaitQueue::ref_to_ptr(entry);
                        false
                    } else {
                        // This is the tail of the wait queue: try to reset.
                        debug_assert_eq!(tail_entry_ptr, addr_of!(*entry));
                        if self
                            .state()
                            .compare_exchange(state, data + transferred + desired, AcqRel, Acquire)
                            .is_err()
                        {
                            // This entry will be processed on a retry.
                            entry.update_next_entry_ptr(null());
                            head_entry_ptr = WaitQueue::ref_to_ptr(entry);
                            reset_failed = true;
                            return true;
                        }

                        // The wait queue was reset.
                        unlocked = true;
                        resolved_entry_ptr = WaitQueue::ref_to_ptr(entry);
                        true
                    }
                } else {
                    // Unlink those that have succeeded in acquiring shared ownership.
                    entry.update_next_entry_ptr(null());
                    head_entry_ptr = WaitQueue::ref_to_ptr(entry);
                    true
                }
            });
            debug_assert!(!reset_failed || !unlocked);

            if !reset_failed && !unlocked {
                unlocked = self
                    .state()
                    .fetch_update(AcqRel, Acquire, |new_state| {
                        let new_data = new_state & WaitQueue::DATA_MASK;
                        debug_assert!(new_data <= data);
                        debug_assert!(new_data + transferred <= WaitQueue::DATA_MASK);

                        if new_data == data {
                            Some((new_state & WaitQueue::ADDR_MASK) | (new_data + transferred))
                        } else {
                            None
                        }
                    })
                    .is_ok();
            }

            if !unlocked {
                state = self.state().fetch_add(transferred, AcqRel) + transferred;
            }

            WaitQueue::iter_forward(resolved_entry_ptr, false, |entry, _next_entry| {
                entry.set_result(0);
                false
            });
        }
    }

    /// Removes a wait queue entry from the wait queue.
    fn remove_wait_queue_entry(
        &self,
        mut state: usize,
        entry_addr_to_remove: usize,
    ) -> (usize, bool) {
        let target_ptr = WaitQueue::addr_to_ptr(entry_addr_to_remove);
        let mut result = Ok((state, false));

        loop {
            debug_assert_eq!(state & WaitQueue::LOCKED_FLAG, WaitQueue::LOCKED_FLAG);
            debug_assert_ne!(state & WaitQueue::ADDR_MASK, 0);

            let tail_entry_ptr = WaitQueue::addr_to_ptr(state & WaitQueue::ADDR_MASK);
            WaitQueue::iter_forward(tail_entry_ptr, true, |entry, next_entry| {
                if WaitQueue::ref_to_ptr(entry) == target_ptr {
                    let prev_entry_ptr = entry.prev_entry_ptr();
                    if let Some(next_entry) = next_entry {
                        next_entry.update_prev_entry_ptr(prev_entry_ptr);
                    }
                    result = if let Some(prev_entry) = unsafe { prev_entry_ptr.as_ref() } {
                        prev_entry.update_next_entry_ptr(entry.next_entry_ptr());
                        Ok((state, true))
                    } else if let Some(next_entry) = next_entry {
                        let next_entry_ptr = WaitQueue::ref_to_ptr(next_entry);
                        debug_assert_eq!(next_entry_ptr.addr() & (!WaitQueue::ADDR_MASK), 0);

                        let next_state =
                            (state & (!WaitQueue::ADDR_MASK)) | next_entry_ptr.expose_provenance();
                        debug_assert_eq!(
                            next_state & WaitQueue::LOCKED_FLAG,
                            WaitQueue::LOCKED_FLAG
                        );

                        self.state()
                            .compare_exchange(state, next_state, AcqRel, Acquire)
                            .map(|_| (next_state, true))
                    } else {
                        // Reset the wait queue and unlock.
                        let next_state = state & WaitQueue::DATA_MASK;
                        self.state()
                            .compare_exchange(state, next_state, AcqRel, Acquire)
                            .map(|_| (next_state, true))
                    };
                    true
                } else {
                    false
                }
            });

            match result {
                Ok((state, removed)) => return (state, removed),
                Err(new_state) => state = new_state,
            }
        }
    }

    /// Cleans up a [`WaitQueue`] entry that was pushed into the wait queue, but has not been
    /// processed.
    fn cleanup_wait_queue(entry: &WaitQueue) {
        let this: &Self = entry.sync_primitive_ref();
        let wait_queue_ptr: *const WaitQueue = addr_of!(*entry);
        let wait_queue_addr = wait_queue_ptr.expose_provenance();

        // Remove the wait queue entry from the wait queue list.
        let mut state = this.state().load(Acquire);
        let mut need_completion = false;
        loop {
            if state & WaitQueue::LOCKED_FLAG == WaitQueue::LOCKED_FLAG {
                // Another thread is processing the wait queue.
                thread::yield_now();
                state = this.state().load(Acquire);
            } else if state & WaitQueue::ADDR_MASK == 0 {
                // The wait queue is empty.
                need_completion = true;
                break;
            } else if let Err(new_state) = this.state().compare_exchange(
                state,
                state | WaitQueue::LOCKED_FLAG,
                AcqRel,
                Acquire,
            ) {
                state = new_state;
            } else {
                let (new_state, removed) =
                    this.remove_wait_queue_entry(state | WaitQueue::LOCKED_FLAG, wait_queue_addr);
                if new_state & WaitQueue::LOCKED_FLAG == WaitQueue::LOCKED_FLAG {
                    // Need to process the wait queue if it is still locked.
                    this.process_wait_queue(new_state);
                }
                if !removed {
                    need_completion = true;
                }
                break;
            }
        }

        if need_completion {
            // The entry was removed from another thread, so will be completed.
            while !entry.result_finalized() {
                thread::yield_now();
            }
            this.release_loop(state, entry.opcode());
        }
    }

    /// Tests whether dropping a wait queue entry without waiting for its completion is safe.
    #[cfg(test)]
    fn test_drop_wait_queue_entry(&self, mode: Opcode) {
        let state = self.state().load(Acquire);
        let async_wait = WaitQueue::new_async(mode, Self::cleanup_wait_queue, self.addr());
        let pinned_async_wait = crate::wait_queue::PinnedWaitQueue(Pin::new(&async_wait));
        debug_assert_eq!(
            addr_of!(async_wait),
            WaitQueue::ref_to_ptr(&pinned_async_wait.0)
        );

        if let Some(f) = self.try_push_wait_queue_entry(pinned_async_wait.0, state, || ()) {
            f();
        }
    }
}

impl Opcode {
    /// Checks if the resourced expressed in `self` can be released from `state`.
    #[inline]
    pub(crate) const fn can_release(self, state: usize) -> bool {
        match self {
            Opcode::Exclusive => {
                let data = state & WaitQueue::DATA_MASK;
                data == WaitQueue::DATA_MASK
            }
            Opcode::Shared => {
                let data = state & WaitQueue::DATA_MASK;
                data >= 1 && data != WaitQueue::DATA_MASK
            }
            Opcode::Semaphore(count) => {
                let data = state & WaitQueue::DATA_MASK;
                let count = count as usize;
                data >= count
            }
            Opcode::Wait => true,
        }
    }

    /// Converts the operation mode into a `usize` value representing resources held by the
    /// corresponding synchronization primitive.
    #[inline]
    pub(crate) const fn release_count(self) -> usize {
        match self {
            Opcode::Exclusive => WaitQueue::DATA_MASK,
            Opcode::Shared => 1,
            Opcode::Semaphore(count) => {
                let count = count as usize;
                debug_assert!(count <= WaitQueue::LOCKED_FLAG);
                count
            }
            Opcode::Wait => 0,
        }
    }
}
