//! [`Lock`] is a low-level locking primitive for both synchronous and asynchronous operations.

#![deny(unsafe_code)]

use std::fmt;
#[cfg(not(feature = "loom"))]
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering::{self, Acquire, Relaxed, Release};

#[cfg(feature = "loom")]
use loom::sync::atomic::AtomicUsize;

use crate::opcode::Opcode;
use crate::sync_primitive::SyncPrimitive;
use crate::wait_queue::WaitQueue;

/// [`Lock`] is a low-level locking primitive for both synchronous and asynchronous operations.
///
/// The locking semantics is similar to [`RwLock`](std::sync::RwLock), however, [`Lock`] only
/// provides low-level locking and releasing methods, hence forcing the user to manage the scope of
/// acquired locks and resources to protect.
#[derive(Default)]
pub struct Lock {
    /// [`Lock`] state.
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
    /// lock.lock_sync();
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
    /// lock.lock_sync();
    /// assert!(lock.is_locked(Relaxed));
    /// assert!(!lock.is_shared(Relaxed));
    /// ```
    #[inline]
    pub fn is_locked(&self, mo: Ordering) -> bool {
        (self.state.load(mo) & WaitQueue::DATA_MASK) == WaitQueue::DATA_MASK
    }

    /// Returns `true` if shared locks are currently held.
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
    /// lock.share_sync();
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
    ///     lock.lock_async().await;
    ///     assert!(lock.is_locked(Relaxed));
    ///     assert!(!lock.is_shared(Relaxed))
    /// };
    /// ```
    #[inline]
    pub async fn lock_async(&self) {
        loop {
            let (result, state) = self.try_lock_internal();
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
    /// lock.lock_sync();
    ///
    /// assert!(lock.is_locked(Relaxed));
    /// assert!(!lock.try_share());
    /// ```
    #[inline]
    pub fn lock_sync(&self) {
        loop {
            let (result, state) = self.try_lock_internal();
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
    /// assert!(lock.try_lock());
    /// assert!(!lock.try_share());
    /// assert!(!lock.try_lock());
    /// ```
    #[inline]
    pub fn try_lock(&self) -> bool {
        self.try_lock_internal().0
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
    ///     lock.share_async().await;
    ///     assert!(!lock.is_locked(Relaxed));
    ///     assert!(lock.is_shared(Relaxed))
    /// };
    /// ```
    #[inline]
    pub async fn share_async(&self) {
        loop {
            let (result, state) = self.try_share_internal();
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
    /// lock.share_sync();
    ///
    /// assert!(lock.is_shared(Relaxed));
    /// assert!(!lock.try_lock());
    /// ```
    #[inline]
    pub fn share_sync(&self) {
        loop {
            let (result, state) = self.try_share_internal();
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
    /// assert!(lock.try_share());
    /// assert!(lock.try_share());
    /// assert!(!lock.try_lock());
    /// ```
    #[inline]
    pub fn try_share(&self) -> bool {
        self.try_share_internal().0
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
    /// lock.lock_sync();
    ///
    /// assert!(lock.release_lock());
    /// assert!(!lock.release_lock());
    ///
    /// assert!(lock.try_share());
    /// assert!(!lock.release_lock());
    /// ```
    #[inline]
    pub fn release_lock(&self) -> bool {
        match self
            .state
            .compare_exchange(WaitQueue::DATA_MASK, 0, Release, Relaxed)
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
    /// lock.share_sync();
    /// lock.share_sync();
    ///
    /// assert!(lock.release_share());
    ///
    /// assert!(!lock.try_lock());
    /// assert!(lock.release_share());
    ///
    /// assert!(!lock.release_share());
    /// assert!(lock.try_lock());
    /// ```
    #[inline]
    pub fn release_share(&self) -> bool {
        match self.state.compare_exchange(1, 0, Release, Relaxed) {
            Ok(_) => true,
            Err(state) => self.release_loop(state, Opcode::Shared),
        }
    }

    /// Tries to acquire an exclusive lock.
    fn try_lock_internal(&self) -> (bool, usize) {
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
    fn try_share_internal(&self) -> (bool, usize) {
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
        let locked = lock_share_state == WaitQueue::DATA_MASK;
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
