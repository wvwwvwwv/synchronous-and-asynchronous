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

#[repr(align(128))]
pub(crate) struct WaitQueue {
    forward_link: AtomicPtr<Self>,
    backward_link: AtomicPtr<Self>,
    state: Mutex<State>,
    condition_variable: Condvar,
    async_cleanup_fn: Option<AsyncContextCleaner>,
    async_poll_completed: AtomicBool,
    desired_resource: usize,
}

type AsyncContextCleaner = (usize, fn(&WaitQueue, usize));

/// State of the wait queue.
#[derive(Debug)]
enum State {
    Initialized,
    Waiting,
    Polling(Waker),
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

    #[cfg(not(feature = "loom"))]
    pub(crate) const fn new(
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

    pub(crate) const fn desired_resources(&self) -> usize {
        self.desired_resource
    }

    pub(crate) fn ref_to_ptr(this: &Self) -> *const Self {
        let wait_queue_ptr: *const Self = addr_of!(*this);
        wait_queue_ptr
    }

    pub(crate) fn addr_to_ptr(wait_queue_addr: usize) -> *const Self {
        debug_assert_eq!(wait_queue_addr % align_of::<Self>(), 0);
        wait_queue_addr as *const Self
    }

    pub(crate) fn install_backward_link(wait_queue_tail_ptr: *const Self) {
        let mut current_waiter_ptr = wait_queue_tail_ptr;
        while !current_waiter_ptr.is_null() {
            current_waiter_ptr = unsafe {
                let next_waiter_ptr = (*current_waiter_ptr).next_ptr();
                if let Some(next_waiter) = next_waiter_ptr.as_ref() {
                    if next_waiter.prev_ptr().is_null() {
                        next_waiter.set_prev_ptr(current_waiter_ptr);
                    } else {
                        debug_assert_eq!(next_waiter.prev_ptr(), current_waiter_ptr);
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
                let next_waiter_ptr = (*current_waiter_ptr).next_ptr();
                if let Some(next_waiter) = next_waiter_ptr.as_ref() {
                    next_waiter.set_prev_ptr(current_waiter_ptr);
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
                let prev_waiter_ptr = (*current_waiter_ptr).prev_ptr();
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
                    self.condition_variable.notify_one();
                }
                State::Polling(waker) => {
                    waker.wake();
                }
                State::ResultReady(_) | State::Initialized => (),
            }
        }
    }

    pub(crate) fn next_ptr(&self) -> *const Self {
        self.forward_link.load(Acquire)
    }

    pub(crate) fn prev_ptr(&self) -> *const Self {
        self.backward_link.load(Acquire)
    }

    pub(crate) fn set_next_ptr(&self, next_ptr: *const Self) {
        self.forward_link.store(next_ptr.cast_mut(), Release);
    }

    pub(crate) fn set_prev_ptr(&self, next_ptr: *const Self) {
        self.backward_link.store(next_ptr.cast_mut(), Release);
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
                if let Ok(returned) = self.condition_variable.wait(state) {
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
            .field("forward_link", &self.forward_link)
            .field("backward_link", &self.backward_link)
            .field("state", &self.state)
            .field("condition_variable", &self.condition_variable)
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
