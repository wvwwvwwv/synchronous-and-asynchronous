//! [`Semaphore`] is a synchronization primitive that allows a fixed number of threads to access a
//! resource concurrently.

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

/// [`Semaphore`] is a synchronization primitive that allows a fixed number of threads to access a
/// resource concurrently.
#[derive(Default)]
pub struct Semaphore {
    /// [`Semaphore`] state.
    state: AtomicUsize,
}

impl Semaphore {
    /// Maximum number of concurrent owners.
    pub const MAX_PERMITS: usize = WaitQueue::DATA_MASK;

    /// Creates a new [`Semaphore`] with the given number of initially available permits.
    ///
    /// The maximum number of available permits is [`MAX_PERMITS`](Self::MAX_PERMITS), and if a
    /// value greater than or equal to [`MAX_PERMITS`](Self::MAX_PERMITS) is provided, it will be
    /// set to [`MAX_PERMITS`](Self::MAX_PERMITS).
    ///
    /// # Examples
    ///
    /// ```
    /// use saa::Semaphore;
    /// use std::sync::atomic::Ordering::Relaxed;
    ///
    /// let semaphore = Semaphore::with_permits(11);
    ///
    /// assert_eq!(semaphore.available_permits(Relaxed), 11);
    ///
    /// assert!(semaphore.try_acquire_many(11));
    /// assert!(!semaphore.is_open(Relaxed));
    /// ```
    #[inline]
    #[must_use]
    pub fn with_permits(permits: usize) -> Self {
        let adjusted_permits = permits.min(Self::MAX_PERMITS);
        Self {
            state: AtomicUsize::new(Self::MAX_PERMITS - adjusted_permits),
        }
    }

    /// Returns `true` if the semaphore is currently open.
    ///
    /// # Examples
    ///
    /// ```
    /// use saa::Semaphore;
    /// use std::sync::atomic::Ordering::Relaxed;
    ///
    /// let semaphore = Semaphore::default();
    /// assert!(semaphore.is_open(Relaxed));
    ///
    /// assert!(semaphore.try_acquire_many(Semaphore::MAX_PERMITS));
    /// assert!(!semaphore.is_open(Relaxed));
    /// ```
    #[inline]
    pub fn is_open(&self, mo: Ordering) -> bool {
        let state = self.state.load(mo);
        (state & WaitQueue::DATA_MASK) != Self::MAX_PERMITS
    }

    /// Returns `true` if the semaphore is currently closed.
    ///
    /// # Examples
    ///
    /// ```
    /// use saa::Semaphore;
    /// use std::sync::atomic::Ordering::Relaxed;
    ///
    /// let semaphore = Semaphore::default();
    /// assert!(!semaphore.is_closed(Relaxed));
    /// assert!(semaphore.is_open(Relaxed));
    ///
    /// assert!(semaphore.try_acquire());
    /// assert!(!semaphore.is_closed(Relaxed));
    /// assert!(semaphore.is_open(Relaxed));
    ///
    /// semaphore.try_acquire_many(Semaphore::MAX_PERMITS - 1);
    /// assert!(semaphore.is_closed(Relaxed));
    /// ```
    #[inline]
    pub fn is_closed(&self, mo: Ordering) -> bool {
        (self.state.load(mo) & WaitQueue::DATA_MASK) == WaitQueue::DATA_MASK
    }

    /// Returns the number of available permits.
    ///
    /// # Examples
    ///
    /// ```
    /// use saa::Semaphore;
    /// use std::sync::atomic::Ordering::Relaxed;
    ///
    /// let semaphore = Semaphore::default();
    /// assert_eq!(semaphore.available_permits(Relaxed), Semaphore::MAX_PERMITS);
    ///
    /// assert!(semaphore.try_acquire());
    /// assert_eq!(semaphore.available_permits(Relaxed), Semaphore::MAX_PERMITS - 1);
    /// ```
    #[inline]
    pub fn available_permits(&self, mo: Ordering) -> usize {
        Self::MAX_PERMITS - (self.state.load(mo) & WaitQueue::DATA_MASK)
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
    pub async fn acquire_async(&self) {
        self.acquire_many_async_with(1, || ()).await;
    }

    /// Gets a permit from the semaphore asynchronously with a wait callback provided.
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
    pub async fn acquire_async_with<F: FnOnce()>(&self, begin_wait: F) {
        self.acquire_many_async_with(1, begin_wait).await;
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
    pub fn acquire_sync(&self) {
        self.acquire_many_sync_with(1, || ());
    }

    /// Gets multiple permits from the semaphore synchronously with a wait callback provided.
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
    /// let mut wait = false;
    /// semaphore.acquire_sync_with(|| { wait = true; });
    /// assert_eq!(semaphore.available_permits(Relaxed), Semaphore::MAX_PERMITS - 1);
    /// assert!(!wait);
    /// ```
    #[inline]
    pub fn acquire_sync_with<F: FnOnce()>(&self, begin_wait: F) {
        self.acquire_many_sync_with(1, begin_wait);
    }

    /// Tries to get a permit from the semaphore.
    ///
    /// Returns `false` if no permits are available.
    ///
    /// # Examples
    ///
    /// ```
    /// use saa::Semaphore;
    ///
    /// let semaphore = Semaphore::default();
    ///
    /// assert!(semaphore.try_acquire());
    /// assert!(!semaphore.try_acquire_many(Semaphore::MAX_PERMITS));
    /// ```
    #[inline]
    pub fn try_acquire(&self) -> bool {
        self.try_acquire_internal(1).0
    }

    /// Gets multiple permits from the semaphore asynchronously.
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
    ///     semaphore.acquire_many_async(11).await;
    ///     assert_eq!(semaphore.available_permits(Relaxed), Semaphore::MAX_PERMITS - 11);
    /// };
    /// ```
    #[inline]
    pub async fn acquire_many_async(&self, count: usize) {
        self.acquire_many_async_with(count, || ()).await;
    }

    /// Gets multiple permits from the semaphore asynchronously with a wait callback provided.
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
    /// async {
    ///     let mut wait = false;
    ///     semaphore.acquire_many_async_with(2, || { wait = true; }).await;
    ///     assert_eq!(semaphore.available_permits(Relaxed), Semaphore::MAX_PERMITS - 2);
    ///     assert!(!wait);
    /// };
    /// ```
    #[inline]
    pub async fn acquire_many_async_with<F: FnOnce()>(&self, count: usize, mut begin_wait: F) {
        loop {
            let (result, state) = self.try_acquire_internal(count);
            if result {
                return;
            }
            // The value is checked in `try_acquire_internal`.
            #[allow(clippy::cast_possible_truncation)]
            if let Err(returned) = self
                .wait_resources_async(state, Opcode::Semaphore(count as u8), begin_wait)
                .await
            {
                begin_wait = returned;
            } else {
                return;
            }
        }
    }

    /// Gets multiple permits from the semaphore synchronously.
    ///
    /// # Examples
    ///
    /// ```
    /// use saa::Semaphore;
    /// use std::sync::atomic::Ordering::Relaxed;
    ///
    /// let semaphore = Semaphore::default();
    ///
    /// semaphore.acquire_many_sync(11);
    /// assert_eq!(semaphore.available_permits(Relaxed), Semaphore::MAX_PERMITS - 11);
    /// ```
    #[inline]
    pub fn acquire_many_sync(&self, count: usize) {
        self.acquire_many_sync_with(count, || ());
    }

    /// Gets multiple permits from the semaphore synchronously with a callback provided.
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
    /// semaphore.acquire_many_sync_with(2, || { wait = true; });
    /// assert_eq!(semaphore.available_permits(Relaxed), Semaphore::MAX_PERMITS - 2);
    /// assert!(!wait);
    /// ```
    #[inline]
    pub fn acquire_many_sync_with<F: FnOnce()>(&self, count: usize, mut begin_wait: F) {
        loop {
            let (result, state) = self.try_acquire_internal(count);
            if result {
                return;
            }
            // The value is checked in `try_acquire_internal`.
            #[allow(clippy::cast_possible_truncation)]
            if let Err(returned) =
                self.wait_resources_sync(state, Opcode::Semaphore(count as u8), begin_wait)
            {
                begin_wait = returned;
            } else {
                return;
            }
        }
    }

    /// Tries to get multiple permits from the semaphore.
    ///
    /// Returns `false` if no permits are available.
    ///
    /// # Examples
    ///
    /// ```
    /// use saa::Semaphore;
    ///
    /// let semaphore = Semaphore::default();
    ///
    /// assert!(semaphore.try_acquire_many(Semaphore::MAX_PERMITS));
    /// assert!(!semaphore.try_acquire());
    /// ```
    #[inline]
    pub fn try_acquire_many(&self, count: usize) -> bool {
        self.try_acquire_internal(count).0
    }

    /// Releases a permit.
    ///
    /// Returns `true` if a permit was successfully released.
    ///
    /// # Examples
    ///
    /// ```
    /// use saa::Semaphore;
    /// use std::sync::atomic::Ordering::Relaxed;
    ///
    /// let semaphore = Semaphore::default();
    ///
    /// assert!(semaphore.try_acquire_many(11));
    /// assert_eq!(semaphore.available_permits(Relaxed), Semaphore::MAX_PERMITS - 11);
    ///
    /// assert!(semaphore.release());
    /// assert_eq!(semaphore.available_permits(Relaxed), Semaphore::MAX_PERMITS - 10);
    /// ```
    #[inline]
    pub fn release(&self) -> bool {
        match self.state.compare_exchange(1, 0, Release, Relaxed) {
            Ok(_) => true,
            Err(state) => self.release_loop(state, Opcode::Semaphore(1)),
        }
    }

    /// Releases permits.
    ///
    /// Returns `true` if the specified number of permits were successfully released.
    ///
    /// # Examples
    ///
    /// ```
    /// use saa::Semaphore;
    /// use std::sync::atomic::Ordering::Relaxed;
    ///
    /// let semaphore = Semaphore::default();
    ///
    /// assert!(semaphore.try_acquire_many(11));
    /// assert_eq!(semaphore.available_permits(Relaxed), Semaphore::MAX_PERMITS - 11);
    ///
    /// assert!(semaphore.release_many(10));
    /// assert_eq!(semaphore.available_permits(Relaxed), Semaphore::MAX_PERMITS - 1);
    /// ```
    #[inline]
    pub fn release_many(&self, count: usize) -> bool {
        let Ok(count) = u8::try_from(count) else {
            return false;
        };
        match self
            .state
            .compare_exchange(count as usize, 0, Release, Relaxed)
        {
            Ok(_) => true,
            Err(state) => self.release_loop(state, Opcode::Semaphore(count)),
        }
    }

    /// Tries to acquire a permit.
    fn try_acquire_internal(&self, count: usize) -> (bool, usize) {
        let mut state = self.state.load(Acquire);
        loop {
            if state & WaitQueue::ADDR_MASK != 0
                || (state & WaitQueue::DATA_MASK) + count > Self::MAX_PERMITS
            {
                // There is a waiting thread, or the lock can no longer be shared.
                return (false, state);
            }

            match self
                .state
                .compare_exchange(state, state + count, Acquire, Relaxed)
            {
                Ok(_) => return (true, 0),
                Err(new_state) => state = new_state,
            }
        }
    }
}

impl fmt::Debug for Semaphore {
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

impl SyncPrimitive for Semaphore {
    #[inline]
    fn state(&self) -> &AtomicUsize {
        &self.state
    }

    #[inline]
    fn max_shared_owners() -> usize {
        Self::MAX_PERMITS
    }
}
