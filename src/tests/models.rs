use std::sync::Arc;

use loom::thread::spawn;

use crate::opcode::Opcode;
use crate::sync_primitive::SyncPrimitive;
use crate::{Lock, Semaphore};

#[test]
fn lock_shared() {
    loom::model(|| {
        let lock = Arc::new(Lock::default());
        lock.lock_sync();
        let lock_clone = lock.clone();
        let thread = spawn(move || {
            lock_clone.share_sync();
            assert!(lock_clone.release_share());
        });
        assert!(lock.release_lock());
        assert!(thread.join().is_ok());
    });
}

#[test]
fn lock_exclusive() {
    loom::model(|| {
        let lock = Arc::new(Lock::default());
        lock.share_sync();
        let lock_clone = lock.clone();
        let thread = spawn(move || {
            lock_clone.lock_sync();
            assert!(lock_clone.release_lock());
        });
        assert!(lock.release_share());
        assert!(thread.join().is_ok());
    });
}

#[test]
fn semaphore_release_acquire() {
    loom::model(|| {
        let semaphore = Arc::new(Semaphore::default());
        semaphore.acquire_many_sync(Semaphore::MAX_PERMITS);
        let semaphore_clone = semaphore.clone();
        let thread = spawn(move || {
            semaphore_clone.acquire_many_sync(9);
            semaphore_clone.acquire_many_sync(2);
        });
        assert!(semaphore.release_many(10));
        assert!(semaphore.release());
        assert!(thread.join().is_ok());
    });
}

#[test]
fn drop_future() {
    loom::model(|| {
        let semaphore = Arc::new(Semaphore::default());
        semaphore.acquire_many_sync(Semaphore::MAX_PERMITS);
        let semaphore_clone = semaphore.clone();
        let thread = spawn(move || {
            semaphore_clone.acquire_many_sync(9);
        });
        semaphore.test_drop_wait_queue_entry(Opcode::Semaphore(11));
        assert!(semaphore.release_many(Semaphore::MAX_PERMITS));
        assert!(thread.join().is_ok());
    });
}
