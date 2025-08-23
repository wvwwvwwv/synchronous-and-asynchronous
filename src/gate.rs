//! [`Gate`] is a synchronization primitive that blocks multiple threads until the gate is opened.

#![allow(clippy::pedantic)]

use std::pin::Pin;
#[cfg(not(feature = "loom"))]
use std::sync::atomic::AtomicUsize;
use std::task::{Context, Poll};

#[cfg(feature = "loom")]
use loom::sync::atomic::AtomicUsize;

use crate::sync_primitive::SyncPrimitive;
use crate::wait_queue::WaitQueue;

/// [`Gate`] is a synchronization primitive that blocks multiple threads until the gate is open.
#[derive(Debug, Default)]
pub struct Gate {
    /// [`Gate`] state.
    state: AtomicUsize,
}

/// The result of attempting to enter a [`Gate`].
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
#[repr(u8)]
pub enum Result {
    /// Successfully entered an open [`Gate`]
    Entered = 0_u8,
    /// Failed to enter a [`Gate`], since it was sealed.
    Sealed = 1_u8,
    /// Failed to enter a [`Gate`], since it was cleared.
    Cleared = 2_u8,
    /// Spurious failure to enter a [`Gate`].
    Spurious = 3_u8,
}

/// Tasks holding a [`Token`] can pass any open [`Gate`] in other code locations.
#[derive(Debug)]
pub struct Token {
    wait_queue_entry: Option<WaitQueue>,
}

impl Gate {
    /// Opens the [`Gate`] to allow the waiting tasks to enter.
    ///
    /// Returns the number of tasks that were permitted to enter the gate.
    ///
    /// # Examples
    ///
    /// ```
    /// use saa::Gate;
    /// use std::sync::atomic::Ordering::Relaxed;
    /// ```
    #[inline]
    pub fn open(&self) -> Option<usize> {
        None
    }

    /// Seals the [`Gate`] to clear the waiting tasks and disallow tasks to enter.
    ///
    /// Returns the number of tasks that were waiting to enter the gate.
    ///
    /// # Examples
    ///
    /// ```
    /// use saa::Gate;
    /// use std::sync::atomic::Ordering::Relaxed;
    /// ```
    #[inline]
    pub fn seal(&self) -> Option<usize> {
        None
    }

    /// Clears the [`Gate`] to get rid of the waiting tasks.
    ///
    /// Returns the number of tasks that were waiting to enter the gate.
    ///
    /// # Examples
    ///
    /// ```
    /// use saa::Gate;
    /// use std::sync::atomic::Ordering::Relaxed;
    /// ```
    #[inline]
    pub fn clear(&self) -> Option<usize> {
        None
    }

    /// Enters the [`Gate`] asynchronously.
    ///
    /// Returns the number of tasks that were waiting to enter the gate.
    ///
    /// # Examples
    ///
    /// ```
    /// use saa::Gate;
    /// ```
    #[inline]
    pub async fn enter_async(&self) -> Result {
        Result::Entered
    }

    /// Enters the [`Gate`] synchronously.
    ///
    /// Returns the number of tasks that were waiting to enter the gate.
    ///
    /// # Examples
    ///
    /// ```
    /// use saa::Gate;
    /// ```
    #[inline]
    pub fn enter_sync(&self) -> Result {
        Result::Entered
    }

    /// Registers a token to allow it to enter the [`Gate`].
    ///
    /// Returns `false` if the [`Token`] is already registered.
    ///
    /// # Examples
    ///
    /// ```
    /// use saa::Gate;
    /// ```
    #[inline]
    pub fn register_token(&self, _token: &Pin<&mut Token>) -> bool {
        false
    }
}

impl SyncPrimitive for Gate {
    #[inline]
    fn state(&self) -> &AtomicUsize {
        &self.state
    }

    #[inline]
    fn max_shared_owners() -> usize {
        usize::MAX
    }
}

impl From<u8> for Result {
    #[inline]
    fn from(value: u8) -> Self {
        match value {
            0 => Result::Entered,
            1 => Result::Sealed,
            2 => Result::Cleared,
            _ => Result::Spurious,
        }
    }
}

impl Future for Token {
    type Output = Option<u8>;

    #[inline]
    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let Some(wait_queue_entry) = self.wait_queue_entry.take() else {
            // The token is not registered in any gate.
            return Poll::Ready(None);
        };
        if let Poll::Ready(result) = wait_queue_entry.poll_result_async(cx) {
            return Poll::Ready(Some(result));
        }
        self.wait_queue_entry.replace(wait_queue_entry);
        Poll::Pending
    }
}
