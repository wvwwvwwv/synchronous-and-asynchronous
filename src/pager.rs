//! [`Pager`] allows the user to remotely wait for a desired resource.

use std::cell::UnsafeCell;
use std::marker::{PhantomData, PhantomPinned};
use std::pin::Pin;

use crate::wait_queue::{Entry, PinnedEntry, WaitQueue};

/// Tasks holding a [`Pager`] can remotely acquire a desired resource.
///
/// [`Pager`] contains a wait queue entry which forms an intrusive linked list. It is important that
/// the [`Pager`] is not moved while it is registered in a synchronization primitive, otherwise it
/// may lead to undefined behavior, therefore [`Pager`] does not implement [`Unpin`].
#[derive(Debug, Default)]
pub struct Pager<'s, S: SyncResult> {
    /// The wait queue for the [`Pager`].
    wait_queue: UnsafeCell<Option<WaitQueue>>,
    /// The [`Pager`] cannot outlive the associated synchronization primitive.
    _phantom: PhantomData<&'s S>,
    /// The [`Pager`] cannot be unpinned.
    _pinned: PhantomPinned,
}

/// Errors that can occur when using a [`Pager`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error {
    /// The [`Pager`] is not registered in a synchronization primitive.
    NotRegistered,
    /// The [`Pager`] is registered in a synchronization primitive with a different mode.
    WrongMode,
    /// The result is not ready.
    NotReady,
}

/// Defines result value interpretation interfaces.
pub trait SyncResult: Sized {
    /// Operation result type.
    type Result: Clone + Copy + Eq + PartialEq;

    /// Converts a `u8` value into a `Self::Result`.
    fn to_result(value: u8, pager_error: Option<Error>) -> Self::Result;
}

impl<'s, S: SyncResult> Pager<'s, S> {
    /// Returns `true` if the [`Pager`] is registered in a synchronization primitive.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::pin::pin;
    ///
    /// use saa::{Gate, Pager};
    /// use saa::gate::State;
    ///
    /// let gate = Gate::default();
    ///
    /// let mut pinned_pager = pin!(Pager::default());
    /// assert!(!pinned_pager.is_registered());
    ///
    /// assert!(gate.register_pager(&mut pinned_pager, false));
    /// assert!(pinned_pager.is_registered());
    ///
    /// assert_eq!(gate.open().1, 1);
    /// ```
    #[inline]
    pub fn is_registered(&self) -> bool {
        self.wait_queue().is_some()
    }

    /// Returns `true` if the [`Pager`] can only be polled synchronously.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::pin::pin;
    ///
    /// use saa::{Gate, Pager};
    /// use saa::gate::State;
    ///
    /// let gate = Gate::default();
    ///
    /// let mut pinned_pager = pin!(Pager::default());
    ///
    /// assert!(gate.register_pager(&mut pinned_pager, true));
    /// assert!(pinned_pager.is_sync());
    ///
    /// assert_eq!(gate.open().1, 1);
    ///
    /// assert_eq!(pinned_pager.poll_sync(), Ok(State::Open));
    /// assert!(!pinned_pager.is_sync());
    /// ```
    #[inline]
    pub fn is_sync(&self) -> bool {
        self.wait_queue().is_some_and(|w| w.entry().is_sync())
    }

    /// Waits for the desired resource to become available asynchronously.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::pin::pin;
    ///
    /// use saa::{Gate, Pager};
    /// use saa::gate::State;
    ///
    /// let gate = Gate::default();
    ///
    /// let mut pinned_pager = pin!(Pager::default());
    ///
    /// assert!(gate.register_pager(&mut pinned_pager, false));
    ///
    /// assert_eq!(gate.open().1, 1);
    ///
    /// async {
    ///     assert_eq!(pinned_pager.poll_async().await, Ok(State::Open));
    /// };
    /// ```
    #[inline]
    pub async fn poll_async(self: &mut Pin<&mut Pager<'s, S>>) -> S::Result {
        let Some(wait_queue) = self.wait_queue() else {
            return S::to_result(0, Some(Error::NotRegistered));
        };
        let pinned_entry = PinnedEntry(Pin::new(wait_queue.entry()));
        let result = pinned_entry.await;
        if result == Entry::ERROR_WRONG_MODE {
            return S::to_result(result, Some(Error::WrongMode));
        }
        self.with_wait_queue_mut(|e| {
            *e = None;
        });
        S::to_result(result, None)
    }

    /// Waits for the desired resource to become available synchronously.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::pin::pin;
    ///
    /// use saa::{Gate, Pager};
    /// use saa::gate::State;
    ///
    /// let gate = Gate::default();
    ///
    /// let mut pinned_pager = pin!(Pager::default());
    ///
    /// assert!(gate.register_pager(&mut pinned_pager, true));
    ///
    /// assert_eq!(gate.open().1, 1);
    ///
    /// assert_eq!(pinned_pager.poll_sync(), Ok(State::Open));
    /// ```
    #[inline]
    pub fn poll_sync(self: &mut Pin<&mut Pager<'s, S>>) -> S::Result {
        self.with_wait_queue_mut(|w| {
            let Some(wait_queue) = w else {
                return S::to_result(0, Some(Error::NotRegistered));
            };
            let entry = wait_queue.entry();
            let result = entry.poll_result_sync();
            if result == Entry::ERROR_WRONG_MODE {
                return S::to_result(0, Some(Error::WrongMode));
            }
            *w = None;
            S::to_result(result, None)
        })
    }

    /// Tries to get the result.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::pin::pin;
    ///
    /// use saa::{Gate, Pager};
    /// use saa::gate::{Error, State};
    ///
    /// let gate = Gate::default();
    ///
    /// let mut pinned_pager = pin!(Pager::default());
    ///
    /// assert!(gate.register_pager(&mut pinned_pager, true));
    ///
    /// assert_eq!(pinned_pager.try_poll(), Err(Error::NotReady));
    /// assert_eq!(gate.open().1, 1);
    ///
    /// assert_eq!(pinned_pager.try_poll(), Ok(State::Open));
    /// assert_eq!(pinned_pager.poll_sync(), Ok(State::Open));
    /// ```
    #[inline]
    pub fn try_poll(&self) -> S::Result {
        self.with_wait_queue(|w| {
            let Some(wait_queue) = w else {
                return S::to_result(0, Some(Error::NotRegistered));
            };

            let entry = wait_queue.entry();
            if let Some(result) = entry.try_acknowledge_result() {
                S::to_result(result, None)
            } else {
                S::to_result(0, Some(Error::NotReady))
            }
        })
    }

    /// Returns a reference to the wait queue entry.
    #[inline]
    pub(crate) fn wait_queue(&self) -> Option<Pin<&WaitQueue>> {
        unsafe {
            (*self.wait_queue.get())
                .as_ref()
                .map(|w| Pin::new_unchecked(w))
        }
    }

    /// Sets the wait queue entry.
    ///
    /// The user must make sure that the `self` is exclusively owned.
    #[inline]
    pub(crate) fn set_wait_queue(&self) {
        self.with_wait_queue_mut(|e| e.replace(WaitQueue::default()));
    }

    /// Returns a reference to the wait queue.
    #[inline]
    fn with_wait_queue<R, F: FnOnce(&Option<WaitQueue>) -> R>(&self, f: F) -> R {
        unsafe { f(&(*self.wait_queue.get())) }
    }

    /// Returns a reference to the wait queue.
    #[inline]
    fn with_wait_queue_mut<R, F: FnOnce(&mut Option<WaitQueue>) -> R>(&self, f: F) -> R {
        unsafe { f(&mut (*self.wait_queue.get())) }
    }
}
