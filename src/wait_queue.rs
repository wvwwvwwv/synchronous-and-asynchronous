//! Wait queue implementation.

use std::future::Future;
use std::mem::align_of;
use std::pin::Pin;
use std::ptr::{from_ref, null_mut, with_exposed_provenance};
#[cfg(not(feature = "loom"))]
use std::sync::Mutex;
use std::sync::atomic::Ordering::{Acquire, Relaxed, Release};
#[cfg(not(feature = "loom"))]
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicU8};
use std::task::{Context, Poll, Waker};
#[cfg(not(feature = "loom"))]
use std::thread::{Thread, current, park, yield_now};

#[cfg(feature = "loom")]
use loom::sync::Mutex;
#[cfg(feature = "loom")]
use loom::sync::atomic::{AtomicBool, AtomicPtr, AtomicU8};
#[cfg(feature = "loom")]
use loom::thread::{Thread, current, park, yield_now};

use crate::opcode::Opcode;

/// Fair and heap-free intrusive wait queue for locking primitives in this crate.
///
/// [`WaitQueue`] itself forms an intrusive linked list of entries where entries are pushed at the
/// tail and popped from the head. [`WaitQueue`] is `128-byte` aligned thus allowing the lower 7
/// bits in a pointer-sized scalar type variable to represent additional states.
#[repr(align(128))]
#[derive(Debug)]
pub(crate) struct WaitQueue {
    /// Points to the entry that was pushed right before this entry.
    next_entry_ptr: AtomicPtr<Self>,
    /// Points to the entry that was pushed right after this entry.
    prev_entry_ptr: AtomicPtr<Self>,
    /// Operation type.
    opcode: Opcode,
    /// Operation result.
    result: AtomicU8,
    /// Result is set.
    ready: AtomicBool,
    /// The result is finalized, and other threads will not access the context.
    finalized: AtomicBool,
    /// The result has been acknowledged.
    acknowledged: AtomicBool,
    /// Monitors the result.
    monitor: Monitor,
}

/// Helper struct for pinning a [`WaitQueue`] to the stack and awaiting it.
pub(crate) struct PinnedWaitQueue<'w>(pub(crate) Pin<&'w WaitQueue>);

/// Contextual data for asynchronous [`WaitQueue`].
#[derive(Debug)]
struct AsyncContext {
    /// Waker to wake an executor when the result is ready.
    waker: Mutex<Option<Waker>>,
    /// Context cleaner when an asynchronous [`WaitQueue`] is cancelled.
    cleaner: fn(&WaitQueue, usize),
    /// Argument to pass to the cleaner function.
    cleaner_arg: usize,
}

/// Contextual data for synchronous [`WaitQueue`].
#[derive(Debug)]
struct SyncContext {
    /// Waker to wake an executor when the result is ready.
    thread: Mutex<Option<Thread>>,
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
    /// The wrong mode of [`WaitQueue`] method is used.
    pub(crate) const ERROR_WRONG_MODE: u8 = u8::MAX;

    /// Indicates that the wait queue is being processed by a thread.
    pub(crate) const LOCKED_FLAG: usize = align_of::<Self>() >> 1;

    /// Mark to extract additional information tagged with the [`WaitQueue`] memory address.
    pub(crate) const DATA_MASK: usize = (align_of::<Self>() >> 1) - 1;

    /// Mask to extract the memory address part from a `usize` value.
    pub(crate) const ADDR_MASK: usize = !(Self::LOCKED_FLAG | Self::DATA_MASK);

    /// Creates a new [`WaitQueue`] for asynchronous method.
    pub(crate) fn new_async(opcode: Opcode, cleaner: fn(&WaitQueue, usize), arg: usize) -> Self {
        let monitor = Monitor::Async(AsyncContext {
            waker: Mutex::new(None),
            cleaner,
            cleaner_arg: arg,
        });
        Self {
            next_entry_ptr: AtomicPtr::new(null_mut()),
            prev_entry_ptr: AtomicPtr::new(null_mut()),
            opcode,
            result: AtomicU8::new(0),
            ready: AtomicBool::new(false),
            finalized: AtomicBool::new(false),
            acknowledged: AtomicBool::new(false),
            monitor,
        }
    }

    /// Creates a new [`WaitQueue`] for synchronous method.
    pub(crate) fn new_sync(opcode: Opcode) -> Self {
        let monitor = Monitor::Sync(SyncContext {
            thread: Mutex::new(None),
        });
        Self {
            next_entry_ptr: AtomicPtr::new(null_mut()),
            prev_entry_ptr: AtomicPtr::new(null_mut()),
            opcode,
            result: AtomicU8::new(0),
            ready: AtomicBool::new(false),
            finalized: AtomicBool::new(false),
            acknowledged: AtomicBool::new(false),
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
    pub(crate) const fn ref_to_ptr(this: &Self) -> *const Self {
        let wait_queue_ptr: *const Self = from_ref(this);
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

    /// Returns `true` if it contains a synchronous context.
    pub(crate) fn is_sync(&self) -> bool {
        matches!(self.monitor, Monitor::Sync(_))
    }

    /// Sets the result to the entry.
    pub(crate) fn set_result(&self, result: u8) {
        debug_assert!(!self.finalized.load(Relaxed));
        self.result.store(result, Release);
        self.ready.store(true, Release);

        match &self.monitor {
            Monitor::Async(async_context) => {
                // Try-locking is sufficient.
                if let Ok(mut waker) = async_context.waker.try_lock() {
                    if let Some(waker) = waker.take() {
                        waker.wake();
                    }
                }
            }
            Monitor::Sync(sync_context) => {
                if let Ok(mut thread) = sync_context.thread.lock() {
                    if let Some(thread) = thread.take() {
                        thread.unpark();
                    }
                }
            }
        }

        self.finalized.store(true, Release);
    }

    /// Polls the result, asynchronously.
    pub(crate) fn poll_result_async(&self, cx: &mut Context<'_>) -> Poll<u8> {
        let Monitor::Async(async_context) = &self.monitor else {
            return Poll::Ready(Self::ERROR_WRONG_MODE);
        };

        if let Some(result) = self.try_acknowledge_result() {
            return Poll::Ready(result);
        }

        let mut this_waker = Some(cx.waker().clone());
        if self.ready.load(Acquire) {
            // No need to install the waker.
            if let Some(result) = self.try_acknowledge_result() {
                return Poll::Ready(result);
            }
        } else if let Ok(mut waker) = async_context.waker.try_lock() {
            if !self.ready.load(Acquire) {
                *waker = this_waker.take();
            }
        }

        if let Some(result) = self.try_acknowledge_result() {
            return Poll::Ready(result);
        }
        if let Some(waker) = this_waker {
            waker.wake();
        }

        Poll::Pending
    }

    /// Polls the result, synchronously.
    pub(crate) fn poll_result_sync(&self) -> u8 {
        let Monitor::Sync(sync_context) = &self.monitor else {
            return Self::ERROR_WRONG_MODE;
        };

        loop {
            if let Some(result) = self.try_acknowledge_result() {
                return result;
            }

            let mut this_thread = Some(current());
            if self.ready.load(Acquire) {
                // No need to install the waker.
                if let Some(result) = self.try_acknowledge_result() {
                    return result;
                }
            } else if let Ok(mut thread) = sync_context.thread.lock() {
                if !self.ready.load(Acquire) {
                    *thread = this_thread.take();
                }
            }

            if let Some(result) = self.try_acknowledge_result() {
                return result;
            }
            if this_thread.is_some() {
                yield_now();
            } else {
                park();
            }
        }
    }

    /// Sets the result as acknowledged if pushing the entry into the wait queue failed.
    pub(crate) fn result_acknowledged(&self) {
        debug_assert!(!self.acknowledged.load(Relaxed));
        self.acknowledged.store(true, Release);
    }

    /// Returns `true` if the result has been finalized.
    pub(crate) fn result_finalized(&self) -> bool {
        debug_assert!(!self.acknowledged.load(Relaxed));
        self.finalized.load(Acquire)
    }

    /// Tries to get the result and acknowledges it.
    pub(crate) fn acknowledge_result_sync(&self) -> u8 {
        loop {
            if let Some(result) = self.try_acknowledge_result() {
                return result;
            }
            yield_now();
        }
    }

    /// Tries to get the result and acknowledges it.
    fn try_acknowledge_result(&self) -> Option<u8> {
        self.finalized.load(Acquire).then(|| {
            debug_assert!(self.ready.load(Relaxed));
            self.acknowledged.store(true, Release);
            self.result.load(Relaxed)
        })
    }
}

impl Drop for WaitQueue {
    #[inline]
    fn drop(&mut self) {
        let Monitor::Async(async_context) = &self.monitor else {
            return;
        };
        if !self.acknowledged.load(Acquire) {
            (async_context.cleaner)(self, async_context.cleaner_arg);
        }
    }
}

impl Future for PinnedWaitQueue<'_> {
    type Output = u8;

    #[inline]
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        this.0.poll_result_async(cx)
    }
}
