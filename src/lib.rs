#![deny(missing_docs, clippy::all, clippy::pedantic)]
#![doc = include_str!("../README.md")]

pub mod barrier;
pub use barrier::Barrier;

pub mod gate;
pub use gate::Gate;

pub mod lock;
pub use lock::Lock;

pub mod pager;
pub use pager::Pager;

pub mod semaphore;
pub use semaphore::Semaphore;

mod opcode;
mod sync_primitive;
mod wait_queue;

#[cfg(test)]
mod tests;
