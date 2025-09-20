#![deny(missing_docs, clippy::all, clippy::pedantic)]
#![doc = include_str!("../README.md")]

pub mod gate;
pub use gate::Gate;

mod lock;
pub use lock::Lock;

mod pager;
pub use pager::Pager;

mod semaphore;
pub use semaphore::Semaphore;

mod opcode;
mod sync_primitive;
mod wait_queue;

#[cfg(test)]
mod tests;
