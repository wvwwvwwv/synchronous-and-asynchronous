#![deny(missing_docs, clippy::all, clippy::pedantic)]
#![doc = include_str!("../README.md")]

mod gate;
pub use gate::Gate;

mod lock;
pub use lock::Lock;

mod semaphore;
pub use semaphore::Semaphore;

mod opcode;
mod sync_primitive;
mod wait_queue;

#[cfg(test)]
mod tests;
