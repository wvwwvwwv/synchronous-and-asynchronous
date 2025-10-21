# Synchronous and Asynchronous Synchronization Primitives

[![Cargo](https://img.shields.io/crates/v/saa)](https://crates.io/crates/saa)
![Crates.io](https://img.shields.io/crates/l/saa)
![GitHub Workflow Status](https://img.shields.io/github/actions/workflow/status/wvwwvwwv/synchronous-and-asynchronous/saa.yml?branch=main)

Word-sized low-level synchronization primitives providing both asynchronous and synchronous interfaces.

## Features

- No heap allocation.
- No hidden global variables.
- Provides both asynchronous and synchronous interfaces.
- Small memory footprint.
- [`Loom`](https://github.com/tokio-rs/loom) support: `features = ["loom"]`.

## Lock

`saa::Lock` is a low-level shared-exclusive lock providing both asynchronous and synchronous interfaces. Synchronous locking methods such as `lock_sync` and `share_sync` can be used alongside their asynchronous counterparts `lock_async` and `share_async` simultaneously. `saa::Lock` implements an allocation-free fair wait queue shared between both synchronous and asynchronous methods.

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

## Barrier

`saa::Barrier` is a synchronization primitive to enable a number of tasks to start execution at the same time.

```rust
use std::sync::Arc;
use std::thread;

use saa::Barrier;

let barrier = Arc::new(Barrier::with_count(8));

let mut threads = Vec::new();

for _ in 0..8 {
    let barrier = barrier.clone();
    threads.push(thread::spawn(move || {
        for _ in 0..4 {
            barrier.wait_sync();
        }
    }));
}

for thread in threads {
    thread.join().unwrap();
}
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

`saa::Gate` is an unbounded barrier that can be opened or sealed manually as needed.

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

let mut count = 0;
while count != 4 {
    if let Ok(n) = gate.permit() {
        count += n;
    }
}

for thread in threads {
    thread.join().unwrap();
}
```

## Pager

`saa::Pager` enables remotely waiting for a resource to become available.

### Examples

```rust
use std::pin::pin;

use saa::{Gate, Pager};
use saa::gate::State;

let gate = Gate::default();

let mut pinned_pager = pin!(Pager::default());

assert!(gate.register_pager(&mut pinned_pager, true));
assert_eq!(gate.open().1, 1);

assert_eq!(pinned_pager.poll_sync(), Ok(State::Open));
```

## Notes

Using synchronous methods in an asynchronous context may lead to deadlocks. Consider a scenario where an asynchronous runtime uses two threads to execute three tasks.

* ThreadId(0): `task-0: share-waiting / pending` || `task-1: "synchronous"-lock-waiting`.
* ThreadId(1): `task-2: release-lock / ready: wake-up task-0` -> `task-2: lock-waiting / pending`.

In this example, `task-0` has logically acquired a shared lock transferred from `task-2`; however, it may remain in the task queue indefinitely depending on the asynchronous runtime's scheduling policy.

## [Changelog](https://github.com/wvwwvwwv/synchronous-and-asynchronous/blob/main/CHANGELOG.md)
