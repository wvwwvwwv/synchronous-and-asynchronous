//! Primitive synchronization operation types.

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
