//! Wait queue implementation.

#[cfg(feature = "loom")]
use loom::sync::atomic::{AtomicBool, AtomicPtr};
#[cfg(feature = "loom")]
use loom::sync::{Condvar, Mutex};
use std::fmt;
use std::future::Future;
use std::mem::{align_of, replace};
use std::pin::Pin;
use std::ptr::{addr_of, null_mut};
use std::sync::atomic::Ordering::{Acquire, Release};
#[cfg(not(feature = "loom"))]
use std::sync::atomic::{AtomicBool, AtomicPtr};
#[cfg(not(feature = "loom"))]
use std::sync::{Condvar, Mutex};
use std::task::Waker;
use std::task::{Context, Poll};

/// Fair and heap-free wait queue for locking primitives in this crate.
///
/// [`WaitQueue`] itself forms an intrusive linked list of entries where entries are pushed at the
/// tail and popped from the head. [`WaitQueue`] is `128-byte` aligned thus allowing the lower 7
/// bits in a pointer-sized scalar type variable to represent additional states.
#[repr(align(128))]
pub(crate) struct WaitQueue {
    /// Points to the entry that was pushed right before this entry.
    next_entry_ptr: AtomicPtr<Self>,
    /// Points to the entry that was pushed right after this entry.
    prev_entry_ptr: AtomicPtr<Self>,
    /// The state of this entry.
    state: Mutex<State>,
    /// Conditional variable to send a signal to a blocking/synchronous waiter.
    cond_var: Condvar,
    /// Cleanup function to be called when this entry is dropped without waiting for completion in
    /// an asynchronous context.
    async_cleanup_fn: Option<AsyncContextCleaner>,
    /// Flag to indicate that this entry has completed polling and got the result.
    async_poll_completed: AtomicBool,
    /// Desired resources to wait, expressed in `usize`.
    desired_resource: usize,
}

/// Function to be invoked when [`WaitQueue`] is dropped before completion.
///
/// The first `usize` argument is passed to the function along with `self`.
type AsyncContextCleaner = (usize, fn(&WaitQueue, usize));

/// State of the wait queue.
#[derive(Debug)]
enum State {
    /// The [`WaitQueue`] has been created.
    Initialized,
    /// The [`WaitQueue`] is waiting for resources in a blocking/synchronous context.
    Waiting,
    /// The [`WaitQueue`] is polling until it gets resources or cancelled in an asynchronous
    /// context.
    Polling(Waker),
    /// The result is ready.
    ResultReady(bool),
}

// `Miri` does not allow threads to keep `&self` when `Self` is dropped even `&self` is not used.
// This mutex synchronizes access to `&self` and `WaitQueue::drop`.
#[cfg(miri)]
static MIRI_MUTEX: std::sync::Mutex<[usize; 8]> = std::sync::Mutex::<[usize; 8]>::new([0; 8]);

macro_rules! _miri_scope_protector {
    () => {{
        #[cfg(miri)]
        let guard = MIRI_MUTEX.lock().unwrap();
        #[cfg(not(miri))]
        let guard = &();
        guard
    }};
}

impl WaitQueue {
    /// Creates a new [`WaitQueue`].
    #[cfg(feature = "loom")]
    pub(crate) fn new(
        desired_resources: usize,
        async_cleanup_fn: Option<AsyncContextCleaner>,
    ) -> Self {
        Self {
            forward_link: AtomicPtr::new(null_mut()),
            backward_link: AtomicPtr::new(null_mut()),
            state: Mutex::new(State::Initialized),
            condition_variable: Condvar::new(),
            async_cleanup_fn,
            async_poll_completed: AtomicBool::new(false),
            desired_resource: desired_resources,
        }
    }

    /// Creates a new [`WaitQueue`].
    #[cfg(not(feature = "loom"))]
    pub(crate) const fn new(
        desired_resources: usize,
        async_cleanup_fn: Option<AsyncContextCleaner>,
    ) -> Self {
        Self {
            next_entry_ptr: AtomicPtr::new(null_mut()),
            prev_entry_ptr: AtomicPtr::new(null_mut()),
            state: Mutex::new(State::Initialized),
            cond_var: Condvar::new(),
            async_cleanup_fn,
            async_poll_completed: AtomicBool::new(false),
            desired_resource: desired_resources,
        }
    }

    /// Gets a raw pointer to the next entry.
    ///
    /// The next entry is the one that was pushed right before this entry.
    pub(crate) fn next_entry_ptr(&self) -> *const Self {
        self.next_entry_ptr.load(Acquire)
    }

    /// Gets a raw pointer to the previous entry.
    ///
    /// The previous entry is the one that was pushed right after this entry.
    pub(crate) fn prev_entry_ptr(&self) -> *const Self {
        self.prev_entry_ptr.load(Acquire)
    }

    /// Updates the next entry pointer.
    pub(crate) fn update_next_entry_ptr(&self, next_ptr: *const Self) {
        debug_assert_eq!(next_ptr as usize % align_of::<Self>(), 0);
        self.next_entry_ptr.store(next_ptr.cast_mut(), Release);
    }

    /// Updates the previous entry pointer.
    pub(crate) fn update_prev_entry_ptr(&self, prev_ptr: *const Self) {
        debug_assert_eq!(prev_ptr as usize % align_of::<Self>(), 0);
        self.prev_entry_ptr.store(prev_ptr.cast_mut(), Release);
    }

    /// Returns its `usize` data that represents the desired resources.
    pub(crate) const fn desired_resources(&self) -> usize {
        self.desired_resource
    }

    /// Converts a reference to `Self` to a raw pointer.
    pub(crate) fn ref_to_ptr(this: &Self) -> *const Self {
        let wait_queue_ptr: *const Self = addr_of!(*this);
        debug_assert_eq!(wait_queue_ptr as usize % align_of::<Self>(), 0);
        wait_queue_ptr
    }

    /// Converts the memory address of `Self` to a raw pointer.
    pub(crate) fn addr_to_ptr(wait_queue_addr: usize) -> *const Self {
        debug_assert_eq!(wait_queue_addr % align_of::<Self>(), 0);
        wait_queue_addr as *const Self
    }

    pub(crate) fn install_backward_link(wait_queue_tail_ptr: *const Self) {
        let mut current_waiter_ptr = wait_queue_tail_ptr;
        while !current_waiter_ptr.is_null() {
            current_waiter_ptr = unsafe {
                let next_waiter_ptr = (*current_waiter_ptr).next_entry_ptr();
                if let Some(next_waiter) = next_waiter_ptr.as_ref() {
                    if next_waiter.prev_entry_ptr().is_null() {
                        next_waiter.update_prev_entry_ptr(current_waiter_ptr);
                    } else {
                        debug_assert_eq!(next_waiter.prev_entry_ptr(), current_waiter_ptr);
                        return;
                    }
                }
                next_waiter_ptr
            };
        }
    }

    pub(crate) fn any_forward<F: FnMut(&Self, Option<&Self>) -> bool>(
        wait_queue_tail_ptr: *const Self,
        mut f: F,
    ) -> bool {
        let mut current_waiter_ptr = wait_queue_tail_ptr;
        while !current_waiter_ptr.is_null() {
            current_waiter_ptr = unsafe {
                let next_waiter_ptr = (*current_waiter_ptr).next_entry_ptr();
                if let Some(next_waiter) = next_waiter_ptr.as_ref() {
                    next_waiter.update_prev_entry_ptr(current_waiter_ptr);
                }

                let _miri_scope_protector = _miri_scope_protector!();
                if f(&*current_waiter_ptr, next_waiter_ptr.as_ref()) {
                    return true;
                }
                next_waiter_ptr
            };
        }
        false
    }

    pub(crate) fn any_backward<F: FnMut(&Self, Option<&Self>) -> bool>(
        wait_queue_head_ptr: *const Self,
        mut f: F,
    ) -> bool {
        let mut current_waiter_ptr = wait_queue_head_ptr;
        while !current_waiter_ptr.is_null() {
            current_waiter_ptr = unsafe {
                let prev_waiter_ptr = (*current_waiter_ptr).prev_entry_ptr();
                let _miri_scope_protector = _miri_scope_protector!();
                if f(&*current_waiter_ptr, prev_waiter_ptr.as_ref()) {
                    return true;
                }
                prev_waiter_ptr
            };
        }
        false
    }
    pub(crate) fn set_result(&self, result: bool) {
        let _miri_scope_protector = _miri_scope_protector!();
        if let Ok(mut state) = self.state.lock() {
            match replace::<State>(&mut *state, State::ResultReady(result)) {
                State::Waiting => {
                    self.cond_var.notify_one();
                }
                State::Polling(waker) => {
                    waker.wake();
                }
                State::ResultReady(_) | State::Initialized => (),
            }
        }
    }

    pub(crate) fn poll_result(&self) -> bool {
        while let Ok(mut state) = self.state.lock() {
            match &*state {
                State::Initialized => {
                    *state = State::Waiting;
                }
                State::Waiting | State::Polling(_) => (),
                State::ResultReady(result) => {
                    let _miri_scope_protector = _miri_scope_protector!();
                    return *result;
                }
            }

            // `loom` does not implement `wait_while`, so use a loop instead.
            while !matches!(*state, State::ResultReady(_)) {
                if let Ok(returned) = self.cond_var.wait(state) {
                    state = returned;
                } else {
                    debug_assert!(false, "The mutex can never be poisoned");
                    return false;
                }
            }
            if let State::ResultReady(result) = &*state {
                return *result;
            }
        }

        debug_assert!(false, "The mutex can never be poisoned");
        false
    }

    pub(crate) fn async_poll_completed(&self) -> bool {
        self.async_poll_completed.load(Acquire)
    }
}

impl fmt::Debug for WaitQueue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WaitQueue")
            .field("forward_link", &self.next_entry_ptr)
            .field("backward_link", &self.prev_entry_ptr)
            .field("state", &self.state)
            .field("condition_variable", &self.cond_var)
            .field("has_async_cleanup_fn", &self.async_cleanup_fn.is_some())
            .field("async_poll_completed", &self.async_poll_completed)
            .field("desired_resource", &self.desired_resource)
            .finish()
    }
}

impl Drop for WaitQueue {
    #[inline]
    fn drop(&mut self) {
        if let Some((arg, cleanup_fn)) = self.async_cleanup_fn.as_ref() {
            if !self.async_poll_completed.load(Acquire) {
                cleanup_fn(self, *arg);
            }
        }
    }
}

impl Future for WaitQueue {
    type Output = bool;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let waker = cx.waker().clone();
        if let Ok(mut state) = self.state.lock() {
            match &mut *state {
                State::Waiting => (),
                State::Polling(_) | State::Initialized => {
                    *state = State::Polling(waker);
                }
                State::ResultReady(result) => {
                    self.async_poll_completed.store(true, Release);
                    let _miri_scope_protector = _miri_scope_protector!();
                    return Poll::Ready(*result);
                }
            }
            Poll::Pending
        } else {
            // Failing to lock means that the mutex is poisoned.
            debug_assert!(false, "The mutex can never be poisoned");
            Poll::Ready(false)
        }
    }
}
