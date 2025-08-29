//! [`Lock`] is a low-level locking primitive for both synchronous and asynchronous operations.

#![deny(unsafe_code)]

use std::fmt;
#[cfg(not(feature = "loom"))]
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering::{self, AcqRel, Acquire, Relaxed, Release};
use std::thread::yield_now;

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

    /// Poisoned state.
    const POISONED_STATE: usize = WaitQueue::LOCKED_FLAG;

    /// Acquired the desired lock.
    const ACQUIRED: u8 = 0_u8;

    /// Could not acquire the desired lock.
    const NOT_ACQUIRED: u8 = 1_u8;

    /// Poisoned error code.
    const POISONED: u8 = 2_u8;

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
        state != Self::POISONED_STATE && (state & WaitQueue::DATA_MASK) == 0
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

    /// Returns `true` if the [`Lock`] is poisoned.
    ///
    /// # Examples
    ///
    /// ```
    /// use saa::Lock;
    /// use std::sync::atomic::Ordering::Relaxed;
    ///
    /// let lock = Lock::default();
    /// assert!(!lock.is_poisoned(Relaxed));
    ///
    /// lock.lock_sync();
    /// assert!(lock.poison_lock());
    /// assert!(lock.is_poisoned(Relaxed));
    /// ```
    #[inline]
    pub fn is_poisoned(&self, mo: Ordering) -> bool {
        self.state.load(mo) == Self::POISONED_STATE
    }

    /// Acquires an exclusive lock asynchronously.
    ///
    /// Returns `false` if the lock is poisoned.
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
    pub async fn lock_async(&self) -> bool {
        self.lock_async_with(|| ()).await
    }

    /// Acquires an exclusive lock asynchronously with a wait callback provided.
    ///
    /// Returns `false` if the lock is poisoned. The callback is invoked when the task starts
    /// waiting for a lock.
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
    pub async fn lock_async_with<F: FnOnce()>(&self, mut begin_wait: F) -> bool {
        loop {
            let (result, state) = self.try_lock_internal();
            if result == Self::ACQUIRED {
                return true;
            } else if result == Self::POISONED {
                return false;
            }
            debug_assert_eq!(result, Self::NOT_ACQUIRED);

            match self
                .wait_resources_async(state, Opcode::Exclusive, begin_wait)
                .await
            {
                Ok(result) => {
                    debug_assert!(result == Self::ACQUIRED || result == Self::POISONED);
                    return result == Self::ACQUIRED;
                }
                Err(returned) => begin_wait = returned,
            }
        }
    }

    /// Acquires an exclusive lock synchronously.
    ///
    /// Returns `false` if the lock is poisoned.
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
    pub fn lock_sync(&self) -> bool {
        self.lock_sync_with(|| ())
    }

    /// Acquires an exclusive lock synchronously with a wait callback provided.
    ///
    /// Returns `false` if the lock is poisoned. The callback is invoked when the task starts
    /// waiting for a lock.
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
    pub fn lock_sync_with<F: FnOnce()>(&self, mut begin_wait: F) -> bool {
        loop {
            let (result, state) = self.try_lock_internal();
            if result == Self::ACQUIRED {
                return true;
            } else if result == Self::POISONED {
                return false;
            }
            debug_assert_eq!(result, Self::NOT_ACQUIRED);

            match self.wait_resources_sync(state, Opcode::Exclusive, begin_wait) {
                Ok(result) => {
                    debug_assert!(result == Self::ACQUIRED || result == Self::POISONED);
                    return result == Self::ACQUIRED;
                }
                Err(returned) => begin_wait = returned,
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
        self.try_lock_internal().0 == Self::ACQUIRED
    }

    /// Acquires a shared lock asynchronously.
    ///
    /// Returns `false` if the lock is poisoned.
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
    pub async fn share_async(&self) -> bool {
        self.share_async_with(|| ()).await
    }

    /// Acquires a shared lock asynchronously with a wait callback provided.
    ///
    /// Returns `false` if the lock is poisoned. The callback is invoked when the task starts
    /// waiting for a lock.
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
    pub async fn share_async_with<F: FnOnce()>(&self, mut begin_wait: F) -> bool {
        loop {
            let (result, state) = self.try_share_internal();
            if result == Self::ACQUIRED {
                return true;
            } else if result == Self::POISONED {
                return false;
            }
            debug_assert_eq!(result, Self::NOT_ACQUIRED);

            match self
                .wait_resources_async(state, Opcode::Shared, begin_wait)
                .await
            {
                Ok(result) => {
                    debug_assert!(result == Self::ACQUIRED || result == Self::POISONED);
                    return result == Self::ACQUIRED;
                }
                Err(returned) => begin_wait = returned,
            }
        }
    }

    /// Acquires a shared lock synchronously.
    ///
    /// Returns `false` if the lock is poisoned.
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
    pub fn share_sync(&self) -> bool {
        self.share_sync_with(|| ())
    }

    /// Acquires a shared lock synchronously with a wait callback provided.
    ///
    /// Returns `false` if the lock is poisoned. The callback is invoked when the task starts
    /// waiting for a lock.
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
    pub fn share_sync_with<F: FnOnce()>(&self, mut begin_wait: F) -> bool {
        loop {
            let (result, state) = self.try_share_internal();
            if result == Self::ACQUIRED {
                return true;
            } else if result == Self::POISONED {
                return false;
            }
            debug_assert_eq!(result, Self::NOT_ACQUIRED);

            match self.wait_resources_sync(state, Opcode::Shared, begin_wait) {
                Ok(result) => {
                    debug_assert!(result == Self::ACQUIRED || result == Self::POISONED);
                    return result == Self::ACQUIRED;
                }
                Err(returned) => begin_wait = returned,
            }
        }
    }

    /// Tries to acquire a shared lock.
    ///
    /// Returns `false` if an exclusive lock was acquired, the number of shared owners has reached
    /// [`Self::MAX_SHARED_OWNERS`], or the lock is poisoned.
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
        self.try_share_internal().0 == Self::ACQUIRED
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
    /// assert!(lock.release_share());
    ///
    /// lock.lock_sync();
    /// lock.poison_lock();
    ///
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

    /// Poisons the lock with an exclusive lock held.
    ///
    /// Returns `false` if an exclusive lock was not held, or the lock was already poisoned.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::sync::atomic::Ordering::Relaxed;
    ///
    /// use saa::Lock;
    ///
    /// let lock = Lock::default();
    ///
    /// assert!(!lock.poison_lock());
    ///
    /// lock.lock_sync();
    ///
    /// assert!(lock.poison_lock());
    /// assert!(lock.is_poisoned(Relaxed));
    ///
    /// assert!(!lock.poison_lock());
    ///
    /// assert!(!lock.lock_sync());
    /// assert!(!lock.share_sync());
    /// assert!(!lock.release_lock());
    /// assert!(!lock.release_share());
    /// ```
    #[inline]
    pub fn poison_lock(&self) -> bool {
        match self.state.compare_exchange(
            WaitQueue::DATA_MASK,
            Self::POISONED_STATE,
            Release,
            Relaxed,
        ) {
            Ok(_) => true,
            Err(state) => self.poison_lock_internal(state),
        }
    }

    /// Clears poison from the lock.
    ///
    /// Returns `true` if the lock was successfully cleared.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::sync::atomic::Ordering::Relaxed;
    ///
    /// use saa::Lock;
    ///
    /// let lock = Lock::default();
    ///
    /// assert!(!lock.poison_lock());
    ///
    /// lock.lock_sync();
    ///
    /// assert!(lock.poison_lock());
    /// assert!(lock.clear_poison());
    /// assert!(!lock.is_poisoned(Relaxed));
    /// ```
    #[inline]
    pub fn clear_poison(&self) -> bool {
        self.state
            .compare_exchange(Self::POISONED_STATE, 0, Release, Relaxed)
            .is_ok()
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
    ///
    /// lock.poison_lock();
    ///
    /// assert!(!lock.release_share());
    /// ```
    #[inline]
    pub fn release_share(&self) -> bool {
        match self.state.compare_exchange(1, 0, Release, Relaxed) {
            Ok(_) => true,
            Err(state) => self.release_loop(state, Opcode::Shared),
        }
    }

    /// Tries to acquire an exclusive lock.
    fn try_lock_internal(&self) -> (u8, usize) {
        let Err(mut state) = self
            .state
            .compare_exchange(0, WaitQueue::DATA_MASK, Acquire, Acquire)
        else {
            return (Self::ACQUIRED, 0);
        };

        loop {
            if state == Self::POISONED_STATE {
                return (Self::POISONED, state);
            } else if state & WaitQueue::ADDR_MASK != 0 || state & WaitQueue::DATA_MASK != 0 {
                // There is a waiting thread, and this should not acquire the lock.
                return (Self::NOT_ACQUIRED, state);
            }

            if state & WaitQueue::DATA_MASK == 0 {
                match self.state.compare_exchange(
                    state,
                    state | WaitQueue::DATA_MASK,
                    Acquire,
                    Acquire,
                ) {
                    Ok(_) => return (Self::ACQUIRED, 0),
                    Err(new_state) => state = new_state,
                }
            }
        }
    }

    /// Tries to acquire a shared lock.
    fn try_share_internal(&self) -> (u8, usize) {
        let Err(mut state) = self.state.compare_exchange(0, 1, Acquire, Acquire) else {
            return (Self::ACQUIRED, 0);
        };

        loop {
            if state == Self::POISONED_STATE {
                return (Self::POISONED, state);
            } else if state & WaitQueue::ADDR_MASK != 0
                || state & WaitQueue::DATA_MASK >= Self::MAX_SHARED_OWNERS
            {
                // There is a waiting thread, or the lock can no longer be shared.
                return (Self::NOT_ACQUIRED, state);
            }

            match self
                .state
                .compare_exchange(state, state + 1, Acquire, Acquire)
            {
                Ok(_) => return (Self::ACQUIRED, 0),
                Err(new_state) => state = new_state,
            }
        }
    }

    /// Poisons the lock.
    fn poison_lock_internal(&self, mut state: usize) -> bool {
        loop {
            if state == Self::POISONED_STATE || state & WaitQueue::DATA_MASK != WaitQueue::DATA_MASK
            {
                // Already poisoned or the lock is not held by the current thread.
                return false;
            }
            if state & WaitQueue::LOCKED_FLAG == WaitQueue::LOCKED_FLAG {
                // This only happens when an asynchronous task is being cancelled.
                yield_now();
                state = self.state.load(Relaxed);
                continue;
            }

            match self
                .state
                .compare_exchange(state, Self::POISONED_STATE, AcqRel, Relaxed)
            {
                Ok(prev_state) => {
                    let entry_addr = prev_state & WaitQueue::ADDR_MASK;
                    if entry_addr != 0 {
                        WaitQueue::iter_forward(WaitQueue::addr_to_ptr(entry_addr), |entry, _| {
                            entry.set_result(Self::POISONED);
                            false
                        });
                    }

                    return true;
                }
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
        let poisoned = state == Self::POISONED_STATE;
        let wait_queue_being_processed = state & WaitQueue::LOCKED_FLAG == WaitQueue::LOCKED_FLAG;
        let wait_queue_tail_addr = state & WaitQueue::ADDR_MASK;
        f.debug_struct("WaitQueue")
            .field("state", &state)
            .field("locked", &locked)
            .field("share_count", &share_count)
            .field("poisoned", &poisoned)
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
