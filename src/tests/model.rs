#[cfg(feature = "loom")]
#[cfg(test)]
mod lock_model {
    use crate::Lock;
    use crate::Semaphore;

    use loom::thread::spawn;
    use std::sync::Arc;

    #[test]
    fn lock_shared() {
        loom::model(|| {
            let lock = Arc::new(Lock::default());
            lock.lock_exclusive_sync();
            let lock_clone = lock.clone();
            let thread = spawn(move || {
                lock_clone.lock_shared_sync();
                assert!(lock_clone.unlock_shared());
            });
            assert!(lock.unlock_exclusive());
            assert!(thread.join().is_ok());
        });
    }

    #[test]
    fn lock_exclusive() {
        loom::model(|| {
            let lock = Arc::new(Lock::default());
            lock.lock_shared_sync();
            let lock_clone = lock.clone();
            let thread = spawn(move || {
                lock_clone.lock_exclusive_sync();
                assert!(lock_clone.unlock_exclusive());
            });
            assert!(lock.unlock_shared());
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
}
