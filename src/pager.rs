//! [`Pager`] allows the user to remotely wait for a desired resource.

#![deny(unsafe_code)]

use std::marker::PhantomData;
use std::pin::Pin;
use std::task::{Context, Poll};

use crate::wait_queue::WaitQueue;

/// Tasks holding a [`Pager`] can remotely get a desired resource.
#[derive(Debug, Default)]
pub struct Pager<'s, S: SyncResult> {
    /// The wait queue entry for the [`Pager`].
    entry: Option<WaitQueue>,
    /// The [`Pager`] cannot outlive the associated synchronization primitive.
    _phantom: PhantomData<&'s S>,
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

/// Define result value interpretation interfaces.
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
    /// use std::pin::Pin;
    ///
    /// use saa::{Gate, Pager};
    /// use saa::gate::State;
    ///
    /// let gate = Gate::default();
    ///
    /// let mut pager = Pager::default();
    ///
    /// let mut pinned_pager = Pin::new(&mut pager);
    /// assert!(!pinned_pager.is_registered());
    ///
    /// assert!(gate.register_sync(&mut pinned_pager));
    /// assert!(pinned_pager.is_registered());
    ///
    /// assert_eq!(gate.open().1, 1);
    /// ```
    #[inline]
    pub fn is_registered(&self) -> bool {
        self.entry.is_some()
    }

    /// Returns `true` if the [`Pager`] can only be polled synchronously.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::pin::Pin;
    ///
    /// use saa::{Gate, Pager};
    /// use saa::gate::State;
    ///
    /// let gate = Gate::default();
    ///
    /// let mut pager = Pager::default();
    /// let mut pinned_pager = Pin::new(&mut pager);
    ///
    /// assert!(gate.register_sync(&mut pinned_pager));
    /// assert!(pinned_pager.is_sync());
    ///
    /// assert_eq!(gate.open().1, 1);
    ///
    /// assert_eq!(pinned_pager.poll_sync(), Ok(State::Open));
    /// assert!(!pinned_pager.is_sync());
    /// ```
    #[inline]
    pub fn is_sync(&self) -> bool {
        self.entry.as_ref().is_some_and(WaitQueue::is_sync)
    }

    /// Waits for the desired resource to become available.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::pin::Pin;
    ///
    /// use saa::{Gate, Pager};
    /// use saa::gate::State;
    ///
    /// let gate = Gate::default();
    ///
    /// let mut pager = Pager::default();
    /// let mut pinned_pager = Pin::new(&mut pager);
    ///
    /// assert!(gate.register_sync(&mut pinned_pager));
    ///
    /// assert_eq!(gate.open().1, 1);
    ///
    /// assert_eq!(pinned_pager.poll_sync(), Ok(State::Open));
    /// ```
    #[inline]
    pub fn poll_sync(self: &mut Pin<&mut Pager<'s, S>>) -> S::Result {
        let Some(entry) = self.entry.as_ref() else {
            return S::to_result(0, Some(Error::NotRegistered));
        };
        let result = entry.poll_result_sync();
        if result == WaitQueue::ERROR_WRONG_MODE {
            return S::to_result(0, Some(Error::WrongMode));
        }
        self.entry.take();
        S::to_result(result, None)
    }

    /// Tries to get the result.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::pin::Pin;
    ///
    /// use saa::{Gate, Pager};
    /// use saa::gate::{Error, State};
    ///
    /// let gate = Gate::default();
    ///
    /// let mut pager = Pager::default();
    /// let mut pinned_pager = Pin::new(&mut pager);
    ///
    /// assert!(gate.register_sync(&mut pinned_pager));
    ///
    /// assert_eq!(pinned_pager.try_poll(), Err(Error::NotReady));
    /// assert_eq!(gate.open().1, 1);
    ///
    /// assert_eq!(pinned_pager.try_poll(), Ok(State::Open));
    /// assert_eq!(pinned_pager.poll_sync(), Ok(State::Open));
    /// ```
    #[inline]
    pub fn try_poll(&self) -> S::Result {
        let Some(entry) = self.entry.as_ref() else {
            return S::to_result(0, Some(Error::NotRegistered));
        };

        if let Some(result) = entry.try_acknowledge_result() {
            S::to_result(result, None)
        } else {
            S::to_result(0, Some(Error::NotReady))
        }
    }

    /// Returns a reference to the wait queue entry.
    #[inline]
    pub(crate) fn entry(&self) -> Option<&WaitQueue> {
        self.entry.as_ref()
    }

    /// Sets the wait queue entry.
    #[inline]
    pub(crate) fn set_entry(&mut self, entry: WaitQueue) {
        self.entry.replace(entry);
    }
}

impl<S: SyncResult> Future for Pager<'_, S> {
    type Output = S::Result;

    #[inline]
    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let Some(entry) = self.entry.as_ref() else {
            return Poll::Ready(S::to_result(0, Some(Error::NotRegistered)));
        };
        if let Poll::Ready(result) = entry.poll_result_async(cx) {
            self.entry.take();
            if result == WaitQueue::ERROR_WRONG_MODE {
                return Poll::Ready(S::to_result(result, Some(Error::WrongMode)));
            }
            return Poll::Ready(S::to_result(result, None));
        }
        Poll::Pending
    }
}
