//! Wait queue implementation.

use std::cell::UnsafeCell;
use std::fmt;
use std::future::Future;
use std::mem::align_of;
use std::pin::Pin;
use std::ptr::{from_ref, null_mut, with_exposed_provenance};
use std::sync::atomic::Ordering::{AcqRel, Acquire, Relaxed, Release};
#[cfg(not(feature = "loom"))]
use std::sync::atomic::{AtomicBool, AtomicPtr};
#[cfg(not(feature = "loom"))]
use std::sync::{Condvar, Mutex};
use std::task::{Context, Poll, Waker};

#[cfg(feature = "loom")]
use loom::sync::atomic::{AtomicBool, AtomicPtr};
#[cfg(feature = "loom")]
use loom::sync::{Condvar, Mutex};

use crate::opcode::Opcode;

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
    /// Operation type.
    opcode: Opcode,
    /// Monitors the result.
    monitor: Monitor,
}

/// Helper struct for pinning a [`WaitQueue`] to the stack and awaiting it.
pub(crate) struct PinnedWaitQueue<'w>(pub(crate) Pin<&'w WaitQueue>);

/// Function to be invoked when [`WaitQueue`] is dropped before completion.
///
/// The first `usize` argument is passed to the function along with `self`.
type AsyncContextCleaner = (usize, fn(&WaitQueue, usize));

/// Contextual data for asynchronous [`WaitQueue`].
struct AsyncContext {
    /// Operation result.
    result: AtomicBool,
    /// Result is set.
    ready: AtomicBool,
    /// The result is finalized, and other threads will not access the context.
    finalized: AtomicBool,
    /// The result has been acknowledged.
    acknowledged: AtomicBool,
    /// Lock to protect `waker`.
    waker_lock: AtomicBool,
    /// Waker to wake an executor when the result is ready.
    waker: UnsafeCell<Option<Waker>>,
    /// Context cleaner when [`WaitQueue`] is cancelled.
    cleaner: AsyncContextCleaner,
}

/// Contextual data for synchronous [`WaitQueue`].
#[derive(Debug, Default)]
struct SyncContext {
    /// The state of this entry.
    state: Mutex<Option<bool>>,
    /// Conditional variable to send a signal to a blocking/synchronous waiter.
    cond_var: Condvar,
}

/// Monitors the result.
#[derive(Debug)]
enum Monitor {
    /// Monitors asynchronously.
    Async(AsyncContext),
    /// Monitors synchronously.
    Sync(SyncContext),
}

impl WaitQueue {
    /// Indicates that the wait queue is being processed by a thread.
    pub(crate) const LOCKED_FLAG: usize = align_of::<Self>() >> 1;

    /// Mark to extract additional information tagged with the [`WaitQueue`] memory address.
    pub(crate) const DATA_MASK: usize = (align_of::<Self>() >> 1) - 1;

    /// Mask to extract the memory address part from a `usize` value.
    pub(crate) const ADDR_MASK: usize = !(Self::LOCKED_FLAG | Self::DATA_MASK);

    /// Creates a new [`WaitQueue`].
    pub(crate) fn new(opcode: Opcode, async_cleanup_fn: Option<AsyncContextCleaner>) -> Self {
        let monitor = if let Some(async_cleanup_fn) = async_cleanup_fn {
            Monitor::Async(AsyncContext {
                result: AtomicBool::new(false),
                ready: AtomicBool::new(false),
                finalized: AtomicBool::new(false),
                acknowledged: AtomicBool::new(false),
                waker_lock: AtomicBool::new(false),
                waker: UnsafeCell::new(None),
                cleaner: async_cleanup_fn,
            })
        } else {
            Monitor::Sync(SyncContext::default())
        };
        Self {
            next_entry_ptr: AtomicPtr::new(null_mut()),
            prev_entry_ptr: AtomicPtr::new(null_mut()),
            opcode,
            monitor,
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
    pub(crate) fn update_next_entry_ptr(&self, next_entry_ptr: *const Self) {
        debug_assert_eq!(next_entry_ptr as usize % align_of::<Self>(), 0);
        self.next_entry_ptr
            .store(next_entry_ptr.cast_mut(), Release);
    }

    /// Updates the previous entry pointer.
    pub(crate) fn update_prev_entry_ptr(&self, prev_entry_ptr: *const Self) {
        debug_assert_eq!(prev_entry_ptr as usize % align_of::<Self>(), 0);
        self.prev_entry_ptr
            .store(prev_entry_ptr.cast_mut(), Release);
    }

    /// Returns the operation code.
    pub(crate) const fn opcode(&self) -> Opcode {
        self.opcode
    }

    /// Converts a reference to `Self` to a raw pointer.
    pub(crate) fn ref_to_ptr(this: &Self) -> *const Self {
        let wait_queue_ptr: *const Self = from_ref(this);
        debug_assert_eq!(wait_queue_ptr as usize % align_of::<Self>(), 0);
        wait_queue_ptr
    }

    /// Converts the memory address of `Self` to a raw pointer.
    pub(crate) fn addr_to_ptr(wait_queue_addr: usize) -> *const Self {
        debug_assert_eq!(wait_queue_addr % align_of::<Self>(), 0);
        with_exposed_provenance(wait_queue_addr)
    }

    /// Installs a pointer to the previous entry on each entry by forward-iterating over entries.
    pub(crate) fn install_backward_link(tail_entry_ptr: *const Self) {
        let mut entry_ptr = tail_entry_ptr;
        while !entry_ptr.is_null() {
            entry_ptr = unsafe {
                let next_entry_ptr = (*entry_ptr).next_entry_ptr();
                if let Some(next_entry) = next_entry_ptr.as_ref() {
                    if next_entry.prev_entry_ptr().is_null() {
                        next_entry.update_prev_entry_ptr(entry_ptr);
                    } else {
                        debug_assert_eq!(next_entry.prev_entry_ptr(), entry_ptr);
                        return;
                    }
                }
                next_entry_ptr
            };
        }
    }

    /// Forward-iterates over entries, and returns `true` when the supplied closure returns `true`.
    pub(crate) fn iter_forward<F: FnMut(&Self, Option<&Self>) -> bool>(
        tail_entry_ptr: *const Self,
        mut f: F,
    ) {
        let mut entry_ptr = tail_entry_ptr;
        while !entry_ptr.is_null() {
            entry_ptr = unsafe {
                let next_entry_ptr = (*entry_ptr).next_entry_ptr();
                if let Some(next_entry) = next_entry_ptr.as_ref() {
                    next_entry.update_prev_entry_ptr(entry_ptr);
                }

                // The result is set here, so the scope should be protected.
                if f(&*entry_ptr, next_entry_ptr.as_ref()) {
                    return;
                }
                next_entry_ptr
            };
        }
    }

    /// Backward-iterates over entries, and returns `true` when the supplied closure returns `true`.
    pub(crate) fn iter_backward<F: FnMut(&Self, Option<&Self>) -> bool>(
        head_entry_ptr: *const Self,
        mut f: F,
    ) {
        let mut entry_ptr = head_entry_ptr;
        while !entry_ptr.is_null() {
            entry_ptr = unsafe {
                let prev_entry_ptr = (*entry_ptr).prev_entry_ptr();
                if f(&*entry_ptr, prev_entry_ptr.as_ref()) {
                    return;
                }
                prev_entry_ptr
            };
        }
    }

    /// Sets the result to the entry.
    pub(crate) fn set_result(&self, result: bool) {
        match &self.monitor {
            Monitor::Async(async_context) => {
                debug_assert!(!async_context.finalized.load(Relaxed));
                async_context.result.store(result, Release);
                async_context.ready.store(true, Release);
                if async_context
                    .waker_lock
                    .compare_exchange(false, true, AcqRel, Relaxed)
                    .is_ok()
                {
                    async_context.waker_lock.store(false, Release);
                }
                unsafe {
                    if let Some(waker) = (*async_context.waker.get()).take() {
                        waker.wake();
                    }
                }
                async_context.finalized.store(true, Release);
            }
            Monitor::Sync(sync_context) => {
                if let Ok(mut state) = sync_context.state.lock() {
                    *state = Some(result);
                    sync_context.cond_var.notify_one();
                }
            }
        }
    }

    /// Polls the result, asynchronously.
    pub(crate) fn poll_result_async(&self, cx: &mut Context<'_>) -> Poll<bool> {
        let Monitor::Async(async_context) = &self.monitor else {
            debug_assert!(false, "Logic error");
            return Poll::Ready(false);
        };
        if async_context.finalized.load(Acquire) {
            debug_assert!(async_context.ready.load(Relaxed));
            async_context.acknowledged.store(true, Release);
            return Poll::Ready(async_context.result.load(Relaxed));
        }

        let waker = cx.waker().clone();
        if async_context.ready.load(Acquire) {
            // No need to install the waker.
            waker.wake();
            if async_context.finalized.load(Acquire) {
                debug_assert!(async_context.ready.load(Relaxed));
                async_context.acknowledged.store(true, Release);
                return Poll::Ready(async_context.result.load(Relaxed));
            }
        } else if async_context
            .waker_lock
            .compare_exchange(false, true, AcqRel, Acquire)
            .is_ok()
        {
            if async_context.ready.load(Acquire) {
                // The result has been ready in the meantime.
                waker.wake();
            } else {
                unsafe {
                    (*async_context.waker.get()) = Some(waker);
                }
            }
            async_context.waker_lock.store(false, Release);
        } else {
            // Failing to lock means that the result is ready.
            waker.wake();
            if async_context.finalized.load(Acquire) {
                debug_assert!(async_context.ready.load(Relaxed));
                async_context.acknowledged.store(true, Release);
                return Poll::Ready(async_context.result.load(Relaxed));
            }
        }

        Poll::Pending
    }

    /// Polls the result, synchronously.
    pub(crate) fn poll_result_sync(&self) -> bool {
        let Monitor::Sync(sync_context) = &self.monitor else {
            debug_assert!(false, "Logic error");
            return false;
        };
        let Ok(mut state) = sync_context.state.lock() else {
            debug_assert!(false, "The mutex can never be poisoned");
            return false;
        };

        loop {
            if let Some(result) = (*state).take() {
                drop(state);
                return result;
            }
            let Ok(returned) = sync_context.cond_var.wait(state) else {
                debug_assert!(false, "The mutex can never be poisoned");
                return false;
            };
            state = returned;
        }
    }

    /// Sets the result as acknowledged if pushing the entry into the wait queue failed.
    pub(crate) fn result_acknowledged(&self) {
        let Monitor::Async(async_context) = &self.monitor else {
            debug_assert!(false, "Logic error");
            return;
        };
        debug_assert!(!async_context.acknowledged.load(Relaxed));

        async_context.acknowledged.store(true, Release);
    }

    /// Returns `true` if the result has been finalized.
    pub(crate) fn result_finalized(&self) -> bool {
        let Monitor::Async(async_context) = &self.monitor else {
            debug_assert!(false, "Logic error");
            return false;
        };
        debug_assert!(!async_context.acknowledged.load(Relaxed));

        async_context.finalized.load(Acquire)
    }
}

impl Drop for WaitQueue {
    #[inline]
    fn drop(&mut self) {
        let Monitor::Async(async_context) = &self.monitor else {
            return;
        };
        if !async_context.acknowledged.load(Acquire) {
            async_context.cleaner.1(self, async_context.cleaner.0);
        }
    }
}

impl Future for PinnedWaitQueue<'_> {
    type Output = bool;

    #[inline]
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        this.0.poll_result_async(cx)
    }
}

/// SAFETY: `UnsafeCell<Waker>` can be sent.
unsafe impl Sync for AsyncContext {}

impl fmt::Debug for AsyncContext {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AsyncContext")
            .field("result", &self.result)
            .field("ready", &self.ready)
            .field("finalized", &self.finalized)
            .field("acknowledged", &self.acknowledged)
            .field("waker_lock", &self.waker_lock)
            .field("waker", &self.waker)
            .field("cleaner_arg", &self.cleaner.0)
            .finish()
    }
}
