use criterion::{criterion_group, criterion_main, Criterion};
use saa::Lock;

fn lock_unlock(c: &mut Criterion) {
    c.bench_function("lock-unlock", |b| {
        b.iter(|| {
            let lock = Lock::default();
            lock.lock_exclusive_sync();
            assert!(lock.unlock_exclusive());
        });
    });
}

fn share_unshare(c: &mut Criterion) {
    c.bench_function("share-unshare", |b| {
        b.iter(|| {
            let lock = Lock::default();
            lock.lock_shared_sync();
            assert!(lock.unlock_shared());
        });
    });
}

criterion_group!(lock, lock_unlock, share_unshare);
criterion_main!(lock);
