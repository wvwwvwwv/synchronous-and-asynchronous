//! Primitive synchronization operation types.

/// Operation types.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum Opcode {
    /// Acquires exclusive ownership.
    Exclusive,
    /// Acquires shared ownership.
    Shared,
    /// Acquires semaphores.
    Semaphore(u8),
    /// Cleanup stale wait queue entries.
    Cleanup,
}
