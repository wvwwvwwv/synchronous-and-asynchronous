//! Define base operations for synchronization primitives.

use crate::wait_queue::WaitQueue;
#[cfg(feature = "loom")]
use loom::sync::atomic::AtomicUsize;
use std::pin::Pin;
use std::ptr::{addr_of, null};
#[cfg(not(feature = "loom"))]
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering::{AcqRel, Acquire, Relaxed, Release};
use std::thread;

/// Operation types.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(crate) enum Opcode {
    /// Acquires exclusive ownership.
    Exclusive,
    /// Acquires shared ownership.
    Shared,
    /// Acquires semaphores.
    Semaphore(u8),
    /// Cleanup stale wait queue entries.
    Cleanup,
}

/// Define base operations for synchronization primitives.
pub(crate) trait SyncPrimitive: Sized {
    /// Returns a reference to the state.
    fn state(&self) -> &AtomicUsize;

    /// Returns the maximum number of shared owners.
    fn max_shared_owners() -> usize;

    /// Converts a reference to `Self` into a memory address.
    fn self_addr(&self) -> usize {
        let self_ptr: *const Self = addr_of!(*self);
        self_ptr as usize
    }

    /// Tries to push a wait queue entry into the wait queue.
    #[must_use]
    fn try_push_wait_queue_entry(&self, entry: &mut Pin<&mut WaitQueue>, state: usize) -> bool {
        let entry_addr = WaitQueue::ref_to_ptr(entry) as usize;
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
        let mut async_wait = WaitQueue::new(
            mode,
            Some((self.self_addr(), Self::cleanup_wait_queue_entry)),
        );
        let mut async_wait_pinned = Pin::new(&mut async_wait);
        if !self.try_push_wait_queue_entry(&mut async_wait_pinned, state) {
            async_wait_pinned.set_result(false);
        }
        async_wait_pinned.await
    }

    /// Waits for the desired resources synchronously.
    #[must_use]
    fn wait_resources_sync(&self, state: usize, mode: Opcode) -> bool {
        debug_assert!(state & WaitQueue::ADDR_MASK != 0 || state & WaitQueue::DATA_MASK != 0);
        let mut sync_wait = WaitQueue::new(mode, None);
        let mut sync_wait_pinned = Pin::new(&mut sync_wait);
        if !self.try_push_wait_queue_entry(&mut sync_wait_pinned, state) {
            sync_wait_pinned.set_result(false);
        }
        sync_wait_pinned.poll_result()
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
                match self.try_process_wait_queue(state, 0, mode) {
                    Ok(()) => return true,
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

    /// Tries to process the wait queue after releasing resources specified by the operation mode
    /// and locking the wait queue.
    ///
    /// Returns `Err` if locking the wait queue failed, the wait queue no longer needs to be
    /// processed, of the wrong argument was supplied.
    fn try_process_wait_queue(
        &self,
        mut state: usize,
        mut entry_addr_to_remove: usize,
        mode: Opcode,
    ) -> Result<(), usize> {
        state = self.try_start_process_wait_queue(state, mode)?;

        // This is the only thread processing the wait queue.
        let mut head_entry_ptr: *const WaitQueue = null();
        loop {
            debug_assert_eq!(state & WaitQueue::LOCKED_FLAG, WaitQueue::LOCKED_FLAG);

            if entry_addr_to_remove != 0 {
                state = match self.remove_wait_queue_entry(state, entry_addr_to_remove) {
                    Ok(new_state) => {
                        entry_addr_to_remove = 0;
                        new_state
                    }
                    Err(new_state) => {
                        state = new_state;
                        continue;
                    }
                };
            }

            let tail_entry_ptr = WaitQueue::addr_to_ptr(state & WaitQueue::ADDR_MASK);
            if head_entry_ptr.is_null() {
                WaitQueue::any_forward(tail_entry_ptr, |entry, next_entry| {
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
            let mut obsolete_entry_ptr: *const WaitQueue = null();
            let mut unlocked = false;

            WaitQueue::any_backward(head_entry_ptr, |entry, prev_entry| {
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
                            return true;
                        }

                        // The wait queue was reset.
                        unlocked = true;
                    }

                    obsolete_entry_ptr = WaitQueue::ref_to_ptr(entry);
                    transferred += desired;
                    unlocked
                } else {
                    // Unlink those that have succeeded in acquiring shared ownership.
                    entry.update_next_entry_ptr(null());
                    head_entry_ptr = WaitQueue::ref_to_ptr(entry);
                    true
                }
            });
            debug_assert_eq!(obsolete_entry_ptr.is_null(), transferred == 0);

            if !unlocked {
                unlocked = if self
                    .state()
                    .fetch_update(AcqRel, Acquire, |new_state| {
                        let new_data = new_state & WaitQueue::DATA_MASK;
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
                    false
                } else {
                    true
                };
            }

            WaitQueue::any_forward(obsolete_entry_ptr, |entry, _next_entry| {
                entry.set_result(true);
                false
            });

            if unlocked {
                return Ok(());
            }
        }
    }

    /// Removes a wait queue entry from the wait queue.
    fn remove_wait_queue_entry(
        &self,
        state: usize,
        entry_addr_to_remove: usize,
    ) -> Result<usize, usize> {
        debug_assert_eq!(state & WaitQueue::LOCKED_FLAG, WaitQueue::LOCKED_FLAG);
        debug_assert_ne!(state & WaitQueue::ADDR_MASK, 0);

        let tail_entry_ptr = WaitQueue::addr_to_ptr(state & WaitQueue::ADDR_MASK);
        let target_ptr = WaitQueue::addr_to_ptr(entry_addr_to_remove);
        let mut result = Ok(state);
        WaitQueue::any_forward(tail_entry_ptr, |entry, next_entry| {
            if WaitQueue::ref_to_ptr(entry) == target_ptr {
                if let Some(prev_entry) = unsafe { entry.prev_entry_ptr().as_ref() } {
                    prev_entry.update_next_entry_ptr(entry.next_entry_ptr());
                } else {
                    let next_entry_ptr = next_entry.map_or(null(), WaitQueue::ref_to_ptr);
                    let next_state = (state & !WaitQueue::ADDR_MASK) | next_entry_ptr as usize;
                    match self
                        .state()
                        .compare_exchange(state, next_state, AcqRel, Acquire)
                    {
                        Ok(_) => result = Ok(next_state),
                        Err(new_state) => result = Err(new_state),
                    }
                }
                if let Some(next_entry) = next_entry {
                    next_entry.update_prev_entry_ptr(entry.prev_entry_ptr());
                }
                true
            } else {
                false
            }
        });
        result
    }

    /// Cleans up a [`WaitQueue`] entry that was pushed into the wait queue, but has not been
    /// processed.
    fn cleanup_wait_queue_entry(entry: &WaitQueue, self_addr: usize) {
        let this: &Self = unsafe { &*(self_addr as *const Self) };
        let wait_queue_ptr: *const WaitQueue = addr_of!(*entry);
        let wait_queue_addr = wait_queue_ptr as usize;

        // Remove the wait queue entry from the wait queue list.
        let mut state = this.state().load(Acquire);
        if state & WaitQueue::ADDR_MASK != 0 {
            while let Err(new_state) =
                this.try_process_wait_queue(state, wait_queue_addr, Opcode::Cleanup)
            {
                if new_state & WaitQueue::LOCKED_FLAG == WaitQueue::LOCKED_FLAG {
                    // Another thread is processing the wait queue.
                    thread::yield_now();
                    state = new_state;
                    debug_assert_ne!(state & WaitQueue::ADDR_MASK, 0);
                    continue;
                }
                // The wait queue is empty.
                debug_assert_eq!(new_state & WaitQueue::ADDR_MASK, 0);
                break;
            }
        }

        // Release any acquired locks if the wait queue was processed.
        if entry.async_poll_completed() {
            this.release_loop(state, entry.opcode());
        }
    }

    /// Tests whether dropping a wait queue entry without waiting for its completion is safe.
    #[cfg(test)]
    fn test_drop_wait_queue_entry(&self, mode: Opcode) {
        let state = self.state().load(Acquire);
        let mut async_wait = WaitQueue::new(
            mode,
            Some((self.self_addr(), Self::cleanup_wait_queue_entry)),
        );
        let mut async_wait_pinned = std::pin::Pin::new(&mut async_wait);
        if !self.try_push_wait_queue_entry(&mut async_wait_pinned, state) {
            async_wait_pinned.set_result(false);
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
            Opcode::Cleanup => true,
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
            Opcode::Cleanup => 0,
        }
    }
}
