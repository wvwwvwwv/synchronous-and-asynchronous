//! [`Config`] defines common configuration options for synchronization primitives.

use std::fmt;
#[cfg(not(feature = "loom"))]
use std::thread::yield_now;

#[cfg(feature = "loom")]
use loom::thread::yield_now;

/// [`Config`] defines common configuration options for synchronization primitives.
pub trait Config: fmt::Debug + Default {
    /// Defines the number of times to spin before entering a wait queue.
    #[inline]
    #[must_use]
    fn spin_count() -> usize {
        4096
    }

    /// Defines the backoff function to use when spinning.
    #[inline]
    fn backoff(spin_count: usize) {
        if spin_count % 64 == 0 {
            yield_now();
        }
    }
}

/// Default configuration for synchronization primitives.
#[derive(Debug, Default)]
pub struct DefaultConfig;

impl Config for DefaultConfig {}
