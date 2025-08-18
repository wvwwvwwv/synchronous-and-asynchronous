use criterion::{criterion_group, criterion_main, Criterion};
use saa::Lock;

fn exclusive_unlock(c: &mut Criterion) {
    c.bench_function("lock-exclusive-unlock", |b| {
        b.iter(|| {
            let lock = Lock::default();
            lock.lock_exclusive_sync();
            assert!(lock.unlock_exclusive());
        });
    });
}

fn shared_shared_unlock_unlock(c: &mut Criterion) {
    c.bench_function("share-shared-unlock-unlock", |b| {
        b.iter(|| {
            let lock = Lock::default();
            lock.lock_shared_sync();
            lock.lock_shared_sync();
            assert!(lock.unlock_shared());
            assert!(lock.unlock_shared());
        });
    });
}

criterion_group!(lock, exclusive_unlock, shared_shared_unlock_unlock);
criterion_main!(lock);
