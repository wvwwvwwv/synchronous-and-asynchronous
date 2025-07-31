#![deny(missing_docs, clippy::all, clippy::pedantic)]
#![doc = include_str!("../README.md")]

pub mod lock;
pub use lock::Lock;

mod wait_queue;

#[cfg(test)]
mod tests;
