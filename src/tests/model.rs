#[cfg(feature = "loom")]
#[cfg(test)]
mod lock_model {
    use crate::Lock;

    use loom::thread::spawn;
    use std::sync::Arc;

    #[test]
    fn lock_share() {
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
}
