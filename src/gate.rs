//! [`Gate`] is a synchronization primitive that blocks tasks to enter a critical section until they
//! are allowed to do so.

#![deny(unsafe_code)]

use std::pin::Pin;
#[cfg(not(feature = "loom"))]
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering::{self, AcqRel, Acquire, Relaxed};
use std::task::{Context, Poll};

#[cfg(feature = "loom")]
use loom::sync::atomic::AtomicUsize;

use crate::opcode::Opcode;
use crate::sync_primitive::SyncPrimitive;
use crate::wait_queue::WaitQueue;

/// [`Gate`] is a synchronization primitive that blocks tasks to enter a critical section until they
/// are allowed to do so.
#[derive(Debug, Default)]
pub struct Gate {
    /// [`Gate`] state.
    state: AtomicUsize,
}

/// Tasks holding a [`Pager`] can remotely get a permit to enter the [`Gate`].
#[derive(Debug, Default)]
pub struct Pager<'g> {
    /// The [`Gate`] that the [`Pager`] is registered in and the wait queue entry for the [`Pager`].
    entry: Option<(&'g Gate, WaitQueue)>,
}

/// The state of a [`Gate`].
///
/// [`Gate`] can be in one of three states.
///
/// * `Controlled` - The default state where tasks can enter the [`Gate`] if permitted.
/// * `Sealed` - The [`Gate`] is sealed and tasks immediately get rejected to enter it.
/// * `Open` - The [`Gate`] is open and tasks can immediately enter it.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
#[repr(u8)]
pub enum State {
    /// The default state where tasks can enter the [`Gate`] if permitted.
    Controlled = 0_u8,
    /// The [`Gate`] is sealed and tasks immediately get rejected when they attempt to enter it.
    Sealed = 1_u8,
    /// The [`Gate`] is open and tasks can immediately enter it.
    Open = 2_u8,
}

/// Errors raised when accessing a [`Gate`].
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
#[repr(u8)]
pub enum Error {
    /// The [`Gate`] rejected the task.
    Rejected = 4_u8,
    /// The [`Gate`] has been sealed.
    Sealed = 8_u8,
    /// Spurious failure to enter a [`Gate`] in a [`Controlled`](State::Controlled) state.
    ///
    /// This can happen if a task holding a [`Pager`] gets cancelled or drops the [`Pager`] before
    /// the [`Gate`] has permitted or rejected the task; the task causes all the other waiting tasks
    /// of the [`Gate`] to get this error.
    SpuriousFailure = 12_u8,
    /// The [`Pager`] is not registered in any [`Gate`].
    NotRegistered = 16_u8,
    /// The wrong asynchronous/synchronous mode was used in a [`Pager`].
    WrongMode = 20_u8,
    /// Unknown error.
    Unknown = 24_u8,
}

impl Gate {
    /// Mask to get the state value from `u8`.
    const STATE_MASK: u8 = 0b11;

    /// Returns the current state of the [`Gate`].
    ///
    /// # Examples
    ///
    /// ```
    /// use std::sync::atomic::Ordering::Relaxed;
    ///
    /// use saa::Gate;
    /// use saa::gate::State;
    ///
    /// let gate = Gate::default();
    ///
    /// assert_eq!(gate.state(Relaxed), State::Controlled);
    /// ```
    #[inline]
    pub fn state(&self, mo: Ordering) -> State {
        State::from(self.state.load(mo) & WaitQueue::DATA_MASK)
    }

    /// Resets the [`Gate`] to its initial state if it is not in a [`Controlled`](State::Controlled)
    /// state.
    ///
    /// Returns the previous state of the [`Gate`].
    ///
    /// # Examples
    ///
    /// ```
    /// use saa::Gate;
    /// use saa::gate::State;
    ///
    /// let gate = Gate::default();
    ///
    /// assert_eq!(gate.reset(), None);
    ///
    /// gate.seal();
    ///
    /// assert_eq!(gate.reset(), Some(State::Sealed));
    /// ```
    #[inline]
    pub fn reset(&self) -> Option<State> {
        match self.state.fetch_update(Relaxed, Relaxed, |value| {
            let state = State::from(value & WaitQueue::DATA_MASK);
            if state == State::Controlled {
                None
            } else {
                debug_assert_eq!(value & WaitQueue::ADDR_MASK, 0);
                Some((value & WaitQueue::ADDR_MASK) | u8::from(state) as usize)
            }
        }) {
            Ok(state) => Some(State::from(state & WaitQueue::DATA_MASK)),
            Err(_) => None,
        }
    }

    /// Permits waiting tasks to enter the [`Gate`] if the [`Gate`] is in a
    /// [`Controlled`](State::Controlled) state.
    ///
    /// Returns the number of permitted tasks.
    ///
    /// # Errors
    ///
    /// Returns an [`Error`] if the [`Gate`] is not in a [`Controlled`](State::Controlled) state.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::sync::Arc;
    /// use std::thread;
    ///
    /// use saa::Gate;
    /// use saa::gate::State;
    ///
    /// let gate = Arc::new(Gate::default());
    ///
    /// let gate_clone = gate.clone();
    ///
    /// let thread = thread::spawn(move || {
    ///     assert_eq!(gate_clone.enter_sync(), Ok(State::Controlled));
    /// });
    ///
    /// loop {
    ///     if gate.permit() == Ok(1) {
    ///         break;
    ///     }
    /// }
    ///
    /// thread.join().unwrap();
    /// ```
    #[inline]
    pub fn permit(&self) -> Result<usize, State> {
        let (state, count) = self.wake_all(None, None);
        if state == State::Controlled {
            Ok(count)
        } else {
            debug_assert_eq!(count, 0);
            Err(state)
        }
    }

    /// Rejects waiting tasks to enter the [`Gate`] if the [`Gate`] is in a
    /// [`Controlled`](State::Controlled) state.
    ///
    /// Returns the number of permitted tasks.
    ///
    /// # Errors
    ///
    /// Returns an [`Error`] if the [`Gate`] is not in a [`Controlled`](State::Controlled) state.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::sync::Arc;
    /// use std::thread;
    ///
    /// use saa::Gate;
    /// use saa::gate::Error;
    ///
    /// let gate = Arc::new(Gate::default());
    ///
    /// let gate_clone = gate.clone();
    ///
    /// let thread = thread::spawn(move || {
    ///     assert_eq!(gate_clone.enter_sync(), Err(Error::Rejected));
    /// });
    ///
    /// loop {
    ///     if gate.reject() == Ok(1) {
    ///         break;
    ///     }
    /// }
    ///
    /// thread.join().unwrap();
    /// ```
    #[inline]
    pub fn reject(&self) -> Result<usize, State> {
        let (state, count) = self.wake_all(None, Some(Error::Rejected));
        if state == State::Controlled {
            Ok(count)
        } else {
            debug_assert_eq!(count, 0);
            Err(state)
        }
    }

    /// Opens the [`Gate`] to allow any tasks to enter it.
    ///
    /// Returns the number of tasks that were waiting to enter it.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::sync::atomic::Ordering::Relaxed;
    ///
    /// use saa::Gate;
    /// use saa::gate::State;
    ///
    /// let gate = Gate::default();
    /// assert_eq!(gate.state(Relaxed), State::Controlled);
    ///
    /// let (prev_state, count) = gate.open();
    ///
    /// assert_eq!(prev_state, State::Controlled);
    /// assert_eq!(count, 0);
    /// assert_eq!(gate.state(Relaxed), State::Open);
    /// ```
    #[inline]
    pub fn open(&self) -> (State, usize) {
        self.wake_all(Some(State::Open), None)
    }

    /// Seals the [`Gate`] to disallow tasks to enter.
    ///
    /// Returns the number of tasks that were waiting to enter the gate.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::sync::atomic::Ordering::Relaxed;
    ///
    /// use saa::Gate;
    /// use saa::gate::State;
    ///
    /// let gate = Gate::default();
    ///
    /// let (prev_state, count) = gate.seal();
    ///
    /// assert_eq!(prev_state, State::Controlled);
    /// assert_eq!(count, 0);
    /// assert_eq!(gate.state(Relaxed), State::Sealed);
    /// ```
    #[inline]
    pub fn seal(&self) -> (State, usize) {
        self.wake_all(Some(State::Sealed), Some(Error::Sealed))
    }

    /// Enters the [`Gate`] asynchronously.
    ///
    /// Returns the number of tasks that were waiting to enter the gate.
    ///
    /// # Errors
    ///
    /// Returns an [`Error`] if it failed to enter the [`Gate`].
    ///
    /// # Examples
    ///
    /// ```
    /// use saa::Gate;
    /// ```
    #[inline]
    pub async fn enter_async(&self) -> Result<State, Error> {
        let mut pager = Pager::default();
        let mut pinned_pager = Pin::new(&mut pager);
        self.register_async(&mut pinned_pager);
        pinned_pager.await
    }

    /// Enters the [`Gate`] synchronously.
    ///
    /// Returns the number of tasks that were waiting to enter the gate.
    ///
    /// # Errors
    ///
    /// Returns an [`Error`] if it failed to enter the [`Gate`].
    ///
    /// # Examples
    ///
    /// ```
    /// use std::sync::Arc;
    /// use std::thread;
    ///
    /// use saa::Gate;
    /// use saa::gate::State;
    ///
    /// let gate = Arc::new(Gate::default());
    ///
    /// let gate_clone = gate.clone();
    /// let thread_1 = thread::spawn(move || {
    ///     assert_eq!(gate_clone.enter_sync(), Ok(State::Controlled));
    /// });
    ///
    /// let gate_clone = gate.clone();
    /// let thread_2 = thread::spawn(move || {
    ///     assert_eq!(gate_clone.enter_sync(), Ok(State::Controlled));
    /// });
    ///
    /// let mut cnt = 0;
    /// while cnt != 2 {
    ///     if let Ok(n) = gate.permit() {
    ///         cnt += n;
    ///     }
    /// }
    ///
    /// thread_1.join().unwrap();
    /// thread_2.join().unwrap();
    /// ```
    #[inline]
    pub fn enter_sync(&self) -> Result<State, Error> {
        let mut pager = Pager::default();
        let mut pinned_pager = Pin::new(&mut pager);
        self.register_sync(&mut pinned_pager);
        pinned_pager.poll_sync()
    }

    /// Registers a [`Pager`] to allow it get a permit to enter the [`Gate`] remotely.
    ///
    /// Returns `false` if the [`Pager`] was already registered.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::pin::Pin;
    ///
    /// use saa::Gate;
    /// use saa::gate::{Error, Pager, State};
    ///
    /// let gate = Gate::default();
    ///
    /// let mut pager = Pager::default();
    /// let mut pinned_pager = Pin::new(&mut pager);
    ///
    /// assert!(gate.register_async(&mut pinned_pager));
    ///
    /// assert_eq!(gate.open().1, 1);
    ///
    /// assert_eq!(pinned_pager.poll_sync(), Err(Error::WrongMode));
    ///
    /// async {
    ///     assert_eq!(pinned_pager.await, Ok(State::Open));
    /// };
    /// ```
    #[inline]
    pub fn register_async<'g>(&'g self, pager: &mut Pin<&mut Pager<'g>>) -> bool {
        if pager.entry.is_some() {
            return false;
        }
        pager.entry.replace((
            self,
            WaitQueue::new_async(Opcode::Wait, Self::noop, self.addr()),
        ));
        self.push_wait_queue_entry(pager);
        true
    }

    /// Registers a [`Pager`] to allow it get a permit to enter the [`Gate`] remotely.
    ///
    /// Returns `false` if the [`Pager`] was already registered.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::pin::Pin;
    /// use std::sync::Arc;
    /// use std::thread;
    ///
    /// use saa::Gate;
    /// use saa::gate::{Pager, State};
    ///
    /// let gate = Arc::new(Gate::default());
    ///
    /// let mut pager = Pager::default();
    /// let mut pinned_pager = Pin::new(&mut pager);
    ///
    /// assert!(gate.register_sync(&mut pinned_pager));
    /// assert!(!gate.register_sync(&mut pinned_pager));
    ///
    /// let gate_clone = gate.clone();
    /// let thread = thread::spawn(move || {
    ///     assert_eq!(gate_clone.permit(), Ok(1));
    /// });
    ///
    /// thread.join().unwrap();
    ///
    /// assert_eq!(pinned_pager.poll_sync(), Ok(State::Controlled));
    /// ```
    #[inline]
    pub fn register_sync<'g>(&'g self, pager: &mut Pin<&mut Pager<'g>>) -> bool {
        if pager.entry.is_some() {
            return false;
        }
        pager
            .entry
            .replace((self, WaitQueue::new_sync(Opcode::Wait)));
        self.push_wait_queue_entry(pager);
        true
    }

    /// Wakes up all the waiting tasks and updates the state.
    ///
    /// Returns `(prev_state, count)` where `prev_state` is the previous state of the gate and
    /// `count` is the number of tasks that were woken up.
    fn wake_all(&self, next_state: Option<State>, error: Option<Error>) -> (State, usize) {
        match self.state.fetch_update(AcqRel, Acquire, |value| {
            if let Some(new_value) = next_state {
                Some(u8::from(new_value) as usize)
            } else {
                Some(value & WaitQueue::DATA_MASK)
            }
        }) {
            Ok(value) | Err(value) => {
                let mut count = 0;
                let entry_addr = value & WaitQueue::ADDR_MASK;
                let prev_state = State::from(value & WaitQueue::DATA_MASK);
                let next_state = next_state.unwrap_or(prev_state);
                let result = Self::into_u8(next_state, error);
                if entry_addr != 0 {
                    WaitQueue::iter_forward(WaitQueue::addr_to_ptr(entry_addr), |entry, _| {
                        entry.set_result(result);
                        count += 1;
                        false
                    });
                }
                (prev_state, count)
            }
        }
    }

    /// Pushes the wait queue entry.
    fn push_wait_queue_entry(&self, entry: &mut Pin<&mut Pager>) {
        if let Some((_, entry)) = entry.entry.as_ref() {
            let pinned_entry = Pin::new(entry);
            loop {
                let state = self.state.load(Relaxed);
                match State::from(state & WaitQueue::DATA_MASK) {
                    State::Controlled => {
                        if !self.try_push_wait_queue_entry(pinned_entry, state) {
                            continue;
                        }
                    }
                    State::Sealed => {
                        entry.set_result(Self::into_u8(State::Sealed, Some(Error::Sealed)));
                    }
                    State::Open => {
                        entry.set_result(Self::into_u8(State::Open, None));
                    }
                }
                break;
            }
        }
    }

    /// Noop function.
    fn noop(_entry: &WaitQueue, _this_addr: usize) {
        unreachable!("Noop function called");
    }

    /// Converts `(State, Error)` into `u8`.
    fn into_u8(state: State, error: Option<Error>) -> u8 {
        u8::from(state) | error.map_or(0_u8, u8::from)
    }

    /// Converts `(State, Error)` into `u8`.
    fn from_u8(value: u8) -> (State, Option<Error>) {
        let state = State::from(value & Self::STATE_MASK);
        let error = value & !(Self::STATE_MASK);
        if error != 0 {
            (state, Some(Error::from(error)))
        } else {
            (state, None)
        }
    }
}

impl Drop for Gate {
    #[inline]
    fn drop(&mut self) {
        if self.state.load(Relaxed) & WaitQueue::ADDR_MASK == 0 {
            return;
        }
        self.seal();
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

impl<'g> Pager<'g> {
    /// Returns `true` if the [`Pager`] is registered in a [`Gate`].
    ///
    /// # Examples
    ///
    /// ```
    /// use std::pin::Pin;
    ///
    /// use saa::Gate;
    /// use saa::gate::{Pager, State};
    ///
    /// let gate = Gate::default();
    ///
    /// let mut pager = Pager::default();
    ///
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
    /// use saa::Gate;
    /// use saa::gate::{Pager, State};
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
        self.entry
            .as_ref()
            .is_some_and(|(_, entry)| entry.is_sync())
    }

    /// Waits for a permit to enter the [`Gate`].
    ///
    /// # Errors
    ///
    /// Returns an [`Error`] if it failed to enter the [`Gate`].
    ///
    /// # Examples
    ///
    /// ```
    /// use std::pin::Pin;
    ///
    /// use saa::Gate;
    /// use saa::gate::{Pager, State};
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
    pub fn poll_sync(self: &mut Pin<&mut Pager<'g>>) -> Result<State, Error> {
        let Some((_, entry)) = self.entry.as_ref() else {
            // The `Pager` is not registered in any `Gate`.
            return Err(Error::NotRegistered);
        };
        let result = entry.poll_result_sync();
        if result == WaitQueue::ERROR_WRONG_MODE {
            return Err(Error::WrongMode);
        }
        self.entry.take();
        let (state, error) = Gate::from_u8(result);
        error.map_or(Ok(state), Err)
    }
}

impl Drop for Pager<'_> {
    #[inline]
    fn drop(&mut self) {
        let Some((gate, entry)) = self.entry.as_mut() else {
            return;
        };
        if entry.try_acknowledge_result().is_none() {
            gate.wake_all(None, Some(Error::SpuriousFailure));
            entry.acknowledge_result_sync();
        }
    }
}

impl Future for Pager<'_> {
    type Output = Result<State, Error>;

    #[inline]
    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let Some((_, entry)) = self.entry.as_ref() else {
            // The `Pager` is not registered in any `Gate`.
            return Poll::Ready(Err(Error::NotRegistered));
        };
        if let Poll::Ready(result) = entry.poll_result_async(cx) {
            self.entry.take();
            if result == WaitQueue::ERROR_WRONG_MODE {
                return Poll::Ready(Err(Error::WrongMode));
            }
            let (state, error) = Gate::from_u8(result);
            return Poll::Ready(error.map_or(Ok(state), Err));
        }
        Poll::Pending
    }
}

impl From<State> for u8 {
    #[inline]
    fn from(value: State) -> Self {
        match value {
            State::Controlled => 0_u8,
            State::Sealed => 1_u8,
            State::Open => 2_u8,
        }
    }
}

impl From<u8> for State {
    #[inline]
    fn from(value: u8) -> Self {
        State::from(value as usize)
    }
}

impl From<usize> for State {
    #[inline]
    fn from(value: usize) -> Self {
        match value {
            0 => State::Controlled,
            1 => State::Sealed,
            _ => State::Open,
        }
    }
}

impl From<Error> for u8 {
    #[inline]
    fn from(value: Error) -> Self {
        match value {
            Error::Rejected => 4_u8,
            Error::Sealed => 8_u8,
            Error::SpuriousFailure => 12_u8,
            Error::NotRegistered => 16_u8,
            Error::WrongMode => 20_u8,
            Error::Unknown => 24_u8,
        }
    }
}

impl From<u8> for Error {
    #[inline]
    fn from(value: u8) -> Self {
        Error::from(value as usize)
    }
}

impl From<usize> for Error {
    #[inline]
    fn from(value: usize) -> Self {
        match value {
            4 => Error::Rejected,
            8 => Error::Sealed,
            12 => Error::SpuriousFailure,
            16 => Error::NotRegistered,
            20 => Error::WrongMode,
            _ => Error::Unknown,
        }
    }
}
