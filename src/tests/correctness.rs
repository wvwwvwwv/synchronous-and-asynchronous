#[cfg(not(feature = "loom"))]
#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::AtomicUsize;
    use std::sync::atomic::Ordering::Relaxed;
    use std::thread;
    use std::time::Duration;

    use crate::sync_primitive::SyncPrimitive;
    use crate::{Lock, Opcode, Semaphore};

    #[cfg_attr(miri, ignore = "Tokio is not compatible with Miri")]
    #[tokio::test(flavor = "multi_thread", worker_threads = 16)]
    async fn lock_shared_wait() {
        let num_tasks = 64;

        let lock = Arc::new(Lock::default());
        let check = Arc::new(AtomicUsize::new(0));

        lock.lock_exclusive_async().await;
        check.fetch_add(usize::MAX, Relaxed);

        let mut tasks = Vec::new();
        for i in 0..num_tasks {
            let lock = lock.clone();
            let check = check.clone();
            tasks.push(tokio::spawn(async move {
                if i % 8 == 0 {
                    // At most 15 tasks can block.
                    lock.lock_shared_sync();
                } else {
                    lock.lock_shared_async().await;
                }
                assert_ne!(check.fetch_add(1, Relaxed), usize::MAX);
                check.fetch_sub(1, Relaxed);
                assert!(lock.unlock_shared());
                lock.lock_shared_async().await;
                assert!(lock.unlock_shared());
            }));
        }

        tokio::time::sleep(Duration::from_millis(50)).await;
        check.fetch_sub(usize::MAX, Relaxed);
        assert!(lock.unlock_exclusive());

        for task in tasks {
            task.await.unwrap();
        }
        assert_eq!(check.load(Relaxed), 0);

        lock.lock_exclusive_async().await;
        assert!(lock.unlock_exclusive());
    }

    #[cfg_attr(miri, ignore = "Tokio is not compatible with Miri")]
    #[tokio::test(flavor = "multi_thread", worker_threads = 16)]
    async fn semaphore_acquire_wait() {
        let num_tasks = 64;

        let semaphore = Arc::new(Semaphore::default());
        let check = Arc::new(AtomicUsize::new(0));

        semaphore.acquire_many_sync(Semaphore::MAX_PERMITS);
        check.fetch_add(Semaphore::MAX_PERMITS, Relaxed);

        let mut tasks = Vec::new();
        for i in 0..num_tasks {
            let semaphore = semaphore.clone();
            let check = check.clone();
            tasks.push(tokio::spawn(async move {
                if i % 8 == 0 {
                    // At most 15 tasks can block.
                    semaphore.acquire_sync();
                } else {
                    semaphore.acquire_async().await;
                }
                assert!(check.fetch_add(1, Relaxed) < Semaphore::MAX_PERMITS);
                check.fetch_sub(1, Relaxed);
                assert!(semaphore.release());
            }));
        }

        tokio::time::sleep(Duration::from_millis(25)).await;
        check.fetch_sub(Semaphore::MAX_PERMITS - 11, Relaxed);
        assert!(semaphore.release_many(Semaphore::MAX_PERMITS - 11));

        tokio::time::sleep(Duration::from_millis(25)).await;
        check.fetch_sub(11, Relaxed);
        assert!(semaphore.release_many(11));

        for task in tasks {
            task.await.unwrap();
        }
        assert_eq!(check.load(Relaxed), 0);

        semaphore.acquire_many_async(Semaphore::MAX_PERMITS).await;
        assert!(semaphore.release_many(Semaphore::MAX_PERMITS));
    }

    #[test]
    fn lock_exclusive_wait() {
        let num_threads = 4;

        let lock = Arc::new(Lock::default());
        let check = Arc::new(AtomicUsize::new(0));

        lock.lock_shared_sync();
        check.fetch_add(1, Relaxed);

        let mut threads = Vec::new();
        for i in 0..num_threads {
            let lock = lock.clone();
            let check = check.clone();
            threads.push(thread::spawn(move || {
                if i == 0 {
                    lock.lock_exclusive_sync();
                    assert_eq!(check.fetch_add(usize::MAX, Relaxed), 0);
                    check.fetch_sub(usize::MAX, Relaxed);
                    assert!(lock.unlock_exclusive());
                    lock.lock_exclusive_sync();
                    assert!(lock.unlock_exclusive());
                } else {
                    lock.lock_shared_sync();
                    check.fetch_add(1, Relaxed);
                    thread::sleep(Duration::from_millis(1));
                    check.fetch_sub(1, Relaxed);
                    assert!(lock.unlock_shared());
                    lock.lock_shared_sync();
                    assert!(lock.unlock_shared());
                }
            }));
        }

        thread::sleep(Duration::from_millis(50));
        check.fetch_sub(1, Relaxed);
        assert!(lock.unlock_shared());

        for thread in threads {
            thread.join().unwrap();
        }
        assert_eq!(check.load(Relaxed), 0);

        lock.lock_shared_sync();
        assert!(lock.unlock_shared());
    }

    #[test]
    fn drop_future() {
        let lock = Arc::new(Lock::default());
        lock.lock_exclusive_sync();

        let mut threads = Vec::new();
        for i in 0..2 {
            let lock = lock.clone();
            threads.push(thread::spawn(move || {
                if i == 0 {
                    lock.lock_exclusive_sync();
                    assert!(lock.unlock_exclusive());
                } else {
                    lock.lock_shared_sync();
                    assert!(lock.unlock_shared());
                }
            }));
        }

        lock.test_drop_wait_queue_entry(Opcode::Exclusive);
        lock.test_drop_wait_queue_entry(Opcode::Shared);
        assert!(lock.unlock_exclusive());

        for thread in threads {
            thread.join().unwrap();
        }

        lock.lock_exclusive_sync();
        assert!(lock.unlock_exclusive());
    }
}
