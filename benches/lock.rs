use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering::{Acquire, Release};
use std::thread;
use std::time::{Duration, Instant};

use criterion::{Criterion, criterion_group, criterion_main};
use saa::Lock;

fn exclusive_unlock(c: &mut Criterion) {
    c.bench_function("lock-exclusive-unlock", |b| {
        b.iter(|| {
            let lock = Lock::default();
            lock.lock_sync();
            assert!(lock.release_lock());
        });
    });
}

fn shared_shared_unlock_unlock(c: &mut Criterion) {
    c.bench_function("share-shared-unlock-unlock", |b| {
        b.iter(|| {
            let lock = Lock::default();
            lock.share_sync();
            lock.share_sync();
            assert!(lock.release_share());
            assert!(lock.release_share());
        });
    });
}

fn wait_awake(c: &mut Criterion) {
    c.bench_function("wait_awake", |b| {
        b.iter_custom(|iters| {
            let lock = Arc::new(Lock::default());
            let entered = Arc::new(AtomicBool::new(false));
            let mut acc = Duration::from_secs(0);
            for _ in 0..iters {
                assert!(lock.lock_sync());
                let lock_clone = lock.clone();
                let entered_clone = entered.clone();
                let thread = thread::spawn(move || {
                    let mut start = Instant::now();
                    lock_clone.lock_sync_with(|| {
                        entered_clone.swap(true, Release);
                        start = Instant::now();
                    });
                    start.elapsed()
                });
                while !entered.load(Acquire) {}
                assert!(lock.release_lock());
                acc += thread.join().unwrap();
                assert!(lock.release_lock());
                assert!(entered.swap(false, Release));
            }
            acc
        })
    });
}

criterion_group!(
    lock,
    exclusive_unlock,
    shared_shared_unlock_unlock,
    wait_awake
);
criterion_main!(lock);
