//! [`Config`] defines common configuration options for synchronization primitives.

use std::fmt;

/// [`Config`] defines common configuration options for synchronization primitives.
pub trait Config: fmt::Debug + Default {
    /// Defines the number of times to spin before entering a wait queue.
    #[inline]
    #[must_use]
    fn spin_count() -> usize {
        16384
    }

    /// Defines the backoff function to use when spinning.
    #[inline]
    fn backoff(_spin_count: usize) {}
}

/// Default configuration for synchronization primitives.
#[derive(Debug, Default)]
pub struct DefaultConfig;

impl Config for DefaultConfig {}
