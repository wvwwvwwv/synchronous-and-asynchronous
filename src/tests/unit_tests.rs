use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering::Relaxed;
use std::thread;
use std::time::Duration;

use crate::opcode::Opcode;
use crate::sync_primitive::SyncPrimitive;
use crate::{Lock, Semaphore};

#[cfg_attr(miri, ignore = "Tokio is not compatible with Miri")]
#[tokio::test(flavor = "multi_thread", worker_threads = 16)]
async fn lock_shared_wait() {
    let num_tasks = 64;

    let lock = Arc::new(Lock::default());
    let check = Arc::new(AtomicUsize::new(0));

    lock.lock_async().await;
    check.fetch_add(usize::MAX, Relaxed);

    let mut tasks = Vec::new();
    for i in 0..num_tasks {
        let lock = lock.clone();
        let check = check.clone();
        tasks.push(tokio::spawn(async move {
            if i % 8 == 0 {
                lock.share_sync();
            } else {
                lock.share_async().await;
            }
            assert_ne!(check.fetch_add(1, Relaxed), usize::MAX);
            check.fetch_sub(1, Relaxed);
            assert!(lock.release_share());
            lock.share_async().await;
            assert!(lock.release_share());
        }));
    }

    tokio::time::sleep(Duration::from_millis(50)).await;
    check.fetch_sub(usize::MAX, Relaxed);
    assert!(lock.release_lock());

    for task in tasks {
        task.await.unwrap();
    }
    assert_eq!(check.load(Relaxed), 0);

    lock.lock_async().await;
    assert!(lock.release_lock());
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
fn lock_sync() {
    let num_threads = if cfg!(miri) {
        4
    } else {
        Lock::MAX_SHARED_OWNERS
    };
    let num_iters = if cfg!(miri) { 16 } else { 256 };

    let lock = Arc::new(Lock::default());
    let check = Arc::new(AtomicUsize::new(0));

    lock.lock_sync();
    check.fetch_add(usize::MAX, Relaxed);

    let mut threads = Vec::new();
    for _ in 0..num_threads {
        let lock = lock.clone();
        let check = check.clone();
        threads.push(thread::spawn(move || {
            for j in 0..num_iters {
                if j % 11 == 0 {
                    lock.lock_sync();
                    assert_eq!(check.fetch_add(usize::MAX, Relaxed), 0);
                    thread::sleep(Duration::from_micros(1));
                    check.fetch_sub(usize::MAX, Relaxed);
                    assert!(lock.release_lock());
                } else {
                    lock.share_sync();
                    assert!(check.fetch_add(1, Relaxed) < Lock::MAX_SHARED_OWNERS);
                    thread::sleep(Duration::from_micros(1));
                    check.fetch_sub(1, Relaxed);
                    assert!(lock.release_share());
                }
            }
        }));
    }

    thread::sleep(Duration::from_micros(1));
    check.fetch_sub(usize::MAX, Relaxed);
    assert!(lock.release_lock());

    for thread in threads {
        thread.join().unwrap();
    }
    assert_eq!(check.load(Relaxed), 0);
}

#[test]
fn semaphore_sync() {
    let num_threads = if cfg!(miri) {
        4
    } else {
        Semaphore::MAX_PERMITS
    };
    let num_iters = if cfg!(miri) { 16 } else { 256 };

    let semaphore = Arc::new(Semaphore::default());
    let check = Arc::new(AtomicUsize::new(0));

    semaphore.acquire_many_sync(Semaphore::MAX_PERMITS);
    check.fetch_add(Semaphore::MAX_PERMITS, Relaxed);

    let mut threads = Vec::new();
    for i in 0..num_threads {
        let semaphore = semaphore.clone();
        let check = check.clone();
        threads.push(thread::spawn(move || {
            for _ in 0..num_iters {
                semaphore.acquire_many_sync(i + 1);
                assert!(check.fetch_add(i + 1, Relaxed) + i < Semaphore::MAX_PERMITS);
                thread::sleep(Duration::from_micros(1));
                check.fetch_sub(i + 1, Relaxed);
                assert!(semaphore.release_many(i + 1));
            }
        }));
    }

    thread::sleep(Duration::from_micros(1));
    check.fetch_sub(Semaphore::MAX_PERMITS, Relaxed);
    assert!(semaphore.release_many(Semaphore::MAX_PERMITS));

    for thread in threads {
        thread.join().unwrap();
    }
    assert_eq!(check.load(Relaxed), 0);
}

#[test]
fn drop_future() {
    let lock = Arc::new(Lock::default());
    lock.lock_sync();

    let mut threads = Vec::new();
    for i in 0..2 {
        let lock = lock.clone();
        threads.push(thread::spawn(move || {
            if i == 0 {
                lock.lock_sync();
                assert!(lock.release_lock());
            } else {
                lock.share_sync();
                assert!(lock.release_share());
            }
        }));
    }

    lock.test_drop_wait_queue_entry(Opcode::Exclusive);
    lock.test_drop_wait_queue_entry(Opcode::Shared);
    assert!(lock.release_lock());

    for thread in threads {
        thread.join().unwrap();
    }

    lock.lock_sync();
    assert!(lock.release_lock());
}

#[cfg_attr(miri, ignore = "Tokio is not compatible with Miri")]
#[tokio::test(flavor = "multi_thread", worker_threads = 16)]
async fn lock_chaos() {
    let num_tasks = Lock::MAX_SHARED_OWNERS;
    let num_iters = 256;

    let lock = Arc::new(Lock::default());
    let check = Arc::new(AtomicUsize::new(0));

    let mut threads = Vec::new();
    let mut tasks = Vec::new();
    for i in 0..num_tasks {
        let lock = lock.clone();
        let check = check.clone();
        if i % 2 == 0 {
            tasks.push(tokio::spawn(async move {
                for j in 0..num_iters {
                    if j % 11 == 0 {
                        lock.lock_async().await;
                        assert_eq!(check.fetch_add(usize::MAX, Relaxed), 0);
                        check.fetch_sub(usize::MAX, Relaxed);
                        assert!(lock.release_lock());
                    } else {
                        lock.share_async().await;
                        assert!(check.fetch_add(1, Relaxed) < Lock::MAX_SHARED_OWNERS);
                        check.fetch_sub(1, Relaxed);
                        assert!(lock.release_share());
                    }
                }
            }));
        } else {
            threads.push(thread::spawn(move || {
                for j in 0..num_iters {
                    if j % 11 == 1 {
                        lock.test_drop_wait_queue_entry(Opcode::Exclusive);
                    } else if j % 7 == 0 {
                        lock.lock_sync();
                        assert_eq!(check.fetch_add(usize::MAX, Relaxed), 0);
                        check.fetch_sub(usize::MAX, Relaxed);
                        assert!(lock.release_lock());
                    } else {
                        lock.share_sync();
                        assert!(check.fetch_add(1, Relaxed) < Lock::MAX_SHARED_OWNERS);
                        check.fetch_sub(1, Relaxed);
                        assert!(lock.release_share());
                    }
                }
            }));
        }
    }

    for thread in threads {
        thread.join().unwrap();
    }
    for task in tasks {
        task.await.unwrap();
    }
    assert_eq!(check.load(Relaxed), 0);
}

#[cfg_attr(miri, ignore = "Tokio is not compatible with Miri")]
#[tokio::test(flavor = "multi_thread", worker_threads = 16)]
async fn semaphore_chaos() {
    let num_tasks = Semaphore::MAX_PERMITS;
    let num_iters = 256;

    let semaphore = Arc::new(Semaphore::default());
    let check = Arc::new(AtomicUsize::new(0));

    let mut threads = Vec::new();
    let mut tasks = Vec::new();
    for i in 0..num_tasks {
        let semaphore = semaphore.clone();
        let check = check.clone();
        if i % 2 == 0 {
            tasks.push(tokio::spawn(async move {
                for _ in 0..num_iters {
                    semaphore.acquire_many_async(i + 1).await;
                    assert!(check.fetch_add(i + 1, Relaxed) + i < Semaphore::MAX_PERMITS);
                    check.fetch_sub(i + 1, Relaxed);
                    assert!(semaphore.release_many(i + 1));
                }
            }));
        } else {
            threads.push(thread::spawn(move || {
                for j in 0..num_iters {
                    if j % 11 == 1 {
                        semaphore.test_drop_wait_queue_entry(Opcode::Semaphore(19));
                    } else {
                        semaphore.acquire_many_sync(i + 1);
                        assert!(check.fetch_add(i + 1, Relaxed) + i < Semaphore::MAX_PERMITS);
                        check.fetch_sub(i + 1, Relaxed);
                        assert!(semaphore.release_many(i + 1));
                    }
                }
            }));
        }
    }

    for thread in threads {
        thread.join().unwrap();
    }
    for task in tasks {
        task.await.unwrap();
    }
    assert_eq!(check.load(Relaxed), 0);
}
