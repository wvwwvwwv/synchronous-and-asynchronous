# Synchronous and Asynchronous Synchronization Primitives

[![Cargo](https://img.shields.io/crates/v/saa)](https://crates.io/crates/saa)
![Crates.io](https://img.shields.io/crates/l/saa)
![GitHub Workflow Status](https://img.shields.io/github/actions/workflow/status/wvwwvwwv/synchronous-and-asynchronous/saa.yml?branch=main)

_WORK-IN-PROGRESS._

* THE CODE IS INCOMPLETE AND NOT AT ALL BATTLE-TESTED.

Low-level synchronization primitives that provide both synchronous and asynchronous interfaces.

#### Features

- Asynchronous counterparts of blocking and synchronous methods.
- [`Loom`](https://github.com/tokio-rs/loom) support: `features = ["loom"]`.
- No spin-locks and no busy loops.

### Lock

`saa::Lock` is a Low-level shared-exclusive lock that provides both synchronous and asynchronous interfaces. Synchronous locking methods such as `lock_exclusive_sync` or `lock_shared_sync` can be used with their asynchronous counterparts, `lock_exclusive_async` or `lock_shared_async`, at the same time. `saa::Lock` implements a heap-allocation-free fair wait queue that is shared among both synchronous and asynchronous methods.

## [Changelog](https://github.com/wvwwvwwv/synchronous-and-asynchronous/blob/main/CHANGELOG.md)
