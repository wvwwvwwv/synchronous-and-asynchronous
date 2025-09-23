//! Wait queue implementation.

use std::cell::UnsafeCell;
use std::future::Future;
use std::marker::PhantomPinned;
use std::mem::align_of;
use std::pin::Pin;
use std::ptr::{from_ref, null, null_mut, with_exposed_provenance};
use std::sync::atomic::Ordering::{AcqRel, Acquire, Relaxed, Release};
#[cfg(not(feature = "loom"))]
use std::sync::atomic::{AtomicPtr, AtomicU16};
use std::task::{Context, Poll, Waker};
#[cfg(not(feature = "loom"))]
use std::thread::{Thread, current, park, yield_now};

#[cfg(feature = "loom")]
use loom::sync::atomic::{AtomicPtr, AtomicU16};
#[cfg(feature = "loom")]
use loom::thread::{Thread, current, park, yield_now};

use crate::opcode::Opcode;
use crate::sync_primitive::SyncPrimitive;

/// Fair and heap-free intrusive wait queue for locking primitives in this crate.
///
/// [`WaitQueue`] itself forms an intrusive linked list of entries where entries are pushed at the
/// tail and popped from the head. [`WaitQueue`] is can be located using a `57-bit` pointer thus
/// allowing the lower 7 bits to denote additional states.
#[derive(Debug, Default)]
#[repr(align(8))]
pub(crate) struct WaitQueue {
    /// Wait queue entry raw data.
    #[cfg(not(feature = "loom"))]
    raw_data: UnsafeCell<[u64; 16]>,
    #[cfg(feature = "loom")] // Loom types are larger than those in the standard library.
    raw_data: UnsafeCell<[[u64; 16]; 32]>,
    /// The [`WaitQueue`] cannot be unpinned since it forms an intrusive linked list.
    _pinned: PhantomPinned,
}

/// Wait queue entry.
#[derive(Debug)]
#[repr(align(8))]
pub(crate) struct Entry {
    /// Points to the entry anchor that was pushed right before this entry.
    next_entry_anchor_ptr: AtomicPtr<u64>,
    /// Points to the entry that was pushed right after this entry.
    prev_entry_ptr: AtomicPtr<Self>,
    /// Operation type.
    opcode: Opcode,
    /// Operation state.
    state: AtomicU16,
    /// Indicates that the wait queue entry is enqueued.
    enqueued: std::sync::atomic::AtomicBool, // `Loom` is too slow when it is added to modeling.
    /// Monitors the result.
    monitor: Monitor,
    /// Context cleanup function when a [`WaitQueue`] is cancelled.
    drop_callback: fn(&Self),
    /// Address of the corresponding synchronization primitive.
    addr: usize,
    /// Offset of the entry within the wait queue.
    offset: u16,
}

/// Helper struct for pinning a [`WaitQueue`] to the stack and awaiting it without consuming it.
pub(crate) struct PinnedEntry<'e>(pub(crate) Pin<&'e Entry>);

/// Contextual data for asynchronous [`WaitQueue`].
#[derive(Debug)]
struct AsyncContext {
    /// Waker to wake an executor when the result is ready.
    waker: UnsafeCell<Option<Waker>>,
}

/// Contextual data for synchronous [`WaitQueue`].
#[derive(Debug)]
struct SyncContext {
    /// Waker to wake an executor when the result is ready.
    thread: UnsafeCell<Option<Thread>>,
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
    /// Virtual alignment of the wait queue.
    #[cfg(not(feature = "loom"))]
    pub(crate) const VIRTUAL_ALIGNMENT: usize = 128;
    #[cfg(feature = "loom")]
    pub(crate) const VIRTUAL_ALIGNMENT: usize = 4096;

    /// Indicates that the wait queue is being processed by a thread.
    #[cfg(not(feature = "loom"))]
    pub(crate) const LOCKED_FLAG: usize = Self::VIRTUAL_ALIGNMENT >> 1;
    #[cfg(feature = "loom")]
    pub(crate) const LOCKED_FLAG: usize = 64;

    /// Mask to extract additional information tagged with the [`WaitQueue`] memory address.
    pub(crate) const DATA_MASK: usize = Self::LOCKED_FLAG - 1;

    /// Mask to extract the memory address part from a `usize` value.
    pub(crate) const ADDR_MASK: usize = !(Self::LOCKED_FLAG | Self::DATA_MASK);

    /// Creates a new [`WaitQueue`].
    pub(crate) fn construct<S: SyncPrimitive>(
        self: Pin<&Self>,
        sync_primitive: &S,
        opcode: Opcode,
        is_sync: bool,
    ) {
        debug_assert_eq!(align_of::<WaitQueue>(), 8);
        debug_assert_eq!(align_of::<Entry>(), 8);
        debug_assert!(size_of::<Entry>() <= Self::VIRTUAL_ALIGNMENT / 2);

        let (anchor_ptr, offset) = self.anchor_ptr();
        unsafe {
            // `0` represents the initial state, therefore take the compliment of the offset.
            *anchor_ptr.cast_mut() = u64::MAX - u64::try_from(offset).unwrap_or(0);
        }
        let entry_ptr = Self::to_entry_ptr(anchor_ptr);
        let monitor = if is_sync {
            Monitor::Sync(SyncContext {
                thread: UnsafeCell::new(None),
            })
        } else {
            Monitor::Async(AsyncContext {
                waker: UnsafeCell::new(None),
            })
        };
        let entry = Entry {
            next_entry_anchor_ptr: AtomicPtr::new(null_mut()),
            prev_entry_ptr: AtomicPtr::new(null_mut()),
            opcode,
            state: AtomicU16::new(0),
            enqueued: std::sync::atomic::AtomicBool::new(false),
            monitor,
            drop_callback: S::drop_wait_queue_entry,
            addr: sync_primitive.addr(),
            offset: u16::try_from(
                entry_ptr.expose_provenance() - self.raw_data.get().expose_provenance(),
            )
            .unwrap_or(0),
        };
        unsafe {
            entry_ptr.cast_mut().write(entry);
        }
    }

    /// Returns a reference to the entry.
    #[inline]
    pub(crate) fn entry(&self) -> &Entry {
        unsafe { &*Self::to_entry_ptr(self.anchor_ptr().0) }
    }

    /// Returns the entry pointer derived from the anchor pointer.
    #[inline]
    pub(crate) fn to_entry_ptr(anchor_ptr: *const u64) -> *const Entry {
        let anchor_val = unsafe { *anchor_ptr };
        if anchor_val == 0 {
            // No entry exists.
            return null();
        }

        anchor_ptr
            .map_addr(|addr| {
                debug_assert_eq!(addr % Self::VIRTUAL_ALIGNMENT, 0);

                let offset = usize::try_from(u64::MAX - anchor_val).unwrap_or(0);
                let start_addr = addr - offset;
                debug_assert_eq!(start_addr % 8, 0);

                if offset < Self::VIRTUAL_ALIGNMENT / 2 {
                    // The anchor is in the first half, so the entry is in the second half.
                    start_addr + Self::VIRTUAL_ALIGNMENT / 2
                } else {
                    // The anchor is in the second half, so the entry is in the first half.
                    start_addr
                }
            })
            .cast::<Entry>()
    }

    /// Converts a synchronization primitive state into an anchor pointer.
    #[inline]
    pub(crate) fn to_anchor_ptr(state: usize) -> *const u64 {
        let anchor_addr = state & Self::ADDR_MASK;
        if anchor_addr == 0 {
            return null();
        }
        with_exposed_provenance::<u64>(anchor_addr)
    }

    /// Returns the anchor pointer that is used to locate the wait queue entry.
    pub(crate) fn anchor_ptr(&self) -> (*const u64, usize) {
        let start_addr = self.raw_data.get();
        let mut offset = 0;
        let anchor_ptr = start_addr
            .map_addr(|addr| {
                let anchor_addr = if addr % Self::VIRTUAL_ALIGNMENT == 0 {
                    // Perfectly aligned, so the anchor is at the start address, and the entry is at
                    // 64th byte.
                    //
                    // `128: start/anchor | 192: entry`.
                    addr
                } else {
                    // If the address is not perfectly aligned, we need to round up to the next
                    // multiple of `Self::VIRTUAL_ALIGNMENT`.
                    //
                    // `32: start/entry | 128: anchor`.
                    // `64: start/entry | 128: anchor`.
                    // `96: start | 128: anchor | 160: entry`.
                    addr + Self::VIRTUAL_ALIGNMENT - (addr % Self::VIRTUAL_ALIGNMENT)
                };
                debug_assert_eq!(addr % 8, 0);
                debug_assert_eq!(anchor_addr % Self::VIRTUAL_ALIGNMENT, 0);
                debug_assert!(anchor_addr - addr < Self::VIRTUAL_ALIGNMENT);
                offset = anchor_addr - addr;
                anchor_addr
            })
            .cast::<u64>();
        (anchor_ptr, offset)
    }
}

impl Entry {
    /// A method is used in the wrong mode.
    pub(crate) const ERROR_WRONG_MODE: u8 = u8::MAX;

    /// Indicates that a result is set.
    const RESULT_SET: u16 = 1_u16 << u8::BITS;

    /// Indicates that a waker is set.
    const WAKER_SET: u16 = 1_u16 << (u8::BITS + 1);

    /// Indicates that a result is finalized.
    const RESULT_FINALIZED: u16 = 1_u16 << (u8::BITS + 2);

    /// Indicates that a result is acknowledged.
    const RESULT_ACKED: u16 = 1_u16 << (u8::BITS + 3);

    /// Returns the anchor pointer derived from the entry pointer.
    #[inline]
    pub(crate) fn to_wait_queue_ptr(entry_ptr: *const Self) -> *const WaitQueue {
        entry_ptr
            .map_addr(|addr| addr - unsafe { usize::from((*entry_ptr).offset) })
            .cast::<WaitQueue>()
    }

    /// Gets a pointer to the next entry anchor.
    ///
    /// The next entry is the one that was pushed right before this entry.
    pub(crate) fn next_entry_anchor_ptr(&self) -> *const u64 {
        self.next_entry_anchor_ptr.load(Acquire)
    }

    /// Gets a pointer to the next entry.
    ///
    /// The next entry is the one that was pushed right before this entry.
    pub(crate) fn next_entry_ptr(&self) -> *const Self {
        let anchor_ptr = self.next_entry_anchor_ptr.load(Acquire);
        if anchor_ptr.is_null() {
            return null();
        }
        WaitQueue::to_entry_ptr(anchor_ptr)
    }

    /// Gets a pointer to the previous entry.
    ///
    /// The previous entry is the one that was pushed right after this entry.
    pub(crate) fn prev_entry_ptr(&self) -> *const Self {
        self.prev_entry_ptr.load(Acquire)
    }

    /// Updates the next entry anchor pointer.
    pub(crate) fn update_next_entry_anchor_ptr(&self, next_entry_anchor_ptr: *const u64) {
        debug_assert_eq!(
            next_entry_anchor_ptr as usize % WaitQueue::VIRTUAL_ALIGNMENT,
            0
        );
        self.next_entry_anchor_ptr
            .store(next_entry_anchor_ptr.cast_mut(), Release);
    }

    /// Updates the previous entry pointer.
    pub(crate) fn update_prev_entry_ptr(&self, prev_entry_ptr: *const Self) {
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

    /// Returns the corresponding synchronization primitive reference.
    pub(crate) fn sync_primitive_ref<S: SyncPrimitive>(&self) -> &S {
        unsafe { &*with_exposed_provenance::<S>(self.addr) }
    }

    /// Sets a pointer to the previous entry on each entry by forward-iterating over entries.
    pub(crate) fn set_prev_ptr(tail_entry_ptr: *const Self) {
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

    /// Forward-iterates over entries, calling the supplied closure for each entry.
    ///
    /// Stops iteration if the closure returns `true`.
    pub(crate) fn iter_forward<F: FnMut(&Self, Option<&Self>) -> bool>(
        tail_entry_ptr: *const Self,
        set_prev: bool,
        mut f: F,
    ) {
        let mut entry_ptr = tail_entry_ptr;
        while !entry_ptr.is_null() {
            entry_ptr = unsafe {
                let next_entry_ptr = (*entry_ptr).next_entry_ptr();
                if set_prev {
                    if let Some(next_entry) = next_entry_ptr.as_ref() {
                        next_entry.update_prev_entry_ptr(entry_ptr);
                    }
                }

                // The result is set here, so the scope should be protected.
                if f(&*entry_ptr, next_entry_ptr.as_ref()) {
                    return;
                }
                next_entry_ptr
            };
        }
    }

    /// Backward-iterates over entries, calling the supplied closure for each entry.
    ///
    /// Stops iteration if the closure returns `true`.
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
        let mut state = self.state.load(Acquire);
        loop {
            debug_assert_eq!(state & Self::RESULT_SET, 0);
            debug_assert_eq!(state & Self::RESULT_FINALIZED, 0);

            // Once the result is set, a waker cannot be set.
            let next_state = (state | Self::RESULT_SET) | u16::from(result);
            match self
                .state
                .compare_exchange_weak(state, next_state, AcqRel, Acquire)
            {
                Ok(_) => {
                    state = next_state;
                    break;
                }
                Err(new_state) => state = new_state,
            }
        }

        if state & Self::WAKER_SET == Self::WAKER_SET {
            // A waker had been set before the result was set.
            unsafe {
                match &self.monitor {
                    Monitor::Async(async_context) => {
                        if let Some(waker) = (*async_context.waker.get()).take() {
                            self.state.fetch_or(Self::RESULT_FINALIZED, AcqRel);
                            waker.wake();
                            return;
                        }
                    }
                    Monitor::Sync(sync_context) => {
                        if let Some(thread) = (*sync_context.thread.get()).take() {
                            self.state.fetch_or(Self::RESULT_FINALIZED, AcqRel);
                            thread.unpark();
                            return;
                        }
                    }
                }
            }
        }
        self.state.fetch_or(Self::RESULT_FINALIZED, AcqRel);
    }

    /// Polls the result, asynchronously.
    pub(crate) fn poll_result_async(&self, cx: &mut Context<'_>) -> Poll<u8> {
        let Monitor::Async(async_context) = &self.monitor else {
            return Poll::Ready(Self::ERROR_WRONG_MODE);
        };

        if let Some(result) = self.try_acknowledge_result() {
            return Poll::Ready(result);
        }

        let mut this_waker = None;
        let state = self.state.load(Acquire);
        if state & Self::RESULT_SET == Self::RESULT_SET {
            // No need to install the waker.
            if let Some(result) = self.try_acknowledge_result() {
                return Poll::Ready(result);
            }
        } else if state & Self::WAKER_SET == Self::WAKER_SET {
            // Replace the waker by clearing the flag first.
            if self
                .state
                .compare_exchange_weak(state, state & !Self::WAKER_SET, AcqRel, Acquire)
                .is_ok()
            {
                this_waker.replace(cx.waker().clone());
            }
        } else {
            this_waker.replace(cx.waker().clone());
        }

        if let Some(waker) = this_waker {
            unsafe {
                (*async_context.waker.get()).replace(waker);
            }
            if self.state.fetch_or(Self::WAKER_SET, Release) & Self::RESULT_SET == Self::RESULT_SET
            {
                // The result has been set, so the waker will not be notified.
                cx.waker().wake_by_ref();
            }
        } else {
            // The waker is not set, so we need to wake the task.
            cx.waker().wake_by_ref();
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

            let mut this_thread = None;
            let state = self.state.load(Acquire);
            if state & Self::RESULT_SET == Self::RESULT_SET {
                // No need to install the thread.
                if let Some(result) = self.try_acknowledge_result() {
                    return result;
                }
            } else if state & Self::WAKER_SET == Self::WAKER_SET {
                // Replace the thread by clearing the flag first.
                if self
                    .state
                    .compare_exchange_weak(state, state & !Self::WAKER_SET, AcqRel, Acquire)
                    .is_ok()
                {
                    this_thread.replace(current());
                }
            } else {
                this_thread.replace(current());
            }

            if let Some(thread) = this_thread {
                unsafe {
                    (*sync_context.thread.get()).replace(thread);
                }
                if self.state.fetch_or(Self::WAKER_SET, Release) & Self::RESULT_SET
                    == Self::RESULT_SET
                {
                    // The result has been set, so the thread will not be signaled.
                    yield_now();
                } else {
                    park();
                }
            } else {
                // The thread is not set, so we need to yield the thread.
                yield_now();
            }
        }
    }

    /// The wait queue entry has been enqueued.
    pub(crate) fn enqueued(&self) {
        debug_assert!(!self.enqueued.load(Relaxed));
        self.enqueued.store(true, Release);
    }

    /// Returns `true` if the result has been finalized.
    pub(crate) fn result_finalized(&self) -> bool {
        let state = self.state.load(Acquire);
        state & Self::RESULT_FINALIZED == Self::RESULT_FINALIZED
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
    pub(crate) fn try_acknowledge_result(&self) -> Option<u8> {
        let state = self.state.load(Acquire);
        if state & Self::RESULT_FINALIZED == Self::RESULT_FINALIZED {
            debug_assert_ne!(state & Self::RESULT_SET, 0);
            self.state.fetch_or(Self::RESULT_ACKED, Release);
            return u8::try_from(state & ((1_u16 << u8::BITS) - 1)).ok();
        }
        None
    }

    /// Prepares for dropping `self`.
    fn prepare_drop(entry_ptr: *mut Self) {
        let this = unsafe { &mut *entry_ptr };
        let state = this.state.load(Acquire);
        // The wait queue entry was enqueued or had a result set but was not acknowledged.
        //
        // The wait queue entry owner will acquire the resource or has already acquired it without
        // knowing it, therefore the resource needs to be released.
        if (this.enqueued.load(Acquire) || state & Self::RESULT_SET == Self::RESULT_SET)
            && state & Self::RESULT_ACKED == 0
        {
            (this.drop_callback)(this);
            this.enqueued.store(false, Release);
        }
    }
}

impl Drop for WaitQueue {
    #[inline]
    fn drop(&mut self) {
        let anchor_ptr = self.anchor_ptr().0;
        let entry_ptr = Self::to_entry_ptr(anchor_ptr).cast_mut();
        if entry_ptr.is_null() {
            return;
        }
        unsafe {
            Entry::prepare_drop(entry_ptr);
            entry_ptr.drop_in_place();
        }
    }
}

unsafe impl Send for WaitQueue {}
unsafe impl Sync for WaitQueue {}

impl Future for PinnedEntry<'_> {
    type Output = u8;

    #[inline]
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        this.0.poll_result_async(cx)
    }
}

unsafe impl Send for Monitor {}
unsafe impl Sync for Monitor {}
