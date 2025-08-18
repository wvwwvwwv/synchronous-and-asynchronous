//! [`Lock`] is a low-level locking primitive for both synchronous and asynchronous operations.

#![deny(unsafe_code)]

use crate::sync_primitive::{Opcode, SyncPrimitive};
use crate::wait_queue::WaitQueue;
#[cfg(feature = "loom")]
use loom::sync::atomic::AtomicUsize;
use std::fmt;
#[cfg(not(feature = "loom"))]
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering::{self, Acquire, Relaxed};

/// [`Lock`] is a low-level locking primitive for both synchronous and asynchronous operations.
///
/// The locking semantics is similar to [`RwLock`](std::sync::RwLock), however, [`Lock`] only
/// provides low-level locking and un-locking methods, hence forcing the user to manage the scope of
/// acquired locks and resources to protect.
#[derive(Default)]
pub struct Lock {
    // State of the [`Lock`].
    state: AtomicUsize,
}

impl Lock {
    /// Maximum number of shared owners.
    pub const MAX_SHARED_OWNERS: usize = WaitQueue::DATA_MASK - 1;

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
        (state & WaitQueue::DATA_MASK) == 0
    }

    /// Returns `true` if an exclusive lock is currently held.
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
        (self.state.load(mo) & WaitQueue::DATA_MASK) == WaitQueue::DATA_MASK
    }

    /// Returns `true` if a shared lock is currently held.
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
        let share_state = self.state.load(mo) & WaitQueue::DATA_MASK;
        share_state != 0 && share_state != WaitQueue::DATA_MASK
    }

    /// Acquires an exclusive lock asynchronously.
    ///
    /// # Examples
    ///
    /// ```
    /// use saa::Lock;
    /// use std::sync::atomic::Ordering::Relaxed;
    ///
    /// let lock = Lock::default();
    ///
    /// async {
    ///     lock.lock_exclusive_async().await;
    ///     assert!(lock.is_locked(Relaxed));
    ///     assert!(!lock.is_shared(Relaxed))
    /// };
    /// ```
    #[inline]
    pub async fn lock_exclusive_async(&self) {
        loop {
            let (result, state) = self.try_lock_exclusive_internal();
            if result {
                return;
            }
            if self.wait_resources_async(state, Opcode::Exclusive).await {
                return;
            }
        }
    }

    /// Acquires an exclusive lock synchronously.
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
            if self.wait_resources_sync(state, Opcode::Exclusive) {
                return;
            }
        }
    }

    /// Tries to acquire an exclusive lock.
    ///
    /// Returns `false` if the lock was not free.
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

    /// Acquires a shared lock asynchronously.
    ///
    /// # Examples
    ///
    /// ```
    /// use saa::Lock;
    /// use std::sync::atomic::Ordering::Relaxed;
    ///
    /// let lock = Lock::default();
    ///
    /// async {
    ///     lock.lock_shared_async().await;
    ///     assert!(!lock.is_locked(Relaxed));
    ///     assert!(lock.is_shared(Relaxed))
    /// };
    /// ```
    #[inline]
    pub async fn lock_shared_async(&self) {
        loop {
            let (result, state) = self.try_lock_shared_internal();
            if result {
                return;
            }
            if self.wait_resources_async(state, Opcode::Shared).await {
                return;
            }
        }
    }

    /// Acquires a shared lock synchronously.
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
            if self.wait_resources_sync(state, Opcode::Shared) {
                return;
            }
        }
    }

    /// Tries to acquire a shared lock.
    ///
    /// Returns `false` if an exclusive lock was acquired or the number of shared owners has reached
    /// [`Self::MAX_SHARED_OWNERS`].
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

    /// Releases an exclusive lock.
    ///
    /// Returns `true` if an exclusive lock was previously held and successfully released.
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
            .compare_exchange(WaitQueue::DATA_MASK, 0, Acquire, Relaxed)
        {
            Ok(_) => true,
            Err(state) => self.release_loop(state, Opcode::Exclusive),
        }
    }

    /// Releases a shared lock.
    ///
    /// Returns `true` if a shared lock was previously held and successfully released.
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
        match self.state.compare_exchange(1, 0, Acquire, Relaxed) {
            Ok(_) => true,
            Err(state) => self.release_loop(state, Opcode::Shared),
        }
    }

    /// Tries to acquire an exclusive lock.
    fn try_lock_exclusive_internal(&self) -> (bool, usize) {
        let Err(mut state) = self
            .state
            .compare_exchange(0, WaitQueue::DATA_MASK, Acquire, Relaxed)
        else {
            return (true, 0);
        };

        loop {
            if state & WaitQueue::ADDR_MASK != 0 || state & WaitQueue::DATA_MASK != 0 {
                // There is a waiting thread, and this should not acquire the lock.
                return (false, state);
            }

            if state & WaitQueue::DATA_MASK == 0 {
                match self.state.compare_exchange(
                    state,
                    state | WaitQueue::DATA_MASK,
                    Acquire,
                    Relaxed,
                ) {
                    Ok(_) => return (true, 0),
                    Err(new_state) => state = new_state,
                }
            }
        }
    }

    /// Tries to acquire a shared lock.
    fn try_lock_shared_internal(&self) -> (bool, usize) {
        let Err(mut state) = self.state.compare_exchange(0, 1, Acquire, Relaxed) else {
            return (true, 0);
        };

        loop {
            if state & WaitQueue::ADDR_MASK != 0
                || state & WaitQueue::DATA_MASK >= Self::MAX_SHARED_OWNERS
            {
                // There is a waiting thread, or the lock can no longer be shared.
                return (false, state);
            }

            match self
                .state
                .compare_exchange(state, state + 1, Acquire, Relaxed)
            {
                Ok(_) => return (true, 0),
                Err(new_state) => state = new_state,
            }
        }
    }
}

impl fmt::Debug for Lock {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let state = self.state.load(Relaxed);
        let lock_share_state = state & WaitQueue::DATA_MASK;
        let locked = state == WaitQueue::DATA_MASK;
        let share_count = if locked { 0 } else { lock_share_state };
        let wait_queue_being_processed = state & WaitQueue::LOCKED_FLAG == WaitQueue::LOCKED_FLAG;
        let wait_queue_tail_addr = state & WaitQueue::ADDR_MASK;
        f.debug_struct("WaitQueue")
            .field("state", &state)
            .field("locked", &locked)
            .field("share_count", &share_count)
            .field("wait_queue_being_processed", &wait_queue_being_processed)
            .field("wait_queue_tail_addr", &wait_queue_tail_addr)
            .finish()
    }
}

impl SyncPrimitive for Lock {
    #[inline]
    fn state(&self) -> &AtomicUsize {
        &self.state
    }

    #[inline]
    fn max_shared_owners() -> usize {
        Self::MAX_SHARED_OWNERS
    }
}
