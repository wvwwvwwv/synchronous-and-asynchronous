//! Wait queue implementation.

#[cfg(feature = "loom")]
use loom::sync::{Condvar, Mutex};
use std::cell::UnsafeCell;
use std::fmt;
use std::future::Future;
use std::mem::align_of;
use std::pin::Pin;
use std::ptr::{addr_of, null_mut, with_exposed_provenance};
use std::sync::atomic::Ordering::{AcqRel, Acquire, Relaxed, Release};
#[cfg(not(feature = "loom"))]
use std::sync::atomic::{AtomicBool, AtomicPtr};
#[cfg(not(feature = "loom"))]
use std::sync::{Condvar, Mutex};
use std::task::Waker;
use std::task::{Context, Poll};

#[cfg(feature = "loom")]
use loom::sync::atomic::{AtomicBool, AtomicPtr};

use crate::Opcode;

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
    /// The context will never be updated.
    finalized: AtomicBool,
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
    /// The alignment of the [`WaitQueue`] in memory represented as a number of bits.
    pub(crate) const ALIGNMENT_BITS: usize = 7;

    /// Indicates that the wait queue is being processed by a thread.
    pub(crate) const LOCKED_FLAG: usize = 1_usize << (Self::ALIGNMENT_BITS - 1);

    /// Mark to extract additional information tagged with the [`WaitQueue`] memory address.
    pub(crate) const DATA_MASK: usize = (1_usize << (Self::ALIGNMENT_BITS - 1)) - 1;

    /// Mask to extract the memory address part from a `usize` value.
    pub(crate) const ADDR_MASK: usize = !(Self::LOCKED_FLAG | Self::DATA_MASK);

    /// Creates a new [`WaitQueue`].
    pub(crate) fn new(opcode: Opcode, async_cleanup_fn: Option<AsyncContextCleaner>) -> Self {
        let monitor = if let Some(async_cleanup_fn) = async_cleanup_fn {
            Monitor::Async(AsyncContext {
                result: AtomicBool::new(false),
                ready: AtomicBool::new(false),
                finalized: AtomicBool::new(false),
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
        let wait_queue_ptr: *const Self = addr_of!(*this);
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
    pub(crate) fn any_forward<F: FnMut(&Self, Option<&Self>) -> bool>(
        tail_entry_ptr: *const Self,
        mut f: F,
    ) -> bool {
        let mut entry_ptr = tail_entry_ptr;
        while !entry_ptr.is_null() {
            entry_ptr = unsafe {
                let next_entry_ptr = (*entry_ptr).next_entry_ptr();
                if let Some(next_entry) = next_entry_ptr.as_ref() {
                    next_entry.update_prev_entry_ptr(entry_ptr);
                }

                let _miri_scope_protector = _miri_scope_protector!();
                if f(&*entry_ptr, next_entry_ptr.as_ref()) {
                    return true;
                }
                next_entry_ptr
            };
        }
        false
    }

    /// Backward-iterates over entries, and returns `true` when the supplied closure returns `true`.
    pub(crate) fn any_backward<F: FnMut(&Self, Option<&Self>) -> bool>(
        head_entry_ptr: *const Self,
        mut f: F,
    ) -> bool {
        let mut entry_ptr = head_entry_ptr;
        while !entry_ptr.is_null() {
            entry_ptr = unsafe {
                let prev_entry_ptr = (*entry_ptr).prev_entry_ptr();
                let _miri_scope_protector = _miri_scope_protector!();
                if f(&*entry_ptr, prev_entry_ptr.as_ref()) {
                    return true;
                }
                prev_entry_ptr
            };
        }
        false
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

    /// Polls the result, synchronously.
    pub(crate) fn poll_result(&self) -> bool {
        let Monitor::Sync(sync_context) = &self.monitor else {
            debug_assert!(false, "Logic error");
            return false;
        };

        while let Ok(mut state) = sync_context.state.lock() {
            while state.is_none() {
                if let Ok(returned) = sync_context.cond_var.wait(state) {
                    state = returned;
                } else {
                    debug_assert!(false, "The mutex can never be poisoned");
                    return false;
                }
            }
            if let Some(result) = state.take() {
                let _miri_scope_protector = _miri_scope_protector!();
                return result;
            }
        }

        debug_assert!(false, "The mutex can never be poisoned");
        false
    }

    /// Returns `true` if asynchronous polling was successfully completed.
    pub(crate) fn async_poll_completed(&self) -> bool {
        let Monitor::Async(async_context) = &self.monitor else {
            return false;
        };
        async_context.finalized.load(Acquire)
    }
}

impl Drop for WaitQueue {
    #[inline]
    fn drop(&mut self) {
        let Monitor::Async(async_context) = &self.monitor else {
            return;
        };
        if !async_context.finalized.load(Relaxed) {
            async_context.cleaner.1(self, async_context.cleaner.0);
        }
    }
}

impl Future for WaitQueue {
    type Output = bool;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let Monitor::Async(async_context) = &mut self.monitor else {
            debug_assert!(false, "Logic error");
            let _miri_scope_protector = _miri_scope_protector!();
            return Poll::Ready(false);
        };
        if async_context.finalized.load(Acquire) {
            debug_assert!(async_context.ready.load(Relaxed));
            let _miri_scope_protector = _miri_scope_protector!();
            return Poll::Ready(async_context.result.load(Relaxed));
        }

        let waker = cx.waker().clone();
        if async_context.ready.load(Acquire) {
            // No need to install the waker.
            waker.wake();
            if async_context.finalized.load(Acquire) {
                let _miri_scope_protector = _miri_scope_protector!();
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
                *async_context.waker.get_mut() = Some(waker);
            }
            async_context.waker_lock.store(false, Release);
        } else {
            // Failing to lock means that the result is ready.
            debug_assert!(async_context.ready.load(Relaxed));
            waker.wake();
            if async_context.finalized.load(Acquire) {
                let _miri_scope_protector = _miri_scope_protector!();
                return Poll::Ready(async_context.result.load(Relaxed));
            }
        }

        Poll::Pending
    }
}

impl fmt::Debug for AsyncContext {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AsyncContext")
            .field("result", &self.result)
            .field("ready", &self.ready)
            .field("finalized", &self.finalized)
            .field("waker_lock", &self.waker_lock)
            .field("waker", &self.waker)
            .field("cleaner_arg", &self.cleaner.0)
            .finish()
    }
}
