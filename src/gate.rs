//! [`Gate`] is a synchronization primitive that blocks tasks to enter a critical section until they
//! are allowed to do so.

#![allow(clippy::pedantic)]

use std::pin::Pin;
#[cfg(not(feature = "loom"))]
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering::{self, AcqRel};
use std::task::{Context, Poll};

#[cfg(feature = "loom")]
use loom::sync::atomic::AtomicUsize;

use crate::sync_primitive::SyncPrimitive;
use crate::wait_queue::WaitQueue;

/// [`Gate`] is a synchronization primitive that blocks tasks to enter a critical section until they
/// are allowed to do so.
#[derive(Debug, Default)]
pub struct Gate {
    /// [`Gate`] state.
    state: AtomicUsize,
}

/// The state of a [`Gate`].
///
/// [`Gate`] can be in one of three states.
///
/// * `Closed` - The default state where tasks can enter the [`Gate`] if permitted.
/// * `Sealed` - The [`Gate`] is sealed and tasks immediately get rejected to enter it.
/// * `Open` - The [`Gate`] is open and tasks can immediately enter it.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
#[repr(u8)]
pub enum State {
    /// The default state where tasks can enter the [`Gate`] if permitted.
    Closed = 0_u8,
    /// The [`Gate`] is sealed and tasks immediately get rejected to enter it.
    Sealed = 1_u8,
    /// The [`Gate`] is open and tasks can immediately enter it.
    Open = 2_u8,
}

/// The result of attempting to enter a [`Gate`].
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
#[repr(u8)]
pub enum Result {
    /// Successfully entered a [`Gate`]
    Entered = 0_u8,
    /// Failed to enter a [`Gate`], since it was sealed.
    Sealed = 1_u8,
    /// Failed to enter a [`Gate`], since waiting tasks were cleared it.
    Cleared = 2_u8,
    /// Spurious failure to enter a [`Gate`] in a [`Closed`](State::Closed) state.
    ///
    /// This can happen if the [`Gate`] is closed and one of waiting tasks directly holding a
    /// [`Pager`] has been cancelled.
    Spurious = 3_u8,
}

/// Tasks holding a [`Pager`] can remotely get a permit to enter the [`Gate`].
#[derive(Debug)]
pub struct Pager {
    wait_queue_entry: Option<WaitQueue>,
}

impl Gate {
    /// Returns the current state of the [`Gate`].
    ///
    /// # Examples
    ///
    /// ```
    /// use saa::Gate;
    /// use std::sync::atomic::Ordering::Relaxed;
    /// ```
    #[inline]
    pub fn state(&self, mo: Ordering) -> State {
        State::from((self.state.load(mo) & WaitQueue::DATA_MASK) as u8)
    }

    /// Resets the [`Gate`] to its initial state.
    ///
    #[inline]
    pub fn reset(&self) {
        let _prev = self.state.swap(u8::from(State::Closed) as usize, AcqRel);
    }

    /// Opens the [`Gate`] to allow any tasks to enter it.
    ///
    /// Returns the number of tasks that were waiting to entier it.
    ///
    /// # Examples
    ///
    /// ```
    /// use saa::Gate;
    /// use std::sync::atomic::Ordering::Relaxed;
    /// ```
    #[inline]
    pub fn open(&self) -> Option<usize> {
        let _prev = self.state.swap(u8::from(State::Open) as usize, AcqRel);
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
        let _prev = self.state.swap(u8::from(State::Sealed) as usize, AcqRel);
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

    /// Registers a [`Pager`] to allow it get a permit to enter the [`Gate`] remotely.
    ///
    /// Returns `false` if the [`Pager`] was already registered.
    ///
    /// # Examples
    ///
    /// ```
    /// use saa::Gate;
    /// ```
    #[inline]
    pub fn register(&self, _token: &Pin<&mut Pager>) -> bool {
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

impl From<State> for u8 {
    #[inline]
    fn from(value: State) -> Self {
        match value {
            State::Closed => 0_u8,
            State::Sealed => 1_u8,
            State::Open => 2_u8,
        }
    }
}

impl From<u8> for State {
    #[inline]
    fn from(value: u8) -> Self {
        match value {
            0_u8 => State::Closed,
            1_u8 => State::Sealed,
            _ => State::Open,
        }
    }
}

impl From<Result> for u8 {
    #[inline]
    fn from(value: Result) -> Self {
        match value {
            Result::Entered => 0_u8,
            Result::Sealed => 1_u8,
            Result::Cleared => 2_u8,
            Result::Spurious => 3_u8,
        }
    }
}

impl From<u8> for Result {
    #[inline]
    fn from(value: u8) -> Self {
        match value {
            0_u8 => Result::Entered,
            1_u8 => Result::Sealed,
            2_u8 => Result::Cleared,
            _ => Result::Spurious,
        }
    }
}

impl Pager {
    /// Waits for a permit to enter the [`Gate`].
    pub fn poll(self: &mut Pin<&mut Pager>) -> Option<Result> {
        let Some(wait_queue_entry) = self.wait_queue_entry.as_ref() else {
            // The `Pager` is not registered in any `Gate`.
            return None;
        };
        let mut result = wait_queue_entry.poll_result_sync();
        if result == WaitQueue::INTERNAL_ERROR {
            result = u8::from(Result::Spurious);
        };
        self.wait_queue_entry.take();
        Some(Result::from(result))
    }
}

impl Future for Pager {
    type Output = Option<Result>;

    #[inline]
    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let Some(wait_queue_entry) = self.wait_queue_entry.as_ref() else {
            // The `Pager` is not registered in any `Gate`.
            return Poll::Ready(None);
        };
        if let Poll::Ready(mut result) = wait_queue_entry.poll_result_async(cx) {
            self.wait_queue_entry.take();
            if result == WaitQueue::INTERNAL_ERROR {
                result = u8::from(Result::Spurious);
            };
            return Poll::Ready(Some(Result::from(result)));
        }
        Poll::Pending
    }
}
