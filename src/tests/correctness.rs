#[cfg(not(feature = "loom"))]
#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use crate::sync_primitive::Opcode;
    use crate::sync_primitive::SyncPrimitive;
    use crate::Lock;

    #[cfg_attr(miri, ignore = "Tokio is not compatible with Miri")]
    #[tokio::test(flavor = "multi_thread", worker_threads = 16)]
    async fn share_wait_acquire() {
        let num_tasks = 64;

        let lock = Arc::new(Lock::default());

        lock.lock_exclusive_async().await;

        let mut tasks = Vec::new();
        for i in 0..num_tasks {
            let lock = lock.clone();
            tasks.push(tokio::spawn(async move {
                if i % 8 == 0 {
                    // At most 15 tasks can block.
                    lock.lock_shared_sync();
                } else {
                    lock.lock_shared_async().await;
                }
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

        lock.test_drop_wait_queue_entry(Opcode::Exclusive);
        lock.test_drop_wait_queue_entry(Opcode::Shared);
        assert!(lock.unlock_exclusive());

        for task in tasks {
            task.await.unwrap();
        }

        lock.lock_exclusive_async().await;
        assert!(lock.unlock_exclusive());
    }
}
