# Synchronous and Asynchronous Synchronization Primitives

[![Cargo](https://img.shields.io/crates/v/saa)](https://crates.io/crates/saa)
![Crates.io](https://img.shields.io/crates/l/saa)
![GitHub Workflow Status](https://img.shields.io/github/actions/workflow/status/wvwwvwwv/synchronous-and-asynchronous/saa.yml?branch=main)

Low-level synchronization primitives that provide both synchronous and asynchronous interfaces.

## Features

- Asynchronous counterparts of synchronous methods.
- [`Loom`](https://github.com/tokio-rs/loom) support: `features = ["loom"]`.
- No spin-locks and no busy loops, except for when an asynchronous task waiting for a resource gets cancelled.

## Lock

`saa::Lock` is a Low-level shared-exclusive lock that provides both synchronous and asynchronous interfaces. Synchronous locking methods such as `lock_sync` or `lock_shared_sync` can be used with their asynchronous counterparts, `lock_exclusive_async` or `lock_shared_async`, at the same time. `saa::Lock` implements a heap-allocation-free fair wait queue that is shared among both synchronous and asynchronous methods.

### Examples

```rust
use saa::Lock;

let lock = Lock::default();

lock.lock_sync();

assert!(!lock.try_lock());
assert!(!lock.try_share());

assert!(!lock.release_share());
assert!(lock.release_lock());

async {
    lock.share_async();
    assert!(lock.release_share());
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

## Gate

`saa::Gate` is an unbounded barrier that can be open of sealed manually whenever needed.

### Examples

```rust
use std::sync::Arc;
use std::thread;

use saa::Gate;
use saa::gate::State;

let gate = Arc::new(Gate::default());

let mut threads = Vec::new();

for _ in 0..4 {
    let gate = gate.clone();
    threads.push(thread::spawn(move || {
        assert_eq!(gate.enter_sync(), Ok(State::Controlled));
    }));
}

let mut cnt = 0;
while cnt != 4 {
    if let Ok(n) = gate.permit() {
        cnt += n;
    }
}

for thread in threads {
    thread.join().unwrap();
}
```

## Notes

Use of synchronous methods in an asynchronous context may lead to a deadlock. Suppose a scenario where an asynchronous runtime provides two threads executing three tasks.

* ThreadId(0): `task-0: share-waiting / pending` || `task-1: "synchronous"-lock-waiting`.
* ThreadId(1): `task-2: release-lock / ready: wake-up task-0` -> `task-2: lock-waiting / pending`.

In the above example, `task-0` logically has acquired a shared lock which was transferred from `task-2`, however, it may remain in the task queue indefinitely, depending on the scheduling policy of the asynchronous runtime.

## [Changelog](https://github.com/wvwwvwwv/synchronous-and-asynchronous/blob/main/CHANGELOG.md)
