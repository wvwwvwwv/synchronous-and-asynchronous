#[cfg(not(feature = "loom"))]
#[cfg(test)]
mod lock_test {
    use std::{sync::Arc, time::Duration};

    use crate::Lock;

    #[cfg_attr(miri, ignore = "Tokio is not compatible with Miri")]
    #[tokio::test]
    async fn lock() {
        let lock = Lock::default();
        lock.lock_exclusive_async().await;
        assert!(!lock.try_lock_shared());
    }

    #[cfg_attr(miri, ignore = "Tokio is not compatible with Miri")]
    #[tokio::test(flavor = "multi_thread", worker_threads = 16)]
    async fn share_wait_acquire() {
        let num_tasks = 64;

        let lock = Arc::new(Lock::default());

        lock.lock_exclusive_async().await;

        let mut tasks = Vec::new();
        for _ in 0..num_tasks {
            let lock = lock.clone();
            tasks.push(tokio::spawn(async move {
                lock.lock_shared_async().await;
                assert!(lock.unlock_shared());
            }));
        }

        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(lock.unlock_exclusive());

        for task in tasks {
            task.await.unwrap();
        }

        lock.lock_exclusive_async().await;
        assert!(lock.unlock_exclusive());
    }

    #[cfg_attr(miri, ignore = "Tokio is not compatible with Miri")]
    #[tokio::test(flavor = "multi_thread", worker_threads = 16)]
    async fn drop_future() {
        let lock = Arc::new(Lock::default());
        lock.lock_exclusive_async().await;

        let mut tasks = Vec::new();
        for i in 0..2 {
            let lock = lock.clone();
            tasks.push(tokio::spawn(async move {
                if i == 0 {
                    lock.lock_exclusive_async().await;
                    assert!(lock.unlock_exclusive());
                } else {
                    lock.lock_shared_async().await;
                    assert!(lock.unlock_shared());
                }
            }));
        }

        lock.test_drop_wait_queue_entry(true);
        lock.test_drop_wait_queue_entry(false);
        assert!(lock.unlock_exclusive());

        for task in tasks {
            task.await.unwrap();
        }

        lock.lock_exclusive_async().await;
        assert!(lock.unlock_exclusive());
    }
}
