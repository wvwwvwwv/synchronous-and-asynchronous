use saa::{Lock, Semaphore};

#[test]
fn lock_exclusive() {
    let lock = Lock::default();

    lock.lock_sync();

    assert!(!lock.try_lock());
    assert!(!lock.try_share());

    assert!(!lock.release_share());
    assert!(lock.release_lock());
}

#[test]
fn acquire() {
    let semaphore = Semaphore::default();

    semaphore.acquire_many_sync(Semaphore::MAX_PERMITS - 1);

    assert!(semaphore.try_acquire());
    assert!(!semaphore.try_acquire());

    assert!(semaphore.release());
    assert!(!semaphore.release_many(Semaphore::MAX_PERMITS));
    assert!(semaphore.release_many(Semaphore::MAX_PERMITS - 1));
}
