use std::hint::black_box;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering::{Acquire, Release};
use std::thread;
use std::time::{Duration, Instant};

use criterion::async_executor::FuturesExecutor;
use criterion::{Criterion, criterion_group, criterion_main};
use saa::{Barrier, Lock};

fn exclusive_unlock_async(c: &mut Criterion) {
    async fn test() {
        let lock = Lock::default();
        lock.lock_async().await;
        assert!(lock.release_lock());
    }
    c.bench_function("lock-exclusive-unlock-async", |b| {
        b.to_async(FuturesExecutor).iter(test);
    });
}

fn exclusive_unlock_sync(c: &mut Criterion) {
    c.bench_function("lock-exclusive-unlock-sync", |b| {
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

fn multi_threaded_workload(iters: u64, num_threads: usize, num_cycles: usize) -> Duration {
    let lock = Arc::new(Lock::default());
    let mut threads = Vec::with_capacity(num_threads);
    let barrier = Arc::new(Barrier::with_count(num_threads));
    for _ in 0..num_threads {
        let barrier = barrier.clone();
        let lock = lock.clone();
        let join = thread::spawn(move || {
            barrier.wait_sync();
            let start = Instant::now();
            for _ in 0..iters {
                assert!(lock.lock_sync());
                let acc = black_box({
                    (black_box(0)..black_box(num_cycles)).fold(black_box(0), |acc, v| acc + v)
                });
                assert_eq!(acc, (0..num_cycles).sum::<usize>());
                assert!(lock.release_lock());
            }
            start.elapsed()
        });
        threads.push(join);
    }
    threads
        .into_iter()
        .fold(Duration::from_nanos(0), |acc, t| acc.max(t.join().unwrap()))
}

fn multi_threaded_workload_std(iters: u64, num_threads: usize, num_cycles: usize) -> Duration {
    let lock = Arc::new(std::sync::Mutex::<()>::default());
    let mut threads = Vec::with_capacity(num_threads);
    let barrier = Arc::new(Barrier::with_count(num_threads));
    for _ in 0..num_threads {
        let barrier = barrier.clone();
        let lock = lock.clone();
        let join = thread::spawn(move || {
            barrier.wait_sync();
            let start = Instant::now();
            for _ in 0..iters {
                let guard = lock.lock().unwrap();
                let acc = black_box({
                    (black_box(0)..black_box(num_cycles)).fold(black_box(0), |acc, v| acc + v)
                });
                assert_eq!(acc, (0..num_cycles).sum::<usize>());
                drop(guard);
            }
            start.elapsed()
        });
        threads.push(join);
    }
    threads
        .into_iter()
        .fold(Duration::from_nanos(0), |acc, t| acc.max(t.join().unwrap()))
}

macro_rules! multi_threaded {
    ($name:ident, $num_threads:expr, $num_cycles:expr) => {
        fn $name(c: &mut Criterion) {
            c.bench_function(stringify!($name), |b| {
                b.iter_custom(|iters| multi_threaded_workload(iters, $num_threads, $num_cycles))
            });
        }
    };
}

macro_rules! multi_threaded_std {
    ($name:ident, $num_threads:expr, $num_cycles:expr) => {
        fn $name(c: &mut Criterion) {
            c.bench_function(stringify!($name), |b| {
                b.iter_custom(|iters| multi_threaded_workload_std(iters, $num_threads, $num_cycles))
            });
        }
    };
}

multi_threaded!(multi_threaded_2_256, 2, 256);
multi_threaded!(multi_threaded_2_65536, 2, 65536);
multi_threaded!(multi_threaded_8_256, 8, 256);
multi_threaded!(multi_threaded_8_65536, 8, 65536);
multi_threaded_std!(multi_threaded_std_2_256, 2, 256);
multi_threaded_std!(multi_threaded_std_2_65536, 2, 65536);
multi_threaded_std!(multi_threaded_std_8_256, 8, 256);
multi_threaded_std!(multi_threaded_std_8_65536, 8, 65536);

criterion_group!(
    lock,
    exclusive_unlock_async,
    exclusive_unlock_sync,
    shared_shared_unlock_unlock,
    wait_awake,
    multi_threaded_2_256,
    multi_threaded_2_65536,
    multi_threaded_8_256,
    multi_threaded_8_65536,
    multi_threaded_std_2_256,
    multi_threaded_std_2_65536,
    multi_threaded_std_8_256,
    multi_threaded_std_8_65536,
);
criterion_main!(lock);
