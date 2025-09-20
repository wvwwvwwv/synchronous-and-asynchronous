//! Primitive synchronization operation types.

use crate::wait_queue::WaitQueue;

/// Operation types.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Opcode {
    /// Acquires exclusive ownership.
    Exclusive,
    /// Acquires shared ownership.
    Shared,
    /// Acquires semaphores.
    Semaphore(u8),
    /// Waits for a condition without trying to acquire resources.
    Wait,
}

impl Opcode {
    /// Checks if the resourced expressed in `self` can be released from `state`.
    #[inline]
    pub(crate) const fn can_release(self, state: usize) -> bool {
        match self {
            Opcode::Exclusive => {
                let data = state & WaitQueue::DATA_MASK;
                data == WaitQueue::DATA_MASK
            }
            Opcode::Shared => {
                let data = state & WaitQueue::DATA_MASK;
                data >= 1 && data != WaitQueue::DATA_MASK
            }
            Opcode::Semaphore(count) => {
                let data = state & WaitQueue::DATA_MASK;
                let count = count as usize;
                data >= count
            }
            Opcode::Wait => true,
        }
    }

    /// Converts the operation mode into a `usize` value representing resources held by the
    /// corresponding synchronization primitive.
    #[inline]
    pub(crate) const fn release_count(self) -> usize {
        match self {
            Opcode::Exclusive => WaitQueue::DATA_MASK,
            Opcode::Shared => 1,
            Opcode::Semaphore(count) => {
                let count = count as usize;
                debug_assert!(count <= WaitQueue::LOCKED_FLAG);
                count
            }
            Opcode::Wait => 0,
        }
    }
}
