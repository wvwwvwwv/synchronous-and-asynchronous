# Changelog

4.4.0

* Add `lock::Mode::Wait`.

4.3.2

* Minor `Future` size reduction.

4.3.1

* Fix a bug in `Pager::try_poll`.

4.3.0

* Remove `Pager::is_sync`.
* All `Pager` methods need a pinned reference.
* `Pager::poll*` methods return the result only once, and further calls without registering it will return an error.

4.2.0

* Remove the 128B alignment requirement for `WaitQueue`.

4.1.0

* API update for the `Pager` API: `Pager` is now explicitly `!Unpin`, and `poll_async` replaces direct `await` calls.

4.0.0

* Add the `Pager` API for all synchronization primitives.
* `Semaphore::acquire_many*` methods return `false` if the specified count is greater than the maximum allowed.

3.3.0

* Reduce the size of asynchronous tasks in general.

3.2.1

* Minor optimization.

3.2.0

* Remove internal use of `Mutex` in the wait queue.

3.1.0

* Add `gate::Pager::try_poll`.

3.0.4

* Inline trivial methods.

3.0.2 - 3.0.3

* Fix the `failure` load ordering when the lock is deliberately poisoned, the gate is open/sealed, or the semaphore is closed after an event.

3.0.1

* Minor improvements to documentation and metadata.

3.0.0

* Add a poisoned state to `saa::Lock`.
* Add `*_with` methods for notifying when a thread enters a wait queue.

2.0.0

* New synchronization primitive: `saa::Gate`.

1.1.0

* Fix a hang issue when an asynchronous task is dropped before completion.
* Work in progress: `saa::Gate`.

1.0.1

* Minor optimization.

1.0.0

* Stabilize.

0.4.0

* Update API.

0.3.0

* Stabilize.

0.2.0

* Update API.

0.1.0

* Initial release.
