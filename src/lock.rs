//! Low-level locking primitives for both synchronous and asynchronous operations.

use crate::wait_queue::WaitQueue;
#[cfg(feature = "loom")]
use loom::sync::atomic::AtomicUsize;
use std::pin::Pin;
use std::ptr::{addr_of, null};
#[cfg(not(feature = "loom"))]
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering::{self, AcqRel, Acquire, Relaxed, Release};
use std::{fmt, thread};

/// [`Lock`] is a low-level locking primitive for both synchronous and asynchronous operations.
#[derive(Default)]
pub struct Lock {
    state: AtomicUsize,
}

/// Locking operation types.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
enum Opcode {
    /// Acquire exclusive ownership.
    Exclusive,
    /// Acquire shared ownership.
    Shared,
    /// Cleanup stale wait queue entries..
    Cleanup,
}

impl Lock {
    /// Number of the lock bits.
    const LOCK_BITS: u32 = 6;
    /// Mask for the lock bits.
    const LOCK_MASK: usize = (1_usize << Self::LOCK_BITS) - 1;
    /// Indicates that the [`Lock`] is exclusively locked.
    const EXCLUSIVE: usize = Self::LOCK_MASK;
    /// Represents a single shared owner.
    const SHARED: usize = 1_usize;
    /// Maximum number of shared owners.
    const MAX_SHARED_OWNERS: usize = Self::LOCK_MASK - Self::SHARED;

    /// Indicates that the wait queue is being processed by a thread.
    const WAIT_QUEUE_LOCKED: usize = 1_usize << Self::LOCK_BITS;

    /// Mask to extract a wait queue entry pointer.
    const ADDR_MASK: usize = !((1_usize << (1 + Self::LOCK_BITS)) - 1);

    /// Returns `true` if the lock is currently free.
    ///
    /// # Examples
    ///
    /// ```
    /// use saa::Lock;
    /// use std::sync::atomic::Ordering::Relaxed;
    ///
    /// let lock = Lock::default();
    /// assert!(lock.is_free(Relaxed));
    ///
    /// lock.lock_exclusive_sync();
    /// assert!(!lock.is_free(Relaxed));
    /// ```
    #[inline]
    pub fn is_free(&self, mo: Ordering) -> bool {
        let state = self.state.load(mo);
        (state & Self::EXCLUSIVE) == 0
    }

    /// Returns `true` if the lock is currently locked.
    ///
    /// # Examples
    ///
    /// ```
    /// use saa::Lock;
    /// use std::sync::atomic::Ordering::Relaxed;
    ///
    /// let lock = Lock::default();
    /// assert!(!lock.is_locked(Relaxed));
    ///
    /// lock.lock_exclusive_sync();
    /// assert!(lock.is_locked(Relaxed));
    /// assert!(!lock.is_shared(Relaxed));
    /// ```
    #[inline]
    pub fn is_locked(&self, mo: Ordering) -> bool {
        (self.state.load(mo) & Self::EXCLUSIVE) == Self::EXCLUSIVE
    }

    /// Returns `true` if the lock is currently shared.
    ///
    /// # Examples
    ///
    /// ```
    /// use saa::Lock;
    /// use std::sync::atomic::Ordering::Relaxed;
    ///
    /// let lock = Lock::default();
    ///
    /// assert!(!lock.is_shared(Relaxed));
    ///
    /// lock.lock_shared_sync();
    /// assert!(lock.is_shared(Relaxed));
    /// assert!(!lock.is_locked(Relaxed));
    /// ```
    #[inline]
    pub fn is_shared(&self, mo: Ordering) -> bool {
        let share_state = self.state.load(mo) & Self::LOCK_MASK;
        share_state != 0 && share_state != Self::EXCLUSIVE
    }

    /// Acquires the exclusive lock, asynchronously.
    #[inline]
    pub async fn lock_exclusive_async(&self) {
        loop {
            let (result, state) = self.try_lock_exclusive_internal();
            if result {
                return;
            }
            if self.lock_wait_async(state).await {
                return;
            }
        }
    }

    /// Acquires the exclusive lock, synchronously.
    ///
    /// # Examples
    ///
    /// ```
    /// use saa::Lock;
    /// use std::sync::atomic::Ordering::Relaxed;
    ///
    /// let lock = Lock::default();
    ///
    /// lock.lock_exclusive_sync();
    ///
    /// assert!(lock.is_locked(Relaxed));
    /// assert!(!lock.try_lock_shared());
    /// ```
    #[inline]
    pub fn lock_exclusive_sync(&self) {
        loop {
            let (result, state) = self.try_lock_exclusive_internal();
            if result {
                return;
            }
            if self.lock_wait_sync(state) {
                return;
            }
        }
    }

    /// Tries to acquire the exclusive lock.
    ///
    /// Returns `false` if the lock has already been locked or shared.
    ///
    /// # Examples
    ///
    /// ```
    /// use saa::Lock;
    ///
    /// let lock = Lock::default();
    ///
    /// assert!(lock.try_lock_exclusive());
    /// assert!(!lock.try_lock_shared());
    /// assert!(!lock.try_lock_exclusive());
    /// ```
    #[inline]
    pub fn try_lock_exclusive(&self) -> bool {
        self.try_lock_exclusive_internal().0
    }

    /// Tries to share the lock.
    #[inline]
    pub async fn lock_shared_async(&self) {
        loop {
            let (result, state) = self.try_lock_shared_internal();
            if result {
                return;
            }
            if self.share_wait_async(state).await {
                return;
            }
        }
    }

    /// Shares the lock.
    ///
    /// Returns `false` if the [`Lock`] has been locked or the maximum number of shared owners has
    /// been reached.
    ///
    /// # Examples
    ///
    /// ```
    /// use saa::Lock;
    /// use std::sync::atomic::Ordering::Relaxed;
    ///
    /// let lock = Lock::default();
    ///
    /// lock.lock_shared_sync();
    ///
    /// assert!(lock.is_shared(Relaxed));
    /// assert!(!lock.try_lock_exclusive());
    /// ```
    #[inline]
    pub fn lock_shared_sync(&self) {
        loop {
            let (result, state) = self.try_lock_shared_internal();
            if result {
                return;
            }
            if self.share_wait_sync(state) {
                return;
            }
        }
    }

    /// Tries to share the lock.
    ///
    /// Returns `false` if the [`Lock`] has been locked or the maximum number of shared owners has
    /// been reached.
    ///
    /// # Examples
    ///
    /// ```
    /// use saa::Lock;
    ///
    /// let lock = Lock::default();
    ///
    /// assert!(lock.try_lock_shared());
    /// assert!(lock.try_lock_shared());
    /// assert!(!lock.try_lock_exclusive());
    /// ```
    #[inline]
    pub fn try_lock_shared(&self) -> bool {
        self.try_lock_shared_internal().0
    }

    /// Unlocks the lock.
    ///
    /// Returns `true` if the lock was previously locked.
    ///
    /// # Examples
    ///
    /// ```
    /// use saa::Lock;
    ///
    /// let lock = Lock::default();
    ///
    /// lock.lock_exclusive_sync();
    ///
    /// assert!(lock.unlock_exclusive());
    /// assert!(!lock.unlock_exclusive());
    ///
    /// assert!(lock.try_lock_shared());
    /// assert!(!lock.unlock_exclusive());
    /// ```
    #[inline]
    pub fn unlock_exclusive(&self) -> bool {
        match self
            .state
            .compare_exchange(Self::EXCLUSIVE, 0, Acquire, Relaxed)
        {
            Ok(_) => true,
            Err(state) => self.release_loop(state, Opcode::Exclusive),
        }
    }

    /// Unshares the lock.
    ///
    /// Returns `true` if the lock was previously shared.
    ///
    /// # Examples
    ///
    /// ```
    /// use saa::Lock;
    ///
    /// let lock = Lock::default();
    ///
    /// lock.lock_shared_sync();
    /// lock.lock_shared_sync();
    ///
    /// assert!(lock.unlock_shared());
    ///
    /// assert!(!lock.try_lock_exclusive());
    /// assert!(lock.unlock_shared());
    ///
    /// assert!(!lock.unlock_shared());
    /// assert!(lock.try_lock_exclusive());
    /// ```
    #[inline]
    pub fn unlock_shared(&self) -> bool {
        match self
            .state
            .compare_exchange(Self::SHARED, 0, Acquire, Relaxed)
        {
            Ok(_) => true,
            Err(state) => self.release_loop(state, Opcode::Shared),
        }
    }

    /// Tests whether dropping a wait queue entry without completing it is safe.
    #[cfg(test)]
    pub fn test_drop_wait_queue_entry(&self, exclusive: bool) {
        let (result, state) = if exclusive {
            self.try_lock_exclusive_internal()
        } else {
            self.try_lock_shared_internal()
        };
        if result {
            return;
        }
        let mut async_wait = WaitQueue::new(
            if exclusive {
                Self::EXCLUSIVE
            } else {
                Self::SHARED
            },
            Some((self.self_addr(), Self::cleanup_wait_queue)),
        );
        let mut async_wait_pinned = Pin::new(&mut async_wait);
        if !self.try_push_wait_queue_entry(&mut async_wait_pinned, state) {
            async_wait_pinned.set_result(false);
        }
    }

    fn self_addr(&self) -> usize {
        let self_ptr: *const Self = addr_of!(*self);
        self_ptr as usize
    }

    fn cleanup_wait_queue(wait_queue: &WaitQueue, self_addr: usize) {
        let this: &Self = unsafe { &*(self_addr as *const Self) };
        let wait_queue_ptr: *const WaitQueue = addr_of!(*wait_queue);
        let wait_queue_addr = wait_queue_ptr as usize;

        // Remove the wait queue entry from the wait queue list.
        let mut state = this.state.load(Acquire);
        if state & Self::ADDR_MASK != 0 {
            while let Err(new_state) =
                this.try_process_wait_queue(state, wait_queue_addr, Opcode::Cleanup)
            {
                if new_state & Self::WAIT_QUEUE_LOCKED == Self::WAIT_QUEUE_LOCKED {
                    // Another thread is processing the wait queue.
                    thread::yield_now();
                    state = new_state;
                    debug_assert_ne!(state & Self::ADDR_MASK, 0);
                    continue;
                }
                // The wait queue is empty.
                debug_assert_eq!(new_state & Self::ADDR_MASK, 0);
                break;
            }
        }

        // Release any acquired locks if the wait queue was processed.
        if wait_queue.async_poll_completed() {
            if wait_queue.desired_resources() == Self::EXCLUSIVE {
                this.unlock_exclusive();
            } else {
                this.unlock_shared();
            }
        }
    }

    async fn lock_wait_async(&self, state: usize) -> bool {
        debug_assert!(state & Self::ADDR_MASK != 0 || state & Self::LOCK_MASK != 0);
        let mut async_wait = WaitQueue::new(
            Self::EXCLUSIVE,
            Some((self.self_addr(), Self::cleanup_wait_queue)),
        );
        let mut async_wait_pinned = Pin::new(&mut async_wait);
        if !self.try_push_wait_queue_entry(&mut async_wait_pinned, state) {
            async_wait_pinned.set_result(false);
        }
        async_wait_pinned.await
    }

    #[must_use]
    fn lock_wait_sync(&self, state: usize) -> bool {
        debug_assert!(state & Self::ADDR_MASK != 0 || state & Self::LOCK_MASK != 0);
        let mut sync_wait = WaitQueue::new(Self::EXCLUSIVE, None);
        let mut sync_wait_pinned = Pin::new(&mut sync_wait);
        if !self.try_push_wait_queue_entry(&mut sync_wait_pinned, state) {
            sync_wait_pinned.set_result(false);
        }
        sync_wait_pinned.poll_result()
    }

    fn try_lock_exclusive_internal(&self) -> (bool, usize) {
        let Err(mut state) = self
            .state
            .compare_exchange(0, Self::EXCLUSIVE, Acquire, Relaxed)
        else {
            return (true, 0);
        };

        loop {
            if state & Self::ADDR_MASK != 0 || state & Self::LOCK_MASK != 0 {
                // There is a waiting thread, and this should not acquire the lock.
                return (false, state);
            }

            if state & Self::LOCK_MASK == 0 {
                match self
                    .state
                    .compare_exchange(state, state | Self::EXCLUSIVE, Acquire, Relaxed)
                {
                    Ok(_) => return (true, 0),
                    Err(new_state) => state = new_state,
                }
            }
        }
    }

    async fn share_wait_async(&self, state: usize) -> bool {
        debug_assert!(
            state & Self::ADDR_MASK != 0 || state & Self::LOCK_MASK >= Self::MAX_SHARED_OWNERS
        );
        let mut async_wait = WaitQueue::new(
            Self::SHARED,
            Some((self.self_addr(), Self::cleanup_wait_queue)),
        );
        let mut async_wait_pinned = Pin::new(&mut async_wait);
        if !self.try_push_wait_queue_entry(&mut async_wait_pinned, state) {
            async_wait_pinned.set_result(false);
        }
        async_wait_pinned.await
    }

    #[must_use]
    fn share_wait_sync(&self, state: usize) -> bool {
        debug_assert!(
            state & Self::ADDR_MASK != 0 || state & Self::LOCK_MASK >= Self::MAX_SHARED_OWNERS
        );
        let mut sync_wait = WaitQueue::new(Self::SHARED, None);
        let mut sync_wait_pinned = Pin::new(&mut sync_wait);
        if !self.try_push_wait_queue_entry(&mut sync_wait_pinned, state) {
            sync_wait_pinned.set_result(false);
        }
        sync_wait_pinned.poll_result()
    }

    fn try_lock_shared_internal(&self) -> (bool, usize) {
        let Err(mut state) = self
            .state
            .compare_exchange(0, Self::SHARED, Acquire, Relaxed)
        else {
            return (true, 0);
        };

        loop {
            if state & Self::ADDR_MASK != 0 || state & Self::LOCK_MASK >= Self::MAX_SHARED_OWNERS {
                // There is a waiting thread, or the lock can no longer be shared.
                return (false, state);
            }

            match self
                .state
                .compare_exchange(state, state + Self::SHARED, Acquire, Relaxed)
            {
                Ok(_) => return (true, 0),
                Err(new_state) => state = new_state,
            }
        }
    }

    #[must_use]
    fn try_push_wait_queue_entry(&self, entry: &mut Pin<&mut WaitQueue>, state: usize) -> bool {
        let entry_addr = WaitQueue::ref_to_ptr(entry) as usize;
        debug_assert_eq!(entry_addr & (!Self::ADDR_MASK), 0);

        entry.update_next_entry_ptr(WaitQueue::addr_to_ptr(state & Self::ADDR_MASK));
        let next_state = (state & (!Self::ADDR_MASK)) | entry_addr;
        self.state
            .compare_exchange(state, next_state, AcqRel, Acquire)
            .is_ok()
    }

    fn can_release(state: usize, mode: Opcode) -> bool {
        match mode {
            Opcode::Shared => {
                let count = state & Self::LOCK_MASK;
                count >= Self::SHARED && count != Self::EXCLUSIVE
            }
            Opcode::Exclusive => {
                let count = state & Self::LOCK_MASK;
                count == Self::EXCLUSIVE
            }
            Opcode::Cleanup => true,
        }
    }

    fn release_count(mode: Opcode) -> usize {
        match mode {
            Opcode::Shared => Self::SHARED,
            Opcode::Exclusive => Self::EXCLUSIVE,
            Opcode::Cleanup => 0,
        }
    }

    /// Releases the supplied count of shares of the lock.
    ///
    /// Returns `false` if the count cannot be subtracted from the lock.
    fn release_loop(&self, mut state: usize, mode: Opcode) -> bool {
        while Self::can_release(state, mode) {
            if state & Self::ADDR_MASK == 0
                || state & Self::WAIT_QUEUE_LOCKED == Self::WAIT_QUEUE_LOCKED
            {
                // In-place unlock or unshare.
                match self.state.compare_exchange(
                    state,
                    state - Self::release_count(mode),
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

    fn unlink(&self, state: usize, entry_addr_to_remove: usize) -> Result<usize, usize> {
        debug_assert_eq!(state & Self::WAIT_QUEUE_LOCKED, Self::WAIT_QUEUE_LOCKED);
        debug_assert_ne!(state & Self::ADDR_MASK, 0);

        let tail_entry_ptr = WaitQueue::addr_to_ptr(state & Self::ADDR_MASK);
        let target_ptr = WaitQueue::addr_to_ptr(entry_addr_to_remove);
        let mut result = Ok(state);
        WaitQueue::any_forward(tail_entry_ptr, |entry, next_entry| {
            if WaitQueue::ref_to_ptr(entry) == target_ptr {
                if let Some(prev_entry) = unsafe { entry.prev_entry_ptr().as_ref() } {
                    prev_entry.update_next_entry_ptr(entry.next_entry_ptr());
                } else {
                    let next_entry_ptr = next_entry.map_or(null(), WaitQueue::ref_to_ptr);
                    let next_state = (state & !Self::ADDR_MASK) | next_entry_ptr as usize;
                    match self
                        .state
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

    /// Tries to start processing the wait queue by marking that the current thread is processing
    /// the wait queue and subtracting the count from the `LOCK_SHARE_COUNT` field.
    ///
    /// Returns an error if another thread is processing the wait queue or the count cannot be
    /// subtracted.
    fn try_start_process_wait_queue(&self, mut state: usize, mode: Opcode) -> Result<usize, usize> {
        if state & Self::ADDR_MASK == 0
            || state & Self::WAIT_QUEUE_LOCKED == Self::WAIT_QUEUE_LOCKED
            || !Self::can_release(state, mode)
        {
            return Err(state);
        }

        let mut next_state = (state | Self::WAIT_QUEUE_LOCKED) - Self::release_count(mode);
        while let Err(new_state) = self
            .state
            .compare_exchange(state, next_state, AcqRel, Relaxed)
        {
            if new_state & Self::ADDR_MASK == 0
                || new_state & Self::WAIT_QUEUE_LOCKED == Self::WAIT_QUEUE_LOCKED
                || !Self::can_release(new_state, mode)
            {
                return Err(new_state);
            }
            state = new_state;
            next_state = (state | Self::WAIT_QUEUE_LOCKED) - Self::release_count(mode);
        }
        Ok(next_state)
    }

    /// Tries to process the wait queue and subtracts the release count from the lock.
    ///
    /// Returns `Err` if marking that it is processing the wait queue failed, the wait queue no
    /// longer needs to be processed, of the count exceeds the `LOCK_SHARE_COUNT` field.
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
            debug_assert_eq!(state & Self::WAIT_QUEUE_LOCKED, Self::WAIT_QUEUE_LOCKED);

            if entry_addr_to_remove != 0 {
                state = match self.unlink(state, entry_addr_to_remove) {
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

            let tail_entry_ptr = WaitQueue::addr_to_ptr(state & Self::ADDR_MASK);
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

            let count = state & Self::LOCK_MASK;
            let mut transferred_count = 0;
            let mut obsolete_entry_ptr: *const WaitQueue = null();
            let mut unlocked = false;

            WaitQueue::any_backward(head_entry_ptr, |entry, prev_entry| {
                let desired_count = entry.desired_resources();
                if count + transferred_count == 0
                    || count + transferred_count + desired_count <= Self::MAX_SHARED_OWNERS
                {
                    // The entry can inherit ownerhip.
                    if prev_entry.is_none() {
                        // This is the tail of the wait queue: try to reset.
                        debug_assert_eq!(tail_entry_ptr, addr_of!(*entry));
                        if self
                            .state
                            .compare_exchange(
                                state,
                                count + transferred_count + desired_count,
                                AcqRel,
                                Acquire,
                            )
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
                    transferred_count += desired_count;
                    unlocked
                } else {
                    // Unlink those that have succeeded in acquiring shared ownership.
                    entry.update_next_entry_ptr(null());
                    head_entry_ptr = WaitQueue::ref_to_ptr(entry);
                    true
                }
            });
            debug_assert_eq!(obsolete_entry_ptr.is_null(), transferred_count == 0);

            if !unlocked {
                unlocked = if self
                    .state
                    .fetch_update(AcqRel, Acquire, |new_state| {
                        let new_count = new_state & Self::LOCK_MASK;
                        if new_state == state || new_count == count {
                            let next_state =
                                (new_state & Self::ADDR_MASK) | (new_count + transferred_count);
                            Some(next_state)
                        } else {
                            None
                        }
                    })
                    .is_err()
                {
                    state = self.state.fetch_add(transferred_count, Release) + transferred_count;
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
}

impl fmt::Debug for Lock {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let state = self.state.load(Relaxed);
        let lock_share_state = state & Self::LOCK_MASK;
        let locked = state == Self::EXCLUSIVE;
        let share_count = if locked {
            0
        } else {
            lock_share_state / Self::SHARED
        };
        let wait_queue_being_processed = state & Self::WAIT_QUEUE_LOCKED == Self::WAIT_QUEUE_LOCKED;
        let wait_queue_tail_addr = state & Self::ADDR_MASK;
        f.debug_struct("WaitQueue")
            .field("state", &state)
            .field("locked", &locked)
            .field("share_count", &share_count)
            .field("wait_queue_being_processed", &wait_queue_being_processed)
            .field("wait_queue_tail_addr", &wait_queue_tail_addr)
            .finish()
    }
}
