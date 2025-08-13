//! [`Semaphore`] is a synchronization primitive that allows a fixed number of threads to access a
//! resource concurrently.

// TODO
#![allow(dead_code, unused_imports)]

use crate::wait_queue::WaitQueue;
#[cfg(feature = "loom")]
use loom::sync::atomic::AtomicUsize;
use std::pin::Pin;
use std::ptr::{addr_of, null};
#[cfg(not(feature = "loom"))]
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering::{self, AcqRel, Acquire, Relaxed, Release};
use std::{fmt, thread};

/// [`Semaphore`] is a synchronization primitive that allows a fixed number of threads to access a
/// resource concurrently.
#[derive(Debug)]
pub struct Semaphore {
    _state: AtomicUsize,
}
