# Changelog

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
* Work-in-progress: `saa::Gate`.

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
