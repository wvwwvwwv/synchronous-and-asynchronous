//! [`Barrier`] is a synchronization primitive that enables multiple tasks to start execution at the
//! same time.

#![deny(unsafe_code)]

use std::fmt;
use std::pin::{Pin, pin};
#[cfg(not(feature = "loom"))]
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering::{self, Acquire, Relaxed, Release};

#[cfg(feature = "loom")]
use loom::sync::atomic::AtomicUsize;

use crate::Pager;
use crate::opcode::Opcode;
use crate::pager::{self, SyncResult};
use crate::sync_primitive::SyncPrimitive;
use crate::wait_queue::{Entry, PinnedEntry, WaitQueue};

/// [`Barrier`] is a synchronization primitive that enables multiple tasks to start execution at the
/// same time.
pub struct Barrier {
    /// [`Semaphore`] state.
    state: AtomicUsize,
}

impl Barrier {
    /// Maximum number of tasks to block.
    pub const MAX_TASKS: usize = WaitQueue::DATA_MASK;

    /// Creates a new [`Barrier`] that can block the given number of tasks.
    ///
    /// The maximum number of tasks to block is defined by [`MAX_TASKS`](Self::MAX_TASKS), and if a
    /// value greater than or equal to [`MAX_TASKS`](Self::MAX_TASKS) is provided, it will be set to
    /// [`MAX_TASKS`](Self::MAX_TASKS).
    ///
    /// # Examples
    ///
    /// ```
    /// use saa::Barrier;
    /// use std::sync::atomic::Ordering::Relaxed;
    ///
    /// let barrier = Barrier::with_count(11);
    ///
    /// assert_eq!(semaphore.available_permits(Relaxed), 11);
    ///
    /// assert!(semaphore.try_acquire_many(11));
    /// assert!(!semaphore.is_open(Relaxed));
    /// ```
    #[inline]
    #[must_use]
    pub fn with_count(count: usize) -> Self {
        let adjusted_count = Self::MAX_TASKS.min(count);
        Self {
            state: AtomicUsize::new(adjusted_count),
        }
    }

    /// Gets a permit from the semaphore asynchronously.
    ///
    /// # Examples
    ///
    /// ```
    /// use saa::Semaphore;
    /// use std::sync::atomic::Ordering::Relaxed;
    ///
    /// let semaphore = Semaphore::default();
    ///
    /// async {
    ///     semaphore.acquire_async().await;
    ///     assert_eq!(semaphore.available_permits(Relaxed), Semaphore::MAX_PERMITS - 1);
    /// };
    /// ```
    #[inline]
    pub async fn wait_async(&self) {
        self.acquire_async_with_internal(1, || {}).await;
    }

    /// Gets a permit from the semaphore asynchronously with a wait callback.
    ///
    /// The callback is invoked when the task starts waiting for a permit.
    ///
    /// # Examples
    ///
    /// ```
    /// use saa::Semaphore;
    /// use std::sync::atomic::Ordering::Relaxed;
    ///
    /// let semaphore = Semaphore::default();
    ///
    /// async {
    ///     let mut wait = false;
    ///     semaphore.acquire_async_with(|| { wait = true; }).await;
    ///     assert_eq!(semaphore.available_permits(Relaxed), Semaphore::MAX_PERMITS - 1);
    ///     assert!(!wait);
    /// };
    /// ```
    #[inline]
    pub async fn wait_async_with<F: FnOnce()>(&self, begin_wait: F) {
        self.acquire_async_with_internal(1, begin_wait).await;
    }

    /// Gets a permit from the semaphore synchronously.
    ///
    /// # Examples
    ///
    /// ```
    /// use saa::Semaphore;
    /// use std::sync::atomic::Ordering::Relaxed;
    ///
    /// let semaphore = Semaphore::default();
    ///
    /// semaphore.acquire_sync();
    /// assert_eq!(semaphore.available_permits(Relaxed), Semaphore::MAX_PERMITS - 1);
    /// ```
    #[inline]
    pub fn wait_sync(&self) {
        self.acquire_many_sync_with(1, || ());
    }

    /// Gets multiple permits from the semaphore synchronously with a wait callback.
    ///
    /// The callback is invoked when the task starts waiting for permits.
    ///
    /// # Examples
    ///
    /// ```
    /// use saa::Semaphore;
    /// use std::sync::atomic::Ordering::Relaxed;
    ///
    /// let semaphore = Semaphore::default();
    ///
    /// let mut wait = false;
    /// semaphore.acquire_sync_with(|| { wait = true; });
    /// assert_eq!(semaphore.available_permits(Relaxed), Semaphore::MAX_PERMITS - 1);
    /// assert!(!wait);
    /// ```
    #[inline]
    pub fn wait_sync_with<F: FnOnce()>(&self, begin_wait: F) {
        self.acquire_many_sync_with(1, begin_wait);
    }

    /// Registers a [`Pager`] to allow it to get a permit remotely.
    ///
    /// `is_sync` indicates whether the [`Pager`] will be polled asynchronously (`false`) or
    /// synchronously (`true`).
    ///
    /// Returns `false` if the [`Pager`] was already registered, or if the count is greater than the
    /// maximum number of permits.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::pin::pin;
    ///
    /// use saa::{Pager, Semaphore};
    ///
    /// let semaphore = Semaphore::default();
    ///
    /// let mut pinned_pager = pin!(Pager::default());
    ///
    /// assert!(semaphore.register_pager(&mut pinned_pager, 1, true));
    /// assert!(!semaphore.register_pager(&mut pinned_pager, 1, true));
    ///
    /// assert!(pinned_pager.poll_sync().is_ok());
    /// ```
    #[inline]
    pub fn register_pager<'s>(
        &'s self,
        pager: &mut Pin<&mut Pager<'s, Self>>,
        count: usize,
        is_sync: bool,
    ) -> bool {
        if count > Self::MAX_TASKS || pager.is_registered() {
            return false;
        }
        let Ok(count) = u8::try_from(count) else {
            return false;
        };

        pager
            .wait_queue()
            .construct(self, Opcode::Semaphore(count), is_sync);

        loop {
            let (result, state) = self.try_acquire_internal(count);
            if result {
                pager.wait_queue().entry().set_result(0);
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

    /// Acquires permits asynchronously.
    #[inline]
    async fn acquire_async_with_internal<F: FnOnce()>(
        &self,
        count: usize,
        mut begin_wait: F,
    ) -> bool {
        if count > Semaphore::MAX_PERMITS {
            return false;
        }
        let Ok(count) = u8::try_from(count) else {
            return false;
        };
        loop {
            let (result, state) = self.try_acquire_internal(count);
            if result {
                return true;
            }
            debug_assert!(state & WaitQueue::ADDR_MASK != 0 || state & WaitQueue::DATA_MASK != 0);

            let async_wait = pin!(WaitQueue::default());
            async_wait
                .as_ref()
                .construct(self, Opcode::Semaphore(count), false);
            if let Some(returned) =
                self.try_push_wait_queue_entry(async_wait.as_ref(), state, begin_wait)
            {
                begin_wait = returned;
                continue;
            }

            PinnedEntry(Pin::new(async_wait.entry())).await;
            return true;
        }
    }

    /// Tries to acquire a permit.
    #[inline]
    fn try_acquire_internal(&self, count: u8) -> (bool, usize) {
        let mut state = self.state.load(Acquire);
        loop {
            if state & WaitQueue::ADDR_MASK != 0
                || (state & WaitQueue::DATA_MASK) + usize::from(count) > Self::MAX_TASKS
            {
                // There is a waiting thread, or the semaphore can no longer be shared.
                return (false, state);
            }

            match self
                .state
                .compare_exchange(state, state + usize::from(count), Acquire, Acquire)
            {
                Ok(_) => return (true, 0),
                Err(new_state) => state = new_state,
            }
        }
    }
}

impl fmt::Debug for Barrier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let state = self.state.load(Relaxed);
        let available_permits = Self::MAX_PERMITS - (state & WaitQueue::DATA_MASK);
        let wait_queue_being_processed = state & WaitQueue::LOCKED_FLAG == WaitQueue::LOCKED_FLAG;
        let wait_queue_tail_addr = state & WaitQueue::ADDR_MASK;
        f.debug_struct("WaitQueue")
            .field("state", &state)
            .field("available_permits", &available_permits)
            .field("wait_queue_being_processed", &wait_queue_being_processed)
            .field("wait_queue_tail_addr", &wait_queue_tail_addr)
            .finish()
    }
}

impl Default for Barrier {
    /// The default number of tasks to block is [`MAX_TASKS`](Self::MAX_TASKS).
    #[inline]
    fn default() -> Self {
        Self {
            state: AtomicUsize::new(Self::MAX_TASKS),
        }
    }
}

impl SyncPrimitive for Barrier {
    #[inline]
    fn state(&self) -> &AtomicUsize {
        &self.state
    }

    #[inline]
    fn max_shared_owners() -> usize {
        Self::MAX_TASKS
    }

    #[inline]
    fn drop_wait_queue_entry(entry: &Entry) {
        Self::force_remove_wait_queue_entry(entry);
    }
}

impl SyncResult for Barrier {
    type Result = Result<(), pager::Error>;

    #[inline]
    fn to_result(_: u8, pager_error: Option<pager::Error>) -> Self::Result {
        pager_error.map_or_else(|| Ok(()), Err)
    }
}
