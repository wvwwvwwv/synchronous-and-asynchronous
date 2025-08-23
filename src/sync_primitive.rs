//! Define base operations for synchronization primitives.

use std::pin::Pin;
use std::ptr::{addr_of, null, with_exposed_provenance};
#[cfg(not(feature = "loom"))]
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering::{AcqRel, Acquire, Relaxed, Release};
use std::thread;

#[cfg(feature = "loom")]
use loom::sync::atomic::AtomicUsize;

use crate::opcode::Opcode;
use crate::wait_queue::{PinnedWaitQueue, WaitQueue};

/// Define base operations for synchronization primitives.
pub(crate) trait SyncPrimitive: Sized {
    /// Returns a reference to the state.
    fn state(&self) -> &AtomicUsize;

    /// Returns the maximum number of shared owners.
    fn max_shared_owners() -> usize;

    /// Converts a reference to `Self` into a memory address.
    fn self_addr(&self) -> usize {
        let self_ptr: *const Self = addr_of!(*self);
        self_ptr.expose_provenance()
    }

    /// Tries to push a wait queue entry into the wait queue.
    #[must_use]
    fn try_push_wait_queue_entry(&self, entry: Pin<&WaitQueue>, state: usize) -> bool {
        let entry_addr = WaitQueue::ref_to_ptr(&entry).expose_provenance();
        debug_assert_eq!(entry_addr & (!WaitQueue::ADDR_MASK), 0);

        entry.update_next_entry_ptr(WaitQueue::addr_to_ptr(state & WaitQueue::ADDR_MASK));
        let next_state = (state & (!WaitQueue::ADDR_MASK)) | entry_addr;
        self.state()
            .compare_exchange(state, next_state, AcqRel, Acquire)
            .is_ok()
    }

    /// Waits for the desired resources asynchronously.
    async fn wait_resources_async(&self, state: usize, mode: Opcode) -> bool {
        debug_assert!(state & WaitQueue::ADDR_MASK != 0 || state & WaitQueue::DATA_MASK != 0);

        if cfg!(miri) {
            return false;
        }

        let async_wait =
            WaitQueue::new_async(mode, Self::cleanup_wait_queue_entry, self.self_addr());
        let pinned_async_wait = PinnedWaitQueue(Pin::new(&async_wait));
        debug_assert_eq!(
            addr_of!(async_wait),
            WaitQueue::ref_to_ptr(&pinned_async_wait.0)
        );

        if !self.try_push_wait_queue_entry(pinned_async_wait.0, state) {
            pinned_async_wait.0.set_result(0);
            pinned_async_wait.0.result_acknowledged();
            return false;
        }
        pinned_async_wait.await;
        true
    }

    /// Waits for the desired resources synchronously.
    #[must_use]
    fn wait_resources_sync(&self, state: usize, mode: Opcode) -> bool {
        debug_assert!(state & WaitQueue::ADDR_MASK != 0 || state & WaitQueue::DATA_MASK != 0);

        if cfg!(miri) {
            return false;
        }

        let sync_wait = WaitQueue::new_sync(mode);
        let pinned_sync_wait = Pin::new(&sync_wait);
        debug_assert_eq!(
            addr_of!(sync_wait),
            WaitQueue::ref_to_ptr(&pinned_sync_wait)
        );

        if self.try_push_wait_queue_entry(pinned_sync_wait, state) {
            pinned_sync_wait.poll_result_sync();
            true
        } else {
            false
        }
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
                match self.try_start_process_wait_queue(state, mode) {
                    Ok(new_state) => {
                        self.process_wait_queue(new_state);
                        return true;
                    }
                    Err(new_state) => state = new_state,
                }
            }
        }
        false
    }

    /// Tries to start processing the wait queue by releasing a resource of the specified type and
    /// locking the wait queue.
    ///
    /// Returns the updated state if successful, or returns the latest state as an error if another
    /// thread was processing the wait queue or the resource could not be released.
    fn try_start_process_wait_queue(&self, mut state: usize, mode: Opcode) -> Result<usize, usize> {
        if state & WaitQueue::ADDR_MASK == 0
            || state & WaitQueue::LOCKED_FLAG == WaitQueue::LOCKED_FLAG
            || !mode.can_release(state)
        {
            return Err(state);
        }

        let mut next_state = (state | WaitQueue::LOCKED_FLAG) - mode.release_count();
        while let Err(new_state) = self
            .state()
            .compare_exchange(state, next_state, AcqRel, Relaxed)
        {
            if new_state & WaitQueue::ADDR_MASK == 0
                || new_state & WaitQueue::LOCKED_FLAG == WaitQueue::LOCKED_FLAG
                || !mode.can_release(state)
            {
                return Err(new_state);
            }
            state = new_state;
            next_state = (state | WaitQueue::LOCKED_FLAG) - mode.release_count();
        }
        Ok(next_state)
    }

    /// Processes the wait queue.
    fn process_wait_queue(&self, mut state: usize) {
        let mut head_entry_ptr: *const WaitQueue = null();
        let mut unlocked = false;
        while !unlocked {
            debug_assert_eq!(state & WaitQueue::LOCKED_FLAG, WaitQueue::LOCKED_FLAG);

            let tail_entry_ptr = WaitQueue::addr_to_ptr(state & WaitQueue::ADDR_MASK);
            if head_entry_ptr.is_null() {
                WaitQueue::iter_forward(tail_entry_ptr, |entry, next_entry| {
                    if next_entry.is_none() {
                        head_entry_ptr = WaitQueue::ref_to_ptr(entry);
                    }
                    false
                });
            } else {
                WaitQueue::install_backward_link(tail_entry_ptr);
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
                    if prev_entry.is_none() {
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
                    }

                    resolved_entry_ptr = WaitQueue::ref_to_ptr(entry);
                    transferred += desired;
                    unlocked
                } else {
                    // Unlink those that have succeeded in acquiring shared ownership.
                    entry.update_next_entry_ptr(null());
                    head_entry_ptr = WaitQueue::ref_to_ptr(entry);
                    true
                }
            });
            debug_assert_eq!(resolved_entry_ptr.is_null(), transferred == 0);

            if reset_failed {
                debug_assert!(!unlocked);
                state = self.state().fetch_add(transferred, Release) + transferred;
            } else if !unlocked {
                if self
                    .state()
                    .fetch_update(AcqRel, Acquire, |new_state| {
                        let new_data = new_state & WaitQueue::DATA_MASK;
                        debug_assert!(new_data + transferred <= WaitQueue::DATA_MASK);

                        if new_state == state || new_data == data {
                            let next_state =
                                (new_state & WaitQueue::ADDR_MASK) | (new_data + transferred);
                            Some(next_state)
                        } else {
                            None
                        }
                    })
                    .is_err()
                {
                    state = self.state().fetch_add(transferred, Release) + transferred;
                } else {
                    unlocked = true;
                }
            }

            WaitQueue::iter_forward(resolved_entry_ptr, |entry, _next_entry| {
                entry.set_result(0);
                // The lock can be released here.
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
            WaitQueue::iter_forward(tail_entry_ptr, |entry, next_entry| {
                if WaitQueue::ref_to_ptr(entry) == target_ptr {
                    if let Some(next_entry) = next_entry {
                        next_entry.update_prev_entry_ptr(entry.prev_entry_ptr());
                    }
                    if let Some(prev_entry) = unsafe { entry.prev_entry_ptr().as_ref() } {
                        prev_entry.update_next_entry_ptr(entry.next_entry_ptr());
                        result = Ok((state, true));
                    } else {
                        let next_entry_ptr = next_entry.map_or(null(), WaitQueue::ref_to_ptr);
                        debug_assert_eq!(next_entry_ptr.addr() & (!WaitQueue::ADDR_MASK), 0);

                        let next_state =
                            (state & (!WaitQueue::ADDR_MASK)) | next_entry_ptr.expose_provenance();
                        debug_assert_eq!(
                            next_state & WaitQueue::LOCKED_FLAG,
                            WaitQueue::LOCKED_FLAG
                        );

                        match self
                            .state()
                            .compare_exchange(state, next_state, AcqRel, Acquire)
                        {
                            Ok(_) => result = Ok((next_state, true)),
                            Err(new_state) => result = Err(new_state),
                        }
                    }
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
    fn cleanup_wait_queue_entry(entry: &WaitQueue, self_addr: usize) {
        let this: &Self = unsafe { &*with_exposed_provenance(self_addr) };
        let wait_queue_ptr: *const WaitQueue = addr_of!(*entry);
        let wait_queue_addr = wait_queue_ptr.expose_provenance();

        // Remove the wait queue entry from the wait queue list.
        let mut state = this.state().load(Acquire);
        let mut need_completion = false;
        loop {
            match this.try_start_process_wait_queue(state, Opcode::Wait) {
                Ok(new_state) => state = new_state,
                Err(new_state) => {
                    if new_state & WaitQueue::LOCKED_FLAG == WaitQueue::LOCKED_FLAG {
                        // Another thread is processing the wait queue.
                        thread::yield_now();
                        state = this.state().load(Acquire);
                        continue;
                    }

                    // The wait queue is empty.
                    debug_assert_eq!(new_state & WaitQueue::LOCKED_FLAG, 0);
                    debug_assert_eq!(new_state & WaitQueue::ADDR_MASK, 0);
                    state = new_state;
                    need_completion = true;
                }
            }
            break;
        }

        if state & WaitQueue::LOCKED_FLAG == WaitQueue::LOCKED_FLAG {
            let (new_state, removed) = this.remove_wait_queue_entry(state, wait_queue_addr);
            this.process_wait_queue(new_state);

            if !removed {
                need_completion = true;
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
        let async_wait =
            WaitQueue::new_async(mode, Self::cleanup_wait_queue_entry, self.self_addr());
        let pinned_async_wait = PinnedWaitQueue(Pin::new(&async_wait));
        debug_assert_eq!(
            addr_of!(async_wait),
            WaitQueue::ref_to_ptr(&pinned_async_wait.0)
        );

        if !self.try_push_wait_queue_entry(pinned_async_wait.0, state) {
            pinned_async_wait.0.set_result(0);
            pinned_async_wait.0.result_acknowledged();
        }
    }
}

impl Opcode {
    /// Checks if the resourced expressed in `self` can be released from `state`.
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
