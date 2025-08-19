#![deny(missing_docs, clippy::all, clippy::pedantic)]
#![doc = include_str!("../README.md")]

pub mod lock;
pub use lock::Lock;

pub mod opcode;
pub use opcode::Opcode;

pub mod semaphore;
pub use semaphore::Semaphore;

mod sync_primitive;
mod wait_queue;

#[cfg(test)]
mod tests;
