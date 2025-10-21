//! [`Lock`] is a low-level locking primitive for both synchronous and asynchronous operations.

#![deny(unsafe_code)]

use std::fmt;
use std::pin::{Pin, pin};
#[cfg(not(feature = "loom"))]
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering::{self, AcqRel, Acquire, Relaxed, Release};
use std::thread::yield_now;

#[cfg(feature = "loom")]
use loom::sync::atomic::AtomicUsize;

use crate::Pager;
use crate::opcode::Opcode;
use crate::pager::{self, SyncResult};
use crate::sync_primitive::SyncPrimitive;
use crate::wait_queue::{Entry, PinnedEntry, WaitQueue};

/// [`Lock`] is a low-level locking primitive for both synchronous and asynchronous operations.
///
/// The locking semantics are similar to [`RwLock`](std::sync::RwLock), however, [`Lock`] only
/// provides low-level locking and releasing methods, hence forcing the user to manage the scope of
/// acquired locks and the resources to protect.
#[derive(Default)]
pub struct Lock {
    /// [`Lock`] state.
    state: AtomicUsize,
}

/// Operation mode.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Mode {
    /// Acquires an exclusive lock.
    Exclusive,
    /// Acquires a shared lock.
    Shared,
    /// Waits for the [`Lock`] to be free or poisoned.
    ///
    /// [`Self::WaitExclusive`], [`Self::WaitShared`], [`Lock::try_lock`], and [`Lock::try_share`]
    /// provide a way to bypass the fair queuing mechanism without spinning.
    WaitExclusive,
    /// Waits for a shared lock to be available or the [`Lock`] to be poisoned.
    ///
    /// If a [`Self::WaitExclusive`] entry is in front of a [`Self::WaitShared`] entry, the
    /// [`Self::WaitShared`] entry has to wait until the [`Self::WaitExclusive`] entry is processed.
    WaitShared,
}

impl Lock {
    /// Maximum number of shared owners.
    pub const MAX_SHARED_OWNERS: usize = WaitQueue::DATA_MASK - 1;

    /// Poisoned state.
    const POISONED_STATE: usize = WaitQueue::LOCKED_FLAG;

    /// Successfully acquired the desired lock.
    const ACQUIRED: u8 = 0_u8;

    /// Failed to acquire the desired lock.
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
        self.acquire_async_with_internal::<_, true>(|| {}).await
    }

    /// Acquires an exclusive lock asynchronously with a wait callback.
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
    pub async fn lock_async_with<F: FnOnce()>(&self, begin_wait: F) -> bool {
        self.acquire_async_with_internal::<_, true>(begin_wait)
            .await
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

    /// Acquires an exclusive lock synchronously with a wait callback.
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
        self.acquire_async_with_internal::<_, false>(|| ()).await
    }

    /// Acquires a shared lock asynchronously with a wait callback.
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
    pub async fn share_async_with<F: FnOnce()>(&self, begin_wait: F) -> bool {
        self.acquire_async_with_internal::<_, false>(begin_wait)
            .await
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

    /// Acquires a shared lock synchronously with a wait callback.
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
    /// Returns `false` if an exclusive lock is held, the number of shared owners has reached
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

    /// Registers a [`Pager`] to allow it to get an exclusive lock or a shared lock remotely.
    ///
    /// `is_sync` indicates whether the [`Pager`] will be polled asynchronously (`false`) or
    /// synchronously (`true`).
    ///
    /// Returns `false` if the [`Pager`] was already registered.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::pin::pin;
    ///
    /// use saa::{Lock, Pager};
    /// use saa::lock::Mode;
    ///
    /// let lock = Lock::default();
    ///
    /// let mut pinned_pager = pin!(Pager::default());
    ///
    /// assert!(lock.register_pager(&mut pinned_pager, Mode::Exclusive, true));
    /// assert!(!lock.register_pager(&mut pinned_pager, Mode::Exclusive, true));
    ///
    /// assert_eq!(pinned_pager.poll_sync(), Ok(true));
    /// ```
    #[inline]
    pub fn register_pager<'l>(
        &'l self,
        pager: &mut Pin<&mut Pager<'l, Self>>,
        mode: Mode,
        is_sync: bool,
    ) -> bool {
        if pager.is_registered() {
            return false;
        }
        let opcode = match mode {
            Mode::Exclusive => Opcode::Exclusive,
            Mode::Shared => Opcode::Shared,
            Mode::WaitExclusive => Opcode::Wait(u8::try_from(WaitQueue::DATA_MASK).unwrap_or(0)),
            Mode::WaitShared => Opcode::Wait(1),
        };

        pager.wait_queue().construct(self, opcode, is_sync);

        loop {
            let (result, state) = match mode {
                Mode::Exclusive => self.try_lock_internal(),
                Mode::Shared => self.try_share_internal(),
                Mode::WaitExclusive | Mode::WaitShared => {
                    let state = self.state.load(Acquire);
                    let result = if state == usize::from(Self::POISONED) {
                        Self::POISONED
                    } else if (mode == Mode::WaitExclusive && (state & WaitQueue::DATA_MASK) == 0)
                        || (mode == Mode::WaitShared
                            && (state & WaitQueue::DATA_MASK) < Self::MAX_SHARED_OWNERS)
                    {
                        // If an available lock is available, return immediately.
                        Self::ACQUIRED
                    } else {
                        Self::NOT_ACQUIRED
                    };
                    (result, state)
                }
            };
            if result == Self::ACQUIRED || result == Self::POISONED {
                pager.wait_queue().entry().set_result(result);
                break;
            }

            if self
                .try_push_wait_queue_entry(pager.wait_queue(), state, || ())
                .is_none()
            {
                break;
            }
        }
        true
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
    /// Returns `false` if an exclusive lock is not held, or the lock was already poisoned.
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
    #[inline]
    fn try_lock_internal(&self) -> (u8, usize) {
        let Err(state) = self
            .state
            .compare_exchange(0, WaitQueue::DATA_MASK, Acquire, Acquire)
        else {
            return (Self::ACQUIRED, 0);
        };
        self.try_lock_internal_slow(state)
    }

    /// Tries to acquire an exclusive lock, slowly.
    fn try_lock_internal_slow(&self, mut state: usize) -> (u8, usize) {
        loop {
            if state == Self::POISONED_STATE {
                return (Self::POISONED, state);
            } else if state & WaitQueue::ADDR_MASK != 0 || state & WaitQueue::DATA_MASK != 0 {
                // There is a waiting thread, so this thread should not acquire the lock.
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

    /// Acquires an exclusive lock or a shared lock asynchronously.
    #[inline]
    async fn acquire_async_with_internal<F: FnOnce(), const EXCLUSIVE: bool>(
        &self,
        mut begin_wait: F,
    ) -> bool {
        loop {
            let (mut result, state) = if EXCLUSIVE {
                self.try_lock_internal()
            } else {
                self.try_share_internal()
            };
            if result == Self::ACQUIRED {
                return true;
            } else if result == Self::POISONED {
                return false;
            }
            debug_assert_eq!(result, Self::NOT_ACQUIRED);
            debug_assert!(state & WaitQueue::ADDR_MASK != 0 || state & WaitQueue::DATA_MASK != 0);

            let mode = if EXCLUSIVE {
                Opcode::Exclusive
            } else {
                Opcode::Shared
            };
            let async_wait = pin!(WaitQueue::default());
            async_wait.as_ref().construct(self, mode, false);
            if let Some(returned) =
                self.try_push_wait_queue_entry(async_wait.as_ref(), state, begin_wait)
            {
                begin_wait = returned;
                continue;
            }

            result = PinnedEntry(Pin::new(async_wait.entry())).await;
            debug_assert!(result == Self::ACQUIRED || result == Self::POISONED);
            return result == Self::ACQUIRED;
        }
    }

    /// Tries to acquire a shared lock.
    #[inline]
    fn try_share_internal(&self) -> (u8, usize) {
        let Err(state) = self.state.compare_exchange(0, 1, Acquire, Acquire) else {
            return (Self::ACQUIRED, 0);
        };
        self.try_share_internal_slow(state)
    }

    /// Tries to acquire a shared lock, slowly.
    fn try_share_internal_slow(&self, mut state: usize) -> (u8, usize) {
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
                // Already poisoned or the lock is not held exclusively by the current thread.
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
                    // A possible data race where the lock is being poisoned before the one that
                    // woke up the current lock owner has finished processing the wait queue is
                    // prevented by the wait queue processing method itself; `model.rs` proves it.
                    debug_assert_eq!(prev_state & WaitQueue::LOCKED_FLAG, 0);
                    let anchor_ptr = WaitQueue::to_anchor_ptr(prev_state);
                    if !anchor_ptr.is_null() {
                        let tail_entry_ptr = WaitQueue::to_entry_ptr(anchor_ptr);
                        Entry::iter_forward(tail_entry_ptr, false, |entry, _| {
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

    #[inline]
    fn drop_wait_queue_entry(entry: &Entry) {
        Self::force_remove_wait_queue_entry(entry);
    }
}

impl SyncResult for Lock {
    type Result = Result<bool, pager::Error>;

    #[inline]
    fn to_result(value: u8, pager_error: Option<pager::Error>) -> Self::Result {
        pager_error.map_or_else(
            || {
                debug_assert!(value == Self::ACQUIRED || value == Self::POISONED);
                Ok(value == Self::ACQUIRED)
            },
            Err,
        )
    }
}
