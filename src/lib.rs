#![deny(missing_docs, clippy::all, clippy::pedantic)]
#![doc = include_str!("../README.md")]

pub mod barrier;
pub use barrier::Barrier;

pub mod gate;
pub use gate::Gate;

pub mod lock;
pub use lock::Lock;

#[cfg(feature = "lock_api")]
pub mod lock_api;
#[cfg(feature = "lock_api")]
pub use lock_api::{
    Mutex, MutexGuard, RwLock, RwLockReadGuard, RwLockWriteGuard, lock_async, read_async,
    write_async,
};

pub mod pager;
pub use pager::Pager;

pub mod semaphore;
pub use semaphore::Semaphore;

mod opcode;
mod sync_primitive;
mod wait_queue;

#[cfg(test)]
mod tests;
