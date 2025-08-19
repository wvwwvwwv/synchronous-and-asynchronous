# Synchronous and Asynchronous Synchronization Primitives

[![Cargo](https://img.shields.io/crates/v/saa)](https://crates.io/crates/saa)
![Crates.io](https://img.shields.io/crates/l/saa)
![GitHub Workflow Status](https://img.shields.io/github/actions/workflow/status/wvwwvwwv/synchronous-and-asynchronous/saa.yml?branch=main)

Low-level synchronization primitives that provide both synchronous and asynchronous interfaces.

## Features

- Asynchronous counterparts of synchronous methods.
- [`Loom`](https://github.com/tokio-rs/loom) support: `features = ["loom"]`.
- No spin-locks and no busy loops.

## Lock

`saa::Lock` is a Low-level shared-exclusive lock that provides both synchronous and asynchronous interfaces. Synchronous locking methods such as `lock_exclusive_sync` or `lock_shared_sync` can be used with their asynchronous counterparts, `lock_exclusive_async` or `lock_shared_async`, at the same time. `saa::Lock` implements a heap-allocation-free fair wait queue that is shared among both synchronous and asynchronous methods.

### Examples

```rust
use saa::Lock;

let lock = Lock::default();

lock.lock_exclusive_sync();

assert!(!lock.try_lock_exclusive());
assert!(!lock.try_lock_shared());

assert!(!lock.unlock_shared());
assert!(lock.unlock_exclusive());

async {
    lock.lock_shared_async();
    assert!(lock.unlock_shared());
};
```

## Semaphore

`saa::Semaphore` is a synchronization primitive that allows a fixed number of threads to access a resource concurrently.

### Examples

```rust
use saa::Semaphore;

let semaphore = Semaphore::default();

semaphore.acquire_many_sync(Semaphore::MAX_PERMITS - 1);

assert!(semaphore.try_acquire());
assert!(!semaphore.try_acquire());

assert!(semaphore.release());
assert!(!semaphore.release_many(Semaphore::MAX_PERMITS));
assert!(semaphore.release_many(Semaphore::MAX_PERMITS - 1));

async {
    semaphore.acquire_async().await;
    assert!(semaphore.release());
};
```

## [Changelog](https://github.com/wvwwvwwv/synchronous-and-asynchronous/blob/main/CHANGELOG.md)
